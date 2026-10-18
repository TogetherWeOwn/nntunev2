//! Regression guards for the `packet_id_conflict` guarantees claimed in
//! the attestation and the `PacketLedger::packet_id_conflicts` doc comment:
//! sticky, exactly one breach per conflicted sequence, and sequence order.
//!
//! The paired acceptance contract lives in the protected
//! `tests/packet_id_conflicts.rs` (owned by the Test Author — never edited
//! here); these tests pin the three behaviours the reviewer mutated
//! (dedupe guard, sticky pin, sort) against the same public surface
//! (`records()`, `packet_id_conflicts()`, and the SLO probe report).
use nntune_mb::replication::{PacketLedger, ReplicationSloProbe};

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
fn repeated_conflicting_writes_yield_exactly_one_breach() {
    let mut ledger = PacketLedger::default();
    record(&mut ledger, "id-A", 5, 1_000, 1_120);
    // Repeat the conflict with two more distinct identities.
    record(&mut ledger, "id-B", 5, 2_000, 2_600);
    record(&mut ledger, "id-B", 5, 2_000, 2_600);
    record(&mut ledger, "id-C", 5, 3_000, 3_600);

    assert_eq!(ledger.packet_id_conflicts(), vec![5]);
    assert_eq!(ledger.records()[0].packet_id, "id-A");

    let report = ReplicationSloProbe::default().evaluate(&ledger, 5, 4_000);
    let conflicts: Vec<_> = report
        .breaches
        .iter()
        .filter(|b| b.kind == "packet_id_conflict")
        .collect();
    assert_eq!(
        conflicts.len(),
        1,
        "one conflicted sequence must yield exactly one breach, got {:?}",
        report.breaches
    );
    assert_eq!(report.breach_count, 1);
    assert_eq!(conflicts[0].sequence, Some(5));
}

#[test]
fn same_id_rerecord_after_conflict_does_not_clear_the_conflict() {
    let mut ledger = PacketLedger::default();
    record(&mut ledger, "id-A", 5, 1_000, 1_120);
    record(&mut ledger, "id-B", 5, 2_000, 2_600);
    // A later retry with the original identity must not clear the pin:
    // the first record stays and the probe must not read green.
    record(&mut ledger, "id-A", 5, 1_000, 1_120);

    assert_eq!(ledger.packet_id_conflicts(), vec![5]);
    assert_eq!(ledger.records()[0].packet_id, "id-A");
    assert_eq!(ledger.records()[0].published_at, 1_000);
    assert_eq!(ledger.records()[0].applied_at, 1_120);

    let report = ReplicationSloProbe::default().evaluate(&ledger, 5, 3_000);
    assert_eq!(
        report.breach_count, 1,
        "sticky conflict must survive a same-id re-record, got {:?}",
        report.breaches
    );
    assert_eq!(report.breaches[0].kind, "packet_id_conflict");
    assert_eq!(report.breaches[0].sequence, Some(5));
}

#[test]
fn conflicts_on_two_sequences_come_back_in_sequence_order() {
    let mut ledger = PacketLedger::default();
    record(&mut ledger, "id-A9", 9, 1_000, 1_120);
    record(&mut ledger, "id-B9", 9, 2_000, 2_600);
    record(&mut ledger, "id-A3", 3, 1_000, 1_120);
    record(&mut ledger, "id-B3", 3, 2_000, 2_600);

    // Recorded out of order (9 then 3); the accessor promises order.
    assert_eq!(ledger.packet_id_conflicts(), vec![3, 9]);

    let report = ReplicationSloProbe::default().evaluate(&ledger, 9, 3_000);
    let conflict_seqs: Vec<_> = report
        .breaches
        .iter()
        .filter(|b| b.kind == "packet_id_conflict")
        .filter_map(|b| b.sequence)
        .collect();
    assert_eq!(
        conflict_seqs,
        vec![3, 9],
        "conflict breaches must be in sequence order, got {:?}",
        report.breaches
    );
}
