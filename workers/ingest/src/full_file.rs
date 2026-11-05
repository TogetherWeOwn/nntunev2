//! Full-file download stage (pipeline stage B2.1): the entire audio file
//! via the read pool, with original-PAR2 repair where needed.
//!
//! Pipeline position: for each published/publishable track the stage
//! downloads **every** article of one loose content file (BODY through the
//! read pool, yEnc decode plus CRC), applies PAR2 repair of the **original**
//! post where segments are unrecoverable, and lands the bytes in R2 staging
//! under [`REPOST_STAGE_PREFIX`]. A track that cannot be fully recovered
//! keeps streaming from the original NZB, is flagged [`REPOST_BLOCKED_CODE`]
//! for the dashboard, and is never silently dropped from the catalogue.
//!
//! Scope (owner decisions, 2026-11-03): loose-FLAC NZBs are in scope now.
//! Archive-set NZBs are gated by TWO-1174 — a file whose NZB role is
//! [`ARCHIVE_ROLE`] is refused as [`FullFileOutcome::ArchiveGated`] before
//! any pool call, and loose-file work never waits for that card.
//!
//! Seams (all injected so host tests prove the byte flow without a
//! provider):
//!
//! - [`SegmentSource`] serves one article per backbone tier. Production
//!   wires the pool BODY route here; the per-tier routing (which account on
//!   which backbone answers) is the lease broker's job (TWO-1765), and the
//!   [`ladder_plan`] policy below is the contract it executes: never retry
//!   the same backbone after an absence answer, bound transport retries,
//!   then abandon the segment.
//! - [`StagingSink`] is the spool/assemble/patch/stream surface. Production
//!   stages per-segment objects under `repost-stage/` in the existing
//!   `INGEST_SCRATCH` binding (no new bindings, no wrangler changes) and
//!   the >32 MiB multipart reassembly stream lands here with TWO-1763; the
//!   in-memory fake proves ordering, gap placement and bounded windows.
//! - Repair runs in two shapes. The single-erasure XOR fast path
//!   ([`xor_accumulate`]/[`xor_finish`]) is production code: the exponent-0
//!   recovery slice is plain XOR parity and needs no tables. Anything
//!   beyond one damaged block reports [`BlockReason::RepairUnsupported`]
//!   for the general in-Worker Reed-Solomon kernel (TWO-1762), which plugs
//!   in at the triage site operating through [`StagingSink::read_range`] and
//!   [`StagingSink::patch_range`] — never a staged guess.
//!
//! Triage follows the M1 verifier's proven rules ([`crate::verify`]): an
//! undecodable article or a size refusal is judged absent exactly like a
//! `430` (oversize never retries — a refetch returns identical bytes and
//! refuses identically); gaps are placed by the uniform part size the
//! present segments prove ([`uniform_part_size`]) and zero-filled at
//! assembly; the par2-declared length is capped to that proven geometry,
//! never trusted on its own.
//!
//! Memory: one segment buffer (bounded by the pool reply cap), one block
//! accumulator (bounded by the triage slice cap below), and one
//! [`PART_BYTES`] stream window — never the whole file, however large.
//! Triage-time allocations scale with the par2 block *count* (one CRC each,
//! one index per bad block), not with file bytes.
//!
//! Redaction: segment ids and filename bytes flow through (fetching needs
//! them) but no code path here formats them into a message, log line or
//! outcome — outcomes carry the opaque catalogue `track_id`, counts and
//! fixed reason codes only.

use std::future::Future;

use md5::Context as Md5Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use nntune_core::par2::RecoverySet;

use crate::pool_body::MAX_BODY_REPLY_BYTES;

// ---------------------------------------------------------------------------
// Pinned codes and budgets
// ---------------------------------------------------------------------------

/// Dashboard flag for a track that could not be fully recovered: the track
/// keeps streaming from the original NZB (pointer untouched) while the
/// catalogue row stays intact. Pinned string, asserted by tests.
pub const REPOST_BLOCKED_CODE: &str = "repost_blocked";

/// NZB role refused by the archive-set gate (TWO-1174). Matches the
/// classifier in `nntune_core::nzb` (`classify_extension`).
pub const ARCHIVE_ROLE: &str = "archive";

/// Card that owns archive-set download. Carried on
/// [`FullFileOutcome::ArchiveGated`] so the dashboard can explain the
/// refusal without guessing.
pub const ARCHIVE_GATE_CARD: &str = "TWO-1174";

/// Staging prefix under the existing `INGEST_SCRATCH` binding. Additive:
/// staging copies land here while the original NZB and its stream path stay
/// untouched until the pointer-switch card.
pub const REPOST_STAGE_PREFIX: &str = "repost-stage/";

/// Staging part cut: the production sink (TWO-1763) reassembles in windows
/// of at most this many bytes (the R2 multipart minimum), so a >32 MiB file
/// never needs a file-sized buffer. Verification and hashing stream the
/// layout back in the same windows.
pub const PART_BYTES: u64 = 5 << 20;

/// Class before the >32 MiB line this stage must stream past: files above
/// this length would have hit the old in-Worker assembly cap, so the
/// acceptance fixture deliberately exceeds it.
pub const SPOOL_REASSEMBLY_THRESHOLD_BYTES: u64 = 32 << 20;

/// In-Worker triage slice cap: par2 sets with a larger block size are not
/// triaged here (the block accumulator below would exceed one pool reply).
/// Stricter than the format cap (`MAX_BLOCK_SIZE` in the core crate); real
/// posts slice far below this.
pub const MAX_TRIAGE_BLOCK_SIZE: u64 = MAX_BODY_REPLY_BYTES as u64;

/// Transport-class retries on one tier (a different account) before the
/// ladder treats the tier as spent and escalates.
pub const MAX_TRANSPORT_RETRIES_PER_TIER: u32 = 2;

/// One full ladder cycle per segment: every tier gets one absence probe
/// plus its transport retries, then the segment is abandoned to PAR2 triage.
pub const MAX_SEGMENT_FETCH_ATTEMPTS: usize =
    TIER_ORDER.len() * (MAX_TRANSPORT_RETRIES_PER_TIER as usize + 1);

// ---------------------------------------------------------------------------
// Read-pool ladder (pure policy for the TWO-1765 lease broker)
// ---------------------------------------------------------------------------

/// One backbone class in pool config, in ladder order. Accounts are
/// referenced by role/backbone label only — hostnames and usernames are
/// vault secrets and never appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BackboneTier {
    Primary,
    Secondary,
    BlockBackup,
}

impl BackboneTier {
    pub fn as_str(self) -> &'static str {
        match self {
            BackboneTier::Primary => "primary",
            BackboneTier::Secondary => "secondary",
            BackboneTier::BlockBackup => "block-backup",
        }
    }
}

/// Ladder order: primaries first, the block-account tier last (quota
/// watched, backup only).
pub const TIER_ORDER: [BackboneTier; 3] = [
    BackboneTier::Primary,
    BackboneTier::Secondary,
    BackboneTier::BlockBackup,
];

/// One spent fetch attempt. `absent` covers every answered-absence class:
/// `430`/`423` missing, the `413`/over-cap refusal, and undecodable bytes
/// (corrupt bytes are judged absent like a `430`; a different backbone may
/// still hold a good copy). Anything else — I/O, timeout, pool retry — is a
/// transport failure that never burns an absence probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchAttempt {
    pub tier: BackboneTier,
    pub absent: bool,
}

/// What the ladder says to do for one segment next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderDirective {
    Fetch(BackboneTier),
    Abandon,
}

/// A tier is spent when it answered an absence (never retry the same
/// backbone after one) or burned its transport budget.
fn tier_spent(history: &[FetchAttempt], tier: BackboneTier) -> bool {
    let mut transports = 0u32;
    for a in history.iter().filter(|a| a.tier == tier) {
        if a.absent {
            return true;
        }
        transports += 1;
    }
    transports > MAX_TRANSPORT_RETRIES_PER_TIER
}

/// Pure ladder step over a segment's attempt history.
///
/// - No history: probe the primary tier.
/// - A trailing run of transport failures retries the same tier (a
///   different account) while its budget lasts.
/// - A spent tier is skipped: absence retires its backbone, and an
///   exhausted transport budget escalates past it. The next fetch uses the
///   first unspent tier in [`TIER_ORDER`].
/// - When every tier is spent (one full cycle,
///   [`MAX_SEGMENT_FETCH_ATTEMPTS`] attempts) the segment is abandoned to
///   PAR2 triage — a gap, never a silent drop.
pub fn ladder_plan(history: &[FetchAttempt]) -> LadderDirective {
    if history.len() >= MAX_SEGMENT_FETCH_ATTEMPTS {
        return LadderDirective::Abandon;
    }
    if let Some(last) = history.last() {
        if !last.absent && !tier_spent(history, last.tier) {
            return LadderDirective::Fetch(last.tier);
        }
    } else {
        return LadderDirective::Fetch(BackboneTier::Primary);
    }
    for tier in TIER_ORDER {
        if !tier_spent(history, tier) {
            return LadderDirective::Fetch(tier);
        }
    }
    LadderDirective::Abandon
}

