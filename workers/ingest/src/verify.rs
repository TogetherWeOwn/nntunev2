//! Verifying runtime: fetch article bodies, verify par2 sets, stage to
//! scratch for the repair handoff.
//!
//! One Queue delivery runs [`run_verifying_step`] over the job's parsed
//! files. The step is resumable per segment: every decoded segment is
//! spooled to the scratch bucket as it is fetched and recorded in
//! [`VerifyProgress`] (stored beside the job checkpoint; see
//! `checkpoint::put_verify`), so a redelivery resumes a file at the
//! first unfetched segment instead of refetching it whole, and a staged
//! file is never refetched while its staged bytes still check out. A
//! lost progress record is rebuilt by adopting the staged objects' own
//! metadata, so scratch is the durable record and the progress value is
//! its index. Per-delivery caps ([`VerifyCaps`] plus a deadline
//! callback) bound how much one message may spend; hitting a cap
//! returns [`VerifyOutcome::Incomplete`] with the progress so far, and
//! the consumer redelivers to continue.
//!
//! File size itself is never a failure: a file whose assembled length
//! fits `max_file_bytes` is assembled, judged and staged as one object;
//! a larger file is judged by streaming its spooled segments back one
//! at a time (incremental SHA-256 and per-block CRC32s, so memory stays
//! O(segment)) and staged *as* its spool — the manifest rows then carry
//! the segment keys for the repair handoff to concatenate. Staged par2
//! is restored the same way, packet by packet into a
//! [`RecoverySetBuilder`], so aggregate par2 size is not capped either;
//! only one buffered packet is bounded (see `max_par2_bytes`).
//!
//! Order inside a step: par2 files first (fetch, stage, parse into
//! recovery sets grouped by set id), then data files (fetch, yEnc-decode,
//! stage, verify). A data file is verified by the cheapest proof first —
//! [`RecoverySet::precheck`] over the per-segment CRC32s captured at
//! decode time, then [`RecoverySet::verify`] — and any shortfall is
//! counted in par2 blocks from the set's per-block CRC32s, which is the
//! unit [`decide_repair`](crate::repair::decide_repair) dispatches on.
//! Missing segments are placed by part index when the file's present
//! parts prove a uniform part size (the near-universal poster layout);
//! without that proof only the contiguous present prefix can vouch for
//! blocks, and the rest count as missing (an upper bound the repair
//! binary re-derives exactly).
//!
//! Nothing here logs or stores credentials: bodies come from the pool
//! Durable Object (which owns the provider connection), scratch objects
//! and the checkpoint carry only content hashes, sizes and block counts.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;

use nntune_core::par2::{peek_set_id, RecoveryFile, RecoverySet, RecoverySetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dispatch::content_sha256_hex;
use crate::jobs::validate_job_id;
use crate::parse::ParsedFile;

/// R2 binding for the ingest scratch bucket (see `wrangler.toml.example`).
pub const SCRATCH_BINDING: &str = "INGEST_SCRATCH";

/// Most files one Verifying step walks (the core parser's file cap).
pub const MAX_VERIFY_FILES: usize = 1024;

/// Per-delivery spend caps for [`run_verifying_step`]. The defaults are
/// sized against the 128 MB per-isolate Worker memory limit. The live
/// peaks are sequential, never additive: fetching holds one article
/// buffer and its decode scratch (each ≤ `max_article_bytes`) plus the
/// one decoded segment being spooled; assembly of an in-memory file
/// holds the assembled buffer (≤ `max_file_bytes`) plus one spooled
/// segment at a time; a larger file is judged streaming, one segment
/// at a time; restoring staged par2 in `load_sets` holds one staged
/// object (or one spool segment) plus ≤ `max_par2_bytes` of buffered
/// packet. Each peak stays well under the isolate limit with the
/// parsed recovery sets (metadata only) and runtime overhead
/// accounted for. The size fields bound *memory*, never the job: an
/// ordinary release larger than any of them is spooled/streamed, not
/// failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyCaps {
    /// Files fetched from the provider per delivery.
    pub max_files_per_delivery: usize,
    /// Segments fetched from the provider per delivery.
    pub max_segments_per_delivery: usize,
    /// Article bytes fetched from the provider per delivery.
    pub max_bytes_per_delivery: u64,
    /// Largest assembled file held in memory for judging/staging.
    /// Bigger files are judged by streaming their spooled segments and
    /// staged as the spool itself (see the module docs) — never failed
    /// for size alone.
    pub max_file_bytes: u64,
    /// Largest single article body accepted from the pool.
    pub max_article_bytes: usize,
    /// Largest single par2 *packet* buffered while restoring staged
    /// par2. Staged par2 streams into the set builder packet by packet,
    /// so the aggregate across files/deliveries is uncapped; a packet
    /// beyond this bound (which implies a par2 block size beyond it,
    /// far outside ordinary posts) makes its file's packets unusable
    /// for in-Worker verification, not the job permanently failed.
    pub max_par2_bytes: u64,
    /// Largest logical file length the streaming judgement will walk.
    /// The length is untrusted (par2-declared or part geometry), and
    /// the streamed zeros for a lying length are real CPU: anything
    /// beyond the intake guard's own declared-bytes bound (64 GiB per
    /// NZB) cannot describe a file this pipeline accepted, so it is a
    /// geometry error, not a stream. Within the bound, the delivery
    /// deadline still stops the walk (see `stream_logical`).
    pub max_logical_bytes: u64,
}

impl Default for VerifyCaps {
    fn default() -> Self {
        Self {
            max_files_per_delivery: 16,
            max_segments_per_delivery: 512,
            max_bytes_per_delivery: 64 << 20,
            max_file_bytes: 32 << 20,
            max_article_bytes: 16 << 20,
            max_par2_bytes: 24 << 20,
            max_logical_bytes: 64 << 30,
        }
    }
}

/// Why a Verifying step produced no outcome. Transient failures may
/// succeed on redelivery (pool busy, scratch unavailable); permanent
/// ones never will (oversize file, unparseable par2). Details carry
/// counts only — never segment ids, file names or provider text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    Transient(String),
    Permanent(String),
}

/// One file staged in scratch, as recorded in the checkpoint and in the
/// staged object's metadata. Content-addressed by `sha256`: a resume
/// trusts the record only while the object still hashes to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedFile {
    pub key: String,
    pub size: u64,
    pub sha256: String,
    /// Par2 blocks this file is short (0 = healthy or not yet judged —
    /// par2 objects always record 0). For data files no set covers, a
    /// damaged file records its missing *segment* count instead: the
    /// consumer only reads "damaged and uncovered" from it.
    #[serde(default)]
    pub missing_blocks: u32,
    /// Hex set id of the recovery set covering this file, when one does.
    #[serde(default)]
    pub par2_set_id: Option<String>,
    /// Spool-segment keys when the staged file *is* its segment spool
    /// (too large to assemble in memory): the manifest then lists these
    /// in order for the repair handoff to concatenate, and `key` names
    /// the segment index (see [`VerifyProgress::spool_meta_key`]) rather
    /// than a byte object. Empty for an ordinarily assembled file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spool: Vec<StagedSpoolSegment>,
    /// Spool-object role only: for a spooled *segment* object (never a
    /// staged-file record), the decoded bytes' CRC32, so a lost
    /// progress write can rebuild the resume cursor from object
    /// metadata alone. `missing` marks a segment recorded without
    /// bytes. Both are 0/false on every staged-file record.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub crc32: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub missing: bool,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// One spooled segment of an in-flight or spooled-staged file: the
/// decoded bytes' scratch key plus everything the judgement needs from
/// them (decode-time CRC32 for the par2 precheck, length for geometry).
/// A segment the provider no longer has (or that failed yEnc decode)
/// records `missing` instead — an answered `430` is durable progress
/// exactly like fetched bytes, and is never refetched pointlessly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedSpoolSegment {
    pub key: String,
    pub size: u64,
    pub crc32: u32,
    #[serde(default)]
    pub missing: bool,
}

