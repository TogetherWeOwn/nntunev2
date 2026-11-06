//! Retention health checker policy (stage B2.7).
//!
//! Pure policy for the Cloudflare Cron-triggered retention sweep over
//! posted files. This module owns the widening check cadence, the
//! per-file health state machine, and the redacted dashboard view. It
//! owns no I/O: the Cron handler (a later slice) lists posted-file rows
//! due via [`is_due`], samples both spools through the verify pipeline's
//! STAT machinery, and feeds the outcomes back through [`plan_action`].
//! Repair itself reuses that same machinery — repair from the surviving
//! spool or PAR2 and re-post (same message-ID first, new IDs plus a DB
//! update only if that fails) — so this module never forks it; it only
//! names which repair the sweep asks for next.
//!
//! Cadence (admin-visible build-time default): a widening STAT sample at
//! about 1/7/30/90 days after verification, then monthly. The canary's
//! scheduled 1/30/90/180-day read-backs ride the same schedule: days 1,
//! 30 and 90 are cadence checks, and day 180 is the third monthly check
//! after day 90 (90 + 30 x 3).
//!
//! Redaction: nothing here carries article references. Samples report
//! per-spool presence counts only, the dashboard view carries the opaque
//! posted-file id plus timestamps, and no log line this module can emit
//! (all fields are counts, enums and opaque ids) can leak a message-ID,
//! subject, file name or segment list.

use serde::{Deserialize, Serialize};

/// Widening cadence: days after verification (then monthly).
/// Build-time default, visible to admins; changing it only moves future
/// `next_check_at` values, never stored history.
pub const CADENCE_DAYS: [u64; 4] = [1, 7, 30, 90];

/// Steady-state interval once the widening cadence is exhausted: monthly.
pub const STEADY_INTERVAL_DAYS: u64 = 30;

/// Seconds per day for cadence arithmetic.
pub const SECS_PER_DAY: u64 = 86_400;

/// Per-file retention health, as surfaced on the ingest detail view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileHealthState {
    /// Both spools answered the last sample (or the file was repaired).
    Healthy,
    /// One spool missed the last sample; a repair re-post is queued.
    Degraded,
    /// A repair re-post is in flight; the next sample judges it.
    Repairing,
    /// Beyond repair (both spools lost, PAR2 insufficient). Terminal:
    /// the track falls back per the playback resolver.
    Lost,
}

impl FileHealthState {
    /// Stable snake-case name for the dashboard view and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            FileHealthState::Healthy => "healthy",
            FileHealthState::Degraded => "degraded",
            FileHealthState::Repairing => "repairing",
            FileHealthState::Lost => "lost",
        }
    }
}

/// Outcome of one sweep sample for one posted file: per-spool presence
/// only. Counts and flags — never article references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleVerdict {
    /// Both spools hold the sampled articles.
    BothPresent,
    /// Exactly one spool missed sampled articles; `repairable` says the
    /// surviving spool or PAR2 covers the gap (judged by the verify
    /// machinery, not here).
    OneSpoolMissing { repairable: bool },
    /// Both spools missed sampled articles; `repairable` says PAR2 (also
    /// posted on both spools) still covers the gap.
    BothMissing { repairable: bool },
}

/// Outcome of a repair round the sweep requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairOutcome {
    /// Same message-ID re-post verified on the missing spool.
    RepairedSameId,
    /// Same-ID re-post failed; new IDs verified and the DB row updated.
    RepairedNewIds,
    /// Beyond redundancy: neither spool nor PAR2 covers the file.
    Unrepairable,
}

/// What the sweep should do next for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionAction {
    /// Record the sample, stay/return healthy, schedule the next check.
    RecordHealthy,
    /// Record degraded and ask the verify machinery for a same-ID repair.
    RepairSameId,
    /// Same-ID repair failed; ask for new IDs plus a DB update.
    RepostNewIds,
    /// Record repairing (repair in flight; the next sample judges it).
    RecordRepairing,
    /// Flag the file lost; the track falls back per the resolver.
    MarkLost,
    /// No sweep work: the file is already terminal.
    Noop,
}

/// Which repair the sweep asks for. Same message-ID first; new IDs plus
/// a DB update only if that fails — the verify pipeline's repair order,
/// named here and executed there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairStrategy {
    SameMessageId,
    NewIds,
}

/// Where the playback resolver should source a track, given file health.
/// A healthy, degraded or repairing file still plays from the posted
/// copy (the surviving spool, or the repair queue); a lost file plays
/// the linked video where one exists, else stays unavailable/hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverFallback {
    /// Play the posted copy (surviving spool; repair covers the gap).
    PostedCopy,
    /// Play the linked video; without one the track is unavailable/hidden.
    VideoThenHidden,
}

/// Resolver source for a file in `state`.
pub fn resolver_fallback(state: FileHealthState) -> ResolverFallback {
    match state {
        FileHealthState::Healthy | FileHealthState::Degraded | FileHealthState::Repairing => {
            ResolverFallback::PostedCopy
        }
        FileHealthState::Lost => ResolverFallback::VideoThenHidden,
    }
}

