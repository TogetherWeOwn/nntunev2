//! Authoritative per-play stream ledger: one row per billed play window.
//!
//! Analytics Engine carries the fast per-second/per-byte signal (see the
//! stream worker's metering); this table is the billing truth the usage
//! endpoint and limit enforcement read. Pure intents plus the parameterized
//! statements the Worker entry binds against PlanetScale Postgres via
//! Hyperdrive — never string-interpolated SQL.
//!
//! Table identity (Paperclip Review `6508688`, warning 1): this per-play
//! table is named `stream_play_ledger`
//! (`migrations/0020_stream_play_ledger.sql`), distinct from the former
//! daily `usage_ledger` aggregate. The ADR revision of 2026-11-03 retired
//! that daily fold (dropped in migration 0026): monthly stream usage is
//! the direct indexed `SUM` over this table, and the daily ingest count
//! derives from accepted `ingest_jobs` rows. See `docs/adr/0002-data-model.md`,
//! `0004-plan-matrix.md` and `0005-billing.md`.
//!
//! Rules:
//! - One [`LedgerRow`] per billed play window: (`user_id`, `track_id`,
//!   `idempotency_key`, `bytes_served`, `stream_seconds`, `recorded_at`).
//!   Play-window seconds are derived by [`derive_stream_seconds`] from the
//!   play's delivered-byte union as a proportion of the track
//!   (`ceil(union_bytes * duration / file_size)`, clamped to
//!   `duration + 2 s`), falling back to the nominal
//!   [`STREAM_BYTES_PER_SEC`] rate only when duration or file size is
//!   unknown. Until migration 0029 the nominal rate was the only
//!   derivation and the schema enforced it; it over-counts FLAC roughly
//!   2-3.7x, so [`prepare_ledger_row`] now bounds caller-derived seconds
//!   instead of recomputing the rate, and [`prepare_play_window`] is the
//!   sink path that derives them here. Seconds always derive from the
//!   window's folded byte total, never by summing per-event seconds
//!   (each event rounds up, so per-event seconds are not additive).
//! - `period_month` (`YYYY-MM`, UTC) derives deterministically from
//!   `recorded_at`; monthly usage is `SUM` over (`user_id`, `period_month`).
//! - Replay safety: the insert is fenced `ON CONFLICT (idempotency_key)
//!   DO NOTHING` through the ADR 0002 rule-3 writer fence (the play owner's
//!   `users` row `FOR SHARE`), so a play finalized behind the account-deletion
//!   marker inserts nothing. Stream plays MUST derive the
//!   key deterministically with [`derive_play_idempotency_key`] from the same
//!   `(user_id, track_id, play_id)` identity the stream worker uses — a
//!   redelivery derives the same key, a genuine second listen carries a
//!   different `play_id` and bills separately. Fresh random handles from
//!   [`new_idempotency_key`] remain for non-play windows; they MUST be minted
//!   once per window and reused across retries (a new random key per retry
//!   cannot deduplicate). A zero-row insert is ambiguous: the Worker MUST
//!   `SELECT` the existing row with [`ledger_select_by_key_statement`], probe
//!   the fence, and run [`classify_fenced_replay`] — exact redelivery reports
//!   [`ReplayOutcome::Duplicate`], any user/track/byte/second disagreement (or
//!   a `play:` key that does not bind its identity) reports
//!   [`ReplayOutcome::Conflict`], and no stored row with a closed fence reports
//!   [`ReplayOutcome::AccountDeleting`] (typed 409 `account_deleting`, never
//!   retried). Conflicts are flagged, never silently converged.
//! - Bounds: a single row carries at most [`MAX_BYTES_PER_ROW`] bytes and
//!   [`MAX_STREAM_SECONDS_PER_ROW`] seconds — decoupled sanity caps since
//!   migration 0029 (the byte cap keeps its historical value); zero-byte
//!   and zero-second rows are rejected — an empty row bills nothing and
//!   only widens the table, and no derivation of positive bytes yields
//!   zero seconds (migration 0030 mirrors the floor in the schema).

use std::collections::BTreeMap;

/// Nominal bytes per stream second: the fallback derivation rate when a
/// play's duration or file size is unknown ([`derive_stream_seconds`]).
/// Historically the only derivation (enforced by migration 0020's rate
/// CHECK, dropped in 0029); the stream worker's legacy meter pins the same
/// literal, so a drift fails the suite on either side instead of silently
/// re-billing history.
pub const STREAM_BYTES_PER_SEC: u64 = 48_000;
/// Maximum stream seconds one row may attribute (24 h of audio).
pub const MAX_STREAM_SECONDS_PER_ROW: u64 = 86_400;
/// Maximum bytes one row may carry. A sanity cap since migration 0029;
/// the value is historical (24 h at the old nominal rate) and no longer
/// coupled to the derivation.
pub const MAX_BYTES_PER_ROW: u64 = 4_147_200_000;
/// Last Unix second representable by the persisted four-digit `YYYY-MM` month.
pub const MAX_RECORDED_AT: u64 = 253_402_300_799;
/// Maximum length of user handles and fresh random keys (chars).
pub const MAX_ID_LEN: usize = 128;
/// Maximum user-id length (chars for handles, bytes for key fields; ASCII —
/// the only alphabet real ids use — is identical in both).
pub const MAX_USER_ID_LEN: usize = 128;
/// Maximum track-id length. Matches the stream worker's 512-byte limit so a
/// stream-accepted track is never rejected here.
pub const MAX_TRACK_ID_LEN: usize = 512;
/// Maximum play-id length. Matches the stream worker's 512-byte limit.
pub const MAX_PLAY_ID_LEN: usize = 512;
/// Maximum idempotency-key length: `play:` + 128-byte user + `:` + 512-byte
/// track + `:` + 512-byte play id = 1159. Covers every stream-derived key
/// and every 22-char fresh random handle.
pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 1159;

