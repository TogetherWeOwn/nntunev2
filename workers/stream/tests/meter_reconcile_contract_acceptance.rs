//! Acceptance tests for the public reconciliation contract in
//! `nntune_stream::meter`.
//!
//! This is passing portable-core coverage. It verifies key-field
//! validation and reconciliation behavior; it does not exercise
//! production wiring. No production `play()` path, Hyperdrive table,
//! Analytics Engine emission, or production log sink is currently wired.
//!
//! A *complete play* is the per-play usage record produced when a play
//! finishes serving (the `BytesServed` moment): one record carrying the
//! total bytes served for that single play. Redelivery means the same
//! record arriving again (Queue/at-least-once replay, a retried sink
//! write); it is not a second listen. A genuine second listen is a
//! different play and must bill separately.
//!
//! Implemented portable-core contract (in `meter.rs`; existing `Meter`,
//! `MeterEvent` and `MemoryMeter` signatures remain unchanged):
//!
//! - `pub const RECONCILE_MISMATCH_METRIC: &str =
//!   "play_reconcile_mismatch";` — the stable metric and structured-log
//!   event name for a mismatch. Production Analytics Engine emission is
//!   not wired.
//! - `MeterKind::ReconcileMismatch` — the `MeterEvent` kind emitted for
//!   a mismatch, with the play's `user_id`, `track_id` and
//!   `bytes` = the metered byte count.
//! - `pub fn derive_play_idempotency_key(user_id, track_id, play_id)
//!   -> Result<String, &'static str>` — returns
//!   `"play:{user_id}:{track_id}:{play_id}"`. Each key field is rejected
//!   if empty, over 128 bytes for a user ID or 512 bytes for a track or
//!   play ID, or contains `:`, `=`, whitespace, or a control character.
//! - `pub struct CompletePlay { idempotency_key, user_id, track_id,
//!   bytes: u64 }` with `CompletePlay::new(user_id, track_id, play_id,
//!   bytes) -> Result<CompletePlay, &'static str>` deriving the key.
//! - `pub struct UsageLedger` (`Default`) — the portable-core model of
//!   the Hyperdrive usage-ledger table, which in production must key
//!   `UNIQUE(idempotency_key)` exactly as the ingest library upsert
//!   converges `ON CONFLICT (idempotency_key)`:
//!   - `record(&mut self, play: &CompletePlay) -> RecordOutcome`
//!     (`Recorded` for a new key, `Duplicate` for a redelivery).
//!     First write wins: a duplicate never appends, sums into, or
//!     overwrites the existing entry — even if its byte count
//!     disagrees (that disagreement is reconcile's job to flag).
//!   - `entries(&self) -> &[LedgerEntry]` in first-record order,
//!     `get(&self, idempotency_key) -> Option<&LedgerEntry>`,
//!     `len()`, `is_empty()`.
//!   - `total_bytes_for_user(&self, user_id) -> u64` — saturating sum
//!     of that user's entries (never panics, never wraps).
//!   - `LedgerEntry { idempotency_key, user_id, track_id, bytes }`
//!     (field-for-field copy of the recorded play; `Clone`, `Debug`,
//!     `PartialEq`, `Eq`). `RecordOutcome` is `Debug`, `PartialEq`, `Eq`.
//! - `pub fn reconcile(plays: &[CompletePlay], ledger: &UsageLedger,
//!   meter: &mut dyn Meter) -> ReconcileReport` — compares each
//!   distinct play (by idempotency key, first occurrence authoritative,
//!   redeliveries checked once) against the ledger:
//!   - a play whose ledger entry is missing, or whose user, track, or
//!     byte count differs from the entry, is a mismatch;
//!   - every mismatch emits exactly one `ReconcileMismatch` meter
//!     event and produces one `ReconcileMismatch` record whose
//!     `log_line()` is a single-line structured log with fields
//!     `event=play_reconcile_mismatch`, `idempotency_key=…`,
//!     `user_id=…`, `track_id=…`, `metered_bytes=…` and
//!     `ledger_bytes=…` (`ledger_bytes=missing` when there is no entry);
//!   - the pass takes `&UsageLedger` and must never adjust, backfill
//!     or drop an entry: a mismatch is flagged, never silently fixed;
//!   - reconciliation is per-play, not per-aggregate: equal totals do
//!     not excuse swapped per-play byte counts; ledger entries with no
//!     corresponding play are out of scope (not flagged, not counted);
//!   - `ReconcileReport { checked, mismatches }` where `checked` is
//!     the number of distinct play keys examined and mismatches keep
//!     first-occurrence input order; `is_clean()` iff no mismatches.
//! - `workers/stream/README.md` documents the reconcile method, the
//!   `play_reconcile_mismatch` metric name, and the idempotency-key
//!   dedup rule ("first write wins, mismatch is flagged, never
//!   silently adjusted").
//!
//! These tests cover portable-core behavior only; they make no claim
//! that production sinks or play-path wiring are active.