/// Day offset of the `completed`-th check (0-based) after verification.
pub fn check_day(completed: u64) -> u64 {
    if (completed as usize) < CADENCE_DAYS.len() {
        CADENCE_DAYS[completed as usize]
    } else {
        let extra = completed - CADENCE_DAYS.len() as u64;
        CADENCE_DAYS[CADENCE_DAYS.len() - 1] + STEADY_INTERVAL_DAYS * (extra + 1)
    }
}

/// Unix time of the `completed`-th check after `verified_at` (0-based).
/// Saturating: far-future timestamps clamp instead of wrapping.
pub fn scheduled_check_at(verified_at: u64, completed: u64) -> u64 {
    verified_at.saturating_add(check_day(completed).saturating_mul(SECS_PER_DAY))
}

/// A file is due when `now` has reached its next scheduled check.
pub fn is_due(now: u64, next_check_at: u64) -> bool {
    now >= next_check_at
}

/// Drive one sweep step for a file in `state` given a fresh `verdict`.
///
/// `repair` reports the repair round already requested for a repairing
/// file (`None` while the request is still queued); it is ignored in
/// every other state.
pub fn plan_action(
    state: FileHealthState,
    verdict: SampleVerdict,
    repair: Option<RepairOutcome>,
) -> RetentionAction {
    match state {
        FileHealthState::Lost => RetentionAction::Noop,
        FileHealthState::Repairing => match repair {
            Some(RepairOutcome::RepairedSameId) | Some(RepairOutcome::RepairedNewIds) => {
                RetentionAction::RecordHealthy
            }
            Some(RepairOutcome::Unrepairable) => RetentionAction::MarkLost,
            None => match verdict {
                SampleVerdict::BothPresent => RetentionAction::RecordHealthy,
                _ => RetentionAction::RecordRepairing,
            },
        },
        FileHealthState::Healthy | FileHealthState::Degraded => match verdict {
            SampleVerdict::BothPresent => RetentionAction::RecordHealthy,
            SampleVerdict::OneSpoolMissing { repairable: true }
            | SampleVerdict::BothMissing { repairable: true } => {
                // The surviving spool or PAR2 covers the gap (judged by
                // the verify machinery): repair and re-post.
                RetentionAction::RepairSameId
            }
            SampleVerdict::OneSpoolMissing { repairable: false }
            | SampleVerdict::BothMissing { repairable: false } => {
                // Neither the surviving copy nor PAR2 covers the gap, so
                // no re-post could rebuild the file: terminal.
                RetentionAction::MarkLost
            }
        },
    }
}

/// Which repair a [`RetentionAction::RepairSameId`] / `RepostNewIds`
/// step asks the verify machinery for. Same message-ID first; new IDs
/// only after the same-ID round fails.
pub fn repair_strategy(action: RetentionAction) -> Option<RepairStrategy> {
    match action {
        RetentionAction::RepairSameId => Some(RepairStrategy::SameMessageId),
        RetentionAction::RepostNewIds => Some(RepairStrategy::NewIds),
        _ => None,
    }
}

/// Follow-up after a same-ID repair round fails: mint new IDs and update
/// the DB row — but only from a repair the sweep actually requested.
pub fn after_same_id_failure(state: FileHealthState) -> RetentionAction {
    match state {
        FileHealthState::Degraded | FileHealthState::Repairing => RetentionAction::RepostNewIds,
        FileHealthState::Healthy | FileHealthState::Lost => RetentionAction::Noop,
    }
}

/// Opaque posted-file reference: 128-bit random id as 32 lowercase hex
/// chars. The sweep and the dashboard join on this only; it never
/// encodes a message-ID, subject, file name or backbone label.
pub fn valid_posted_file_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Redacted per-file health for the ingest detail view: state plus
/// check timestamps only. No article references by construction — the
/// struct has no field that could carry one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHealthView {
    pub posted_file_id: String,
    pub state: FileHealthState,
    pub last_check_at: u64,
    pub next_check_at: u64,
}

