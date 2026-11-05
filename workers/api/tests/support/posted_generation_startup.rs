//! Atomic admitted-job startup and receipt preparation on disposable PostgreSQL.
//! These call the database API with synthetic trusted-dispatch inputs, not a
//! deployed coordinator or Queue consumer. Dropped replies simulate lost acks;
//! receipt insertion failure is injected, but commit-error recovery is not.

#[path = "posted_generation_owner_fence.rs"]
mod owner_fence;

use super::*;
use nntune_ingest::posted_generations::start_durable_attempt_acquisition;

async fn queued_case(db: &Client) -> (Case, String) {
    let uuid = db_uuid(db).await;
    let hash = digest(db, "content", &uuid).await;
    let plan = digest(db, "plan", &uuid).await;
    let owner = seed_user(db, &uuid).await;
    let job = format!("pg-job-{uuid}");
    let key = format!("ingest:{owner}:{hash}:upload");
    assert!(
        nntune_ingest::ingest_jobs::validate_admit_params(&job, &owner, &key, 1700000000).is_ok()
    );
    assert_eq!(
        db.execute(
            nntune_ingest::ingest_jobs::upsert_ingest_job_sql(),
            &[&job, &owner, &key, &1700000000_i64]
        )
        .await
        .quiet("admit queued fixture job"),
        1
    );
    (
        Case {
            sealed: Sealed::new(&uuid, &hash, &plan, 0x10),
            uuid,
            job,
            owner,
        },
        key,
    )
}

async fn start(
    db: &mut Client,
    c: &Case,
    key: &str,
    request: &str,
    expected: i64,
) -> AttemptAcquisition {
    match start_durable_attempt_acquisition(db, &c.job, &c.owner, key, request, expected).await {
        Ok(Some(receipt)) => receipt,
        _ => panic!("admitted startup refused"),
    }
}

async fn startup_state(db: &impl tokio_postgres::GenericClient, c: &Case) -> (String, String) {
    let rows = db
        .query_one(
            "SELECT md5(jsonb_build_array( \
         (SELECT to_jsonb(u) FROM users u WHERE u.id = $1), \
         (SELECT jsonb_agg(to_jsonb(r) ORDER BY r.request_id) \
          FROM posted_file_attempt_receipts r WHERE r.job_id = $2))::text)",
            &[&c.owner, &c.job],
        )
        .await
        .quiet("snapshot startup fences and receipts")
        .get(0);
    (state(db, c).await, rows)
}

async fn receipt_count(db: &Client, c: &Case) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_attempt_receipts WHERE job_id = $1",
        &[&c.job],
    )
    .await
}

async fn job_status(db: &Client, c: &Case) -> String {
    db.query_one("SELECT status FROM ingest_jobs WHERE id = $1", &[&c.job])
        .await
        .quiet("read startup status")
        .get(0)
}

#[test]
fn postgres_startup_atomically_starts_admitted_job_without_granting_attempt_authority() {
    run_test(async {
        let mut db = connect().await;
        let (c, key) = queued_case(&db).await;
        // The old running-only API must still refuse a queued job.
        let before = startup_state(&db, &c).await;
        assert!(matches!(
            prepare_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &c.uuid, 0).await,
            Ok(None)
        ));
        assert!(before == startup_state(&db, &c).await);
        let custody = snapshot(&db, &c.sealed).await;
        let receipt = start(&mut db, &c, &key, &c.uuid, 0).await;
        assert_eq!(job_status(&db, &c).await, "running");
        assert_eq!(receipt_count(&db, &c).await, 1);
        assert_eq!(attempt_rows(&db, &c.job).await, 0);
        assert!(custody == snapshot(&db, &c.sealed).await);
        assert_eq!(format!("{receipt:?}"), "AttemptAcquisition([REDACTED])");
        assert!(is_stale(
            &recover_attempt_acquisition(&mut db, &receipt).await
        ));
        let _handle = handle_of(
            acquire_prepared_attempt(&mut db, &receipt).await,
            "explicit acquire after startup",
        );
        assert_eq!(attempt_rows(&db, &c.job).await, 1);
    });
}

