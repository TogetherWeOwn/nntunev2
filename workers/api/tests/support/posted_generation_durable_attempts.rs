//! Durable receipt preparation on disposable PostgreSQL. Restart/lost-response
//! cases drop local receipts and clients; they do not inject a commit failure or
//! exercise a deployed coordinator. Request identities below stand in for the
//! trusted coordinator's durable dispatch contract, never Queue/KV authority.

use super::*;

const MIGRATION_0046: &str =
    include_str!("../../../../migrations/0048_posted_file_attempt_receipts.sql");

async fn durable(db: &mut Client, c: &Case, key: &str, expected: i64) -> AttemptAcquisition {
    match prepare_durable_attempt_acquisition(db, &c.job, &c.owner, key, expected).await {
        Ok(Some(receipt)) => receipt,
        _ => panic!("durable receipt preparation refused"),
    }
}

async fn full_state(db: &Client, c: &Case) -> (String, Option<String>) {
    let receipts = db
        .query_one(
            "SELECT md5(jsonb_agg(to_jsonb(t) ORDER BY request_id)::text) \
             FROM posted_file_attempt_receipts t WHERE job_id = $1",
            &[&c.job],
        )
        .await
        .quiet("snapshot durable receipts")
        .get(0);
    (state(db, c).await, receipts)
}

async fn receipt_count(db: &Client, c: &Case) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_attempt_receipts WHERE job_id = $1",
        &[&c.job],
    )
    .await
}

#[test]
fn postgres_durable_prepare_invalid_missing_foreign_and_stopped_requests_write_nothing() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let foreign = case(&db).await;
        let before = full_state(&db, &c).await;
        let long = "a".repeat(65);
        for bad in ["", "a b", "a/b", "a\n", "é", long.as_str()] {
            for (job, owner, key) in [
                (bad, c.owner.as_str(), c.uuid.as_str()),
                (c.job.as_str(), bad, c.uuid.as_str()),
                (c.job.as_str(), c.owner.as_str(), bad),
            ] {
                assert!(matches!(
                    prepare_durable_attempt_acquisition(&mut db, job, owner, key, 0).await,
                    Err(StoreError::InvalidInput)
                ));
            }
        }
        for bad in [-1, i64::MIN, i64::MAX] {
            assert!(matches!(
                prepare_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &c.uuid, bad).await,
                Err(StoreError::InvalidInput)
            ));
        }
        for (job, owner, expected) in [
            (c.uuid.as_str(), c.owner.as_str(), 0),
            (c.job.as_str(), foreign.owner.as_str(), 0),
            (c.job.as_str(), c.owner.as_str(), 1),
        ] {
            assert!(matches!(
                prepare_durable_attempt_acquisition(&mut db, job, owner, &c.uuid, expected).await,
                Ok(None)
            ));
        }
        assert!(before == full_state(&db, &c).await);
        set_status(&db, &c.job, "failed").await;
        let stopped = full_state(&db, &c).await;
        assert!(matches!(
            prepare_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &c.uuid, 0).await,
            Ok(None)
        ));
        assert!(stopped == full_state(&db, &c).await);
        assert_eq!(receipt_count(&db, &c).await, 0);
    });
}

#[test]
fn postgres_durable_prepare_restart_before_acquisition_reuses_exact_persisted_proposal() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let before = state(&db, &c).await;
        let receipt = durable(&mut db, &c, &c.uuid, 0).await;
        assert_eq!(format!("{receipt:?}"), "AttemptAcquisition([REDACTED])");
        assert!(
            before == state(&db, &c).await,
            "preparation granted authority"
        );
        assert_eq!(attempt_rows(&db, &c.job).await, 0);
        assert_eq!(receipt_count(&db, &c).await, 1);
        assert!(is_stale(
            &recover_attempt_acquisition(&mut db, &receipt).await
        ));
        let prepared = full_state(&db, &c).await;
        drop(receipt);
        drop(db);

        let mut restarted = connect().await;
        let receipt = durable(&mut restarted, &c, &c.uuid, 0).await;
        assert!(prepared == full_state(&restarted, &c).await);
        let _handle = handle_of(
            acquire_prepared_attempt(&mut restarted, &receipt).await,
            "acquire durable receipt after restart",
        );
        let exact: bool = restarted
            .query_one(
                "SELECT r.attempt = a.attempt AND r.token = a.token \
                 FROM posted_file_attempt_receipts r \
                 JOIN posted_file_job_attempts a USING (job_id) \
                 WHERE r.job_id = $1 AND r.request_id = $2",
                &[&c.job, &c.uuid],
            )
            .await
            .quiet("compare durable proposal to acquired attempt")
            .get(0);
        assert!(exact, "restart substituted a different token");
    });
}

