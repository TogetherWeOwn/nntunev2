//! Authoritative usage ledger: one row per billed play segment.
//!
//! Analytics Engine carries the fast per-second/per-byte signal (see the
//! stream worker's metering); this table is the billing truth the usage
//! endpoint and limit enforcement read. Pure intents plus the parameterized
//! statements the Worker entry binds against PlanetScale Postgres via
//! Hyperdrive — never string-interpolated SQL.
//!
//! Rules:
//! - One [`LedgerRow`] per billed service window: (`user_id`, `track_id`,
//!   `idempotency_key`, `bytes_served`, `stream_seconds`, `recorded_at`).
//!   Seconds MUST equal `ceil(bytes / STREAM_BYTES_PER_SEC)` — the same rate
//!   the stream worker attributes with — derived from the window's folded
//!   byte total, never by summing per-event seconds (each event rounds up,
//!   so per-event seconds are not additive); mismatched seconds are rejected.
//! - `period_month` (`YYYY-MM`, UTC) derives deterministically from
//!   `recorded_at`; monthly usage is `SUM` over (`user_id`, `period_month`).
//! - Replay safety: the insert is `ON CONFLICT (idempotency_key) DO NOTHING`,
//!   so a retried finalize never double-bills. Keys are fresh 128-bit
//!   base64url handles from [`new_idempotency_key`].
//! - Bounds: a single row carries at most [`MAX_BYTES_PER_ROW`] bytes (24 h
//!   of audio at the nominal rate); zero-byte rows are rejected — an empty
//!   row bills nothing and only widens the table.

use rand::RngCore;
use std::collections::BTreeMap;

/// Nominal bytes per stream second. MUST match the stream worker's metering
/// rate: both sides pin the same literal vectors in tests, so a drift fails
/// the suite on either side instead of silently re-billing history.
pub const STREAM_BYTES_PER_SEC: u64 = 48_000;
/// Maximum stream seconds one row may attribute (24 h of audio).
pub const MAX_STREAM_SECONDS_PER_ROW: u64 = 86_400;
/// Maximum bytes one row may carry: exactly 24 h at the nominal rate.
pub const MAX_BYTES_PER_ROW: u64 = 4_147_200_000;
/// Maximum length of user, track and idempotency-key handles (chars).
pub const MAX_ID_LEN: usize = 128;

/// Why a ledger row was rejected. Failures stay generic: inputs are never
/// echoed (they may carry caller-chosen handles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerError {
    /// Shape violation (empty/overlong handle, bad key charset, zero or
    /// oversized bytes, seconds disagreeing with bytes): HTTP 422.
    Invalid,
}

impl LedgerError {
    pub fn http_status(&self) -> u16 {
        match self {
            LedgerError::Invalid => 422,
        }
    }
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Invalid => write!(f, "invalid usage row"),
        }
    }
}

/// Fresh 128-bit idempotency key (base64url, 22 chars, via the shared
/// [`crate::b64`] helper). Never sequential. Mint once per billed service
/// window and reuse the returned key for every finalize retry, so
/// `ON CONFLICT (idempotency_key) DO NOTHING` can deduplicate retries.
pub fn new_idempotency_key() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    crate::b64::encode(&bytes)
}

/// Calendar month (`YYYY-MM`, UTC) containing Unix timestamp `recorded_at`.
/// Dependency-free civil-date conversion (Hinnant's algorithm); valid for the
/// full `u64` timestamp range.
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

fn check_handle(value: &str) -> Result<String, LedgerError> {
    if value.is_empty() || value.chars().count() > MAX_ID_LEN {
        return Err(LedgerError::Invalid);
    }
    Ok(value.to_string())
}

fn check_key(value: &str) -> Result<String, LedgerError> {
    if value.is_empty() || value.chars().count() > MAX_ID_LEN {
        return Err(LedgerError::Invalid);
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(LedgerError::Invalid);
    }
    Ok(value.to_string())
}

/// Build the ledger intent for one billed service window. `stream_seconds`
/// must already equal `ceil(bytes_served / STREAM_BYTES_PER_SEC)` — the
/// Worker passes through what the stream worker metered, and any skew fails
/// closed instead of billing a number neither side derived.
pub fn prepare_ledger_row(
    user_id: &str,
    track_id: &str,
    idempotency_key: &str,
    bytes_served: u64,
    stream_seconds: u64,
    recorded_at: u64,
) -> Result<LedgerRow, LedgerError> {
    let user_id = check_handle(user_id)?;
    let track_id = check_handle(track_id)?;
    let idempotency_key = check_key(idempotency_key)?;
    if bytes_served == 0 || bytes_served > MAX_BYTES_PER_ROW {
        return Err(LedgerError::Invalid);
    }
    if stream_seconds != bytes_served.div_ceil(STREAM_BYTES_PER_SEC) {
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

// --- Statements: parameterized, idempotent ----------------------------------
//
// The Worker binds intent fields positionally ($1..$n); never interpolates a
// value into SQL. The insert is replay-safe (`ON CONFLICT DO NOTHING` on the
// idempotency key): a retried finalize inserts zero rows instead of
// double-billing. The monthly total is the usage truth for caps and display.

/// Parameters: idempotency_key, user_id, track_id, period_month,
/// stream_seconds, bytes_served, recorded_at. Zero inserted rows means the
/// key already exists (safe replay), never an error.
pub fn ledger_insert_statement() -> &'static str {
    "INSERT INTO usage_ledger (idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (idempotency_key) DO NOTHING"
}

/// Parameters: user_id, period_month. One row; zeros when the user has no
/// usage in the period (COALESCE, never NULL). Both sums are cast to BIGINT
/// (`SUM` over BIGINT returns NUMERIC in Postgres) so callers can always
/// decode them as integers.
pub fn ledger_monthly_total_statement() -> &'static str {
    "SELECT COALESCE(SUM(stream_seconds), 0)::BIGINT AS stream_seconds, COALESCE(SUM(bytes_served), 0)::BIGINT AS bytes_served FROM usage_ledger WHERE user_id = $1 AND period_month = $2"
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
        assert_eq!(LedgerError::Invalid.http_status(), 422);
    }

    #[test]
    fn idempotency_keys_are_fresh_22_char_base64url() {
        let key = new_idempotency_key();
        assert_eq!(key.len(), 22);
        assert!(check_key(&key).is_ok());
        let other = new_idempotency_key();
        assert_ne!(key, other, "keys must not repeat");
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
                &new_idempotency_key(),
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
                    &new_idempotency_key(),
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
    fn statements_match_exact_text() {
        assert_eq!(
            ledger_insert_statement(),
            "INSERT INTO usage_ledger (idempotency_key, user_id, track_id, period_month, stream_seconds, bytes_served, recorded_at) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (idempotency_key) DO NOTHING",
            "ledger insert"
        );
        assert_eq!(
            ledger_monthly_total_statement(),
            "SELECT COALESCE(SUM(stream_seconds), 0)::BIGINT AS stream_seconds, COALESCE(SUM(bytes_served), 0)::BIGINT AS bytes_served FROM usage_ledger WHERE user_id = $1 AND period_month = $2",
            "monthly total"
        );
    }
}
