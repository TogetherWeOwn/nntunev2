//! Restart-only dispatch recovery and bounded delivery on real PostgreSQL.
//! Dispatch inputs are synthetic trusted coordinates, NOT Queue authentication.
//! Client/handle drops model restart; no process or commit-error injection.

use super::*;
use nntune_ingest::posted_generations::{
    acquire_prepared_attempt, drive_durable_recheck, recover_durable_attempt_acquisition,
    start_durable_attempt_acquisition, DurableAttemptRequest,
};

fn request(c: &Case, key: &str, id: &str, expected: i64) -> DurableAttemptRequest {
    DurableAttemptRequest::new(&c.job, &c.owner, key, id, expected)
        .expect("valid synthetic dispatch coordinates")
}

async fn fixture(db: &mut Client, acquired: bool) -> (Case, String) {
    let c = case(db).await;
    let key = format!("ingest:{}:{}:upload", c.owner, c.sealed.hash);
    db.execute(
        "UPDATE ingest_jobs SET idempotency_key = $2, status = 'queued' WHERE id = $1",
        &[&c.job, &key],
    )
    .await
    .quiet("bind synthetic admitted dispatch");
    let receipt =
        match start_durable_attempt_acquisition(db, &c.job, &c.owner, &key, &c.uuid, 0).await {
            Ok(Some(receipt)) => receipt,
            _ => panic!("start durable delivery fixture"),
        };
    if acquired {
        assert!(matches!(
            acquire_prepared_attempt(db, &receipt).await,
            Ok(AttemptOutcome::Acquired(_))
        ));
    }
    // Drop the startup receipt and any acquired handle before recovery.
    (c, key)
}

async fn recovered(db: &mut Client, r: &DurableAttemptRequest) -> AttemptHandle {
    match recover_durable_attempt_acquisition(db, r).await {
        Ok(AttemptOutcome::Acquired(handle)) => handle,
        _ => panic!("recover exact durable dispatch"),
    }
}

async fn durable_begun(db: &mut Client) -> (Case, String, AttemptHandle) {
    let (c, key) = fixture(db, true).await;
    let attempt = recovered(db, &request(&c, &key, &c.uuid, 0)).await;
    let h = admitted(db, &attempt, &c.sealed).await;
    applied(
        begin_verification_round(db, &h).await,
        "begin durable delivery",
    );
    (c, key, attempt)
}

async fn full_state(db: &impl tokio_postgres::GenericClient, c: &Case) -> String {
    db.query_one(
        "SELECT md5(jsonb_build_array( \
         (SELECT to_jsonb(u) FROM users u WHERE u.id = $1), \
         (SELECT to_jsonb(t) FROM ingest_jobs t WHERE t.id = $2), \
         (SELECT to_jsonb(t) FROM posted_file_job_attempts t WHERE t.job_id = $2), \
         (SELECT jsonb_agg(to_jsonb(r) ORDER BY r.request_id) \
          FROM posted_file_attempt_receipts r WHERE r.job_id = $2), \
         (SELECT to_jsonb(t) FROM posted_files t WHERE t.id = $3), \
         (SELECT to_jsonb(t) FROM posted_file_registry t WHERE t.plaintext_sha256 = $4), \
         (SELECT to_jsonb(t) FROM posted_file_operations t WHERE t.id = $5), \
         (SELECT to_jsonb(t) FROM posted_file_health t WHERE t.generation_id = $3), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.kind, t.owner_id) \
          FROM posted_file_references t WHERE t.generation_id = $3), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.round, t.slot, t.side) \
          FROM posted_file_article_progress t WHERE t.generation_id = $3), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.round, t.side) \
          FROM posted_file_content_checks t WHERE t.generation_id = $3), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.round) \
          FROM posted_file_recheck_schedule t WHERE t.generation_id = $3))::text)",
        &[
            &c.owner,
            &c.job,
            &c.sealed.generation,
            &c.sealed.hash,
            &c.sealed.op,
        ],
    )
    .await
    .quiet("snapshot full durable delivery state")
    .get(0)
}

