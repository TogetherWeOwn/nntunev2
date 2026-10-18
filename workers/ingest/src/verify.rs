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
//! a larger file is staged *as* its spool — the manifest rows then carry
//! the segment keys (a later enablement slice materializes them into a
//! single bytestream; the repair worker's current input contract reads
//! whole objects by content-hash key, see the staged-key docs). Judging
//! a spooled file never walks an untrusted par2-declared length: the
//! logical length is bounded by the NZB's declared segment bytes and
//! the part geometry the fetched segments prove, a healthy file is
//! vouched for by the decode-time segment CRCs plus one spool listing
//! (no read-back walk at all), and a damaged file's block counter
//! streams only the present bytes, fast-forwarding zero gaps with
//! CRC32 combine (see `judge_spooled`). That walk persists its
//! segment-boundary state in a [`JudgeCursor`] in the progress
//! record, so a deadline stop resumes mid-file instead of
//! re-reading every present segment on the next delivery. A
//! spooled file's staged
//! `sha256` is a digest of its segment records, not of a byte walk.
//! Staged par2 is restored packet by packet into a
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
    /// Largest logical file length the judgement will consider. The
    /// length is untrusted (par2-declared or part geometry): before
    /// this bound is even consulted, `load_segments` caps it by the
    /// NZB's declared segment bytes and the proven part geometry, and
    /// zero gaps cost CRC32-combine steps, not streamed bytes — but
    /// anything beyond the intake guard's own declared-bytes bound
    /// (64 GiB per NZB) cannot describe a file this pipeline accepted,
    /// so it stays a geometry error.
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
    /// in order (enablement-slice work: the repair input worker does
    /// not consume spools yet), and `key` names
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