/// Per-file resume state for one Verifying round, stored beside the job
/// checkpoint under its own KV key (see `checkpoint::put_verify`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyProgress {
    /// Repair round this progress belongs to (`r{round}` in scratch keys).
    pub round: u32,
    /// Staged files by NZB file index.
    pub staged: BTreeMap<usize, StagedFile>,
    /// Decoded segments spooled so far for files still being fetched, by
    /// NZB file index: the per-file resume cursor. Entry `k` of a file's
    /// list is segment `k`'s record in NZB order, so a redelivery
    /// resumes at the first segment without one. Dropped when the file
    /// stages as one assembled object; kept (as the file's staged form)
    /// when the file is too large to assemble in memory.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spool: BTreeMap<usize, Vec<StagedSpoolSegment>>,
    /// The set the completed step dispatched (or would dispatch) repair
    /// for; `None` until every file is judged.
    #[serde(default)]
    pub par2_set_id: Option<String>,
    /// Blocks short in `par2_set_id`'s set once judged (0 before/complete).
    #[serde(default)]
    pub missing_blocks: u32,
}

/// The staged `in/manifest.json` document: the repair handoff's index
/// of one round's staged inputs. `staging_id` identifies this exact
/// staging of the round's bytes (repair idempotency binds to it — see
/// the core scratch contract): it is derived from every staged file's
/// content hash, so rewriting the manifest over identical bytes keeps
/// the id, while any byte change (a refetched file, a new delivery's
/// damage) mints a different one. Deterministic derivation needs no
/// stored state or RNG and can never reuse an id for other bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScratchManifest {
    pub staging_id: String,
    pub files: Vec<ManifestEntry>,
}

impl VerifyProgress {
    pub fn new(round: u32) -> Self {
        Self {
            round,
            ..Self::default()
        }
    }

    /// Scratch prefix for one round: `ingest-scratch/{job_id}/r{round}/`.
    pub fn scratch_prefix(job_id: &str, round: u32) -> String {
        format!("ingest-scratch/{job_id}/r{round}/")
    }

    /// Staged-input key for one file:
    /// `ingest-scratch/{job_id}/r{round}/in/{index:04}-{sha256(file_name)}`.
    /// Hashing the name keeps hostile NZB names out of key space
    /// entirely, and the NZB file index — deterministic across
    /// deliveries because every delivery re-parses the digest-pinned
    /// payload — keeps two files that share a name from sharing (or
    /// adopting) one staged object.
    pub fn file_key(job_id: &str, round: u32, file_index: usize, file_name: &str) -> String {
        format!(
            "{}in/{file_index:04}-{}",
            Self::scratch_prefix(job_id, round),
            content_sha256_hex(file_name.as_bytes())
        )
    }

    /// Manifest key for one round (next to `in/`, per the scratch contract).
    pub fn manifest_key(job_id: &str, round: u32) -> String {
        format!("{}in/manifest.json", Self::scratch_prefix(job_id, round))
    }

    /// Spool key for one decoded segment of one file:
    /// `ingest-scratch/{job_id}/r{round}/spool/{index:04}/{segment:06}`.
    /// Deterministic across deliveries, so a redelivery (or a lost
    /// progress write) finds earlier segments already at their keys.
    pub fn spool_key(job_id: &str, round: u32, file_index: usize, segment: usize) -> String {
        format!(
            "{}spool/{file_index:04}/{segment:06}",
            Self::scratch_prefix(job_id, round)
        )
    }

    /// Segment-index key for a file staged as its spool: carries the
    /// file's staged judgement (size/sha256/missing blocks/set id) in
    /// object metadata exactly like an assembled object's, with the
    /// ordered segment list as the body — so resume-trust and metadata
    /// adoption work for spooled files just like for assembled ones.
    pub fn spool_meta_key(job_id: &str, round: u32, file_index: usize) -> String {
        format!(
            "{}spool/{file_index:04}/index.json",
            Self::scratch_prefix(job_id, round)
        )
    }
}

/// What one Verifying step concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Every file healthy: the job advances to Matching.
    Complete,
    /// Blocks are missing and the set's recovery slices cover them:
    /// dispatch repair for this set.
    RepairNeeded {
        par2_set_id: String,
        missing_blocks: u32,
    },
    /// Blocks are missing beyond recovery (or no set covers them):
    /// fail the job. `par2_set_id` is `None` for uncovered damage.
    Unrecoverable {
        par2_set_id: Option<String>,
        missing_blocks: u32,
    },
    /// A per-delivery cap stopped the step with work left; progress is
    /// recorded and the next delivery resumes.
    Incomplete,
}

/// What one article fetch found. `TooLarge` (the pool's `413`, or the
/// NNTP client's own read limit) is per-article data like `Missing`:
/// the segment is judged absent, never a job failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArticleFetch {
    Present(Vec<u8>),
    Missing,
    TooLarge,
}

/// Article-body source: the pool Durable Object in production, a direct
/// NNTP client in tests.
pub trait BodySource {
    fn fetch_body<'a>(
        &'a mut self,
        segment_id: &'a str,
    ) -> impl Future<Output = Result<ArticleFetch, VerifyError>> + 'a;
}

/// Scratch object store: R2 in production, a map in tests. `head`
/// returns the metadata recorded at `put` time so a resume can trust a
/// staged record without re-reading the bytes.
pub trait ScratchStore {
    fn head<'a>(
        &'a self,
        key: &'a str,
    ) -> impl Future<Output = Result<Option<StagedFile>, VerifyError>> + 'a;
    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, VerifyError>> + 'a;
    fn put<'a>(
        &'a self,
        key: &'a str,
        bytes: Vec<u8>,
        meta: &'a StagedFile,
    ) -> impl Future<Output = Result<(), VerifyError>> + 'a;
}

/// One manifest row: the repair handoff's view of a staged file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub file_name: String,
    pub key: String,
    pub size: u64,
    /// `data` or `par2`.
    pub role: String,
    /// Spool-segment keys in file order when the file was staged as its
    /// segment spool (too large to assemble in-Worker): the handoff
    /// concatenates these (zero-filling each gap named in `missing`)
    /// instead of reading `key`'s bytes. Empty for an assembled file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<String>,
    /// Indexes (into `segments`) whose articles are gone: those byte
    /// ranges are part of the damage the repair side re-derives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<usize>,
}

/// What spooling one segment concluded.
enum SpoolOne {
    /// The segment is durably recorded: present (bytes in scratch) or
    /// answered missing/undecodable (record only). Either way the
    /// cursor advances past it.
    Fetched(StagedSpoolSegment),
    /// The article exceeds the per-article cap: unusable like a `430`,
    /// but nothing is recorded — spooling stops here and the file is
    /// judged with this and every later segment absent.
    Oversize,
}

/// How a fully spooled file's bytes are presented to the judgement.
enum LoadedFile {
    /// Assembled file bytes: gaps placed by the proven uniform part
    /// size and zero-filled, or just the contiguous present prefix when
    /// no part size is proven (see the module docs).
    Assembled(Vec<u8>),
    /// Too large to hold in memory: judged by streaming the spool.
    Spooled { logical_len: u64 },
}

struct Spend {
    files: usize,
    segments: usize,
    bytes: u64,
}

impl Spend {
    fn exhausted(&self, caps: &VerifyCaps) -> bool {
        self.files >= caps.max_files_per_delivery
            || self.segments >= caps.max_segments_per_delivery
            || self.bytes >= caps.max_bytes_per_delivery
    }
}

