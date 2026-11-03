//! Two-spool verification (stage B2.5) + plaintext-sha256 dedupe.
//!
//! After the first spool verifies, the pipeline keeps a second copy on a
//! *different backbone*: a verify account on that backbone `STAT`s every
//! article (in batches of [`STAT_BATCH_SIZE`]) after a propagation delay,
//! and articles answering `430`/`423` are re-posted with the **same
//! message-ID** through a posting account on that backbone until verified
//! or the re-post budget is exhausted. Transport errors never burn re-post
//! budget; a `435`/`441` duplicate after `STAT` counts as present; rechecks
//! back off. Crash recovery `STAT`s and re-posts only the missing articles.
//!
//! Storage shape (Engineering Lead build-time decision): the second copy
//! is the same message-IDs posted via a posting account on the second
//! backbone — no second NZB, no second ID set. The posted-file record
//! ([`PostedFileRecord`], migration 0031) stores per-backbone
//! presence/verification state per article set; accounts sharing a backbone
//! count as one copy. Repair reads from either spool first, then PAR2
//! (itself present on both spools); only if same-ID re-post fails does
//! repair mint new IDs and update the DB row.
//!
//! Dedupe: if the tagged plaintext sha256 already has a verified two-spool
//! post, reuse it instead of posting again; post fresh only if that post
//! fails verification/retention.
//!
//! Budget exhaustion leaves the track on its original NZB with a typed
//! state ([`TwoSpoolOutcome::RepostExhausted`], code
//! [`REPOST_EXHAUSTED_CODE`]).
//!
//! This module is pure (no Worker `Env`, no pool bindings): the live
//! `STAT`/`POST` calls ride the pool Durable Object like the probe/body
//! paths once that route exists, and until then the driver below runs
//! against injected callables so every rule above is unit-tested without
//! a provider. Nothing here logs or stores message-IDs, subjects, file
//! names or segment lists: progress records carry per-article verdicts
//! (counts and indexes only), and the DB row carries hashes and counts,
//! never IDs.
//!
//! Role/backbone vocabulary: accounts are referenced by pool-config
//! role/backbone label only (hostnames/usernames are vault secrets and
//! never appear here). [`validate_backbone_label`] pins the label shape.

use serde::{Deserialize, Serialize};

/// Articles per second-spool `STAT` batch: small enough that one batch is
/// a short sequential round-trip run, large enough that a cap-sized NZB
/// finishes in a bounded number of batches.
pub const STAT_BATCH_SIZE: usize = 100;

/// Propagation delay before the first second-spool `STAT`: the post needs
/// time to reach the second backbone. A recheck that still finds a missing
/// article waits out [`recheck_delay_ms`] instead.
pub const PROPAGATION_DELAY_MS: u64 = 60_000;

/// Base recheck backoff: `30 s * 2^recheck`, capped at one hour. Rechecks
/// back off so a slow backbone is polled gently, never hammered.
pub const RECHECK_BASE_DELAY_MS: u64 = 30_000;
/// Longest single recheck wait.
pub const RECHECK_MAX_DELAY_MS: u64 = 3_600_000;

/// Most same-ID re-post attempts spent on one article before it counts as
/// exhausted. Transport errors never decrement this: only answered
/// `POST` attempts increment [`TwoSpoolProgress::reposts_spent`].
pub const MAX_REPOST_ATTEMPTS_PER_ARTICLE: u32 = 3;

/// Most rechecks (full missing-set `STAT` passes) before the round gives
/// up waiting for propagation and fails typed instead of polling forever.
pub const MAX_RECHECKS: u32 = 5;

/// Machine-readable code for the budget-exhausted terminal state: the
/// track stays on its original NZB ([`TwoSpoolOutcome::RepostExhausted`]
/// carries `keep_original_nzb: true`).
pub const REPOST_EXHAUSTED_CODE: &str = "repost_budget_exhausted";

/// A backbone label names one Usenet backbone in pool config (for example
/// the two pool lanes the ingest pool is configured with). Shape: 1..=64
/// lowercase alphanumerics and dashes — never a hostname.
pub fn validate_backbone_label(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Tagged plaintext sha256: lowercase hex of the tagged plaintext bytes
/// (pre-encryption). The dedupe key.
pub fn validate_plaintext_sha256(raw: &str) -> bool {
    raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit())
}

/// What one second-spool `STAT` found. `Missing` covers both `430` (no
/// such article) and `423` (no articles in range): either way the article
/// is not on this backbone and needs a same-ID re-post. Anything that is
/// not an answered NNTP verdict (I/O, timeout, unexpected code) is
/// `Transport`: retry later without burning re-post budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatVerdict {
    Present,
    Missing,
    Transport,
}

/// Classify a `STAT` reply code. Only answered verdicts consume budget;
/// everything else is transport noise the recheck absorbs.
pub fn classify_stat_code(code: u16) -> StatVerdict {
    match code {
        223 => StatVerdict::Present,
        430 | 423 => StatVerdict::Missing,
        _ => StatVerdict::Transport,
    }
}

/// What one same-ID re-post attempt found. `DuplicatePresent` (`435`/`441`
/// after `STAT`) counts as present: the article reached the backbone
/// between the `STAT` and the `POST` (or a redelivered `POST` replayed),
/// so no further attempt is needed. `Transport` (I/O, timeout, refusal
/// codes outside the duplicate pair) retries without burning budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostVerdict {
    Posted,
    DuplicatePresent,
    Transport,
    /// The backbone permanently refused this post (auth, posting not
    /// allowed): retrying the same bytes cannot help.
    Refused,
}

