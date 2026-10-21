//! Pin replication-sequence MAX termination without wrap.
//!
//! Card title says `u64::MAX`; the store (`migration 0017`
//! `mb_replication_packet.sequence BIGINT`) and the probe
//! ([`ReplicationSloProbe::evaluate`]) are `i64`, so `u64::MAX` is
//! unrepresentable here and `i64::MAX` is the effective maximum. Every test
//! below uses `i64::MAX` / `i64::MAX - 1` on that reading.
//!
//! What is pinned (current behavior, implementation unchanged):
//! - Evaluating through `i64::MAX` terminates: no wrap to 0/MIN, no panic,
//!   no hang. A wrap would surface as a breach at 0/MIN or as an unbounded
//!   scan; a hang would time the target out.
//! - `i64::MAX - 1` still advances to `MAX`: the one-slot gap reports the
//!   single next sequence.
//! - Truncated/pending counts stay consistent: `breach_count` always equals
//!   the full breach list, the structured log's `missing_count` /
//!   `pending_count` / `truncated` agree with the report, and an in-budget
//!   pending packet at `MAX` counts as pending instead of breaching.
//!
//! Known edge pinned as-is: a ledger that already contains `MAX` still
//! reports one `missing_sequence` at `MAX` when `latest_sequence == MAX`
//! (the post-record `expected` uses `saturating_add`, so it sticks at `MAX`
//! and the tail gap `MAX..=MAX` re-enumerates the present packet). A future
//! fix for that off-by-one should update these assertions deliberately; the
//! termination guarantees above still hold.
//!
//! Sensitivity: mutate either `checked_add` arm in `workers/mb/src/replication.rs`
//! (`report_gap`'s pending-cursor advance or `enumerate_unknown`'s step) to a
//! wrapping/saturating step and this target goes red — e.g. a wrapping step
//! turns the single-`MAX` cases into a capped 10_001-breach scan, and a
//! saturating cursor turns the in-budget pending case into a spurious breach.
//! Verified 2026-10-18 by local mutation (wrapping step) before restoring.
//!
//! Standalone integration target: no existing suite is edited.

use nntune_mb::replication::{
    PacketLedger, ReplicationSloProbe, MAX_MISSING_SEQUENCE_BREACHES, MAX_STRUCTURED_LOG_BREACHES,
    REPLICATION_LAG_SLO_SECS,
};
use serde_json::Value;

const MAX: i64 = i64::MAX;
const MAX_MINUS_ONE: i64 = i64::MAX - 1;

fn parse_log(report: &nntune_mb::replication::ReplicationSloReport) -> Value {
    serde_json::from_str(&report.structured_log).expect("structured log must be JSON")
}

fn assert_no_wrap(report: &nntune_mb::replication::ReplicationSloReport) {
    assert!(
        report
            .breaches
            .iter()
            .filter(|b| b.kind == "missing_sequence")
            .all(|b| matches!(b.sequence, Some(s) if s >= MAX_MINUS_ONE)),
        "top-end enumeration must never wrap to 0/MIN: {:?}",
        report.breaches.iter().take(5).collect::<Vec<_>>(),
    );
}

/// A ledger holding the maximum sequence evaluates cleanly at `latest == MAX`.
#[test]
fn max_present_terminates_without_wrap() {
    let mut ledger = PacketLedger::default();
    ledger.record(MAX, 0, 0);

    let report = ReplicationSloProbe::default().evaluate(&ledger, MAX, 0);

    // Terminates with a bounded report (a wrap/scan would explode to the cap).
    assert_eq!(report.breach_count, report.breaches.len());
    assert_eq!(report.pending_count, 0);
    assert_no_wrap(&report);
    // Current behavior (see module docs): the present MAX re-enumerates once.
    assert_eq!(report.breach_count, 1);
    assert_eq!(report.breaches[0].kind, "missing_sequence");
    assert_eq!(report.breaches[0].sequence, Some(MAX));
    assert_eq!(report.breaches[0].lag_secs, None);

    let event = parse_log(&report);
    assert_eq!(event["event"], "mb_replication_slo_breach");
    assert_eq!(event["breach_count"], 1);
    assert_eq!(event["missing_count"], 1);
    assert_eq!(event["pending_count"], 0);
    assert_eq!(event["truncated"], false);
    assert_eq!(event["logged_breach_count"], 1);
}

/// Two consecutive top-end packets still terminate without wrapping.
#[test]
fn consecutive_top_sequences_terminate_without_wrap() {
    let mut ledger = PacketLedger::default();
    ledger.record(MAX_MINUS_ONE, 0, 0);
    ledger.record(MAX, 0, 0);

    let report = ReplicationSloProbe::default().evaluate(&ledger, MAX, 0);

    assert_eq!(report.breach_count, report.breaches.len());
    assert_eq!(report.pending_count, 0);
    assert_no_wrap(&report);
    // Same saturating-tail edge as the single-MAX case: pinned as-is.
    assert_eq!(report.breach_count, 1);
    assert_eq!(report.breaches[0].sequence, Some(MAX));

    let event = parse_log(&report);
    assert_eq!(event["missing_count"], 1);
    assert_eq!(event["pending_count"], 0);
    assert_eq!(event["truncated"], false);
}

