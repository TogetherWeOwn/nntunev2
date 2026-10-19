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
//!   `stream_play_ledger` table (`migrations/0020_stream_play_ledger.sql`,
//!   NOT the ADR daily `usage_ledger`), which in production must key
//!   `UNIQUE(idempotency_key)` exactly as the ingest library upsert
//!   converges `ON CONFLICT (idempotency_key)`. [`UsageLedger::record`]
//!   converges exact redeliveries to one entry, first write wins, and
//!   reports [`RecordOutcome::Conflict`] instead of converging when a
//!   redelivery disagrees or the key does not bind its identity fields.
//!   The production sink is an account-owned writer, so it must also
//!   carry the ADR 0002 rule 3 deletion fence: [`LEDGER_WRITE_FENCE_SQL`]
//!   is the in-statement fence its INSERT must select through, and
//!   [`UsageLedger::record_fenced`] is the portable-core mirror (a
//!   fenced play reports [`RecordOutcome::AccountDeleting`] and appends
//!   nothing, so a play can never repopulate a deleting account's
//!   ledger/play-event rows).
//! - [`reconcile`] compares each distinct metered play against the ledger
//!   and flags (never silently adjusts) every mismatch with one
//!   [`MeterKind::ReconcileMismatch`] event and a single-line log
//!   representation from [`ReconcileMismatch::log_line`]. A redelivery
//!   inside the same batch that conflicts with its first occurrence is
//!   flagged as its own mismatch. Mismatch events carry `bytes = 0`
//!   (they serve nothing; counts stay on the [`ReconcileMismatch`]
//!   record). Production sink wiring is separate.

/// Event name for the mismatch representation emitted by [`reconcile`].
/// Wiring this event to Analytics Engine remains a production integration task.
pub const RECONCILE_MISMATCH_METRIC: &str = "play_reconcile_mismatch";

use crate::pool::Lane;

/// Analytics Engine binding name: must match `[analytics_engine_datasets]`
/// in `workers/stream/wrangler.toml`.
pub const METER_DATASET_BINDING: &str = "METER";
/// Analytics Engine dataset receiving stream usage events.
pub const METER_DATASET: &str = "nntune_meter";

/// Nominal bytes per stream second: served bytes map to stream seconds at
/// this rate (roughly 384 kbit/s lossless-ish audio). This is a metering
/// primitive, not a bitrate claim about any track: it lets byte counts and
/// second counts stay mutually derivable without a per-track bitrate lookup.
/// The ledger side derives seconds from bytes with this same rate.
pub const STREAM_BYTES_PER_SEC: u64 = 48_000;

/// Stream seconds attributed to `bytes` served, rounding any partial second
/// up: a started second counts. Zero bytes attribute zero seconds.
pub fn stream_seconds_for_bytes(bytes: u64) -> u64 {
    bytes.div_ceil(STREAM_BYTES_PER_SEC)
}

/// What happened, in billing-relevant order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeterKind {
    PlayStart,
    CacheHit,
    CacheMiss,
    BytesServed,
    PrefetchEnqueued,
    ProviderError,
    /// A play was rejected because its grant had expired
    /// (`GrantError::Expired`). Carries the requested track and the granted
    /// user only; never secret or token material.
    GrantRejectedExpired,
    /// A play was rejected because its grant's remaining lifetime exceeded
    /// `MAX_GRANT_TTL + CLOCK_SKEW_SECS` (`GrantError::TtlTooLong`).
    /// Same payload shape as the other rejection events.
    GrantRejectedExcessiveExpiry,
    /// A play was rejected because a valid grant was presented for the
    /// wrong track (`PlayError::WrongTrack`). `track_id` is the requested
    /// track, not the granted one.
    GrantRejectedWrongTrack,
    /// A provider-level failure quarantined the pool; the event carries the
    /// failed call's lane and the backoff just applied.
    QuarantineEntered,
    /// A successful call cleared a pending quarantine (no backoff).
    QuarantineExited,
    /// A pooled connection the provider silently closed was transparently
    /// redialed (no backoff, no quarantine).
    WarmRedial,
    /// A flagged reconciliation mismatch. Non-serving: its [`MeterEvent`]
    /// always carries `bytes = 0`; the flagged counts stay on the
    /// [`ReconcileMismatch`] record returned by [`reconcile`].
    ReconcileMismatch,
}