#[test]
fn postgres_durable_prepare_restart_after_lost_acquisition_response_recovers_pending_bundle() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let receipt = durable(&mut db, &c, &c.uuid, 0).await;
        // Lost-response simulation, not a commit-error injection.
        let first = handle_of(
            acquire_prepared_attempt(&mut db, &receipt).await,
            "initial durable acquisition",
        );
        let generation = admitted(&mut db, &first, &c.sealed).await;
        drop(generation);
        set_lease(&db, &c.job, 100).await;
        let before = full_state(&db, &c).await;
        drop(receipt);
        drop(db);

        let mut restarted = connect().await;
        let receipt = durable(&mut restarted, &c, &c.uuid, 0).await;
        for outcome in [
            recover_attempt_acquisition(&mut restarted, &receipt).await,
            acquire_prepared_attempt(&mut restarted, &receipt).await,
        ] {
            let restored = handle_of(outcome, "recover durable attempt after restart");
            assert!(restored == first);
            let Ok(AdmissionOutcome::ResumedSameCandidate(handle, bundle)) =
                recover_pending(&mut restarted, &restored, &c.sealed.hash).await
            else {
                panic!("durable receipt did not recover exact pending custody")
            };
            assert!(handle.generation_id() == c.sealed.generation);
            assert!(bundle.custody().expect("restored custody") == c.sealed.custody());
        }
        assert!(
            before == full_state(&restarted, &c).await,
            "restart renewed or wrote state"
        );
        assert_eq!(receipt_count(&restarted, &c).await, 1);
    });
}

#[test]
fn postgres_durable_prepare_replay_mismatches_and_busy_new_keys_never_remint_or_borrow() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let other = case(&db).await;
        let receipt = durable(&mut db, &c, &c.uuid, 0).await;
        let first = handle_of(
            acquire_prepared_attempt(&mut db, &receipt).await,
            "acquire before replay controls",
        );
        set_lease(&db, &c.job, 100).await;
        let before = full_state(&db, &c).await;
        for (job, owner, key, expected) in [
            (c.job.as_str(), c.owner.as_str(), c.uuid.as_str(), 1),
            (c.job.as_str(), other.owner.as_str(), c.uuid.as_str(), 0),
            (c.job.as_str(), c.owner.as_str(), other.uuid.as_str(), 0),
            (other.job.as_str(), c.owner.as_str(), c.uuid.as_str(), 0),
        ] {
            assert!(matches!(
                prepare_durable_attempt_acquisition(&mut db, job, owner, key, expected).await,
                Ok(None)
            ));
            assert!(before == full_state(&db, &c).await);
        }
        for _ in 0..2 {
            let replay = durable(&mut db, &c, &c.uuid, 0).await;
            assert!(
                handle_of(
                    recover_attempt_acquisition(&mut db, &replay).await,
                    "recover same durable request"
                ) == first
            );
            assert!(before == full_state(&db, &c).await);
        }
        assert_eq!(receipt_count(&db, &c).await, 1);
        assert_eq!(receipt_count(&db, &other).await, 0);

        // Authenticated ownership drift cannot rebind an existing receipt.
        db.execute(
            "UPDATE ingest_jobs SET user_id = $2 WHERE id = $1",
            &[&c.job, &other.owner],
        )
        .await
        .quiet("change fixture job ownership");
        let drifted = full_state(&db, &c).await;
        for owner in [&c.owner, &other.owner] {
            assert!(matches!(
                prepare_durable_attempt_acquisition(&mut db, &c.job, owner, &c.uuid, 0).await,
                Ok(None)
            ));
        }
        assert!(drifted == full_state(&db, &c).await);
    });
}

#[test]
fn postgres_durable_prepare_expired_and_superseded_receipts_never_renew_or_advance() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let original = durable(&mut db, &c, &c.uuid, 0).await;
        let _first = handle_of(
            acquire_prepared_attempt(&mut db, &original).await,
            "first durable attempt",
        );
        expire(&db, &c.job).await;
        let expired = full_state(&db, &c).await;
        let replay = durable(&mut db, &c, &c.uuid, 0).await;
        assert!(is_stale(&acquire_prepared_attempt(&mut db, &replay).await));
        assert!(is_stale(
            &recover_attempt_acquisition(&mut db, &replay).await
        ));
        assert!(expired == full_state(&db, &c).await);
        assert!(matches!(
            prepare_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &c.uuid, 1).await,
            Ok(None)
        ));
        let new_key = db_uuid(&db).await;
        let second_receipt = durable(&mut db, &c, &new_key, 1).await;
        let second = handle_of(
            acquire_prepared_attempt(&mut db, &second_receipt).await,
            "advance explicitly to distinct durable request",
        );
        let superseded = full_state(&db, &c).await;
        let old = durable(&mut db, &c, &c.uuid, 0).await;
        assert!(is_stale(&acquire_prepared_attempt(&mut db, &old).await));
        assert!(is_stale(&recover_attempt_acquisition(&mut db, &old).await));
        let current = durable(&mut db, &c, &new_key, 1).await;
        assert!(
            handle_of(
                recover_attempt_acquisition(&mut db, &current).await,
                "recover second durable attempt"
            ) == second
        );
        assert!(superseded == full_state(&db, &c).await);
        assert_eq!(receipt_count(&db, &c).await, 2);
    });
}