/// Run one Verifying step. See the module docs for the order, caps and
/// resume contract. `deadline_exceeded` is polled before each file and
/// segment (the consumer passes a wall-clock budget).
pub async fn run_verifying_step<B: BodySource, S: ScratchStore>(
    job_id: &str,
    files: &[ParsedFile],
    progress: &mut VerifyProgress,
    source: &mut B,
    scratch: &S,
    caps: &VerifyCaps,
    deadline_exceeded: &dyn Fn() -> bool,
) -> Result<VerifyOutcome, VerifyError> {
    if !validate_job_id(job_id) {
        return Err(VerifyError::Permanent("invalid job id".into()));
    }
    if files.is_empty() || files.len() > MAX_VERIFY_FILES {
        return Err(VerifyError::Permanent("file count out of bounds".into()));
    }
    let round = progress.round;
    let mut spend = Spend {
        files: 0,
        segments: 0,
        bytes: 0,
    };
    let mut dirty = false;

    // Par2 files first, then data files, each in NZB order.
    let order: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.is_par2)
        .chain(files.iter().enumerate().filter(|(_, f)| !f.is_par2))
        .map(|(i, _)| i)
        .collect();

    let mut sets: Vec<RecoverySet> = Vec::new();
    let mut sets_loaded = false;
    let mut claimed: HashSet<[u8; 16]> = HashSet::new();

    for &idx in &order {
        let file = &files[idx];
        // Once the first data file starts, the par2 sets must be parsed;
        // entries judged on earlier deliveries stay claimed.
        if !file.is_par2 && !sets_loaded {
            sets = load_sets(files, progress, scratch, caps).await?;
            sets_loaded = true;
            claim_staged(&sets, files, progress, &mut claimed);
        }

        if let Some(existing) = progress.staged.get(&idx).cloned() {
            // Resume trust: the staged object must still be there with
            // the recorded hash, else the file is fetched again below.
            match scratch.head(&existing.key).await? {
                Some(meta) if meta.sha256 == existing.sha256 && meta.size == existing.size => {
                    continue;
                }
                _ => {
                    progress.staged.remove(&idx);
                    // `claim_staged` claimed this file's par2 entry from
                    // the record just evicted: recompute the claims
                    // without it, or the refetch below would find its
                    // own entry taken and be judged as uncovered.
                    claimed = HashSet::new();
                    claim_staged(&sets, files, progress, &mut claimed);
                    dirty = true;
                }
            }
        } else {
            // No record (first delivery, or a lost progress write): a
            // staged object from an earlier delivery carries its own
            // judgement in its metadata — adopt it instead of refetching.
            let key = VerifyProgress::file_key(job_id, round, idx, &file.name);
            if let Some(meta) = scratch.head(&key).await? {
                if meta.key == key {
                    progress.staged.insert(idx, meta);
                    if !file.is_par2 {
                        claim_staged(&sets, files, progress, &mut claimed);
                    }
                    dirty = true;
                    continue;
                }
            }
            // A file staged as its spool: adopt the segment index (its
            // body is the full staged record, its metadata the same
            // judgement an assembled object carries).
            let meta_key = VerifyProgress::spool_meta_key(job_id, round, idx);
            let mut adopted_spooled = false;
            if let Some(meta) = scratch.head(&meta_key).await? {
                if meta.key == meta_key {
                    if let Some(body) = scratch.get(&meta_key).await? {
                        if let Ok(mut adopted) = serde_json::from_slice::<StagedFile>(&body) {
                            if adopted.key == meta_key
                                && adopted.sha256 == meta.sha256
                                && adopted.size == meta.size
                            {
                                adopted.missing_blocks = meta.missing_blocks;
                                adopted.par2_set_id = meta.par2_set_id.clone();
                                progress.staged.insert(idx, adopted);
                                if !file.is_par2 {
                                    claim_staged(&sets, files, progress, &mut claimed);
                                }
                                dirty = true;
                                adopted_spooled = true;
                            }
                        }
                    }
                }
            }
            if adopted_spooled {
                continue;
            }
            // Adopt a half-spooled file whose progress write was lost:
            // the cursor is rebuilt by probing the deterministic spool
            // keys until one is absent.
            #[allow(clippy::map_entry)]
            if !progress.spool.contains_key(&idx) {
                let mut adopted = Vec::new();
                for n in 0..file.segment_ids.len() {
                    let key = VerifyProgress::spool_key(job_id, round, idx, n);
                    match scratch.head(&key).await? {
                        Some(meta) => adopted.push(StagedSpoolSegment {
                            key: meta.key,
                            size: meta.size,
                            crc32: meta.crc32,
                            missing: meta.missing,
                        }),
                        None => break,
                    }
                }
                if !adopted.is_empty() {
                    progress.spool.insert(idx, adopted);
                    dirty = true;
                }
            }
        }

        if spend.exhausted(caps) || deadline_exceeded() {
            write_manifest(job_id, round, files, progress, scratch, dirty).await?;
            return Ok(VerifyOutcome::Incomplete);
        }

        // Validate the resume cursor against scratch: a vanished spool
        // object truncates it, so that segment (and everything after
        // it) refetches below instead of being trusted sight unseen.
        if let Some(spool) = progress.spool.get(&idx).cloned() {
            let mut valid = spool.len();
            for (n, rec) in spool.iter().enumerate() {
                if !rec.missing && scratch.head(&rec.key).await?.is_none() {
                    valid = n;
                    break;
                }
            }
            if valid < spool.len() {
                progress
                    .spool
                    .get_mut(&idx)
                    .expect("cursor")
                    .truncate(valid);
                dirty = true;
            }
        }

        // Spool the remaining segments, resuming at the cursor. A cap
        // or the deadline stops mid-file with every fetched segment
        // already durable in scratch and recorded in the cursor.
        let total_segments = file.segment_ids.len();
        let mut stopped = false;
        loop {
            let have = progress.spool.get(&idx).map_or(0, Vec::len);
            if have >= total_segments {
                break;
            }
            if spend.segments >= caps.max_segments_per_delivery
                || spend.bytes >= caps.max_bytes_per_delivery
                || deadline_exceeded()
            {
                stopped = true;
                break;
            }
            match spool_one(
                job_id,
                round,
                idx,
                have,
                &file.segment_ids[have],
                source,
                scratch,
                caps,
                &mut spend,
            )
            .await?
            {
                SpoolOne::Fetched(rec) => {
                    progress.spool.entry(idx).or_default().push(rec);
                    dirty = true;
                }
                // An oversize article is unusable like a `430`: stop
                // spooling and judge the file with this and every later
                // segment absent (counted in the block judgement).
                SpoolOne::Oversize => break,
            }
        }
        if stopped {
            write_manifest(job_id, round, files, progress, scratch, dirty).await?;
            return Ok(VerifyOutcome::Incomplete);
        }
        spend.files += 1;
        let segs = progress.spool.get(&idx).cloned().unwrap_or_default();

        let matched = if file.is_par2 {
            None
        } else {
            match_set_file(&sets, file, &segs, total_segments, &mut claimed)
        };
        let expected_len = matched.map(|(_, f)| f.length);
        let seg_crcs: Vec<(u32, u64)> = segs.iter().map(seg_crc_len).collect();
        let missing_segments = {
            let answered = segs.iter().filter(|s| s.missing).count();
            let unfetched = total_segments.saturating_sub(segs.len());
            (answered + unfetched) as u32
        };

        let load = load_segments(&segs, total_segments, expected_len, caps, scratch).await?;
        let Some(load) = load else {
            // The implied geometry overflows u64 (or exceeds the walk
            // bound): the par2 length and the part sizes disagree
            // beyond any real file.
            return Err(VerifyError::Permanent("file geometry out of bounds".into()));
        };

        // `None` from the spooled arm means the streaming judgement hit
        // the delivery deadline: stop the step with the spool durable
        // and judge again next delivery.
        let staged = match load {
            LoadedFile::Assembled(bytes) => {
                let (missing_blocks, par2_set_id) = if file.is_par2 {
                    (0, None)
                } else {
                    judge_file(matched, missing_segments, &seg_crcs, &bytes)
                };
                let key = VerifyProgress::file_key(job_id, round, idx, &file.name);
                let staged = StagedFile {
                    size: bytes.len() as u64,
                    sha256: content_sha256_hex(&bytes),
                    key: key.clone(),
                    missing_blocks,
                    par2_set_id,
                    spool: Vec::new(),
                    crc32: 0,
                    missing: false,
                };
                scratch.put(&key, bytes, &staged).await?;
                // The assembled object supersedes the spool cursor (the
                // spool objects themselves expire with the bucket).
                progress.spool.remove(&idx);
                Some(staged)
            }
            LoadedFile::Spooled { logical_len } => {
                let judged = if file.is_par2 {
                    spool_sha256(
                        &segs,
                        total_segments,
                        logical_len,
                        scratch,
                        deadline_exceeded,
                    )
                    .await?
                    .map(|sha| (0, None, sha))
                } else {
                    judge_spooled(
                        matched,
                        missing_segments,
                        &seg_crcs,
                        &segs,
                        total_segments,
                        logical_len,
                        scratch,
                        deadline_exceeded,
                    )
                    .await?
                };
                match judged {
                    None => None,
                    Some((missing_blocks, par2_set_id, sha256)) => {
                        let key = VerifyProgress::spool_meta_key(job_id, round, idx);
                        let staged = StagedFile {
                            size: logical_len,
                            sha256,
                            key: key.clone(),
                            missing_blocks,
                            par2_set_id,
                            spool: segs.clone(),
                            crc32: 0,
                            missing: false,
                        };
                        let body = serde_json::to_vec(&staged).map_err(|_| {
                            VerifyError::Transient("spool index encode failed".into())
                        })?;
                        scratch.put(&key, body, &staged).await?;
                        Some(staged)
                    }
                }
            }
        };
        let Some(staged) = staged else {
            write_manifest(job_id, round, files, progress, scratch, dirty).await?;
            return Ok(VerifyOutcome::Incomplete);
        };
        progress.staged.insert(idx, staged);
        dirty = true;
    }

    write_manifest(job_id, round, files, progress, scratch, dirty).await?;
    Ok(finalize(files, &sets, progress))
}