#[test]
fn postgres_durable_delivery_restart_recovers_exact_attempt_and_cursor_without_handles() {
    run_test(async {
        let mut db = connect().await;
        let (c, key, attempt) = durable_begun(&mut db).await;
        let r = request(&c, &key, &c.uuid, 0);
        assert_eq!(format!("{r:?}"), "DurableAttemptRequest([REDACTED])");
        let before = full_state(&db, &c).await;
        assert!(recovered(&mut db, &r).await == attempt);
        assert!(before == full_state(&db, &c).await);
        drop(attempt);
        drop(r);
        make_due(&db, &c.sealed).await;
        drop(db);
        let mut t = Transport::new(&c.sealed);
        // Reconstruct only the original durable coordinates on every connection.
        for expected_cursor in 1..ARTICLES * 2 {
            let mut db = connect().await;
            progress_handle(
                drive_durable_recheck(
                    &mut db,
                    &request(&c, &key, &c.uuid, 0),
                    &c.sealed.hash,
                    &mut t,
                    1,
                )
                .await,
                "restart bounded delivery",
            );
            assert_eq!(cursor(&db, &c.sealed).await, expected_cursor);
        }
        let mut db = connect().await;
        let h = complete_handle(
            drive_durable_recheck(
                &mut db,
                &request(&c, &key, &c.uuid, 0),
                &c.sealed.hash,
                &mut t,
                1,
            )
            .await,
            "restart final slice",
        );
        assert!(h.generation_id() == c.sealed.generation);
        assert!(h.operation_id() == c.sealed.op);
        assert!(
            t.calls
                == [
                    st(0, true),
                    st(0, false),
                    st(1, true),
                    st(1, false),
                    st(2, true),
                    st(2, false)
                ]
        );
        assert!(t.exact);
        let before = full_state(&db, &c).await;
        assert!(matches!(
            drive_durable_recheck(
                &mut db,
                &request(&c, &key, &c.uuid, 0),
                &c.sealed.hash,
                &mut t,
                1,
            )
            .await,
            Ok(RecheckStep::PresenceComplete(_))
        ));
        assert!(before == full_state(&db, &c).await);
        assert_eq!(t.calls.len(), (ARTICLES * 2) as usize);
        assert_eq!(content_rows(&db, &c.sealed).await, 0);
    });
}

#[test]
fn postgres_durable_delivery_missing_or_unacquired_receipts_never_create_authority() {
    run_test(async {
        let mut db = connect().await;
        for acquired in [false, true] {
            let (c, key) = fixture(&mut db, acquired).await;
            let mut t = Transport::new(&c.sealed);
            // Existing unacquired receipt, absent request and wrong next attempt.
            let ids = if acquired {
                vec![("missing", 0), (c.uuid.as_str(), 1)]
            } else {
                vec![(c.uuid.as_str(), 0), ("missing", 0)]
            };
            for (id, expected) in ids {
                let r = request(&c, &key, id, expected);
                let before = full_state(&db, &c).await;
                assert!(matches!(
                    recover_durable_attempt_acquisition(&mut db, &r).await,
                    Ok(AttemptOutcome::StaleOrMissing)
                ));
                assert!(matches!(
                    drive_durable_recheck(&mut db, &r, &c.sealed.hash, &mut t, 1).await,
                    Ok(RecheckStep::StaleOrDiverged)
                ));
                assert!(before == full_state(&db, &c).await);
            }
            assert!(t.calls.is_empty());
        }
        // A legacy live attempt has no durable receipt to discover/borrow.
        let c = case(&db).await;
        let key = format!("ingest:{}:{}:upload", c.owner, c.sealed.hash);
        db.execute(
            "UPDATE ingest_jobs SET idempotency_key = $2 WHERE id = $1",
            &[&c.job, &key],
        )
        .await
        .quiet("bind legacy fixture admission");
        let _legacy = acquire(&mut db, &c.job, &c.owner, 0).await;
        let r = request(&c, &key, &c.uuid, 0);
        let before = full_state(&db, &c).await;
        assert!(matches!(
            recover_durable_attempt_acquisition(&mut db, &r).await,
            Ok(AttemptOutcome::StaleOrMissing)
        ));
        assert!(before == full_state(&db, &c).await);
    });
}