use std::path::PathBuf;

use nntune_stream::meter::{
    derive_play_idempotency_key, reconcile, CompletePlay, LedgerEntry, MemoryMeter, MeterKind,
    RecordOutcome, UsageLedger, RECONCILE_MISMATCH_METRIC,
};

fn play(user: &str, track: &str, play_id: &str, bytes: u64) -> CompletePlay {
    CompletePlay::new(user, track, play_id, bytes).expect("valid play")
}

fn ledger_with(plays: &[CompletePlay]) -> UsageLedger {
    let mut ledger = UsageLedger::default();
    for p in plays {
        ledger.record(p);
    }
    ledger
}

fn mismatch_events(meter: &MemoryMeter) -> usize {
    meter
        .events
        .iter()
        .filter(|e| e.kind == MeterKind::ReconcileMismatch)
        .count()
}

// ---------------------------------------------------------------- idempotency key

#[test]
fn idempotency_key_binds_user_track_and_play() {
    let key = derive_play_idempotency_key("user-1", "track-9", "play-7").expect("key");
    assert_eq!(key, "play:user-1:track-9:play-7");
    // Deterministic: the same play redelivered derives the same key.
    assert_eq!(
        key,
        derive_play_idempotency_key("user-1", "track-9", "play-7").expect("key")
    );
    // Every bound field matters: changing any one changes the key.
    assert_ne!(
        key,
        derive_play_idempotency_key("user-2", "track-9", "play-7").expect("key")
    );
    assert_ne!(
        key,
        derive_play_idempotency_key("user-1", "track-8", "play-7").expect("key")
    );
    assert_ne!(
        key,
        derive_play_idempotency_key("user-1", "track-9", "play-8").expect("key")
    );
}

#[test]
fn idempotency_key_rejects_empty_delimiter_and_nul_fields() {
    for (user, track, play_id) in [
        ("", "track-9", "play-7"),
        ("user-1", "", "play-7"),
        ("user-1", "track-9", ""),
        // Delimiter confusion: both sides of this pair must be rejected
        // outright, never mapped onto the same joined key.
        ("a:b", "c", "d"),
        ("a", "b:c", "d"),
        ("user-1", "track-9", "play:7"),
        ("user\0", "track-9", "play-7"),
        ("user-1", "track\09", "play-7"),
        ("user-1", "track-9", "play\0"),
    ] {
        assert!(
            derive_play_idempotency_key(user, track, play_id).is_err(),
            "({user:?}, {track:?}, {play_id:?}) must be rejected"
        );
        assert!(
            CompletePlay::new(user, track, play_id, 1).is_err(),
            "CompletePlay::new must reject the same inputs"
        );
    }
}

#[test]
fn complete_play_carries_the_derived_key_and_its_fields() {
    let p = play("user-1", "track-9", "play-7", 40_000);
    assert_eq!(p.idempotency_key, "play:user-1:track-9:play-7");
    assert_eq!(p.user_id, "user-1");
    assert_eq!(p.track_id, "track-9");
    assert_eq!(p.bytes, 40_000);
}