/// Classify a same-ID `POST` reply code. `had_prior_missing_stat` is
/// accepted for call-site symmetry with the "duplicate after STAT" rule
/// but does not change the mapping: a `435`/`441` duplicate counts as
/// present whether or not the preceding `STAT` said missing (a redelivery
/// replays the same bytes, so the duplicate is the expected answer).
///
/// Only an explicit `440` (posting prohibited) is terminal: auth and
/// permission codes (`480`/`481`/`502`) ride the pool's re-auth and
/// redelivery path, so they retry later without burning budget or
/// exhausting the article — exactly like transport noise. The live client
/// ([`nntune_core::nntp::NntpClient::post_article`]) confirms a `435`/`441`
/// with `STAT` before reporting success; see [`classify_post_error`] for
/// the client-error side of the same mapping.
pub fn classify_post_code(code: u16, _had_prior_missing_stat: bool) -> PostVerdict {
    match code {
        240 => PostVerdict::Posted,
        435 | 441 => PostVerdict::DuplicatePresent,
        440 => PostVerdict::Refused,
        _ => PostVerdict::Transport,
    }
}

/// Maps a same-ID re-post client failure onto a verdict. Only answered
/// server verdicts consume budget or settle articles; everything else —
/// transport I/O (dropped connections, timeouts), unconfirmed duplicates,
/// stale-credential and permission states — retries on a fresh delivery
/// without burning this round's budget. `440` prohibited is the one
/// terminal mapping: retrying the same bytes on the same account cannot
/// succeed, so the article exhausts immediately (see
/// [`TwoSpoolOutcome::RepostExhausted`]).
pub fn classify_post_error(err: &nntune_core::error::Error) -> PostVerdict {
    use nntune_core::error::Error;
    match err {
        Error::PostProhibited(_) => PostVerdict::Refused,
        _ => PostVerdict::Transport,
    }
}

/// Maps a second-spool `STAT` client failure onto a verdict: only a
/// confirmed absence (`430`, or a bare `423`) counts as missing; every
/// other failure — transport I/O, stale credentials, unexpected codes —
/// is a recheck, never a re-post trigger.
pub fn classify_stat_error(err: &nntune_core::error::Error) -> StatVerdict {
    use nntune_core::error::Error;
    match err {
        Error::ArticleNotFound(_) => StatVerdict::Missing,
        Error::NntpReply { code: 423, .. } => StatVerdict::Missing,
        _ => StatVerdict::Transport,
    }
}

/// Backoff before recheck `recheck` (0-based): `30 s * 2^recheck`,
/// saturating at one hour. Pure so the schedule is unit-pinned.
pub fn recheck_delay_ms(recheck: u32) -> u64 {
    let shift = recheck.min(8);
    RECHECK_BASE_DELAY_MS
        .saturating_mul(1 << shift)
        .min(RECHECK_MAX_DELAY_MS)
}

/// One article's second-spool state. Indexes (not IDs) identify articles:
/// the NZB order is deterministic across deliveries, so index `i` always
/// names the same article without storing its message-ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArticleSpool {
    /// Not yet `STAT`ed on the second backbone this round.
    Unchecked,
    /// `STAT` answered missing; awaiting a same-ID re-post. `u32` counts
    /// same-ID attempts already spent (transport errors never increment).
    Missing { reposts_spent: u32 },
    /// Re-posted (or duplicate-present) but not yet re-`STAT`ed to
    /// verified. Carries the spent count for the budget ledger.
    RepostedUnverified { reposts_spent: u32 },
    /// Verified present on both backbones.
    Verified,
    /// Budget exhausted for this article; the round will conclude
    /// [`TwoSpoolOutcome::RepostExhausted`] keeping the original NZB.
    Exhausted,
}

impl ArticleSpool {
    /// True once no further `STAT`/`POST` is needed for this article.
    pub fn is_settled(&self) -> bool {
        matches!(self, ArticleSpool::Verified | ArticleSpool::Exhausted)
    }

    /// True while this article still needs a `STAT` (never checked, or
    /// re-posted but not yet re-verified).
    pub fn needs_stat(&self) -> bool {
        matches!(
            self,
            ArticleSpool::Unchecked | ArticleSpool::RepostedUnverified { .. }
        )
    }

    /// True while this article still needs a same-ID re-post.
    pub fn needs_repost(&self) -> bool {
        matches!(self, ArticleSpool::Missing { .. })
    }
}

/// Per-backbone copy state for one posted file's article set. Accounts
/// sharing a backbone count as one copy: the backbone label (not the
/// account) keys the copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpoolCopyState {
    Unknown,
    Present,
    Missing,
    Reposted,
    Verified,
}

/// The posted-file record: one row per tagged plaintext sha256 (migration
/// 0031). Same message-IDs on both backbones — no second NZB, no second
/// ID set — so the row carries hashes and counts, never message-IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostedFileRecord {
    /// Tagged plaintext sha256 (dedupe key, lowercase hex).
    pub plaintext_sha256: String,
    /// Backbone the first spool was posted through.
    pub backbone_post: String,
    /// Backbone the second copy was verified/re-posted through.
    pub backbone_verify: String,
    /// First-spool copy state.
    pub post_copy: SpoolCopyState,
    /// Second-spool copy state.
    pub verify_copy: SpoolCopyState,
    /// Articles in the set / verified on both backbones. Counts only:
    /// the article set itself lives in the NZB, not the row.
    pub article_count: u32,
    pub verified_count: u32,
    /// Content sha256 of the original NZB bytes (kept so an exhausted
    /// round can name the NZB the track stays on, without storing it).
    pub original_nzb_sha256: Option<String>,
    /// True once both copies verify; only then may dedupe reuse this row.
    pub two_spool_verified: bool,
    /// True once the re-post budget ran out keeping the original NZB.
    pub repost_exhausted: bool,
}

