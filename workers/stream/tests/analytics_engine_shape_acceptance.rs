//! Acceptance tests: stream Analytics Engine payload shape stays unchanged.
//!
//! The worker-logging slice adds trace correlation without touching the
//! stream worker's existing Analytics Engine row or event vocabulary: one
//! finished play still maps to one row with stable blob/double positions,
//! the sampling index stays the hashed user identity, and the structured log
//! line keeps exactly its legacy keys. Correlation arrives additively in a
//! later slice; this pin must be updated deliberately if that ever changes
//! the shape asserted here.
//!
//! Static fixtures only: no live pool, no NNTP, no network.

use nntune_stream::observability::{hash_user, CacheObserveEvent, CacheStatus};
use std::collections::BTreeSet;

fn hit() -> CacheObserveEvent {
    CacheObserveEvent::new("req-001", "user-1", CacheStatus::Hit, 12, 40_000, None)
}

#[test]
fn analytics_engine_row_shape_and_positions_are_stable() {
    let event = hit();
    assert_eq!(
        event.analytics_blobs(),
        vec![
            "hit".to_string(),
            event.user_hash.clone(),
            "req-001".to_string(),
            String::new(),
        ],
        "blobs stay [cache_status, user_hash, request_id, pool_error_code]"
    );
    assert_eq!(event.analytics_doubles(), vec![12.0, 40_000.0]);
    assert_eq!(event.analytics_index(), event.user_hash);
    assert_eq!(event.analytics_index().len(), 64);
    assert_ne!(event.analytics_index(), event.cache_status.as_str());
}

#[test]
fn pool_error_projects_to_blob4_without_moving_other_columns() {
    let event = CacheObserveEvent::new(
        "req-009",
        "user-9",
        CacheStatus::Miss,
        1_200,
        0,
        Some("POOL_EXHAUSTED".to_string()),
    );
    let blobs = event.analytics_blobs();
    assert_eq!(blobs[0], "miss");
    assert_eq!(blobs[2], "req-009");
    assert_eq!(blobs[3], "POOL_EXHAUSTED");
    assert_eq!(event.analytics_doubles(), vec![1_200.0, 0.0]);
    assert_eq!(event.analytics_index(), event.user_hash);
}

#[test]
fn json_log_keeps_exactly_its_legacy_keys() {
    let parsed: serde_json::Value = serde_json::from_str(&hit().to_json_log()).unwrap();
    let object = parsed.as_object().unwrap();
    let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "request_id",
            "user_hash",
            "cache_status",
            "ttft_ms",
            "bytes",
            "pool_error_code",
        ]),
        "stream log shape is unchanged; correlation stays additive"
    );
    assert_eq!(object["cache_status"], "hit");
    assert_eq!(object["ttft_ms"], 12);
    assert_eq!(object["bytes"], 40_000);
    assert_eq!(object["pool_error_code"], serde_json::Value::Null);
}

#[test]
fn raw_user_id_never_leaves_the_event() {
    let event = hit();
    assert_eq!(event.user_hash, hash_user("user-1"));
    assert_eq!(event.user_hash.len(), 64);
    assert_ne!(hash_user("user-1"), hash_user("user-2"));
    let debug = format!("{event:?}");
    let blobs = event.analytics_blobs().join("\n");
    for rendering in [debug, blobs, event.to_json_log()] {
        assert!(
            !rendering.contains("user-1"),
            "raw user id leaked into {rendering}"
        );
    }
}
