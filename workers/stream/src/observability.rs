//! Cache observability: one Analytics Engine row + one structured log line
//! per play.
//!
//! The play path ([`crate::play`]) already emits [`crate::meter`] events for
//! the usage ledger. This module is the Analytics Engine projection of the
//! same outcome: a single [`CacheObserveEvent`] per play carrying the cache
//! hit/miss signal, time-to-first-byte, bytes served and the pool error code
//! (M4 metering builds on this row shape).
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
//!   indexes: [ev.cache_status],       // sample on the hit/miss index
//! });
//! console.log(ev.to_json_log()); // structured JSON, Workers Logs greppable
//! ```

use crate::grant::sha256;

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
pub const STAGING_QUERY_TEMPLATE: &str = "\
-- nntune stream cache hit-rate + TTFT (staging evidence query).\n\
-- Column mapping: blob1 = cache_status, blob2 = user_hash,\n\
-- blob3 = request_id, blob4 = pool_error_code; double1 = ttft_ms,\n\
-- double2 = bytes. Dataset `nntune_meter` is the METER binding in\n\
-- workers/stream/wrangler.toml.\n\
SELECT\n\
  blob1 AS cache_status,\n\
  blob4 AS pool_error_code,\n\
  COUNT(*) AS plays,\n\
  SUM(double2) AS bytes,\n\
  AVG(double1) AS avg_ttft_ms,\n\
  quantileExact(0.95)(double1) AS p95_ttft_ms\n\
FROM nntune_meter\n\
WHERE timestamp > NOW() - INTERVAL '24' HOUR\n\
GROUP BY cache_status, pool_error_code\n\
ORDER BY cache_status;";