impl PostedFileRecord {
    /// A fresh first-spool post, second copy not yet checked.
    pub fn fresh_post(
        plaintext_sha256: &str,
        backbone_post: &str,
        backbone_verify: &str,
        article_count: u32,
        original_nzb_sha256: Option<&str>,
    ) -> Result<Self, &'static str> {
        if !validate_plaintext_sha256(plaintext_sha256) {
            return Err("plaintext sha256 must be 64 hex chars");
        }
        if !validate_backbone_label(backbone_post) || !validate_backbone_label(backbone_verify) {
            return Err("backbone labels must be 1..=64 [a-z0-9-]");
        }
        if backbone_post == backbone_verify {
            return Err("verify backbone must differ from the posting backbone");
        }
        if let Some(sha) = original_nzb_sha256 {
            if !validate_plaintext_sha256(sha) {
                return Err("original NZB sha256 must be 64 hex chars");
            }
        }
        Ok(Self {
            plaintext_sha256: plaintext_sha256.to_ascii_lowercase(),
            backbone_post: backbone_post.to_string(),
            backbone_verify: backbone_verify.to_string(),
            post_copy: SpoolCopyState::Present,
            verify_copy: SpoolCopyState::Unknown,
            article_count,
            verified_count: 0,
            original_nzb_sha256: original_nzb_sha256.map(str::to_string),
            two_spool_verified: false,
            repost_exhausted: false,
        })
    }

    /// Fold one successful same-ID re-post into the row: the second copy
    /// is present-but-unverified until the re-`STAT` confirms it. Never
    /// clears a terminal state (`Verified` stays verified).
    pub fn record_reposted(&mut self) {
        if !self.two_spool_verified {
            self.verify_copy = SpoolCopyState::Reposted;
        }
    }

    /// Fold one verified article into the counts; marks the row verified
    /// once every article verifies on both backbones.
    pub fn record_verified_article(&mut self) {
        self.verified_count = self.verified_count.saturating_add(1);
        if self.verified_count >= self.article_count && self.article_count > 0 {
            self.verify_copy = SpoolCopyState::Verified;
            self.two_spool_verified = true;
        }
    }
}

/// Where repair reads from, in order. Either spool first, then PAR2
/// (itself present on both spools); new IDs are minted only when the
/// same-ID re-post path has failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairSource {
    SpoolPost,
    SpoolVerify,
    Par2,
    /// Last resort: mint new message-IDs and update the DB row.
    MintNewIds,
}

/// Repair read order for one posted-file record: either spool first,
/// then PAR2, then mint-new-IDs only if the same-ID path failed
/// (exhausted or unverified with no budget left).
pub fn repair_read_order(record: &PostedFileRecord) -> Vec<RepairSource> {
    let spool_ok = |state: SpoolCopyState| {
        matches!(
            state,
            SpoolCopyState::Present | SpoolCopyState::Reposted | SpoolCopyState::Verified
        )
    };
    let mut order = Vec::with_capacity(4);
    if spool_ok(record.post_copy) {
        order.push(RepairSource::SpoolPost);
    }
    if spool_ok(record.verify_copy) {
        order.push(RepairSource::SpoolVerify);
    }
    order.push(RepairSource::Par2);
    if record.repost_exhausted
        || (!record.two_spool_verified && record.verify_copy == SpoolCopyState::Missing)
    {
        order.push(RepairSource::MintNewIds);
    }
    order
}

/// Dedupe decision for one tagged plaintext sha256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupeDecision {
    /// A verified two-spool post already exists: reuse it, post nothing.
    Reuse { posted_file_id: String },
    /// No usable post exists: post fresh. `reason` names why
    /// (`"no_verified_post"`, `"post_failed_verification"`,
    /// `"post_lost_retention"`) for the log line — never an ID.
    PostFresh { reason: &'static str },
}

/// Decide dedupe from a lookup of the sha256's existing post, if any.
/// `verified` must be true AND the post must still be retained: a post
/// that failed verification or fell out of retention posts fresh.
pub fn decide_dedupe(
    plaintext_sha256: &str,
    existing: Option<(&str, bool, bool)>,
) -> Result<DedupeDecision, &'static str> {
    if !validate_plaintext_sha256(plaintext_sha256) {
        return Err("plaintext sha256 must be 64 hex chars");
    }
    match existing {
        Some((id, verified, retained)) if verified && retained => Ok(DedupeDecision::Reuse {
            posted_file_id: id.to_string(),
        }),
        Some((_, verified, _)) if !verified => Ok(DedupeDecision::PostFresh {
            reason: "post_failed_verification",
        }),
        Some(_) => Ok(DedupeDecision::PostFresh {
            reason: "post_lost_retention",
        }),
        None => Ok(DedupeDecision::PostFresh {
            reason: "no_verified_post",
        }),
    }
}

/// Resume state for one two-spool verification round. Article slots are
/// indexed by NZB order (no IDs stored); a redelivery resumes at the
/// first unsettled slot, so crash recovery `STAT`s and re-posts only the
/// missing articles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TwoSpoolProgress {
    /// Backbones for this round: post lane vs verify lane (must differ).
    pub backbone_post: String,
    pub backbone_verify: String,
    /// Per-article second-spool state in NZB order.
    pub articles: Vec<ArticleSpool>,
    /// Completed recheck passes (drives [`recheck_delay_ms`]).
    pub rechecks: u32,
    /// Same-ID re-post attempts spent this round (answered attempts
    /// only; transport errors never increment).
    pub reposts_spent: u32,
}