#[test]
fn postgres_durable_delivery_rejects_mismatched_admission_owner_request_and_attempt() {
    run_test(async {
        let mut db = connect().await;
        let (c, key, _attempt) = durable_begun(&mut db).await;
        make_due(&db, &c.sealed).await;
        let (other, other_key) = fixture(&mut db, true).await;
        let variants = [
            DurableAttemptRequest::new(&c.uuid, &c.owner, &key, &c.uuid, 0).unwrap(),
            DurableAttemptRequest::new(&c.job, &other.owner, &key, &c.uuid, 0).unwrap(),
            request(&c, &other_key, &c.uuid, 0),
            request(&c, &key, &other.uuid, 0),
            request(&c, &key, &c.uuid, 1),
            request(&other, &other_key, &other.uuid, 0),
        ];
        let mut t = Transport::new(&c.sealed);
        let before = full_state(&db, &c).await;
        let foreign_before = full_state(&db, &other).await;
        for r in variants {
            assert!(matches!(
                drive_durable_recheck(&mut db, &r, &c.sealed.hash, &mut t, 1).await,
                Ok(RecheckStep::StaleOrDiverged)
            ));
            assert!(before == full_state(&db, &c).await);
            assert!(foreign_before == full_state(&db, &other).await);
        }
        assert!(t.calls.is_empty());
        // Another syntactically valid hash never discovers this candidate.
        assert!(matches!(
            drive_durable_recheck(
                &mut db,
                &request(&c, &key, &c.uuid, 0),
                &other.sealed.hash,
                &mut t,
                1,
            )
            .await,
            Ok(RecheckStep::StaleOrDiverged)
        ));
        assert!(before == full_state(&db, &c).await);
        assert!(t.calls.is_empty());
    });
}

#[test]
fn postgres_durable_delivery_expired_or_superseded_request_never_acquires_or_takes_over() {
    run_test(async {
        let mut db = connect().await;
        let (c, key, _attempt) = durable_begun(&mut db).await;
        make_due(&db, &c.sealed).await;
        db.execute(
            "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
            &[&c.job],
        )
        .await
        .quiet("expire durable fixture attempt");
        let mut t = Transport::new(&c.sealed);
        let old = request(&c, &key, &c.uuid, 0);
        let next = request(&c, &key, "next", 1);
        let before = full_state(&db, &c).await;
        for r in [&old, &next] {
            assert!(matches!(
                drive_durable_recheck(&mut db, r, &c.sealed.hash, &mut t, 1).await,
                Ok(RecheckStep::StaleOrDiverged)
            ));
            assert!(before == full_state(&db, &c).await);
        }
        let receipt =
            match start_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &key, "next", 1)
                .await
            {
                Ok(Some(r)) => r,
                _ => panic!("explicit next request"),
            };
        assert!(matches!(
            acquire_prepared_attempt(&mut db, &receipt).await,
            Ok(AttemptOutcome::Acquired(_))
        ));
        let before = full_state(&db, &c).await;
        // Next attempt is live, but candidate takeover is still an explicit act.
        for r in [&old, &next] {
            assert!(matches!(
                drive_durable_recheck(&mut db, r, &c.sealed.hash, &mut t, 1).await,
                Ok(RecheckStep::StaleOrDiverged)
            ));
            assert!(before == full_state(&db, &c).await);
        }
        assert!(t.calls.is_empty());
    });
}

