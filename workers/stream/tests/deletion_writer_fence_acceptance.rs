//! Test-first acceptance pins for stream publication racing deletion.
//!
//! The stream writer must fence media, grant, and play-event publication before
//! committing. These source-level checks are deliberately runnable without a
//! database or Worker runtime; database race coverage remains for the writer's
//! transaction tests once that persistence path is implemented.

const STREAM_SOURCE: &str = include_str!("../src/lib.rs");

#[test]
fn stream_publication_uses_user_fence_for_all_user_owned_writes() {
    for table in ["track_media", "user_cache_grants", "play_event_outbox"] {
        assert!(
            STREAM_SOURCE.contains(table),
            "stream publication contract must include {table}"
        );
    }
    assert!(
        STREAM_SOURCE.contains("FOR SHARE")
            && STREAM_SOURCE.contains("users")
            && STREAM_SOURCE.contains("deleting_at IS NULL"),
        "stream publication must lock and reject a deleting account before inserts"
    );
}

#[test]
fn fenced_stream_writes_return_stable_account_conflict_and_active_users_continue() {
    assert!(
        STREAM_SOURCE.contains("account_deleting")
            && STREAM_SOURCE.contains("operation_id")
            && STREAM_SOURCE.contains("retry_after")
            && STREAM_SOURCE.contains("409"),
        "fenced stream writes must return HTTP 409 account_deleting with operation_id and retry_after"
    );
    assert!(
        STREAM_SOURCE.contains("deleting_at IS NULL"),
        "an active account with NULL deleting_at must keep the successful publication path"
    );
}