#[test]
fn postgres_startup_invalid_missing_foreign_admission_and_terminal_bindings_write_nothing() {
    run_test(async {
        let mut db = connect().await;
        let (c, key) = queued_case(&db).await;
        let (foreign, foreign_key) = queued_case(&db).await;
        let before = startup_state(&db, &c).await;
        for (job, owner, admission, request, expected) in [
            ("", c.owner.as_str(), key.as_str(), c.uuid.as_str(), 0),
            (c.job.as_str(), "a b", key.as_str(), c.uuid.as_str(), 0),
            (
                c.job.as_str(),
                c.owner.as_str(),
                "not-admitted",
                c.uuid.as_str(),
                0,
            ),
            (c.job.as_str(), c.owner.as_str(), key.as_str(), "a/b", 0),
            (
                c.job.as_str(),
                c.owner.as_str(),
                key.as_str(),
                c.uuid.as_str(),
                -1,
            ),
            (
                c.job.as_str(),
                c.owner.as_str(),
                key.as_str(),
                c.uuid.as_str(),
                i64::MAX,
            ),
        ] {
            assert!(matches!(
                start_durable_attempt_acquisition(
                    &mut db, job, owner, admission, request, expected
                )
                .await,
                Err(StoreError::InvalidInput)
            ));
            assert!(before == startup_state(&db, &c).await);
        }
        for (job, owner, admission, expected) in [
            (c.uuid.as_str(), c.owner.as_str(), key.as_str(), 0),
            (c.job.as_str(), foreign.owner.as_str(), key.as_str(), 0),
            (c.job.as_str(), c.owner.as_str(), foreign_key.as_str(), 0),
            (c.job.as_str(), c.owner.as_str(), key.as_str(), 1),
        ] {
            assert!(matches!(
                start_durable_attempt_acquisition(
                    &mut db, job, owner, admission, &c.uuid, expected
                )
                .await,
                Ok(None)
            ));
            assert!(before == startup_state(&db, &c).await);
        }
        for status in ["done", "failed"] {
            set_status(&db, &c.job, status).await;
            let stopped = startup_state(&db, &c).await;
            assert!(matches!(
                start_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &key, &c.uuid, 0)
                    .await,
                Ok(None)
            ));
            assert!(stopped == startup_state(&db, &c).await);
        }
        assert_eq!(receipt_count(&db, &c).await, 0);
        assert_eq!(receipt_count(&db, &foreign).await, 0);
    });
}

#[test]
fn postgres_startup_deleting_owner_refuses_new_start_and_existing_receipt_replay() {
    run_test(async {
        let mut db = connect().await;
        for already_started in [false, true] {
            let (c, key) = queued_case(&db).await;
            if already_started {
                let _receipt = start(&mut db, &c, &key, &c.uuid, 0).await;
            }
            db.execute(
                "UPDATE users SET deleting_at = clock_timestamp() WHERE id = $1",
                &[&c.owner],
            )
            .await
            .quiet("schedule fixture owner deletion");
            let before = startup_state(&db, &c).await;
            assert!(matches!(
                start_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &key, &c.uuid, 0)
                    .await,
                Ok(None)
            ));
            assert!(before == startup_state(&db, &c).await);
            assert_eq!(attempt_rows(&db, &c.job).await, 0);
        }
    });
}