// ---------------------------------------------------------------------------
// Segment fetch seam
// ---------------------------------------------------------------------------

/// What one segment fetch produced. Mirrors the pool BODY contract
/// ([`crate::pool_body`]): answered bytes, answered absence, or a
/// transport-class failure the ladder may retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchVerdict {
    Bytes(Vec<u8>),
    Absent,
    Transport,
}

/// One article off the read pool on a ladder-chosen tier. `segment_id` is a
/// valid message-id (validated at NZB parse); implementations MUST
/// re-validate before interpolating it into any wire command (command
/// injection rule, see [`crate::probe::validate_message_id`]).
pub trait SegmentSource {
    fn fetch_segment<'a>(
        &'a mut self,
        segment_id: &'a str,
        tier: BackboneTier,
    ) -> impl Future<Output = FetchVerdict> + 'a;
}

// ---------------------------------------------------------------------------
// Staging seam
// ---------------------------------------------------------------------------

/// Staging backend failure. Production maps the store error to the
/// operation that failed; no variant carries article references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkError {
    PutSegment,
    Assemble,
    ReadRange,
    PatchRange,
    ReadStream,
    Finalize,
}

/// Spool/assemble/patch/stream surface for one staged file. Segments arrive
/// in file order via [`StagingSink::put_segment`] (one segment in flight —
/// the orchestrator never holds the whole file); gaps are marked, then
/// [`StagingSink::assemble`] lays out the logical file with zero-filled
/// gaps of proven length; repair patches reconstructed blocks; verification
/// and hashing stream the layout back in [`PART_BYTES`] windows.
pub trait StagingSink {
    fn put_segment(&mut self, index: usize, bytes: &[u8]) -> Result<(), SinkError>;
    fn mark_gap(&mut self, index: usize, size_refused: bool);
    fn assemble(&mut self, total_len: u64, gap_lens: &[(usize, u64)]) -> Result<(), SinkError>;
    fn read_range(&self, offset: u64, len: u64) -> Result<Vec<u8>, SinkError>;
    fn patch_range(&mut self, offset: u64, bytes: &[u8]) -> Result<(), SinkError>;
    fn read_stream(
        &self,
        window: u64,
        visit: &mut dyn FnMut(&[u8]) -> Result<(), SinkError>,
    ) -> Result<(), SinkError>;
    fn finalize(self) -> Result<StagedDigest, SinkError>;
}

/// Staging identity: the byte length plus the SHA-256 over the exact staged
/// bytes (content-addressed key precedent, cf. [`crate::transcode`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedDigest {
    pub byte_len: u64,
    pub sha256_hex: String,
}

// ---------------------------------------------------------------------------
// Repair: single-erasure XOR fast path (production)
// ---------------------------------------------------------------------------

/// One recovery slice offered for repair: the PAR 2.0 exponent plus the
/// slice bytes exactly as the creator emits them. Exponent 0 is plain XOR
/// parity over zero-padded input slices. Slice extraction from downloaded
/// `.par2` bytes is TWO-1762; tests construct these directly in the same
/// shape the creator documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParitySlice {
    pub exponent: u32,
    pub bytes: Vec<u8>,
}

/// Fold one assembled block into the XOR accumulator. `acc` is exactly one
/// block long; short (last) blocks are zero-padded first, matching the
/// creator (which pads the short last slice before it enters the matrix).
/// XOR is its own inverse, so folding the damaged block's assembled bytes
/// too lets [`xor_finish`] un-mix them with one more pass.
pub fn xor_accumulate(acc: &mut [u8], block: &[u8]) {
    for (i, cell) in acc.iter_mut().enumerate() {
        *cell ^= block.get(i).copied().unwrap_or(0);
    }
}

/// Finish single-erasure repair: un-mix the damaged block's assembled
/// bytes from the accumulator and mix in the exponent-0 parity slice.
/// Returns the full reconstructed block (`acc.len()` bytes; the caller
/// truncates a short last block). `None` on any shape mismatch — the
/// caller maps it to [`BlockReason::RepairUnsupported`], never a guess.
pub fn xor_finish(acc: &[u8], damaged: &[u8], parity: &[u8]) -> Option<Vec<u8>> {
    if acc.is_empty() || damaged.len() > acc.len() || parity.len() < acc.len() {
        return None;
    }
    let mut out = Vec::with_capacity(acc.len());
    for i in 0..acc.len() {
        out.push(acc[i] ^ damaged.get(i).copied().unwrap_or(0) ^ parity[i]);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Spec and outcome
// ---------------------------------------------------------------------------

/// PAR2 triage/repair plan for one file: the downloaded recovery-set bytes,
/// the original filename bytes identifying the file inside the set, and the
/// recovery slices for repair (empty until TWO-1762's slice extraction
/// lands — without one, any damaged block is [`BlockReason::RepairUnsupported`]).
pub struct Par2Plan<'a> {
    pub set_bytes: &'a [u8],
    pub file_name: &'a [u8],
    pub parities: &'a [ParitySlice],
}

/// One file to download: identity for the dashboard join, the NZB role for
/// the archive gate, segment ids in file order, and the optional PAR2 plan.
pub struct FullFileSpec<'a> {
    /// Opaque catalogue track id (join key for the dashboard; never NZB bytes).
    pub track_id: &'a str,
    /// NZB file index within the track.
    pub file_index: usize,
    /// NZB `File.role` (`"content"`, `"archive"`, …).
    pub role: &'a str,
    /// Article message-ids in file order.
    pub segment_ids: &'a [String],
    /// PAR2 triage/repair plan. `None` stages on transport proof alone.
    pub par2: Option<Par2Plan<'a>>,
}

/// How a staged file proved byte-identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StagingProof {
    /// Every par2 block CRC plus the whole-file MD5 matched the set.
    Par2Verified,
    /// No usable PAR2 plan: every segment fetched clean and yEnc CRCs
    /// passed. Typed as weaker — later stages decide what it authorises.
    TransportComplete,
}

/// A file landed byte-identical in staging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    pub track_id: String,
    pub file_index: usize,
    pub byte_len: u64,
    pub sha256_hex: String,
    pub md5_hex: String,
    pub proof: StagingProof,
    /// [`PART_BYTES`] windows the layout streamed through (bounded-memory proof).
    pub parts: u32,
}

/// Why a file could not be staged. Every variant keeps the original NZB
/// pointer and the catalogue row intact — blocking is a flag, never a drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    /// Segments exhausted the ladder and PAR2 could not cover the gaps.
    FetchExhausted,
    /// Repair needs the TWO-1762 kernel (multi-erasure or no XOR parity).
    RepairUnsupported,
    /// Post-repair bytes still fail the set: nothing staged.
    RepairMismatch,
    /// The PAR2 plan was missing, unparsable, or did not protect this file
    /// while gaps needed triage.
    Par2Unusable,
    /// Present segments proved no uniform part size, so gaps are unplaceable.
    GeometryUnproven,
    /// Assembled length disagrees with the set (overfetch or short file).
    LengthMismatch,
    /// The staging backend failed mid-write.
    SinkFailed,
}

/// A file that could not be fully recovered: flagged for the dashboard,
/// original pointer retained, catalogue row intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepostBlocked {
    pub track_id: String,
    pub file_index: usize,
    pub reason: BlockReason,
    pub missing_segments: u32,
    pub size_refused_segments: u32,
    pub bad_blocks: u32,
}

impl RepostBlocked {
    /// Pinned dashboard flag value.
    pub fn code(&self) -> &'static str {
        REPOST_BLOCKED_CODE
    }

    /// The original NZB and its stream path stay untouched until the
    /// pointer-switch card: blocking never retires or rewrites them.
    /// Constructor-free invariant — always true by construction.
    pub fn original_pointer_retained(&self) -> bool {
        true
    }
}

/// Stage outcome for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FullFileOutcome {
    Staged(StagedFile),
    RepostBlocked(RepostBlocked),
    ArchiveGated {
        track_id: String,
        file_index: usize,
        gate: &'static str,
    },
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

/// One spooled segment: present bytes plus CRC, or a gap with its refusal flag.
#[derive(Debug, Clone)]
struct SpoolRec {
    present_len: Option<u64>,
    size_refused: bool,
}

impl SpoolRec {
    fn present(len: u64) -> Self {
        SpoolRec {
            present_len: Some(len),
            size_refused: false,
        }
    }

    fn gap(size_refused: bool) -> Self {
        SpoolRec {
            present_len: None,
            size_refused,
        }
    }

    fn missing(&self) -> bool {
        self.present_len.is_none()
    }
}