/// `MAX - 1` advances to `MAX`: the one-slot gap reports exactly the next sequence.
#[test]
fn max_minus_one_gap_reports_max() {
    let mut ledger = PacketLedger::default();
    ledger.record(MAX_MINUS_ONE, 0, 0);

    let report = ReplicationSloProbe::default().evaluate(&ledger, MAX, 0);

    assert_eq!(report.breach_count, 1);
    assert_eq!(report.breaches[0].kind, "missing_sequence");
    assert_eq!(report.breaches[0].sequence, Some(MAX));
    assert_eq!(report.breaches[0].lag_secs, None);
    assert_eq!(report.pending_count, 0);
    assert_no_wrap(&report);
}

/// `MAX - 1` with `latest == MAX - 1` is healthy: no spurious tail gap.
#[test]
fn max_minus_one_healthy_when_latest_is_max_minus_one() {
    let mut ledger = PacketLedger::default();
    ledger.record(MAX_MINUS_ONE, 0, 0);

    let report = ReplicationSloProbe::default().evaluate(&ledger, MAX_MINUS_ONE, 0);

    assert_eq!(report.breach_count, 0);
    assert!(report.breaches.is_empty());
    assert_eq!(report.pending_count, 0);
    let event = parse_log(&report);
    assert_eq!(event["breach_count"], 0);
    assert_eq!(event["missing_count"], 0);
    assert_eq!(event["pending_count"], 0);
    assert_eq!(event["truncated"], false);
}

/// A full-range span to `MAX` truncates instead of scanning ~9e18 sequences.
#[test]
fn full_span_to_max_truncates_with_consistent_counts() {
    let mut ledger = PacketLedger::default();
    ledger.record(0, 0, 0);

    let report = ReplicationSloProbe::default().evaluate(&ledger, MAX, 0);

    // Bounded by the enumeration cap plus its marker — completion itself
    // proves no hang, the bound proves no full-span scan.
    assert_eq!(report.breach_count, MAX_MISSING_SEQUENCE_BREACHES + 1);
    assert_eq!(report.breach_count, report.breaches.len());
    assert_eq!(report.pending_count, 0);
    assert_eq!(
        report.breaches.last().map(|b| b.kind),
        Some("missing_sequence_truncated")
    );

    let event = parse_log(&report);
    assert_eq!(event["breach_count"], report.breach_count);
    assert_eq!(event["missing_count"], MAX_MISSING_SEQUENCE_BREACHES);
    assert_eq!(event["pending_count"], 0);
    assert_eq!(event["truncated"], true);
    let logged = event["breaches"].as_array().unwrap();
    assert!(logged.len() <= MAX_STRUCTURED_LOG_BREACHES);
    assert!(
        logged
            .iter()
            .any(|entry| entry["kind"] == "missing_sequence_truncated"),
        "truncation marker must survive the log cap"
    );
    assert!(report.structured_log.len() < 256 * 1024);
}

/// A pending packet at `MAX` inside its budget counts as pending, not a breach.
#[test]
fn pending_at_max_inside_budget_is_pending_not_breach() {
    let mut ledger = PacketLedger::default();
    ledger.record(MAX_MINUS_ONE, 0, 0);
    ledger.record_published(MAX, 1_000);

    let report = ReplicationSloProbe::default().evaluate(&ledger, MAX, 1_060);

    // Exercises the `checked_add` None arm: the cursor past MAX returns
    // instead of wrapping, so the covered slot is not re-enumerated.
    assert_eq!(report.breach_count, 0);
    assert!(report.breaches.is_empty());
    assert_eq!(report.pending_count, 1);
    assert_no_wrap(&report);

    let event = parse_log(&report);
    assert_eq!(event["breach_count"], 0);
    assert_eq!(event["missing_count"], 0);
    assert_eq!(event["pending_count"], 1);
    assert_eq!(event["truncated"], false);
}

/// The same pending packet breaches with its age once past the lag budget.
#[test]
fn overdue_pending_at_max_breaches_with_age() {
    let mut ledger = PacketLedger::default();
    ledger.record(MAX_MINUS_ONE, 0, 0);
    ledger.record_published(MAX, 1_000);

    let now = 1_000 + REPLICATION_LAG_SLO_SECS + 1;
    let report = ReplicationSloProbe::default().evaluate(&ledger, MAX, now);

    assert_eq!(report.breach_count, 1);
    assert_eq!(report.breaches[0].kind, "missing_sequence");
    assert_eq!(report.breaches[0].sequence, Some(MAX));
    assert_eq!(
        report.breaches[0].lag_secs,
        Some(REPLICATION_LAG_SLO_SECS + 1)
    );
    assert_eq!(report.pending_count, 0);
    assert_no_wrap(&report);

    let event = parse_log(&report);
    assert_eq!(event["breach_count"], 1);
    assert_eq!(event["missing_count"], 1);
    assert_eq!(event["pending_count"], 0);
}