/// Why a ledger row was rejected. Failures stay generic: inputs are never
/// echoed (they may carry caller-chosen handles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerError {
    /// Shape violation (empty/overlong handle, bad key charset, zero or
    /// oversized bytes, oversized seconds, play key not binding
    /// its identity): HTTP 422.
    Invalid,
    /// Secure randomness unavailable while minting a fresh idempotency key:
    /// HTTP 503. Callers map this to the existing `service_unavailable`
    /// shape (retryable); never a panic, fallback, or reused key.
    RandomnessUnavailable,
}

impl LedgerError {
    pub fn http_status(&self) -> u16 {
        match self {
            LedgerError::Invalid => 422,
            LedgerError::RandomnessUnavailable => 503,
        }
    }
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Invalid => write!(f, "invalid usage row"),
            LedgerError::RandomnessUnavailable => write!(f, "secure randomness unavailable"),
        }
    }
}

/// Fresh 128-bit idempotency key (base64url, 22 chars, via the shared
/// [`crate::b64`] helper). Never sequential. For NON-PLAY windows only: mint
/// once per billed service window and reuse the returned key for every
/// finalize retry, so `ON CONFLICT (idempotency_key) DO NOTHING` can
/// deduplicate retries. Stream plays MUST use
/// [`derive_play_idempotency_key`] instead — a fresh random key per retry
/// cannot deduplicate a redelivery.
///
/// Fail-closed: secure-randomness failure returns
/// [`LedgerError::RandomnessUnavailable`] (HTTP 503) so callers emit the
/// existing `service_unavailable` shape. Never a panic, never a fallback
/// key, never a reused key. The success path is byte-identical: the same 16
/// entropy bytes through the same [`crate::b64::encode`].
pub fn new_idempotency_key() -> Result<String, LedgerError> {
    let bytes = crate::random::secure_random_bytes::<16>()
        .map_err(|_| LedgerError::RandomnessUnavailable)?;
    Ok(crate::b64::encode(&bytes))
}

/// Byte-based key-field check mirroring the stream worker's
/// `is_valid_key_field` exactly: non-empty, at most `max_bytes` bytes, and
/// containing only ASCII alphanumerics, `-`, or `_`, matching the persisted
/// ledger key alphabet. Colons delimit fields; identities are never rewritten.
fn is_valid_key_field(field: &str, max_bytes: usize) -> bool {
    !field.is_empty()
        && field.len() <= max_bytes
        && field
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Derive the idempotency key for one stream play: `play:{user}:{track}:{play}`.
///
/// Byte-for-byte the stream worker's `derive_play_idempotency_key`
/// (`workers/stream/src/meter.rs`): same `play:` vocabulary, same 128/512/512
/// byte limits, same rejections. A redelivered play derives the same key; a
/// genuine second listen carries a different `play_id` and bills separately.
/// The feed-through test pins the shared vectors, so either side drifting
/// fails the suite instead of silently unbilled plays.
pub fn derive_play_idempotency_key(
    user_id: &str,
    track_id: &str,
    play_id: &str,
) -> Result<String, LedgerError> {
    if !is_valid_key_field(user_id, MAX_USER_ID_LEN) {
        return Err(LedgerError::Invalid);
    }
    if !is_valid_key_field(track_id, MAX_TRACK_ID_LEN) {
        return Err(LedgerError::Invalid);
    }
    if !is_valid_key_field(play_id, MAX_PLAY_ID_LEN) {
        return Err(LedgerError::Invalid);
    }
    Ok(format!("play:{user_id}:{track_id}:{play_id}"))
}

/// The `play:` key must bind the play's identity: exactly
/// `play:{user_id}:{track_id}:{play_id}` with the same field limits as
/// [`derive_play_idempotency_key`]. [`LedgerRow`] exposes public fields, so a
/// directly constructed value can bypass derivation; `prepare` re-checks here
/// so the dedup point never accepts a key the derivation would reject.
fn key_binds_identity(idempotency_key: &str, user_id: &str, track_id: &str) -> bool {
    if !is_valid_key_field(user_id, MAX_USER_ID_LEN)
        || !is_valid_key_field(track_id, MAX_TRACK_ID_LEN)
    {
        return false;
    }
    let Some(rest) = idempotency_key.strip_prefix("play:") else {
        return false;
    };
    let mut parts = rest.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(user), Some(track), Some(play_id), None) => {
            user == user_id && track == track_id && is_valid_key_field(play_id, MAX_PLAY_ID_LEN)
        }
        _ => false,
    }
}

