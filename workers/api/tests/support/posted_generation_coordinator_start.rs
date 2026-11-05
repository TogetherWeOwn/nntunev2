//! Explicit atomic coordinator startup, not Queue authentication or deployment.
//! Dropped clients/results simulate restart/lost replies, not process or commit
//! errors. Trigger failures exercise pre-commit rollback on disposable PostgreSQL.

use super::*;
use nntune_ingest::posted_generations::{
    recover_durable_attempt_acquisition, start_and_acquire_durable_attempt, DurableAttemptRequest,
};

fn request(c: &Case, key: &str, id: &str, expected: i64) -> DurableAttemptRequest {
    DurableAttemptRequest::new(&c.job, &c.owner, key, id, expected)
        .expect("synthetic trusted coordinator request")
}

#[test]
fn postgres_coordinator_start_acquires_atomically_and_restart_replays_without_renewal() {
    run_test(async {
        let mut db = connect().await;
        let (c, key) = queued_case(&db).await;
        let r = request(&c, &key, &c.uuid, 0);
        let custody = snapshot(&db, &c.sealed).await;
        let first = handle_of(
            start_and_acquire_durable_attempt(&mut db, &r).await,
            "atomic coordinator startup",
        );
        assert_eq!(job_status(&db, &c).await, "running");
        assert_eq!(receipt_count(&db, &c).await, 1);
        assert_eq!(attempt_rows(&db, &c.job).await, 1);
        assert!(custody == snapshot(&db, &c.sealed).await);
        let _candidate = admitted(&mut db, &first, &c.sealed).await;
        set_lease(&db, &c.job, 100).await;
        let before = startup_state(&db, &c).await;
        drop(r);
        drop(db);
        let mut db = connect().await;
        let r = request(&c, &key, &c.uuid, 0);
        assert!(
            handle_of(
                start_and_acquire_durable_attempt(&mut db, &r).await,
                "restart atomic startup",
            ) == first
        );
        assert!(
            handle_of(
                recover_durable_attempt_acquisition(&mut db, &r).await,
                "read-only recovery after startup",
            ) == first
        );
        assert!(before == startup_state(&db, &c).await);
    });
}

#[test]
fn postgres_coordinator_start_completes_existing_preparation_after_client_drop() {
    run_test(async {
        let mut db = connect().await;
        let (c, key) = queued_case(&db).await;
        let receipt = start(&mut db, &c, &key, &c.uuid, 0).await;
        assert_eq!(attempt_rows(&db, &c.job).await, 0);
        let prepared = startup_state(&db, &c).await.1;
        drop(db);
        let mut db = connect().await;
        let first = handle_of(
            start_and_acquire_durable_attempt(&mut db, &request(&c, &key, &c.uuid, 0)).await,
            "complete persisted preparation",
        );
        assert!(prepared == startup_state(&db, &c).await.1);
        // Compare against the retained original receipt: no replacement proposal.
        assert!(
            handle_of(
                acquire_prepared_attempt(&mut db, &receipt).await,
                "original proposal after combined acquisition",
            ) == first
        );
    });
}

#[test]
fn postgres_coordinator_start_acquisition_error_rolls_back_job_receipt_and_attempt() {
    run_blocking(true, async {
        let mut db = connect().await;
        let (c, key) = queued_case(&db).await;
        let r = request(&c, &key, &c.uuid, 0);
        let before = startup_state(&db, &c).await;
        db.batch_execute(
            "CREATE FUNCTION pg_temp.reject_coordinator_attempt() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'injected acquisition refusal'; END; $$; \
             CREATE TRIGGER reject_coordinator_attempt BEFORE INSERT ON posted_file_job_attempts \
             FOR EACH ROW EXECUTE FUNCTION pg_temp.reject_coordinator_attempt();"
        ).await.quiet("install coordinator acquisition failure injection");
        let result = start_and_acquire_durable_attempt(&mut db, &r).await;
        db.batch_execute("DROP TRIGGER reject_coordinator_attempt ON posted_file_job_attempts")
            .await
            .quiet("remove coordinator acquisition failure injection");
        assert!(matches!(result, Err(StoreError::Database)));
        assert!(before == startup_state(&db, &c).await);
        assert_eq!(job_status(&db, &c).await, "queued");
        assert_eq!(receipt_count(&db, &c).await, 0);
        assert_eq!(attempt_rows(&db, &c.job).await, 0);
        let _first = handle_of(
            start_and_acquire_durable_attempt(&mut db, &r).await,
            "retry rolled-back coordinator startup",
        );
    });
}

