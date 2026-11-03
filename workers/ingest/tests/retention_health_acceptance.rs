//! Retention health checker acceptance (stage B2.7, policy slice).
//!
//! Drives the pure retention policy end to end through scripted spool
//! samples — the same shapes the Cron sweep will feed it once the
//! posted-file store, the verify STAT machinery and the Cron handler
//! land. No live provider, no article references: samples carry
//! per-spool presence only, files are opaque 128-bit hex ids, and the
//! spools are referred to by role label only.
//!
//! - Simulated loss is detected, repaired and returns to healthy.
//! - A beyond-redundancy loss lands as `lost` with resolver fallback.
//! - The dashboard view carries state plus timestamps and nothing else.

use nntune_ingest::retention::{
    after_same_id_failure, check_day, health_view, is_due, plan_action, repair_strategy,
    resolver_fallback, scheduled_check_at, FileHealthState, RepairOutcome, RepairStrategy,
    ResolverFallback, RetentionAction, SampleVerdict,
};

/// Opaque posted-file ids: random 128-bit hex, never article references.
fn file_a() -> String {
    "9f2c".repeat(8)
}

fn file_b() -> String {
    "41ad".repeat(8)
}

#[test]
fn simulated_loss_is_detected_repaired_and_returns_to_healthy() {
    let verified_at = 1_800_000_000u64;
    let mut state = FileHealthState::Healthy;
    let mut completed = 0u64;

    // Day-1 sample: both spools hold the file.
    let next = scheduled_check_at(verified_at, completed);
    assert!(is_due(next, next));
    assert_eq!(
        plan_action(state, SampleVerdict::BothPresent, None),
        RetentionAction::RecordHealthy
    );
    completed += 1;

    // Day-7 sample: one spool missed articles but the surviving spool
    // covers them — same-ID re-post first, never new IDs up front.
    let next = scheduled_check_at(verified_at, completed);
    assert_eq!(
        next,
        verified_at + check_day(1) * nntune_ingest::retention::SECS_PER_DAY
    );
    let action = plan_action(
        state,
        SampleVerdict::OneSpoolMissing { repairable: true },
        None,
    );
    assert_eq!(action, RetentionAction::RepairSameId);
    assert_eq!(repair_strategy(action), Some(RepairStrategy::SameMessageId));
    state = FileHealthState::Degraded;
    assert_eq!(
        resolver_fallback(state),
        ResolverFallback::PostedCopy,
        "a degraded file still plays the posted copy"
    );

    // Repair round verified on the missing spool: back to healthy and
    // the widening cadence continues from the repair time.
    let action = plan_action(
        FileHealthState::Repairing,
        SampleVerdict::OneSpoolMissing { repairable: true },
        Some(RepairOutcome::RepairedSameId),
    );
    assert_eq!(action, RetentionAction::RecordHealthy);
    state = FileHealthState::Healthy;
    completed += 1;

    let view = health_view(&file_a(), state, next, completed, verified_at).unwrap();
    assert_eq!(view.state, FileHealthState::Healthy);
    assert_eq!(
        view.next_check_at,
        scheduled_check_at(verified_at, completed)
    );
    assert_eq!(resolver_fallback(view.state), ResolverFallback::PostedCopy);
}

#[test]
fn same_id_failure_escalates_to_new_ids_before_loss() {
    // A failed same-ID round mints new IDs and updates the DB row; the
    // file is lost only if that round is unrepairable too.
    assert_eq!(
        after_same_id_failure(FileHealthState::Degraded),
        RetentionAction::RepostNewIds
    );
    assert_eq!(
        repair_strategy(RetentionAction::RepostNewIds),
        Some(RepairStrategy::NewIds)
    );
    let action = plan_action(
        FileHealthState::Repairing,
        SampleVerdict::BothMissing { repairable: false },
        Some(RepairOutcome::Unrepairable),
    );
    assert_eq!(action, RetentionAction::MarkLost);
}

#[test]
fn beyond_redundancy_loss_lands_as_lost_with_resolver_fallback() {
    // Both spools missed and PAR2 cannot cover the gap: terminal.
    let action = plan_action(
        FileHealthState::Healthy,
        SampleVerdict::BothMissing { repairable: false },
        None,
    );
    assert_eq!(action, RetentionAction::MarkLost);

    let state = FileHealthState::Lost;
    assert_eq!(
        resolver_fallback(state),
        ResolverFallback::VideoThenHidden,
        "a lost file plays the linked video, else stays unavailable/hidden"
    );
    // Terminal: further samples are no-ops, never a silent resurrection.
    assert_eq!(
        plan_action(state, SampleVerdict::BothPresent, None),
        RetentionAction::Noop
    );

    let view = health_view(&file_b(), state, 1_800_001_000, 2, 1_800_000_000).unwrap();
    let json = serde_json::to_value(&view).unwrap();
    assert_eq!(json["state"], "lost");
    assert_eq!(json["posted_file_id"], file_b());
}

#[test]
fn dashboard_view_carries_state_and_timestamps_only() {
    let view = health_view(
        &file_a(),
        FileHealthState::Degraded,
        1_800_010_000,
        1,
        1_800_000_000,
    )
    .unwrap();
    let json = serde_json::to_value(&view).unwrap();
    let map = json.as_object().unwrap();
    // Exactly these fields: state, last check, next check. No room for
    // article references by construction.
    assert_eq!(map.len(), 4);
    assert!(map.contains_key("posted_file_id"));
    assert_eq!(json["state"], "degraded");
    assert_eq!(json["last_check_at"], 1_800_010_000u64);
    assert_eq!(json["next_check_at"], scheduled_check_at(1_800_000_000, 1));
    // The serialized view must not leak wire-shaped text.
    let rendered = serde_json::to_string(&view).unwrap();
    assert!(!rendered.contains('@'), "no message-ID may ride the view");
    assert!(!rendered.contains('<') && !rendered.contains('>'));
    for word in ["message", "subject", "segment", "spool", "backbone"] {
        assert!(
            !rendered.to_lowercase().contains(word),
            "redacted word in view: {word}"
        );
    }
    // Debug carries the same redacted fields only.
    let debug = format!("{view:?}");
    assert!(!debug.contains('@'));
}