#[test]
fn postgres_startup_restart_reloads_exact_receipt_and_pending_bundle_without_renewal() {
    run_test(async {
        let mut db = connect().await;
        let (c, key) = queued_case(&db).await;
        let receipt = start(&mut db, &c, &key, &c.uuid, 0).await;
        let prepared = startup_state(&db, &c).await;
        drop(receipt);
        drop(db);
        let mut db = connect().await;
        let receipt = start(&mut db, &c, &key, &c.uuid, 0).await;
        assert!(prepared == startup_state(&db, &c).await);
        let first = handle_of(
            acquire_prepared_attempt(&mut db, &receipt).await,
            "acquire restarted startup",
        );
        let _generation = admitted(&mut db, &first, &c.sealed).await;
        set_lease(&db, &c.job, 100).await;
        let acquired = startup_state(&db, &c).await;
        drop(receipt);
        drop(db);
        let mut db = connect().await;
        let receipt = start(&mut db, &c, &key, &c.uuid, 0).await;
        let restored = handle_of(
            recover_attempt_acquisition(&mut db, &receipt).await,
            "recover startup lost response",
        );
        assert!(restored == first);
        let Ok(AdmissionOutcome::ResumedSameCandidate(_, bundle)) =
            recover_pending(&mut db, &restored, &c.sealed.hash).await
        else {
            panic!("startup did not recover exact pending custody")
        };
        assert!(bundle.custody().expect("stored startup custody") == c.sealed.custody());
        assert!(acquired == startup_state(&db, &c).await);
    });
}

#[test]
fn postgres_startup_reset_to_queued_never_revives_receipt_or_attempt_history() {
    run_test(async {
        let mut db = connect().await;
        for receipt_history in [true, false] {
            let (c, key) = queued_case(&db).await;
            if receipt_history {
                let _receipt = start(&mut db, &c, &key, &c.uuid, 0).await;
            } else {
                set_status(&db, &c.job, "running").await;
                let _attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
                expire(&db, &c.job).await;
            }
            set_status(&db, &c.job, "queued").await;
            let before = startup_state(&db, &c).await;
            let new_request = db_uuid(&db).await;
            for request in [&c.uuid, &new_request] {
                for expected in [0, 1] {
                    assert!(matches!(
                        start_durable_attempt_acquisition(
                            &mut db, &c.job, &c.owner, &key, request, expected
                        )
                        .await,
                        Ok(None)
                    ));
                    assert!(before == startup_state(&db, &c).await);
                }
            }
        }
    });
}

#[test]
fn postgres_startup_running_replay_and_explicit_predecessor_rules_never_borrow_authority() {
    run_test(async {
        let mut db = connect().await;
        let (c, key) = queued_case(&db).await;
        let receipt = start(&mut db, &c, &key, &c.uuid, 0).await;
        let _first = handle_of(
            acquire_prepared_attempt(&mut db, &receipt).await,
            "first startup attempt",
        );
        let other = db_uuid(&db).await;
        let busy = startup_state(&db, &c).await;
        assert!(matches!(
            start_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &key, &other, 0).await,
            Ok(None)
        ));
        assert!(matches!(
            start_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &key, &c.uuid, 1).await,
            Ok(None)
        ));
        assert!(busy == startup_state(&db, &c).await);
        expire(&db, &c.job).await;
        let expired = startup_state(&db, &c).await;
        let stale = start(&mut db, &c, &key, &c.uuid, 0).await;
        assert!(is_stale(&acquire_prepared_attempt(&mut db, &stale).await));
        assert!(expired == startup_state(&db, &c).await);
        let next = start(&mut db, &c, &key, &other, 1).await;
        let second = handle_of(
            acquire_prepared_attempt(&mut db, &next).await,
            "explicit successor startup",
        );
        let superseded = startup_state(&db, &c).await;
        let old = start(&mut db, &c, &key, &c.uuid, 0).await;
        assert!(is_stale(&recover_attempt_acquisition(&mut db, &old).await));
        let current = start(&mut db, &c, &key, &other, 1).await;
        assert!(
            handle_of(
                recover_attempt_acquisition(&mut db, &current).await,
                "exact current startup"
            ) == second
        );
        assert!(superseded == startup_state(&db, &c).await);
    });
}

