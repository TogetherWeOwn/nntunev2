//! Deletion scheduling between durable startup and later use must fence every
//! attempt/claim entry. Independent sessions establish both lock orderings;
//! these are database controls, not cancellation of in-flight network requests.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, prepare_content_check, prepare_presence_check, record_content_check,
    record_presence_check, renew_candidate_lease, ArticleSide, ContentCheckResult,
    GenerationHandle, MutationOutcome,
};
use nntune_ingest::two_spool::StatVerdict;

async fn schedule(db: &impl tokio_postgres::GenericClient, owner: &str) {
    assert_eq!(
        db.execute(
            "UPDATE users SET deleting_at = clock_timestamp() WHERE id = $1",
            &[&owner],
        )
        .await
        .quiet("schedule generation owner deletion"),
        1
    );
}

async fn candidate(db: &mut Client) -> (Case, AttemptAcquisition, AttemptHandle, GenerationHandle) {
    let (c, key) = queued_case(db).await;
    let receipt = start(db, &c, &key, &c.uuid, 0).await;
    let attempt = handle_of(
        acquire_prepared_attempt(db, &receipt).await,
        "acquire owner fixture",
    );
    let admitted_handle = admitted(db, &attempt, &c.sealed).await;
    let handle = match begin_verification_round(db, &admitted_handle).await {
        Ok(MutationOutcome::Applied(handle)) => *handle,
        _ => panic!("start owner fixture verification"),
    };
    (c, receipt, attempt, handle)
}

#[test]
fn postgres_owner_deletion_fences_all_attempt_entries_after_startup_without_mutation() {
    run_test(async {
        let mut db = connect().await;
        for acquired in [false, true] {
            let (c, key) = queued_case(&db).await;
            let receipt = start(&mut db, &c, &key, &c.uuid, 0).await;
            if acquired {
                let _handle = handle_of(
                    acquire_prepared_attempt(&mut db, &receipt).await,
                    "acquire before deletion",
                );
            }
            schedule(&db, &c.owner).await;
            let before = startup_state(&db, &c).await;
            assert!(matches!(
                prepare_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &c.uuid, 0).await,
                Ok(None)
            ));
            let other_request = db_uuid(&db).await;
            assert!(matches!(
                prepare_durable_attempt_acquisition(&mut db, &c.job, &c.owner, &other_request, 0)
                    .await,
                Ok(None)
            ));
            assert!(is_stale(
                &acquire_attempt(&mut db, &c.job, &c.owner, 0).await
            ));
            assert!(is_stale(&acquire_prepared_attempt(&mut db, &receipt).await));
            assert!(is_stale(
                &recover_attempt_acquisition(&mut db, &receipt).await
            ));
            assert!(before == startup_state(&db, &c).await);
            // Grace cancellation restores the original receipt, never a new token.
            db.execute(
                "UPDATE users SET deleting_at = NULL WHERE id = $1",
                &[&c.owner],
            )
            .await
            .quiet("cancel fixture deletion");
            let first = handle_of(
                acquire_prepared_attempt(&mut db, &receipt).await,
                "acquire after grace cancellation",
            );
            let restored = handle_of(
                recover_attempt_acquisition(&mut db, &receipt).await,
                "recover after grace cancellation",
            );
            assert!(first == restored);
            assert_eq!(receipt_count(&db, &c).await, 1);
        }
    });
}

