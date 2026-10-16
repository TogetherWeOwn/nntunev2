//! Metering event types and portable-core reconciliation model. [`MemoryMeter`]
//! records events in-process for tests; production sink wiring is separate.
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
//!   [`MeterKind::ReconcileMismatch`] event and a single-line log
//!   representation from [`ReconcileMismatch::log_line`]. Production sink
//!   wiring is separate.

/// Event name for the mismatch representation emitted by [`reconcile`].
/// Wiring this event to Analytics Engine remains a production integration task.
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

fn is_valid_key_field(field: &str, max_bytes: usize) -> bool {
    !field.is_empty()
        && field.len() <= max_bytes
        && !field.contains(':')
        && !field.contains('=')
        && !field.chars().any(|ch| ch.is_whitespace() || ch.is_control())
}

/// Derive the idempotency key for one play: `play:{user}:{track}:{play}`.
///
/// Mirrors the `ingest:{user}:{sha}:{ref}` vocabulary of
/// `derive_idempotency_key` in the ingest Worker. Rejects (Err, no panic)
/// any field that is empty, over its byte limit, or contains `:`, `=`,
/// whitespace, or control characters, so keys remain unambiguous and safe
/// to emit as single-line structured logs.
pub fn derive_play_idempotency_key(
    user_id: &str,
    track_id: &str,
    play_id: &str,
) -> Result<String, &'static str> {
    if !is_valid_key_field(user_id, 128) {
        return Err("user id must be 1-128 bytes without ':', '=', whitespace, or controls");
    }
    if !is_valid_key_field(track_id, 512) {
        return Err("track id must be 1-512 bytes without ':', '=', whitespace, or controls");
    }
    if !is_valid_key_field(play_id, 512) {
        return Err("play id must be 1-512 bytes without ':', '=', whitespace, or controls");
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

fn escape_log_field(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':') {
            escaped.push(byte as char);
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

impl ReconcileMismatch {
    /// Single-line structured log for the mismatch, carrying the
    /// [`RECONCILE_MISMATCH_METRIC`] event name, the play identity, and
    /// both byte counts (`ledger_bytes=missing` when there is no entry).
    /// Identity values are percent-escaped, including when public fields
    /// were constructed without [`derive_play_idempotency_key`].
    pub fn log_line(&self) -> String {
        let ledger_bytes = match self.ledger_bytes {
            Some(bytes) => bytes.to_string(),
            None => "missing".to_string(),
        };
        format!(
            "event={} idempotency_key={} user_id={} track_id={} metered_bytes={} ledger_bytes={}",
            RECONCILE_MISMATCH_METRIC,
            escape_log_field(&self.idempotency_key),
            escape_log_field(&self.user_id),
            escape_log_field(&self.track_id),
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
/// ledger entry is missing, whose user or track differs, or whose metered
/// bytes differ from the entry's bytes, is a mismatch. Every mismatch emits
/// exactly one [`MeterKind::ReconcileMismatch`] meter event (carrying the
/// metered byte count) and produces one [`ReconcileMismatch`] record.
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

        let ledger_entry = ledger.get(&play.idempotency_key);
        let ledger_bytes = ledger_entry.map(|entry| entry.bytes);
        let is_mismatch = match ledger_entry {
            Some(entry) => {
                entry.user_id != play.user_id
                    || entry.track_id != play.track_id
                    || entry.bytes != play.bytes
            }
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

    #[test]
    fn reconcile_detects_identity_mismatch_with_same_key_and_bytes() {
        let metered = CompletePlay::new("user-1", "track-1", "play-1", 40).unwrap();
        let mut ledger = UsageLedger::default();
        ledger.record(&metered);

        let mut wrong_identity = metered.clone();
        wrong_identity.user_id = "user-2".to_string();
        let mut meter = MemoryMeter::default();
        let report = reconcile(&[wrong_identity], &ledger, &mut meter);

        assert_eq!(report.mismatches.len(), 1);
        assert_eq!(report.mismatches[0].ledger_bytes, Some(40));
        assert_eq!(meter.events.len(), 1);

        let mut wrong_track = metered.clone();
        wrong_track.track_id = "track-2".to_string();
        let report = reconcile(&[wrong_track], &ledger, &mut meter);
        assert_eq!(report.mismatches.len(), 1);
        assert_eq!(meter.events.len(), 2);
    }

    #[test]
    fn idempotency_key_fields_reject_log_breakers_and_oversized_values() {
        for invalid in ["has space", "has\t tab", "has\nnewline", "forged=value"] {
            assert!(derive_play_idempotency_key(invalid, "track", "play").is_err());
            assert!(derive_play_idempotency_key("user", invalid, "play").is_err());
            assert!(derive_play_idempotency_key("user", "track", invalid).is_err());
        }
        assert!(derive_play_idempotency_key(&"u".repeat(129), "track", "play").is_err());
        assert!(derive_play_idempotency_key("user", &"t".repeat(513), "play").is_err());
        assert!(derive_play_idempotency_key("user", "track", &"p".repeat(513)).is_err());
    }

    #[test]
    fn mismatch_log_escapes_public_unvalidated_identity_fields() {
        let mismatch = ReconcileMismatch {
            idempotency_key: "play:user\nledger_bytes=999".to_string(),
            user_id: "user with space".to_string(),
            track_id: "track=value".to_string(),
            metered_bytes: 40,
            ledger_bytes: Some(40),
        };
        let line = mismatch.log_line();
        assert!(!line.contains('\n'));
        assert!(line.contains("idempotency_key=play:user%0Aledger_bytes%3D999"));
        assert!(line.contains("user_id=user%20with%20space"));
        assert!(line.contains("track_id=track%3Dvalue"));
        assert!(line.ends_with("metered_bytes=40 ledger_bytes=40"));
    }
}
