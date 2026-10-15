//! Acceptance tests for [TOG-25178](/TOG/issues/TOG-25178) (roadmap R3):
//! cache observability with Analytics Engine and structured logs.
//!
//! Builder contract (implement in `workers/stream/src/observability.rs`,
//! wired to the `METER` Analytics Engine dataset in `wrangler.toml`):
//!
//! ```rust,ignore
//! pub enum CacheStatus { Hit, Miss }
//! pub struct CacheObserveEvent {
//!     pub request_id: String,
//!     pub user_hash: String, // hex SHA-256 of the raw user id, never raw
//!     pub cache_status: CacheStatus,
//!     pub ttft_ms: u64,
//!     pub bytes: u64,
//!     pub pool_error_code: Option<String>,
//! }
//! impl CacheObserveEvent {
//!     pub fn new(
//!         request_id: impl Into<String>,
//!         user_id: &str, // raw id in, hashed before storage
//!         cache_status: CacheStatus,
//!         ttft_ms: u64,
//!         bytes: u64,
//!         pool_error_code: Option<String>,
//!     ) -> Self;
//!     pub fn to_json_log(&self) -> String; // structured JSON log line
//!     pub fn analytics_blobs(&self) -> Vec<String>; // string columns
//!     pub fn analytics_doubles(&self) -> Vec<f64>; // numeric columns
//! }
//! pub fn hash_user(user_id: &str) -> String; // lowercase hex SHA-256, 64 chars
//! pub const STAGING_QUERY_TEMPLATE: &str; // Analytics Engine SQL over nntune_meter
//! ```
//!
//! Failing command:
//! `cargo test -p nntune-stream --test cache_observability`
//!
//! RED reason (2026-10-15): `nntune_stream::observability` does not exist
//! yet, so this target fails to compile with `unresolved import`. The
//! Builder on [TOG-25178](/TOG/issues/TOG-25178) turns it green without
//! editing this file.

use nntune_stream::observability::{
    hash_user, CacheObserveEvent, CacheStatus, STAGING_QUERY_TEMPLATE,
};

fn hit_event() -> CacheObserveEvent {
    CacheObserveEvent::new("req-hit-001", "user-1", CacheStatus::Hit, 12, 40_000, None)
}

fn miss_event() -> CacheObserveEvent {
    CacheObserveEvent::new(
        "req-miss-002",
        "user-1",
        CacheStatus::Miss,
        850,
        60_000,
        None,
    )
}

#[test]
fn hit_row_carries_hit_status_ttft_bytes_and_no_pool_error() {
    let ev = hit_event();
    let blobs = ev.analytics_blobs();
    let joined = blobs.join("|");
    assert!(
        joined.contains("hit"),
        "Analytics blobs must carry cache_status=hit, got: {joined}"
    );
    assert!(
        !joined.contains("miss"),
        "hit row must not carry miss status, got: {joined}"
    );
    let doubles = ev.analytics_doubles();
    assert!(
        doubles.contains(&12.0),
        "Analytics doubles must carry ttft_ms=12, got: {doubles:?}"
    );
    assert!(
        doubles.contains(&40_000.0),
        "Analytics doubles must carry bytes=40000, got: {doubles:?}"
    );
    assert!(
        ev.pool_error_code.is_none(),
        "clean hit must have no pool error code"
    );
}

#[test]
fn miss_row_carries_miss_status_ttft_bytes() {
    let ev = miss_event();
    let blobs = ev.analytics_blobs();
    let joined = blobs.join("|");
    assert!(
        joined.contains("miss"),
        "Analytics blobs must carry cache_status=miss, got: {joined}"
    );
    let doubles = ev.analytics_doubles();
    assert!(
        doubles.contains(&850.0),
        "Analytics doubles must carry ttft_ms=850, got: {doubles:?}"
    );
    assert!(
        doubles.contains(&60_000.0),
        "Analytics doubles must carry bytes=60000, got: {doubles:?}"
    );
}