/// Calendar month (`YYYY-MM`, UTC) containing Unix timestamp `recorded_at`.
/// Dependency-free civil-date conversion (Hinnant's algorithm). Persisted
/// timestamps are bounded by [`MAX_RECORDED_AT`] so the resulting year fits the
/// migration's four-digit `period_month` constraint.
pub fn period_month_for_timestamp(recorded_at: u64) -> String {
    let (year, month, _) = civil_from_days((recorded_at / 86_400) as i64);
    format!("{year:04}-{month:02}")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Inverse of [`civil_from_days`] (Hinnant's algorithm): days since the Unix
/// epoch for a civil date. Only ever called with day 1 of a valid month.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// First instant (Unix seconds, UTC) of the calendar month after the one
/// containing `now_unix` — the stream-hour meter's reset instant (the
/// period is the UTC calendar month, deliberately not the billing
/// anniversary; ADR 0004). An instant exactly at a month start resets at
/// the *next* month start.
pub fn next_month_start_unix(now_unix: u64) -> u64 {
    let (year, month, _) = civil_from_days((now_unix / 86_400) as i64);
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    (days_from_civil(next_year, next_month, 1) * 86_400) as u64
}

/// `YYYY-MM-DDTHH:MM:SSZ` rendering of a Unix-seconds instant (UTC), for
/// the billing/usage DTOs. Dependency-free, same civil conversion as
/// [`period_month_for_timestamp`].
pub fn iso8601_utc(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

/// Validated ledger intent. The Worker binds it to
/// [`ledger_insert_statement`]. Never serialized to clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerRow {
    pub idempotency_key: String,
    pub user_id: String,
    pub track_id: String,
    pub period_month: String,
    pub stream_seconds: u64,
    pub bytes_served: u64,
    pub recorded_at: u64,
}

fn check_user_handle(value: &str) -> Result<String, LedgerError> {
    if value.is_empty() || value.chars().count() > MAX_USER_ID_LEN {
        return Err(LedgerError::Invalid);
    }
    Ok(value.to_string())
}

fn check_track_handle(value: &str) -> Result<String, LedgerError> {
    if value.is_empty() || value.chars().count() > MAX_TRACK_ID_LEN {
        return Err(LedgerError::Invalid);
    }
    Ok(value.to_string())
}

fn check_key(value: &str) -> Result<String, LedgerError> {
    if value.is_empty() || value.chars().count() > MAX_IDEMPOTENCY_KEY_LEN {
        return Err(LedgerError::Invalid);
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b':')
    {
        return Err(LedgerError::Invalid);
    }
    Ok(value.to_string())
}

/// Build the ledger intent for one billed service window. The caller
/// derives `stream_seconds` (play windows via [`derive_stream_seconds`]);
/// this gate bounds it to `[1, MAX_STREAM_SECONDS_PER_ROW]` rather than
/// recomputing a derivation it lacks the duration/file inputs for. The
/// floor restores the retired rate equality's implicit guarantee: every
/// derivation bills a positive byte count at least one second, so a
/// zero-second row can never be a faithful derivation of its bytes (and
/// migration 0030 enforces the same floor in the schema).
/// Until migration 0029 it also required the nominal-rate equality
/// `seconds == ceil(bytes / STREAM_BYTES_PER_SEC)`; that coupling is
/// retired with the rate CHECK it mirrored.
///
/// Keys: fresh [`new_idempotency_key`] handles are accepted as-is; a key
/// starting with `play:` MUST bind `(user_id, track_id)` per
/// [`derive_play_idempotency_key`] (forged or truncated play keys fail
/// closed). Stream plays should prefer [`prepare_play_ledger_row`], which
/// carries the `play_id` explicitly and derives the key.
pub fn prepare_ledger_row(
    user_id: &str,
    track_id: &str,
    idempotency_key: &str,
    bytes_served: u64,
    stream_seconds: u64,
    recorded_at: u64,
) -> Result<LedgerRow, LedgerError> {
    let user_id = check_user_handle(user_id)?;
    let track_id = check_track_handle(track_id)?;
    let idempotency_key = check_key(idempotency_key)?;
    if idempotency_key.starts_with("play:")
        && !key_binds_identity(&idempotency_key, &user_id, &track_id)
    {
        return Err(LedgerError::Invalid);
    }
    if bytes_served == 0 || bytes_served > MAX_BYTES_PER_ROW {
        return Err(LedgerError::Invalid);
    }
    if stream_seconds == 0 || stream_seconds > MAX_STREAM_SECONDS_PER_ROW {
        return Err(LedgerError::Invalid);
    }
    if recorded_at > MAX_RECORDED_AT {
        return Err(LedgerError::Invalid);
    }
    Ok(LedgerRow {
        period_month: period_month_for_timestamp(recorded_at),
        idempotency_key,
        user_id,
        track_id,
        stream_seconds,
        bytes_served,
        recorded_at,
    })
}

/// Build the ledger intent for one stream play, carrying its `play_id`.
///
/// Derives the idempotency key deterministically with
/// [`derive_play_idempotency_key`] from `(user_id, track_id, play_id)` — the
/// same derivation the stream worker's `CompletePlay::new` uses — then runs
/// [`prepare_ledger_row`]. A redelivery with the same play id derives the
/// same key and converges via `ON CONFLICT DO NOTHING` +
/// [`classify_replay`]; a genuine second listen carries a different play id
/// and bills as its own row. The stream worker MUST fold that play's bytes
/// only (never a cross-play `(user, track)` sum) before calling here:
/// per-event seconds round up, so the billed seconds derive from the play's
/// folded byte total.
pub fn prepare_play_ledger_row(
    user_id: &str,
    track_id: &str,
    play_id: &str,
    bytes_served: u64,
    stream_seconds: u64,
    recorded_at: u64,
) -> Result<LedgerRow, LedgerError> {
    let idempotency_key = derive_play_idempotency_key(user_id, track_id, play_id)?;
    prepare_ledger_row(
        user_id,
        track_id,
        &idempotency_key,
        bytes_served,
        stream_seconds,
        recorded_at,
    )
}

/// Outcome of [`derive_stream_seconds`]: the billed seconds and whether
/// the nominal-rate fallback produced them (the caller flags the metric;
/// a fallback row's snapshot columns stay NULL so it is recomputable as
/// `ceil(bytes / STREAM_BYTES_PER_SEC)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecondsDerivation {
    pub seconds: u64,
    pub rate_fallback: bool,
}

/// Derive billed seconds for one play from its delivered-byte union.
///
/// With both inputs known the seconds are the delivered proportion of
/// the track, `ceil(union_bytes * duration / file_size)`, clamped to
/// `[0, duration_s + 2]` — a full play is exact by construction, a
/// seek-heavy play never exceeds the track's duration plus the 2 s
/// tolerance, and re-delivered spans add nothing because the union, not
/// the playhead, is measured. The multiply runs in `u128`, so no
/// in-domain input overflows. Without a usable duration or file size
/// the legacy nominal rate applies (`ceil(union / STREAM_BYTES_PER_SEC)`)
/// and `rate_fallback` is set. Either path is additionally clamped to
/// [`MAX_STREAM_SECONDS_PER_ROW`].
pub fn derive_stream_seconds(
    union_bytes: u64,
    file_bytes: Option<u64>,
    duration_ms: Option<u64>,
) -> SecondsDerivation {
    if let (Some(file_bytes), Some(duration_ms)) = (file_bytes, duration_ms) {
        if file_bytes > 0 && duration_ms > 0 {
            let numerator = u128::from(union_bytes) * u128::from(duration_ms);
            let denominator = u128::from(file_bytes) * 1_000;
            let duration_s = duration_ms.div_ceil(1_000);
            // Clamp in u128 before the cast: an out-of-domain union must
            // saturate at the clamp, never truncate into a small value.
            let cap = u128::from(duration_s.saturating_add(2))
                .min(u128::from(MAX_STREAM_SECONDS_PER_ROW));
            let seconds = numerator.div_ceil(denominator).min(cap) as u64;
            return SecondsDerivation {
                seconds,
                rate_fallback: false,
            };
        }
    }
    SecondsDerivation {
        seconds: union_bytes
            .div_ceil(STREAM_BYTES_PER_SEC)
            .min(MAX_STREAM_SECONDS_PER_ROW),
        rate_fallback: true,
    }
}

/// Resolve a track's duration for [`derive_stream_seconds`] from the
/// metadata hierarchy: the track's own `duration_ms`, then the
/// MusicBrainz recording's, then a bitrate estimate
/// (`file_bytes * 8 / bitrate_kbps` milliseconds — `track_media.bitrate`
/// is kbit/s). Zero/unknown rungs are skipped; `None` leaves the caller
/// on the nominal-rate fallback.
pub fn resolve_duration_ms(
    track_duration_ms: Option<u64>,
    mb_duration_ms: Option<u64>,
    bitrate_kbps: Option<u64>,
    file_bytes: Option<u64>,
) -> Option<u64> {
    if let Some(ms) = track_duration_ms.filter(|ms| *ms > 0) {
        return Some(ms);
    }
    if let Some(ms) = mb_duration_ms.filter(|ms| *ms > 0) {
        return Some(ms);
    }
    let (kbps, bytes) = (bitrate_kbps?, file_bytes?);
    if kbps == 0 || bytes == 0 {
        return None;
    }
    let estimated = (u128::from(bytes) * 8 / u128::from(kbps)) as u64;
    (estimated > 0).then_some(estimated)
}

/// Validated play-window intent for the meter sink: one play's
/// delivered-byte union and the seconds derived from it, plus the
/// `file_bytes` / `duration_ms` snapshots the derivation consumed
/// (migration 0029 columns; `None` when the nominal-rate fallback
/// derived the seconds). The Worker binds it to
/// [`ledger_play_window_upsert_statement`]. Never serialized to clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayWindow {
    pub idempotency_key: String,
    pub user_id: String,
    pub track_id: String,
    pub period_month: String,
    pub stream_seconds: u64,
    pub bytes_served: u64,
    pub file_bytes: Option<u64>,
    pub duration_ms: Option<u64>,
    /// True when the nominal-rate fallback derived `stream_seconds`;
    /// drives the fallback metric, never persisted.
    pub rate_fallback: bool,
    pub recorded_at: u64,
}