// ---------------------------------------------------------------- ledger dedup

#[test]
fn redelivered_play_converges_to_one_ledger_entry() {
    let p = play("user-1", "track-9", "play-7", 40_000);
    let mut ledger = UsageLedger::default();
    assert!(ledger.is_empty());
    assert_eq!(ledger.record(&p), RecordOutcome::Recorded);
    // At-least-once redelivery of the identical record, repeatedly:
    assert_eq!(ledger.record(&p), RecordOutcome::Duplicate);
    assert_eq!(ledger.record(&p), RecordOutcome::Duplicate);
    assert_eq!(ledger.len(), 1, "redeliveries converge to one entry");
    assert_eq!(
        ledger.entries(),
        &[LedgerEntry {
            idempotency_key: "play:user-1:track-9:play-7".to_string(),
            user_id: "user-1".to_string(),
            track_id: "track-9".to_string(),
            bytes: 40_000,
        }]
    );
    assert_eq!(
        ledger.total_bytes_for_user("user-1"),
        40_000,
        "a redelivered play bills once, not three times"
    );
}

#[test]
fn redelivery_with_disagreeing_bytes_never_rewrites_the_entry() {
    let mut ledger = UsageLedger::default();
    ledger.record(&play("user-1", "track-9", "play-7", 40_000));
    // Same play, redelivered with a different byte count (truncated or
    // corrupted replay): first write wins; reconcile flags the drift.
    assert_eq!(
        ledger.record(&play("user-1", "track-9", "play-7", 41_500)),
        RecordOutcome::Duplicate
    );
    assert_eq!(ledger.len(), 1);
    assert_eq!(
        ledger.get("play:user-1:track-9:play-7").map(|e| e.bytes),
        Some(40_000),
        "duplicate must neither overwrite nor sum into the entry"
    );
    assert_eq!(ledger.total_bytes_for_user("user-1"), 40_000);
}

#[test]
fn distinct_plays_of_the_same_track_still_bill_separately() {
    // Dedup is per play, never per user+track: listening twice is two
    // entries and both plays count toward the user's total.
    let mut ledger = UsageLedger::default();
    assert_eq!(
        ledger.record(&play("user-1", "track-9", "play-7", 40_000)),
        RecordOutcome::Recorded
    );
    assert_eq!(
        ledger.record(&play("user-1", "track-9", "play-8", 40_000)),
        RecordOutcome::Recorded
    );
    // The same play id under another user is a different key.
    assert_eq!(
        ledger.record(&play("user-2", "track-9", "play-7", 12_000)),
        RecordOutcome::Recorded
    );
    assert_eq!(ledger.len(), 3);
    assert_eq!(ledger.total_bytes_for_user("user-1"), 80_000);
    assert_eq!(ledger.total_bytes_for_user("user-2"), 12_000);
    assert_eq!(ledger.total_bytes_for_user("nobody"), 0);
}

#[test]
fn ledger_lookup_order_and_totals_are_stable() {
    let plays = [
        play("user-1", "track-9", "play-7", 10),
        play("user-1", "track-1", "play-2", 20),
        play("user-1", "track-9", "play-7", 10), // duplicate, dropped
        play("user-1", "track-2", "play-3", 30),
    ];
    let ledger = ledger_with(&plays);
    assert_eq!(ledger.len(), 3);
    let keys: Vec<&str> = ledger
        .entries()
        .iter()
        .map(|e| e.idempotency_key.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "play:user-1:track-9:play-7",
            "play:user-1:track-1:play-2",
            "play:user-1:track-2:play-3"
        ],
        "entries keep first-record order"
    );
    assert!(ledger.get("play:user-1:track-1:play-2").is_some());
    assert!(ledger.get("play:user-1:track-1:play-99").is_none());
    assert!(ledger.get("").is_none());

    // Saturating totals: two maximal entries must not panic or wrap.
    let huge = ledger_with(&[
        play("user-9", "track-1", "play-1", u64::MAX),
        play("user-9", "track-1", "play-2", u64::MAX),
    ]);
    assert_eq!(huge.total_bytes_for_user("user-9"), u64::MAX);
}