/// Build the dashboard view. Refuses non-opaque ids so a message-ID,
/// subject or file name can never ride this struct into UI or logs.
pub fn health_view(
    posted_file_id: &str,
    state: FileHealthState,
    last_check_at: u64,
    completed_checks: u64,
    verified_at: u64,
) -> Result<FileHealthView, &'static str> {
    if !valid_posted_file_id(posted_file_id) {
        return Err("posted_file_id must be the opaque 128-bit hex id");
    }
    Ok(FileHealthView {
        posted_file_id: posted_file_id.to_string(),
        state,
        last_check_at,
        next_check_at: scheduled_check_at(verified_at, completed_checks),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cadence_widens_then_goes_monthly() {
        assert_eq!(
            CADENCE_DAYS,
            [1, 7, 30, 90],
            "widening cadence is the admin-visible default"
        );
        assert_eq!(STEADY_INTERVAL_DAYS, 30);
        let days: Vec<u64> = (0..7).map(check_day).collect();
        assert_eq!(days, vec![1, 7, 30, 90, 120, 150, 180]);
    }

    #[test]
    fn scheduled_checks_add_days_without_wrapping() {
        assert_eq!(scheduled_check_at(1_000_000, 0), 1_000_000 + SECS_PER_DAY);
        assert_eq!(
            scheduled_check_at(1_000_000, 3),
            1_000_000 + 90 * SECS_PER_DAY
        );
        // Day 180 (the canary's last read-back) is the third monthly check.
        assert_eq!(scheduled_check_at(0, 6), 180 * SECS_PER_DAY);
        assert_eq!(scheduled_check_at(u64::MAX, 6), u64::MAX);
    }

    #[test]
    fn due_only_at_or_past_the_schedule() {
        assert!(is_due(100, 100));
        assert!(is_due(101, 100));
        assert!(!is_due(99, 100));
    }

    #[test]
    fn healthy_sample_stays_healthy() {
        assert_eq!(
            plan_action(FileHealthState::Healthy, SampleVerdict::BothPresent, None),
            RetentionAction::RecordHealthy
        );
    }

    #[test]
    fn one_spool_loss_repairs_same_id_first() {
        assert_eq!(
            plan_action(
                FileHealthState::Healthy,
                SampleVerdict::OneSpoolMissing { repairable: true },
                None
            ),
            RetentionAction::RepairSameId
        );
        assert_eq!(
            repair_strategy(RetentionAction::RepairSameId),
            Some(RepairStrategy::SameMessageId)
        );
        // A failed same-ID round escalates to new IDs, never straight to lost.
        assert_eq!(
            after_same_id_failure(FileHealthState::Degraded),
            RetentionAction::RepostNewIds
        );
        assert_eq!(
            repair_strategy(RetentionAction::RepostNewIds),
            Some(RepairStrategy::NewIds)
        );
    }

    #[test]
    fn unrepairable_gaps_are_terminal() {
        for verdict in [
            SampleVerdict::OneSpoolMissing { repairable: false },
            SampleVerdict::BothMissing { repairable: false },
        ] {
            assert_eq!(
                plan_action(FileHealthState::Healthy, verdict, None),
                RetentionAction::MarkLost,
                "{verdict:?}"
            );
        }
        // A both-spools gap that PAR2 still covers repairs like a
        // single-spool gap: same-ID re-post first.
        assert_eq!(
            plan_action(
                FileHealthState::Healthy,
                SampleVerdict::BothMissing { repairable: true },
                None
            ),
            RetentionAction::RepairSameId
        );
        assert_eq!(
            plan_action(FileHealthState::Lost, SampleVerdict::BothPresent, None),
            RetentionAction::Noop
        );
    }

    #[test]
    fn repairing_judged_by_repair_then_sample() {
        // Finished repairs return to healthy regardless of spool state.
        for outcome in [RepairOutcome::RepairedSameId, RepairOutcome::RepairedNewIds] {
            assert_eq!(
                plan_action(
                    FileHealthState::Repairing,
                    SampleVerdict::BothMissing { repairable: true },
                    Some(outcome)
                ),
                RetentionAction::RecordHealthy,
                "{outcome:?}"
            );
        }
        assert_eq!(
            plan_action(
                FileHealthState::Repairing,
                SampleVerdict::BothMissing { repairable: true },
                Some(RepairOutcome::Unrepairable)
            ),
            RetentionAction::MarkLost
        );
        // Repair still queued: a full sample closes it, a gapped one waits.
        assert_eq!(
            plan_action(FileHealthState::Repairing, SampleVerdict::BothPresent, None),
            RetentionAction::RecordHealthy
        );
        assert_eq!(
            plan_action(
                FileHealthState::Repairing,
                SampleVerdict::OneSpoolMissing { repairable: true },
                None
            ),
            RetentionAction::RecordRepairing
        );
    }

    #[test]
    fn lost_files_fall_back_past_the_posted_copy() {
        assert_eq!(
            resolver_fallback(FileHealthState::Healthy),
            ResolverFallback::PostedCopy
        );
        assert_eq!(
            resolver_fallback(FileHealthState::Degraded),
            ResolverFallback::PostedCopy
        );
        assert_eq!(
            resolver_fallback(FileHealthState::Repairing),
            ResolverFallback::PostedCopy
        );
        assert_eq!(
            resolver_fallback(FileHealthState::Lost),
            ResolverFallback::VideoThenHidden
        );
    }

    #[test]
    fn opaque_ids_only() {
        assert!(valid_posted_file_id(&"ab".repeat(16)));
        assert!(valid_posted_file_id(&"AB".repeat(16)));
        for bad in [
            "",
            "abc",
            &"ab".repeat(15),
            &"ab".repeat(17),
            "<32chars@example.000>",
            "random-subject-line-for-a-file",
            "89abcdef0123456789abcdef0123456g",
        ] {
            assert!(!valid_posted_file_id(bad), "{bad}");
        }
        assert!(health_view("xyz", FileHealthState::Healthy, 0, 0, 0).is_err());
    }
}
