//! Verifying runtime: fetch article bodies, verify par2 sets, stage to
//! scratch for the repair handoff.
//!
//! One Queue delivery runs [`run_verifying_step`] over the job's parsed
//! files. The step is resumable per file: every fetched file is staged to
//! the scratch bucket and recorded in [`VerifyProgress`] (stored beside
//! the job checkpoint; see `checkpoint::put_verify`) before the next file
//! starts, so a redelivery never refetches a file whose staged bytes
//! still check out. A lost progress record is rebuilt by adopting the
//! staged objects' own metadata, so scratch is the durable record and
//! the progress value is its index. Per-delivery caps
//! ([`VerifyCaps`] plus a deadline callback) bound how much one message
//! may spend; hitting a cap returns [`VerifyOutcome::Incomplete`] with the
//! progress so far, and the consumer redelivers to continue.
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

use nntune_core::par2::{RecoveryFile, RecoverySet};
use serde::{Deserialize, Serialize};

use crate::dispatch::content_sha256_hex;
use crate::jobs::validate_job_id;
use crate::parse::ParsedFile;

/// R2 binding for the ingest scratch bucket (see `wrangler.toml.example`).
pub const SCRATCH_BINDING: &str = "INGEST_SCRATCH";

/// Most files one Verifying step walks (the core parser's file cap).
pub const MAX_VERIFY_FILES: usize = 1024;

/// Per-delivery spend caps for [`run_verifying_step`]. The defaults are
/// sized for music posts inside a Worker isolate: the whole file being
/// verified is held in memory, so the file cap plus one article buffer
/// must stay well under the isolate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyCaps {
    /// Files fetched from the provider per delivery.
    pub max_files_per_delivery: usize,
    /// Segments fetched from the provider per delivery.
    pub max_segments_per_delivery: usize,
    /// Article bytes fetched from the provider per delivery.
    pub max_bytes_per_delivery: u64,
    /// Largest assembled file verified in-Worker. Bigger files fail
    /// permanently instead of pressing isolate memory.
    pub max_file_bytes: u64,
    /// Largest single article body accepted from the pool.
    pub max_article_bytes: usize,
}

impl Default for VerifyCaps {
    fn default() -> Self {
        Self {
            max_files_per_delivery: 16,
            max_segments_per_delivery: 512,
            max_bytes_per_delivery: 64 << 20,
            max_file_bytes: 48 << 20,
            max_article_bytes: 16 << 20,
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
}

/// Per-file resume state for one Verifying round, stored beside the job
/// checkpoint under its own KV key (see `checkpoint::put_verify`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyProgress {
    /// Repair round this progress belongs to (`r{round}` in scratch keys).
    pub round: u32,
    /// Staged files by NZB file index.
    pub staged: BTreeMap<usize, StagedFile>,
    /// The set the completed step dispatched (or would dispatch) repair
    /// for; `None` until every file is judged.
    #[serde(default)]
    pub par2_set_id: Option<String>,
    /// Blocks short in `par2_set_id`'s set once judged (0 before/complete).
    #[serde(default)]
    pub missing_blocks: u32,
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

    /// Staged-input key for one file: the plan's
    /// `ingest-scratch/{job_id}/r{round}/in/{sha256(file_name)}`. Hashing
    /// the name keeps hostile NZB names out of key space entirely.
    pub fn file_key(job_id: &str, round: u32, file_name: &str) -> String {
        format!(
            "{}in/{}",
            Self::scratch_prefix(job_id, round),
            content_sha256_hex(file_name.as_bytes())
        )
    }

    /// Manifest key for one round (next to `in/`, per the scratch contract).
    pub fn manifest_key(job_id: &str, round: u32) -> String {
        format!("{}manifest.json", Self::scratch_prefix(job_id, round))
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

/// Article-body source: the pool Durable Object in production, a direct
/// NNTP client in tests. `Ok(None)` is a `430` (article gone).
pub trait BodySource {
    fn fetch_body<'a>(
        &'a mut self,
        segment_id: &'a str,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, VerifyError>> + 'a;
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
    pub role: &'static str,
}

/// Decoded segments of one file, in NZB order.
enum Seg {
    Present {
        bytes: Vec<u8>,
        crc32: u32,
    },
    /// Article gone (`430`) or undecodable: no trustworthy bytes.
    Missing,
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
            sets = load_sets(files, progress, scratch).await?;
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
                    dirty = true;
                }
            }
        } else {
            // No record (first delivery, or a lost progress write): a
            // staged object from an earlier delivery carries its own
            // judgement in its metadata — adopt it instead of refetching.
            let key = VerifyProgress::file_key(job_id, round, &file.name);
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
        }

        if spend.exhausted(caps) || deadline_exceeded() {
            write_manifest(job_id, round, files, progress, scratch, dirty).await?;
            return Ok(VerifyOutcome::Incomplete);
        }

