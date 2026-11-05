//! The final, irreversible lifecycle case in the disposable Actions bank.
//! Its lexically last name and the runner's single test thread are deliberate:
//! no test may reopen the gate or run legacy-fixture setup after retirement.
//! Committed cutoff/replay is database evidence, not a deployed-worker drain.

use super::posted_generation_legacy_fence::{insert_legacy, legacy_fixture, FIELD_UPDATES};
use super::*;
use nntune_ingest::posted_files::touch_retention_check_sql;
use nntune_ingest::posted_generations::{begin_verification_round, MutationOutcome};

const RETIRE: &str = "UPDATE posted_file_legacy_writer_gate SET retired = TRUE WHERE singleton";
const RETIRED: &str = "legacy posted file writes are retired";
const MONOTONIC: &str = "legacy writer retirement is monotonic";

async fn backend(db: &Client) -> i32 {
    db.query_one("SELECT pg_backend_pid()", &[])
        .await
        .quiet("retirement backend pid")
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
    panic!("retirement contender never reached lock barrier");
}

async fn gate_is_retired(db: &Client) -> bool {
    db.query_one(
        "SELECT retired FROM posted_file_legacy_writer_gate WHERE singleton",
        &[],
    )
    .await
    .quiet("read retirement gate")
    .get(0)
}

async fn fresh_sealed(db: &Client) -> Sealed {
    let uuid = db_uuid(db).await;
    let hash = digest(db, "retirement-content", &uuid).await;
    let plan = digest(db, "retirement-plan", &uuid).await;
    Sealed::new(&uuid, &hash, &plan, 0x75)
}

async fn custody_digest(db: &Client) -> Option<String> {
    db.query_one(
        "SELECT md5(string_agg(t::text, ',' ORDER BY t.id)) FROM posted_files t",
        &[],
    )
    .await
    .quiet("retirement custody snapshot")
    .get(0)
}

async fn expect_retired(result: Result<u64, tokio_postgres::Error>) {
    expect_db_error(
        result,
        &SqlState::RAISE_EXCEPTION,
        Some(RETIRED),
        "retired legacy writer",
    );
}