#[test]
fn postgres_durable_delivery_waiting_unbegun_stopped_and_deleting_work_retains_state() {
    run_test(async {
        let mut db = connect().await;
        let (c, key) = fixture(&mut db, true).await;
        let r = request(&c, &key, &c.uuid, 0);
        let attempt = recovered(&mut db, &r).await;
        let handle = admitted(&mut db, &attempt, &c.sealed).await;
        let mut t = Transport::new(&c.sealed);
        let before = full_state(&db, &c).await;
        assert!(matches!(
            drive_durable_recheck(&mut db, &r, &c.sealed.hash, &mut t, 1).await,
            Ok(RecheckStep::StaleOrDiverged)
        ));
        assert!(before == full_state(&db, &c).await);
        applied(
            begin_verification_round(&mut db, &handle).await,
            "begin waiting fixture",
        );
        let before = full_state(&db, &c).await;
        assert!(matches!(
            drive_durable_recheck(&mut db, &r, &c.sealed.hash, &mut t, 1).await,
            Ok(RecheckStep::Waiting { .. })
        ));
        assert!(before == full_state(&db, &c).await);
        for status in ["queued", "done", "failed"] {
            db.execute(
                "UPDATE ingest_jobs SET status = $2 WHERE id = $1",
                &[&c.job, &status],
            )
            .await
            .quiet("stop durable fixture job");
            let before = full_state(&db, &c).await;
            assert!(matches!(
                drive_durable_recheck(&mut db, &r, &c.sealed.hash, &mut t, 1).await,
                Ok(RecheckStep::StaleOrDiverged)
            ));
            assert!(before == full_state(&db, &c).await);
        }
        db.execute(
            "UPDATE ingest_jobs SET status = 'running' WHERE id = $1",
            &[&c.job],
        )
        .await
        .quiet("restore running fixture job");
        db.execute(
            "UPDATE users SET deleting_at = clock_timestamp() WHERE id = $1",
            &[&c.owner],
        )
        .await
        .quiet("schedule fixture deletion");
        let before = full_state(&db, &c).await;
        assert!(matches!(
            drive_durable_recheck(&mut db, &r, &c.sealed.hash, &mut t, 1).await,
            Ok(RecheckStep::StaleOrDiverged)
        ));
        assert!(before == full_state(&db, &c).await);
        assert!(t.calls.is_empty());
    });
}

#[test]
fn postgres_durable_delivery_network_callbacks_keep_original_fences_and_release_locks() {
    run_test(async {
        let mut db = connect().await;
        for fault in [Fault::Revision, Fault::JobStatus, Fault::AttemptSuperseded] {
            let (c, key, attempt) = durable_begun(&mut db).await;
            make_due(&db, &c.sealed).await;
            let mut t = Transport::new(&c.sealed);
            t.hook = Some(Hook {
                db: connect().await,
                attempt,
                job: c.job.clone(),
                owner: c.owner.clone(),
                sealed: c.sealed.clone(),
                fault,
                at_repost: false,
                fired: false,
            });
            assert!(matches!(
                drive_durable_recheck(
                    &mut db,
                    &request(&c, &key, &c.uuid, 0),
                    &c.sealed.hash,
                    &mut t,
                    1,
                )
                .await,
                Ok(RecheckStep::StaleOrDiverged)
            ));
            assert!(t.hook.as_ref().expect("durable hook").fired);
            assert!(t.calls == [st(0, true)]);
            assert!(t.exact);
            assert_eq!(cursor(&db, &c.sealed).await, 0);
            assert_eq!(repost_sum(&db, &c.sealed).await, 0);
            assert_eq!(content_rows(&db, &c.sealed).await, 0);
        }
    });
}

async fn backend(db: &Client) -> i32 {
    db.query_one("SELECT pg_backend_pid()", &[])
        .await
        .quiet("read durable recovery backend")
        .get(0)
}