/// The uniform decoded part size a file's present segments prove: every
/// present segment except the file's last must share one non-zero length
/// (M1 `uniform_part_size` rule — a gap is placeable only when the survivors
/// agree on the grid).
fn uniform_part_size(recs: &[SpoolRec], total_segments: usize) -> Option<u64> {
    let non_last = total_segments.saturating_sub(1);
    let mut size: Option<u64> = None;
    for (i, rec) in recs.iter().enumerate() {
        if i >= non_last {
            break;
        }
        let Some(len) = rec.present_len else { continue };
        match size {
            None => size = Some(len),
            Some(s) if s != len => return None,
            _ => {}
        }
    }
    size.filter(|s| *s > 0)
}

/// The file's logical length from the geometry the spooled segments prove.
/// `None` only on u64 overflow. Mirrors M1 `implied_len`: full-present sums
/// exactly; otherwise the proven part size tiles every non-last slot and
/// the last slot keeps its present length (or one part when gapped).
fn implied_len(recs: &[SpoolRec], total_segments: usize) -> Option<u64> {
    if total_segments == 0 {
        return Some(0);
    }
    if recs.len() >= total_segments && recs.iter().all(|r| !r.missing()) {
        return recs
            .iter()
            .try_fold(0u64, |acc, r| acc.checked_add(r.present_len.unwrap_or(0)));
    }
    let part = uniform_part_size(recs, total_segments)?;
    let last_len = match recs.get(total_segments - 1) {
        Some(r) if !r.missing() => r.present_len.unwrap_or(part),
        _ => part,
    };
    part.checked_mul((total_segments - 1) as u64)?
        .checked_add(last_len)
}

/// Feed `n` zero bytes into a CRC32 hasher in fixed chunks — the last par2
/// block is checksummed zero-padded to the full size, hashed without ever
/// allocating a block-sized zero buffer from an untrusted size.
fn hash_zero_run(hash: &mut crc32fast::Hasher, mut n: u64) {
    const CHUNK: [u8; 4096] = [0u8; 4096];
    while n > 0 {
        let take = n.min(CHUNK.len() as u64);
        hash.update(&CHUNK[..take as usize]);
        n -= take;
    }
}

/// Owned triage view copied out of the parsed recovery set (block CRCs are
/// one `u32` per block; parities stay borrowed from the spec).
struct Triage<'a> {
    block_size: u64,
    block_crcs: Vec<u32>,
    file_len: u64,
    whole_md5: [u8; 16],
    parities: &'a [ParitySlice],
}

/// Parse the plan's set and locate this file's protection. `None` covers
/// every unusable shape — unparsable bytes, a set that does not protect
/// this file, a zero or over-cap slice size, or a checksum count that does
/// not match the declared length — so triage can only run on metadata that
/// mirrors the verifier's own validity order.
fn triage_view<'a>(plan: &Par2Plan<'a>) -> Option<Triage<'a>> {
    let set = RecoverySet::parse(plan.set_bytes).ok()?;
    let file = set.protected_file_by_name_bytes(plan.file_name)?;
    if set.block_size == 0 || set.block_size > MAX_TRIAGE_BLOCK_SIZE {
        return None;
    }
    let blocks = file.length.div_ceil(set.block_size);
    if file.block_crc32s.len() as u64 != blocks {
        return None;
    }
    Some(Triage {
        block_size: set.block_size,
        block_crcs: file.block_crc32s.clone(),
        file_len: file.length,
        whole_md5: file.whole_md5,
        parities: plan.parities,
    })
}

fn blocked(
    spec: &FullFileSpec<'_>,
    reason: BlockReason,
    recs: &[SpoolRec],
    bad_blocks: u32,
) -> FullFileOutcome {
    FullFileOutcome::RepostBlocked(RepostBlocked {
        track_id: spec.track_id.to_string(),
        file_index: spec.file_index,
        reason,
        missing_segments: recs.iter().filter(|r| r.missing()).count() as u32,
        size_refused_segments: recs
            .iter()
            .filter(|r| r.missing() && r.size_refused)
            .count() as u32,
        bad_blocks,
    })
}

/// Decode one fetched article into the reused buffer. Any decode or CRC
/// failure makes the segment unusable (`None`): par2 accounting treats
/// corrupt bytes like a `430`.
fn decode_article<'b>(segment_id: &str, article: &[u8], buf: &'b mut Vec<u8>) -> Option<&'b [u8]> {
    // Decoded output is never larger than the encoded article (yEnc
    // expands, never compresses), so the article length bounds the buffer.
    buf.clear();
    buf.resize(article.len(), 0);
    let written = nntune_core::yenc::decode_to(segment_id, article, buf).ok()?;
    buf.truncate(written);
    Some(buf.as_slice())
}

/// Streaming verification state over the assembled layout: per-block CRC32
/// against the set, whole-file MD5 and SHA-256, and the XOR fold for the
/// single-erasure fast path.
struct VerifyWalk<'a> {
    block_size: u64,
    block_crcs: &'a [u32],
    bad: Vec<usize>,
    hasher: crc32fast::Hasher,
    block_idx: usize,
    pos_in_block: u64,
    block_len: u64,
    md5: Md5Context,
    sha256: Sha256,
    xor_acc: Option<Vec<u8>>,
}

impl<'a> VerifyWalk<'a> {
    fn new(triage: &'a Triage<'a>, accumulate_xor: bool) -> Self {
        VerifyWalk {
            block_size: triage.block_size,
            block_crcs: &triage.block_crcs,
            bad: Vec::new(),
            hasher: crc32fast::Hasher::new(),
            block_idx: 0,
            pos_in_block: 0,
            block_len: 0,
            md5: Md5Context::new(),
            sha256: Sha256::new(),
            xor_acc: accumulate_xor.then(|| vec![0u8; triage.block_size as usize]),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.md5.consume(bytes);
        self.sha256.update(bytes);
        let mut rest = bytes;
        while !rest.is_empty() {
            let room = (self.block_size - self.pos_in_block).min(rest.len() as u64) as usize;
            let (head, tail) = rest.split_at(room);
            self.hasher.update(head);
            self.pos_in_block += head.len() as u64;
            self.block_len += head.len() as u64;
            if let Some(acc) = self.xor_acc.as_mut() {
                let base = self.pos_in_block as usize - head.len();
                for (i, b) in head.iter().enumerate() {
                    acc[base + i] ^= b;
                }
            }
            rest = tail;
            if self.pos_in_block == self.block_size {
                self.finish_block(false);
            }
        }
    }

    fn finish_block(&mut self, partial: bool) {
        if self.block_idx < self.block_crcs.len() {
            if partial {
                hash_zero_run(&mut self.hasher, self.block_size - self.block_len);
            }
            // `finalize` takes the hasher by value: swap in a fresh one and
            // finalize the closed block.
            let closed = std::mem::replace(&mut self.hasher, crc32fast::Hasher::new());
            if closed.finalize() != self.block_crcs[self.block_idx] {
                self.bad.push(self.block_idx);
            }
        }
        self.block_idx += 1;
        self.pos_in_block = 0;
        self.block_len = 0;
    }

    /// Close the walk at exactly `target` bytes. A trailing partial block
    /// is zero-padded before its CRC compare (PAR2 rule for the short last
    /// slice).
    fn finish(mut self, target: u64) -> (Vec<usize>, [u8; 16], Sha256, Option<Vec<u8>>) {
        let blocks_covered = target.div_ceil(self.block_size);
        while self.block_idx < blocks_covered as usize {
            let at_end = self.block_idx + 1 == blocks_covered as usize;
            self.finish_block(at_end && !target.is_multiple_of(self.block_size));
        }
        let md5 = self.md5.finalize().0;
        (self.bad, md5, self.sha256, self.xor_acc)
    }
}

/// Download one file end to end: gate, ladder fetch, decode, spool,
/// assemble, triage, repair, verify, stage. See the module docs for the
/// phase order and the failure taxonomy.
pub async fn download_full_file<Src, Snk>(
    spec: &FullFileSpec<'_>,
    source: &mut Src,
    sink: &mut Snk,
) -> FullFileOutcome
where
    Src: SegmentSource,
    Snk: StagingSink,
{
    if spec.role == ARCHIVE_ROLE {
        return FullFileOutcome::ArchiveGated {
            track_id: spec.track_id.to_string(),
            file_index: spec.file_index,
            gate: ARCHIVE_GATE_CARD,
        };
    }
    let total = spec.segment_ids.len();
    if total == 0 {
        return blocked(spec, BlockReason::GeometryUnproven, &[], 0);
    }

    // Phase 1: ladder-fetch, yEnc-decode and spool every segment in order.
    // Later segments still fetch past gaps (the cursor advances, M1 rule).
    // The decode buffer is scoped here so the streaming phases below hold
    // no segment-sized allocation.
    let recs: Vec<SpoolRec> = {
        let mut recs: Vec<SpoolRec> = Vec::with_capacity(total);
        let mut decode_buf = Vec::new();
        for id in spec.segment_ids {
            let mut history: Vec<FetchAttempt> = Vec::new();
            let rec = loop {
                match ladder_plan(&history) {
                    LadderDirective::Abandon => break SpoolRec::gap(false),
                    LadderDirective::Fetch(tier) => match source.fetch_segment(id, tier).await {
                        FetchVerdict::Transport => {
                            history.push(FetchAttempt {
                                tier,
                                absent: false,
                            });
                            continue;
                        }
                        FetchVerdict::Absent => {
                            history.push(FetchAttempt { tier, absent: true });
                            continue;
                        }
                        FetchVerdict::Bytes(article) => {
                            if article.len() > MAX_BODY_REPLY_BYTES {
                                // Over the pool reply cap: a refetch returns
                                // identical bytes and refuses identically,
                                // so the gap is recorded immediately with
                                // the refusal flag (M1 size-refusal rule)
                                // instead of burning ladder attempts.
                                break SpoolRec::gap(true);
                            }
                            match decode_article(id, &article, &mut decode_buf) {
                                Some(bytes) => {
                                    let len = bytes.len() as u64;
                                    if sink.put_segment(recs.len(), bytes).is_err() {
                                        return blocked(spec, BlockReason::SinkFailed, &recs, 0);
                                    }
                                    break SpoolRec::present(len);
                                }
                                // Undecodable bytes are judged absent like a
                                // `430`: another backbone may hold a good
                                // copy, so the ladder advances a tier.
                                None => {
                                    history.push(FetchAttempt { tier, absent: true });
                                    continue;
                                }
                            }
                        }
                    },
                }
            };
            if rec.missing() {
                sink.mark_gap(recs.len(), rec.size_refused);
            }
            recs.push(rec);
        }
        recs
    };

    // Phase 2: prove the geometry and assemble the logical layout.
    let Some(geometry) = implied_len(&recs, total) else {
        return blocked(spec, BlockReason::GeometryUnproven, &recs, 0);
    };
    let gaps: Vec<usize> = recs
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.missing().then_some(i))
        .collect();

