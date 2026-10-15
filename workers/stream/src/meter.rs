//! Metering: every play emits events for Analytics Engine and the usage
//! ledger (plan enforcement). [`MemoryMeter`] records them in-process for
//! tests; production sinks them to the `meter` Analytics Engine dataset and
//! the Hyperdrive usage-ledger table with the same shapes.
//!
//! Complete-play reconciliation with a deduped usage ledger:
//!
//! - [`derive_play_idempotency_key`] binds `(user_id, track_id, play_id)`
//!   into a `play:{user}:{track}:{play}` key. A redelivered play derives
//!   the same key; a genuine second listen carries a different `play_id`
//!   and bills separately.
//! - [`UsageLedger`] is the portable-core model of the Hyperdrive
//!   usage-ledger table, which in production must key
//!   `UNIQUE(idempotency_key)` exactly as the ingest library upsert
//!   converges `ON CONFLICT (idempotency_key)`. [`UsageLedger::record`]
//!   converges redeliveries to one entry, first write wins.
//! - [`reconcile`] compares each distinct metered play against the ledger
//!   and flags (never silently adjusts) every mismatch with one
//!   [`MeterKind::ReconcileMismatch`] event on the existing `Meter` sink
//!   (`METER` / `nntune_meter`) plus a single-line structured log from
//!   [`ReconcileMismatch::log_line`] under the
//!   [`RECONCILE_MISMATCH_METRIC`] event name.

/// Stable Analytics Engine metric name (and structured-log event name)
/// emitted once per mismatched play by [`reconcile`]. It rides the
/// existing `Meter` trait → `METER` / `nntune_meter` sink; no new dataset.
pub const RECONCILE_MISMATCH_METRIC: &str = "play_reconcile_mismatch";

/// What happened, in billing-relevant order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeterKind {
    PlayStart,
    CacheHit,
    CacheMiss,
    BytesServed,
    PrefetchEnqueued,
    ProviderError,
    ReconcileMismatch,
}

/// One billable-or-debuggable fact about a play.
#[derive(Clone, Debug)]
pub struct MeterEvent {
    pub kind: MeterKind,
    pub user_id: String,
    pub track_id: String,
    /// Bytes served to the client (0 for non-serving events).
    pub bytes: u64,
}

pub trait Meter {
    fn emit(&mut self, event: MeterEvent);
}

/// In-process meter for tests.
#[derive(Default, Debug)]
pub struct MemoryMeter {
    pub events: Vec<MeterEvent>,
}

impl Meter for MemoryMeter {
    fn emit(&mut self, event: MeterEvent) {
        self.events.push(event);
    }
}

impl MemoryMeter {
    pub fn has_kind(&self, kind: MeterKind) -> bool {
        self.events.iter().any(|e| e.kind == kind)
    }
}

fn is_valid_key_field(field: &str) -> bool {
    !field.is_empty() && !field.contains(':') && !field.contains('\0')
}

/// Derive the idempotency key for one play: `play:{user}:{track}:{play}`.
///
/// Mirrors the `ingest:{user}:{sha}:{ref}` vocabulary of
/// `derive_idempotency_key` in the ingest Worker. Rejects (Err, no panic)
/// any field that is empty or contains `:` or NUL, so two different
/// `(user, track, play)` triples can never format to the same key.
pub fn derive_play_idempotency_key(
    user_id: &str,
    track_id: &str,
    play_id: &str,
) -> Result<String, &'static str> {
    if !is_valid_key_field(user_id) {
        return Err("user id must be non-empty and must not contain ':' or NUL");
    }
    if !is_valid_key_field(track_id) {
        return Err("track id must be non-empty and must not contain ':' or NUL");
    }
    if !is_valid_key_field(play_id) {
        return Err("play id must be non-empty and must not contain ':' or NUL");
    }
    Ok(format!("play:{user_id}:{track_id}:{play_id}"))
}

/// One complete play: the per-play usage record produced when a play
/// finishes serving (the `BytesServed` moment), carrying the total bytes
/// served for that single play.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletePlay {
    pub idempotency_key: String,
    pub user_id: String,
    pub track_id: String,
    pub bytes: u64,
}

impl CompletePlay {
    pub fn new(
        user_id: &str,
        track_id: &str,
        play_id: &str,
        bytes: u64,
    ) -> Result<Self, &'static str> {
        let idempotency_key = derive_play_idempotency_key(user_id, track_id, play_id)?;
        Ok(Self {
            idempotency_key,
            user_id: user_id.to_string(),
            track_id: track_id.to_string(),
            bytes,
        })
    }
}

/// One converged usage-ledger row: a field-for-field copy of the first
/// recorded play for its idempotency key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    pub idempotency_key: String,
    pub user_id: String,
    pub track_id: String,
    pub bytes: u64,
}

/// Outcome of [`UsageLedger::record`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordOutcome {
    Recorded,
    Duplicate,
}

/// Portable-core model of the Hyperdrive usage-ledger table.
///
/// Production must key `UNIQUE(idempotency_key)` exactly as the ingest
/// library upsert converges `ON CONFLICT (idempotency_key)`.
#[derive(Default, Debug)]
pub struct UsageLedger {
    entries: Vec<LedgerEntry>,
}

