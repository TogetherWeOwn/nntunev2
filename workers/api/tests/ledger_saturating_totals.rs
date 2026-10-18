//! Regression pin: `fold_monthly_totals` saturates instead of wrapping.
//!
//! The monthly totals are the truth for caps and display: a wrap to a small
//! number would grant unbounded quota. These tests bypass the per-row
//! validation caps (which reject huge rows at prepare time) by constructing
//! [`LedgerRow`] directly, so the fold sees adversarial totals the writer
//! fence could still accumulate over a long-lived account.

use nntune_api::ledger::{fold_monthly_totals, LedgerRow};

fn raw_row(
    user: &str,
    period_month: &str,
    stream_seconds: u64,
    bytes_served: u64,
    key_suffix: &str,
) -> LedgerRow {
    LedgerRow {
        idempotency_key: format!("key-{key_suffix}"),
        user_id: user.to_string(),
        track_id: "track-x".to_string(),
        period_month: period_month.to_string(),
        stream_seconds,
        bytes_served,
        recorded_at: 1_767_225_600,
    }
}

#[test]
fn stream_seconds_saturate_at_u64_max() {
    let rows = vec![
        raw_row("user-alice", "2026-01", u64::MAX - 1, 1, "a"),
        raw_row("user-alice", "2026-01", 2, 1, "b"),
    ];
    let totals = fold_monthly_totals(&rows);
    assert_eq!(totals.len(), 1);
    assert_eq!(totals[0].stream_seconds, u64::MAX);
}

#[test]
fn bytes_served_saturate_at_u64_max() {
    let rows = vec![
        raw_row("user-alice", "2026-01", 1, u64::MAX - 1, "a"),
        raw_row("user-alice", "2026-01", 1, 2, "b"),
    ];
    let totals = fold_monthly_totals(&rows);
    assert_eq!(totals.len(), 1);
    assert_eq!(totals[0].bytes_served, u64::MAX);
}

#[test]
fn saturation_is_per_key_independent() {
    let rows = vec![
        raw_row("user-alice", "2026-01", u64::MAX, 10, "a1"),
        raw_row("user-alice", "2026-01", 5, 7, "a2"),
        raw_row("user-bob", "2026-01", 3, 4, "b1"),
    ];
    let totals = fold_monthly_totals(&rows);
    assert_eq!(totals.len(), 2);
    let alice = totals
        .iter()
        .find(|t| t.user_id == "user-alice")
        .expect("alice total");
    let bob = totals
        .iter()
        .find(|t| t.user_id == "user-bob")
        .expect("bob total");
    assert_eq!(alice.stream_seconds, u64::MAX);
    assert_eq!(alice.bytes_served, 17);
    assert_eq!(bob.stream_seconds, 3);
    assert_eq!(bob.bytes_served, 4);
}

#[test]
fn max_rows_fold_identically_in_either_order() {
    let a = raw_row("user-alice", "2026-01", u64::MAX, u64::MAX, "a");
    let b = raw_row("user-alice", "2026-01", u64::MAX, u64::MAX, "b");
    let forward = fold_monthly_totals(&[a.clone(), b.clone()]);
    let reversed = fold_monthly_totals(&[b, a]);
    assert_eq!(forward, reversed);
    assert_eq!(forward.len(), 1);
    assert_eq!(forward[0].stream_seconds, u64::MAX);
    assert_eq!(forward[0].bytes_served, u64::MAX);
}