/// Stable Analytics Engine blob for a [`MeterKind`]. Distinct kinds never
/// share a slug.
pub fn kind_slug(kind: MeterKind) -> &'static str {
    match kind {
        MeterKind::PlayStart => "play_start",
        MeterKind::CacheHit => "cache_hit",
        MeterKind::CacheMiss => "cache_miss",
        MeterKind::BytesServed => "bytes_served",
        MeterKind::PrefetchEnqueued => "prefetch_enqueued",
        MeterKind::ProviderError => "provider_error",
        MeterKind::GrantRejectedExpired => "grant_rejected_expired",
        MeterKind::GrantRejectedExcessiveExpiry => "grant_rejected_excessive_expiry",
        MeterKind::GrantRejectedWrongTrack => "grant_rejected_wrong_track",
        MeterKind::QuarantineEntered => "quarantine_entered",
        MeterKind::QuarantineExited => "quarantine_exited",
        MeterKind::WarmRedial => "warm_redial",
        MeterKind::ReconcileMismatch => "play_reconcile_mismatch",
    }
}

/// One billable-or-debuggable fact about a play.
#[derive(Clone, Debug)]
pub struct MeterEvent {
    pub kind: MeterKind,
    pub user_id: String,
    pub track_id: String,
    /// Bytes served to the client (0 for non-serving events).
    pub bytes: u64,
    /// Stream seconds attributed ([`stream_seconds_for_bytes`]); 0 for
    /// non-serving events.
    pub seconds: u64,
    /// Pool lane of the call a pool-health event belongs to; `None` for
    /// events unrelated to pool state.
    pub lane: Option<Lane>,
    /// Quarantine backoff in milliseconds for
    /// [`MeterKind::QuarantineEntered`]; 0 otherwise.
    pub backoff_ms: u64,
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
        && field
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Derive the idempotency key for one play: `play:{user}:{track}:{play}`.
///
/// Mirrors the `ingest:{user}:{sha}:{ref}` vocabulary of
/// `derive_idempotency_key` in the ingest Worker. Each field must be non-empty,
/// within its byte limit, and contain only ASCII alphanumerics, `-`, or `_`,
/// matching the persisted ledger key alphabet. Rejects invalid fields (Err,
/// no panic), without rewriting identities; `:` is only a structural delimiter.
pub fn derive_play_idempotency_key(
    user_id: &str,
    track_id: &str,
    play_id: &str,
) -> Result<String, &'static str> {
    if !is_valid_key_field(user_id, 128) {
        return Err("user id must be 1-128 bytes of ASCII alphanumerics, '-', or '_'");
    }
    if !is_valid_key_field(track_id, 512) {
        return Err("track id must be 1-512 bytes of ASCII alphanumerics, '-', or '_'");
    }
    if !is_valid_key_field(play_id, 512) {
        return Err("play id must be 1-512 bytes of ASCII alphanumerics, '-', or '_'");
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
    /// The play's account is being deleted (`users.deleting_at` is set),
    /// so the fenced ledger write was refused and nothing was appended
    /// (ADR 0002 rule 3). Only [`UsageLedger::record_fenced`] reports
    /// this; the stable wire/job code is `account_deleting`.
    AccountDeleting,
    /// The play was not converged: either its idempotency key does not
    /// bind its `(user_id, track_id)` identity fields (the key must be
    /// exactly `play:{user_id}:{track_id}:{play_id}` with valid fields),
    /// or a ledger entry already exists for the key with a different
    /// user, track, or byte count. A conflict never appends, sums into,
    /// or overwrites the existing entry.
    Conflict,
}

/// The idempotency key must bind the play's identity: exactly
/// `play:{user_id}:{track_id}:{play_id}` where every field passes
/// [`is_valid_key_field`] with the same limits as
/// [`derive_play_idempotency_key`]. [`CompletePlay`] exposes public
/// fields, so a directly constructed value can bypass
/// [`CompletePlay::new`]; the ledger re-checks the invariant here so
/// the dedup point never accepts a key the derivation function would
/// reject.
fn key_binds_identity(play: &CompletePlay) -> bool {
    if !is_valid_key_field(&play.user_id, 128) || !is_valid_key_field(&play.track_id, 512) {
        return false;
    }
    let Some(rest) = play.idempotency_key.strip_prefix("play:") else {
        return false;
    };
    let mut parts = rest.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(user), Some(track), Some(play_id), None) => {
            user == play.user_id && track == play.track_id && is_valid_key_field(play_id, 512)
        }
        _ => false,
    }
}