// ---------------------------------------------------------------- reconcile: clean

#[test]
fn reconcile_is_clean_when_every_play_matches_the_ledger() {
    let plays = [
        play("user-1", "track-9", "play-7", 40_000),
        play("user-1", "track-2", "play-8", 12_345),
        play("user-2", "track-9", "play-1", 40_000),
    ];
    let ledger = ledger_with(&plays);
    let mut meter = MemoryMeter::default();
    let report = reconcile(&plays, &ledger, &mut meter);
    assert_eq!(report.checked, 3);
    assert!(report.mismatches.is_empty());
    assert!(report.is_clean());
    assert_eq!(
        mismatch_events(&meter),
        0,
        "no metric may fire on a clean pass"
    );
    assert!(meter.events.is_empty());
}

#[test]
fn reconcile_of_nothing_is_clean_and_silent() {
    let ledger = UsageLedger::default();
    let mut meter = MemoryMeter::default();
    let report = reconcile(&[], &ledger, &mut meter);
    assert_eq!(report.checked, 0);
    assert!(report.is_clean());
    assert!(meter.events.is_empty());
}

// ---------------------------------------------------------------- reconcile: flagged

#[test]
fn byte_mismatch_is_flagged_with_metric_and_log_and_never_adjusted() {
    // Ledger recorded 40_000 for the play; the metered play now says
    // 39_999. The pass flags it — it must not "fix" either side.
    let metered = [play("user-1", "track-9", "play-7", 39_999)];
    let ledger = ledger_with(&[play("user-1", "track-9", "play-7", 40_000)]);
    let before: Vec<LedgerEntry> = ledger.entries().to_vec();

    let mut meter = MemoryMeter::default();
    let report = reconcile(&metered, &ledger, &mut meter);

    assert_eq!(report.checked, 1);
    assert!(!report.is_clean());
    assert_eq!(report.mismatches.len(), 1);
    let m = &report.mismatches[0];
    assert_eq!(m.idempotency_key, "play:user-1:track-9:play-7");
    assert_eq!(m.user_id, "user-1");
    assert_eq!(m.track_id, "track-9");
    assert_eq!(m.metered_bytes, 39_999);
    assert_eq!(m.ledger_bytes, Some(40_000));

    // Exactly one metric event, carrying the metered byte count.
    assert_eq!(mismatch_events(&meter), 1);
    let ev = &meter.events[0];
    assert_eq!(ev.kind, MeterKind::ReconcileMismatch);
    assert_eq!(ev.user_id, "user-1");
    assert_eq!(ev.track_id, "track-9");
    assert_eq!(ev.bytes, 39_999);

    // Structured log: one line, named event, both byte counts, the key.
    let line = m.log_line();
    assert!(!line.contains('\n'), "log line must be a single line");
    for needle in [
        "event=play_reconcile_mismatch",
        "idempotency_key=play:user-1:track-9:play-7",
        "user_id=user-1",
        "track_id=track-9",
        "metered_bytes=39999",
        "ledger_bytes=40000",
    ] {
        assert!(line.contains(needle), "log line missing {needle}: {line}");
    }

    // No silent adjust: the ledger is byte-for-byte what it was.
    assert_eq!(ledger.entries(), before.as_slice());
    assert_eq!(ledger.total_bytes_for_user("user-1"), 40_000);
}

#[test]
fn missing_ledger_entry_is_flagged_missing_and_never_backfilled() {
    let metered = [play("user-3", "track-5", "play-1", 8_192)];
    let ledger = UsageLedger::default();
    let mut meter = MemoryMeter::default();
    let report = reconcile(&metered, &ledger, &mut meter);
    assert_eq!(report.checked, 1);
    assert_eq!(report.mismatches.len(), 1);
    assert_eq!(report.mismatches[0].ledger_bytes, None);
    assert!(report.mismatches[0]
        .log_line()
        .contains("ledger_bytes=missing"));
    assert_eq!(mismatch_events(&meter), 1);
    assert!(
        ledger.is_empty(),
        "reconcile must not backfill the missing entry"
    );
}

