//! Cache observability: the Analytics Engine row + structured log line for
//! one play, as an event shape plus its projection from a play outcome.
//!
//! The play path ([`crate::play`]) already emits [`crate::meter`] events for
//! the usage ledger. This module is the Analytics Engine projection of the
//! same outcome: a single [`CacheObserveEvent`] per play carrying the cache
//! hit/miss signal, time-to-first-byte, bytes served and the pool error code
//! (M4 metering builds on this row shape).
//!
//! Wiring status: [`CacheObserveEvent::from_play`] and
//! [`crate::play::PlayStream::observe_event`] build events from started
//! streams; [`crate::play::play_with_observe_event`] projects pool-acquisition
//! failures that return before a stream exists. The caller supplies the
//! `request_id` and the TTFT it measures around the first served byte. This
//! crate is lib-only — nothing calls `writeDataPoint` until the Worker/DO
//! entry point lands, and nothing in this module emits on its own.
//!
//! Privacy: the raw user id never leaves the caller. [`CacheObserveEvent::new`]
//! hashes it with [`hash_user`] (SHA-256 hex) before storage, so the event,
//! its [`CacheObserveEvent::to_json_log`] line, its Analytics Engine columns
//! and its `Debug` output can never leak the raw id.
//!
//! Analytics Engine mapping (`METER` dataset `nntune_meter`, bound in
//! `workers/stream/wrangler.toml`):
//!
//! ```text
//! blob1 = cache_status  ('hit' | 'miss')
//! blob2 = user_hash     (SHA-256 hex, 64 chars)
//! blob3 = request_id
//! blob4 = pool_error_code ('' when the play had no pool error)
//! double1 = ttft_ms
//! double2 = bytes
//! ```
//!
//! The Worker/DO entry point maps one finished play to one event and writes
//! it with a single `writeDataPoint` call:
//!
//! ```js,ignore
//! env.METER.writeDataPoint({
//!   blobs: ev.analytics_blobs(),     // [status, user_hash, request_id, pool_error]
//!   doubles: ev.analytics_doubles(), // [ttft_ms, bytes]
//!   indexes: [ev.analytics_index()],  // user_hash: the sampling key
//! });
//! console.log(ev.to_json_log()); // structured JSON, Workers Logs greppable
//! ```
//!
//! The index is `user_hash`, not `cache_status`: Analytics Engine samples
//! per index value, so a two-value index would down-sample every hit as one
//! bucket. `cache_status` stays queryable as `blob1`. Because writes can be
//! sampled, queries must weight aggregates by `_sample_interval` (see
//! [`STAGING_QUERY_TEMPLATE`]).

use crate::grant::sha256;
use crate::play::PlayError;

/// Whether the play was served from the R2 cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheStatus {
    Hit,
    Miss,
}

impl CacheStatus {
    /// Analytics Engine / log spelling: lowercase `hit` / `miss`.
    pub fn as_str(self) -> &'static str {
        match self {
            CacheStatus::Hit => "hit",
            CacheStatus::Miss => "miss",
        }
    }
}

/// One observable fact about a finished play: the Analytics Engine row and
/// the structured log line are two renderings of this value.
///
/// Only the hashed user identity is stored — there is no raw-id field to
/// leak through `Debug`, logs or Analytics Engine columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheObserveEvent {
    pub request_id: String,
    /// Lowercase hex SHA-256 of the raw user id (see [`hash_user`]).
    pub user_hash: String,
    pub cache_status: CacheStatus,
    /// Milliseconds from authorize to first byte.
    pub ttft_ms: u64,
    /// Bytes served to the client (0 when the play failed before serving).
    pub bytes: u64,
    /// Pool-level failure, e.g. `POOL_QUARANTINED` / `POOL_EXHAUSTED`;
    /// `None` when the pool was healthy.
    pub pool_error_code: Option<String>,
}