/// Fetch, yEnc-decode and spool one segment. Present bytes go to scratch
/// immediately (with their CRC32 in the object record, so a lost
/// progress write can rebuild the cursor from metadata alone).
#[allow(clippy::too_many_arguments)]
async fn spool_one<B: BodySource, S: ScratchStore>(
    job_id: &str,
    round: u32,
    file_index: usize,
    segment: usize,
    segment_id: &str,
    source: &mut B,
    scratch: &S,
    caps: &VerifyCaps,
    spend: &mut Spend,
) -> Result<SpoolOne, VerifyError> {
    spend.segments += 1;
    let key = VerifyProgress::spool_key(job_id, round, file_index, segment);
    let missing_rec = StagedSpoolSegment {
        key: key.clone(),
        size: 0,
        crc32: 0,
        missing: true,
    };
    let article = match source.fetch_body(segment_id).await? {
        ArticleFetch::Present(article) => article,
        ArticleFetch::Missing => return Ok(SpoolOne::Fetched(missing_rec)),
        // The source refused the article for size (pool `413` / client
        // read limit): same oversize stop as the in-Worker cap below.
        ArticleFetch::TooLarge => return Ok(SpoolOne::Oversize),
    };
    spend.bytes = spend.bytes.saturating_add(article.len() as u64);
    if article.len() > caps.max_article_bytes {
        return Ok(SpoolOne::Oversize);
    }
    let Some(bytes) = decode_segment(segment_id, &article) else {
        return Ok(SpoolOne::Fetched(missing_rec));
    };
    let rec = StagedSpoolSegment {
        key: key.clone(),
        size: bytes.len() as u64,
        crc32: crc32fast::hash(&bytes),
        missing: false,
    };
    let meta = StagedFile {
        size: rec.size,
        sha256: content_sha256_hex(&bytes),
        key: key.clone(),
        missing_blocks: 0,
        par2_set_id: None,
        spool: Vec::new(),
        crc32: rec.crc32,
        missing: false,
    };
    scratch.put(&key, bytes, &meta).await?;
    Ok(SpoolOne::Fetched(rec))
}

/// Decode one yEnc article. Any decode or CRC failure makes the segment
/// unusable (`None`): par2 accounting treats corrupt bytes like a `430`.
fn decode_segment(segment_id: &str, article: &[u8]) -> Option<Vec<u8>> {
    // Decoded output is never larger than the encoded article (yEnc
    // expands, never compresses), so the article length bounds `dst`.
    let mut dst = vec![0u8; article.len()];
    let written = nntune_core::yenc::decode_to(segment_id, article, &mut dst).ok()?;
    dst.truncate(written);
    Some(dst)
}

/// `(crc32, len)` of one spooled segment for the par2 precheck; a
/// missing segment contributes nothing.
fn seg_crc_len(seg: &StagedSpoolSegment) -> (u32, u64) {
    if seg.missing {
        (0, 0)
    } else {
        (seg.crc32, seg.size)
    }
}

/// The uniform decoded part size a file's present segments prove: every
/// present segment except the file's last must share one length, and at
/// least one such segment must be present to prove it.
fn uniform_part_size(segs: &[StagedSpoolSegment], total_segments: usize) -> Option<u64> {
    let non_last = total_segments.saturating_sub(1);
    let mut size: Option<u64> = None;
    for (i, seg) in segs.iter().enumerate() {
        if i >= non_last {
            break;
        }
        if seg.missing {
            continue;
        }
        match size {
            None => size = Some(seg.size),
            Some(s) if s != seg.size => return None,
            _ => {}
        }
    }
    size.filter(|s| *s > 0)
}

/// The file's logical length: the par2-declared length when known, else
/// the geometry the spooled segments imply. `None` only on u64 overflow
/// (a par2 length and part geometry that cannot describe a real file).
fn implied_len(
    segs: &[StagedSpoolSegment],
    total_segments: usize,
    expected_len: Option<u64>,
) -> Option<u64> {
    if total_segments == 0 {
        return Some(0);
    }
    if let Some(len) = expected_len {
        return Some(len);
    }
    if segs.len() >= total_segments && segs.iter().all(|s| !s.missing) {
        return segs.iter().try_fold(0u64, |acc, s| acc.checked_add(s.size));
    }
    let part_size = uniform_part_size(segs, total_segments)?;
    let last_len = match segs.get(total_segments - 1) {
        Some(s) if !s.missing => s.size,
        _ => part_size,
    };
    part_size
        .checked_mul((total_segments - 1) as u64)?
        .checked_add(last_len)
}

/// Present a fully spooled file for judging: assembled in memory when
/// the implied length fits `max_file_bytes`, spooled (streamed) when it
/// does not. The implied length is checked *before* any allocation: it
/// derives from the untrusted par2 length and from part geometry over
/// mostly-missing segments, neither of which the fetched-bytes caps
/// bound. A spool object that vanished after cursor validation reads as
/// zeros here (counted as damage by the block judgement, never as a
/// pass).
async fn load_segments<S: ScratchStore>(
    segs: &[StagedSpoolSegment],
    total_segments: usize,
    expected_len: Option<u64>,
    caps: &VerifyCaps,
    scratch: &S,
) -> Result<Option<LoadedFile>, VerifyError> {
    if segs.len() >= total_segments && segs.iter().all(|s| !s.missing) {
        let Some(total) = segs.iter().try_fold(0u64, |acc, s| acc.checked_add(s.size)) else {
            return Ok(None);
        };
        if total > caps.max_logical_bytes {
            return Ok(None);
        }
        if total > caps.max_file_bytes || usize::try_from(total).is_err() {
            return Ok(Some(LoadedFile::Spooled { logical_len: total }));
        }
        let mut out = Vec::with_capacity(total as usize);
        for seg in segs {
            match scratch.get(&seg.key).await? {
                Some(bytes) => out.extend_from_slice(&bytes),
                None => out.resize(out.len() + seg.size as usize, 0),
            }
        }
        return Ok(Some(LoadedFile::Assembled(out)));
    }
    let Some(part_size) = uniform_part_size(segs, total_segments) else {
        // No proven part size: only the contiguous present prefix can
        // vouch for blocks, and it is bounded by fetched bytes.
        let mut out = Vec::new();
        for seg in segs {
            if seg.missing {
                break;
            }
            match scratch.get(&seg.key).await? {
                Some(bytes) => out.extend_from_slice(&bytes),
                None => break,
            }
        }
        return Ok(Some(LoadedFile::Assembled(out)));
    };
    let Some(total) = implied_len(segs, total_segments, expected_len) else {
        return Ok(None);
    };
    if total > caps.max_logical_bytes {
        return Ok(None);
    }
    if total > caps.max_file_bytes || usize::try_from(total).is_err() {
        return Ok(Some(LoadedFile::Spooled { logical_len: total }));
    }
    let mut out = vec![0u8; total as usize];
    for (i, seg) in segs.iter().enumerate() {
        if seg.missing {
            continue;
        }
        let Some(bytes) = scratch.get(&seg.key).await? else {
            continue;
        };
        // `total` fits `usize` here (checked above), so the part
        // offset — bounded by `total` for any in-file segment — does
        // too once compared in u64 first.
        let start = i as u64 * part_size;
        if start >= total {
            break;
        }
        let start = start as usize;
        let take = bytes.len().min(out.len() - start);
        out[start..start + take].copy_from_slice(&bytes[..take]);
    }
    Ok(Some(LoadedFile::Assembled(out)))
}