impl TwoSpoolProgress {
    pub fn new(
        backbone_post: &str,
        backbone_verify: &str,
        article_count: usize,
    ) -> Result<Self, &'static str> {
        if !validate_backbone_label(backbone_post) || !validate_backbone_label(backbone_verify) {
            return Err("backbone labels must be 1..=64 [a-z0-9-]");
        }
        if backbone_post == backbone_verify {
            return Err("verify backbone must differ from the posting backbone");
        }
        Ok(Self {
            backbone_post: backbone_post.to_string(),
            backbone_verify: backbone_verify.to_string(),
            articles: vec![ArticleSpool::Unchecked; article_count],
            rechecks: 0,
            reposts_spent: 0,
        })
    }

    /// Indexes still needing a `STAT` (crash-recovery resume set).
    pub fn pending_stat(&self) -> Vec<usize> {
        self.articles
            .iter()
            .enumerate()
            .filter(|(_, a)| a.needs_stat())
            .map(|(i, _)| i)
            .collect()
    }

    /// Indexes still needing a same-ID re-post.
    pub fn pending_repost(&self) -> Vec<usize> {
        self.articles
            .iter()
            .enumerate()
            .filter(|(_, a)| a.needs_repost())
            .map(|(i, _)| i)
            .collect()
    }

    /// True once every article is settled (verified or exhausted).
    pub fn is_settled(&self) -> bool {
        self.articles.iter().all(ArticleSpool::is_settled)
    }

    /// Fold one answered `STAT` verdict into slot `index`. Transport
    /// verdicts leave the slot untouched (recheck, no budget burn).
    pub fn fold_stat(&mut self, index: usize, verdict: StatVerdict) {
        let Some(slot) = self.articles.get_mut(index) else {
            return;
        };
        match (*slot, verdict) {
            (ArticleSpool::Unchecked, StatVerdict::Present) => *slot = ArticleSpool::Verified,
            (ArticleSpool::Unchecked, StatVerdict::Missing) => {
                *slot = ArticleSpool::Missing { reposts_spent: 0 }
            }
            (ArticleSpool::RepostedUnverified { reposts_spent }, StatVerdict::Present) => {
                let _ = reposts_spent;
                *slot = ArticleSpool::Verified;
            }
            (ArticleSpool::RepostedUnverified { reposts_spent }, StatVerdict::Missing) => {
                if reposts_spent >= MAX_REPOST_ATTEMPTS_PER_ARTICLE {
                    *slot = ArticleSpool::Exhausted;
                } else {
                    *slot = ArticleSpool::Missing { reposts_spent };
                }
            }
            // A missing slot re-`STAT`ed present (propagation caught up)
            // verifies without spending a re-post.
            (ArticleSpool::Missing { .. }, StatVerdict::Present) => {
                *slot = ArticleSpool::Verified;
            }
            _ => {}
        }
    }

    /// Fold one answered same-ID re-post verdict into slot `index`.
    /// Only answered attempts burn budget; transport leaves the slot and
    /// the budget untouched.
    pub fn fold_repost(&mut self, index: usize, verdict: PostVerdict) {
        let Some(slot) = self.articles.get_mut(index) else {
            return;
        };
        match (*slot, verdict) {
            (ArticleSpool::Missing { reposts_spent }, PostVerdict::Posted)
            | (ArticleSpool::Missing { reposts_spent }, PostVerdict::DuplicatePresent) => {
                self.reposts_spent = self.reposts_spent.saturating_add(1);
                let spent = reposts_spent.saturating_add(1);
                *slot = ArticleSpool::RepostedUnverified {
                    reposts_spent: spent,
                };
            }
            (ArticleSpool::Missing { reposts_spent }, PostVerdict::Refused) => {
                let _ = reposts_spent;
                *slot = ArticleSpool::Exhausted;
            }
            _ => {}
        }
    }

    /// Split the pending `STAT` set into batches of [`STAT_BATCH_SIZE`]
    /// (index ranges, deterministic NZB order).
    pub fn stat_batches(&self) -> Vec<Vec<usize>> {
        self.pending_stat()
            .chunks(STAT_BATCH_SIZE)
            .map(<[usize]>::to_vec)
            .collect()
    }
}

/// What one two-spool round concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TwoSpoolOutcome {
    /// Every article verified on both backbones.
    Verified { article_count: u32 },
    /// The re-post budget ran out: the track stays on its original NZB
    /// (`keep_original_nzb` is always true; the consumer keeps the
    /// first-spool bytes and records this typed state instead of minting
    /// new IDs here — that is repair's last resort, with a DB row
    /// update, per [`repair_read_order`]).
    RepostExhausted {
        verified: u32,
        exhausted: u32,
        keep_original_nzb: bool,
    },
    /// More work remains (transport noise or rechecks): redeliver and
    /// resume from the unsettled slots only.
    Incomplete { rechecks: u32 },
}

/// Aggregate the per-article states once no more `STAT`/re-post calls fit
/// this delivery. Exhaustion is terminal and typed; anything unsettled but
/// unexhausted is a resume (`Incomplete`) — unless the round has already
/// waited out the full recheck schedule ([`MAX_RECHECKS`]): then whatever
/// never verified is abandoned with the same typed state, because both
/// paths leave the track on its original NZB. `exhausted` counts every
/// article that never verified (budget-exhausted or recheck-abandoned).
pub fn conclude(progress: &TwoSpoolProgress) -> TwoSpoolOutcome {
    let verified = progress
        .articles
        .iter()
        .filter(|a| **a == ArticleSpool::Verified)
        .count() as u32;
    let exhausted = progress
        .articles
        .iter()
        .filter(|a| **a == ArticleSpool::Exhausted)
        .count() as u32;
    if progress.is_settled() {
        if exhausted > 0 {
            return TwoSpoolOutcome::RepostExhausted {
                verified,
                exhausted,
                keep_original_nzb: true,
            };
        }
        return TwoSpoolOutcome::Verified {
            article_count: verified,
        };
    }
    if progress.rechecks >= MAX_RECHECKS {
        let unsettled = progress.articles.len() as u32 - verified - exhausted;
        return TwoSpoolOutcome::RepostExhausted {
            verified,
            exhausted: exhausted + unsettled,
            keep_original_nzb: true,
        };
    }
    TwoSpoolOutcome::Incomplete {
        rechecks: progress.rechecks,
    }
}