#[test]
fn postgres_durable_prepare_concurrent_same_request_persists_one_proposal_and_acquires_once() {
    run_test(async {
        let db = connect().await;
        let c = case(&db).await;
        let gate = Arc::new(Barrier::new(6));
        let mut tasks = Vec::new();
        for _ in 0..6 {
            let gate = Arc::clone(&gate);
            let (job, owner, key) = (c.job.clone(), c.owner.clone(), c.uuid.clone());
            let mut worker = connect().await;
            tasks.push(tokio::spawn(async move {
                gate.wait().await;
                prepare_durable_attempt_acquisition(&mut worker, &job, &owner, &key, 0).await
            }));
        }
        let mut receipts = Vec::new();
        for task in tasks {
            match task.await.expect("concurrent durable preparation task") {
                Ok(Some(receipt)) => receipts.push(receipt),
                _ => panic!("same durable request did not converge"),
            }
        }
        assert_eq!(receipt_count(&db, &c).await, 1);
        assert_eq!(attempt_rows(&db, &c.job).await, 0);
        let acquired = race(receipts).await;
        let mut handles = acquired
            .into_iter()
            .map(|r| handle_of(r, "concurrent durable acquire"));
        let first = handles.next().expect("one acquisition result");
        assert!(handles.all(|h| h == first));
        assert_eq!(attempt_rows(&db, &c.job).await, 1);
    });
}

#[test]
fn postgres_durable_prepare_distinct_requests_never_recover_the_winners_token() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let mut keys = Vec::new();
        let mut receipts = Vec::new();
        for _ in 0..4 {
            let key = db_uuid(&db).await;
            receipts.push(durable(&mut db, &c, &key, 0).await);
            keys.push(key);
        }
        let results = race(receipts).await;
        assert_eq!(results.iter().filter(|r| is_acquired(r)).count(), 1);
        for r in &results {
            assert!(is_acquired(r) || is_refused(r));
        }
        let winner = handle_of(
            results.into_iter().find(is_acquired).expect("winner"),
            "winner",
        );
        let before = full_state(&db, &c).await;
        let mut recovered = 0;
        for key in keys {
            let receipt = durable(&mut db, &c, &key, 0).await;
            let outcome = recover_attempt_acquisition(&mut db, &receipt).await;
            if is_acquired(&outcome) {
                assert!(handle_of(outcome, "exact request winner") == winner);
                recovered += 1;
            } else {
                assert!(is_stale(&outcome));
            }
        }
        assert_eq!(recovered, 1);
        assert_eq!(receipt_count(&db, &c).await, 4);
        assert!(before == full_state(&db, &c).await);
    });
}

#[test]
fn postgres_durable_receipt_immutability_constraints_and_migration_replay_preserve_identity() {
    run_blocking(true, async {
        let mut db = connect().await;
        let c = case(&db).await;
        let _receipt = durable(&mut db, &c, &c.uuid, 0).await;
        let before = full_state(&db, &c).await;
        for sql in [
            "UPDATE posted_file_attempt_receipts SET request_id = gen_random_uuid()::text WHERE job_id = $1",
            "UPDATE posted_file_attempt_receipts SET owner_id = gen_random_uuid()::text WHERE job_id = $1",
            "UPDATE posted_file_attempt_receipts SET attempt = attempt + 1 WHERE job_id = $1",
            "UPDATE posted_file_attempt_receipts SET token = gen_random_uuid()::text WHERE job_id = $1",
            "UPDATE posted_file_attempt_receipts SET job_id = gen_random_uuid()::text WHERE job_id = $1",
            "DELETE FROM posted_file_attempt_receipts WHERE job_id = $1",
        ] {
            expect_db_error(db.execute(sql, &[&c.job]).await, &SqlState::CHECK_VIOLATION,
                Some("posted attempt receipt is immutable"), "receipt mutation");
            assert!(before == full_state(&db, &c).await);
        }
        db.execute(
            "UPDATE posted_file_attempt_receipts SET token = token WHERE job_id = $1",
            &[&c.job],
        )
        .await
        .quiet("receipt true no-op");
        for (key, attempt, token) in [
            ("", 1_i64, "valid"),
            ("valid", 0, "valid"),
            ("valid", 1, ""),
        ] {
            expect_db_error(db.execute(
                "INSERT INTO posted_file_attempt_receipts (job_id, request_id, owner_id, attempt, token) \
                 VALUES ($1, $2, $3, $4, $5)", &[&c.job, &key, &c.owner, &attempt, &token]).await,
                &SqlState::CHECK_VIOLATION, None, "invalid receipt shape");
        }
        for _ in 0..2 {
            db.batch_execute(MIGRATION_0046)
                .await
                .quiet("replay receipt migration");
        }
        assert!(before == full_state(&db, &c).await);
        let replay = durable(&mut db, &c, &c.uuid, 0).await;
        let _handle = handle_of(
            acquire_prepared_attempt(&mut db, &replay).await,
            "acquire replayed receipt",
        );
        assert_eq!(receipt_count(&db, &c).await, 1);
    });
}
