//! ADR 0013 retires own-provider cache grants, not signed playback grants.
//! Keep the pool attribution work from reviving the unused BYOP publisher.

use nntune_ingest::transcode::{
    classify_cache_object_rows, upsert_cache_object_sql, PublishOutcome, CACHE_OBJECT_PARAM_ORDER,
};

#[test]
fn transcode_retires_per_user_publication_helpers() {
    let source = include_str!("../src/transcode.rs");
    for retired in [
        "user_cache_grants",
        "upsert_cache_grant_sql",
        "select_cache_grant_by_pk_sql",
        "CACHE_GRANT_PARAM_ORDER",
        "upsert_track_media_sql",
        "select_track_media_by_pk_sql",
        "TRACK_MEDIA_PARAM_ORDER",
        "classify_publish_zero_rows",
    ] {
        assert!(!source.contains(retired), "retired publisher: {retired}");
    }
}

#[test]
fn remaining_publication_is_content_global_and_idempotent() {
    assert_eq!(
        upsert_cache_object_sql(),
        "INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (content_hash) DO NOTHING"
    );
    assert_eq!(
        CACHE_OBJECT_PARAM_ORDER,
        [
            "content_hash",
            "byte_size",
            "r2_bucket",
            "r2_key",
            "last_verified_at"
        ]
    );
    assert_eq!(classify_cache_object_rows(0), PublishOutcome::Duplicate);
    assert_eq!(classify_cache_object_rows(1), PublishOutcome::Published);
}

#[test]
fn runtime_does_not_publish_own_provider_grants_or_per_user_artifact_mappings() {
    let source = include_str!("../src/queue_consumer.rs");
    for retired in [
        "upsert_cache_grant_sql",
        "select_cache_grant_by_pk_sql",
        "upsert_track_media_sql",
        "select_track_media_by_pk_sql",
        "INSERT INTO user_cache_grants",
        "INSERT INTO track_media",
    ] {
        assert!(
            !source.contains(retired),
            "retired runtime write: {retired}"
        );
    }
    assert!(source.contains("crate::transcode::upsert_cache_object_sql()"));
    assert!(source.contains("classify_cache_object_rows(affected)"));
}