#[test]
fn postgres_coordinator_start_foreign_missing_stopped_deleting_and_reset_jobs_refuse() {
    run_test(async {
        let mut db = connect().await;
        let (c, key) = queued_case(&db).await;
        let (other, other_key) = queued_case(&db).await;
        let before = startup_state(&db, &c).await;
        let foreign_before = startup_state(&db, &other).await;
        for r in [
            DurableAttemptRequest::new(&c.uuid, &c.owner, &key, &c.uuid, 0).unwrap(),
            DurableAttemptRequest::new(&c.job, &other.owner, &key, &c.uuid, 0).unwrap(),
            request(&c, &other_key, &c.uuid, 0),
            request(&c, &key, &c.uuid, 1),
        ] {
            assert!(is_stale(
                &start_and_acquire_durable_attempt(&mut db, &r).await
            ));
            assert!(before == startup_state(&db, &c).await);
            assert!(foreign_before == startup_state(&db, &other).await);
        }
        for status in ["done", "failed"] {
            set_status(&db, &c.job, status).await;
            let before = startup_state(&db, &c).await;
            assert!(is_stale(
                &start_and_acquire_durable_attempt(&mut db, &request(&c, &key, &c.uuid, 0)).await
            ));
            assert!(before == startup_state(&db, &c).await);
        }
        for prepared in [false, true] {
            let (c, key) = queued_case(&db).await;
            if prepared {
                let _receipt = start(&mut db, &c, &key, &c.uuid, 0).await;
            }
            db.execute(
                "UPDATE users SET deleting_at = clock_timestamp() WHERE id = $1",
                &[&c.owner],
            )
            .await
            .quiet("delete coordinator owner");
            let before = startup_state(&db, &c).await;
            assert!(is_stale(
                &start_and_acquire_durable_attempt(&mut db, &request(&c, &key, &c.uuid, 0)).await
            ));
            assert!(before == startup_state(&db, &c).await);
        }
        let (c, key) = queued_case(&db).await;
        let _first = handle_of(
            start_and_acquire_durable_attempt(&mut db, &request(&c, &key, &c.uuid, 0)).await,
            "startup before artificial reset",
        );
        set_status(&db, &c.job, "queued").await;
        let before = startup_state(&db, &c).await;
        assert!(is_stale(
            &start_and_acquire_durable_attempt(&mut db, &request(&c, &key, &c.uuid, 0)).await
        ));
        assert!(before == startup_state(&db, &c).await);
    });
}

#[test]
fn postgres_coordinator_start_concurrent_requests_commit_one_exact_attempt() {
    run_test(async {
        let db = connect().await;
        for same in [true, false] {
            let (c, key) = queued_case(&db).await;
            let gate = Arc::new(Barrier::new(6));
            let ids = [c.uuid.clone(), db_uuid(&db).await, db_uuid(&db).await];
            let mut tasks = Vec::new();
            for index in 0..6 {
                let r = request(&c, &key, &ids[if same { 0 } else { index / 2 }], 0);
                let gate = Arc::clone(&gate);
                let mut db = connect().await;
                tasks.push(tokio::spawn(async move {
                    gate.wait().await;
                    start_and_acquire_durable_attempt(&mut db, &r).await
                }));
            }
            let mut winners = Vec::new();
            for task in tasks {
                match task.await.expect("concurrent atomic coordinator") {
                    Ok(AttemptOutcome::Acquired(h)) => winners.push(h),
                    Ok(AttemptOutcome::StaleOrMissing) if !same => {}
                    _ => panic!("unexpected coordinator race outcome"),
                }
            }
            assert_eq!(winners.len(), if same { 6 } else { 2 });
            assert!(winners.iter().all(|h| h == &winners[0]));
            assert_eq!(receipt_count(&db, &c).await, 1);
            assert_eq!(attempt_rows(&db, &c.job).await, 1);
            assert_eq!(job_status(&db, &c).await, "running");
        }
    });
}