/// Build the play-window intent for one play's cumulative delivered
/// union. Derives the idempotency key like [`prepare_play_ledger_row`]
/// and the seconds via [`derive_stream_seconds`], so client-reported
/// play time is never an input. `union_bytes` is the union total so far
/// (monotonic per play): the upsert converges redeliveries of the same
/// or an older snapshot to a no-op. Zero union, a zero file size, or a
/// zero duration are inconsistent inputs and fail closed — resolve
/// unknowns to `None` (via [`resolve_duration_ms`]) instead.
pub fn prepare_play_window(
    user_id: &str,
    track_id: &str,
    play_id: &str,
    union_bytes: u64,
    file_bytes: Option<u64>,
    duration_ms: Option<u64>,
    recorded_at: u64,
) -> Result<PlayWindow, LedgerError> {
    let idempotency_key = derive_play_idempotency_key(user_id, track_id, play_id)?;
    if union_bytes == 0 || union_bytes > MAX_BYTES_PER_ROW {
        return Err(LedgerError::Invalid);
    }
    if file_bytes == Some(0) || duration_ms == Some(0) {
        return Err(LedgerError::Invalid);
    }
    if recorded_at > MAX_RECORDED_AT {
        return Err(LedgerError::Invalid);
    }
    let derivation = derive_stream_seconds(union_bytes, file_bytes, duration_ms);
    // Snapshots persist only for the proportional derivation: a fallback
    // row must recompute from bytes alone, so it stores NULLs even when
    // one of the two inputs happened to be known.
    let (file_bytes, duration_ms) = if derivation.rate_fallback {
        (None, None)
    } else {
        (file_bytes, duration_ms)
    };
    Ok(PlayWindow {
        period_month: period_month_for_timestamp(recorded_at),
        idempotency_key,
        user_id: user_id.to_string(),
        track_id: track_id.to_string(),
        stream_seconds: derivation.seconds,
        bytes_served: union_bytes,
        file_bytes,
        duration_ms,
        rate_fallback: derivation.rate_fallback,
        recorded_at,
    })
}

/// What a zero-row fenced `ON CONFLICT DO NOTHING` insert means. The insert
/// alone cannot tell an exact redelivery from a conflicting payload with the
/// same key — or from the writer fence refusing the write — so the Worker
/// MUST select the existing row, probe the fence, and run
/// [`classify_fenced_replay`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayOutcome {
    /// No row for this key yet and the fence is open: the insert should have
    /// affected one row.
    Fresh,
    /// Exact redelivery: every billed field matches. Safe to acknowledge,
    /// never a double-bill.
    Duplicate,
    /// Same key, different billed payload — or a `play:` key that does not
    /// bind its identity. Never converges, never overwrites: flag it.
    Conflict,
    /// No row for this key and the fence is closed (`users.deleting_at` is
    /// set): the play was refused behind the account-deletion marker and
    /// nothing was written. The Worker fails closed with the typed 409
    /// `account_deleting` (`retryable: false`), never retried behind the
    /// delete.
    AccountDeleting,
}

