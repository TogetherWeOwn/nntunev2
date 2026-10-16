//! Acceptance contract for the replication-lag SLO probe.
//!
//! The builder owns the implementation; this file deliberately imports the
//! public contract that implementation must provide.
use nntune_mb::replication::{PacketLedger, ReplicationSloProbe};
use serde_json::Value;

const TWO_HOURS_SECS: i64 = 2 * 60 * 60;

#[test]
fn ledger_preserves_publication_application_and_lag_per_packet() {
    let mut ledger = PacketLedger::default();
    ledger.record(41, 1_000, 1_120);
    ledger.record(42, 1_360, 1_540);

    let records = ledger.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].sequence, 41);
    assert_eq!(records[0].published_at, 1_000);
    assert_eq!(records[0].applied_at, 1_120);
    assert_eq!(records[0].lag_secs, 120);
    assert_eq!(records[1].sequence, 42);
    assert_eq!(records[1].published_at, 1_360);
    assert_eq!(records[1].applied_at, 1_540);
    assert_eq!(records[1].lag_secs, 180);
}

#[test]
fn probe_reports_over_two_hour_lag_and_missing_packet_sequences() {
    let mut ledger = PacketLedger::default();
    ledger.record(7, 1_000, 1_000 + TWO_HOURS_SECS + 1);
    ledger.record(9, 1_500, 1_600);

    let report = ReplicationSloProbe::default().evaluate(&ledger, 9, 1_700);

    assert!(report.breaches.iter().any(|breach| {
        breach.kind == "lag_exceeded"
            && breach.sequence == Some(7)
            && breach.lag_secs == Some(TWO_HOURS_SECS + 1)
    }));
    assert!(report
        .breaches
        .iter()
        .any(|breach| breach.kind == "missing_sequence" && breach.sequence == Some(8)));
}

#[test]
fn breach_emits_structured_log_and_metric_instead_of_silent_skip() {
    let mut ledger = PacketLedger::default();
    ledger.record(3, 100, 100 + TWO_HOURS_SECS + 1);

    let report = ReplicationSloProbe::default().evaluate(&ledger, 3, 100 + TWO_HOURS_SECS + 2);

    let event: Value = serde_json::from_str(&report.structured_log).unwrap();
    assert_eq!(event["event"], "mb_replication_slo_breach");
    assert_eq!(event["breach_count"], report.breaches.len());
    assert!(report.metric_name.starts_with("mb_replication_slo_breach"));
    assert!(!report.breaches.is_empty());
}

#[test]
fn readme_documents_p95_source_and_breach_definition() {
    let readme = include_str!("../README.md").to_ascii_lowercase();
    assert!(
        readme.contains("p95 source"),
        "document the source population for p95"
    );
    assert!(
        readme.contains("breach definition"),
        "document exactly when a breach occurs"
    );
    assert!(
        readme.contains("mb_replication_slo_breach"),
        "document the emitted metric name"
    );
}