/// The re-posted bytes must be the same article under the same
/// message-ID: no second ID set exists. Both sides (the stored article
/// fetched from the first spool and the bytes handed to `POST`) hash
/// equal, and the posted ID equals the original ID byte-identically.
pub fn validate_same_id_repost(
    original_id: &str,
    repost_id: &str,
    original_bytes_sha256_hex: &str,
    repost_bytes_sha256_hex: &str,
) -> Result<(), &'static str> {
    if original_id != repost_id {
        return Err("re-post must reuse the original message-ID");
    }
    if original_bytes_sha256_hex != repost_bytes_sha256_hex {
        return Err("re-post bytes must match the original article bytes");
    }
    if !validate_plaintext_sha256(original_bytes_sha256_hex) {
        return Err("article bytes sha256 must be 64 hex chars");
    }
    Ok(())
}

/// Backbones for one round: the verify lane must differ from the posting
/// lane, or the "second copy" is the same spool counted twice. Accounts
/// sharing a backbone count as one copy, so this compares labels, not
/// accounts.
pub fn validate_backbone_pair(post: &str, verify: &str) -> Result<(), &'static str> {
    if !validate_backbone_label(post) || !validate_backbone_label(verify) {
        return Err("backbone labels must be 1..=64 [a-z0-9-]");
    }
    if post == verify {
        return Err("verify backbone must differ from the posting backbone");
    }
    Ok(())
}

// --- `posted_files` statements (migration 0031) ---------------------------
// Pure statement owners for the `posted_files` table, mirroring the
// `track_writers` pattern: this module pins the SQL text, the `$n`
// parameter orders, and the zero-row classification. Execution lands
// with the writers-wiring follow-up through the shared `LIBRARY_DB`
// client. The rows are content-global (keyed by plaintext sha256, like
// `cache_objects` is keyed by content hash): no user identity, no
// writer fence, no message-IDs — hashes and counts only.

/// Latest bindable article count: mirrors the migration `CHECK`
/// (`article_count > 0`).
pub const MAX_POSTED_ARTICLES: u32 = 1_000_000;

/// `$n` parameter order for [`upsert_posted_file_sql`].
pub const POSTED_FILE_UPSERT_PARAM_ORDER: [&str; 6] = [
    "plaintext_sha256",
    "backbone_post",
    "backbone_verify",
    "article_count",
    "original_nzb_sha256",
    "post_copy",
];

/// First-spool admission: one row per plaintext sha256, first writer
/// wins (`ON CONFLICT (plaintext_sha256) DO NOTHING` — a replay carries
/// the same deterministic key, so redeliveries converge). Verification
/// and exhaustion move the row through [`mark_two_spool_verified_sql`]
/// / [`mark_repost_exhausted_sql`], never a second insert.
pub fn upsert_posted_file_sql() -> &'static str {
    "INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count, original_nzb_sha256, post_copy) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (plaintext_sha256) DO NOTHING"
}

/// Dedupe lookup: the verified two-spool post for one sha256, if any.
/// Only `two_spool_verified` rows are reusable; anything else posts
/// fresh (see [`decide_dedupe`]).
pub fn select_verified_by_sha_sql() -> &'static str {
    "SELECT id, two_spool_verified FROM posted_files WHERE plaintext_sha256 = $1 AND two_spool_verified"
}

/// Mark one row verified on both spools with its final verified count.
/// Parameters: `verified_count` ($1), `plaintext_sha256` ($2).
pub fn mark_two_spool_verified_sql() -> &'static str {
    "UPDATE posted_files SET verify_copy = 'verified', verified_count = $1, two_spool_verified = TRUE WHERE plaintext_sha256 = $2"
}

/// Mark one row re-post-exhausted (typed state; the track keeps its
/// original NZB). Parameters: `verified_count` ($1),
/// `plaintext_sha256` ($2).
pub fn mark_repost_exhausted_sql() -> &'static str {
    "UPDATE posted_files SET verify_copy = 'missing', verified_count = $1, repost_exhausted = TRUE WHERE plaintext_sha256 = $2"
}

/// Check upsert values before they touch the database. Bounds mirror
/// migration 0031 (`CHECK`s on the sha shapes, the label lengths, and
/// `article_count > 0`); the backbone pair must differ.
#[allow(clippy::too_many_arguments)]
pub fn validate_posted_file_params(
    plaintext_sha256: &str,
    backbone_post: &str,
    backbone_verify: &str,
    article_count: u32,
    original_nzb_sha256: Option<&str>,
) -> Result<(), &'static str> {
    if !validate_plaintext_sha256(plaintext_sha256) {
        return Err("plaintext sha256 must be 64 hex chars");
    }
    validate_backbone_pair(backbone_post, backbone_verify)?;
    if article_count == 0 || article_count > MAX_POSTED_ARTICLES {
        return Err("article count must be 1..=1000000");
    }
    if let Some(sha) = original_nzb_sha256 {
        if !validate_plaintext_sha256(sha) {
            return Err("original NZB sha256 must be 64 hex chars");
        }
    }
    Ok(())
}

/// Outcome of one [`mark_two_spool_verified_sql`] /
/// [`mark_repost_exhausted_sql`] write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostedFileWriteOutcome {
    /// The row advanced to the new state.
    Advanced,
    /// Zero rows: the sha is unknown (no upsert landed) — post fresh
    /// instead of treating a missing row as verified.
    MissingRow,
}