fn feed_zeros(mut n: u64, feed: &mut dyn FnMut(&[u8])) {
    const ZEROS: [u8; 8192] = [0u8; 8192];
    while n > 0 {
        // Compare in u64 before narrowing: `n as usize` truncates on
        // wasm32 and would spin forever at exact 4 GiB multiples.
        let take = n.min(ZEROS.len() as u64) as usize;
        feed(&ZEROS[..take]);
        n -= take as u64;
    }
}

/// What one [`stream_logical`] walk did.
enum Streamed {
    /// Walked all `logical_len` bytes. `vanished` records a present
    /// spool object that disappeared after cursor validation (its
    /// range streamed as zeros): the streamed bytes then no longer
    /// match the decode-time records, so a record-based healthy proof
    /// must not be trusted.
    Done { vanished: bool },
    /// The delivery deadline stopped the walk; judge again next
    /// delivery (streaming progress itself is not resumable).
    Stopped,
}

/// Feed a zero run in bounded pieces, polling the deadline between
/// them. Returns false when the deadline stopped the run early.
fn feed_zeros_polled(
    mut n: u64,
    feed: &mut dyn FnMut(&[u8]),
    deadline_exceeded: &dyn Fn() -> bool,
) -> bool {
    const PIECE: u64 = 1 << 20;
    while n > 0 {
        if deadline_exceeded() {
            return false;
        }
        let take = n.min(PIECE);
        feed_zeros(take, feed);
        n -= take;
    }
    true
}

/// Stream a spooled file's logical bytes in order: present segments at
/// their part offsets (concatenated when every segment is present),
/// every gap — missing segment, vanished spool object, tail past the
/// last spooled segment — as zeros, truncated at `logical_len`. This is
/// exactly the byte layout [`load_segments`] materializes for an
/// assembled file, so streaming and in-memory judgements agree. The
/// deadline is polled between segments and inside long zero runs: a
/// lying (untrusted) logical length streams zeros only until the
/// delivery budget ends, never unboundedly.
async fn stream_logical<S: ScratchStore>(
    segs: &[StagedSpoolSegment],
    total_segments: usize,
    logical_len: u64,
    scratch: &S,
    deadline_exceeded: &dyn Fn() -> bool,
    feed: &mut dyn FnMut(&[u8]),
) -> Result<Streamed, VerifyError> {
    let all_present = segs.len() >= total_segments && segs.iter().all(|s| !s.missing);
    let part_size = if all_present {
        0
    } else {
        uniform_part_size(segs, total_segments).unwrap_or(0)
    };
    let mut pos: u64 = 0;
    let mut vanished = false;
    for (i, seg) in segs.iter().enumerate() {
        if pos >= logical_len {
            break;
        }
        if deadline_exceeded() {
            return Ok(Streamed::Stopped);
        }
        let start = if all_present {
            pos
        } else {
            i as u64 * part_size
        };
        if start > pos {
            let gap = (start - pos).min(logical_len - pos);
            if !feed_zeros_polled(gap, feed, deadline_exceeded) {
                return Ok(Streamed::Stopped);
            }
            pos += gap;
            if pos >= logical_len {
                break;
            }
        }
        if seg.missing {
            continue;
        }
        let Some(bytes) = scratch.get(&seg.key).await? else {
            // Vanished after cursor validation: zero-fill its range so
            // every later byte keeps its logical offset (the assembled
            // path does the same), and flag it for the judgement.
            vanished = true;
            if all_present {
                let fill = seg.size.min(logical_len - pos);
                if !feed_zeros_polled(fill, feed, deadline_exceeded) {
                    return Ok(Streamed::Stopped);
                }
                pos += fill;
            }
            continue;
        };
        // Overlap guard: bytes before `pos` were already fed (cannot
        // happen under a proven uniform part size, but keeps every
        // logical byte fed exactly once by construction).
        let skip = pos.saturating_sub(start) as usize;
        if skip >= bytes.len() {
            continue;
        }
        // Compare in u64 before narrowing: `(logical_len - pos) as
        // usize` truncates on wasm32 for files over 4 GiB.
        let take = ((bytes.len() - skip) as u64).min(logical_len - pos) as usize;
        feed(&bytes[skip..skip + take]);
        pos += take as u64;
    }
    if pos < logical_len && !feed_zeros_polled(logical_len - pos, feed, deadline_exceeded) {
        return Ok(Streamed::Stopped);
    }
    Ok(Streamed::Done { vanished })
}

/// Incremental SHA-256 of a spooled file's logical bytes (`None` when
/// the deadline stopped the walk).
async fn spool_sha256<S: ScratchStore>(
    segs: &[StagedSpoolSegment],
    total_segments: usize,
    logical_len: u64,
    scratch: &S,
    deadline_exceeded: &dyn Fn() -> bool,
) -> Result<Option<String>, VerifyError> {
    let mut hasher = Sha256::new();
    let streamed = stream_logical(
        segs,
        total_segments,
        logical_len,
        scratch,
        deadline_exceeded,
        &mut |b| hasher.update(b),
    )
    .await?;
    Ok(match streamed {
        Streamed::Done { .. } => Some(hex::encode(hasher.finalize())),
        Streamed::Stopped => None,
    })
}

/// Streaming counterpart of [`count_bad_blocks`]: par2 blocks are
/// CRC-checked (zero-padded, per the spec) as their bytes stream past,
/// so memory never scales with file or block size.
struct BlockCounter<'a> {
    block_crc32s: &'a [u32],
    block_size: u64,
    hasher: crc32fast::Hasher,
    in_block: u64,
    index: usize,
    bad: u32,
}

impl BlockCounter<'_> {
    fn update(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            if self.index >= self.block_crc32s.len() {
                return;
            }
            let room = (self.block_size - self.in_block) as usize;
            let take = room.min(bytes.len());
            self.hasher.update(&bytes[..take]);
            self.in_block += take as u64;
            bytes = &bytes[take..];
            if self.in_block == self.block_size {
                self.finish_block();
            }
        }
    }

    fn finish_block(&mut self) {
        let want = self.block_crc32s[self.index];
        let got = std::mem::replace(&mut self.hasher, crc32fast::Hasher::new()).finalize();
        if got != want {
            self.bad += 1;
        }
        self.index += 1;
        self.in_block = 0;
    }

    fn finish(mut self) -> u32 {
        if self.index < self.block_crc32s.len() && self.in_block > 0 {
            let pad = self.block_size - self.in_block;
            feed_zeros(pad, &mut |z| self.hasher.update(z));
            self.finish_block();
        }
        // Blocks the stream never reached count as bad, like blocks past
        // the assembled bytes in `count_bad_blocks`.
        self.bad + (self.block_crc32s.len() - self.index) as u32
    }
}