/// Resume cursor for one spooled file's streaming block judgement:
/// the block counter's state at a segment boundary (see
/// `judge_spooled`). Judging a damaged oversize file reads back every
/// present segment, which can exceed one delivery's read budget;
/// persisting this cursor lets each delivery advance the walk instead
/// of restarting it, and that advance counts as progress. The cursor
/// is trusted only while its `fingerprint` — the staged spool digest
/// plus the covering set id — still matches the file being judged,
/// and its counters are internally consistent (see `valid_cursor`);
/// anything else restarts the walk, never misjudges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeCursor {
    /// Next segment index the walk has yet to feed.
    pub segment: usize,
    /// Logical bytes fed to the block counter so far.
    pub pos: u64,
    /// Index of the block currently being accumulated.
    pub block_index: usize,
    /// Bytes accumulated into that block (the running hasher's length).
    pub in_block: u64,
    /// The running block hasher's CRC32 at the boundary.
    pub crc32: u32,
    /// Blocks already counted bad.
    pub bad: u32,
    /// `{spool_digest}:{set_id_hex}:{block_size}:{block_count}` for
    /// the judgement this cursor belongs to; a mismatch discards it.
    pub fingerprint: String,
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
    /// Streaming-judgement resume cursors by NZB file index, for
    /// spooled files whose block walk a deadline stopped mid-way.
    /// Removed when the file's judgement lands.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub judging: BTreeMap<usize, JudgeCursor>,
    /// The set the completed step dispatched (or would dispatch) repair
    /// for; `None` until every file is judged. When several sets are
    /// dispatched together this is a bounded digest of their ids (see
    /// `finalize`); `repair_set_ids` then carries the full list.
    #[serde(default)]
    pub par2_set_id: Option<String>,
    /// Every damaged set dispatched in the combined repair round, in
    /// deterministic order. Empty for a single-set (or no) dispatch,
    /// where `par2_set_id` is the set id itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repair_set_ids: Vec<String>,
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
    /// adopting) one staged object. Note this is the full bucket key,
    /// deliberately distinct from the repair worker's input addressing
    /// (`{prefix}in/{sha256(file_name)}` in the core scratch contract):
    /// reconciling the two key namespaces — and how duplicate NZB file
    /// names map onto the manifest's unique names — is enablement work
    /// before the consumer is switched on; the manifest rows carry
    /// these full keys today so resume/adoption never guess.
    pub fn file_key(job_id: &str, round: u32, file_index: usize, file_name: &str) -> String {
        format!(
            "{}in/{file_index:04}-{}",
            Self::scratch_prefix(job_id, round),
            content_sha256_hex(file_name.as_bytes())
        )
    }

    /// Manifest key for one round (inside `in/`, per the scratch contract).
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

    /// Scratch prefix covering one file's spool objects (its segment
    /// keys and its segment-index key): one listing under this prefix
    /// validates the whole resume cursor, where a HEAD per segment
    /// would grow quadratically across deliveries.
    pub fn spool_prefix(job_id: &str, round: u32, file_index: usize) -> String {
        format!(
            "{}spool/{file_index:04}/",
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
    /// Every object key under `prefix` (one listing operation per page
    /// of keys, never one operation per object).
    fn list_keys<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Future<Output = Result<Vec<String>, VerifyError>> + 'a;
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
    /// segment spool (too large to assemble in-Worker): a future
    /// materialization step concatenates these (zero-filling each gap
    /// named in `missing`) — the repair worker's current input
    /// contract reads whole objects only, so a spooled staging is not
    /// yet repairable. Empty for an assembled file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<String>,
    /// Indexes (into `segments`) whose articles are gone: those byte
    /// ranges are part of the damage the repair side re-derives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<usize>,
}

/// How a fully spooled file's bytes are presented to the judgement.
enum LoadedFile {
    /// Assembled file bytes: gaps placed by the proven uniform part
    /// size and zero-filled, or just the contiguous present prefix when
    /// no part size is proven (see the module docs). `vanished` counts
    /// present spool objects that disappeared between cursor
    /// validation and this read-back (their ranges read as zeros): the
    /// judgement must not let a decode-time proof vouch for them.
    Assembled { bytes: Vec<u8>, vanished: u32 },
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
                    if existing.spool.is_empty() {
                        continue;
                    }
                    // A file staged as its spool is vouched for by its
                    // segment *records* (see `spool_digest`): the index
                    // object alone cannot notice a segment object that
                    // vanished after staging. One listing re-checks the
                    // spool; a vanished segment evicts the record and
                    // resumes from the truncated cursor below instead
                    // of shipping a manifest key that dangles.
                    let prefix = VerifyProgress::spool_prefix(job_id, round, idx);
                    let present: HashSet<String> =
                        scratch.list_keys(&prefix).await?.into_iter().collect();
                    let first_gone = existing
                        .spool
                        .iter()
                        .position(|rec| !rec.missing && !present.contains(&rec.key));
                    let Some(first_gone) = first_gone else {
                        continue;
                    };
                    progress.staged.remove(&idx);
                    let mut cursor = existing.spool.clone();
                    cursor.truncate(first_gone);
                    progress.spool.insert(idx, cursor);
                    claimed = HashSet::new();
                    claim_staged(&sets, files, progress, &mut claimed);
                    dirty = true;
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
                                // Same guard as resume trust: adopt the
                                // staged judgement only while every
                                // present spool segment is still there;
                                // otherwise fall through to the cursor
                                // adoption below and refetch.
                                let prefix = VerifyProgress::spool_prefix(job_id, round, idx);
                                let spool_intact = adopted.spool.iter().all(|s| s.missing)
                                    || scratch
                                        .list_keys(&prefix)
                                        .await?
                                        .iter()
                                        .collect::<HashSet<_>>()
                                        .is_superset(
                                            &adopted
                                                .spool
                                                .iter()
                                                .filter(|s| !s.missing)
                                                .map(|s| &s.key)
                                                .collect::<HashSet<_>>(),
                                        );
                                if spool_intact {
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
                    // Adoption is a HEAD per segment (the cursor is
                    // rebuilt from object metadata), so it polls the
                    // deadline and keeps the adopted prefix when the
                    // budget ends — the next delivery re-adopts the
                    // rest. The spend check just below then stops the
                    // step before any fetching.
                    if deadline_exceeded() {
                        break;
                    }
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
        // One listing of the file's spool prefix (a page of 1,000 keys
        // per operation) replaces a HEAD per segment — revalidating by
        // HEAD would re-issue one operation per already-spooled
        // segment on every delivery, growing quadratically with file
        // size and eventually eating the whole delivery budget before
        // a single new segment could fetch.
        if let Some(spool) = progress.spool.get(&idx).cloned() {
            let mut valid = spool.len();
            if spool.iter().any(|rec| !rec.missing) {
                let prefix = VerifyProgress::spool_prefix(job_id, round, idx);
                let present: HashSet<String> =
                    scratch.list_keys(&prefix).await?.into_iter().collect();
                for (n, rec) in spool.iter().enumerate() {
                    if !rec.missing && !present.contains(&rec.key) {
                        valid = n;
                        break;
                    }
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
            let rec = spool_one(
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
            .await?;
            progress.spool.entry(idx).or_default().push(rec);
            dirty = true;
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

        // The NZB's own bound on this file's decoded length: yEnc
        // expands, never compresses, so the decoded bytes cannot
        // outgrow the encoded `bytes=` declarations. `None` when any
        // segment declares none (or the declarations overflow).
        let nzb_bound = file
            .segment_bytes
            .iter()
            .try_fold(0u64, |acc, b| acc.checked_add((*b)?));
        let load = load_segments(
            &segs,
            total_segments,
            expected_len,
            nzb_bound,
            caps,
            scratch,
        )
        .await?;
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
            LoadedFile::Assembled { bytes, vanished } => {
                let (missing_blocks, par2_set_id) = if file.is_par2 {
                    // A vanished range zero-fills into the staged par2
                    // bytes; `load_sets` then simply cannot use those
                    // packets — conservative, never a false pass.
                    (0, None)
                } else {
                    judge_file(matched, missing_segments, vanished, &seg_crcs, &bytes)
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
                progress.judging.remove(&idx);
                Some(staged)
            }
            LoadedFile::Spooled { logical_len } => {
                // Take the file's judging cursor (if a previous
                // delivery's walk stopped mid-file): `judge_spooled`
                // resumes from it, writes back its advance on a
                // deadline stop, and clears it once judged.
                let mut cursor = progress.judging.remove(&idx);
                let judged = if file.is_par2 {
                    // No byte walk: the staged record's digest is over
                    // the segment records (see `spool_digest`), and
                    // `load_sets` reads back what it can — a vanished
                    // spool segment makes that volume unusable there,
                    // never a false pass here.
                    Ok(Some((0, None, spool_digest(&segs, logical_len))))
                } else {
                    judge_spooled(
                        matched,
                        missing_segments,
                        &seg_crcs,
                        &segs,
                        total_segments,
                        logical_len,
                        &VerifyProgress::spool_prefix(job_id, round, idx),
                        scratch,
                        deadline_exceeded,
                        &mut cursor,
                    )
                    .await
                };
                // Reinstate the cursor before propagating any error,
                // so a transient scratch failure mid-walk keeps the
                // walk's advance instead of restarting it.
                if let Some(cursor) = cursor {
                    progress.judging.insert(idx, cursor);
                }
                let judged = judged?;
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
        progress.judging.remove(&idx);
        dirty = true;
    }

    write_manifest(job_id, round, files, progress, scratch, dirty).await?;
    Ok(finalize(files, &sets, progress))
}

/// Fetch, yEnc-decode and spool one segment, returning its durable
/// cursor record. Present bytes go to scratch immediately (with their
/// CRC32 in the object record, so a lost progress write can rebuild
/// the cursor from metadata alone). A segment that is missing,
/// undecodable, or refused for size (`413`, the client read limit, or
/// the in-Worker article cap) records `missing` instead — it is judged
/// absent, the cursor advances past it, and later segments still
/// fetch: oversize is a property of this one article (a refetch would
/// be refused identically), never a reason to count the file's whole
/// tail as gone.
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
) -> Result<StagedSpoolSegment, VerifyError> {
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
        ArticleFetch::Missing => return Ok(missing_rec),
        // The source refused the article for size (pool `413` / client
        // read limit): judged absent like a `430`, same as the
        // in-Worker cap below.
        ArticleFetch::TooLarge => return Ok(missing_rec),
    };
    spend.bytes = spend.bytes.saturating_add(article.len() as u64);
    if article.len() > caps.max_article_bytes {
        return Ok(missing_rec);
    }
    let Some(bytes) = decode_segment(segment_id, &article) else {
        return Ok(missing_rec);
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
    Ok(rec)
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

/// The file's logical length from the geometry the spooled segments
/// prove. `None` only on u64 overflow (part geometry that cannot
/// describe a real file). The par2-declared length is applied by the
/// caller, capped to this geometry — never trusted on its own.
fn implied_len(segs: &[StagedSpoolSegment], total_segments: usize) -> Option<u64> {
    if total_segments == 0 {
        return Some(0);
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
/// does not. The implied length is checked *before* any allocation and
/// never trusted from par2 alone: a par2-declared length above both
/// the part geometry the fetched segments prove and the NZB's own
/// declared-bytes bound (`nzb_bound`) is capped to the tighter of
/// those, so a crafted par2 cannot make the judgement walk (or
/// allocate) a length the NZB never declared — the excess simply
/// counts as blocks the staged bytes cannot vouch for. A spool object
/// that vanished after cursor validation reads as zeros here and is
/// counted in `vanished`, so the judgement treats it as damage, never
/// as a pass.
async fn load_segments<S: ScratchStore>(
    segs: &[StagedSpoolSegment],
    total_segments: usize,
    expected_len: Option<u64>,
    nzb_bound: Option<u64>,
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
        let mut vanished = 0u32;
        for seg in segs {
            match scratch.get(&seg.key).await? {
                Some(bytes) => out.extend_from_slice(&bytes),
                None => {
                    vanished += 1;
                    out.resize(out.len() + seg.size as usize, 0);
                }
            }
        }
        return Ok(Some(LoadedFile::Assembled {
            bytes: out,
            vanished,
        }));
    }
    let Some(part_size) = uniform_part_size(segs, total_segments) else {
        // No proven part size: only the contiguous present prefix can
        // vouch for blocks. The spool cursor persists across
        // deliveries, so that prefix is *not* bounded by one
        // delivery's fetched bytes — cap the in-memory run at
        // `max_file_bytes` and let the block judgement count every
        // later block as bad (conservative, never a false pass).
        let mut out = Vec::new();
        let mut vanished = 0u32;
        for seg in segs {
            if seg.missing {
                break;
            }
            match scratch.get(&seg.key).await? {
                Some(bytes) => {
                    if (out.len() as u64).saturating_add(bytes.len() as u64) > caps.max_file_bytes {
                        break;
                    }
                    out.extend_from_slice(&bytes);
                }
                None => {
                    vanished += 1;
                    break;
                }
            }
        }
        return Ok(Some(LoadedFile::Assembled {
            bytes: out,
            vanished,
        }));
    };
    let Some(geometry) = implied_len(segs, total_segments) else {
        return Ok(None);
    };
    // The par2 length is authoritative only where the file's own
    // geometry and the NZB's declarations can contain it.
    let mut total = expected_len.map_or(geometry, |len| len.min(geometry));
    if let Some(bound) = nzb_bound {
        total = total.min(bound);
    }
    if total > caps.max_logical_bytes {
        return Ok(None);
    }
    if total > caps.max_file_bytes || usize::try_from(total).is_err() {
        return Ok(Some(LoadedFile::Spooled { logical_len: total }));
    }
    let mut out = vec![0u8; total as usize];
    let mut vanished = 0u32;
    for (i, seg) in segs.iter().enumerate() {
        if seg.missing {
            continue;
        }
        let Some(bytes) = scratch.get(&seg.key).await? else {
            vanished += 1;
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
    Ok(Some(LoadedFile::Assembled {
        bytes: out,
        vanished,
    }))
}

/// A hasher over exactly `n` zero bytes, built by CRC32 combine
/// doubling (each combine is logarithmic in the combined length), so
/// a zero run of any size costs O(log n) instead of n hashed bytes.
fn zero_crc_hasher(mut n: u64) -> crc32fast::Hasher {
    let mut result = crc32fast::Hasher::new();
    let mut power = crc32fast::Hasher::new();
    power.update(&[0u8; 1]);
    while n > 0 {
        if n & 1 == 1 {
            result.combine(&power);
        }
        n >>= 1;
        if n > 0 {
            let doubled = power.clone();
            power.combine(&doubled);
        }
    }
    result
}

/// Advance a CRC32 by `n` zero bytes without materializing them.
fn crc32_zeros(hasher: &mut crc32fast::Hasher, n: u64) {
    if n > 0 {
        hasher.combine(&zero_crc_hasher(n));
    }
}

/// Consumer of a [`stream_logical`] walk. Zero runs arrive through
/// `zeros` so a consumer can fast-forward them (CRC32 combine)
/// instead of hashing gigabytes of nothing for a gap.
trait LogicalSink {
    fn bytes(&mut self, bytes: &[u8]);
    fn zeros(&mut self, n: u64);
    /// The walk is about to feed segment `segment` (or the tail past
    /// the last segment, `segment == total`) having fed exactly `pos`
    /// logical bytes: a resumable consumer snapshots its state here,
    /// so a deadline stop anywhere later in the segment resumes from
    /// this boundary.
    fn boundary(&mut self, _segment: usize, _pos: u64) {}
}

/// What one [`stream_logical`] walk did.
enum Streamed {
    /// Walked all `logical_len` bytes. A present spool object that
    /// vanished after cursor validation streams as zeros, so the
    /// counter counts its blocks as damage — the only caller streams
    /// into the block counter, never into a record-based proof.
    Done,
    /// The delivery deadline stopped the walk; the sink's last
    /// segment-boundary snapshot resumes it next delivery.
    Stopped,
}

/// Feed a zero run in bounded pieces, polling the deadline between
/// them. Returns false when the deadline stopped the run early.
fn feed_zeros_polled(
    mut n: u64,
    feed: &mut dyn LogicalSink,
    deadline_exceeded: &dyn Fn() -> bool,
) -> bool {
    const PIECE: u64 = 1 << 20;
    while n > 0 {
        if deadline_exceeded() {
            return false;
        }
        let take = n.min(PIECE);
        feed.zeros(take);
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
/// work is proportional to the *present* bytes plus the block count:
/// `logical_len` arrives already bounded by the NZB's declared bytes
/// and the proven part geometry (see `load_segments`), zero runs go
/// through [`LogicalSink::zeros`], and the deadline is still polled
/// between segments and inside zero runs. `start_segment`/`start_pos`
/// resume a stopped walk at a segment boundary the sink snapshotted
/// (both 0 for a fresh walk): the segments before the boundary —
/// and the scratch reads they cost — are not repeated.
#[allow(clippy::too_many_arguments)]
async fn stream_logical<S: ScratchStore>(
    segs: &[StagedSpoolSegment],
    total_segments: usize,
    logical_len: u64,
    start_segment: usize,
    start_pos: u64,
    scratch: &S,
    deadline_exceeded: &dyn Fn() -> bool,
    feed: &mut dyn LogicalSink,
) -> Result<Streamed, VerifyError> {
    let all_present = segs.len() >= total_segments && segs.iter().all(|s| !s.missing);
    let part_size = if all_present {
        0
    } else {
        uniform_part_size(segs, total_segments).unwrap_or(0)
    };
    let mut pos: u64 = start_pos;
    for (i, seg) in segs.iter().enumerate().skip(start_segment) {
        if pos >= logical_len {
            break;
        }
        feed.boundary(i, pos);
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
            // path does the same); its blocks count as damage.
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
        feed.bytes(&bytes[skip..skip + take]);
        pos += take as u64;
    }
    if pos < logical_len {
        feed.boundary(segs.len(), pos);
        if !feed_zeros_polled(logical_len - pos, feed, deadline_exceeded) {
            return Ok(Streamed::Stopped);
        }
    }
    Ok(Streamed::Done)
}

/// Digest identifying a spooled file's staged state: SHA-256 over the
/// logical length and every segment record (position, size,
/// decode-time CRC32, missing flag). A spooled file's staged `sha256`
/// is this record digest, not a hash of the file bytes — computing a
/// whole-file SHA would mean re-reading and hashing every byte (plus
/// every zero of every gap) inside one delivery, a one-shot walk with
/// no resumable state whose length an untrusted par2 declares. The
/// resume contract only needs the digest to change whenever the
/// staged spool changes, which the records capture exactly (a segment
/// whose bytes change under the same record would need a CRC32
/// collision at the same length, and the par2 judgement never trusts
/// this digest as a health proof).
fn spool_digest(segs: &[StagedSpoolSegment], logical_len: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(logical_len.to_le_bytes());
    for seg in segs {
        hasher.update(seg.size.to_le_bytes());
        hasher.update(seg.crc32.to_le_bytes());
        hasher.update([u8::from(seg.missing)]);
    }
    hex::encode(hasher.finalize())
}

/// Count present segment records whose spool object is no longer in
/// scratch, from one listing of the file's spool prefix (no listing
/// is issued when every segment is already missing).
async fn count_vanished<S: ScratchStore>(
    segs: &[StagedSpoolSegment],
    spool_prefix: &str,
    scratch: &S,
) -> Result<u32, VerifyError> {
    if segs.iter().all(|s| s.missing) {
        return Ok(0);
    }
    let present: HashSet<String> = scratch.list_keys(spool_prefix).await?.into_iter().collect();
    Ok(segs
        .iter()
        .filter(|s| !s.missing && !present.contains(&s.key))
        .count() as u32)
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
    /// Identifies the judgement (spool digest + set id) boundary
    /// snapshots belong to.
    fingerprint: String,
    /// State at the last segment boundary fed: where a deadline
    /// stop resumes.
    boundary: Option<JudgeCursor>,
}

impl<'a> BlockCounter<'a> {
    fn new(block_crc32s: &'a [u32], block_size: u64, fingerprint: String) -> Self {
        Self {
            block_crc32s,
            block_size,
            hasher: crc32fast::Hasher::new(),
            in_block: 0,
            index: 0,
            bad: 0,
            fingerprint,
            boundary: None,
        }
    }

    /// Resume from a validated cursor. The hasher's accumulated
    /// length is exactly `in_block` (it resets per block, and zero
    /// runs combine with their length tracked), so
    /// `new_with_initial_len` rebuilds it from the stored CRC.
    fn resume(block_crc32s: &'a [u32], block_size: u64, cursor: &JudgeCursor) -> Self {
        Self {
            block_crc32s,
            block_size,
            hasher: crc32fast::Hasher::new_with_initial_len(cursor.crc32, cursor.in_block),
            in_block: cursor.in_block,
            index: cursor.block_index,
            bad: cursor.bad,
            fingerprint: cursor.fingerprint.clone(),
            boundary: Some(cursor.clone()),
        }
    }

    fn snapshot(&self, segment: usize, pos: u64) -> JudgeCursor {
        JudgeCursor {
            segment,
            pos,
            block_index: self.index,
            in_block: self.in_block,
            crc32: self.hasher.clone().finalize(),
            bad: self.bad,
            fingerprint: self.fingerprint.clone(),
        }
    }
}

/// Whether a stored judging cursor may resume this judgement: same
/// spool digest and set, in-range indexes, and counters that agree
/// with each other (`pos` is exactly the bytes the block state
/// accounts for while blocks remain). Anything else — a changed
/// spool, a different set, a corrupt or tampered record — restarts
/// the walk from zero instead of judging from a state the bytes did
/// not produce.
fn valid_cursor(
    cursor: &JudgeCursor,
    fingerprint: &str,
    segments: usize,
    logical_len: u64,
    block_size: u64,
    block_count: usize,
) -> bool {
    if cursor.fingerprint != fingerprint
        || cursor.segment > segments
        || cursor.pos > logical_len
        || cursor.block_index > block_count
        || cursor.bad as usize > cursor.block_index
    {
        return false;
    }
    if cursor.block_index >= block_count {
        // Every block finished: `pos` must have covered them all (it
        // may run past once the counter stops accumulating).
        return cursor.in_block == 0
            && (block_count as u64)
                .checked_mul(block_size)
                .is_some_and(|covered| cursor.pos >= covered);
    }
    if cursor.in_block >= block_size {
        return false;
    }
    (cursor.block_index as u64)
        .checked_mul(block_size)
        .and_then(|bytes| bytes.checked_add(cursor.in_block))
        == Some(cursor.pos)
}

impl BlockCounter<'_> {
    fn update(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            if self.index >= self.block_crc32s.len() {
                return;
            }
            // Keep the room in u64 until bounded by the input length:
            // narrowing first would truncate on wasm32 for a block
            // size above 4 GiB.
            let room = self.block_size - self.in_block;
            let take = room.min(bytes.len() as u64) as usize;
            self.hasher.update(&bytes[..take]);
            self.in_block += take as u64;
            bytes = &bytes[take..];
            if self.in_block == self.block_size {
                self.finish_block();
            }
        }
    }

    fn zeros(&mut self, mut n: u64) {
        while n > 0 {
            if self.index >= self.block_crc32s.len() {
                return;
            }
            let room = self.block_size - self.in_block;
            let take = room.min(n);
            crc32_zeros(&mut self.hasher, take);
            self.in_block += take;
            n -= take;
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
            crc32_zeros(&mut self.hasher, pad);
            self.finish_block();
        }
        // Blocks the stream never reached count as bad, like blocks past
        // the assembled bytes in `count_bad_blocks`.
        self.bad + (self.block_crc32s.len() - self.index) as u32
    }
}

impl LogicalSink for BlockCounter<'_> {
    fn bytes(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }

    fn zeros(&mut self, n: u64) {
        BlockCounter::zeros(self, n);
    }

    fn boundary(&mut self, segment: usize, pos: u64) {
        self.boundary = Some(self.snapshot(segment, pos));
    }
}

/// Streaming counterpart of [`judge_file`] for a file too large to
/// assemble: same cheapest-proof-first order (segment-CRC precheck,
/// then per-block CRCs), plus the staged record's digest
/// ([`spool_digest`], derived from the segment records — never a
/// whole-file byte walk). The healthy and uncovered paths do no
/// read-back walk at all: the precheck vouches for the decode-time
/// bytes, and one spool listing confirms those bytes are still
/// staged. Only a damaged file streams, and then only its present
/// bytes (zero gaps fast-forward through CRC32 combine), over a
/// logical length `load_segments` already bounded by the NZB's
/// declared bytes and the proven part geometry. That walk is
/// resumable across deliveries through `cursor`: a deadline stop
/// returns `None` with the walk's segment-boundary state written
/// back, so the next delivery continues where this one stopped
/// instead of re-reading every present segment; a completed
/// judgement clears it.
#[allow(clippy::too_many_arguments)]
async fn judge_spooled<S: ScratchStore>(
    matched: Option<(&RecoverySet, &RecoveryFile)>,
    missing_segments: u32,
    seg_crcs: &[(u32, u64)],
    segs: &[StagedSpoolSegment],
    total_segments: usize,
    logical_len: u64,
    spool_prefix: &str,
    scratch: &S,
    deadline_exceeded: &dyn Fn() -> bool,
    cursor: &mut Option<JudgeCursor>,
) -> Result<Option<(u32, Option<String>, String)>, VerifyError> {
    let digest = spool_digest(segs, logical_len);
    let (missing_blocks, par2_set_id) = match matched {
        None => {
            // Uncovered: healthy iff every segment decoded *and* its
            // spool object is still staged. A vanished object counts
            // exactly like the missing segment it now is.
            let vanished = count_vanished(segs, spool_prefix, scratch).await?;
            (missing_segments + vanished, None)
        }
        Some((set, entry)) => {
            let set_hex = hex::encode(set.set_id);
            let healthy = missing_segments == 0
                && set
                    .precheck_bytes(&entry.filename_bytes, seg_crcs)
                    .unwrap_or(false);
            // The precheck vouches for the decode-time bytes; trust it
            // only while every spool object those bytes staged into is
            // still there (one listing, no read-back).
            let intact = healthy && count_vanished(segs, spool_prefix, scratch).await? == 0;
            if intact {
                (0, Some(set_hex))
            } else if set.block_size == 0 || logical_len > entry.length {
                // An over-long file trusts nothing (as in
                // `count_bad_blocks`): every block counts, with no
                // walk at all.
                (entry.block_crc32s.len() as u32, Some(set_hex))
            } else {
                // Damaged (or a spool object vanished after the
                // precheck inputs were recorded): count blocks over
                // the bytes actually staged — vanished/missing ranges
                // stream as zeros. A stored cursor resumes the walk
                // at its segment boundary when it still describes
                // this exact spool and set.
                let fingerprint = format!(
                    "{digest}:{set_hex}:{}:{}",
                    set.block_size,
                    entry.block_crc32s.len()
                );
                let resume = cursor.take().filter(|saved| {
                    valid_cursor(
                        saved,
                        &fingerprint,
                        segs.len(),
                        logical_len,
                        set.block_size,
                        entry.block_crc32s.len(),
                    )
                });
                let (mut counter, start_segment, start_pos) = match &resume {
                    Some(saved) => (
                        BlockCounter::resume(&entry.block_crc32s, set.block_size, saved),
                        saved.segment,
                        saved.pos,
                    ),
                    None => (
                        BlockCounter::new(&entry.block_crc32s, set.block_size, fingerprint),
                        0,
                        0,
                    ),
                };
                let streamed = match stream_logical(
                    segs,
                    total_segments,
                    logical_len,
                    start_segment,
                    start_pos,
                    scratch,
                    deadline_exceeded,
                    &mut counter,
                )
                .await
                {
                    Ok(streamed) => streamed,
                    Err(error) => {
                        *cursor = counter.boundary;
                        return Err(error);
                    }
                };
                match streamed {
                    Streamed::Done => (counter.finish(), Some(set_hex)),
                    Streamed::Stopped => {
                        *cursor = counter.boundary;
                        return Ok(None);
                    }
                }
            }
        }
    };
    *cursor = None;
    Ok(Some((missing_blocks, par2_set_id, digest)))
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
            // An assembled volume with a missing article carries a
            // zero-filled (or truncated) range inside it: feed only
            // the complete packet prefix, like the spooled path
            // below, so one damaged range does not discard every
            // recovery slice the volume holds before it.
            let complete = complete_packet_prefix(&bytes);
            if complete > 0 {
                feed_par2_bytes(&mut builders, &bytes[..complete], caps.max_par2_bytes);
            }
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
    // when every segment decoded. The sum is checked: overflowing part
    // sizes prove no length, so there is no match (fail closed, no panic).
    if segs.len() >= total_segments && segs.iter().all(|s| !s.missing) {
        // Checked sum: on overflow there is no true assembled length, so
        // skip the length fallback (no match) and let the loader fail
        // closed with the permanent geometry error. An unchecked sum would
        // panic in debug and could match a wrapped length in release.
        let len = segs
            .iter()
            .try_fold(0u64, |acc, s| acc.checked_add(s.size))?;
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
        // Per-file staged records always carry their single covering
        // set's id; the `+` split additionally honors combined ids
        // written by older progress records.
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
/// captured before assembly consumed the segments. `vanished` counts
/// present spool objects that disappeared before the assembly
/// read-back (their ranges are zeros in `assembled`): the decode-time
/// proofs vouch for bytes that are no longer staged, so a vanished
/// range disables the healthy proof entirely — the symmetric
/// counterpart of `judge_spooled`'s listing check — and counts as
/// damage via the block judgement.
fn judge_file(
    matched: Option<(&RecoverySet, &RecoveryFile)>,
    missing_segments: u32,
    vanished: u32,
    seg_crcs: &[(u32, u64)],
    assembled: &[u8],
) -> (u32, Option<String>) {
    let Some((set, entry)) = matched else {
        // No set covers this file: healthy iff every segment decoded
        // and is still staged.
        return (missing_segments + vanished, None);
    };
    let set_hex = hex::encode(set.set_id);
    if missing_segments == 0 && vanished == 0 {
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
    // Recomputed from this judgement; only a combined dispatch fills it.
    progress.repair_set_ids = Vec::new();
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
    // together in one repair round (blocks summed) instead of one
    // round per set. The dispatched identifier must fit the repair
    // contract's 256-char bound however many sets combine: a single
    // set dispatches under its own id, while a combination dispatches
    // under the hex SHA-256 (64 chars) of the sorted ids joined with
    // `+`, with the full list kept in `repair_set_ids` — joining the
    // raw ids would exceed the bound at eight 32-char sets and fail a
    // fully repairable release as "no par2 set covers them".
    if !missing_by_set.is_empty() {
        let set_ids: Vec<String> = missing_by_set.keys().cloned().collect();
        let missing: u32 = missing_by_set.values().sum();
        let set_id = if set_ids.len() == 1 {
            progress.repair_set_ids = Vec::new();
            set_ids.into_iter().next().expect("one set")
        } else {
            let combined = content_sha256_hex(set_ids.join("+").as_bytes());
            progress.repair_set_ids = set_ids;
            combined
        };
        progress.par2_set_id = Some(set_id.clone());
        progress.missing_blocks = missing;
        return VerifyOutcome::RepairNeeded {
            par2_set_id: set_id,
            missing_blocks: missing,
        };
    }
    progress.repair_set_ids = Vec::new();
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
    fn implied_len_uses_only_proven_geometry() {
        // The helper knows nothing of the par2-declared length;
        // `load_segments` caps that length by this proven geometry
        // and the NZB bound before anything allocates or streams
        // (see `lying_par2_length_is_capped_by_geometry_and_the_nzb_bound`).
        let gapped = vec![srec(4), smissing(), smissing()];
        // Uniform part size 4 over 3 parts.
        assert_eq!(implied_len(&gapped, 3), Some(12));
        // All present: the plain sum.
        assert_eq!(implied_len(&[srec(4), srec(3), srec(2)], 3), Some(9));
        // No proven part size (nothing present before the last part).
        assert_eq!(implied_len(&[smissing(), srec(5)], 2), None);
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
        // Both sets repairable: one combined dispatch (blocks summed)
        // instead of one round per set, under a bounded digest of the
        // sorted ids with the full list recorded beside it.
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
                par2_set_id: content_sha256_hex(format!("{a_hex}+{b_hex}").as_bytes()),
                missing_blocks: 5,
            }
        );
        assert_eq!(progress.repair_set_ids, [a_hex, b_hex]);
        assert_eq!(progress.missing_blocks, 5);
    }

    #[test]
    fn finalize_combined_id_stays_bounded_past_eight_sets() {
        // Eight 32-char set ids joined with '+' would be 263 chars and
        // `decide_repair` (256 cap) would fail a fully repairable
        // release as uncovered. The combined dispatch id is the
        // 64-char digest at any set count, and decides Ok.
        let sets: Vec<RecoverySet> = (0..9u8).map(|i| set(0x10 + i, 4)).collect();
        let files: Vec<ParsedFile> = (0..9)
            .map(|i| parsed(&format!("f{i}.flac"), false))
            .collect();
        let mut progress = VerifyProgress::new(0);
        for (i, s) in sets.iter().enumerate() {
            progress
                .staged
                .insert(i, staged("sha", 1, Some(hex::encode(s.set_id))));
        }
        let outcome = finalize(&files, &sets, &mut progress);
        let VerifyOutcome::RepairNeeded {
            par2_set_id,
            missing_blocks,
        } = outcome
        else {
            panic!("expected repair, got {outcome:?}");
        };
        assert_eq!(missing_blocks, 9);
        assert_eq!(par2_set_id.len(), 64);
        assert_eq!(progress.repair_set_ids.len(), 9);
        let decided = crate::repair::decide_repair(
            "job_0123456789abcdef",
            "user",
            "idem",
            &par2_set_id,
            missing_blocks,
            1,
        );
        assert!(matches!(decided, Ok(Some(_))), "{decided:?}");
    }

    #[test]
    fn finalize_single_set_dispatch_keeps_its_own_id() {
        let a = set(0x0a, 4);
        let a_hex = hex::encode(a.set_id);
        let files = vec![parsed("a.flac", false)];
        let mut progress = VerifyProgress::new(0);
        progress
            .staged
            .insert(0, staged("sha", 2, Some(a_hex.clone())));
        let outcome = finalize(&files, &[a], &mut progress);
        assert_eq!(
            outcome,
            VerifyOutcome::RepairNeeded {
                par2_set_id: a_hex,
                missing_blocks: 2,
            }
        );
        assert!(progress.repair_set_ids.is_empty());
    }

    #[test]
    fn crc32_zeros_matches_hashing_real_zeros() {
        for n in [0u64, 1, 7, 255, 256, 100_000, (1 << 20) + 3] {
            let mut fast = crc32fast::Hasher::new();
            fast.update(b"prefix");
            crc32_zeros(&mut fast, n);
            fast.update(b"suffix");
            let mut slow = crc32fast::Hasher::new();
            slow.update(b"prefix");
            let zeros = [0u8; 8192];
            let mut remaining = n;
            while remaining > 0 {
                let take = remaining.min(zeros.len() as u64) as usize;
                slow.update(&zeros[..take]);
                remaining -= take as u64;
            }
            slow.update(b"suffix");
            assert_eq!(fast.finalize(), slow.finalize(), "n={n}");
        }
    }

    #[test]
    fn block_counter_counts_across_zero_runs() {
        // Block size 8: block 0 = "abcd" + 4 zeros (good), block 1 =
        // 8 zeros (want differs), block 2 = 4 zeros padded (good).
        let crc = |bytes: &[u8]| crc32fast::hash(bytes);
        let wants = [crc(b"abcd\0\0\0\0"), crc(b"not-zero"), crc(&[0u8; 8])];
        let mut counter = BlockCounter::new(&wants, 8, "fp".into());
        counter.bytes(b"abcd");
        counter.zeros(16);
        assert_eq!(counter.finish(), 1);
    }

    #[test]
    fn block_counter_resumes_from_a_boundary_snapshot() {
        // Same stream as above, split at a snapshot taken mid-block:
        // the resumed counter must reach the uninterrupted verdict,
        // which pins the snapshot -> `new_with_initial_len` rebuild.
        let crc = |bytes: &[u8]| crc32fast::hash(bytes);
        let wants = [crc(b"abcd\0\0\0\0"), crc(b"not-zero"), crc(&[0u8; 8])];
        let mut first = BlockCounter::new(&wants, 8, "fp".into());
        first.bytes(b"abc");
        first.boundary(1, 3);
        let cursor = first.boundary.expect("snapshot");
        assert!(valid_cursor(&cursor, "fp", 4, 20, 8, wants.len()));
        let mut resumed = BlockCounter::resume(&wants, 8, &cursor);
        resumed.bytes(b"d");
        resumed.zeros(16);
        assert_eq!(resumed.finish(), 1);
    }

    #[test]
    fn valid_cursor_rejects_foreign_or_inconsistent_state() {
        let base = JudgeCursor {
            segment: 1,
            pos: 10,
            block_index: 1,
            in_block: 2,
            crc32: 7,
            bad: 1,
            fingerprint: "fp".into(),
        };
        assert!(valid_cursor(&base, "fp", 4, 64, 8, 8));
        assert!(!valid_cursor(&base, "other", 4, 64, 8, 8), "foreign");
        assert!(
            !valid_cursor(
                &JudgeCursor {
                    pos: 11,
                    ..base.clone()
                },
                "fp",
                4,
                64,
                8,
                8
            ),
            "pos disagrees with the block state"
        );
        assert!(
            !valid_cursor(
                &JudgeCursor {
                    bad: 2,
                    ..base.clone()
                },
                "fp",
                4,
                64,
                8,
                8
            ),
            "more bad blocks than finished blocks"
        );
        assert!(
            !valid_cursor(
                &JudgeCursor {
                    in_block: 8,
                    pos: 16,
                    ..base.clone()
                },
                "fp",
                4,
                64,
                8,
                8
            ),
            "a full block would have finished"
        );
        assert!(
            !valid_cursor(
                &JudgeCursor {
                    segment: 5,
                    ..base.clone()
                },
                "fp",
                4,
                64,
                8,
                8
            ),
            "segment out of range"
        );
        // All blocks finished: pos may run past the covered bytes.
        let done = JudgeCursor {
            segment: 4,
            pos: 70,
            block_index: 8,
            in_block: 0,
            crc32: 0,
            bad: 3,
            fingerprint: "fp".into(),
        };
        assert!(valid_cursor(&done, "fp", 4, 80, 8, 8));
    }

    #[test]
    fn spool_digest_binds_the_segment_records() {
        let segs = vec![srec(4), smissing()];
        let a = spool_digest(&segs, 12);
        assert_eq!(a, spool_digest(&segs, 12), "deterministic");
        assert_ne!(a, spool_digest(&segs, 13), "length is bound");
        let mut changed = segs.clone();
        changed[0].crc32 = 42;
        assert_ne!(a, spool_digest(&changed, 12), "records are bound");
        assert_eq!(a.len(), 64);
    }

    struct MapScratch(HashMap<String, Vec<u8>>);

    impl ScratchStore for MapScratch {
        async fn head(&self, _key: &str) -> Result<Option<StagedFile>, VerifyError> {
            Ok(None)
        }

        async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, VerifyError> {
            Ok(self.0.get(key).cloned())
        }

        async fn put(
            &self,
            _key: &str,
            _bytes: Vec<u8>,
            _meta: &StagedFile,
        ) -> Result<(), VerifyError> {
            Ok(())
        }

        async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, VerifyError> {
            Ok(self
                .0
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect())
        }
    }

    fn krec(key: &str, size: u64) -> StagedSpoolSegment {
        StagedSpoolSegment {
            key: key.into(),
            size,
            crc32: 0,
            missing: false,
        }
    }

    #[tokio::test]
    async fn leading_run_without_part_size_is_capped_at_max_file_bytes() {
        // Present non-final parts disagree in size (10 vs 20), so no
        // part size is proven; across deliveries the spooled prefix
        // can outgrow the isolate, so assembly stops at the cap.
        let segs = vec![krec("a", 10), krec("b", 20), krec("c", 30), smissing()];
        let scratch = MapScratch(HashMap::from([
            ("a".to_string(), vec![1u8; 10]),
            ("b".to_string(), vec![2u8; 20]),
            ("c".to_string(), vec![3u8; 30]),
        ]));
        let caps = VerifyCaps {
            max_file_bytes: 25,
            ..VerifyCaps::default()
        };
        let loaded = load_segments(&segs, 4, None, None, &caps, &scratch)
            .await
            .expect("load")
            .expect("some");
        match loaded {
            LoadedFile::Assembled { bytes, .. } => assert_eq!(bytes.len(), 10),
            LoadedFile::Spooled { .. } => panic!("leading run assembles"),
        }
    }

    #[tokio::test]
    async fn vanished_spool_object_reads_as_zeros_and_counts() {
        let segs = vec![krec("a", 4), krec("b", 4), smissing()];
        let scratch = MapScratch(HashMap::from([("a".to_string(), vec![7u8; 4])]));
        let loaded = load_segments(&segs, 3, None, None, &VerifyCaps::default(), &scratch)
            .await
            .expect("load")
            .expect("some");
        match loaded {
            LoadedFile::Assembled { bytes, vanished } => {
                assert_eq!(bytes.len(), 12);
                assert_eq!(&bytes[..4], &[7u8; 4]);
                assert_eq!(&bytes[4..], &[0u8; 8]);
                assert_eq!(vanished, 1);
            }
            LoadedFile::Spooled { .. } => panic!("small file assembles"),
        }
    }

    #[tokio::test]
    async fn lying_par2_length_is_capped_by_geometry_and_the_nzb_bound() {
        // A par2 declaring 1 TiB for a 3-part file whose proven
        // geometry is 12 bytes: the judgement length is the geometry,
        // never the declared walk.
        let segs = vec![krec("a", 4), smissing(), smissing()];
        let scratch = MapScratch(HashMap::from([("a".to_string(), vec![7u8; 4])]));
        let caps = VerifyCaps {
            max_file_bytes: 2,
            ..VerifyCaps::default()
        };
        let loaded = load_segments(&segs, 3, Some(1 << 40), Some(100), &caps, &scratch)
            .await
            .expect("load")
            .expect("some");
        match loaded {
            LoadedFile::Spooled { logical_len } => assert_eq!(logical_len, 12),
            LoadedFile::Assembled { .. } => panic!("over the in-memory cap spools"),
        }
    }

    #[tokio::test]
    async fn stream_logical_stops_at_the_deadline() {
        let segs = vec![krec("a", 4)];
        let scratch = MapScratch(HashMap::from([("a".to_string(), vec![7u8; 4])]));
        let wants = [crc32fast::hash(&[7u8; 4])];
        let mut counter = BlockCounter::new(&wants, 4, "fp".into());
        let stopped = stream_logical(&segs, 1, 4, 0, 0, &scratch, &|| true, &mut counter)
            .await
            .expect("stream");
        assert!(matches!(stopped, Streamed::Stopped));
    }

    #[tokio::test]
    async fn stream_logical_resumes_at_a_segment_boundary() {
        // Walk three 4-byte segments as one 12-byte block; stop after
        // the first, resume from the boundary snapshot, and the
        // verdict matches an uninterrupted walk without re-reading
        // segment 0 (it is gone from scratch by resume time).
        let segs = vec![krec("a", 4), krec("b", 4), krec("c", 4)];
        let logical: Vec<u8> = (0u8..12).collect();
        let wants = [crc32fast::hash(&logical)];
        let scratch = MapScratch(HashMap::from([
            ("a".to_string(), logical[0..4].to_vec()),
            ("b".to_string(), logical[4..8].to_vec()),
            ("c".to_string(), logical[8..12].to_vec()),
        ]));
        let polls = std::cell::Cell::new(0u32);
        let deadline = || {
            polls.set(polls.get() + 1);
            polls.get() > 1
        };
        let mut first = BlockCounter::new(&wants, 12, "fp".into());
        let stopped = stream_logical(&segs, 3, 12, 0, 0, &scratch, &deadline, &mut first)
            .await
            .expect("stream");
        assert!(matches!(stopped, Streamed::Stopped));
        let cursor = first.boundary.expect("boundary snapshot");
        assert_eq!((cursor.segment, cursor.pos), (1, 4));

        let scratch = MapScratch(HashMap::from([
            ("b".to_string(), logical[4..8].to_vec()),
            ("c".to_string(), logical[8..12].to_vec()),
        ]));
        let mut resumed = BlockCounter::resume(&wants, 12, &cursor);
        let done = stream_logical(
            &segs,
            3,
            12,
            cursor.segment,
            cursor.pos,
            &scratch,
            &|| false,
            &mut resumed,
        )
        .await
        .expect("stream");
        assert!(matches!(done, Streamed::Done));
        assert_eq!(resumed.finish(), 0);
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
        progress.judging.insert(
            1,
            JudgeCursor {
                segment: 2,
                pos: 24,
                block_index: 1,
                in_block: 8,
                crc32: 5,
                bad: 1,
                fingerprint: "fp".into(),
            },
        );
        let json = serde_json::to_string(&progress).unwrap();
        let back: VerifyProgress = serde_json::from_str(&json).unwrap();
        assert_eq!(back, progress);
    }

    fn header_with_len(len: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        out.extend_from_slice(b"PAR2\0PKT");
        out.extend_from_slice(&len.to_le_bytes());
        out
    }

    fn valid_64b_packet() -> Vec<u8> {
        let mut out = vec![0u8; 64];
        out[..8].copy_from_slice(b"PAR2\0PKT");
        out[8..16].copy_from_slice(&64u64.to_le_bytes());
        out
    }

    #[test]
    fn packet_walk_stops_on_max_declared_len() {
        // 16-byte header declaring len = u64::MAX: the alignment guard
        // (`MAX % 4 == 3`) stops the walk at prefix 0 — no panic, no alloc.
        let bytes = header_with_len(u64::MAX);
        assert_eq!(bytes.len(), 16);
        assert_eq!(complete_packet_prefix(&bytes), 0);
        assert!(!packets_within_bound(&bytes, u64::MAX));
    }

    #[test]
    fn packet_walk_overflow_needs_checked_add() {
        // Whole first packet (64 bytes) then the largest multiple of 4:
        // passes alignment, so only the checked-add arm can stop the walk
        // at 64. With wrapping arithmetic the walk would advance to 60.
        const ALIGNED_MAX: u64 = u64::MAX - 3;
        assert_eq!(ALIGNED_MAX % 4, 0);
        let mut bytes = valid_64b_packet();
        bytes.extend_from_slice(&header_with_len(ALIGNED_MAX));
        assert_eq!(bytes.len(), 80);
        assert_eq!(complete_packet_prefix(&bytes), 64);
        assert_eq!(64usize.wrapping_add(ALIGNED_MAX as usize), 60);
        assert!(!packets_within_bound(&bytes, u64::MAX));
    }

    #[test]
    fn implied_len_overflow_is_none_for_present_missing_two_pow_63() {
        // Two segments [2^63 present, missing]: part size 2^63, last length
        // falls back to the part size, and 2^63 + 2^63 overflows u64, so no
        // geometry is proven. Wrapping would yield total 0 (small enough to
        // pass every cap), so this pin kills the wrapping mutant.
        const TWO_POW_63: u64 = 1 << 63;
        let segs = vec![srec(TWO_POW_63), smissing()];
        assert_eq!(uniform_part_size(&segs, 2), Some(TWO_POW_63));
        assert_eq!(implied_len(&segs, 2), None);
        assert_eq!(TWO_POW_63.wrapping_add(TWO_POW_63), 0);
    }

    #[test]
    fn implied_len_all_present_sum_overflow_is_none() {
        // All-present [2^63, 2^63, 1]: the checked sum overflows, so no
        // geometry is proven. Wrapping would yield 1 and hide the overflow.
        const TWO_POW_63: u64 = 1 << 63;
        let segs = vec![srec(TWO_POW_63), srec(TWO_POW_63), srec(1)];
        assert_eq!(implied_len(&segs, 3), None);
        assert_eq!(TWO_POW_63.wrapping_add(TWO_POW_63).wrapping_add(1), 1);
    }

    #[test]
    fn match_set_file_length_fallback_is_checked() {
        // Overflowing part sizes prove no length: no match, no panic.
        const TWO_POW_63: u64 = 1 << 63;
        let segs = vec![srec(TWO_POW_63), srec(TWO_POW_63), srec(1)];
        let file = parsed("track.flac", false);
        let mut claimed = std::collections::HashSet::new();
        assert!(match_set_file(&[], &file, &segs, 3, &mut claimed).is_none());
    }
}