#[test]
fn postgres_owner_deletion_fences_existing_candidate_handles_and_original_callbacks() {
    run_test(async {
        let mut db = connect().await;
        let (c, receipt, attempt, handle) = candidate(&mut db).await;
        let presence = match prepare_presence_check(&mut db, &handle, 0, ArticleSide::Post).await {
            Ok(Some(permit)) => permit,
            _ => panic!("prepare presence before deletion"),
        };
        let content = match prepare_content_check(&mut db, &handle, ArticleSide::Post).await {
            Ok(Some(permit)) => permit,
            _ => panic!("prepare content before deletion"),
        };
        schedule(&db, &c.owner).await;
        let before = startup_state(&db, &c).await;
        assert!(matches!(
            posting_fence_is_current(&mut db, &handle).await,
            Ok(false)
        ));
        assert!(matches!(
            renew_candidate_lease(&mut db, &handle).await,
            Ok(false)
        ));
        assert!(matches!(
            recover_pending(&mut db, &attempt, &c.sealed.hash).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        assert!(is_stale(
            &recover_attempt_acquisition(&mut db, &receipt).await
        ));
        assert!(matches!(
            prepare_presence_check(&mut db, &handle, 0, ArticleSide::Post).await,
            Ok(None)
        ));
        assert!(matches!(
            prepare_content_check(&mut db, &handle, ArticleSide::Post).await,
            Ok(None)
        ));
        assert!(matches!(
            record_presence_check(&mut db, &presence, StatVerdict::Present).await,
            Ok(MutationOutcome::StaleOrDiverged)
        ));
        assert!(matches!(
            record_content_check(&mut db, &content, ContentCheckResult::Unavailable).await,
            Ok(MutationOutcome::StaleOrDiverged)
        ));
        assert!(before == startup_state(&db, &c).await);
    });
}

#[test]
fn postgres_owner_deletion_attempt_entries_recheck_after_owner_lock_wait() {
    run_test(async {
        let db = connect().await;
        // Legacy acquire, exact acquire, lost-ack recovery, durable preparation.
        for entry in 0..4 {
            for commit_deletion in [false, true] {
                let mut worker = connect().await;
                let (c, key) = queued_case(&db).await;
                let receipt = start(&mut worker, &c, &key, &c.uuid, 0).await;
                if entry == 2 {
                    let _handle = handle_of(
                        acquire_prepared_attempt(&mut worker, &receipt).await,
                        "acquire before recovery race",
                    );
                }
                let waiter = backend(&worker).await;
                let mut holder = connect().await;
                let holder_pid = backend(&holder).await;
                let tx = holder.transaction().await.quiet("owner race transaction");
                schedule(&tx, &c.owner).await;
                let before_commit = startup_state(&tx, &c).await;
                let job = c.job.clone();
                let owner = c.owner.clone();
                let request = c.uuid.clone();
                let task = tokio::spawn(async move {
                    match entry {
                        0 => is_acquired(&acquire_attempt(&mut worker, &job, &owner, 0).await),
                        1 => is_acquired(&acquire_prepared_attempt(&mut worker, &receipt).await),
                        2 => is_acquired(&recover_attempt_acquisition(&mut worker, &receipt).await),
                        _ => match prepare_durable_attempt_acquisition(
                            &mut worker,
                            &job,
                            &owner,
                            &request,
                            0,
                        )
                        .await
                        {
                            Ok(receipt) => receipt.is_some(),
                            Err(_) => panic!("prepare behind owner fence"),
                        },
                    }
                });
                blocked_by(&db, waiter, holder_pid).await;
                assert!(!task.is_finished());
                if commit_deletion {
                    tx.commit().await.quiet("commit deletion winner");
                } else {
                    tx.rollback().await.quiet("rollback deletion control");
                }
                let accepted = task.await.unwrap_or_else(|_| panic!("owner race task"));
                assert_eq!(accepted, !commit_deletion);
                if commit_deletion {
                    assert!(before_commit == startup_state(&db, &c).await);
                }
            }
        }
    });
}

#[test]
fn postgres_owner_deletion_candidate_fence_rechecks_after_owner_lock_wait() {
    run_test(async {
        let mut db = connect().await;
        for commit_deletion in [false, true] {
            let (c, _receipt, _attempt, handle) = candidate(&mut db).await;
            let mut worker = connect().await;
            let waiter = backend(&worker).await;
            let mut holder = connect().await;
            let holder_pid = backend(&holder).await;
            let tx = holder.transaction().await.quiet("candidate owner race");
            schedule(&tx, &c.owner).await;
            let before = startup_state(&tx, &c).await;
            let task =
                tokio::spawn(async move { posting_fence_is_current(&mut worker, &handle).await });
            blocked_by(&db, waiter, holder_pid).await;
            assert!(!task.is_finished());
            if commit_deletion {
                tx.commit().await.quiet("commit candidate owner deletion");
            } else {
                tx.rollback()
                    .await
                    .quiet("rollback candidate owner deletion");
            }
            assert!(matches!(task.await, Ok(Ok(current)) if current != commit_deletion));
            if commit_deletion {
                assert!(before == startup_state(&db, &c).await);
            }
        }
    });
}

#[test]
fn postgres_owner_fence_is_held_before_job_wait_and_through_attempt_commit() {
    run_test(async {
        let db = connect().await;
        let mut worker = connect().await;
        let (c, key) = queued_case(&db).await;
        let receipt = start(&mut worker, &c, &key, &c.uuid, 0).await;
        let mut job_holder = connect().await;
        let job_pid = backend(&job_holder).await;
        let job_tx = job_holder.transaction().await.quiet("hold fixture job");
        job_tx
            .query_one(
                "SELECT id FROM ingest_jobs WHERE id = $1 FOR UPDATE",
                &[&c.job],
            )
            .await
            .quiet("lock fixture job");
        let writer_pid = backend(&worker).await;
        let writer =
            tokio::spawn(async move { acquire_prepared_attempt(&mut worker, &receipt).await });
        blocked_by(&db, writer_pid, job_pid).await;
        // The writer is blocked on the job, but must already hold the user fence.
        let deleter = connect().await;
        let deleter_pid = backend(&deleter).await;
        let owner = c.owner.clone();
        let deletion = tokio::spawn(async move { schedule(&deleter, &owner).await });
        blocked_by(&db, deleter_pid, writer_pid).await;
        assert!(!writer.is_finished());
        assert!(!deletion.is_finished());
        job_tx.commit().await.quiet("release job for fenced writer");
        let _handle = handle_of(
            writer
                .await
                .unwrap_or_else(|_| panic!("fenced writer task")),
            "writer wins before deletion",
        );
        deletion
            .await
            .unwrap_or_else(|_| panic!("deletion waiter task"));
        assert_eq!(attempt_rows(&db, &c.job).await, 1);
        let mut db = db;
        let before = startup_state(&db, &c).await;
        let receipt = prepare_for(&db, &c, 0).await;
        assert!(is_stale(&acquire_prepared_attempt(&mut db, &receipt).await));
        assert!(before == startup_state(&db, &c).await);
    });
}
