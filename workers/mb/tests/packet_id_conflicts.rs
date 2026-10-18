//! Acceptance contract: a recorded sequence keeps its first `packet_id`.
//!
//! The builder owns the implementation; this file pins the observable
//! contract through the existing public surface (`records()`, `pending()`,
//! and the SLO probe report):
//!
//! - Re-recording a sequence with a *different* `packet_id` is rejected:
//!   the original record (id, timestamps, lag) is left untouched.
//! - The rejection is surfaced, not silent: the probe reports a
//!   `packet_id_conflict` breach for that sequence, so probe health cannot
//!   read green while the ledger has seen a conflicting identity.
//! - Re-recording with the *same* `packet_id` stays an idempotent retry
//!   exactly as today (replacement/lag recomputation, no conflict).
//! - A conflict on one sequence changes no other record, clears/resurrects
//!   no pending entry, and does not poison later recording.
use nntune_mb::replication::{PacketLedger, ReplicationSloProbe};
use serde_json::Value;

fn record(
    ledger: &mut PacketLedger,
    packet_id: &str,
    sequence: i64,
    published_at: i64,
    applied_at: i64,
) {
    ledger.record_packet(packet_id.to_string(), sequence, published_at, applied_at);
}

#[test]
fn conflicting_packet_id_does_not_overwrite_the_recorded_packet() {
    let mut ledger = PacketLedger::default();
    record(&mut ledger, "id-A", 5, 1_000, 1_120);
    // A different upstream identity for the same sequence must not replace
    // the recorded packet, its timestamps, or its lag.
    record(&mut ledger, "id-B", 5, 2_000, 2_600);

    let records = ledger.records();
    assert_eq!(records.len(), 1, "one recorded packet per sequence");
    assert_eq!(records[0].packet_id, "id-A");
    assert_eq!(records[0].sequence, 5);
    assert_eq!(records[0].published_at, 1_000);
    assert_eq!(records[0].applied_at, 1_120);
    assert_eq!(records[0].lag_secs, 120);
}

#[test]
fn conflicting_packet_id_is_surfaced_as_a_probe_breach() {
    let mut ledger = PacketLedger::default();
    record(&mut ledger, "id-A", 5, 1_000, 1_120);
    record(&mut ledger, "id-B", 5, 2_000, 2_600);

    // Lag is within budget and no sequence is missing, so the conflict is
    // the only thing the probe may report.
    let report = ReplicationSloProbe::default().evaluate(&ledger, 5, 3_000);
    assert_eq!(
        report.breach_count, 1,
        "conflict must surface as exactly one breach, got {:?}",
        report.breaches
    );
    assert_eq!(report.breaches[0].kind, "packet_id_conflict");
    assert_eq!(report.breaches[0].sequence, Some(5));

    let event: Value = serde_json::from_str(&report.structured_log).unwrap();
    assert_eq!(event["event"], "mb_replication_slo_breach");
    assert_eq!(event["breach_count"], 1);
    assert!(
        event["breaches"].as_array().is_some_and(|logged| logged
            .iter()
            .any(|entry| { entry["kind"] == "packet_id_conflict" && entry["sequence"] == 5 })),
        "structured log must carry the conflict, got {}",
        report.structured_log
    );
}

#[test]
fn same_packet_id_retry_is_idempotent_and_not_a_conflict() {
    let mut ledger = PacketLedger::default();
    record(&mut ledger, "id-A", 5, 1_000, 1_120);
    // Byte-identical retry (e.g. a retried applier write).
    record(&mut ledger, "id-A", 5, 1_000, 1_120);

    let records = ledger.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].packet_id, "id-A");
    assert_eq!(records[0].published_at, 1_000);
    assert_eq!(records[0].applied_at, 1_120);
    assert_eq!(records[0].lag_secs, 120);

    let report = ReplicationSloProbe::default().evaluate(&ledger, 5, 3_000);
    assert_eq!(
        report.breach_count, 0,
        "same-id retry must not breach, got {:?}",
        report.breaches
    );
}

#[test]
fn same_packet_id_rerecord_still_updates_lag_like_today() {
    let mut ledger = PacketLedger::default();
    record(&mut ledger, "id-A", 5, 1_000, 1_120);
    // Same identity, corrected application time: today's replacement and
    // lag recomputation are unchanged, and this is not a conflict.
    record(&mut ledger, "id-A", 5, 1_000, 1_300);

    let records = ledger.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].packet_id, "id-A");
    assert_eq!(records[0].applied_at, 1_300);
    assert_eq!(records[0].lag_secs, 300);

    let report = ReplicationSloProbe::default().evaluate(&ledger, 5, 3_000);
    assert_eq!(
        report.breach_count, 0,
        "same-id re-record must not breach, got {:?}",
        report.breaches
    );
}

#[test]
fn conflict_does_not_touch_other_records_pending_or_later_recording() {
    let mut ledger = PacketLedger::default();
    record(&mut ledger, "id-A", 5, 1_000, 1_120);
    record(&mut ledger, "id-B", 5, 2_000, 2_600);
    // The ledger stays usable after a rejected conflict.
    record(&mut ledger, "id-C", 6, 1_100, 1_220);
    ledger.record_published(7, 2_900);

    let records = ledger.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].packet_id, "id-A");
    assert_eq!(records[0].lag_secs, 120);
    assert_eq!(records[1].packet_id, "id-C");
    assert_eq!(records[1].sequence, 6);
    assert_eq!(records[1].lag_secs, 120);

    let pending = ledger.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].sequence, 7);
    assert_eq!(pending[0].published_at, 2_900);

    // Sequence 7 is in flight inside its budget (pending, not missing) and
    // no lag is exceeded; the conflict on 5 is the only breach.
    let report = ReplicationSloProbe::default().evaluate(&ledger, 7, 3_000);
    assert_eq!(report.pending_count, 1);
    assert_eq!(
        report.breach_count, 1,
        "only the conflict may breach, got {:?}",
        report.breaches
    );
    assert_eq!(report.breaches[0].kind, "packet_id_conflict");
    assert_eq!(report.breaches[0].sequence, Some(5));
}

#[test]
fn conflicting_attempt_does_not_clear_or_resurrect_pending() {
    let mut ledger = PacketLedger::default();
    ledger.record_published(5, 1_000);
    assert_eq!(ledger.pending_published_at(5), Some(1_000));

    // Applying the packet clears its pending entry, as today.
    record(&mut ledger, "id-A", 5, 1_000, 1_120);
    assert_eq!(ledger.pending_published_at(5), None);
    assert!(ledger.pending().is_empty());

    // A rejected conflicting identity neither re-pends the sequence nor
    // disturbs pending state.
    record(&mut ledger, "id-B", 5, 2_000, 2_600);
    assert_eq!(ledger.pending_published_at(5), None);
    assert!(ledger.pending().is_empty());
    assert_eq!(ledger.records()[0].packet_id, "id-A");
}