/// Classify a retried finalize against the stored row.
///
/// `existing = None` (select found no row) is [`ReplayOutcome::Fresh`]. Exact
/// field-for-field agreement is [`ReplayOutcome::Duplicate`]; any
/// disagreement in user, track, bytes, seconds, timestamp, or derived month —
/// or a stored/incoming `play:` key that does not bind its identity — is
/// [`ReplayOutcome::Conflict`]. First write wins: conflicts never append,
/// sum into, or overwrite the stored row (that disagreement is the
/// reconciler's job to flag).
pub fn classify_replay(existing: Option<&LedgerRow>, incoming: &LedgerRow) -> ReplayOutcome {
    let Some(stored) = existing else {
        return ReplayOutcome::Fresh;
    };
    if stored != incoming {
        return ReplayOutcome::Conflict;
    }
    if stored.idempotency_key.starts_with("play:")
        && !key_binds_identity(&stored.idempotency_key, &stored.user_id, &stored.track_id)
    {
        return ReplayOutcome::Conflict;
    }
    ReplayOutcome::Duplicate
}

/// Classify a zero-row fenced insert after the `SELECT` + fence probe.
///
/// `account_deleting` is the fence state the Worker obtains from the
/// lock-free `ACCOUNT_DELETING_PROBE_SQL` (`account_deletion.rs`) in the same
/// handling as the insert — `true` when `users.deleting_at` is set. The
/// stored row decides first: when a row exists the write changed nothing new
/// (the row predates the marker), so an exact redelivery acknowledges as
/// [`ReplayOutcome::Duplicate`] and a disagreement flags
/// [`ReplayOutcome::Conflict`] — healthy duplicates never read as
/// `account_deleting`. Only a missing row consults the probe: fence closed is
/// [`ReplayOutcome::AccountDeleting`], fence open is [`ReplayOutcome::Fresh`]
/// (the insert should have landed; surface it, never assume a replay).
pub fn classify_fenced_replay(
    existing: Option<&LedgerRow>,
    incoming: &LedgerRow,
    account_deleting: bool,
) -> ReplayOutcome {
    if existing.is_some() {
        return classify_replay(existing, incoming);
    }
    if account_deleting {
        return ReplayOutcome::AccountDeleting;
    }
    ReplayOutcome::Fresh
}

/// Monthly usage for one user: what the usage endpoint reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthlyTotal {
    pub user_id: String,
    pub period_month: String,
    pub stream_seconds: u64,
    pub bytes_served: u64,
}

/// Fold rows into per-(user, month) totals, ordered by (user, month).
/// Saturating: totals never wrap. Order-independent by construction
/// (pinned by the property tests).
pub fn fold_monthly_totals(rows: &[LedgerRow]) -> Vec<MonthlyTotal> {
    let mut acc: BTreeMap<(&str, &str), (u64, u64)> = BTreeMap::new();
    for row in rows {
        let entry = acc
            .entry((row.user_id.as_str(), row.period_month.as_str()))
            .or_insert((0, 0));
        entry.0 = entry.0.saturating_add(row.stream_seconds);
        entry.1 = entry.1.saturating_add(row.bytes_served);
    }
    acc.into_iter()
        .map(
            |((user_id, period_month), (stream_seconds, bytes_served))| MonthlyTotal {
                user_id: user_id.to_string(),
                period_month: period_month.to_string(),
                stream_seconds,
                bytes_served,
            },
        )
        .collect()
}

// --- Statements: parameterized, fenced, idempotent ----------------------------
//
// The Worker binds intent fields positionally ($1..$n); never interpolates a
// value into SQL. The insert carries the ADR 0002 rule-3 writer fence
// in-statement (the `library.rs` genre pattern): it selects through the play
// owner's `users` row `FOR SHARE`, so a play finalized behind the
// account-deletion marker inserts zero rows instead of repopulating a swept
// ledger. The insert is replay-safe (`ON CONFLICT DO NOTHING` on the
// idempotency key): a retried finalize inserts zero rows instead of
// double-billing. Zero inserted rows are three-ways ambiguous (exact
// redelivery vs conflicting payload vs fenced refusal), so the Worker selects
// the existing row with `ledger_select_by_key_statement`, probes the fence,
// and runs `classify_fenced_replay`: duplicates acknowledge, conflicts flag,
// fenced refusals fail closed with the typed 409. The monthly total is the
// usage truth for caps and display.

/// Fenced replay-safe insert for one billed play window.
///
/// Parameters: idempotency_key ($1), user_id ($2, also the fence key),
/// track_id ($3), period_month ($4), stream_seconds ($5), bytes_served ($6),
/// recorded_at ($7). One inserted row means the play was first billed while
/// the fence was open; zero rows means the key already exists or the fence
/// refused the write — SELECT the stored row, probe the fence, and run
/// [`classify_fenced_replay`], never assume a benign replay.
pub fn ledger_insert_statement() -> &'static str {
    "INSERT INTO stream_play_ledger (idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at) SELECT $1, $2, $3, $4, $5, $6, $7 FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (idempotency_key) DO NOTHING"
}

