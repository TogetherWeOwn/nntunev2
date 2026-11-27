//! Cap-interaction regression: both report caps bind at once.
//!
//! Fills the structured-log cap with individual `lag_exceeded` breaches and
//! simultaneously overflows the missing-sequence enumeration cap with one
//! bounded unknown gap. Asserts the enumeration-truncation marker survives
//! the log cap by displacing an earlier entry. Tests-only; implementation
//! and cap values are unchanged. Deterministic public-probe fixture only.

use nntune_mb::replication::{
    PacketLedger, ReplicationSloProbe, MAX_MISSING_SEQUENCE_BREACHES, MAX_STRUCTURED_LOG_BREACHES,
    REPLICATION_LAG_SLO_SECS,
};
use serde_json::Value;

#[test]
fn both_caps_fire_preserves_missing_sequence_truncated_marker() {
    // Enough individual breaches to saturate the structured-log cap on their
    // own, so the truncation marker can only survive by displacing one.
    const EXTRA_INDIVIDUAL: i64 = 50;
    let num_lag = MAX_STRUCTURED_LOG_BREACHES as i64 + EXTRA_INDIVIDUAL;
    // One bounded unknown gap past the enumeration cap (no i64::MAX span).
    const EXTRA_UNKNOWN: i64 = 5_000;
    let unknown_gap = MAX_MISSING_SEQUENCE_BREACHES as i64 + EXTRA_UNKNOWN;

    let mut ledger = PacketLedger::default();
    for sequence in 0..num_lag {
        ledger.record(sequence, 0, REPLICATION_LAG_SLO_SECS + 1);
    }
    let latest = num_lag + unknown_gap - 1;

    let report = ReplicationSloProbe::default().evaluate(&ledger, latest, 0);

    // Full report: every lag breach plus the capped enumeration plus its marker.
    let expected_breach_count = num_lag as usize + MAX_MISSING_SEQUENCE_BREACHES + 1;
    assert_eq!(
        report.breach_count, expected_breach_count,
        "full breach list must keep every lag breach and the capped enumeration",
    );
    assert_eq!(
        report.breaches.len(),
        expected_breach_count,
        "breach_count must equal the full breach list",
    );
    assert!(
        report
            .breaches
            .iter()
            .any(|b| b.kind == "missing_sequence_truncated"),
        "enumeration cap must append the truncation breach",
    );
    assert_eq!(report.pending_count, 0);

    // Structured log stays parseable and within its existing cap.
    let event: Value =
        serde_json::from_str(&report.structured_log).expect("structured log must be JSON");
    assert_eq!(event["event"], "mb_replication_slo_breach");
    assert_eq!(event["breach_count"], expected_breach_count);
    assert_eq!(event["missing_count"], MAX_MISSING_SEQUENCE_BREACHES);
    assert_eq!(event["pending_count"], 0);
    let logged = event["breaches"]
        .as_array()
        .expect("logged breaches must be an array");
    assert_eq!(
        logged.len(),
        MAX_STRUCTURED_LOG_BREACHES,
        "logged list must stay within the existing structured-log cap",
    );
    assert_eq!(event["logged_breach_count"], MAX_STRUCTURED_LOG_BREACHES);
    assert_eq!(
        event["truncated"], true,
        "both caps binding must mark the log truncated",
    );

    // The marker survives even though the first log-cap entries are all
    // individual lag breaches: retaining it displaces an earlier entry.
    assert!(
        logged
            .iter()
            .any(|entry| entry["kind"] == "missing_sequence_truncated"),
        "missing_sequence_truncated must remain present when both caps fire",
    );
    assert!(
        report.structured_log.len() < 256 * 1024,
        "structured log is {} bytes",
        report.structured_log.len()
    );
}

#[test]
fn future_publication_consumes_missing_cap_without_hiding_valid_pending() {
    let mut ledger = PacketLedger::default();
    ledger.record(0, 0, 0);
    let cap = MAX_MISSING_SEQUENCE_BREACHES as i64;
    ledger.record_published(cap, i64::MAX);
    ledger.record_published(cap + 1, i64::MAX);
    ledger.record_published(cap + 2, 0);
    // A later applied packet creates another gap, exercising the ordered
    // pending scan after the first gap uses up the missing report cap.
    ledger.record(cap + 3, 0, 0);
    ledger.record_published(cap + 4, 0);

    let probe = ReplicationSloProbe::default();
    let at_cap = probe.evaluate(&ledger, cap, 0);
    assert_eq!(at_cap.breach_count, MAX_MISSING_SEQUENCE_BREACHES);
    assert_eq!(at_cap.pending_count, 0);
    assert_eq!(at_cap.breaches.last().unwrap().sequence, Some(cap));
    assert_eq!(at_cap.breaches.last().unwrap().lag_secs, None);

    let over_cap = probe.evaluate(&ledger, cap + 4, 0);
    assert_eq!(over_cap.breach_count, MAX_MISSING_SEQUENCE_BREACHES + 1);
    assert_eq!(over_cap.pending_count, 2);
    assert_eq!(
        over_cap.breaches.last().unwrap().kind,
        "missing_sequence_truncated"
    );
}