/// Streaming counterpart of [`judge_file`] for a file too large to
/// assemble: same cheapest-proof-first order (segment-CRC precheck,
/// then per-block CRCs), plus the file's SHA-256 for its staged record.
/// `None` when the deadline stopped the walk (judge again next
/// delivery).
#[allow(clippy::too_many_arguments)]
async fn judge_spooled<S: ScratchStore>(
    matched: Option<(&RecoverySet, &RecoveryFile)>,
    missing_segments: u32,
    seg_crcs: &[(u32, u64)],
    segs: &[StagedSpoolSegment],
    total_segments: usize,
    logical_len: u64,
    scratch: &S,
    deadline_exceeded: &dyn Fn() -> bool,
) -> Result<Option<(u32, Option<String>, String)>, VerifyError> {
    let mut hasher = Sha256::new();
    let (missing_blocks, par2_set_id) = match matched {
        None => {
            match stream_logical(
                segs,
                total_segments,
                logical_len,
                scratch,
                deadline_exceeded,
                &mut |b| hasher.update(b),
            )
            .await?
            {
                Streamed::Done { .. } => {}
                Streamed::Stopped => return Ok(None),
            }
            (missing_segments, None)
        }
        Some((set, entry)) => {
            let set_hex = hex::encode(set.set_id);
            let healthy = missing_segments == 0
                && set
                    .precheck_bytes(&entry.filename_bytes, seg_crcs)
                    .unwrap_or(false);
            if healthy {
                match stream_logical(
                    segs,
                    total_segments,
                    logical_len,
                    scratch,
                    deadline_exceeded,
                    &mut |b| hasher.update(b),
                )
                .await?
                {
                    // The precheck vouches for the decode-time bytes;
                    // trust it only while the streamed bytes are those
                    // same bytes (no spool object vanished en route).
                    Streamed::Done { vanished: false } => (0, Some(set_hex)),
                    Streamed::Stopped => return Ok(None),
                    Streamed::Done { vanished: true } => {
                        if set.block_size == 0 || logical_len > entry.length {
                            (entry.block_crc32s.len() as u32, Some(set_hex))
                        } else {
                            // Re-walk counting blocks over the bytes
                            // actually staged: the vanished range reads
                            // as zeros and counts as damage. The SHA
                            // from the first walk already covers
                            // exactly those bytes.
                            let mut counter = BlockCounter {
                                block_crc32s: &entry.block_crc32s,
                                block_size: set.block_size,
                                hasher: crc32fast::Hasher::new(),
                                in_block: 0,
                                index: 0,
                                bad: 0,
                            };
                            let streamed = stream_logical(
                                segs,
                                total_segments,
                                logical_len,
                                scratch,
                                deadline_exceeded,
                                &mut |b| counter.update(b),
                            )
                            .await?;
                            match streamed {
                                Streamed::Done { .. } => (counter.finish(), Some(set_hex)),
                                Streamed::Stopped => return Ok(None),
                            }
                        }
                    }
                }
            } else if set.block_size == 0 || logical_len > entry.length {
                // An over-long file trusts nothing (as in
                // `count_bad_blocks`): every block counts.
                match stream_logical(
                    segs,
                    total_segments,
                    logical_len,
                    scratch,
                    deadline_exceeded,
                    &mut |b| hasher.update(b),
                )
                .await?
                {
                    Streamed::Done { .. } => {}
                    Streamed::Stopped => return Ok(None),
                }
                (entry.block_crc32s.len() as u32, Some(set_hex))
            } else {
                let mut counter = BlockCounter {
                    block_crc32s: &entry.block_crc32s,
                    block_size: set.block_size,
                    hasher: crc32fast::Hasher::new(),
                    in_block: 0,
                    index: 0,
                    bad: 0,
                };
                let streamed = stream_logical(
                    segs,
                    total_segments,
                    logical_len,
                    scratch,
                    deadline_exceeded,
                    &mut |b| {
                        hasher.update(b);
                        counter.update(b);
                    },
                )
                .await?;
                match streamed {
                    Streamed::Done { .. } => (counter.finish(), Some(set_hex)),
                    Streamed::Stopped => return Ok(None),
                }
            }
        }
    };
    Ok(Some((
        missing_blocks,
        par2_set_id,
        hex::encode(hasher.finalize()),
    )))
}

/// Parse the staged par2 files into recovery sets, grouped by set id.
/// Every staged object's packets stream into its set's
/// [`RecoverySetBuilder`] (routed by the first packet's set id, fully
/// validated by the builder), so a volume carrying only recovery slices
/// contributes them to its set without parsing standalone, and restore
/// memory is one staged object (or one spooled segment) at a time —
/// aggregate staged par2 is never capped. A single *packet* beyond
/// `caps.max_par2_bytes` cannot be buffered for parsing; its object is
/// then unusable in-Worker and its slices are simply absent, which the
/// recoverability check sees (never a false "recoverable", and never a
/// permanent failure of an otherwise judgeable release). A stream
/// that fails validation drops only itself, never the set.
async fn load_sets<S: ScratchStore>(
    files: &[ParsedFile],
    progress: &VerifyProgress,
    scratch: &S,
    caps: &VerifyCaps,
) -> Result<Vec<RecoverySet>, VerifyError> {
    let mut builders: BTreeMap<[u8; 16], RecoverySetBuilder> = BTreeMap::new();
    let mut any_par2 = false;
    for (idx, file) in files.iter().enumerate() {
        if !file.is_par2 {
            continue;
        }
        any_par2 = true;
        let Some(staged) = progress.staged.get(&idx) else {
            continue;
        };
        if staged.spool.is_empty() {
            let Some(bytes) = scratch.get(&staged.key).await? else {
                continue;
            };
            feed_par2_bytes(&mut builders, &bytes, caps.max_par2_bytes);
            continue;
        }
        // Spooled par2: stream segment by segment, carrying a packet
        // split across segment boundaries in `pending`.
        let mut pending: Vec<u8> = Vec::new();
        let mut usable = true;
        for seg in &staged.spool {
            if seg.missing {
                usable = false;
                break;
            }
            let Some(bytes) = scratch.get(&seg.key).await? else {
                usable = false;
                break;
            };
            pending.extend_from_slice(&bytes);
            let complete = complete_packet_prefix(&pending);
            if complete > 0 {
                feed_par2_bytes(&mut builders, &pending[..complete], caps.max_par2_bytes);
                pending.drain(..complete);
            }
            if pending.len() as u64 > caps.max_par2_bytes {
                usable = false;
                break;
            }
        }
        if usable && !pending.is_empty() {
            feed_par2_bytes(&mut builders, &pending, caps.max_par2_bytes);
        }
    }
    if !any_par2 {
        return Ok(Vec::new());
    }
    let mut sets = Vec::new();
    for builder in builders.into_values() {
        if let Ok(set) = builder.finish() {
            sets.push(set);
        }
    }
    sets.sort_by_key(|s| s.set_id);
    Ok(sets)
}

/// Route one packet-aligned byte stream into its set's builder. Streams
/// that do not start with a packet header, contain a packet beyond the
/// buffered-packet bound, or fail validation are dropped — a stream
/// that starts well-formed then contradicts itself cannot be partially
/// trusted, but damage is contained to that stream (per-object, as the
/// previous parse-at-once restore contained it).
fn feed_par2_bytes(
    builders: &mut BTreeMap<[u8; 16], RecoverySetBuilder>,
    bytes: &[u8],
    max_packet: u64,
) {
    let Some(set_id) = peek_set_id(bytes) else {
        return;
    };
    if !packets_within_bound(bytes, max_packet) {
        return;
    }
    // Validate-then-commit on a clone (metadata only, so the copy is
    // small): a stream that contradicts itself part-way drops *itself*,
    // never the good packets other volumes already contributed — one
    // corrupt volume must not silence the whole set's recovery.
    let mut trial = builders.get(&set_id).cloned().unwrap_or_default();
    if trial.add_stream(bytes).is_ok() {
        builders.insert(set_id, trial);
    }
}

/// Length of the longest prefix of `bytes` made of whole par2 packets
/// (by their declared lengths; the builder validates the contents).
fn complete_packet_prefix(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    while offset + 16 <= bytes.len() {
        let len = u64::from_le_bytes(bytes[offset + 8..offset + 16].try_into().unwrap());
        if len < 64 || len % 4 != 0 {
            break;
        }
        let Ok(len) = usize::try_from(len) else {
            break;
        };
        let Some(next) = offset.checked_add(len) else {
            break;
        };
        if next > bytes.len() {
            break;
        }
        offset = next;
    }
    offset
}

/// Whether `bytes` is a whole number of packets, each within the
/// buffered-packet bound.
fn packets_within_bound(bytes: &[u8], max_packet: u64) -> bool {
    let mut offset = 0usize;
    while offset < bytes.len() {
        if offset + 16 > bytes.len() {
            return false;
        }
        let len = u64::from_le_bytes(bytes[offset + 8..offset + 16].try_into().unwrap());
        if len > max_packet || len < 64 || len % 4 != 0 {
            return false;
        }
        let Ok(len) = usize::try_from(len) else {
            return false;
        };
        let Some(next) = offset.checked_add(len) else {
            return false;
        };
        if next > bytes.len() {
            return false;
        }
        offset = next;
    }
    true
}