/// Classify a marking write's affected-row count. Pure; the caller owns
/// the select. A divergent write (row exists but already terminal) also
/// reads as `Advanced` when the predicate matches — the states are
/// monotonic (`verified`/`exhausted` never clear), so a replay converges.
pub fn classify_posted_file_write(affected_rows: u64) -> PostedFileWriteOutcome {
    if affected_rows >= 1 {
        PostedFileWriteOutcome::Advanced
    } else {
        PostedFileWriteOutcome::MissingRow
    }
}

/// Article indexes (NZB order) for one delivery's `STAT` batch: a
/// contiguous window of [`STAT_BATCH_SIZE`] over the pending set.
/// Pure index arithmetic so the batching is unit-pinned without IDs.
pub fn stat_batch_window(pending: &[usize], batch: usize) -> &[usize] {
    let start = batch.saturating_mul(STAT_BATCH_SIZE);
    if start >= pending.len() {
        return &[];
    }
    let end = (start + STAT_BATCH_SIZE).min(pending.len());
    &pending[start..end]
}

/// Per-delivery accounting: how many answered re-posts one delivery may
/// spend before it must persist and redeliver (crash recovery resumes
/// from the persisted slots). Transport outcomes never decrement.
pub const MAX_REPOSTS_PER_DELIVERY: u32 = 200;

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const NZB_SHA: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn stat_batches_cover_every_article_in_hundreds() {
        let progress = TwoSpoolProgress::new("backbone-a", "backbone-b", 250).unwrap();
        let batches = progress.stat_batches();
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 100);
        assert_eq!(batches[1].len(), 100);
        assert_eq!(batches[2].len(), 50);
        let flat: Vec<usize> = batches.concat();
        assert_eq!(flat, (0..250).collect::<Vec<_>>());
    }

    #[test]
    fn stat_batch_window_slices_pending_in_hundreds() {
        let pending: Vec<usize> = (0..250).collect();
        assert_eq!(stat_batch_window(&pending, 0).len(), 100);
        assert_eq!(stat_batch_window(&pending, 1)[0], 100);
        assert_eq!(stat_batch_window(&pending, 2).len(), 50);
        assert!(stat_batch_window(&pending, 3).is_empty());
    }

    #[test]
    fn stat_codes_map_to_missing_present_or_transport() {
        assert_eq!(classify_stat_code(223), StatVerdict::Present);
        assert_eq!(classify_stat_code(430), StatVerdict::Missing);
        assert_eq!(classify_stat_code(423), StatVerdict::Missing);
        for code in [0, 100, 200, 340, 400, 500, 502, 503] {
            assert_eq!(
                classify_stat_code(code),
                StatVerdict::Transport,
                "code {code} must not burn budget"
            );
        }
    }

    #[test]
    fn post_duplicate_after_stat_counts_as_present() {
        // The 435/441 duplicate counts as present whether or not the
        // preceding STAT said missing (redelivery replays the same bytes).
        for prior_missing in [false, true] {
            assert_eq!(
                classify_post_code(435, prior_missing),
                PostVerdict::DuplicatePresent
            );
            assert_eq!(
                classify_post_code(441, prior_missing),
                PostVerdict::DuplicatePresent
            );
        }
        assert_eq!(classify_post_code(240, true), PostVerdict::Posted);
        // Transport noise never burns budget. Auth/permission codes ride
        // re-auth and redelivery: they retry without burning or exhausting.
        for code in [430, 423, 400, 480, 481, 500, 502, 503] {
            assert_eq!(classify_post_code(code, true), PostVerdict::Transport);
        }
        // Only an explicit 440 prohibited exhausts the article.
        assert_eq!(classify_post_code(440, true), PostVerdict::Refused);
    }

    #[test]
    fn client_errors_map_without_burning_budget_except_prohibited() {
        use nntune_core::error::Error;
        // The live re-post path (`post_article`): transport I/O, stale
        // credentials, permission states and unconfirmed duplicates all
        // retry without budget burn.
        for err in [
            Error::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "t")),
            Error::AuthFailed("<redacted>".into()),
            Error::NoPermission("<redacted>".into()),
            Error::ArticleNotFound("x".into()),
            Error::NntpReply {
                code: 435,
                text: "dup".into(),
            },
            Error::Malformed("bad framing".into()),
        ] {
            assert_eq!(classify_post_error(&err), PostVerdict::Transport);
        }
        assert_eq!(
            classify_post_error(&Error::PostProhibited("<redacted>".into())),
            PostVerdict::Refused
        );
        // The live STAT path: only confirmed absence re-posts; a bare 423
        // (the client maps only 430 to ArticleNotFound) also counts.
        assert_eq!(
            classify_stat_error(&Error::ArticleNotFound("x".into())),
            StatVerdict::Missing
        );
        assert_eq!(
            classify_stat_error(&Error::NntpReply {
                code: 423,
                text: "no articles".into(),
            }),
            StatVerdict::Missing
        );
        for err in [
            Error::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "t")),
            Error::AuthFailed("<redacted>".into()),
            Error::NntpReply {
                code: 503,
                text: "x".into(),
            },
        ] {
            assert_eq!(classify_stat_error(&err), StatVerdict::Transport);
        }
    }

    #[test]
    fn transport_never_burns_repost_budget() {
        let mut progress = TwoSpoolProgress::new("backbone-a", "backbone-b", 2).unwrap();
        progress.fold_stat(0, StatVerdict::Missing);
        // Transport STAT leaves the slot missing-but-unspent.
        progress.fold_stat(0, StatVerdict::Transport);
        assert_eq!(
            progress.articles[0],
            ArticleSpool::Missing { reposts_spent: 0 }
        );
        // Transport POST leaves the slot and the budget untouched.
        progress.fold_repost(0, PostVerdict::Transport);
        assert_eq!(
            progress.articles[0],
            ArticleSpool::Missing { reposts_spent: 0 }
        );
        assert_eq!(progress.reposts_spent, 0);
    }

    #[test]
    fn repost_budget_exhausts_per_article_then_types_the_round() {
        let mut progress = TwoSpoolProgress::new("backbone-a", "backbone-b", 1).unwrap();
        progress.fold_stat(0, StatVerdict::Missing);
        for _ in 0..MAX_REPOST_ATTEMPTS_PER_ARTICLE {
            progress.fold_repost(0, PostVerdict::Posted);
            // Still missing after the re-post: propagation never landed.
            progress.fold_stat(0, StatVerdict::Missing);
            if progress.articles[0] == ArticleSpool::Exhausted {
                break;
            }
        }
        // One more missing STAT past the per-article budget exhausts it.
        if !matches!(progress.articles[0], ArticleSpool::Exhausted) {
            progress.fold_stat(0, StatVerdict::Missing);
        }
        assert_eq!(progress.articles[0], ArticleSpool::Exhausted);
        assert_eq!(
            conclude(&progress),
            TwoSpoolOutcome::RepostExhausted {
                verified: 0,
                exhausted: 1,
                keep_original_nzb: true,
            }
        );
    }

    #[test]
    fn recheck_schedule_exhaustion_types_the_round_on_the_original_nzb() {
        // A backbone that stays silent: every delivery rechecks, verifies
        // nothing new, and redelivers. Once the full backoff schedule is
        // waited out the round gives up typed instead of polling forever —
        // the track stays on its original NZB either way.
        let mut progress = TwoSpoolProgress::new("backbone-a", "backbone-b", 2).unwrap();
        progress.fold_stat(0, StatVerdict::Present);
        progress.fold_stat(1, StatVerdict::Transport);
        assert!(matches!(
            conclude(&progress),
            TwoSpoolOutcome::Incomplete { .. }
        ));
        progress.rechecks = MAX_RECHECKS;
        assert_eq!(
            conclude(&progress),
            TwoSpoolOutcome::RepostExhausted {
                verified: 1,
                exhausted: 1,
                keep_original_nzb: true,
            }
        );
    }

    #[test]
    fn repost_refusal_exhausts_immediately_without_burning_retries() {
        let mut progress = TwoSpoolProgress::new("backbone-a", "backbone-b", 1).unwrap();
        progress.fold_stat(0, StatVerdict::Missing);
        progress.fold_repost(0, PostVerdict::Refused);
        assert_eq!(progress.articles[0], ArticleSpool::Exhausted);
    }

    #[test]
    fn missing_then_present_verifies_without_spending_a_repost() {
        // Propagation caught up between passes: the re-STAT verifies.
        let mut progress = TwoSpoolProgress::new("backbone-a", "backbone-b", 1).unwrap();
        progress.fold_stat(0, StatVerdict::Missing);
        progress.fold_stat(0, StatVerdict::Present);
        assert_eq!(progress.articles[0], ArticleSpool::Verified);
        assert_eq!(progress.reposts_spent, 0);
    }

    #[test]
    fn crash_recovery_resumes_only_unsettled_slots() {
        let mut progress = TwoSpoolProgress::new("backbone-a", "backbone-b", 4).unwrap();
        progress.fold_stat(0, StatVerdict::Present);
        progress.fold_stat(1, StatVerdict::Missing);
        progress.fold_repost(1, PostVerdict::Posted);
        progress.fold_stat(1, StatVerdict::Present);
        // Slots 0 and 1 are settled; a redelivery resumes at 2 and 3 only.
        assert_eq!(progress.pending_stat(), vec![2, 3]);
        assert_eq!(progress.pending_repost(), Vec::<usize>::new());
        // Serializing the progress is the crash-recovery record: a fresh
        // instance from the same JSON resumes identically.
        let json = serde_json::to_string(&progress).unwrap();
        let resumed: TwoSpoolProgress = serde_json::from_str(&json).unwrap();
        assert_eq!(resumed.pending_stat(), vec![2, 3]);
    }

    #[test]
    fn verified_round_concludes_verified() {
        let mut progress = TwoSpoolProgress::new("backbone-a", "backbone-b", 2).unwrap();
        progress.fold_stat(0, StatVerdict::Present);
        progress.fold_stat(1, StatVerdict::Present);
        assert_eq!(
            conclude(&progress),
            TwoSpoolOutcome::Verified { article_count: 2 }
        );
    }

    #[test]
    fn rechecks_back_off_with_an_hour_cap() {
        assert_eq!(recheck_delay_ms(0), 30_000);
        assert_eq!(recheck_delay_ms(1), 60_000);
        assert_eq!(recheck_delay_ms(5), 960_000);
        assert_eq!(recheck_delay_ms(6), 1_920_000);
        assert_eq!(recheck_delay_ms(7), 3_600_000);
        assert_eq!(recheck_delay_ms(100), 3_600_000);
    }

    #[test]
    fn backbone_pair_must_be_two_distinct_labels() {
        assert!(validate_backbone_pair("backbone-a", "backbone-b").is_ok());
        assert!(validate_backbone_pair("backbone-a", "backbone-a").is_err());
        assert!(validate_backbone_pair("", "backbone-b").is_err());
        assert!(validate_backbone_pair("Backbone-A", "backbone-b").is_err());
        assert!(!validate_backbone_label("backbone_a"));
        assert!(!validate_backbone_label("pool.example"));
    }

    #[test]
    fn same_id_repost_rejects_new_ids_and_new_bytes() {
        assert!(validate_same_id_repost("a@x", "a@x", SHA, SHA).is_ok());
        assert!(validate_same_id_repost("a@x", "b@x", SHA, SHA).is_err());
        assert!(validate_same_id_repost("a@x", "a@x", SHA, NZB_SHA).is_err());
    }

    #[test]
    fn dedupe_reuses_only_verified_retained_posts() {
        assert!(matches!(
            decide_dedupe(SHA, Some(("posted_01", true, true))).unwrap(),
            DedupeDecision::Reuse { .. }
        ));
        assert!(matches!(
            decide_dedupe(SHA, Some(("posted_01", false, true))).unwrap(),
            DedupeDecision::PostFresh { .. }
        ));
        assert!(matches!(
            decide_dedupe(SHA, Some(("posted_01", true, false))).unwrap(),
            DedupeDecision::PostFresh { .. }
        ));
        assert!(matches!(
            decide_dedupe(SHA, None).unwrap(),
            DedupeDecision::PostFresh { .. }
        ));
        assert!(decide_dedupe("not-hex", None).is_err());
    }

    #[test]
    fn repair_reads_either_spool_then_par2_and_mints_last() {
        let mut record =
            PostedFileRecord::fresh_post(SHA, "backbone-a", "backbone-b", 3, Some(NZB_SHA))
                .unwrap();
        // First spool only: spool, then PAR2, no minting yet.
        assert_eq!(
            repair_read_order(&record),
            vec![RepairSource::SpoolPost, RepairSource::Par2,]
        );
        record.verify_copy = SpoolCopyState::Verified;
        record.two_spool_verified = true;
        // Both spools healthy: either spool first, then PAR2, still no mint.
        assert_eq!(
            repair_read_order(&record),
            vec![
                RepairSource::SpoolPost,
                RepairSource::SpoolVerify,
                RepairSource::Par2,
            ]
        );
        // Exhausted same-ID path: mint-new-IDs is the last resort, with a
        // DB row update by the caller.
        record.verify_copy = SpoolCopyState::Missing;
        record.two_spool_verified = false;
        record.repost_exhausted = true;
        assert_eq!(
            repair_read_order(&record),
            vec![
                RepairSource::SpoolPost,
                RepairSource::Par2,
                RepairSource::MintNewIds,
            ]
        );
    }

    #[test]
    fn posted_file_statements_are_parameterized_and_pinned() {
        assert!(upsert_posted_file_sql().contains("$6"));
        assert!(!upsert_posted_file_sql().contains("backbone-a"));
        assert!(select_verified_by_sha_sql().contains("two_spool_verified"));
        assert!(mark_two_spool_verified_sql().contains("verified_count = $1"));
        assert!(mark_repost_exhausted_sql().contains("repost_exhausted = TRUE"));
        assert_eq!(POSTED_FILE_UPSERT_PARAM_ORDER.len(), 6);
        assert!(
            validate_posted_file_params(SHA, "backbone-a", "backbone-b", 10, Some(NZB_SHA)).is_ok()
        );
        assert!(
            validate_posted_file_params("short", "backbone-a", "backbone-b", 10, None).is_err()
        );
        assert!(validate_posted_file_params(SHA, "backbone-a", "backbone-a", 10, None).is_err());
        assert!(validate_posted_file_params(SHA, "backbone-a", "backbone-b", 0, None).is_err());
        assert_eq!(
            classify_posted_file_write(1),
            PostedFileWriteOutcome::Advanced
        );
        assert_eq!(
            classify_posted_file_write(0),
            PostedFileWriteOutcome::MissingRow
        );
    }

    #[test]
    fn fresh_post_records_first_spool_and_defers_the_second() {
        let record =
            PostedFileRecord::fresh_post(SHA, "backbone-a", "backbone-b", 3, Some(NZB_SHA))
                .unwrap();
        assert_eq!(record.post_copy, SpoolCopyState::Present);
        assert_eq!(record.verify_copy, SpoolCopyState::Unknown);
        assert!(!record.two_spool_verified);
        assert!(!record.repost_exhausted);
        // Same backbone twice would count one copy twice: refused.
        assert!(PostedFileRecord::fresh_post(SHA, "backbone-a", "backbone-a", 3, None).is_err());
    }

    #[test]
    fn record_reposted_marks_the_second_copy_present_but_unverified() {
        let mut record =
            PostedFileRecord::fresh_post(SHA, "backbone-a", "backbone-b", 2, None).unwrap();
        record.record_reposted();
        assert_eq!(record.verify_copy, SpoolCopyState::Reposted);
        assert!(!record.two_spool_verified);
        // A re-post landing after full verification changes nothing.
        record.record_verified_article();
        record.record_verified_article();
        record.record_reposted();
        assert_eq!(record.verify_copy, SpoolCopyState::Verified);
        // The repair order reads the re-posted spool before PAR2.
        let mut partial =
            PostedFileRecord::fresh_post(SHA, "backbone-a", "backbone-b", 2, None).unwrap();
        partial.record_reposted();
        assert_eq!(
            repair_read_order(&partial),
            vec![
                RepairSource::SpoolPost,
                RepairSource::SpoolVerify,
                RepairSource::Par2,
            ]
        );
    }

    #[test]
    fn record_verified_article_marks_the_row_at_full_coverage() {
        let mut record =
            PostedFileRecord::fresh_post(SHA, "backbone-a", "backbone-b", 2, None).unwrap();
        record.record_verified_article();
        assert!(!record.two_spool_verified);
        record.record_verified_article();
        assert!(record.two_spool_verified);
        assert_eq!(record.verify_copy, SpoolCopyState::Verified);
    }

    #[test]
    fn max_reposts_per_delivery_bounds_one_delivery_spend() {
        assert!(MAX_REPOSTS_PER_DELIVERY >= STAT_BATCH_SIZE as u32);
    }
}