#[test]
fn postgres_durable_recovery_rechecks_after_owner_job_and_attempt_lock_waits() {
    run_test(async {
        let mut db = connect().await;
        for held in 0..3 {
            for change in [false, true] {
                let (c, key) = fixture(&mut db, true).await;
                let r = request(&c, &key, &c.uuid, 0);
                let mut holder = connect().await;
                let holder_pid = backend(&holder).await;
                let tx = holder
                    .transaction()
                    .await
                    .quiet("hold durable recovery row");
                let (sql, id) = match held {
                    0 => ("SELECT id FROM users WHERE id = $1 FOR UPDATE", &c.owner),
                    1 => (
                        "SELECT id FROM ingest_jobs WHERE id = $1 FOR UPDATE",
                        &c.job,
                    ),
                    _ => (
                        "SELECT job_id FROM posted_file_job_attempts WHERE job_id = $1 FOR UPDATE",
                        &c.job,
                    ),
                };
                tx.query_one(sql, &[id])
                    .await
                    .quiet("lock durable recovery row");
                let mut worker = connect().await;
                let waiter = backend(&worker).await;
                let task = tokio::spawn(async move {
                    recover_durable_attempt_acquisition(&mut worker, &r).await
                });
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let blocked: bool = db
                            .query_one(
                                "SELECT $2 = ANY(pg_blocking_pids($1))",
                                &[&waiter, &holder_pid],
                            )
                            .await
                            .quiet("observe durable recovery wait")
                            .get(0);
                        if blocked {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("durable recovery never reached lock barrier");
                assert!(!task.is_finished());
                if change {
                    let (sql, id) = match held {
                        0 => (
                            "UPDATE users SET deleting_at = clock_timestamp() WHERE id = $1",
                            &c.owner,
                        ),
                        1 => (
                            "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
                            &c.job,
                        ),
                        _ => (
                            "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
                            &c.job,
                        ),
                    };
                    tx.execute(sql, &[id])
                        .await
                        .quiet("mutate durable recovery fence while blocked");
                }
                // Capture the intended state while recovery is STILL blocked.
                let before = full_state(&tx, &c).await;
                tx.commit().await.quiet("release durable recovery barrier");
                let result = task
                    .await
                    .unwrap_or_else(|_| panic!("durable recovery waiter failed"));
                if change {
                    assert!(matches!(result, Ok(AttemptOutcome::StaleOrMissing)));
                } else {
                    assert!(matches!(result, Ok(AttemptOutcome::Acquired(_))));
                }
                assert!(before == full_state(&db, &c).await);
            }
        }
    });
}

#[test]
fn postgres_durable_delivery_malformed_coordinates_and_bounds_refuse_before_work() {
    run_test(async {
        let mut db = connect().await;
        let (c, key, _attempt) = durable_begun(&mut db).await;
        for (job, owner, admission, id, expected) in [
            ("", c.owner.as_str(), key.as_str(), c.uuid.as_str(), 0),
            (
                c.job.as_str(),
                "bad owner",
                key.as_str(),
                c.uuid.as_str(),
                0,
            ),
            (
                c.job.as_str(),
                c.owner.as_str(),
                "bad-key",
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
                DurableAttemptRequest::new(job, owner, admission, id, expected),
                Err(StoreError::InvalidInput)
            ));
        }
        let r = request(&c, &key, &c.uuid, 0);
        let mut t = Transport::new(&c.sealed);
        let before = full_state(&db, &c).await;
        for (hash, limit) in [
            ("bad", 1),
            (c.sealed.hash.as_str(), 0),
            (c.sealed.hash.as_str(), STAT_BATCH_SIZE + 1),
        ] {
            assert!(matches!(
                drive_durable_recheck(&mut db, &r, hash, &mut t, limit).await,
                Err(StoreError::InvalidInput)
            ));
        }
        assert!(before == full_state(&db, &c).await);
        assert!(t.calls.is_empty());
    });
}