/// Find the protected recovery-file entry covering one NZB file: exact
/// name first, then — for obfuscated names — a unique length match
/// against the assembled length. Claims the entry so two NZB files
/// cannot both match it.
fn match_set_file<'a>(
    sets: &'a [RecoverySet],
    file: &ParsedFile,
    segs: &[StagedSpoolSegment],
    total_segments: usize,
    claimed: &mut HashSet<[u8; 16]>,
) -> Option<(&'a RecoverySet, &'a RecoveryFile)> {
    for set in sets {
        if let Some(entry) = set.protected_file_by_name(&file.name) {
            if claimed.insert(entry.file_id) {
                return Some((set, entry));
            }
        }
    }
    // Length fallback needs the true assembled length: only meaningful
    // when every segment decoded.
    if segs.len() >= total_segments && segs.iter().all(|s| !s.missing) {
        let len: u64 = segs.iter().map(|s| s.size).sum();
        let mut found: Option<(&RecoverySet, &RecoveryFile)> = None;
        for set in sets {
            for entry in set.files.values() {
                if entry.protected
                    && entry.has_file_desc
                    && entry.length == len
                    && !claimed.contains(&entry.file_id)
                {
                    if found.is_some() {
                        return None; // ambiguous: no match
                    }
                    found = Some((set, entry));
                }
            }
        }
        if let Some((set, entry)) = found {
            claimed.insert(entry.file_id);
            return Some((set, entry));
        }
    }
    None
}

/// Claim the recovery entries of data files judged on earlier
/// deliveries, so the length fallback cannot reassign them on resume.
fn claim_staged(
    sets: &[RecoverySet],
    files: &[ParsedFile],
    progress: &VerifyProgress,
    claimed: &mut HashSet<[u8; 16]>,
) {
    for (idx, file) in files.iter().enumerate() {
        if file.is_par2 {
            continue;
        }
        let Some(staged) = progress.staged.get(&idx) else {
            continue;
        };
        let Some(set_hex) = &staged.par2_set_id else {
            continue;
        };
        // A combined dispatch id (`setA+setB`) claims each listed set's
        // entry for this file the same way a single id does.
        let mut candidates: Vec<&str> = set_hex.split('+').collect();
        if candidates.is_empty() {
            candidates.push(set_hex.as_str());
        }
        for set in sets {
            if !candidates.contains(&hex::encode(set.set_id).as_str()) {
                continue;
            }
            if let Some(entry) = set.protected_file_by_name(&file.name) {
                claimed.insert(entry.file_id);
            } else if let Some(entry) = set
                .files
                .values()
                .find(|e| e.protected && e.has_file_desc && e.length == staged.size)
            {
                claimed.insert(entry.file_id);
            }
        }
    }
}

/// Judge one assembled data file: `(missing_blocks, covering set id)`.
/// Takes the per-segment facts (missing count, decode-time CRC32s)
/// captured before assembly consumed the segments.
fn judge_file(
    matched: Option<(&RecoverySet, &RecoveryFile)>,
    missing_segments: u32,
    seg_crcs: &[(u32, u64)],
    assembled: &[u8],
) -> (u32, Option<String>) {
    let Some((set, entry)) = matched else {
        // No set covers this file: healthy iff every segment decoded.
        return (missing_segments, None);
    };
    let set_hex = hex::encode(set.set_id);
    if missing_segments == 0 {
        // Cheapest proof first: precheck recombines decode-time CRCs;
        // verify rehashes only when precheck cannot vouch for the file.
        if set
            .precheck_bytes(&entry.filename_bytes, seg_crcs)
            .unwrap_or(false)
            || set.verify_bytes(&entry.filename_bytes, assembled).is_ok()
        {
            return (0, Some(set_hex));
        }
    }
    (count_bad_blocks(set, entry, assembled), Some(set_hex))
}

/// Count par2 blocks of `entry` that `assembled` cannot vouch for:
/// blocks whose zero-padded CRC32 disagrees, plus every block past the
/// assembled bytes. An over-long assembly trusts nothing.
fn count_bad_blocks(set: &RecoverySet, entry: &RecoveryFile, assembled: &[u8]) -> u32 {
    let total = entry.block_crc32s.len() as u32;
    if set.block_size == 0 || assembled.len() as u64 > entry.length {
        return total;
    }
    let bs = set.block_size as usize;
    let mut bad = 0u32;
    for (i, &want) in entry.block_crc32s.iter().enumerate() {
        let start = i * bs;
        if start >= assembled.len() {
            bad += 1;
            continue;
        }
        let end = (start + bs).min(assembled.len());
        let mut h = crc32fast::Hasher::new();
        h.update(&assembled[start..end]);
        if end - start < bs {
            // Zero-pad in fixed chunks: `bs` is parse-capped only at
            // 256 MiB, so padding must never allocate proportionally.
            const ZEROS: [u8; 8192] = [0u8; 8192];
            let mut pad = bs - (end - start);
            while pad > 0 {
                let take = pad.min(ZEROS.len());
                h.update(&ZEROS[..take]);
                pad -= take;
            }
        }
        if h.finalize() != want {
            bad += 1;
        }
    }
    bad
}

/// Aggregate the per-file judgements once every file is staged.
fn finalize(
    files: &[ParsedFile],
    sets: &[RecoverySet],
    progress: &mut VerifyProgress,
) -> VerifyOutcome {
    let recovery_by_set: HashMap<String, u32> = sets
        .iter()
        .map(|s| (hex::encode(s.set_id), s.recovery_blocks as u32))
        .collect();
    let mut missing_by_set: BTreeMap<String, u32> = BTreeMap::new();
    for (idx, file) in files.iter().enumerate() {
        if file.is_par2 {
            continue;
        }
        let Some(staged) = progress.staged.get(&idx) else {
            return VerifyOutcome::Incomplete;
        };
        if staged.missing_blocks == 0 {
            continue;
        }
        match &staged.par2_set_id {
            None => {
                progress.par2_set_id = None;
                progress.missing_blocks = staged.missing_blocks;
                return VerifyOutcome::Unrecoverable {
                    par2_set_id: None,
                    missing_blocks: staged.missing_blocks,
                };
            }
            Some(set_id) => {
                *missing_by_set.entry(set_id.clone()).or_default() += staged.missing_blocks;
            }
        }
    }
    // Judge every damaged set before dispatching any repair: a set
    // short beyond its recovery dooms the release no matter which set
    // a repair round would fix first, so a round spent on an earlier
    // repairable set only delays the same verdict (and with more
    // damaged sets than MAX_REPAIR_ROUNDS, one-set-per-round dispatch
    // would fail a fully repairable release).
    for (set_id, missing) in &missing_by_set {
        let recovery = recovery_by_set.get(set_id).copied().unwrap_or(0);
        if *missing > recovery {
            progress.par2_set_id = Some(set_id.clone());
            progress.missing_blocks = *missing;
            return VerifyOutcome::Unrecoverable {
                par2_set_id: Some(set_id.clone()),
                missing_blocks: *missing,
            };
        }
    }
    // Every damaged set is individually recoverable: dispatch them
    // together in one repair round — set ids joined with `+` in
    // deterministic (BTreeMap) order, blocks summed — instead of one
    // round per set.
    if !missing_by_set.is_empty() {
        let set_id = missing_by_set.keys().cloned().collect::<Vec<_>>().join("+");
        let missing: u32 = missing_by_set.values().sum();
        progress.par2_set_id = Some(set_id.clone());
        progress.missing_blocks = missing;
        return VerifyOutcome::RepairNeeded {
            par2_set_id: set_id,
            missing_blocks: missing,
        };
    }
    progress.par2_set_id = sets.first().map(|s| hex::encode(s.set_id));
    progress.missing_blocks = 0;
    VerifyOutcome::Complete
}