impl CacheObserveEvent {
    /// Build an event. `user_id` is the raw id: it is hashed with
    /// [`hash_user`] before storage and never retained.
    pub fn new(
        request_id: impl Into<String>,
        user_id: &str,
        cache_status: CacheStatus,
        ttft_ms: u64,
        bytes: u64,
        pool_error_code: Option<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            user_hash: hash_user(user_id),
            cache_status,
            ttft_ms,
            bytes,
            pool_error_code,
        }
    }

    /// Project a finished play outcome to an event: `cache_hit` selects the
    /// status, `bytes` is what the client was served, and a pool-level
    /// `error` is mapped through [`PlayError::pool_error_code`] (non-pool
    /// errors and `None` carry no code). `ttft_ms` is measured by the caller
    /// around the first served byte — this crate owns no clock.
    pub fn from_play(
        request_id: impl Into<String>,
        user_id: &str,
        cache_hit: bool,
        ttft_ms: u64,
        bytes: u64,
        error: Option<&PlayError>,
    ) -> Self {
        Self::new(
            request_id,
            user_id,
            if cache_hit {
                CacheStatus::Hit
            } else {
                CacheStatus::Miss
            },
            ttft_ms,
            bytes,
            error.and_then(PlayError::pool_error_code).map(String::from),
        )
    }

    /// Structured JSON log line: one object per line for Workers Logs.
    /// Carries the request id, the hashed (never raw) user identity and the
    /// cache status alongside TTFT, bytes and the pool error code.
    pub fn to_json_log(&self) -> String {
        let pool_error = match &self.pool_error_code {
            Some(code) => format!("\"{}\"", escape_json_str(code)),
            None => "null".to_string(),
        };
        format!(
            "{{\"request_id\":\"{}\",\"user_hash\":\"{}\",\"cache_status\":\"{}\",\"ttft_ms\":{},\"bytes\":{},\"pool_error_code\":{}}}",
            escape_json_str(&self.request_id),
            escape_json_str(&self.user_hash),
            self.cache_status.as_str(),
            self.ttft_ms,
            self.bytes,
            pool_error,
        )
    }

    /// String columns for the Analytics Engine `METER` dataset:
    /// `[cache_status, user_hash, request_id, pool_error_code]`. Positions
    /// are stable (empty string when there is no pool error) so the worker
    /// glue can pass this straight to `writeDataPoint`.
    pub fn analytics_blobs(&self) -> Vec<String> {
        vec![
            self.cache_status.as_str().to_string(),
            self.user_hash.clone(),
            self.request_id.clone(),
            self.pool_error_code.clone().unwrap_or_default(),
        ]
    }

    /// Numeric columns for the Analytics Engine `METER` dataset:
    /// `[ttft_ms, bytes]`.
    pub fn analytics_doubles(&self) -> Vec<f64> {
        vec![self.ttft_ms as f64, self.bytes as f64]
    }

    /// The `writeDataPoint` index (sampling key) for this row: the stable
    /// per-user hash. Never `cache_status` — a two-value index makes
    /// Analytics Engine sample all hits as a single bucket (see module docs).
    pub fn analytics_index(&self) -> &str {
        &self.user_hash
    }
}