#[test]
fn reconcile_is_per_play_not_per_aggregate() {
    // Totals agree (300 = 300) but the per-play bytes are swapped:
    // an aggregate-only check would wave this through.
    let metered = [
        play("user-1", "track-a", "play-1", 100),
        play("user-1", "track-b", "play-2", 200),
    ];
    let ledger = ledger_with(&[
        play("user-1", "track-a", "play-1", 200),
        play("user-1", "track-b", "play-2", 100),
    ]);
    let mut meter = MemoryMeter::default();
    let report = reconcile(&metered, &ledger, &mut meter);
    assert_eq!(report.checked, 2);
    assert_eq!(report.mismatches.len(), 2);
    assert_eq!(mismatch_events(&meter), 2);
    assert_eq!(ledger.total_bytes_for_user("user-1"), 300);
}

#[test]
fn reconcile_checks_each_redelivery_once_in_input_order() {
    // The metered input itself can contain redeliveries: each play is
    // checked (and, on mismatch, reported) exactly once, in
    // first-occurrence order; the first occurrence's bytes are the
    // authoritative metered value.
    let metered = [
        play("user-1", "track-b", "play-2", 200), // missing from ledger
        play("user-1", "track-a", "play-1", 100), // matches
        play("user-1", "track-b", "play-2", 999), // redelivery, ignored
        play("user-1", "track-c", "play-3", 300), // byte mismatch
        play("user-1", "track-a", "play-1", 555), // redelivery, ignored
    ];
    let ledger = ledger_with(&[
        play("user-1", "track-a", "play-1", 100),
        play("user-1", "track-c", "play-3", 301),
        play("user-1", "track-z", "play-9", 1), // no metered play: out of scope
    ]);
    let mut meter = MemoryMeter::default();
    let report = reconcile(&metered, &ledger, &mut meter);
    assert_eq!(report.checked, 3, "distinct play keys, not input rows");
    let flagged: Vec<&str> = report
        .mismatches
        .iter()
        .map(|m| m.idempotency_key.as_str())
        .collect();
    assert_eq!(
        flagged,
        ["play:user-1:track-b:play-2", "play:user-1:track-c:play-3"],
        "one record per mismatched play, first-occurrence order"
    );
    assert_eq!(report.mismatches[0].metered_bytes, 200);
    assert_eq!(mismatch_events(&meter), 2, "one metric per mismatched play");
    assert_eq!(ledger.len(), 3, "ledger untouched, extra entry included");
}

#[test]
fn reconcile_flags_only_the_disagreeing_user() {
    // Same track, two users, same nominal size: a per-user key mix-up
    // must surface on the wrong user's play only.
    let metered = [
        play("user-1", "track-9", "play-7", 40_000),
        play("user-2", "track-9", "play-7", 40_000),
    ];
    let ledger = ledger_with(&[play("user-1", "track-9", "play-7", 40_000)]);
    let mut meter = MemoryMeter::default();
    let report = reconcile(&metered, &ledger, &mut meter);
    assert_eq!(report.checked, 2);
    assert_eq!(report.mismatches.len(), 1);
    assert_eq!(report.mismatches[0].user_id, "user-2");
    assert_eq!(meter.events[0].user_id, "user-2");
}

// ---------------------------------------------------------------- names + docs

#[test]
fn metric_name_is_the_documented_stable_string() {
    assert_eq!(RECONCILE_MISMATCH_METRIC, "play_reconcile_mismatch");
}

#[test]
fn readme_documents_reconcile_method_metric_and_dedup_rule() {
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let readme =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    for needle in [
        "reconcile",
        "play_reconcile_mismatch",
        "ReconcileMismatch",
        "idempotency",
    ] {
        assert!(readme.contains(needle), "README must document {needle}");
    }
}