/// Fenced monotonic upsert for one play's cumulative delivered union
/// (the meter sink's write). Parameters: idempotency_key ($1), user_id
/// ($2, also the fence key), track_id ($3), period_month ($4),
/// stream_seconds ($5), bytes_served ($6), recorded_at ($7), file_bytes
/// ($8, nullable), duration_ms ($9, nullable).
///
/// Unlike [`ledger_insert_statement`]'s first-write-wins, sink snapshots
/// converge: the same play flushes repeatedly as its union grows, and
/// only a strictly larger union advances the row. The advancing flush
/// replaces the snapshot columns AS A PAIR (no per-column COALESCE):
/// the stored seconds must always recompute from the stored inputs, so
/// a fallback flush (NULL snapshots, nominal-rate seconds —
/// [`prepare_play_window`]) must clear snapshots an earlier
/// proportional flush left behind rather than advertise proportional
/// inputs beside fallback-derived seconds.
/// Redelivering the same or an older snapshot affects zero rows — as
/// does a closed writer fence — so the Worker disambiguates a zero-row
/// outcome with [`ledger_select_by_key_statement`] plus the fence probe,
/// never by assuming a benign replay. `period_month` and `recorded_at`
/// intentionally stay at the first flush: a play crossing a month
/// boundary bills to its starting month.
pub fn ledger_play_window_upsert_statement() -> &'static str {
    "INSERT INTO stream_play_ledger (idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at, file_bytes, duration_ms) SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9 FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (idempotency_key) DO UPDATE SET stream_seconds = EXCLUDED.stream_seconds, bytes_served = EXCLUDED.bytes_served, file_bytes = EXCLUDED.file_bytes, duration_ms = EXCLUDED.duration_ms WHERE EXCLUDED.bytes_served > stream_play_ledger.bytes_served"
}

/// Parameters: idempotency_key. Fetches the stored row after a zero-row
/// insert so [`classify_replay`] can separate exact redeliveries
/// ([`ReplayOutcome::Duplicate`]) from conflicting payloads
/// ([`ReplayOutcome::Conflict`]).
pub fn ledger_select_by_key_statement() -> &'static str {
    "SELECT idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at FROM stream_play_ledger WHERE idempotency_key = $1"
}