#[test]
fn zz_postgres_retirement_lifecycle_serializes_inserts_health_snapshots_and_replay() {
    run_blocking(true, async {
        let mut db = connect().await;
        assert!(
            !gate_is_retired(&db).await,
            "a fresh disposable bank starts with retirement OFF"
        );
        let (legacy_id, legacy) = legacy_fixture(&db, 0x76).await;
        let at: i64 = 1_700_000_000;
        assert_eq!(
            db.execute(touch_retention_check_sql(), &[&legacy_id, &at])
                .await
                .quiet("unretired callback"),
            1
        );

        // Retirement-first rollback releases BOTH kinds of waiting legacy
        // writes. No uncommitted cutoff can be mistaken for a durable cutoff.
        let retiring = connect().await;
        retiring
            .batch_execute("BEGIN")
            .await
            .quiet("begin rolled-back cutoff");
        assert_eq!(
            retiring
                .execute(RETIRE, &[])
                .await
                .quiet("tentative cutoff"),
            1
        );
        let inserter = connect().await;
        let insert_pid = backend(&inserter).await;
        let inserted = fresh_sealed(&db).await;
        let insert_copy = inserted.clone();
        let insert_task = tokio::spawn(async move { insert_legacy(&inserter, &insert_copy).await });
        let writer = connect().await;
        let write_pid = backend(&writer).await;
        let write_id = legacy_id.clone();
        let write_task = tokio::spawn(async move {
            writer
                .execute(
                    "UPDATE posted_files SET verified_count = 1 WHERE id = $1",
                    &[&write_id],
                )
                .await
        });
        wait_for_lock(&db, insert_pid).await;
        wait_for_lock(&db, write_pid).await;
        retiring
            .batch_execute("ROLLBACK")
            .await
            .quiet("rollback cutoff");
        assert_eq!(
            insert_task
                .await
                .expect("rollback insert task")
                .quiet("insert after cutoff rollback"),
            1
        );
        assert_eq!(
            write_task
                .await
                .expect("rollback health task")
                .quiet("health after cutoff rollback"),
            1
        );
        assert!(!gate_is_retired(&db).await);
        assert_eq!(custody_rows(&db, &inserted).await, 1);

        // Keep this row's physical tuple unchanged across retirement, so a
        // stale callback's serialization failure must come from the GATE, not
        // from the separate in-flight writer's custody update.
        let (stable_id, _) = legacy_fixture(&db, 0x77).await;
        let stable_xmin: String = db
            .query_one(
                "SELECT xmin::text FROM posted_files WHERE id = $1",
                &[&stable_id],
            )
            .await
            .quiet("snapshot untouched legacy tuple")
            .get(0);
        // Establish old MVCC snapshots BEFORE the committed cutoff, without
        // holding the shared retirement lock. Both legacy insert and callback
        // must subsequently fail serialization, not see a permanently open gate.
        let mut snapshots = Vec::new();
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
            for insert in [true, false] {
                let stale = connect().await;
                stale
                    .batch_execute(&format!("BEGIN ISOLATION LEVEL {isolation}"))
                    .await
                    .quiet("begin pre-cutoff snapshot");
                assert!(!gate_is_retired(&stale).await);
                assert_eq!(
                    count(
                        &stale,
                        "SELECT count(*) FROM posted_files WHERE id = $1",
                        &[&stable_id]
                    )
                    .await,
                    1
                );
                snapshots.push((stale, insert));
            }
        }

        // Two independent old writers hold the shared lock through commit.
        // The cutoff cannot commit after only one of them has finished.
        let inserter = connect().await;
        inserter
            .batch_execute("BEGIN")
            .await
            .quiet("begin old insert");
        let in_flight = fresh_sealed(&db).await;
        assert_eq!(
            insert_legacy(&inserter, &in_flight)
                .await
                .quiet("old insert before cutoff"),
            1
        );
        let writer = connect().await;
        writer
            .batch_execute("BEGIN")
            .await
            .quiet("begin old health");
        assert_eq!(
            writer
                .execute(
                    "UPDATE posted_files SET post_copy = 'present' WHERE id = $1",
                    &[&legacy_id]
                )
                .await
                .quiet("old health before cutoff"),
            1
        );
        let retiring = connect().await;
        let retire_pid = backend(&retiring).await;
        let retire_task = tokio::spawn(async move { retiring.execute(RETIRE, &[]).await });
        wait_for_lock(&db, retire_pid).await;
        assert!(!gate_is_retired(&db).await);
        inserter
            .batch_execute("COMMIT")
            .await
            .quiet("finish old insert");
        wait_for_lock(&db, retire_pid).await;
        assert!(!gate_is_retired(&db).await);
        writer
            .batch_execute("COMMIT")
            .await
            .quiet("finish old health");
        assert_eq!(
            retire_task
                .await
                .expect("retirement task")
                .quiet("commit cutoff"),
            1
        );
        assert!(gate_is_retired(&db).await);
        assert_eq!(custody_rows(&db, &in_flight).await, 1);
        let before = custody_digest(&db).await;
        let current_xmin: String = db
            .query_one(
                "SELECT xmin::text FROM posted_files WHERE id = $1",
                &[&stable_id],
            )
            .await
            .quiet("check untouched legacy tuple")
            .get(0);
        assert!(current_xmin == stable_xmin);

        for (stale, insert) in snapshots {
            let result = if insert {
                let s = fresh_sealed(&db).await;
                insert_legacy(&stale, &s).await
            } else {
                stale
                    .execute(
                        "UPDATE posted_files SET verify_copy = 'present' WHERE id = $1",
                        &[&stable_id],
                    )
                    .await
            };
            expect_db_error(
                result,
                &SqlState::T_R_SERIALIZATION_FAILURE,
                None,
                "pre-cutoff snapshot writer",
            );
            stale
                .batch_execute("ROLLBACK")
                .await
                .quiet("rollback stale writer");
        }
        assert!(custody_digest(&db).await == before);

        // Retirement-first COMMIT: even sessions started before it cannot
        // insert or write health after waiting on its exclusive gate row lock.
        let retiring = connect().await;
        retiring
            .batch_execute("BEGIN")
            .await
            .quiet("begin idempotent cutoff");
        assert_eq!(
            retiring.execute(RETIRE, &[]).await.quiet("repeat cutoff"),
            1
        );
        let inserter = connect().await;
        let insert_pid = backend(&inserter).await;
        let refused = fresh_sealed(&db).await;
        let insert_copy = refused.clone();
        let insert_task = tokio::spawn(async move { insert_legacy(&inserter, &insert_copy).await });
        let writer = connect().await;
        let write_pid = backend(&writer).await;
        let write_id = legacy_id.clone();
        let write_task = tokio::spawn(async move {
            writer
                .execute(
                    "UPDATE posted_files SET verify_copy = 'present' WHERE id = $1",
                    &[&write_id],
                )
                .await
        });
        wait_for_lock(&db, insert_pid).await;
        wait_for_lock(&db, write_pid).await;
        retiring
            .batch_execute("COMMIT")
            .await
            .quiet("finish idempotent cutoff");
        expect_retired(insert_task.await.expect("retired insert task")).await;
        expect_retired(write_task.await.expect("retired health task")).await;
        assert_eq!(custody_rows(&db, &refused).await, 0);

        // BEFORE INSERT also fences the old ON CONFLICT DO NOTHING helper:
        // a hash conflict must never masquerade as persistence of a fresh mint.
        expect_retired(insert_legacy(&db, &legacy).await).await;
        for (_, by_id, by_hash) in FIELD_UPDATES {
            // Every value differs from this clean legacy row's defaults.
            let id: String = db
                .query_one(select_posted_file_id_by_sha_sql(), &[&inserted.hash])
                .await
                .quiet("clean old row id")
                .get(0);
            expect_retired(db.execute(by_id, &[&id]).await).await;
            expect_retired(db.execute(by_hash, &[&inserted.hash]).await).await;
        }
        let changed_at = at + 1;
        expect_retired(
            db.execute(touch_retention_check_sql(), &[&legacy_id, &changed_at])
                .await,
        )
        .await;
        assert_eq!(
            db.execute(
                "UPDATE posted_files SET last_retention_check = last_retention_check WHERE id = $1",
                &[&legacy_id]
            )
            .await
            .quiet("retired true no-op"),
            1
        );
        assert!(custody_digest(&db).await == before);

        // Managed admission/recovery and the current health API stay usable;
        // the cutoff never grants reuse, selection or multi-generation writes.
        let c = case(&db).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let handle = admitted(&mut db, &attempt, &c.sealed).await;
        let MutationOutcome::Applied(current) = begin_verification_round(&mut db, &handle)
            .await
            .expect("managed round after cutoff")
        else {
            panic!("managed round was refused after cutoff")
        };
        assert!(posting_fence_is_current(&mut db, &current)
            .await
            .expect("managed fence after cutoff"));
        assert!(matches!(
            recover(&mut db, &attempt, &c.sealed.hash, &c.sealed.op)
                .await
                .expect("managed recovery after cutoff"),
            AdmissionOutcome::ResumedSameCandidate(_, _)
        ));
        let managed_before = snapshot(&db, &c.sealed).await;
        let all_before = custody_digest(&db).await;

        // The production schema offers no reopen/delete/truncate path.
        for sql in [
            "UPDATE posted_file_legacy_writer_gate SET retired = FALSE WHERE singleton",
            "DELETE FROM posted_file_legacy_writer_gate",
            "TRUNCATE posted_file_legacy_writer_gate",
            "INSERT INTO posted_file_legacy_writer_gate (singleton) VALUES (TRUE) ON CONFLICT (singleton) DO UPDATE SET retired = EXCLUDED.retired",
        ] {
            expect_db_error(db.batch_execute(sql).await, &SqlState::RAISE_EXCEPTION, Some(MONOTONIC), "monotonic cutoff");
            assert!(gate_is_retired(&db).await);
        }
        db.batch_execute(MIGRATION_0045)
            .await
            .quiet("replay retirement migration");
        assert!(gate_is_retired(&db).await);
        assert!(custody_digest(&db).await == all_before);
        assert!(snapshot(&db, &c.sealed).await == managed_before);
        db.batch_execute(MIGRATION_0037)
            .await
            .quiet("replay historical preparation after cutoff");
        db.batch_execute(MIGRATION_0042)
            .await
            .quiet("replay historical health fence after cutoff");
        db.batch_execute(MIGRATION_0043)
            .await
            .quiet("replay historical registration fence after cutoff");
        db.batch_execute(MIGRATION_0044)
            .await
            .quiet("replay reference fence after cutoff");
        db.batch_execute(MIGRATION_0045)
            .await
            .quiet("replay retirement after historical migrations");
        assert!(gate_is_retired(&db).await);
        assert!(custody_digest(&db).await == all_before);
        assert!(snapshot(&db, &c.sealed).await == managed_before);
        expect_retired(insert_legacy(&db, &refused).await).await;
        assert_eq!(count(&db, "SELECT count(*) FROM pg_constraint WHERE conrelid = 'posted_files'::regclass AND contype = 'u' AND pg_get_constraintdef(oid) = 'UNIQUE (plaintext_sha256)'", &[]).await, 1);
    });
}