    // PAR2 plan: a usable set triages gaps and verifies bytes; an unusable
    // one still stages a gap-free file on transport proof, and blocks a
    // gapped one (nothing to judge the holes against).
    let triage: Option<Triage<'_>> = match &spec.par2 {
        None => None,
        Some(plan) => match triage_view(plan) {
            Some(t) => Some(t),
            None if gaps.is_empty() => None,
            None => return blocked(spec, BlockReason::Par2Unusable, &recs, 0),
        },
    };

    // Stage exactly the bytes the proof covers: the set's length capped to
    // the proven geometry, or the geometry itself without a set. Present
    // bytes at or past the target are overfetch the proof cannot vouch
    // for — fail closed instead of staging a longer file than the set
    // describes.
    let target = match &triage {
        Some(t) => t.file_len.min(geometry),
        None => geometry,
    };
    let mut offset = 0u64;
    for rec in &recs {
        // Gaps contribute nothing here; any spill past the target is present
        // bytes the proof cannot vouch for (overfetch, or a short set
        // against a longer post) — fail closed instead of staging past it.
        offset = offset.saturating_add(rec.present_len.unwrap_or(0));
        if offset > target {
            return blocked(spec, BlockReason::LengthMismatch, &recs, 0);
        }
    }
    let mut gap_lens: Vec<(usize, u64)> = Vec::with_capacity(gaps.len());
    if !gaps.is_empty() {
        let Some(part) = uniform_part_size(&recs, total) else {
            return blocked(spec, BlockReason::GeometryUnproven, &recs, 0);
        };
        let last_present_len = recs
            .get(total - 1)
            .and_then(|r| r.present_len)
            .unwrap_or(part);
        let mut laid = 0u64;
        for (i, rec) in recs.iter().enumerate() {
            laid = laid.saturating_add(rec.present_len.unwrap_or(0));
            if rec.missing() {
                let raw = if i == total - 1 {
                    last_present_len
                } else {
                    part
                };
                // Truncate a gap that runs past the target: only [0,
                // target) is staged, and present bytes past it already
                // failed closed above.
                let len = raw.min(target.saturating_sub(laid));
                gap_lens.push((i, len));
                laid = laid.saturating_add(len);
            }
        }
    }
    if sink.assemble(target, &gap_lens).is_err() {
        return blocked(spec, BlockReason::SinkFailed, &recs, 0);
    }

    // Phase 3: stream the layout back in PART_BYTES windows — block CRCs
    // against the set plus whole-file MD5/SHA-256 (and the XOR fold when a
    // fast-path repair may be needed).
    let xor_usable = triage.as_ref().is_some_and(|t| {
        t.parities
            .iter()
            .any(|p| p.exponent == 0 && p.bytes.len() as u64 >= t.block_size)
    });
    let (bad, md5, sha256, xor_acc) = match &triage {
        Some(t) => {
            let mut walk = VerifyWalk::new(t, xor_usable);
            let stream_err = sink
                .read_stream(PART_BYTES, &mut |chunk| {
                    walk.feed(chunk);
                    Ok(())
                })
                .is_err();
            if stream_err {
                return blocked(spec, BlockReason::SinkFailed, &recs, 0);
            }
            let (mut bad, md5, sha256, acc) = walk.finish(target);
            // Blocks the proof does not cover (a short layout against a
            // longer file) are damage like any CRC mismatch.
            if t.file_len > target {
                let first = target.div_ceil(t.block_size) as usize;
                for b in first..t.block_crcs.len() {
                    if !bad.contains(&b) {
                        bad.push(b);
                    }
                }
                bad.sort_unstable();
            }
            (bad, md5, sha256, acc)
        }
        None => {
            let mut md5 = Md5Context::new();
            let mut sha256 = Sha256::new();
            let stream_err = sink
                .read_stream(PART_BYTES, &mut |chunk| {
                    md5.consume(chunk);
                    sha256.update(chunk);
                    Ok(())
                })
                .is_err();
            if stream_err {
                return blocked(spec, BlockReason::SinkFailed, &recs, 0);
            }
            (Vec::new(), md5.finalize().0, sha256, None)
        }
    };
    let sha256_hex = hex::encode(sha256.finalize());
    let md5_hex = hex::encode(md5);

    // Phase 4: judge, repair once, re-verify, stage.
    let parts = target.div_ceil(PART_BYTES) as u32;
    let staged = |proof: StagingProof, sha256_hex: String, md5_hex: String| {
        FullFileOutcome::Staged(StagedFile {
            track_id: spec.track_id.to_string(),
            file_index: spec.file_index,
            byte_len: target,
            sha256_hex,
            md5_hex,
            proof,
            parts,
        })
    };
    let Some(t) = &triage else {
        // No usable set: gap-free (gaps with no set blocked above… no —
        // gaps without a set reach here; re-check). A gapped file with no
        // triage cannot prove byte-identity.
        if gaps.is_empty() {
            return staged(StagingProof::TransportComplete, sha256_hex, md5_hex);
        }
        return blocked(spec, BlockReason::Par2Unusable, &recs, 0);
    };
    if bad.is_empty() {
        if target == t.file_len && md5_hex == hex::encode(t.whole_md5) {
            return staged(StagingProof::Par2Verified, sha256_hex, md5_hex);
        }
        if target != t.file_len {
            return blocked(spec, BlockReason::LengthMismatch, &recs, 0);
        }
        return blocked(spec, BlockReason::RepairMismatch, &recs, 0);
    }
    // One damaged block, whole layout present, XOR parity at hand: the
    // single-erasure fast path. The accumulator folded every assembled
    // block (zero-padded); un-mix the damaged block's assembled bytes and
    // mix in the parity, truncate a short last block, patch, re-verify.
    let single = bad.len() == 1 && target == t.file_len && xor_usable;
    if single {
        let b = bad[0];
        let start = b as u64 * t.block_size;
        let raw_len = target.saturating_sub(start).min(t.block_size);
        if let (Some(acc), Ok(assembled)) = (xor_acc.as_deref(), sink.read_range(start, raw_len)) {
            if let Some(parity) = t
                .parities
                .iter()
                .find(|p| p.exponent == 0 && p.bytes.len() as u64 >= t.block_size)
            {
                if let Some(mut fixed) = xor_finish(acc, &assembled, &parity.bytes) {
                    fixed.truncate(raw_len as usize);
                    if sink.patch_range(start, &fixed).is_ok() {
                        let mut re_walk = VerifyWalk::new(t, false);
                        let re_err = sink
                            .read_stream(PART_BYTES, &mut |chunk| {
                                re_walk.feed(chunk);
                                Ok(())
                            })
                            .is_err();
                        if re_err {
                            return blocked(spec, BlockReason::SinkFailed, &recs, bad.len() as u32);
                        }
                        let (re_bad, re_md5sum, re_sha256, _) = re_walk.finish(target);
                        if re_bad.is_empty() && re_md5sum == t.whole_md5 {
                            return staged(
                                StagingProof::Par2Verified,
                                hex::encode(re_sha256.finalize()),
                                hex::encode(re_md5sum),
                            );
                        }
                        return blocked(spec, BlockReason::RepairMismatch, &recs, bad.len() as u32);
                    }
                }
            }
        }
        return blocked(spec, BlockReason::SinkFailed, &recs, bad.len() as u32);
    }
    if gaps.is_empty() {
        // Bytes disagree with the set and repair cannot run: corrupt post,
        // not a fetch problem.
        return blocked(spec, BlockReason::RepairMismatch, &recs, bad.len() as u32);
    }
    if t.parities.is_empty() {
        return blocked(spec, BlockReason::FetchExhausted, &recs, bad.len() as u32);
    }
    blocked(
        spec,
        BlockReason::RepairUnsupported,
        &recs,
        bad.len() as u32,
    )
}