#[test]
fn postgres_coordinator_start_expired_and_superseded_receipts_require_explicit_next_request() {
    run_test(async {
        let mut db = connect().await;
        let (c, key) = queued_case(&db).await;
        let old = request(&c, &key, &c.uuid, 0);
        let first = handle_of(
            start_and_acquire_durable_attempt(&mut db, &old).await,
            "initial coordinator request",
        );
        let _candidate = admitted(&mut db, &first, &c.sealed).await;
        let custody = snapshot(&db, &c.sealed).await;
        let next = request(&c, &key, "next", 1);
        let before = startup_state(&db, &c).await;
        assert!(is_stale(
            &start_and_acquire_durable_attempt(&mut db, &next).await
        ));
        assert!(before == startup_state(&db, &c).await);
        expire(&db, &c.job).await;
        let expired = startup_state(&db, &c).await;
        assert!(is_stale(
            &start_and_acquire_durable_attempt(&mut db, &old).await
        ));
        assert!(expired == startup_state(&db, &c).await);
        let second = handle_of(
            start_and_acquire_durable_attempt(&mut db, &next).await,
            "explicit authorized next request",
        );
        assert!(second != first);
        assert!(custody == snapshot(&db, &c.sealed).await);
        let superseded = startup_state(&db, &c).await;
        assert!(is_stale(
            &start_and_acquire_durable_attempt(&mut db, &old).await
        ));
        assert!(superseded == startup_state(&db, &c).await);
        // Acquisition alone does not take over the previous candidate claim.
        assert!(matches!(
            recover_pending(&mut db, &second, &c.sealed.hash).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        assert!(superseded == startup_state(&db, &c).await);
    });
}

#[test]
fn postgres_coordinator_start_replay_rechecks_attempt_lease_after_observed_lock_wait() {
    run_test(async {
        let mut db = connect().await;
        for expires in [false, true] {
            let (c, key) = queued_case(&db).await;
            let r = request(&c, &key, &c.uuid, 0);
            let first = handle_of(
                start_and_acquire_durable_attempt(&mut db, &r).await,
                "coordinator before attempt barrier",
            );
            let now: i64 = db
                .query_one("SELECT extract(epoch FROM clock_timestamp())::bigint", &[])
                .await
                .quiet("read coordinator database clock")
                .get(0);
            let deadline = now + 3;
            if expires {
                db.execute(
                    "UPDATE posted_file_job_attempts SET lease_until = $2 WHERE job_id = $1",
                    &[&c.job, &deadline],
                )
                .await
                .quiet("set coordinator short lease");
            }
            let before = startup_state(&db, &c).await;
            let mut holder = connect().await;
            let mut waiter = connect().await;
            let holder_pid = backend(&holder).await;
            let waiter_pid = backend(&waiter).await;
            let tx = holder.transaction().await.quiet("hold coordinator attempt");
            tx.query_one(
                "SELECT attempt FROM posted_file_job_attempts WHERE job_id = $1 FOR UPDATE",
                &[&c.job],
            )
            .await
            .quiet("lock coordinator attempt");
            let (outcome, during) =
                tokio::join!(start_and_acquire_durable_attempt(&mut waiter, &r), async {
                    blocked_by(&db, waiter_pid, holder_pid).await;
                    if expires {
                        let live: bool = tx
                            .query_one(
                                "SELECT extract(epoch FROM clock_timestamp())::bigint < $1",
                                &[&deadline],
                            )
                            .await
                            .quiet("check live coordinator barrier")
                            .get(0);
                        assert!(live, "coordinator missed live lease barrier");
                        tx.query_one(
                            "SELECT pg_sleep(GREATEST(0.0, $1::bigint::double precision - \
                             extract(epoch FROM clock_timestamp())::double precision) + 1.0)",
                            &[&deadline],
                        )
                        .await
                        .quiet("expire coordinator lease during wait");
                    }
                    let during = startup_state(&tx, &c).await;
                    tx.commit().await.quiet("release coordinator attempt");
                    during
                });
            if expires {
                assert!(is_stale(&outcome));
            } else {
                assert!(handle_of(outcome, "unchanged coordinator barrier") == first);
            }
            assert!(before == during);
            assert!(before == startup_state(&db, &c).await);
        }
    });
}

#[test]
fn postgres_coordinator_start_rechecks_owner_and_job_after_observed_lock_waits() {
    run_test(async {
        let observer = connect().await;
        for owner_lock in [true, false] {
            let (c, key) = queued_case(&observer).await;
            let mut holder = connect().await;
            let mut waiter = connect().await;
            let holder_pid = backend(&holder).await;
            let waiter_pid = backend(&waiter).await;
            let tx = holder.transaction().await.quiet("hold coordinator fence");
            if owner_lock {
                tx.execute(
                    "UPDATE users SET deleting_at = clock_timestamp() WHERE id = $1",
                    &[&c.owner],
                )
                .await
                .quiet("delete coordinator owner behind barrier");
            } else {
                tx.execute(
                    "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
                    &[&c.job],
                )
                .await
                .quiet("stop coordinator job behind barrier");
            }
            let expected = startup_state(&tx, &c).await;
            let r = request(&c, &key, &c.uuid, 0);
            let task =
                tokio::spawn(
                    async move { start_and_acquire_durable_attempt(&mut waiter, &r).await },
                );
            blocked_by(&observer, waiter_pid, holder_pid).await;
            tx.commit().await.quiet("release coordinator fence");
            assert!(is_stale(&task.await.expect("waiting coordinator startup")));
            assert!(expected == startup_state(&observer, &c).await);
        }
    });
}