        let segments = fetch_segments(file, source, caps, &mut spend, deadline_exceeded).await?;
        let Some(segments) = segments else {
            // Cap or deadline hit mid-file: nothing staged for this
            // file, the next delivery refetches it whole.
            write_manifest(job_id, round, files, progress, scratch, dirty).await?;
            return Ok(VerifyOutcome::Incomplete);
        };
        spend.files += 1;

        let matched = if file.is_par2 {
            None
        } else {
            match_set_file(&sets, file, &segments, &mut claimed)
        };
        let Some(assembled) = assemble(
            &segments,
            matched.map(|(_, f)| f.length),
            caps.max_file_bytes,
        ) else {
            return Err(VerifyError::Permanent(
                "assembled file exceeds the verify size cap".into(),
            ));
        };

        let (missing_blocks, par2_set_id) = if file.is_par2 {
            (0, None)
        } else {
            judge_file(matched, &segments, &assembled)
        };

        let key = VerifyProgress::file_key(job_id, round, &file.name);
        let staged = StagedFile {
            size: assembled.len() as u64,
            sha256: content_sha256_hex(&assembled),
            key: key.clone(),
            missing_blocks,
            par2_set_id,
        };
        scratch.put(&key, assembled, &staged).await?;
        progress.staged.insert(idx, staged);
        dirty = true;
    }

    write_manifest(job_id, round, files, progress, scratch, dirty).await?;
    Ok(finalize(files, &sets, progress))
}