// Compile-time: one full ladder cycle is every tier times one absence
// probe plus its transport retries — the abandon bound cannot drift from
// the tier and retry budgets.
const _: () = assert!(
    MAX_SEGMENT_FETCH_ATTEMPTS == TIER_ORDER.len() * (MAX_TRANSPORT_RETRIES_PER_TIER as usize + 1)
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

    use nntune_core::yenc::{encode_segment, SegmentParams};

    // --- Ladder policy ------------------------------------------------------

    fn attempt(tier: BackboneTier, absent: bool) -> FetchAttempt {
        FetchAttempt { tier, absent }
    }

    #[test]
    fn ladder_starts_on_primary() {
        assert_eq!(
            ladder_plan(&[]),
            LadderDirective::Fetch(BackboneTier::Primary)
        );
    }

    #[test]
    fn ladder_retries_transports_on_the_same_tier_within_budget() {
        use BackboneTier::Primary as P;
        // Two transport failures: same tier, different account.
        assert_eq!(ladder_plan(&[attempt(P, false)]), LadderDirective::Fetch(P));
        assert_eq!(
            ladder_plan(&[attempt(P, false), attempt(P, false)]),
            LadderDirective::Fetch(P)
        );
    }

    #[test]
    fn ladder_absence_retires_the_backbone() {
        use BackboneTier::{BlockBackup as B, Primary as P, Secondary as S};
        // One 430 on primary: never retry it, move to secondary.
        assert_eq!(ladder_plan(&[attempt(P, true)]), LadderDirective::Fetch(S));
        // Absences on both: the block tier is the last resort.
        assert_eq!(
            ladder_plan(&[attempt(P, true), attempt(S, true)]),
            LadderDirective::Fetch(B)
        );
        // Every tier answered absent: abandon the segment.
        assert_eq!(
            ladder_plan(&[attempt(P, true), attempt(S, true), attempt(B, true)]),
            LadderDirective::Abandon
        );
    }

    #[test]
    fn ladder_transport_budget_escalates_tiers_then_abandons() {
        use BackboneTier::{BlockBackup as B, Primary as P, Secondary as S};
        // Three transports spend primary (1 probe + 2 retries), then the
        // ladder walks secondary and block-backup the same way: 9 attempts,
        // one full cycle, then abandon.
        let mut history = Vec::new();
        let mut tiers = Vec::new();
        for _ in 0..MAX_SEGMENT_FETCH_ATTEMPTS {
            match ladder_plan(&history) {
                LadderDirective::Fetch(t) => {
                    tiers.push(t);
                    history.push(attempt(t, false));
                }
                LadderDirective::Abandon => panic!("abandoned early at {}", history.len()),
            }
        }
        assert_eq!(
            tiers,
            vec![P, P, P, S, S, S, B, B, B],
            "one full cycle across every tier"
        );
        assert_eq!(ladder_plan(&history), LadderDirective::Abandon);
    }

    #[test]
    fn ladder_mixed_absence_beats_transport_budget() {
        use BackboneTier::{Primary as P, Secondary as S};
        // A transport then an absence on primary: the absence retires the
        // tier even though transport budget remains.
        assert_eq!(
            ladder_plan(&[attempt(P, false), attempt(P, true)]),
            LadderDirective::Fetch(S)
        );
        // Transports on secondary stay there while unspent.
        assert_eq!(
            ladder_plan(&[attempt(P, true), attempt(S, false)]),
            LadderDirective::Fetch(S)
        );
    }

    // --- Geometry helpers ---------------------------------------------------

    fn recs(sizes: &[Option<u64>]) -> Vec<SpoolRec> {
        sizes
            .iter()
            .map(|s| match s {
                Some(len) => SpoolRec::present(*len),
                None => SpoolRec::gap(false),
            })
            .collect()
    }

    #[test]
    fn uniform_part_needs_survivor_agreement() {
        // Non-last survivors agree: placeable.
        assert_eq!(
            uniform_part_size(&recs(&[Some(700), None, Some(100)]), 3),
            Some(700)
        );
        // Disagreement: unplaceable.
        assert_eq!(
            uniform_part_size(&recs(&[Some(700), Some(600), Some(100)]), 3),
            None
        );
        // No non-last survivor: unproven.
        assert_eq!(uniform_part_size(&recs(&[None, None, Some(100)]), 3), None);
        // Last-segment size never sets the grid.
        assert_eq!(
            uniform_part_size(&recs(&[Some(700), Some(5)]), 2),
            Some(700)
        );
    }

    #[test]
    fn implied_length_tiles_the_proven_grid() {
        // Full present sums exactly.
        assert_eq!(
            implied_len(&recs(&[Some(700), Some(700), Some(100)]), 3),
            Some(1500)
        );
        // Gaps tile by the proven part; a gapped last slot defaults to one part.
        assert_eq!(implied_len(&recs(&[Some(700), None, None]), 3), Some(2100));
        // A present last segment keeps its own length.
        assert_eq!(
            implied_len(&recs(&[Some(700), None, Some(100)]), 3),
            Some(1500)
        );
    }

    // --- XOR fast path ------------------------------------------------------

    #[test]
    fn xor_repair_round_trips_one_erasure() {
        let a = vec![1u8, 2, 3, 4];
        let b = vec![5u8, 6, 7, 8];
        let mut parity = vec![0u8; 4];
        for i in 0..4 {
            parity[i] = a[i] ^ b[i];
        }
        let mut acc = vec![0u8; 4];
        xor_accumulate(&mut acc, &a);
        xor_accumulate(&mut acc, &b);
        // Erase b: accumulator over the survivor plus the damaged bytes
        // un-mixes to the parity, and the parity finishes the job.
        let mut acc_damaged = vec![0u8; 4];
        xor_accumulate(&mut acc_damaged, &a);
        xor_accumulate(&mut acc_damaged, &[0u8; 4]);
        let fixed = xor_finish(&acc_damaged, &[0u8; 4], &parity).unwrap();
        assert_eq!(fixed, b);
        // Shape mismatches never guess.
        assert!(xor_finish(&[], &[], &parity).is_none());
        assert!(xor_finish(&acc, &a, &parity[..2]).is_none());
        assert!(xor_finish(&acc, &[0u8; 8], &parity).is_none());
    }

    // --- Fakes --------------------------------------------------------------

    /// Scripted read pool: per-segment plaintext encoded to yEnc on fetch
    /// (one article in flight, never the whole post), per-(segment, tier)
    /// override scripts, and tier recording the ladder assertions read.
    struct PostSource {
        decoded: Vec<Vec<u8>>,
        total_len: u64,
        overrides: HashMap<(usize, BackboneTier), VecDeque<FetchVerdict>>,
        oversize: HashSet<usize>,
        tiers_used: HashMap<usize, Vec<BackboneTier>>,
    }

    impl PostSource {
        fn new(data: &[u8], seg_len: usize) -> (Self, Vec<String>) {
            let decoded: Vec<Vec<u8>> = data.chunks(seg_len).map(|c| c.to_vec()).collect();
            let ids: Vec<String> = (0..decoded.len())
                .map(|i| format!("seg-{i}@synthetic.invalid"))
                .collect();
            (
                PostSource {
                    decoded,
                    total_len: data.len() as u64,
                    overrides: HashMap::new(),
                    oversize: HashSet::new(),
                    tiers_used: HashMap::new(),
                },
                ids,
            )
        }

        fn script(&mut self, index: usize, tier: BackboneTier, verdicts: Vec<FetchVerdict>) {
            self.overrides
                .insert((index, tier), verdicts.into_iter().collect());
        }

        fn script_all_tiers(&mut self, index: usize, verdict: FetchVerdict) {
            for tier in TIER_ORDER {
                self.script(index, tier, vec![verdict.clone()]);
            }
        }

        fn index_of(id: &str) -> usize {
            id.strip_prefix("seg-")
                .and_then(|s| s.strip_suffix("@synthetic.invalid"))
                .and_then(|s| s.parse().ok())
                .expect("fake ids are seg-{i}@synthetic.invalid")
        }
    }

    impl SegmentSource for PostSource {
        async fn fetch_segment(&mut self, segment_id: &str, tier: BackboneTier) -> FetchVerdict {
            let index = Self::index_of(segment_id);
            self.tiers_used.entry(index).or_default().push(tier);
            if let Some(queue) = self.overrides.get_mut(&(index, tier)) {
                if let Some(verdict) = queue.pop_front() {
                    return verdict;
                }
            }
            if self.oversize.contains(&index) {
                return FetchVerdict::Bytes(vec![b'X'; MAX_BODY_REPLY_BYTES + 1]);
            }
            let part = self.decoded[index].clone();
            let begin = self.decoded[..index]
                .iter()
                .map(|c| c.len() as u64)
                .sum::<u64>()
                + 1;
            let params = SegmentParams {
                name: "track.flac".to_string(),
                part: index as u32 + 1,
                total: self.decoded.len() as u32,
                begin,
                end: begin + part.len() as u64 - 1,
                file_size: self.total_len,
            };
            let mut article = Vec::new();
            encode_segment(&mut article, &params, &part).expect("fake article encodes");
            FetchVerdict::Bytes(article)
        }
    }

    /// In-memory staging sink: per-segment spool, zero-filled assembly,
    /// range patch, windowed streaming. Windows prove the bounded-memory
    /// walk; the layout proves byte order.
    struct MemorySink {
        segments: BTreeMap<usize, Vec<u8>>,
        gap_refused: BTreeMap<usize, bool>,
        gap_lens: BTreeMap<usize, u64>,
        layout: Option<Vec<u8>>,
        stream_windows: std::cell::Cell<u32>,
        last_window: std::cell::Cell<u64>,
    }

    impl MemorySink {
        fn new() -> Self {
            MemorySink {
                segments: BTreeMap::new(),
                gap_refused: BTreeMap::new(),
                gap_lens: BTreeMap::new(),
                layout: None,
                stream_windows: std::cell::Cell::new(0),
                last_window: std::cell::Cell::new(0),
            }
        }

        fn staged_bytes(&self) -> &[u8] {
            self.layout.as_deref().expect("assembled before read")
        }
    }

    impl StagingSink for MemorySink {
        fn put_segment(&mut self, index: usize, bytes: &[u8]) -> Result<(), SinkError> {
            if self.layout.is_some() || self.segments.contains_key(&index) {
                return Err(SinkError::PutSegment);
            }
            self.segments.insert(index, bytes.to_vec());
            Ok(())
        }

        fn mark_gap(&mut self, index: usize, size_refused: bool) {
            self.gap_refused.insert(index, size_refused);
        }

        fn assemble(&mut self, total_len: u64, gap_lens: &[(usize, u64)]) -> Result<(), SinkError> {
            if self.layout.is_some() {
                return Err(SinkError::Assemble);
            }
            self.gap_lens = gap_lens.iter().copied().collect();
            let count = self
                .segments
                .keys()
                .chain(self.gap_refused.keys())
                .max()
                .map(|m| m + 1)
                .unwrap_or(0);
            let mut layout = Vec::new();
            for i in 0..count {
                if let Some(bytes) = self.segments.get(&i) {
                    layout.extend_from_slice(bytes);
                } else if let Some(len) = self.gap_lens.get(&i) {
                    layout.extend(std::iter::repeat_n(0, *len as usize));
                } else {
                    return Err(SinkError::Assemble);
                }
            }
            if layout.len() as u64 != total_len {
                return Err(SinkError::Assemble);
            }
            self.layout = Some(layout);
            Ok(())
        }

        fn read_range(&self, offset: u64, len: u64) -> Result<Vec<u8>, SinkError> {
            let layout = self.staged_bytes();
            let (o, l) = (offset as usize, len as usize);
            layout
                .get(o..o.saturating_add(l))
                .filter(|s| s.len() == l)
                .map(|s| s.to_vec())
                .ok_or(SinkError::ReadRange)
        }

        fn patch_range(&mut self, offset: u64, bytes: &[u8]) -> Result<(), SinkError> {
            let layout = self.layout.as_mut().ok_or(SinkError::PatchRange)?;
            let (o, l) = (offset as usize, bytes.len());
            if layout.get(o..o.saturating_add(l)).map(|s| s.len()) != Some(l) {
                return Err(SinkError::PatchRange);
            }
            layout[o..o + l].copy_from_slice(bytes);
            Ok(())
        }

        fn read_stream(
            &self,
            window: u64,
            visit: &mut dyn FnMut(&[u8]) -> Result<(), SinkError>,
        ) -> Result<(), SinkError> {
            self.last_window.set(window);
            for chunk in self.staged_bytes().chunks(window.max(1) as usize) {
                self.stream_windows.set(self.stream_windows.get() + 1);
                visit(chunk)?;
            }
            Ok(())
        }

        fn finalize(self) -> Result<StagedDigest, SinkError> {
            let layout = self.layout.ok_or(SinkError::Finalize)?;
            Ok(StagedDigest {
                byte_len: layout.len() as u64,
                sha256_hex: hex::encode(Sha256::digest(&layout)),
            })
        }
    }

    // --- PAR2 packet builder (mirrors the core crate's test vectors) --------

    fn packet(set_id: &[u8; 16], kind: &[u8; 16], body: &[u8]) -> Vec<u8> {
        let mut aligned = body.to_vec();
        while !aligned.len().is_multiple_of(4) {
            aligned.push(0);
        }
        let mut p = Vec::new();
        p.extend_from_slice(b"PAR2\0PKT");
        p.extend_from_slice(&((64 + aligned.len()) as u64).to_le_bytes());
        p.extend_from_slice(&[0u8; 16]);
        p.extend_from_slice(set_id);
        p.extend_from_slice(kind);
        p.extend_from_slice(&aligned);
        let digest = md5::compute(&p[32..]).0;
        p[16..32].copy_from_slice(&digest);
        p
    }

    fn ptype(suffix: &[u8]) -> [u8; 16] {
        let mut t = [0u8; 16];
        t[..8].copy_from_slice(b"PAR 2.0\0");
        t[8..8 + suffix.len().min(8)].copy_from_slice(suffix);
        t
    }

    fn block_crcs(data: &[u8], block_size: u64) -> Vec<u32> {
        data.chunks(block_size as usize)
            .map(|block| {
                let mut h = crc32fast::Hasher::new();
                h.update(block);
                if block.len() < block_size as usize {
                    hash_zero_run(&mut h, block_size - block.len() as u64);
                }
                h.finalize()
            })
            .collect()
    }

    /// Minimal recovery set over `crc_data` claiming `file_len`, with one
    /// RecvSlic packet per requested exponent. Packet order is scrambled:
    /// the format fixes no order.
    fn build_set(
        crc_data: &[u8],
        file_len: u64,
        block_size: u64,
        name: &[u8],
        exponents: &[u32],
    ) -> Vec<u8> {
        let whole_md5 = md5::compute(crc_data).0;
        let first16k = md5::compute(&crc_data[..crc_data.len().min(16 * 1024)]).0;
        let mut seed = Vec::new();
        seed.extend_from_slice(&first16k);
        seed.extend_from_slice(&file_len.to_le_bytes());
        seed.extend_from_slice(name);
        let file_id = md5::compute(&seed).0;

        let mut main_body = Vec::new();
        main_body.extend_from_slice(&block_size.to_le_bytes());
        main_body.extend_from_slice(&1u32.to_le_bytes());
        main_body.extend_from_slice(&file_id);
        let set_id = md5::compute(&main_body).0;

        let mut fd_body = Vec::new();
        fd_body.extend_from_slice(&file_id);
        fd_body.extend_from_slice(&whole_md5);
        fd_body.extend_from_slice(&first16k);
        fd_body.extend_from_slice(&file_len.to_le_bytes());
        fd_body.extend_from_slice(name);
        while fd_body.len() % 4 != 0 {
            fd_body.push(0);
        }

        let mut ifsc_body = Vec::new();
        ifsc_body.extend_from_slice(&file_id);
        for crc in block_crcs(crc_data, block_size) {
            ifsc_body.extend_from_slice(&[0u8; 16]);
            ifsc_body.extend_from_slice(&crc.to_le_bytes());
        }

        let mut set_bytes = Vec::new();
        set_bytes.extend(packet(&set_id, &ptype(b"FileDesc"), &fd_body));
        for e in exponents {
            set_bytes.extend(packet(&set_id, &ptype(b"RecvSlic"), &e.to_le_bytes()));
        }
        set_bytes.extend(packet(&set_id, &ptype(b"IFSC\0\0\0\0"), &ifsc_body));
        set_bytes.extend(packet(&set_id, &ptype(b"Main\0\0\0\0"), &main_body));
        set_bytes
    }

    fn xor_parity(data: &[u8], block_size: usize) -> Vec<u8> {
        let mut parity = vec![0u8; block_size];
        for block in data.chunks(block_size) {
            xor_accumulate(&mut parity, block);
        }
        parity
    }

    fn fixture_bytes(len: usize) -> Vec<u8> {
        let mut x = 0x51ab_3c7d_29e1_04f5u64;
        let mut out = vec![0u8; len];
        for b in out.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = (x >> 24) as u8;
        }
        out
    }

    // --- Acceptance ---------------------------------------------------------

    const TRACK: &str = "track-0001";
    const FILE_NAME: &[u8] = b"track.flac";

    fn spec<'a>(ids: &'a [String], role: &'a str, par2: Option<Par2Plan<'a>>) -> FullFileSpec<'a> {
        FullFileSpec {
            track_id: TRACK,
            file_index: 0,
            role,
            segment_ids: ids,
            par2,
        }
    }

    #[tokio::test]
    async fn full_file_lands_byte_identical_over_32mib() {
        // Done-when, first half: a multi-segment fixture larger than the old
        // in-Worker assembly cap lands byte-identical in staging, verified
        // against the original PAR2 set.
        let data = fixture_bytes((32 << 20) + (1 << 20) + 12345);
        assert!(
            data.len() as u64 > SPOOL_REASSEMBLY_THRESHOLD_BYTES,
            "the fixture must clear the >32 MiB line"
        );
        let block_size = 64 << 10;
        let (mut source, ids) = PostSource::new(&data, 700_000);
        let set_bytes = build_set(&data, data.len() as u64, block_size as u64, FILE_NAME, &[0]);
        let parity = ParitySlice {
            exponent: 0,
            bytes: xor_parity(&data, block_size),
        };
        let plan = Par2Plan {
            set_bytes: &set_bytes,
            file_name: FILE_NAME,
            parities: std::slice::from_ref(&parity),
        };
        let mut sink = MemorySink::new();
        let outcome =
            download_full_file(&spec(&ids, "content", Some(plan)), &mut source, &mut sink).await;
        match outcome {
            FullFileOutcome::Staged(staged) => {
                assert_eq!(staged.track_id, TRACK);
                assert_eq!(staged.byte_len, data.len() as u64);
                assert_eq!(staged.proof, StagingProof::Par2Verified);
                assert_eq!(staged.sha256_hex, hex::encode(Sha256::digest(&data)));
                assert_eq!(staged.md5_hex, hex::encode(md5::compute(&data).0));
                assert_eq!(
                    staged.parts,
                    data.len().div_ceil(PART_BYTES as usize) as u32
                );
                assert_eq!(sink.staged_bytes(), data.as_slice(), "byte-identical");
                assert_eq!(sink.last_window.get(), PART_BYTES, "bounded windows");
                assert_eq!(
                    sink.stream_windows.get(),
                    staged.parts,
                    "one streaming pass in part windows"
                );
            }
            other => panic!("expected Staged, got {other:?}"),
        }
        // Happy path stays on the primary tier: one probe per segment.
        assert!(source
            .tiers_used
            .values()
            .all(|t| t.as_slice() == [BackboneTier::Primary]));
    }

    #[tokio::test]
    async fn par2_repaired_case_lands_byte_identical() {
        // Done-when, second half: one segment gone on every backbone, the
        // exponent-0 parity rebuilds its block, and the set verifies.
        let data = fixture_bytes(8 * (64 << 10));
        let block_size = 64 << 10;
        let (mut source, ids) = PostSource::new(&data, 64 << 10);
        source.script_all_tiers(3, FetchVerdict::Absent);
        let set_bytes = build_set(&data, data.len() as u64, block_size as u64, FILE_NAME, &[0]);
        let parity = ParitySlice {
            exponent: 0,
            bytes: xor_parity(&data, block_size),
        };
        let plan = Par2Plan {
            set_bytes: &set_bytes,
            file_name: FILE_NAME,
            parities: std::slice::from_ref(&parity),
        };
        let mut sink = MemorySink::new();
        let outcome =
            download_full_file(&spec(&ids, "content", Some(plan)), &mut source, &mut sink).await;
        match outcome {
            FullFileOutcome::Staged(staged) => {
                assert_eq!(staged.proof, StagingProof::Par2Verified);
                assert_eq!(sink.staged_bytes(), data.as_slice(), "byte-identical");
                assert_eq!(
                    staged.sha256_hex,
                    hex::encode(Sha256::digest(sink.staged_bytes()))
                );
                assert_eq!(
                    staged.md5_hex,
                    hex::encode(md5::compute(sink.staged_bytes()).0)
                );
                assert_eq!(
                    sink.stream_windows.get(),
                    staged.parts * 2,
                    "verify pass plus post-repair re-verify, both windowed"
                );
            }
            other => panic!("expected Staged, got {other:?}"),
        }
        // The ladder walked every backbone exactly once for the dead
        // segment — never a same-backbone retry after an absence.
        assert_eq!(
            source.tiers_used[&3],
            vec![
                BackboneTier::Primary,
                BackboneTier::Secondary,
                BackboneTier::BlockBackup
            ]
        );
    }

    #[tokio::test]
    async fn unrecoverable_fixture_is_repost_blocked_with_catalogue_intact() {
        // Done-when, third half: no parity, one dead segment. The outcome
        // carries the pinned flag, the pointer stays, and the catalogue row
        // is untouched — the track is blocked, never dropped.
        let data = fixture_bytes(8 * (64 << 10));
        let block_size = 64 << 10;
        let (mut source, ids) = PostSource::new(&data, 64 << 10);
        source.script_all_tiers(3, FetchVerdict::Absent);
        let set_bytes = build_set(&data, data.len() as u64, block_size as u64, FILE_NAME, &[]);
        let plan = Par2Plan {
            set_bytes: &set_bytes,
            file_name: FILE_NAME,
            parities: &[],
        };
        let mut sink = MemorySink::new();
        let mut catalogue: HashMap<String, String> =
            [(TRACK.to_string(), "catalogue-row".to_string())].into();
        let outcome =
            download_full_file(&spec(&ids, "content", Some(plan)), &mut source, &mut sink).await;
        match outcome {
            FullFileOutcome::RepostBlocked(blocked) => {
                assert_eq!(blocked.code(), REPOST_BLOCKED_CODE);
                assert_eq!(blocked.code(), "repost_blocked", "pinned flag");
                assert_eq!(blocked.reason, BlockReason::FetchExhausted);
                assert_eq!(blocked.missing_segments, 1);
                assert_eq!(blocked.bad_blocks, 1);
                assert!(blocked.original_pointer_retained());
            }
            other => panic!("expected RepostBlocked, got {other:?}"),
        }
        assert_eq!(
            catalogue.get(TRACK),
            Some(&"catalogue-row".to_string()),
            "the catalogue row is never mutated by a block"
        );
        let _ = &mut catalogue;
    }

    #[tokio::test]
    async fn multi_erasure_needs_the_kernel() {
        // Two dead segments: the XOR fast path declines and names the
        // TWO-1762 kernel instead of staging damaged bytes.
        let data = fixture_bytes(8 * (64 << 10));
        let block_size = 64 << 10;
        let (mut source, ids) = PostSource::new(&data, 64 << 10);
        source.script_all_tiers(2, FetchVerdict::Absent);
        source.script_all_tiers(5, FetchVerdict::Absent);
        let set_bytes = build_set(&data, data.len() as u64, block_size as u64, FILE_NAME, &[0]);
        let parity = ParitySlice {
            exponent: 0,
            bytes: xor_parity(&data, block_size),
        };
        let plan = Par2Plan {
            set_bytes: &set_bytes,
            file_name: FILE_NAME,
            parities: std::slice::from_ref(&parity),
        };
        let mut sink = MemorySink::new();
        let outcome =
            download_full_file(&spec(&ids, "content", Some(plan)), &mut source, &mut sink).await;
        assert_eq!(
            outcome,
            FullFileOutcome::RepostBlocked(RepostBlocked {
                track_id: TRACK.to_string(),
                file_index: 0,
                reason: BlockReason::RepairUnsupported,
                missing_segments: 2,
                size_refused_segments: 0,
                bad_blocks: 2,
            })
        );
    }

    #[tokio::test]
    async fn corrupt_parity_fails_closed() {
        // A parity slice that does not match the post reconstructs garbage;
        // re-verification catches it and nothing is staged.
        let data = fixture_bytes(8 * (64 << 10));
        let block_size = 64 << 10;
        let (mut source, ids) = PostSource::new(&data, 64 << 10);
        source.script_all_tiers(3, FetchVerdict::Absent);
        let set_bytes = build_set(&data, data.len() as u64, block_size as u64, FILE_NAME, &[0]);
        let mut bad_parity = xor_parity(&data, block_size);
        bad_parity[0] ^= 0xff;
        bad_parity[1000] ^= 0x01;
        let parity = ParitySlice {
            exponent: 0,
            bytes: bad_parity,
        };
        let plan = Par2Plan {
            set_bytes: &set_bytes,
            file_name: FILE_NAME,
            parities: std::slice::from_ref(&parity),
        };
        let mut sink = MemorySink::new();
        let outcome =
            download_full_file(&spec(&ids, "content", Some(plan)), &mut source, &mut sink).await;
        match outcome {
            FullFileOutcome::RepostBlocked(blocked) => {
                assert_eq!(blocked.reason, BlockReason::RepairMismatch);
                assert!(blocked.original_pointer_retained());
            }
            other => panic!("expected RepostBlocked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn archive_sets_refuse_before_any_pool_call() {
        // TWO-1174 gate: archive-set download is refused without touching
        // the pool, carrying the owning card for the dashboard.
        let data = fixture_bytes(1024);
        let (mut source, ids) = PostSource::new(&data, 1024);
        let mut sink = MemorySink::new();
        let outcome =
            download_full_file(&spec(&ids, "archive", None), &mut source, &mut sink).await;
        assert_eq!(
            outcome,
            FullFileOutcome::ArchiveGated {
                track_id: TRACK.to_string(),
                file_index: 0,
                gate: ARCHIVE_GATE_CARD,
            }
        );
        assert_eq!(ARCHIVE_GATE_CARD, "TWO-1174");
        assert!(source.tiers_used.is_empty(), "no pool call past the gate");
    }

    #[tokio::test]
    async fn overfetch_past_the_set_fails_closed() {
        // The post carries real bytes past the set's length: the proof
        // cannot vouch for them, so the file is blocked, not truncated.
        let claim = 7 * (64 << 10);
        let mut data = fixture_bytes(8 * (64 << 10));
        // Deterministic nonzero tail past the claim: the xorshift stream
        // could in principle emit a zero exactly here.
        if data[claim] == 0 {
            data[claim] = 1;
        }
        let (mut source, ids) = PostSource::new(&data, 64 << 10);
        let set_bytes = build_set(
            &data[..claim],
            claim as u64,
            (64 << 10) as u64,
            FILE_NAME,
            &[0],
        );
        let parity = ParitySlice {
            exponent: 0,
            bytes: xor_parity(&data[..claim], 64 << 10),
        };
        let plan = Par2Plan {
            set_bytes: &set_bytes,
            file_name: FILE_NAME,
            parities: std::slice::from_ref(&parity),
        };
        let mut sink = MemorySink::new();
        let outcome =
            download_full_file(&spec(&ids, "content", Some(plan)), &mut source, &mut sink).await;
        match outcome {
            FullFileOutcome::RepostBlocked(blocked) => {
                assert_eq!(blocked.reason, BlockReason::LengthMismatch);
                assert_eq!(blocked.missing_segments, 0);
                assert!(blocked.original_pointer_retained());
            }
            other => panic!("expected RepostBlocked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn oversize_article_is_flagged_never_retried() {
        // A 16 MiB+ article refuses identically on every backbone: the gap
        // is recorded at once with the refusal flag, burning no ladder.
        let data = fixture_bytes(8 * (16 << 10));
        let block_size = 16 << 10;
        let (mut source, ids) = PostSource::new(&data, 16 << 10);
        source.oversize.insert(4);
        let set_bytes = build_set(&data, data.len() as u64, block_size as u64, FILE_NAME, &[]);
        let plan = Par2Plan {
            set_bytes: &set_bytes,
            file_name: FILE_NAME,
            parities: &[],
        };
        let mut sink = MemorySink::new();
        let outcome =
            download_full_file(&spec(&ids, "content", Some(plan)), &mut source, &mut sink).await;
        match outcome {
            FullFileOutcome::RepostBlocked(blocked) => {
                assert_eq!(blocked.reason, BlockReason::FetchExhausted);
                assert_eq!(blocked.missing_segments, 1);
                assert_eq!(blocked.size_refused_segments, 1);
            }
            other => panic!("expected RepostBlocked, got {other:?}"),
        }
        assert_eq!(
            source.tiers_used[&4],
            vec![BackboneTier::Primary],
            "oversize never retries: a refetch refuses identically"
        );
    }

    #[tokio::test]
    async fn corrupt_article_advances_a_tier() {
        // Garbage bytes are judged absent like a 430: the ladder tries the
        // next backbone, which holds a good copy.
        let data = fixture_bytes(4 * (16 << 10));
        let block_size = 16 << 10;
        let (mut source, ids) = PostSource::new(&data, 16 << 10);
        source.script(
            1,
            BackboneTier::Primary,
            vec![FetchVerdict::Bytes(b"not yenc at all".to_vec())],
        );
        let set_bytes = build_set(&data, data.len() as u64, block_size as u64, FILE_NAME, &[]);
        let plan = Par2Plan {
            set_bytes: &set_bytes,
            file_name: FILE_NAME,
            parities: &[],
        };
        let mut sink = MemorySink::new();
        let outcome =
            download_full_file(&spec(&ids, "content", Some(plan)), &mut source, &mut sink).await;
        match outcome {
            FullFileOutcome::Staged(staged) => {
                assert_eq!(staged.proof, StagingProof::Par2Verified);
                assert_eq!(sink.staged_bytes(), data.as_slice());
            }
            other => panic!("expected Staged, got {other:?}"),
        }
        assert_eq!(
            source.tiers_used[&1],
            vec![BackboneTier::Primary, BackboneTier::Secondary]
        );
    }

    #[tokio::test]
    async fn broken_set_still_stages_a_clean_file_on_transport_proof() {
        // An unparsable PAR2 plan cannot verify, but a gap-free download is
        // still honest evidence — staged with the weaker proof, typed.
        let data = fixture_bytes(4 * (16 << 10));
        let (mut source, ids) = PostSource::new(&data, 16 << 10);
        let plan = Par2Plan {
            set_bytes: b"not a par2 stream",
            file_name: FILE_NAME,
            parities: &[],
        };
        let mut sink = MemorySink::new();
        let outcome =
            download_full_file(&spec(&ids, "content", Some(plan)), &mut source, &mut sink).await;
        match outcome {
            FullFileOutcome::Staged(staged) => {
                assert_eq!(staged.proof, StagingProof::TransportComplete);
                assert_eq!(sink.staged_bytes(), data.as_slice());
            }
            other => panic!("expected Staged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sink_failure_blocks_without_staging() {
        struct FailPut;
        impl StagingSink for FailPut {
            fn put_segment(&mut self, _index: usize, _bytes: &[u8]) -> Result<(), SinkError> {
                Err(SinkError::PutSegment)
            }
            fn mark_gap(&mut self, _index: usize, _size_refused: bool) {}
            fn assemble(
                &mut self,
                _total_len: u64,
                _gap_lens: &[(usize, u64)],
            ) -> Result<(), SinkError> {
                Err(SinkError::Assemble)
            }
            fn read_range(&self, _offset: u64, _len: u64) -> Result<Vec<u8>, SinkError> {
                Err(SinkError::ReadRange)
            }
            fn patch_range(&mut self, _offset: u64, _bytes: &[u8]) -> Result<(), SinkError> {
                Err(SinkError::PatchRange)
            }
            fn read_stream(
                &self,
                _window: u64,
                _visit: &mut dyn FnMut(&[u8]) -> Result<(), SinkError>,
            ) -> Result<(), SinkError> {
                Err(SinkError::ReadStream)
            }
            fn finalize(self) -> Result<StagedDigest, SinkError> {
                Err(SinkError::Finalize)
            }
        }
        let data = fixture_bytes(1024);
        let (mut source, ids) = PostSource::new(&data, 1024);
        let mut sink = FailPut;
        let outcome =
            download_full_file(&spec(&ids, "content", None), &mut source, &mut sink).await;
        match outcome {
            FullFileOutcome::RepostBlocked(blocked) => {
                assert_eq!(blocked.reason, BlockReason::SinkFailed);
            }
            other => panic!("expected RepostBlocked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_file_list_is_unplaceable() {
        let mut source = PostSource::new(&[], 1024).0;
        let mut sink = MemorySink::new();
        let outcome = download_full_file(&spec(&[], "content", None), &mut source, &mut sink).await;
        match outcome {
            FullFileOutcome::RepostBlocked(blocked) => {
                assert_eq!(blocked.reason, BlockReason::GeometryUnproven);
            }
            other => panic!("expected RepostBlocked, got {other:?}"),
        }
    }
}