/// The account-deletion fence the production ledger INSERT must select
/// its row through (ADR 0002 rule 3): bind the play owner's user id to
/// `$1`; the derived `writer_fence` row exists only while that account's
/// `deleting_at IS NULL`, and the `FOR SHARE` lock is held until the
/// INSERT commits, so the scheduling `UPDATE users` either waits for
/// the play-event/ledger write or fences it. A fenced INSERT affects 0
/// rows and must be classified as `account_deleting`, never retried
/// behind the delete. Zero rows is ambiguous under `DO NOTHING`, though:
/// a duplicate redelivery with an open fence also affects 0 rows. The
/// production sink must converge with `DO UPDATE` (like the ingest
/// library upsert) or probe the fence first, so healthy duplicates never
/// read as `account_deleting`.
pub const LEDGER_WRITE_FENCE_SQL: &str =
    "(SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence";

/// Portable-core model of the Hyperdrive `stream_play_ledger` table.
///
/// Production must key `UNIQUE(idempotency_key)` exactly as the ingest
/// library upsert converges `ON CONFLICT (idempotency_key)`. A zero-row
/// `DO NOTHING` insert is ambiguous: the api Worker's
/// `ledger_select_by_key_statement` + `classify_replay` separate exact
/// redeliveries (`Duplicate`) from conflicting payloads (`Conflict`).
#[derive(Default, Debug)]
pub struct UsageLedger {
    entries: Vec<LedgerEntry>,
}

impl UsageLedger {
    /// Record a complete play. A new idempotency key appends one entry
    /// ([`RecordOutcome::Recorded`]); an exact redelivery converges to
    /// the existing entry ([`RecordOutcome::Duplicate`]).
    ///
    /// First write wins: a duplicate never appends, sums into, or
    /// overwrites the existing entry. A redelivery whose user, track, or
    /// byte count disagrees with the entry, or a play whose key does not
    /// bind its identity fields, reports [`RecordOutcome::Conflict`] and
    /// likewise changes nothing (that disagreement is [`reconcile`]'s
    /// job to flag).
    pub fn record(&mut self, play: &CompletePlay) -> RecordOutcome {
        if !key_binds_identity(play) {
            return RecordOutcome::Conflict;
        }
        if let Some(existing) = self
            .entries
            .iter()
            .find(|e| e.idempotency_key == play.idempotency_key)
        {
            if existing.user_id == play.user_id
                && existing.track_id == play.track_id
                && existing.bytes == play.bytes
            {
                return RecordOutcome::Duplicate;
            }
            return RecordOutcome::Conflict;
        }
        self.entries.push(LedgerEntry {
            idempotency_key: play.idempotency_key.clone(),
            user_id: play.user_id.clone(),
            track_id: play.track_id.clone(),
            bytes: play.bytes,
        });
        RecordOutcome::Recorded
    }

    /// Record a complete play through the account-deletion fence.
    ///
    /// `account_deleting` is the fence state the production sink obtains
    /// from [`LEDGER_WRITE_FENCE_SQL`] in the same statement as its
    /// INSERT (`true` when the fence row is absent because
    /// `users.deleting_at` is set). A fenced play is refused before any
    /// identity/dedup work and appends nothing — the mirror of the
    /// production INSERT affecting 0 rows — so neither a first play nor
    /// a redelivery can repopulate the ledger behind the marker. With
    /// the fence open this is exactly [`UsageLedger::record`].
    pub fn record_fenced(&mut self, play: &CompletePlay, account_deleting: bool) -> RecordOutcome {
        if account_deleting {
            return RecordOutcome::AccountDeleting;
        }
        self.record(play)
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
/// Each distinct play (by idempotency key, first occurrence authoritative)
/// is compared against the ledger: a play whose ledger entry is missing,
/// whose user or track differs, or whose metered bytes differ from the
/// entry's bytes, is a mismatch. A redelivery inside the same batch whose
/// user, track, or bytes conflict with its first occurrence is flagged as
/// its own mismatch, so a truncated or corrupted replay cannot hide behind
/// the first observation. Every mismatch emits exactly one
/// [`MeterKind::ReconcileMismatch`] meter event (non-serving: `bytes = 0`)
/// and produces one [`ReconcileMismatch`] record carrying the metered and
/// ledger byte counts.
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
    let mut first_seen: Vec<&CompletePlay> = Vec::with_capacity(plays.len());
    let mut mismatches: Vec<ReconcileMismatch> = Vec::new();

    // Emit one mismatch record + event for `play`. `ledger_bytes` is the
    // ledger entry for the key when one exists.
    let flag = |play: &CompletePlay,
                ledger_bytes: Option<u64>,
                meter: &mut dyn Meter,
                mismatches: &mut Vec<ReconcileMismatch>| {
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
            bytes: 0,
            seconds: 0,
            lane: None,
            backoff_ms: 0,
        });
    };

    for play in plays {
        if let Some(first) = first_seen
            .iter()
            .find(|seen| seen.idempotency_key == play.idempotency_key)
        {
            if first.user_id != play.user_id
                || first.track_id != play.track_id
                || first.bytes != play.bytes
            {
                let ledger_bytes = ledger.get(&play.idempotency_key).map(|entry| entry.bytes);
                flag(play, ledger_bytes, meter, &mut mismatches);
            }
            continue;
        }
        first_seen.push(play);

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

        flag(play, ledger_bytes, meter, &mut mismatches);
    }

    ReconcileReport {
        checked: first_seen.len(),
        mismatches,
    }
}