/// Fetch and yEnc-decode every segment of one file. `Ok(None)` means a
/// cap or the deadline stopped the fetch mid-file.
async fn fetch_segments<B: BodySource>(
    file: &ParsedFile,
    source: &mut B,
    caps: &VerifyCaps,
    spend: &mut Spend,
    deadline_exceeded: &dyn Fn() -> bool,
) -> Result<Option<Vec<Seg>>, VerifyError> {
    let mut segments = Vec::with_capacity(file.segment_ids.len());
    let mut decoded_total: u64 = 0;
    for id in &file.segment_ids {
        if spend.segments >= caps.max_segments_per_delivery
            || spend.bytes >= caps.max_bytes_per_delivery
            || deadline_exceeded()
        {
            return Ok(None);
        }
        spend.segments += 1;
        let Some(article) = source.fetch_body(id).await? else {
            segments.push(Seg::Missing);
            continue;
        };
        spend.bytes = spend.bytes.saturating_add(article.len() as u64);
        if article.len() > caps.max_article_bytes {
            return Err(VerifyError::Permanent(
                "article exceeds the verify size cap".into(),
            ));
        }
        match decode_segment(id, &article) {
            Some(bytes) => {
                decoded_total = decoded_total.saturating_add(bytes.len() as u64);
                if decoded_total > caps.max_file_bytes {
                    return Err(VerifyError::Permanent(
                        "assembled file exceeds the verify size cap".into(),
                    ));
                }
                let crc32 = crc32fast::hash(&bytes);
                segments.push(Seg::Present { bytes, crc32 });
            }
            None => segments.push(Seg::Missing),
        }
    }
    Ok(Some(segments))
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

/// Assemble a file's bytes from its decoded segments, or `None` when
/// the implied length exceeds `max_file_bytes`. The implied length is
/// checked *before* any allocation: it derives from the untrusted par2
/// length and from part geometry over mostly-missing segments, neither
/// of which the fetched-bytes caps bound. All-present files concatenate
/// in order. With gaps, present parts are placed at `index * part_size`
/// when they prove one uniform part size (see the module docs);
/// otherwise only the contiguous present prefix is kept. The buffer is
/// sized from the par2 expected length when known, so the repair side
/// receives a full-length damaged file.
fn assemble(segments: &[Seg], expected_len: Option<u64>, max_file_bytes: u64) -> Option<Vec<u8>> {
    if segments.iter().all(|s| matches!(s, Seg::Present { .. })) {
        let total = segments.iter().try_fold(0u64, |acc, s| match s {
            Seg::Present { bytes, .. } => acc.checked_add(bytes.len() as u64),
            Seg::Missing => Some(acc),
        })?;
        if total > max_file_bytes {
            return None;
        }
        let mut out = Vec::with_capacity(total as usize);
        for seg in segments {
            if let Seg::Present { bytes, .. } = seg {
                out.extend_from_slice(bytes);
            }
        }
        return Some(out);
    }
    let part_size = uniform_part_size(segments);
    let Some(part_size) = part_size else {
        let mut total = 0u64;
        for seg in segments {
            match seg {
                Seg::Present { bytes, .. } => {
                    total = total.checked_add(bytes.len() as u64)?;
                }
                Seg::Missing => break,
            }
        }
        if total > max_file_bytes {
            return None;
        }
        let mut out = Vec::with_capacity(total as usize);
        for seg in segments {
            match seg {
                Seg::Present { bytes, .. } => out.extend_from_slice(bytes),
                Seg::Missing => break,
            }
        }
        return Some(out);
    };
    let total = match expected_len {
        Some(len) => len,
        None => {
            let last_len = match segments.last() {
                Some(Seg::Present { bytes, .. }) => bytes.len() as u64,
                _ => part_size,
            };
            part_size
                .checked_mul(segments.len().saturating_sub(1) as u64)?
                .checked_add(last_len)?
        }
    };
    if total > max_file_bytes {
        return None;
    }
    let mut out = vec![0u8; total as usize];
    for (i, seg) in segments.iter().enumerate() {
        if let Seg::Present { bytes, .. } = seg {
            let start = (i as u64 * part_size) as usize;
            if start >= out.len() {
                break;
            }
            let take = bytes.len().min(out.len() - start);
            out[start..start + take].copy_from_slice(&bytes[..take]);
        }
    }
    Some(out)
}

/// The uniform decoded part size a file's present segments prove: every
/// present segment except the file's last must share one length, and at
/// least one such segment must be present to prove it.
fn uniform_part_size(segments: &[Seg]) -> Option<u64> {
    let mut size: Option<u64> = None;
    for seg in segments.iter().take(segments.len().saturating_sub(1)) {
        if let Seg::Present { bytes, .. } = seg {
            match size {
                None => size = Some(bytes.len() as u64),
                Some(s) if s != bytes.len() as u64 => return None,
                _ => {}
            }
        }
    }
    size.filter(|s| *s > 0)
}

/// Parse the staged par2 files into recovery sets, grouped by set id.
/// A par2 file that does not parse alone (a volume carrying only
/// recovery slices has no Main packet) is retried concatenated onto each
/// parsed group and merged into the one whose set id it parses under —
/// its slices then count toward recovery. Bytes that parse under no
/// group and not alone are dropped (their slices are simply absent,
/// which the recoverability check then sees).
async fn load_sets<S: ScratchStore>(
    files: &[ParsedFile],
    progress: &VerifyProgress,
    scratch: &S,
) -> Result<Vec<RecoverySet>, VerifyError> {
    let mut groups: HashMap<[u8; 16], Vec<u8>> = HashMap::new();
    let mut unparsed: Vec<u8> = Vec::new();
    let mut any_par2 = false;
    for (idx, file) in files.iter().enumerate() {
        if !file.is_par2 {
            continue;
        }
        any_par2 = true;
        let Some(staged) = progress.staged.get(&idx) else {
            continue;
        };
        let Some(bytes) = scratch.get(&staged.key).await? else {
            continue;
        };
        match RecoverySet::parse(&bytes) {
            Ok(set) => groups
                .entry(set.set_id)
                .or_default()
                .extend_from_slice(&bytes),
            Err(_) => unparsed.extend_from_slice(&bytes),
        }
    }
    if !any_par2 {
        return Ok(Vec::new());
    }
    if !unparsed.is_empty() {
        let mut merged = false;
        for (set_id, bytes) in groups.iter_mut() {
            let mut trial = bytes.clone();
            trial.extend_from_slice(&unparsed);
            if let Ok(set) = RecoverySet::parse(&trial) {
                if set.set_id == *set_id {
                    *bytes = trial;
                    merged = true;
                    break;
                }
            }
        }
        if !merged {
            if let Ok(set) = RecoverySet::parse(&unparsed) {
                groups.entry(set.set_id).or_default().extend(unparsed);
            }
        }
    }
    let mut sets = Vec::new();
    for bytes in groups.into_values() {
        if let Ok(set) = RecoverySet::parse(&bytes) {
            sets.push(set);
        }
    }
    sets.sort_by_key(|s| s.set_id);
    Ok(sets)
}

/// Find the protected recovery-file entry covering one NZB file: exact
/// name first, then — for obfuscated names — a unique length match
/// against the assembled length. Claims the entry so two NZB files
/// cannot both match it.
fn match_set_file<'a>(
    sets: &'a [RecoverySet],
    file: &ParsedFile,
    segments: &[Seg],
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
    if segments.iter().all(|s| matches!(s, Seg::Present { .. })) {
        let len: u64 = segments
            .iter()
            .map(|s| match s {
                Seg::Present { bytes, .. } => bytes.len() as u64,
                Seg::Missing => 0,
            })
            .sum();
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
        for set in sets {
            if hex::encode(set.set_id) != *set_hex {
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
fn judge_file(
    matched: Option<(&RecoverySet, &RecoveryFile)>,
    segments: &[Seg],
    assembled: &[u8],
) -> (u32, Option<String>) {
    let missing_segments = segments
        .iter()
        .filter(|s| matches!(s, Seg::Missing))
        .count() as u32;
    let Some((set, entry)) = matched else {
        // No set covers this file: healthy iff every segment decoded.
        return (missing_segments, None);
    };
    let set_hex = hex::encode(set.set_id);
    if missing_segments == 0 {
        let seg_crcs: Vec<(u32, u64)> = segments
            .iter()
            .map(|s| match s {
                Seg::Present { bytes, crc32 } => (*crc32, bytes.len() as u64),
                Seg::Missing => (0, 0),
            })
            .collect();
        // Cheapest proof first: precheck recombines decode-time CRCs;
        // verify rehashes only when precheck cannot vouch for the file.
        if set
            .precheck_bytes(&entry.filename_bytes, &seg_crcs)
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
    // Deterministic order (BTreeMap): the first short set is dispatched;
    // later sets dispatch on later repair rounds.
    if let Some((set_id, missing)) = missing_by_set.iter().next() {
        let recovery = recovery_by_set.get(set_id).copied().unwrap_or(0);
        progress.par2_set_id = Some(set_id.clone());
        progress.missing_blocks = *missing;
        if *missing > recovery {
            return VerifyOutcome::Unrecoverable {
                par2_set_id: Some(set_id.clone()),
                missing_blocks: *missing,
            };
        }
        return VerifyOutcome::RepairNeeded {
            par2_set_id: set_id.clone(),
            missing_blocks: *missing,
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
                role: if file.is_par2 { "par2" } else { "data" },
            })
        })
        .collect();
    let json = serde_json::to_string(&entries)
        .map_err(|_| VerifyError::Transient("manifest encode failed".into()))?;
    let key = VerifyProgress::manifest_key(job_id, round);
    let meta = StagedFile {
        size: json.len() as u64,
        sha256: content_sha256_hex(json.as_bytes()),
        key: key.clone(),
        missing_blocks: 0,
        par2_set_id: None,
    };
    scratch.put(&key, json.into_bytes(), &meta).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(bytes: &[u8]) -> Seg {
        Seg::Present {
            crc32: crc32fast::hash(bytes),
            bytes: bytes.to_vec(),
        }
    }

    const JOB: &str = "job_x";

    #[test]
    fn scratch_keys_follow_the_plan_layout() {
        assert_eq!(
            VerifyProgress::scratch_prefix(JOB, 2),
            "ingest-scratch/job_x/r2/"
        );
        let key = VerifyProgress::file_key(JOB, 0, "a/b.flac");
        assert!(key.starts_with("ingest-scratch/job_x/r0/in/"));
        assert!(!key.contains("a/b"), "hostile names never reach key space");
        assert_eq!(
            VerifyProgress::manifest_key(JOB, 0),
            "ingest-scratch/job_x/r0/manifest.json"
        );
    }

    #[test]
    fn assemble_places_gaps_by_uniform_part_size() {
        let segments = vec![seg(b"aaaa"), Seg::Missing, seg(b"cc")];
        let out = assemble(&segments, Some(10), 1024).expect("within cap");
        assert_eq!(out, b"aaaa\0\0\0\0cc");
    }

    #[test]
    fn assemble_without_uniform_proof_keeps_the_present_prefix() {
        let segments = vec![seg(b"aaa"), Seg::Missing, seg(b"ccccc")];
        // Only one present non-last part of size 3 proves the part size.
        let out = assemble(&segments, None, 1024).expect("within cap");
        assert_eq!(out.len(), 3 + 3 + 5);
        let segments = vec![Seg::Missing, seg(b"ccccc")];
        let out = assemble(&segments, None, 1024).expect("within cap");
        assert!(out.is_empty(), "no proof, no prefix");
    }

    #[test]
    fn assemble_refuses_implied_lengths_over_the_cap_before_allocating() {
        // The par2-declared length is untrusted: a 100 MB declaration with
        // most segments missing must fail the cap check, not allocate the
        // implied file first (and never truncate a >4 GiB length into a
        // short buffer on wasm32).
        let segments = vec![seg(b"aaaa"), Seg::Missing, Seg::Missing];
        assert_eq!(assemble(&segments, Some(100 << 20), 1 << 20), None);
        assert_eq!(assemble(&segments, Some(u64::MAX), 48 << 20), None);
        // Implied geometry without par2 is capped the same way.
        let segments = vec![seg(&vec![0u8; 700_000]), Seg::Missing, Seg::Missing];
        assert_eq!(assemble(&segments, None, 1 << 20), None);
    }

    #[test]
    fn progress_roundtrips_with_legacy_defaults() {
        let legacy = serde_json::json!({"round": 1, "staged": {}});
        let progress: VerifyProgress = serde_json::from_value(legacy).unwrap();
        assert_eq!(progress.round, 1);
        assert!(progress.par2_set_id.is_none());
        assert_eq!(progress.missing_blocks, 0);
    }
}