/// Rewrite the round manifest from the staged records (only when the
/// step staged or evicted something).
async fn write_manifest<S: ScratchStore>(
    job_id: &str,
    round: u32,
    files: &[ParsedFile],
    progress: &VerifyProgress,
    scratch: &S,
    dirty: bool,
) -> Result<(), VerifyError> {
    if !dirty {
        return Ok(());
    }
    let entries: Vec<ManifestEntry> = files
        .iter()
        .enumerate()
        .filter_map(|(idx, file)| {
            progress.staged.get(&idx).map(|staged| ManifestEntry {
                file_name: file.name.clone(),
                key: staged.key.clone(),
                size: staged.size,
                role: if file.is_par2 { "par2" } else { "data" }.to_string(),
                segments: staged.spool.iter().map(|s| s.key.clone()).collect(),
                missing: staged
                    .spool
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.missing)
                    .map(|(i, _)| i)
                    .collect(),
            })
        })
        .collect();
    // The staging id binds repair idempotency to these exact bytes:
    // derive it from every staged file's content hash (in NZB order),
    // so identical bytes keep the id and any byte change mints another.
    let mut staging_hasher = Sha256::new();
    for idx in 0..files.len() {
        if let Some(staged) = progress.staged.get(&idx) {
            staging_hasher.update(idx.to_le_bytes());
            staging_hasher.update(staged.sha256.as_bytes());
            staging_hasher.update(staged.size.to_le_bytes());
        }
    }
    let manifest = ScratchManifest {
        staging_id: hex::encode(staging_hasher.finalize()),
        files: entries,
    };
    let json = serde_json::to_string(&manifest)
        .map_err(|_| VerifyError::Transient("manifest encode failed".into()))?;
    let key = VerifyProgress::manifest_key(job_id, round);
    let meta = StagedFile {
        size: json.len() as u64,
        sha256: content_sha256_hex(json.as_bytes()),
        key: key.clone(),
        missing_blocks: 0,
        par2_set_id: None,
        spool: Vec::new(),
        crc32: 0,
        missing: false,
    };
    scratch.put(&key, json.into_bytes(), &meta).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srec(size: u64) -> StagedSpoolSegment {
        StagedSpoolSegment {
            key: "k".into(),
            size,
            crc32: 0,
            missing: false,
        }
    }

    fn smissing() -> StagedSpoolSegment {
        StagedSpoolSegment {
            missing: true,
            ..srec(0)
        }
    }

    const JOB: &str = "job_x";

    #[test]
    fn scratch_keys_follow_the_plan_layout() {
        assert_eq!(
            VerifyProgress::scratch_prefix(JOB, 2),
            "ingest-scratch/job_x/r2/"
        );
        let key = VerifyProgress::file_key(JOB, 0, 3, "a/b.flac");
        assert!(key.starts_with("ingest-scratch/job_x/r0/in/0003-"));
        assert!(!key.contains("a/b"), "hostile names never reach key space");
        // Same name, different NZB index: never the same staged object.
        assert_ne!(
            VerifyProgress::file_key(JOB, 0, 0, "track.flac"),
            VerifyProgress::file_key(JOB, 0, 1, "track.flac")
        );
        assert_eq!(
            VerifyProgress::manifest_key(JOB, 0),
            "ingest-scratch/job_x/r0/in/manifest.json"
        );
        assert_eq!(
            VerifyProgress::spool_key(JOB, 1, 2, 3),
            "ingest-scratch/job_x/r1/spool/0002/000003"
        );
        assert_eq!(
            VerifyProgress::spool_meta_key(JOB, 1, 2),
            "ingest-scratch/job_x/r1/spool/0002/index.json"
        );
    }

    #[test]
    fn implied_len_uses_par2_length_then_proven_geometry() {
        // The par2-declared length is authoritative when known — the
        // judgement then decides (streaming if large), never an
        // allocation before the size check.
        let gapped = vec![srec(4), smissing(), smissing()];
        assert_eq!(implied_len(&gapped, 3, Some(100 << 20)), Some(100 << 20));
        assert_eq!(implied_len(&gapped, 3, Some(u64::MAX)), Some(u64::MAX));
        // Without par2: uniform part size 4 over 3 parts.
        assert_eq!(implied_len(&gapped, 3, None), Some(12));
        // All present: the plain sum.
        assert_eq!(implied_len(&[srec(4), srec(3), srec(2)], 3, None), Some(9));
        // No proven part size (nothing present before the last part).
        assert_eq!(implied_len(&[smissing(), srec(5)], 2, None), None);
    }

    fn parsed(name: &str, is_par2: bool) -> ParsedFile {
        ParsedFile {
            name: name.into(),
            is_par2,
            segment_ids: vec!["s@x".into()],
            segment_bytes: vec![None],
        }
    }

    fn set(byte: u8, recovery_blocks: usize) -> RecoverySet {
        RecoverySet {
            set_id: [byte; 16],
            block_size: 256,
            recovery_blocks,
            files: HashMap::new(),
        }
    }

    fn staged(sha: &str, missing_blocks: u32, par2_set_id: Option<String>) -> StagedFile {
        StagedFile {
            key: "k".into(),
            size: 1,
            sha256: sha.into(),
            missing_blocks,
            par2_set_id,
            spool: Vec::new(),
            crc32: 0,
            missing: false,
        }
    }

    #[test]
    fn finalize_judges_every_damaged_set_before_dispatching_repair() {
        // Set A (first in BTreeMap order) is repairable; set B is short
        // beyond recovery. finalize must not spend a repair round on A:
        // the release is Unrecoverable because of B.
        let a = set(0x0a, 4);
        let b = set(0x0b, 1);
        let a_hex = hex::encode(a.set_id);
        let b_hex = hex::encode(b.set_id);
        let files = vec![parsed("a.flac", false), parsed("b.flac", false)];
        let mut progress = VerifyProgress::new(0);
        progress.staged.insert(0, staged("sha", 2, Some(a_hex)));
        progress
            .staged
            .insert(1, staged("sha", 3, Some(b_hex.clone())));
        let outcome = finalize(&files, &[a, b], &mut progress);
        assert_eq!(
            outcome,
            VerifyOutcome::Unrecoverable {
                par2_set_id: Some(b_hex),
                missing_blocks: 3,
            }
        );
    }

    #[test]
    fn finalize_dispatches_all_recoverable_sets_in_one_round() {
        // Both sets repairable: one combined dispatch (ids joined with
        // '+', blocks summed) instead of one round per set.
        let a = set(0x0a, 4);
        let b = set(0x0b, 4);
        let a_hex = hex::encode(a.set_id);
        let b_hex = hex::encode(b.set_id);
        let files = vec![parsed("a.flac", false), parsed("b.flac", false)];
        let mut progress = VerifyProgress::new(0);
        progress
            .staged
            .insert(0, staged("sha", 2, Some(a_hex.clone())));
        progress
            .staged
            .insert(1, staged("sha", 3, Some(b_hex.clone())));
        let outcome = finalize(&files, &[a, b], &mut progress);
        assert_eq!(
            outcome,
            VerifyOutcome::RepairNeeded {
                par2_set_id: format!("{a_hex}+{b_hex}"),
                missing_blocks: 5,
            }
        );
        assert_eq!(progress.missing_blocks, 5);
    }

    #[test]
    fn progress_roundtrips_with_legacy_defaults() {
        let legacy = serde_json::json!({"round": 1, "staged": {}});
        let progress: VerifyProgress = serde_json::from_value(legacy).unwrap();
        assert_eq!(progress.round, 1);
        assert!(progress.par2_set_id.is_none());
        assert_eq!(progress.missing_blocks, 0);
        assert!(progress.spool.is_empty());
    }

    #[test]
    fn progress_roundtrips_the_spool_cursor() {
        let mut progress = VerifyProgress::new(0);
        progress.spool.insert(
            1,
            vec![StagedSpoolSegment {
                key: "k/0".into(),
                size: 12,
                crc32: 9,
                missing: false,
            }],
        );
        let json = serde_json::to_string(&progress).unwrap();
        let back: VerifyProgress = serde_json::from_str(&json).unwrap();
        assert_eq!(back, progress);
    }
}
