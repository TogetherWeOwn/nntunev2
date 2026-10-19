//! Authoritative per-play stream ledger: one row per billed play window.
//!
//! Analytics Engine carries the fast per-second/per-byte signal (see the
//! stream worker's metering); this table is the billing truth the usage
//! endpoint and limit enforcement read. Pure intents plus the parameterized
//! statements the Worker entry binds against PlanetScale Postgres via
//! Hyperdrive — never string-interpolated SQL.
//!
//! Table identity (Paperclip Review `6508688`, warning 1): this is NOT the
//! accepted ADR 0002/0004 daily `usage_ledger` (`period DATE`, `ingest_jobs`,
//! `UNIQUE (user_id, period)`, additive `ON CONFLICT (user_id, period)
//! DO UPDATE`). That daily aggregate remains the cap-enforcement shape and is
//! built in a later slice by folding these per-play windows (and ingest
//! accepts). To keep the future `CREATE TABLE IF NOT EXISTS usage_ledger`
//! from silently no-op'ing on a colliding name and then failing its
//! `ON CONFLICT (user_id, period)`, this slice's per-play table is named
//! `stream_play_ledger` (`migrations/0020_stream_play_ledger.sql`). See the
//! amendment notes in `docs/adr/0002-data-model.md`, `0004-plan-matrix.md`
//! and `0005-billing.md`.
//!
//! Rules:
//! - One [`LedgerRow`] per billed play window: (`user_id`, `track_id`,
//!   `idempotency_key`, `bytes_served`, `stream_seconds`, `recorded_at`).
//!   Seconds MUST equal `ceil(bytes / STREAM_BYTES_PER_SEC)` — the same rate
//!   the stream worker attributes with — derived from the window's folded
//!   byte total, never by summing per-event seconds (each event rounds up,
//!   so per-event seconds are not additive); mismatched seconds are rejected.
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
//! - Bounds: a single row carries at most [`MAX_BYTES_PER_ROW`] bytes (24 h
//!   of audio at the nominal rate); zero-byte rows are rejected — an empty
//!   row bills nothing and only widens the table.

use std::collections::BTreeMap;

/// Nominal bytes per stream second. MUST match the stream worker's metering
/// rate: both sides pin the same literal vectors in tests, so a drift fails
/// the suite on either side instead of silently re-billing history.
pub const STREAM_BYTES_PER_SEC: u64 = 48_000;
/// Maximum stream seconds one row may attribute (24 h of audio).
pub const MAX_STREAM_SECONDS_PER_ROW: u64 = 86_400;
/// Maximum bytes one row may carry: exactly 24 h at the nominal rate.
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
    /// oversized bytes, seconds disagreeing with bytes, play key not binding
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
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| LedgerError::RandomnessUnavailable)?;
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

/// Build the ledger intent for one billed service window. `stream_seconds`
/// must already equal `ceil(bytes_served / STREAM_BYTES_PER_SEC)` — the
/// Worker passes through what the stream worker metered, and any skew fails
/// closed instead of billing a number neither side derived.
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
    if stream_seconds != bytes_served.div_ceil(STREAM_BYTES_PER_SEC) {
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
        // Literal pins: the rate re-bills history and the row cap moves the
        // corruption tripwire, so both change only deliberately and together
        // with the stream worker's pins.
        assert_eq!(STREAM_BYTES_PER_SEC, 48_000);
        assert_eq!(MAX_STREAM_SECONDS_PER_ROW, 86_400);
        assert_eq!(MAX_BYTES_PER_ROW, 4_147_200_000);
        assert_eq!(MAX_BYTES_PER_ROW, 86_400 * 48_000);
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
    fn seconds_bytes_agreement_vectors() {
        // The prepare gate recomputes the ceiling: these are the same
        // literals the stream worker pins, so a rate drift fails here.
        for (bytes, seconds) in [
            (1u64, 1u64),
            (47_999, 1),
            (48_000, 1),
            (48_001, 2),
            (4_147_200_000, 86_400),
        ] {
            let row = prepare_ledger_row(
                "user-alice",
                "track-1",
                &new_idempotency_key().expect("mint with working RNG"),
                bytes,
                seconds,
                1_767_225_600,
            )
            .unwrap();
            assert_eq!(row.stream_seconds, seconds);
            assert_eq!(row.period_month, "2026-01");
        }
        // Skewed seconds fail closed, both directions.
        for bad_seconds in [0, 1, 3] {
            assert_eq!(
                prepare_ledger_row(
                    "user-alice",
                    "track-1",
                    &new_idempotency_key().expect("mint with working RNG"),
                    48_001,
                    bad_seconds,
                    1_767_225_600,
                ),
                Err(LedgerError::Invalid),
                "seconds {bad_seconds} for 48_001 bytes"
            );
        }
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