impl UsageLedger {
    /// Record a complete play. A new idempotency key appends one entry
    /// ([`RecordOutcome::Recorded`]); a redelivery converges to the
    /// existing entry ([`RecordOutcome::Duplicate`]).
    ///
    /// First write wins: a duplicate never appends, sums into, or
    /// overwrites the existing entry — even if its byte count disagrees
    /// (that disagreement is [`reconcile`]'s job to flag).
    pub fn record(&mut self, play: &CompletePlay) -> RecordOutcome {
        if self
            .entries
            .iter()
            .any(|e| e.idempotency_key == play.idempotency_key)
        {
            return RecordOutcome::Duplicate;
        }
        self.entries.push(LedgerEntry {
            idempotency_key: play.idempotency_key.clone(),
            user_id: play.user_id.clone(),
            track_id: play.track_id.clone(),
            bytes: play.bytes,
        });
        RecordOutcome::Recorded
    }

    /// Ledger rows in first-record order.
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    pub fn get(&self, idempotency_key: &str) -> Option<&LedgerEntry> {
        self.entries
            .iter()
            .find(|e| e.idempotency_key == idempotency_key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Saturating sum of one user's entries (never panics, never wraps).
    pub fn total_bytes_for_user(&self, user_id: &str) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.user_id == user_id)
            .fold(0u64, |acc, e| acc.saturating_add(e.bytes))
    }
}

/// One flagged per-play mismatch from [`reconcile`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconcileMismatch {
    pub idempotency_key: String,
    pub user_id: String,
    pub track_id: String,
    pub metered_bytes: u64,
    pub ledger_bytes: Option<u64>,
}

impl ReconcileMismatch {
    /// Single-line structured log for the mismatch, carrying the
    /// [`RECONCILE_MISMATCH_METRIC`] event name, the play identity, and
    /// both byte counts (`ledger_bytes=missing` when there is no entry).
    pub fn log_line(&self) -> String {
        let ledger_bytes = match self.ledger_bytes {
            Some(bytes) => bytes.to_string(),
            None => "missing".to_string(),
        };
        format!(
            "event={} idempotency_key={} user_id={} track_id={} metered_bytes={} ledger_bytes={}",
            RECONCILE_MISMATCH_METRIC,
            self.idempotency_key,
            self.user_id,
            self.track_id,
            self.metered_bytes,
            ledger_bytes,
        )
    }
}

/// Result of one [`reconcile`] pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Number of distinct play keys examined.
    pub checked: usize,
    /// One record per mismatched play, in first-occurrence input order.
    pub mismatches: Vec<ReconcileMismatch>,
}

impl ReconcileReport {
    pub fn is_clean(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Reconcile metered complete plays against the usage ledger.
///
/// Each distinct play (by idempotency key, first occurrence authoritative,
/// redeliveries checked once) is compared against the ledger: a play whose
/// ledger entry is missing, or whose metered bytes differ from the entry's
/// bytes, is a mismatch. Every mismatch emits exactly one
/// [`MeterKind::ReconcileMismatch`] meter event (carrying the metered byte
/// count) and produces one [`ReconcileMismatch`] record.
///
/// The pass takes `&UsageLedger` and never adjusts, backfills, or drops an
/// entry: a mismatch is flagged, never silently fixed. Reconciliation is
/// per-play, not per-aggregate: equal totals do not excuse swapped
/// per-play byte counts. Ledger entries with no corresponding play are out
/// of scope (not flagged, not counted).
pub fn reconcile(
    plays: &[CompletePlay],
    ledger: &UsageLedger,
    meter: &mut dyn Meter,
) -> ReconcileReport {
    let mut seen: Vec<&str> = Vec::with_capacity(plays.len());
    let mut mismatches: Vec<ReconcileMismatch> = Vec::new();

    for play in plays {
        if seen.iter().any(|key| *key == play.idempotency_key) {
            continue;
        }
        seen.push(play.idempotency_key.as_str());

        let ledger_bytes = ledger.get(&play.idempotency_key).map(|e| e.bytes);
        let is_mismatch = match ledger_bytes {
            Some(entry_bytes) => entry_bytes != play.bytes,
            None => true,
        };
        if !is_mismatch {
            continue;
        }

        mismatches.push(ReconcileMismatch {
            idempotency_key: play.idempotency_key.clone(),
            user_id: play.user_id.clone(),
            track_id: play.track_id.clone(),
            metered_bytes: play.bytes,
            ledger_bytes,
        });
        meter.emit(MeterEvent {
            kind: MeterKind::ReconcileMismatch,
            user_id: play.user_id.clone(),
            track_id: play.track_id.clone(),
            bytes: play.bytes,
        });
    }

    ReconcileReport {
        checked: seen.len(),
        mismatches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitted_events_are_recorded_in_order() {
        let mut meter = MemoryMeter::default();
        for kind in [MeterKind::PlayStart, MeterKind::BytesServed] {
            meter.emit(MeterEvent {
                kind,
                user_id: "u".to_string(),
                track_id: "t".to_string(),
                bytes: 7,
            });
        }
        assert_eq!(meter.events.len(), 2);
        assert!(meter.has_kind(MeterKind::PlayStart));
        assert!(!meter.has_kind(MeterKind::CacheHit));
    }
}
