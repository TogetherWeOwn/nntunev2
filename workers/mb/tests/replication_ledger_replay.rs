//! Replay, replacement and ordering pins for the replication ledger.
//!
//! Disjoint from `replication_lag_acceptance.rs` (report-cap contract) and
//! the inline unit tests in `replication.rs`: this file pins that re-recording
//! a sequence with the same packet ID replaces (never duplicates), that
//! snapshots sort by sequence, and that applying a packet clears only its
//! own pending entry. Uses only
//! the public `PacketLedger` / `ReplicationSloProbe` APIs with synthetic
//! timestamps; no DB or network.
use nntune_mb::replication::{PacketLedger, ReplicationSloProbe, REPLICATION_LAG_SLO_SECS};

#[test]
fn same_id_replay_replaces_record_and_recomputes_lag() {
    let mut ledger = PacketLedger::default();
    ledger.record_packet("pkt-a".to_string(), 5, 1_000, 1_120);

    // Healthy first write: inside the lag budget, so no lag breach.
    let healthy = ReplicationSloProbe::default().evaluate(&ledger, 5, 1_200);
    assert!(
        !healthy
            .breaches
            .iter()
            .any(|breach| breach.kind == "lag_exceeded"),
        "first write with 120s lag must not breach, got {:?}",
        healthy.breaches
    );

    // Replay the same sequence and packet ID with replacement timestamps
    // whose lag exceeds the budget.
    ledger.record_packet(
        "pkt-a".to_string(),
        5,
        2_000,
        2_000 + REPLICATION_LAG_SLO_SECS + 1,
    );

    let records = ledger.records();
    assert_eq!(records.len(), 1, "replay must replace, not duplicate");
    assert_eq!(records[0].sequence, 5);
    assert_eq!(records[0].packet_id, "pkt-a");
    assert_eq!(records[0].published_at, 2_000);
    assert_eq!(records[0].applied_at, 2_000 + REPLICATION_LAG_SLO_SECS + 1);
    assert_eq!(records[0].lag_secs, REPLICATION_LAG_SLO_SECS + 1);

    let report = ReplicationSloProbe::default().evaluate(&ledger, 5, 3_000_000);
    assert!(
        report.breaches.iter().any(|breach| {
            breach.kind == "lag_exceeded"
                && breach.sequence == Some(5)
                && breach.lag_secs == Some(REPLICATION_LAG_SLO_SECS + 1)
        }),
        "replayed lag must be recomputed from the replacement timestamps, got {:?}",
        report.breaches
    );
}

#[test]
fn republish_pending_sequence_replaces_publication_time_and_flips_verdict() {
    let mut ledger = PacketLedger::default();
    ledger.record(1, 0, 600);
    ledger.record_published(2, 10_000);

    let probe = ReplicationSloProbe::default();
    let now = 10_060;
    let in_flight = probe.evaluate(&ledger, 2, now);
    assert_eq!(in_flight.breach_count, 0);
    assert_eq!(in_flight.pending_count, 1);
    assert_eq!(ledger.pending_published_at(2), Some(10_000));

    // Re-publish the same pending sequence with an older publication time so
    // its age now exceeds the budget at the same evaluation instant.
    let stale_published_at = now - REPLICATION_LAG_SLO_SECS - 1;
    ledger.record_published(2, stale_published_at);
    assert_eq!(
        ledger.pending_published_at(2),
        Some(stale_published_at),
        "re-publish must replace the publication time"
    );
    assert_eq!(
        ledger.pending().len(),
        1,
        "re-publish must replace, not duplicate"
    );

    let stalled = probe.evaluate(&ledger, 2, now);
    assert_eq!(stalled.pending_count, 0);
    assert!(
        stalled.breaches.iter().any(|breach| {
            breach.kind == "missing_sequence"
                && breach.sequence == Some(2)
                && breach.lag_secs == Some(REPLICATION_LAG_SLO_SECS + 1)
        }),
        "older replacement publication time must flip the verdict to a breach, got {:?}",
        stalled.breaches
    );
}

#[test]
fn out_of_order_inserts_return_sorted_snapshots() {
    let mut ledger = PacketLedger::default();
    // Timestamps run opposite to sequence so a sort keyed on time would
    // produce a different order than the sequence order asserted below.
    ledger.record_packet("pkt-9".to_string(), 9, 700, 760);
    ledger.record_packet("pkt-7".to_string(), 7, 900, 960);
    ledger.record_packet("pkt-8".to_string(), 8, 800, 860);
    ledger.record_published(12, 1_000);
    ledger.record_published(10, 1_200);
    ledger.record_published(11, 1_100);

    let sequences: Vec<i64> = ledger.records().iter().map(|r| r.sequence).collect();
    assert_eq!(sequences, vec![7, 8, 9]);

    let pending: Vec<i64> = ledger.pending().iter().map(|p| p.sequence).collect();
    assert_eq!(pending, vec![10, 11, 12]);
}

#[test]
fn apply_one_pending_packet_clears_only_its_matching_entry() {
    let mut ledger = PacketLedger::default();
    // Pending 20 shares the applied packet's publication time, so only a
    // clear keyed on sequence (not on time) leaves it in place.
    ledger.record_published(20, 2_100);
    ledger.record_published(21, 2_100);
    ledger.record_published(22, 2_200);

    ledger.record_packet("pkt-21".to_string(), 21, 2_100, 2_200);

    assert_eq!(ledger.pending_published_at(21), None);
    assert_eq!(ledger.pending_published_at(20), Some(2_100));
    assert_eq!(ledger.pending_published_at(22), Some(2_200));
    let remaining: Vec<i64> = ledger.pending().iter().map(|p| p.sequence).collect();
    assert_eq!(remaining, vec![20, 22]);
}
