//! Test-first contract pins for deletion writer fences.
//!
//! These checks deliberately exercise the worker's parameterized SQL rather
//! than a live database. The ADR requires the fence to be checked in the same
//! transaction as each write, with locks held through commit.

use nntune_api::library;

#[test]
fn api_library_mutations_take_user_fence_and_require_active_account() {
    for (name, sql) in [
        ("genre insert", library::genre_insert_statement()),
        ("genre update", library::genre_update_statement()),
        ("genre delete", library::genre_delete_statement()),
        ("playlist insert", library::playlist_insert_statement()),
        ("playlist update", library::playlist_update_statement()),
    ] {
        assert!(
            sql.contains("FOR SHARE")
                && sql.contains("users")
                && sql.contains("deleting_at IS NULL"),
            "{name} must lock its owner row and reject a scheduled account deletion: {sql}"
        );
    }
}

#[test]
fn playlist_mutations_fence_target_and_account_before_writing() {
    for (name, sql) in [
        ("playlist insert", library::playlist_insert_statement()),
        ("playlist update", library::playlist_update_statement()),
    ] {
        assert!(
            sql.contains("playlists") && sql.contains("deleting_at IS NULL"),
            "{name} must reject a deleting playlist target: {sql}"
        );
        let owner_lock = sql.find("users").unwrap_or(usize::MAX);
        let target_lock = sql.find("playlists").unwrap_or(usize::MAX);
        assert!(
            sql.contains("FOR SHARE") && owner_lock < target_lock && sql.contains("playlists"),
            "{name} must acquire the owner fence before the playlist target fence: {sql}"
        );
    }
}

#[test]
fn fenced_api_writes_have_stable_conflict_outcomes() {
    let source = include_str!("../src/library.rs");
    for code in ["account_deleting", "container_deleting"] {
        assert!(
            source.contains(code),
            "fenced writes must expose stable 409 code {code} with blocking operation id"
        );
    }
    assert!(
        source.contains("409") && source.contains("operation_id") && source.contains("retry_after"),
        "fence conflicts must carry HTTP 409, operation_id, and retry_after"
    );
}

#[test]
fn active_account_write_sql_is_not_rejected_by_the_fence() {
    for sql in [
        library::genre_insert_statement(),
        library::genre_update_statement(),
        library::genre_delete_statement(),
        library::playlist_insert_statement(),
        library::playlist_update_statement(),
    ] {
        assert!(
            sql.contains("deleting_at IS NULL"),
            "active accounts (NULL deleting_at) must retain the successful write path: {sql}"
        );
    }
}