/// One Analytics Engine point: blobs carry identity, doubles carry
/// magnitude, the index is the billed user. `doubles` holds
/// `[bytes, seconds]`; both stay exactly representable for every realistic
/// play (magnitudes far below 2^53).
#[derive(Clone, Debug, PartialEq)]
pub struct AnalyticsPoint {
    pub dataset: &'static str,
    pub index: String,
    pub blobs: [String; 3],
    pub doubles: [f64; 2],
}

/// Build the [`AnalyticsPoint`] for one event. Blobs are
/// `[kind, user_id, track_id]`; the index is the user.
pub fn analytics_point(event: &MeterEvent) -> AnalyticsPoint {
    AnalyticsPoint {
        dataset: METER_DATASET,
        index: event.user_id.clone(),
        blobs: [
            kind_slug(event.kind).to_string(),
            event.user_id.clone(),
            event.track_id.clone(),
        ],
        doubles: [event.bytes as f64, event.seconds as f64],
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
                seconds: stream_seconds_for_bytes(7),
                lane: None,
                backoff_ms: 0,
            });
        }
        assert_eq!(meter.events.len(), 2);
        assert!(meter
            .events
            .iter()
            .all(|e| e.lane.is_none() && e.backoff_ms == 0));
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
        // "bell\u{7}char" and "nul\0char" are control characters that are not
        // whitespace, so they pin the control-character arm on its own.
        for invalid in [
            "has space",
            "has\t tab",
            "has\nnewline",
            "forged=value",
            "bell\u{7}char",
            "nul\0char",
        ] {
            assert!(derive_play_idempotency_key(invalid, "track", "play").is_err());
            assert!(derive_play_idempotency_key("user", invalid, "play").is_err());
            assert!(derive_play_idempotency_key("user", "track", invalid).is_err());
        }
        assert!(derive_play_idempotency_key(&"u".repeat(129), "track", "play").is_err());
        assert!(derive_play_idempotency_key("user", &"t".repeat(513), "play").is_err());
        assert!(derive_play_idempotency_key("user", "track", &"p".repeat(513)).is_err());
    }

    #[test]
    fn record_conflicts_on_disagreeing_redelivery_without_rewriting() {
        let mut ledger = UsageLedger::default();
        assert_eq!(
            ledger.record(&CompletePlay::new("user-1", "track-9", "play-7", 40_000).unwrap()),
            RecordOutcome::Recorded
        );
        assert_eq!(
            ledger.record(&CompletePlay::new("user-1", "track-9", "play-7", 41_500).unwrap()),
            RecordOutcome::Conflict
        );
        assert_eq!(ledger.len(), 1);
        assert_eq!(
            ledger.get("play:user-1:track-9:play-7").map(|e| e.bytes),
            Some(40_000),
            "conflict must neither overwrite nor sum into the entry"
        );
        assert_eq!(ledger.total_bytes_for_user("user-1"), 40_000);
    }

    #[test]
    fn record_conflicts_when_the_key_does_not_bind_its_identity() {
        // Forged key: binds alice's identity but bills mallory.
        let forged = CompletePlay {
            idempotency_key: "play:alice:t:1".to_string(),
            user_id: "mallory".to_string(),
            track_id: "t".to_string(),
            bytes: 5,
        };
        let mut ledger = UsageLedger::default();
        assert_eq!(ledger.record(&forged), RecordOutcome::Conflict);
        assert!(ledger.is_empty());
        assert_eq!(ledger.total_bytes_for_user("mallory"), 0);

        // Empty key/user/track is not a billable play either.
        let empty = CompletePlay {
            idempotency_key: String::new(),
            user_id: String::new(),
            track_id: String::new(),
            bytes: 5,
        };
        assert_eq!(ledger.record(&empty), RecordOutcome::Conflict);
        assert!(ledger.is_empty());
    }

    #[test]
    fn fenced_record_refuses_deleting_accounts_and_records_normal_plays() {
        let play = CompletePlay::new("user-1", "track-9", "play-7", 40_000).unwrap();
        let mut ledger = UsageLedger::default();
        // Fence open: identical to record, including dedup convergence.
        assert_eq!(ledger.record_fenced(&play, false), RecordOutcome::Recorded);
        assert_eq!(ledger.record_fenced(&play, false), RecordOutcome::Duplicate);
        assert_eq!(ledger.len(), 1);
        // Fence closed: a later play for a new key appends nothing and
        // does not disturb the entry recorded before scheduling.
        let later = CompletePlay::new("user-1", "track-9", "play-8", 40_000).unwrap();
        assert_eq!(
            ledger.record_fenced(&later, true),
            RecordOutcome::AccountDeleting
        );
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.total_bytes_for_user("user-1"), 40_000);
        // A fenced redelivery is refused as deleting, never converged
        // as a duplicate: the fence runs before identity/dedup.
        assert_eq!(
            ledger.record_fenced(&play, true),
            RecordOutcome::AccountDeleting
        );
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn ledger_write_fence_sql_locks_only_a_non_deleting_owner_row() {
        assert_eq!(
            LEDGER_WRITE_FENCE_SQL,
            "(SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence"
        );
        assert!(!LEDGER_WRITE_FENCE_SQL.contains("FOR UPDATE"));
        assert!(!LEDGER_WRITE_FENCE_SQL.contains("NOWAIT"));
        assert!(!LEDGER_WRITE_FENCE_SQL.contains("SKIP LOCKED"));
        assert!(!LEDGER_WRITE_FENCE_SQL.contains("IS NOT NULL"));
    }

    #[test]
    fn reconcile_flags_a_conflicting_redelivery_in_the_same_batch() {
        // Same identity fields, different bytes: the derived keys match,
        // so the second play is a conflicting redelivery of the first.
        let first = CompletePlay::new("user-1", "track-9", "play-7", 40_000).unwrap();
        let replay = CompletePlay::new("user-1", "track-9", "play-7", 41_500).unwrap();
        debug_assert_eq!(first.idempotency_key, replay.idempotency_key);
        let ledger = {
            let mut ledger = UsageLedger::default();
            ledger.record(&first);
            ledger
        };
        let mut meter = MemoryMeter::default();
        let report = reconcile(&[first, replay], &ledger, &mut meter);
        assert_eq!(report.checked, 1);
        assert_eq!(report.mismatches.len(), 1);
        assert_eq!(report.mismatches[0].metered_bytes, 41_500);
        assert_eq!(report.mismatches[0].ledger_bytes, Some(40_000));
        assert_eq!(meter.events.len(), 1);
        assert_eq!(meter.events[0].bytes, 0);
    }

    #[test]
    fn mismatch_events_carry_zero_bytes() {
        let metered = CompletePlay::new("user-1", "track-9", "play-7", 39_999).unwrap();
        let mut ledger = UsageLedger::default();
        ledger.record(&CompletePlay::new("user-1", "track-9", "play-7", 40_000).unwrap());
        let mut meter = MemoryMeter::default();
        let report = reconcile(std::slice::from_ref(&metered), &ledger, &mut meter);
        assert_eq!(report.mismatches.len(), 1);
        assert_eq!(report.mismatches[0].metered_bytes, 39_999);
        assert_eq!(meter.events.len(), 1);
        assert_eq!(meter.events[0].kind, MeterKind::ReconcileMismatch);
        assert_eq!(
            meter.events[0].bytes, 0,
            "non-serving mismatch events must not inflate byte sums"
        );
    }

    #[test]
    fn idempotency_key_fields_accept_exact_byte_limits() {
        let user = "u".repeat(128);
        let track = "t".repeat(512);
        let play_id = "p".repeat(512);
        let key = derive_play_idempotency_key(&user, &track, &play_id).unwrap();
        assert_eq!(key, format!("play:{user}:{track}:{play_id}"));

        // Non-ASCII identities are refused even at the byte limit:
        // 64 two-byte chars are exactly 128 bytes, 65 are 130.
        assert!(derive_play_idempotency_key(&"\u{e9}".repeat(64), "t", "p").is_err());
        assert!(derive_play_idempotency_key(&"\u{e9}".repeat(65), "t", "p").is_err());

        // The ledger accepts exact-limit fields too, so its identity re-check
        // uses the same inclusive limits as the derivation.
        let play = CompletePlay::new(&user, &track, &play_id, 1).unwrap();
        let mut ledger = UsageLedger::default();
        assert_eq!(ledger.record(&play), RecordOutcome::Recorded);
        assert_eq!(ledger.record(&play), RecordOutcome::Duplicate);
        assert_eq!(ledger.len(), 1);
    }

    fn forged(key: &str, user: &str, track: &str) -> CompletePlay {
        CompletePlay {
            idempotency_key: key.to_string(),
            user_id: user.to_string(),
            track_id: track.to_string(),
            bytes: 5,
        }
    }

    #[test]
    fn record_conflicts_on_each_forged_identity_arm() {
        let long_user = "u".repeat(129);
        let long_track = "t".repeat(513);
        let long_play = "p".repeat(513);
        // Every case fails exactly one arm of the identity re-check: the key
        // matches the play's fields everywhere except the arm named.
        let cases = [
            (
                "user differs from key",
                forged("play:alice:t:1", "mallory", "t"),
            ),
            ("track differs from key", forged("play:u:t:1", "u", "other")),
            ("empty user", forged("play::t:1", "", "t")),
            ("whitespace user", forged("play:a b:t:1", "a b", "t")),
            ("'=' user", forged("play:a=b:t:1", "a=b", "t")),
            ("control user", forged("play:a\u{7}b:t:1", "a\u{7}b", "t")),
            (
                "oversized user",
                forged(&format!("play:{long_user}:t:1"), &long_user, "t"),
            ),
            ("empty track", forged("play:u::1", "u", "")),
            ("whitespace track", forged("play:u:a b:1", "u", "a b")),
            ("'=' track", forged("play:u:a=b:1", "u", "a=b")),
            (
                "oversized track",
                forged(&format!("play:u:{long_track}:1"), "u", &long_track),
            ),
            ("empty play id", forged("play:u:t:", "u", "t")),
            ("whitespace play id", forged("play:u:t:p 1", "u", "t")),
            ("'=' play id", forged("play:u:t:p=1", "u", "t")),
            ("newline play id", forged("play:u:t:p\n1", "u", "t")),
            (
                "oversized play id",
                forged(&format!("play:u:t:{long_play}"), "u", "t"),
            ),
            ("wrong prefix", forged("pay:u:t:1", "u", "t")),
            ("missing play id segment", forged("play:u:t", "u", "t")),
            ("extra segment", forged("play:u:t:1:2", "u", "t")),
        ];
        for (label, play) in cases {
            let mut ledger = UsageLedger::default();
            assert_eq!(ledger.record(&play), RecordOutcome::Conflict, "{label}");
            assert!(ledger.is_empty(), "{label}");
            assert_eq!(ledger.total_bytes_for_user(&play.user_id), 0, "{label}");
        }
    }

    #[test]
    fn reconcile_flags_a_forged_same_key_redelivery_with_equal_bytes() {
        let first = CompletePlay::new("user-1", "track-9", "play-7", 40_000).unwrap();
        let mut ledger = UsageLedger::default();
        ledger.record(&first);

        // Public fields let a redelivery keep the key and the bytes while
        // changing only who is billed or which track it is attributed to.
        let mut other_user = first.clone();
        other_user.user_id = "user-2".to_string();
        let mut other_track = first.clone();
        other_track.track_id = "track-8".to_string();

        for (label, replay) in [("user", other_user), ("track", other_track)] {
            let mut meter = MemoryMeter::default();
            let report = reconcile(&[first.clone(), replay.clone()], &ledger, &mut meter);
            assert_eq!(report.checked, 1, "{label}");
            assert_eq!(report.mismatches.len(), 1, "{label}");
            assert_eq!(report.mismatches[0].user_id, replay.user_id, "{label}");
            assert_eq!(report.mismatches[0].track_id, replay.track_id, "{label}");
            assert_eq!(report.mismatches[0].metered_bytes, 40_000, "{label}");
            assert_eq!(report.mismatches[0].ledger_bytes, Some(40_000), "{label}");
            assert_eq!(meter.events.len(), 1, "{label}");
            assert_eq!(meter.events[0].bytes, 0, "{label}");
        }
    }

    #[test]
    fn reconcile_does_not_flag_an_agreeing_redelivery_in_the_same_batch() {
        let play = CompletePlay::new("user-1", "track-9", "play-7", 40_000).unwrap();

        // Matching ledger entry: at-least-once redelivery of the same play
        // is expected and must reconcile clean.
        let mut ledger = UsageLedger::default();
        ledger.record(&play);
        let mut meter = MemoryMeter::default();
        let report = reconcile(&[play.clone(), play.clone()], &ledger, &mut meter);
        assert_eq!(report.checked, 1);
        assert!(report.is_clean());
        assert!(meter.events.is_empty());

        // Missing ledger entry: the first occurrence is flagged once; its
        // agreeing redelivery must not be flagged a second time.
        let empty = UsageLedger::default();
        let mut meter = MemoryMeter::default();
        let report = reconcile(&[play.clone(), play.clone(), play], &empty, &mut meter);
        assert_eq!(report.checked, 1);
        assert_eq!(report.mismatches.len(), 1);
        assert_eq!(report.mismatches[0].ledger_bytes, None);
        assert_eq!(meter.events.len(), 1);
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

    #[test]
    fn dataset_names_match_wrangler() {
        assert_eq!(METER_DATASET_BINDING, "METER");
        assert_eq!(METER_DATASET, "nntune_meter");
    }

    #[test]
    fn nominal_rate_is_pinned() {
        // Literal pin (not in terms of the constant): changing the rate
        // re-bills every historical byte count, so it must be deliberate.
        assert_eq!(STREAM_BYTES_PER_SEC, 48_000);
    }

    #[test]
    fn seconds_vectors() {
        assert_eq!(stream_seconds_for_bytes(0), 0);
        assert_eq!(stream_seconds_for_bytes(1), 1);
        assert_eq!(stream_seconds_for_bytes(47_999), 1);
        assert_eq!(stream_seconds_for_bytes(48_000), 1);
        assert_eq!(stream_seconds_for_bytes(48_001), 2);
        assert_eq!(stream_seconds_for_bytes(96_000), 2);
        assert_eq!(stream_seconds_for_bytes(96_001), 3);
        // No overflow at the top: div_ceil never adds before dividing.
        // u64::MAX = 18_446_744_073_709_551_615 = 48_000 * 384_307_168_202_282
        // + 41_615, so the ceiling is one past the quotient.
        assert_eq!(stream_seconds_for_bytes(u64::MAX), 384_307_168_202_283);
    }

    #[test]
    fn kind_slugs_are_distinct_snake_case() {
        let slugs = [
            kind_slug(MeterKind::PlayStart),
            kind_slug(MeterKind::CacheHit),
            kind_slug(MeterKind::CacheMiss),
            kind_slug(MeterKind::BytesServed),
            kind_slug(MeterKind::PrefetchEnqueued),
            kind_slug(MeterKind::ProviderError),
            kind_slug(MeterKind::GrantRejectedExpired),
            kind_slug(MeterKind::GrantRejectedExcessiveExpiry),
            kind_slug(MeterKind::GrantRejectedWrongTrack),
            kind_slug(MeterKind::QuarantineEntered),
            kind_slug(MeterKind::QuarantineExited),
            kind_slug(MeterKind::WarmRedial),
            kind_slug(MeterKind::ReconcileMismatch),
        ];
        assert_eq!(
            slugs,
            [
                "play_start",
                "cache_hit",
                "cache_miss",
                "bytes_served",
                "prefetch_enqueued",
                "provider_error",
                "grant_rejected_expired",
                "grant_rejected_excessive_expiry",
                "grant_rejected_wrong_track",
                "quarantine_entered",
                "quarantine_exited",
                "warm_redial",
                "play_reconcile_mismatch"
            ]
        );
        let mut distinct = slugs.to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), slugs.len());
    }

    #[test]
    fn analytics_point_carries_identity_in_blobs_and_magnitude_in_doubles() {
        let event = MeterEvent {
            kind: MeterKind::BytesServed,
            user_id: "user-1".to_string(),
            track_id: "track-9".to_string(),
            bytes: 100_000,
            seconds: stream_seconds_for_bytes(100_000),
            lane: None,
            backoff_ms: 0,
        };
        let point = analytics_point(&event);
        assert_eq!(point.dataset, "nntune_meter");
        assert_eq!(point.index, "user-1");
        assert_eq!(
            point.blobs,
            [
                "bytes_served".to_string(),
                "user-1".to_string(),
                "track-9".to_string()
            ]
        );
        assert_eq!(point.doubles, [100_000.0, 3.0]);
    }

    #[test]
    fn two_plays_on_one_track_bill_independently() {
        // There is no cross-play (user, track) fold: each play carries its
        // own byte total (`CompletePlay.bytes`, fed from the play's own
        // `served` counter), and the ledger converges per idempotency key.
        // Summing both plays' bytes into play-7's row would bill play-7 for
        // both listens and bill (or conflict) play-8 on top.
        let play_7 = CompletePlay::new("user-1", "track-9", "play-7", 40_000).unwrap();
        let play_8 = CompletePlay::new("user-1", "track-9", "play-8", 75_000).unwrap();
        assert_ne!(play_7.idempotency_key, play_8.idempotency_key);
        let mut ledger = UsageLedger::default();
        assert_eq!(ledger.record(&play_7), RecordOutcome::Recorded);
        assert_eq!(ledger.record(&play_8), RecordOutcome::Recorded);
        assert_eq!(ledger.len(), 2);
        assert_eq!(
            ledger.get(&play_7.idempotency_key).map(|e| e.bytes),
            Some(40_000),
            "play-7 bills only its own bytes, never the cross-play sum"
        );
        assert_eq!(
            ledger.get(&play_8.idempotency_key).map(|e| e.bytes),
            Some(75_000)
        );
        assert_eq!(ledger.total_bytes_for_user("user-1"), 115_000);
        // A redelivery of play-7 converges to play-7 alone and leaves
        // play-8 untouched.
        assert_eq!(ledger.record(&play_7), RecordOutcome::Duplicate);
        assert_eq!(ledger.len(), 2);
        assert_eq!(
            ledger.get(&play_8.idempotency_key).map(|e| e.bytes),
            Some(75_000)
        );
    }

    // Deterministic stand-in for a property-test RNG (this crate stays
    // dependency-free): xorshift64* over a fixed seed.
    fn rng_next(state: &mut u64) -> u64 {
        *state ^= *state >> 12;
        *state ^= *state << 25;
        *state ^= *state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    #[test]
    fn seconds_are_the_ceiling_monotone_and_split_bounded() {
        let mut state = 0x1234_5678_9ABC_DEF1u64;
        for i in 0usize..5_000 {
            let bytes = if i.is_multiple_of(7) {
                // Edge strata near multiples of the rate.
                48_000 * (rng_next(&mut state) % 5) + rng_next(&mut state) % 3
            } else {
                rng_next(&mut state) % 20_000_000
            };
            let seconds = stream_seconds_for_bytes(bytes);
            // Ceiling: the attributed seconds cover the bytes, and one fewer
            // second would not (for any positive byte count).
            assert!(seconds * STREAM_BYTES_PER_SEC >= bytes, "bytes {bytes}");
            if bytes > 0 {
                assert!(
                    seconds
                        .saturating_sub(1)
                        .saturating_mul(STREAM_BYTES_PER_SEC)
                        < bytes,
                    "bytes {bytes}"
                );
            } else {
                assert_eq!(seconds, 0);
            }
        }
        let mut sorted: Vec<u64> = {
            let mut s = 0xABCDu64;
            (0..2_000).map(|_| rng_next(&mut s) % 50_000_000).collect()
        };
        sorted.sort_unstable();
        let mut last = 0u64;
        for bytes in sorted {
            let seconds = stream_seconds_for_bytes(bytes);
            assert!(seconds >= last, "monotone at {bytes}");
            last = seconds;
        }
        // Split bound: serving a..b in two events attributes at most one
        // extra second versus serving a+b at once, never fewer.
        let mut s2 = 0x99u64;
        for _ in 0..2_000 {
            let a = rng_next(&mut s2) % 5_000_000;
            let b = rng_next(&mut s2) % 5_000_000;
            let whole = stream_seconds_for_bytes(a.saturating_add(b));
            let split = stream_seconds_for_bytes(a).saturating_add(stream_seconds_for_bytes(b));
            assert!(split >= whole, "split {a}+{b}");
            assert!(split <= whole.saturating_add(1), "split {a}+{b}");
        }
    }

    #[test]
    fn doubles_stay_exact_for_realistic_plays() {
        // Every byte count a real play can serve is far below 2^53, so the
        // Analytics Engine doubles round-trip exactly.
        let mut state = 42u64;
        for _ in 0..1_000 {
            let bytes = rng_next(&mut state) % 10_737_418_240;
            let event = MeterEvent {
                kind: MeterKind::BytesServed,
                user_id: "u".to_string(),
                track_id: "t".to_string(),
                bytes,
                seconds: stream_seconds_for_bytes(bytes),
                lane: None,
                backoff_ms: 0,
            };
            let point = analytics_point(&event);
            assert_eq!(point.doubles[0] as u64, bytes);
            assert_eq!(point.doubles[1] as u64, event.seconds);
        }
    }
}
