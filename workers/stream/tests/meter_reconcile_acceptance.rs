//! Passing regression/acceptance tests for complete-play reconciliation
//! with a deduped usage ledger.
//!
//! Behavioural coverage against the public API
//! (`meter::{Meter, MeterEvent, MeterKind, MemoryMeter}`) and files on
//! disk. The README documents the portable-core reconciliation model and
//! metric name; production sinks are not claimed to be wired.
//!
//! - The `Meter` sink stays an append-only fact log: dedup belongs to
//!   the usage ledger, never to `MemoryMeter`. Two identical
//!   `BytesServed` events must both be recorded by the meter — a
//!   redelivered play is still a fact about what the play path tried to
//!   emit, and silently dropping it here would hide double-emission
//!   bugs instead of converging at the ledger (positive control: this
//!   passes today and must keep passing).
//! - The `METER` binding to the `nntune_meter` dataset in `wrangler.toml`
//!   is the intended future Analytics Engine sink for the reconcile
//!   metric. This change emits nothing to Analytics Engine; the binding
//!   must stay declared (positive control).
//!
//! The API for idempotency-key derivation, the deduped ledger, and the
//! reconcile pass is covered in
//! `meter_reconcile_contract_acceptance.rs`. README describes the
//! portable-core model; production `play()` wiring, Hyperdrive storage,
//! Analytics Engine emission, and production log-sink wiring are not
//! asserted or represented as active here.

use std::path::PathBuf;

use nntune_stream::meter::{MemoryMeter, Meter, MeterEvent, MeterKind};

fn manifest_file(name: &str) -> String {
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn readme_documents_the_reconcile_method_and_metric_name() {
    let readme = manifest_file("README.md");
    // The reconcile pass (method) and the `play_reconcile_mismatch` name
    // it carries on mismatch: an operator must be able to go from a
    // firing alert to the code and the ledger semantics without reading
    // source. Nothing is emitted to Analytics Engine by this change.
    for needle in [
        "reconcile",
        "play_reconcile_mismatch",
        "ReconcileMismatch",
        "idempotency",
    ] {
        assert!(
            readme.contains(needle),
            "workers/stream/README.md must document {needle}"
        );
    }
}

#[test]
fn meter_stays_an_append_only_fact_log_under_redelivery() {
    // Redelivery converges at the usage ledger, not in the meter: the
    // meter records every emission verbatim (including a byte-identical
    // replay), so reconcile has the full metered history to check.
    let mut meter = MemoryMeter::default();
    let play = MeterEvent {
        kind: MeterKind::BytesServed,
        user_id: "user-1".to_string(),
        track_id: "track-9".to_string(),
        bytes: 40_000,
        lane: None,
        backoff_ms: 0,
    };
    meter.emit(play.clone());
    meter.emit(play);
    assert_eq!(meter.events.len(), 2, "meter must not dedup redeliveries");
    let metered: u64 = meter.events.iter().map(|e| e.bytes).sum();
    assert_eq!(
        metered, 80_000,
        "raw metered total double-counts a redelivery; the ledger is the dedup point"
    );
}

#[test]
fn meter_event_order_and_kinds_are_unchanged_by_the_new_variant() {
    // Adding `MeterKind::ReconcileMismatch` must not disturb the
    // ordering or the meaning of the existing billing-relevant kinds.
    let mut meter = MemoryMeter::default();
    for kind in [
        MeterKind::PlayStart,
        MeterKind::CacheMiss,
        MeterKind::BytesServed,
    ] {
        meter.emit(MeterEvent {
            kind,
            user_id: "user-1".to_string(),
            track_id: "track-9".to_string(),
            bytes: 7,
            lane: None,
            backoff_ms: 0,
        });
    }
    let kinds: Vec<MeterKind> = meter.events.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [
            MeterKind::PlayStart,
            MeterKind::CacheMiss,
            MeterKind::BytesServed
        ]
    );
    assert!(meter.has_kind(MeterKind::BytesServed));
    assert!(!meter.has_kind(MeterKind::CacheHit));
    assert!(!meter.has_kind(MeterKind::ProviderError));
}

#[test]
fn analytics_engine_meter_dataset_stays_bound() {
    // `nntune_meter` is the intended future Analytics Engine dataset for
    // the reconcile metric; nothing is emitted to it yet. `wrangler.toml`
    // must keep the `METER` binding so that wiring has a target.
    let wrangler = manifest_file("wrangler.toml");
    assert!(wrangler.contains("METER"), "METER binding is required");
    assert!(
        wrangler.contains("nntune_meter"),
        "Analytics Engine dataset is required"
    );
}
