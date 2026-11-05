//! Independent-session registration/legacy-callback ordering on real PostgreSQL.
//! These are database fences, not a deployed-worker drain or network proof.

use super::*;

async fn backend(db: &Client) -> i32 {
    db.query_one("SELECT pg_backend_pid()", &[])
        .await
        .quiet("registration backend pid")
        .get(0)
}

async fn wait_for_lock(observer: &Client, pid: i32) {
    for _ in 0..5000 {
        if count(
            observer,
            "SELECT count(*) FROM pg_stat_activity WHERE pid = $1 AND wait_event_type = 'Lock'",
            &[&pid],
        )
        .await
            == 1
        {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("registration contender never reached row lock");
}

async fn registration_first(commit: bool) {
    let db = connect().await;
    let (id, s) = legacy_fixture(&db, 0x71).await;
    let original = row_digest(&db, &id).await;
    let registrar = connect().await;
    registrar
        .batch_execute("BEGIN")
        .await
        .quiet("begin registration");
    register(&registrar, &id, &s.hash).await;
    let registered = legacy_snapshot(&registrar, &id, &s.hash).await;
    assert!(registered.iter().all(Option::is_some));
    assert!(row_digest(&registrar, &id).await == original);

    let writer = connect().await;
    let pid = backend(&writer).await;
    let write_id = id.clone();
    let task = tokio::spawn(async move {
        let at: i64 = 1_700_000_000;
        writer
            .execute(touch_retention_check_sql(), &[&write_id, &at])
            .await
    });
    wait_for_lock(&db, pid).await;
    registrar
        .batch_execute(if commit { "COMMIT" } else { "ROLLBACK" })
        .await
        .quiet("finish registration barrier");
    let result = task.await.expect("legacy callback task");
    if commit {
        expect_fenced(result, "callback behind committed registration").await;
        assert!(legacy_snapshot(&db, &id, &s.hash).await == registered);
    } else {
        assert_eq!(result.quiet("callback behind rolled-back registration"), 1);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_registry WHERE plaintext_sha256 = $1",
                &[&s.hash]
            )
            .await,
            0
        );
        assert_eq!(count(&db, "SELECT count(*) FROM posted_files WHERE id = $1 AND last_retention_check = 1700000000", &[&id]).await, 1);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_health WHERE generation_id = $1",
                &[&id]
            )
            .await,
            0
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_references WHERE generation_id = $1",
                &[&id]
            )
            .await,
            0
        );
    }
}

#[test]
fn postgres_registration_first_blocks_and_refuses_read_committed_legacy_callback() {
    run_test(registration_first(true));
}

#[test]
fn postgres_registration_rollback_releases_legacy_callback_without_registering() {
    run_test(registration_first(false));
}

async fn callback_first(commit: bool) {
    let db = connect().await;
    let (id, s) = legacy_fixture(&db, 0x72).await;
    let original = row_digest(&db, &id).await;
    let writer = connect().await;
    writer
        .batch_execute("BEGIN")
        .await
        .quiet("begin old callback");
    let at: i64 = 1_700_000_000;
    writer
        .execute(touch_post_verified_sql(), &[&id, &at])
        .await
        .quiet("old callback before registration");
    let changed = row_digest(&writer, &id).await;
    assert!(changed != original);

    let registrar = connect().await;
    let pid = backend(&registrar).await;
    let reg_id = id.clone();
    let hash = s.hash.clone();
    let task = tokio::spawn(async move {
        registrar
            .batch_execute("BEGIN")
            .await
            .quiet("begin waiting registration");
        register(&registrar, &reg_id, &hash).await;
        registrar
            .batch_execute("COMMIT")
            .await
            .quiet("commit waiting registration");
    });
    wait_for_lock(&db, pid).await;
    writer
        .batch_execute(if commit { "COMMIT" } else { "ROLLBACK" })
        .await
        .quiet("release old callback");
    task.await.expect("registration task");
    assert!(row_digest(&db, &id).await == if commit { changed } else { original });
    let registered = legacy_snapshot(&db, &id, &s.hash).await;
    assert!(registered.iter().all(Option::is_some));
    assert_eq!(count(&db, "SELECT count(*) FROM posted_file_health WHERE generation_id = $1 AND state = 'unknown' AND verified_at IS NULL AND retention_checked_at IS NULL", &[&id]).await, 1);
    assert!(is_fenced(&db, &id).await);
    assert!(legacy_snapshot(&db, &id, &s.hash).await == registered);
}