#[test]
fn provider_error_row_carries_pool_error_code() {
    let ev = CacheObserveEvent::new(
        "req-err-003",
        "user-1",
        CacheStatus::Miss,
        1_200,
        0,
        Some("POOL_QUARANTINED".to_string()),
    );
    assert_eq!(
        ev.pool_error_code.as_deref(),
        Some("POOL_QUARANTINED"),
        "provider failure must surface a pool error code for metering"
    );
    let blobs = ev.analytics_blobs();
    let joined = blobs.join("|");
    assert!(
        joined.contains("POOL_QUARANTINED"),
        "Analytics blobs must carry the pool error code, got: {joined}"
    );
}

#[test]
fn structured_log_is_json_with_request_id_user_hash_cache_status() {
    let ev = hit_event();
    let line = ev.to_json_log();
    // Keep this dependency-free: pin the JSON shape by hand (the stream
    // core stays dependency-free so `cargo test` runs anywhere).
    for key in [
        "\"request_id\"",
        "\"user_hash\"",
        "\"cache_status\"",
        "\"ttft_ms\"",
        "\"bytes\"",
    ] {
        assert!(
            line.contains(key),
            "structured log must contain {key}, got: {line}"
        );
    }
    assert!(
        line.contains("\"req-hit-001\""),
        "log must carry the request id, got: {line}"
    );
    assert!(
        line.contains("\"hit\""),
        "log must carry cache_status=hit, got: {line}"
    );
    // Must be one JSON object per line (Workers Logs greppable).
    let trimmed = line.trim();
    assert!(
        trimmed.starts_with('{') && trimmed.ends_with('}'),
        "log line must be a single JSON object, got: {line}"
    );
    assert!(
        !trimmed.contains('\n'),
        "log line must not span multiple lines, got: {line}"
    );
}

#[test]
fn log_and_row_never_contain_the_raw_user_id() {
    let raw = "user-1-sensitive-id";
    let ev = CacheObserveEvent::new("req-pii-004", raw, CacheStatus::Hit, 5, 100, None);
    let line = ev.to_json_log();
    assert!(
        !line.contains(raw),
        "structured log must never contain the raw user id, got: {line}"
    );
    let blobs = ev.analytics_blobs().join("|");
    assert!(
        !blobs.contains(raw),
        "Analytics blobs must never contain the raw user id, got: {blobs}"
    );
    let debug = format!("{ev:?}");
    assert!(
        !debug.contains(raw),
        "event Debug must never leak the raw user id, got: {debug}"
    );
    assert!(
        line.contains(&ev.user_hash) || blobs.contains(&ev.user_hash),
        "hashed identity must be present so plays stay attributable"
    );
}

#[test]
fn user_hash_is_sha256_hex_not_raw() {
    let h = hash_user("user-1");
    assert_eq!(h.len(), 64, "user hash must be 64 hex chars, got: {h}");
    assert!(
        h.chars().all(|c| c.is_ascii_hexdigit()),
        "user hash must be lowercase hex, got: {h}"
    );
    assert!(
        !h.contains("user-1"),
        "hash must not embed the raw id, got: {h}"
    );
    // SHA-256("abc") is the canonical vector; the hasher must be real SHA-256.
    assert_eq!(
        hash_user("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "user hash must be SHA-256 hex"
    );
    assert_eq!(
        hash_user("user-1"),
        hash_user("user-1"),
        "hashing must be deterministic per user"
    );
    assert_ne!(
        hash_user("user-1"),
        hash_user("user-2"),
        "distinct users must hash distinctly"
    );
}

#[test]
fn staging_query_template_selects_required_columns() {
    let q = STAGING_QUERY_TEMPLATE;
    for token in [
        "nntune_meter",
        "cache_status",
        "ttft_ms",
        "bytes",
        "pool_error_code",
    ] {
        assert!(
            q.contains(token),
            "staging query must reference {token}, got:\n{q}"
        );
    }
    let upper = q.to_ascii_uppercase();
    assert!(
        upper.contains("SELECT"),
        "staging query must be a SELECT, got:\n{q}"
    );
    assert!(
        upper.contains("FROM"),
        "staging query must have a FROM, got:\n{q}"
    );
    // Hit-rate template must split by cache status and bound the window.
    assert!(
        upper.contains("GROUP BY") || upper.contains("COUNT"),
        "staging query must aggregate by cache status for hit-rate, got:\n{q}"
    );
    assert!(
        upper.contains("TIMESTAMP") || upper.contains("INTERVAL") || upper.contains("WHERE"),
        "staging query must bound the time window, got:\n{q}"
    );
}