#[test]
fn postgres_startup_concurrent_same_and_distinct_requests_preserve_exact_acquisition() {
    run_test(async {
        let db = connect().await;
        let (c, key) = queued_case(&db).await;
        let gate = Arc::new(Barrier::new(6));
        let requests = [c.uuid.clone(), db_uuid(&db).await, db_uuid(&db).await];
        let mut tasks = Vec::new();
        for index in 0..6 {
            let (job, owner, key, request) = (
                c.job.clone(),
                c.owner.clone(),
                key.clone(),
                requests[index / 2].clone(),
            );
            let gate = Arc::clone(&gate);
            let mut worker = connect().await;
            tasks.push(tokio::spawn(async move {
                gate.wait().await;
                start_durable_attempt_acquisition(&mut worker, &job, &owner, &key, &request, 0)
                    .await
            }));
        }
        let mut receipts = Vec::new();
        for task in tasks {
            match task.await.expect("concurrent startup task") {
                Ok(Some(receipt)) => receipts.push(receipt),
                _ => panic!("concurrent startup preparation refused"),
            }
        }
        assert_eq!(job_status(&db, &c).await, "running");
        assert_eq!(receipt_count(&db, &c).await, 3);
        assert_eq!(attempt_rows(&db, &c.job).await, 0);
        let results = race(receipts).await;
        assert_eq!(results.iter().filter(|r| is_acquired(r)).count(), 2);
        assert!(results.iter().all(|r| is_acquired(r) || is_refused(r)));
        let mut winners = results
            .into_iter()
            .filter(is_acquired)
            .map(|r| handle_of(r, "startup winner"));
        let first = winners.next().expect("one startup winner");
        assert!(winners.all(|h| h == first));
        assert_eq!(attempt_rows(&db, &c.job).await, 1);
    });
}

#[test]
fn postgres_startup_receipt_insert_error_rolls_back_running_transition() {
    run_blocking(true, async {
        let mut db = connect().await;
        let (c, key) = queued_case(&db).await;
        let before = startup_state(&db, &c).await;
        // Schema-gated failure injection is confined to this disposable bank.
        db.batch_execute(
            "CREATE FUNCTION pg_temp.reject_startup_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'injected receipt refusal'; END; $$; \
             CREATE TRIGGER reject_startup_receipt BEFORE INSERT ON posted_file_attempt_receipts \
             FOR EACH ROW EXECUTE FUNCTION pg_temp.reject_startup_receipt();"
        ).await.quiet("install startup receipt failure injection");
        let result =
            start_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &key, &c.uuid, 0).await;
        db.batch_execute("DROP TRIGGER reject_startup_receipt ON posted_file_attempt_receipts")
            .await
            .quiet("remove startup receipt failure injection");
        assert!(matches!(result, Err(StoreError::Database)));
        assert!(before == startup_state(&db, &c).await);
        assert_eq!(job_status(&db, &c).await, "queued");
        let _receipt = start(&mut db, &c, &key, &c.uuid, 0).await;
        assert_eq!(job_status(&db, &c).await, "running");
    });
}

#[test]
fn postgres_startup_rechecks_owner_and_job_after_independent_session_lock_waits() {
    run_test(async {
        let observer = connect().await;
        for owner_lock in [true, false] {
            let (c, key) = queued_case(&observer).await;
            let mut holder = connect().await;
            let mut waiter = connect().await;
            let holder_pid = backend(&holder).await;
            let waiter_pid = backend(&waiter).await;
            let tx = holder.transaction().await.quiet("hold startup fence");
            if owner_lock {
                tx.execute(
                    "UPDATE users SET deleting_at = clock_timestamp() WHERE id = $1",
                    &[&c.owner],
                )
                .await
                .quiet("delete owner behind startup barrier");
            } else {
                tx.execute(
                    "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
                    &[&c.job],
                )
                .await
                .quiet("stop job behind startup barrier");
            }
            let expected = startup_state(&tx, &c).await;
            let (job, owner, request) = (c.job.clone(), c.owner.clone(), c.uuid.clone());
            let task = tokio::spawn(async move {
                start_durable_attempt_acquisition(&mut waiter, &job, &owner, &key, &request, 0)
                    .await
            });
            blocked_by(&observer, waiter_pid, holder_pid).await;
            tx.commit().await.quiet("release startup barrier");
            assert!(matches!(
                task.await.expect("waiting startup task"),
                Ok(None)
            ));
            assert!(expected == startup_state(&observer, &c).await);
        }
    });
}