/// Parameters: user_id, period_month. One row; zeros when the user has no
/// usage in the period (COALESCE, never NULL). Both sums are cast to BIGINT
/// (`SUM` over BIGINT returns NUMERIC in Postgres) so callers can always
/// decode them as integers.
pub fn ledger_monthly_total_statement() -> &'static str {
    "SELECT COALESCE(SUM(stream_seconds), 0)::BIGINT AS stream_seconds, COALESCE(SUM(bytes_served), 0)::BIGINT AS bytes_served FROM stream_play_ledger WHERE user_id = $1 AND period_month = $2"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_pinned() {
        // Literal pins: the fallback rate re-bills history and the row caps
        // move the corruption tripwire, so each changes only deliberately.
        // The byte cap keeps its historical value but is NOT pinned to
        // `86_400 * 48_000` anymore: migration 0029 decoupled the caps
        // from the derivation, so asserting the product would re-couple
        // them in the suite.
        assert_eq!(STREAM_BYTES_PER_SEC, 48_000);
        assert_eq!(MAX_STREAM_SECONDS_PER_ROW, 86_400);
        assert_eq!(MAX_BYTES_PER_ROW, 4_147_200_000);
        assert_eq!(MAX_ID_LEN, 128);
        assert_eq!(MAX_USER_ID_LEN, 128);
        assert_eq!(MAX_TRACK_ID_LEN, 512);
        assert_eq!(MAX_PLAY_ID_LEN, 512);
        // play: + 128 + : + 512 + : + 512.
        assert_eq!(MAX_IDEMPOTENCY_KEY_LEN, 1_159);
        assert_eq!(LedgerError::Invalid.http_status(), 422);
    }

    #[test]
    fn idempotency_keys_are_fresh_22_char_base64url() {
        let key = new_idempotency_key().expect("mint with working RNG");
        assert_eq!(key.len(), 22);
        assert!(check_key(&key).is_ok());
        let other = new_idempotency_key().expect("mint with working RNG");
        assert_ne!(key, other, "keys must not repeat");
    }

    #[test]
    fn play_key_derivation_matches_the_stream_worker() {
        // Shared vectors with workers/stream/src/meter.rs: either side
        // drifting fails here instead of silently unbilled plays.
        assert_eq!(
            derive_play_idempotency_key("user-1", "track-9", "play-7").unwrap(),
            "play:user-1:track-9:play-7"
        );
        for invalid in ["has space", "has\ttab", "forged=value", "a:b"] {
            assert!(derive_play_idempotency_key(invalid, "track", "play").is_err());
            assert!(derive_play_idempotency_key("user", invalid, "play").is_err());
            assert!(derive_play_idempotency_key("user", "track", invalid).is_err());
        }
        assert!(derive_play_idempotency_key(&"u".repeat(129), "t", "p").is_err());
        assert!(derive_play_idempotency_key("u", &"t".repeat(513), "p").is_err());
        assert!(derive_play_idempotency_key("u", "t", &"p".repeat(513)).is_err());
    }

    #[test]
    fn reset_and_iso_vectors() {
        // ISO rendering: epoch, a mid-month instant, a leap day.
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso8601_utc(1_709_164_800), "2024-02-29T00:00:00Z");
        // Reset = first instant of the next UTC calendar month, including
        // the December rollover and an instant exactly at a month start.
        let reset = |now: u64| iso8601_utc(next_month_start_unix(now) as i64);
        assert_eq!(reset(1_700_000_000), "2023-12-01T00:00:00Z");
        // 2026-12-31T23:59:59Z is one second before 2027-01-01.
        assert_eq!(reset(1_798_761_599), "2027-01-01T00:00:00Z");
        // 2026-01-01T00:00:00Z exactly: the *next* month start, never now.
        assert_eq!(reset(1_767_225_600), "2026-02-01T00:00:00Z");
        // Leap February has 29 days: 2024-02-29 resets 2024-03-01.
        assert_eq!(reset(1_709_164_800), "2024-03-01T00:00:00Z");
        // The reset always lands in the following period_month.
        for now in [0_u64, 1_700_000_000, 1_767_225_600, 1_798_761_599] {
            assert_ne!(
                period_month_for_timestamp(next_month_start_unix(now)),
                period_month_for_timestamp(now),
                "reset for {now} must leave the current period"
            );
        }
    }

    #[test]
    fn period_vectors() {
        // Unix epoch, a mid-month instant, month boundaries, a leap day and
        // a far-future date: all pinned as YYYY-MM UTC.
        assert_eq!(period_month_for_timestamp(0), "1970-01");
        // 2023-11-14T22:13:20Z.
        assert_eq!(period_month_for_timestamp(1_700_000_000), "2023-11");
        // 2026-01-01T00:00:00Z and the second before it.
        assert_eq!(period_month_for_timestamp(1_767_225_600), "2026-01");
        assert_eq!(period_month_for_timestamp(1_767_225_599), "2025-12");
        // 2024-02-29T00:00:00Z (leap day) and 2024-03-01T00:00:00Z.
        assert_eq!(period_month_for_timestamp(1_709_164_800), "2024-02");
        assert_eq!(period_month_for_timestamp(1_709_251_200), "2024-03");
        // 2100-01-01T00:00:00Z.
        assert_eq!(period_month_for_timestamp(4_102_444_800), "2100-01");
    }

    #[test]
    fn recorded_at_fits_bigint_and_four_digit_month() {
        let row = prepare_ledger_row(
            "user-alice",
            "track-1",
            &new_idempotency_key().expect("mint with working RNG"),
            1,
            1,
            MAX_RECORDED_AT,
        )
        .unwrap();
        assert_eq!(row.period_month, "9999-12");
        for recorded_at in [MAX_RECORDED_AT + 1, i64::MAX as u64, u64::MAX] {
            assert_eq!(
                prepare_ledger_row(
                    "user-alice",
                    "track-1",
                    &new_idempotency_key().expect("mint with working RNG"),
                    1,
                    1,
                    recorded_at,
                ),
                Err(LedgerError::Invalid),
                "recorded_at {recorded_at} must fit the database month domain"
            );
        }
    }

    #[test]
    fn prepare_bounds_seconds_without_rate_coupling() {
        // Since migration 0029 the prepare gate bounds caller-derived
        // seconds instead of recomputing the nominal rate: a FLAC window
        // whose proportional seconds sit far below ceil(bytes / 48_000)
        // prepares fine. The floor stays: zero seconds for positive
        // bytes is no faithful derivation (both derivation paths bill
        // at least one second) and must fail closed, like the cap.
        assert_eq!(
            prepare_ledger_row(
                "user-alice",
                "track-1",
                &new_idempotency_key().expect("mint with working RNG"),
                48_001,
                0,
                1_767_225_600,
            ),
            Err(LedgerError::Invalid),
            "zero seconds for positive bytes must fail closed"
        );
        let row = prepare_ledger_row(
            "user-alice",
            "track-1",
            &new_idempotency_key().expect("mint with working RNG"),
            48_001,
            1,
            1_767_225_600,
        )
        .unwrap();
        assert_eq!(row.stream_seconds, 1);
        assert_eq!(row.period_month, "2026-01");
        let row = prepare_ledger_row(
            "user-alice",
            "track-1",
            &new_idempotency_key().expect("mint with working RNG"),
            4_147_200_000,
            MAX_STREAM_SECONDS_PER_ROW,
            1_767_225_600,
        )
        .unwrap();
        assert_eq!(row.stream_seconds, 86_400);
        assert_eq!(
            prepare_ledger_row(
                "user-alice",
                "track-1",
                &new_idempotency_key().expect("mint with working RNG"),
                48_001,
                MAX_STREAM_SECONDS_PER_ROW + 1,
                1_767_225_600,
            ),
            Err(LedgerError::Invalid),
            "seconds above the per-row cap must fail closed"
        );
    }

    #[test]
    fn derivation_is_proportional_with_duration_clamp() {
        // 30 MB FLAC, 240 s: a full play is exact by construction and a
        // half play accrues half the duration — the nominal rate would
        // have billed 625 s / 313 s for the same bytes.
        let file = 30_000_000u64;
        let duration_ms = 240_000u64;
        let full = derive_stream_seconds(file, Some(file), Some(duration_ms));
        assert_eq!(
            full,
            SecondsDerivation {
                seconds: 240,
                rate_fallback: false
            }
        );
        let half = derive_stream_seconds(file / 2, Some(file), Some(duration_ms));
        assert_eq!(half.seconds, 120);
        // A sub-second remainder rounds up exactly once, from the folded
        // union: 1 byte past the half still bills 121 s, never 240.
        let just_past = derive_stream_seconds(file / 2 + 1, Some(file), Some(duration_ms));
        assert_eq!(just_past.seconds, 121);
        // An over-file union (inconsistent sink input) clamps to
        // duration + 2 s instead of billing past the track.
        let over = derive_stream_seconds(file * 3, Some(file), Some(duration_ms));
        assert_eq!(over.seconds, 242);
        // Odd durations ceil to whole seconds for the clamp bound.
        let odd = derive_stream_seconds(file, Some(file), Some(200_500));
        assert_eq!(odd.seconds, 201);
        let odd_over = derive_stream_seconds(file * 2, Some(file), Some(200_500));
        assert_eq!(odd_over.seconds, 203);
        // Nothing delivered bills nothing.
        assert_eq!(
            derive_stream_seconds(0, Some(file), Some(duration_ms)).seconds,
            0
        );
    }

    #[test]
    fn derivation_falls_back_to_the_nominal_rate() {
        // Same literals the stream worker pins, so a rate drift fails here.
        for (bytes, seconds) in [
            (1u64, 1u64),
            (47_999, 1),
            (48_000, 1),
            (48_001, 2),
            (4_147_200_000, 86_400),
        ] {
            for (file, duration) in [
                (None, None),
                (Some(30_000_000), None),
                (None, Some(240_000)),
                (Some(0), Some(240_000)),
                (Some(30_000_000), Some(0)),
            ] {
                let derived = derive_stream_seconds(bytes, file, duration);
                assert_eq!(
                    derived,
                    SecondsDerivation {
                        seconds,
                        rate_fallback: true
                    },
                    "bytes={bytes} file={file:?} duration={duration:?}"
                );
            }
        }
    }

    #[test]
    fn duration_resolution_walks_the_hierarchy() {
        // Track > MusicBrainz recording > bitrate estimate > unknown.
        assert_eq!(
            resolve_duration_ms(Some(240_000), Some(241_000), Some(1_000), Some(30_000_000)),
            Some(240_000)
        );
        assert_eq!(
            resolve_duration_ms(None, Some(241_000), Some(1_000), Some(30_000_000)),
            Some(241_000)
        );
        // Zero rungs are skipped, never resolved as a 0 ms duration.
        assert_eq!(
            resolve_duration_ms(Some(0), Some(241_000), None, None),
            Some(241_000)
        );
        // Bitrate estimate: 30 MB at 1_000 kbit/s is 240 s.
        assert_eq!(
            resolve_duration_ms(None, None, Some(1_000), Some(30_000_000)),
            Some(240_000)
        );
        assert_eq!(
            resolve_duration_ms(None, None, Some(0), Some(30_000_000)),
            None
        );
        assert_eq!(resolve_duration_ms(None, None, Some(1_000), None), None);
        assert_eq!(resolve_duration_ms(None, None, None, None), None);
    }

    #[test]
    fn play_window_derives_seconds_and_snapshots() {
        let window = prepare_play_window(
            "user-1",
            "track-9",
            "play-7",
            15_000_000,
            Some(30_000_000),
            Some(240_000),
            1_767_225_600,
        )
        .unwrap();
        assert_eq!(window.idempotency_key, "play:user-1:track-9:play-7");
        assert_eq!(window.stream_seconds, 120);
        assert_eq!(window.file_bytes, Some(30_000_000));
        assert_eq!(window.duration_ms, Some(240_000));
        assert!(!window.rate_fallback);
        assert_eq!(window.period_month, "2026-01");
        // Fallback derivation stores NULL snapshots even when one input
        // was known, so the row recomputes from bytes alone.
        let fallback = prepare_play_window(
            "user-1",
            "track-9",
            "play-7",
            48_001,
            Some(30_000_000),
            None,
            1_767_225_600,
        )
        .unwrap();
        assert_eq!(fallback.stream_seconds, 2);
        assert_eq!(fallback.file_bytes, None);
        assert_eq!(fallback.duration_ms, None);
        assert!(fallback.rate_fallback);
        // Inconsistent inputs fail closed: no bytes, zero file/duration,
        // oversized union, unmeterable play id.
        for (union, file, duration) in [
            (0, Some(30_000_000), Some(240_000)),
            (1, Some(0), Some(240_000)),
            (1, Some(30_000_000), Some(0)),
            (MAX_BYTES_PER_ROW + 1, None, None),
        ] {
            assert_eq!(
                prepare_play_window(
                    "user-1",
                    "track-9",
                    "play-7",
                    union,
                    file,
                    duration,
                    1_767_225_600
                ),
                Err(LedgerError::Invalid),
                "union={union} file={file:?} duration={duration:?}"
            );
        }
        assert_eq!(
            prepare_play_window("user-1", "track-9", "play:7", 1, None, None, 1_767_225_600),
            Err(LedgerError::Invalid)
        );
    }

    #[test]
    fn fenced_replay_prefers_the_stored_row_over_the_probe() {
        let first =
            prepare_play_ledger_row("user-1", "track-9", "play-7", 48_000, 1, 1_767_225_600)
                .expect("valid play row");
        let replay =
            prepare_play_ledger_row("user-1", "track-9", "play-7", 48_000, 1, 1_767_225_600)
                .unwrap();
        // A stored row decides without consulting the probe: a healthy
        // duplicate never reads as account_deleting, even with the marker set.
        assert_eq!(
            classify_fenced_replay(Some(&first), &replay, true),
            ReplayOutcome::Duplicate
        );
        assert_eq!(
            classify_fenced_replay(Some(&first), &replay, false),
            ReplayOutcome::Duplicate
        );
        let mut tampered = first.clone();
        tampered.bytes_served = 96_000;
        tampered.stream_seconds = 2;
        assert_eq!(
            classify_fenced_replay(Some(&first), &tampered, false),
            ReplayOutcome::Conflict
        );
        // No stored row: only the probe decides. Fence closed refuses the
        // play behind the marker; fence open means the insert should have
        // landed.
        assert_eq!(
            classify_fenced_replay(None, &first, true),
            ReplayOutcome::AccountDeleting
        );
        assert_eq!(
            classify_fenced_replay(None, &first, false),
            ReplayOutcome::Fresh
        );
    }

    #[test]
    fn statements_match_exact_text() {
        assert_eq!(
            ledger_insert_statement(),
            "INSERT INTO stream_play_ledger (idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at) SELECT $1, $2, $3, $4, $5, $6, $7 FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (idempotency_key) DO NOTHING",
            "ledger insert"
        );
        assert_eq!(
            ledger_play_window_upsert_statement(),
            "INSERT INTO stream_play_ledger (idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at, file_bytes, duration_ms) SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9 FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (idempotency_key) DO UPDATE SET stream_seconds = EXCLUDED.stream_seconds, bytes_served = EXCLUDED.bytes_served, file_bytes = EXCLUDED.file_bytes, duration_ms = EXCLUDED.duration_ms WHERE EXCLUDED.bytes_served > stream_play_ledger.bytes_served",
            "play window upsert"
        );
        assert_eq!(
            ledger_select_by_key_statement(),
            "SELECT idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at FROM stream_play_ledger WHERE idempotency_key = $1",
            "ledger select by key"
        );
        assert_eq!(
            ledger_monthly_total_statement(),
            "SELECT COALESCE(SUM(stream_seconds), 0)::BIGINT AS stream_seconds, COALESCE(SUM(bytes_served), 0)::BIGINT AS bytes_served FROM stream_play_ledger WHERE user_id = $1 AND period_month = $2",
            "monthly total"
        );
    }
}