/// Hash a raw user id to lowercase hex SHA-256 (64 chars). Deterministic per
/// user; distinct users hash distinctly; the digest never embeds the input.
pub fn hash_user(user_id: &str) -> String {
    sha256(user_id.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Escape a string for embedding in a JSON double-quoted value.
fn escape_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Sample staging query for the Cloudflare dashboard (Analytics Engine SQL
/// API): hit-rate, byte volume and TTFT split by cache status over the last
/// 24h. Paste the evidence run on the card; see the module docs for the
/// blob/double column mapping.
///
/// Analytics Engine samples adaptively per index, so every aggregate is
/// weighted by `_sample_interval` (`SUM(_sample_interval)` for the count,
/// `SUM(_sample_interval * x)` for sums/averages) and the p95 uses
/// `quantileExactWeighted`, the quantile form AE documents for sampled data.
/// Unweighted `COUNT`/`SUM`/`AVG` would under-report as soon as sampling
/// starts.
pub const STAGING_QUERY_TEMPLATE: &str = "\
-- nntune stream cache hit-rate + TTFT (staging evidence query).\n\
-- Column mapping: blob1 = cache_status, blob2 = user_hash,\n\
-- blob3 = request_id, blob4 = pool_error_code; double1 = ttft_ms,\n\
-- double2 = bytes. Dataset `nntune_meter` is the METER binding in\n\
-- workers/stream/wrangler.toml. Aggregates are _sample_interval-\n\
-- weighted because Analytics Engine samples per index (user_hash).\n\
SELECT\n\
  blob1 AS cache_status,\n\
  blob4 AS pool_error_code,\n\
  SUM(_sample_interval) AS plays,\n\
  SUM(_sample_interval * double2) AS bytes,\n\
  SUM(_sample_interval * double1) / SUM(_sample_interval) AS avg_ttft_ms,\n\
  quantileExactWeighted(0.95)(double1, _sample_interval) AS p95_ttft_ms\n\
FROM nntune_meter\n\
WHERE timestamp > NOW() - INTERVAL '24' HOUR\n\
GROUP BY cache_status, pool_error_code\n\
ORDER BY cache_status;";

#[cfg(test)]
mod tests {
    use super::*;

    // Request ids deliberately contain neither "hit" nor "miss", so only the
    // status column itself can satisfy the status assertions.
    fn hit() -> CacheObserveEvent {
        CacheObserveEvent::new("req-001", "user-1", CacheStatus::Hit, 12, 40_000, None)
    }

    #[test]
    fn blobs_and_doubles_pin_exact_column_order_for_hit_miss_and_error() {
        let ev = hit();
        assert_eq!(
            ev.analytics_blobs(),
            vec!["hit", ev.user_hash.as_str(), "req-001", ""]
        );
        assert_eq!(ev.analytics_doubles(), vec![12.0, 40_000.0]);

        let ev = CacheObserveEvent::new("req-002", "user-1", CacheStatus::Miss, 850, 60_000, None);
        assert_eq!(
            ev.analytics_blobs(),
            vec!["miss", ev.user_hash.as_str(), "req-002", ""]
        );
        assert_eq!(ev.analytics_doubles(), vec![850.0, 60_000.0]);

        let ev = CacheObserveEvent::new(
            "req-003",
            "user-1",
            CacheStatus::Miss,
            1_200,
            0,
            Some("POOL_QUARANTINED".to_string()),
        );
        assert_eq!(
            ev.analytics_blobs(),
            vec!["miss", ev.user_hash.as_str(), "req-003", "POOL_QUARANTINED"]
        );
        assert_eq!(ev.analytics_doubles(), vec![1_200.0, 0.0]);
    }

    #[test]
    fn analytics_index_is_the_user_hash_not_the_status() {
        let ev = hit();
        assert_eq!(ev.analytics_index(), ev.user_hash);
        assert_eq!(ev.analytics_index().len(), 64);
        assert_ne!(ev.analytics_index(), ev.cache_status.as_str());
    }

    #[test]
    fn json_log_is_exact_full_line_with_and_without_pool_error() {
        let ev = hit();
        assert_eq!(
            ev.to_json_log(),
            format!(
                "{{\"request_id\":\"req-001\",\"user_hash\":\"{}\",\"cache_status\":\"hit\",\"ttft_ms\":12,\"bytes\":40000,\"pool_error_code\":null}}",
                ev.user_hash
            )
        );

        let ev = CacheObserveEvent::new(
            "req-003",
            "user-1",
            CacheStatus::Miss,
            1_200,
            0,
            Some("POOL_EXHAUSTED".to_string()),
        );
        assert_eq!(
            ev.to_json_log(),
            format!(
                "{{\"request_id\":\"req-003\",\"user_hash\":\"{}\",\"cache_status\":\"miss\",\"ttft_ms\":1200,\"bytes\":0,\"pool_error_code\":\"POOL_EXHAUSTED\"}}",
                ev.user_hash
            )
        );
    }

    #[test]
    fn json_log_escapes_quote_backslash_newline_and_control_chars() {
        let ev = CacheObserveEvent::new(
            "a\"b\\c\nd\u{0001}e",
            "user-1",
            CacheStatus::Hit,
            1,
            2,
            Some("x\"y".to_string()),
        );
        let line = ev.to_json_log();
        assert_eq!(
            line,
            format!(
                "{{\"request_id\":\"a\\\"b\\\\c\\nd\\u0001e\",\"user_hash\":\"{}\",\"cache_status\":\"hit\",\"ttft_ms\":1,\"bytes\":2,\"pool_error_code\":\"x\\\"y\"}}",
                ev.user_hash
            )
        );
        assert!(!line.contains('\n'), "log must stay one line: {line}");
    }

    #[test]
    fn from_play_maps_status_and_pool_error_codes() {
        let ev = CacheObserveEvent::from_play("req-001", "user-1", true, 12, 40_000, None);
        assert_eq!(ev.cache_status, CacheStatus::Hit);
        assert_eq!(ev.pool_error_code, None);

        let err = PlayError::PoolQuarantined {
            retry_after_ms: 500,
        };
        let ev = CacheObserveEvent::from_play("req-002", "user-1", false, 0, 0, Some(&err));
        assert_eq!(ev.cache_status, CacheStatus::Miss);
        assert_eq!(ev.pool_error_code.as_deref(), Some("POOL_QUARANTINED"));

        let ev = CacheObserveEvent::from_play(
            "req-003",
            "user-1",
            false,
            0,
            0,
            Some(&PlayError::PoolExhausted),
        );
        assert_eq!(ev.pool_error_code.as_deref(), Some("POOL_EXHAUSTED"));

        // Non-pool failures are not pool error codes.
        let ev = CacheObserveEvent::from_play(
            "req-004",
            "user-1",
            false,
            0,
            0,
            Some(&PlayError::WrongTrack),
        );
        assert_eq!(ev.pool_error_code, None);
    }

    #[test]
    fn staging_query_weights_aggregates_by_sample_interval() {
        let q = STAGING_QUERY_TEMPLATE;
        for token in [
            "SUM(_sample_interval) AS plays",
            "SUM(_sample_interval * double2) AS bytes",
            "SUM(_sample_interval * double1) / SUM(_sample_interval) AS avg_ttft_ms",
            "quantileExactWeighted(0.95)(double1, _sample_interval)",
        ] {
            assert!(
                q.contains(token),
                "staging query must contain {token}:\n{q}"
            );
        }
        for banned in ["COUNT(*)", "AVG(double1)", "quantileExact(0.95)"] {
            assert!(
                !q.contains(banned),
                "staging query must not use unweighted/unsupported {banned}:\n{q}"
            );
        }
    }
}