#[test]
fn postgres_callback_first_registration_waits_and_never_promotes_old_timestamp() {
    run_test(callback_first(true));
}

#[test]
fn postgres_callback_rollback_registration_waits_and_keeps_original_custody() {
    run_test(callback_first(false));
}

#[test]
fn postgres_registration_invalidates_older_repeatable_and_serializable_snapshots() {
    run_test(async {
        for begin in [
            "BEGIN ISOLATION LEVEL REPEATABLE READ",
            "BEGIN ISOLATION LEVEL SERIALIZABLE",
        ] {
            let db = connect().await;
            let (id, s) = legacy_fixture(&db, 0x73).await;
            let writer = connect().await;
            writer
                .batch_execute(begin)
                .await
                .quiet("begin stale snapshot");
            let original = row_digest(&writer, &id).await;
            assert_eq!(
                count(
                    &writer,
                    "SELECT count(*) FROM posted_file_registry WHERE plaintext_sha256 = $1",
                    &[&s.hash]
                )
                .await,
                0
            );
            register(&db, &id, &s.hash).await;
            let registered = legacy_snapshot(&db, &id, &s.hash).await;
            assert!(row_digest(&db, &id).await == original);
            let at: i64 = 1_700_000_000;
            expect_db_error(
                writer
                    .execute(touch_retention_check_sql(), &[&id, &at])
                    .await,
                &SqlState::T_R_SERIALIZATION_FAILURE,
                None,
                "stale snapshot callback",
            );
            writer
                .batch_execute("ROLLBACK")
                .await
                .quiet("rollback stale snapshot");
            assert!(legacy_snapshot(&db, &id, &s.hash).await == registered);
        }
    });
}

#[test]
fn postgres_competing_registrations_serialize_without_changing_the_bundle() {
    run_test(async {
        let db = connect().await;
        let (id, s) = legacy_fixture(&db, 0x74).await;
        let registrar = connect().await;
        registrar
            .batch_execute("BEGIN")
            .await
            .quiet("begin first registration");
        register(&registrar, &id, &s.hash).await;
        let registered = legacy_snapshot(&registrar, &id, &s.hash).await;
        let contender = connect().await;
        let pid = backend(&contender).await;
        let reg_id = id.clone();
        let hash = s.hash.clone();
        let task = tokio::spawn(async move {
            register(&contender, &reg_id, &hash).await;
        });
        wait_for_lock(&db, pid).await;
        registrar
            .batch_execute("COMMIT")
            .await
            .quiet("commit first registration");
        task.await.expect("competing registration task");
        assert!(legacy_snapshot(&db, &id, &s.hash).await == registered);
        assert!(is_fenced(&db, &id).await);
        assert_eq!(custody_rows(&db, &s).await, 1);
    });
}

#[test]
fn postgres_registration_replay_keeps_fences_after_historical_schema_replay() {
    run_blocking(true, async {
        let db = connect().await;
        let (id, s) = registered_legacy(&db, 0x75).await;
        let before = legacy_snapshot(&db, &id, &s.hash).await;
        db.batch_execute(MIGRATION_0043)
            .await
            .quiet("replay registration migration");
        db.batch_execute(MIGRATION_0037)
            .await
            .quiet("historical generation replay");
        db.batch_execute(MIGRATION_0042)
            .await
            .quiet("historical callback replay");
        db.batch_execute(MIGRATION_0043)
            .await
            .quiet("repeat registration migration");
        assert!(legacy_snapshot(&db, &id, &s.hash).await == before);
        assert_eq!(count(&db, "SELECT count(*) FROM pg_trigger WHERE tgrelid = 'public.posted_file_registry'::regclass AND tgname = 'serialize_posted_file_registration'", &[]).await, 1);
        assert!(is_fenced(&db, &id).await);
        registration_first(true).await;
    });
}
