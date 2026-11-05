//! Real-PostgreSQL tests for the schema-free additive attempt-acquisition
//! receipt API (`prepare_attempt_acquisition`, `acquire_prepared_attempt`,
//! `recover_attempt_acquisition`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `acquire`, `admitted`,
//! `run_test`, `count`, `Quiet`). Only the public API is used: no private
//! field of a receipt or handle is read. Outcomes are matched and compared
//! (`AttemptHandle: PartialEq`), never formatted, so no assertion echoes a
//! token, identity or row value. Every state comparison is a full logical
//! snapshot (ingest job, attempt row, and the existing custody bundle).
//!
//! "Ignored acknowledgment" below is a LOST-RESPONSE SIMULATION: the
//! committed acquisition result is dropped unused. It is not network or
//! commit error injection and proves nothing about `CommitUnknown`.

use super::*;
use nntune_ingest::posted_generations::{
    acquire_prepared_attempt, prepare_attempt_acquisition, recover_attempt_acquisition,
    recover_pending, AttemptAcquisition,
};
use std::sync::Arc;
use tokio::sync::Barrier;

type Acquired = Result<AttemptOutcome, StoreError>;

fn is_acquired(outcome: &Acquired) -> bool {
    matches!(outcome, Ok(AttemptOutcome::Acquired(_)))
}

fn is_stale(outcome: &Acquired) -> bool {
    matches!(outcome, Ok(AttemptOutcome::StaleOrMissing))
}

fn is_refused(outcome: &Acquired) -> bool {
    matches!(
        outcome,
        Ok(AttemptOutcome::StaleOrMissing) | Ok(AttemptOutcome::Busy)
    )
}

fn handle_of(outcome: Acquired, ctx: &'static str) -> AttemptHandle {
    match outcome {
        Ok(AttemptOutcome::Acquired(handle)) => handle,
        _ => panic!("{ctx}: expected an acquired attempt"),
    }
}

async fn prepare(db: &Client, job: &str, owner: &str, expected: i64) -> AttemptAcquisition {
    match prepare_attempt_acquisition(db, job, owner, expected).await {
        Ok(receipt) => receipt,
        Err(_) => panic!("prepare attempt acquisition"),
    }
}

async fn prepare_for(db: &Client, c: &Case, expected: i64) -> AttemptAcquisition {
    prepare(db, &c.job, &c.owner, expected).await
}

/// Full logical snapshot: ingest job row, attempt row and the custody bundle
/// keyed by the case's synthetic identities (absent parts snapshot as null).
async fn state(db: &impl tokio_postgres::GenericClient, c: &Case) -> String {
    db.query_one(
        "SELECT md5(jsonb_build_array( \
         (SELECT to_jsonb(t) FROM ingest_jobs t WHERE t.id = $1), \
         (SELECT to_jsonb(t) FROM posted_file_job_attempts t WHERE t.job_id = $1), \
         (SELECT to_jsonb(t) FROM posted_files t WHERE t.id = $2), \
         (SELECT to_jsonb(t) FROM posted_file_registry t WHERE t.plaintext_sha256 = $3), \
         (SELECT to_jsonb(t) FROM posted_file_operations t WHERE t.id = $4), \
         (SELECT to_jsonb(t) FROM posted_file_health t WHERE t.generation_id = $2), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.kind, t.owner_id) \
            FROM posted_file_references t WHERE t.generation_id = $2), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.round, t.slot, t.side) \
            FROM posted_file_article_progress t WHERE t.generation_id = $2), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.round, t.side) \
            FROM posted_file_content_checks t WHERE t.generation_id = $2), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.round) \
            FROM posted_file_recheck_schedule t WHERE t.generation_id = $2) \
         )::text)",
        &[&c.job, &c.sealed.generation, &c.sealed.hash, &c.sealed.op],
    )
    .await
    .quiet("snapshot attempt acquisition state")
    .get(0)
}

async fn attempt_row(db: &Client, job: &str) -> Option<(i64, String, i64)> {
    db.query_opt(
        "SELECT attempt, token, lease_until FROM posted_file_job_attempts WHERE job_id = $1",
        &[&job],
    )
    .await
    .quiet("read attempt row")
    .map(|row| (row.get(0), row.get(1), row.get(2)))
}

async fn attempt_rows(db: &Client, job: &str) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_job_attempts WHERE job_id = $1",
        &[&job],
    )
    .await
}

/// Lease expressed relative to the database clock, as production does.
async fn set_lease(db: &Client, job: &str, seconds_from_now: i64) {
    db.execute(
        "UPDATE posted_file_job_attempts \
         SET lease_until = extract(epoch FROM clock_timestamp())::bigint + $2::bigint \
         WHERE job_id = $1",
        &[&job, &seconds_from_now],
    )
    .await
    .quiet("set attempt fixture lease");
}

async fn expire(db: &Client, job: &str) {
    db.execute(
        "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
        &[&job],
    )
    .await
    .quiet("expire attempt fixture lease");
}

async fn set_status(db: &Client, job: &str, status: &'static str) {
    db.execute(
        "UPDATE ingest_jobs SET status = $2 WHERE id = $1",
        &[&job, &status],
    )
    .await
    .quiet("set job fixture status");
}

/// A job whose attempt 1 was acquired through the existing path and whose
/// lease has expired; returns the old handle (never used again).
async fn expired_predecessor(db: &mut Client, c: &Case) -> AttemptHandle {
    let handle = acquire(db, &c.job, &c.owner, 0).await;
    expire(db, &c.job).await;
    handle
}

/// Real concurrency on independent sessions released from one barrier.
async fn race(receipts: Vec<AttemptAcquisition>) -> Vec<Acquired> {
    let gate = Arc::new(Barrier::new(receipts.len()));
    let mut tasks = Vec::new();
    for receipt in receipts {
        let gate = Arc::clone(&gate);
        let mut db = connect().await;
        tasks.push(tokio::spawn(async move {
            gate.wait().await;
            acquire_prepared_attempt(&mut db, &receipt).await
        }));
    }
    let mut results = Vec::new();
    for task in tasks {
        results.push(
            task.await
                .unwrap_or_else(|_| panic!("racing acquirer task")),
        );
    }
    results
}

#[derive(Clone, Copy)]
enum Held {
    Job,
    Attempt,
}

async fn backend(db: &Client) -> i32 {
    db.query_one("SELECT pg_backend_pid()", &[])
        .await
        .quiet("read acquisition backend")
        .get(0)
}

async fn blocked_by(db: &Client, waiter: i32, holder: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = db
                .query_one("SELECT $2 = ANY(pg_blocking_pids($1))", &[&waiter, &holder])
                .await
                .quiet("observe acquisition lock barrier")
                .get(0);
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovery never blocked on the held job/attempt lock");
}

/// An independent session holds the job or attempt row lock while recovery
/// waits on it. With `expire_during_wait` the lease runs out on the real
/// database clock while recovery is blocked; otherwise nothing changes.
async fn recover_across_lock_wait(held: Held, expire_during_wait: bool) {
    let mut db = connect().await;
    let c = case(&db).await;
    let receipt = prepare_for(&db, &c, 0).await;
    let handle = handle_of(
        acquire_prepared_attempt(&mut db, &receipt).await,
        "acquire before lock wait",
    );
    let mut blocker = connect().await;
    let blocker_pid = backend(&blocker).await;
    let mut worker = connect().await;
    let worker_pid = backend(&worker).await;
    let now: i64 = db
        .query_one("SELECT extract(epoch FROM clock_timestamp())::bigint", &[])
        .await
        .quiet("read acquisition database clock")
        .get(0);
    let deadline = now + 3;
    if expire_during_wait {
        db.execute(
            "UPDATE posted_file_job_attempts SET lease_until = $2 WHERE job_id = $1",
            &[&c.job, &deadline],
        )
        .await
        .quiet("set short fixture lease");
    }
    let before = state(&db, &c).await;

    let tx = blocker
        .transaction()
        .await
        .quiet("begin acquisition barrier");
    match held {
        Held::Job => {
            tx.query_one(
                "SELECT id FROM ingest_jobs WHERE id = $1 FOR UPDATE",
                &[&c.job],
            )
            .await
            .quiet("hold job row");
        }
        Held::Attempt => {
            tx.query_one(
                "SELECT attempt FROM posted_file_job_attempts WHERE job_id = $1 FOR UPDATE",
                &[&c.job],
            )
            .await
            .quiet("hold attempt row");
        }
    }
    let (outcome, during) =
        tokio::join!(recover_attempt_acquisition(&mut worker, &receipt), async {
            blocked_by(&db, worker_pid, blocker_pid).await;
            assert!(
                before == state(&tx, &c).await,
                "recovery wrote before the barrier"
            );
            if expire_during_wait {
                let live: bool = tx
                    .query_one(
                        "SELECT extract(epoch FROM clock_timestamp())::bigint < $1",
                        &[&deadline],
                    )
                    .await
                    .quiet("verify recovery reached the barrier with a live lease")
                    .get(0);
                assert!(live, "recovery did not reach the barrier with a live lease");
                tx.query_one(
                    "SELECT pg_sleep(GREATEST(0.0, $1::bigint::double precision - \
                         extract(epoch FROM clock_timestamp())::double precision) + 1.0)",
                    &[&deadline],
                )
                .await
                .quiet("expire lease on the database clock");
            }
            let snapshot = state(&tx, &c).await;
            tx.commit().await.quiet("release acquisition barrier");
            snapshot
        });
    if expire_during_wait {
        assert!(
            is_stale(&outcome),
            "recovery must refuse a lease that expired during the lock wait"
        );
    } else {
        assert!(
            handle_of(outcome, "unchanged control recovery") == handle,
            "unchanged control must recover the same handle"
        );
    }
    assert!(before == during, "state moved while recovery was blocked");
    assert!(
        before == state(&db, &c).await,
        "recovery changed durable state"
    );
}

// ---- tests ----------------------------------------------------------------

#[test]
fn postgres_acquisition_prepare_rejects_invalid_input_writes_nothing_and_is_redacted() {
    run_test(async {
        let db = connect().await;
        let c = case(&db).await;
        let before = state(&db, &c).await;
        let long = "a".repeat(65);
        for bad in ["", "a b", "a/b", "a\n", "é", long.as_str()] {
            assert!(matches!(
                prepare_attempt_acquisition(&db, bad, &c.owner, 0).await,
                Err(StoreError::InvalidInput)
            ));
            assert!(matches!(
                prepare_attempt_acquisition(&db, &c.job, bad, 0).await,
                Err(StoreError::InvalidInput)
            ));
        }
        for bad in [-1, i64::MIN, i64::MAX] {
            assert!(matches!(
                prepare_attempt_acquisition(&db, &c.job, &c.owner, bad).await,
                Err(StoreError::InvalidInput)
            ));
        }
        assert!(before == state(&db, &c).await);

        // Valid preparation (including a copy) grants and persists nothing.
        let receipt = prepare_for(&db, &c, 0).await;
        let copy = receipt.clone();
        let again = prepare_for(&db, &c, 0).await;
        assert!(before == state(&db, &c).await);
        assert_eq!(attempt_rows(&db, &c.job).await, 0);
        assert!(attempt_row(&db, &c.job).await.is_none());

        // Nothing was persisted, so no receipt recovers anything yet.
        let mut reader = connect().await;
        for r in [&receipt, &copy, &again] {
            assert!(is_stale(&recover_attempt_acquisition(&mut reader, r).await));
        }
        assert!(before == state(&db, &c).await);

        for r in [&receipt, &copy] {
            let shown = format!("{r:?}");
            assert!(shown.contains("REDACTED"), "receipt debug is not redacted");
            for secret in [&c.job, &c.owner, &c.uuid] {
                assert!(
                    !shown.contains(secret.as_str()),
                    "receipt debug leaks input"
                );
            }
        }
    });
}

#[test]
fn postgres_acquisition_lost_response_simulation_recovers_same_attempt_and_pending_custody() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let receipt = prepare_for(&db, &c, 0).await;

        // LOST-RESPONSE SIMULATION: the acquisition commits, then the caller
        // drops the returned handle unused. No network or commit fault is
        // injected here.
        let ignored = acquire_prepared_attempt(&mut db, &receipt).await;
        assert!(is_acquired(&ignored));
        drop(ignored);
        let (attempt, token, lease) = attempt_row(&db, &c.job).await.expect("persisted attempt");
        assert_eq!(attempt, 1);
        let before = state(&db, &c).await;

        let mut reader = connect().await;
        let recovered = handle_of(
            recover_attempt_acquisition(&mut reader, &receipt).await,
            "recover ignored acknowledgment",
        );
        assert!(before == state(&db, &c).await, "recovery wrote state");
        assert!(attempt_row(&db, &c.job).await == Some((attempt, token.clone(), lease)));

        // The recovered handle is real posting authority for the same attempt.
        let generation = admitted(&mut db, &recovered, s).await;
        assert_eq!(generation.operation_id(), s.op);
        let pinned = db
            .query_one(
                "SELECT attempt = $2 AND attempt_token = $3 FROM posted_file_operations WHERE id = $1",
                &[&s.op, &attempt, &token],
            )
            .await
            .quiet("read admitted attempt binding")
            .get::<_, bool>(0);
        assert!(pinned, "candidate is not bound to the recovered attempt");
        assert_eq!(attempt_rows(&db, &c.job).await, 1);

        // Lost admission acknowledgment too: drop the generation handle and
        // re-derive exact pending custody from the recovered attempt handle.
        drop(generation);
        let after_admit = state(&db, &c).await;
        let again = handle_of(
            recover_attempt_acquisition(&mut reader, &receipt).await,
            "recover after admission",
        );
        assert!(again == recovered);
        let Ok(AdmissionOutcome::ResumedSameCandidate(resumed, bundle)) =
            recover_pending(&mut reader, &again, &s.hash).await
        else {
            panic!("pending candidate was not resumed through the recovered handle")
        };
        assert!(resumed.generation_id() == s.generation);
        assert!(resumed.operation_id() == s.op);
        assert!(bundle.generation_id() == s.generation);
        assert!(bundle.plan_sha256() == s.plan);
        assert!(bundle.custody().expect("recovered custody") == s.custody());
        assert!(posting_fence_is_current(&mut reader, &resumed)
            .await
            .expect("recovered posting fence"));
        assert!(
            after_admit == state(&db, &c).await,
            "pending recovery wrote state"
        );
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
    });
}

#[test]
fn postgres_acquisition_duplicate_acquire_and_recover_do_not_renew_or_write() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let receipt = prepare_for(&db, &c, 0).await;
        let first = handle_of(
            acquire_prepared_attempt(&mut db, &receipt).await,
            "first acquisition",
        );
        let initial: bool = db
            .query_one(
                "SELECT lease_until BETWEEN extract(epoch FROM clock_timestamp())::bigint + 290 \
                 AND extract(epoch FROM clock_timestamp())::bigint + 301 \
                 FROM posted_file_job_attempts WHERE job_id = $1",
                &[&c.job],
            )
            .await
            .quiet("read first lease")
            .get(0);
        assert!(
            initial,
            "first acquisition did not take a database-clock lease"
        );

        // A lease strictly inside the renewal window: any renewal is visible.
        set_lease(&db, &c.job, 100).await;
        let fixed = attempt_row(&db, &c.job).await.expect("attempt row");
        let before = state(&db, &c).await;
        let mut other = connect().await;
        let copy = receipt.clone();
        for _ in 0..2 {
            assert!(
                handle_of(
                    acquire_prepared_attempt(&mut db, &receipt).await,
                    "duplicate acquire"
                ) == first
            );
            assert!(
                handle_of(
                    acquire_prepared_attempt(&mut other, &copy).await,
                    "duplicate acquire on a second session"
                ) == first
            );
            assert!(
                handle_of(
                    recover_attempt_acquisition(&mut other, &receipt).await,
                    "duplicate recover"
                ) == first
            );
            assert!(before == state(&db, &c).await, "duplicate call wrote state");
            assert!(attempt_row(&db, &c.job).await == Some(fixed.clone()));
        }

        // Same with a pending candidate in the snapshot.
        let generation = admitted(&mut db, &first, s).await;
        drop(generation);
        let admitted_state = state(&db, &c).await;
        assert!(
            handle_of(
                acquire_prepared_attempt(&mut other, &receipt).await,
                "duplicate acquire with candidate"
            ) == first
        );
        assert!(
            handle_of(
                recover_attempt_acquisition(&mut other, &copy).await,
                "duplicate recover with candidate"
            ) == first
        );
        assert!(admitted_state == state(&db, &c).await);
        assert_eq!(attempt_rows(&db, &c.job).await, 1);
    });
}

#[test]
fn postgres_acquisition_wrong_request_identity_drift_status_and_absence_refuse() {
    run_test(async {
        let mut db = connect().await;
        let mut other = connect().await;
        let c = case(&db).await;
        let receipt = prepare_for(&db, &c, 0).await;

        // Absent: nothing persisted, so nothing recovers.
        let absent = state(&db, &c).await;
        assert!(is_stale(
            &recover_attempt_acquisition(&mut other, &receipt).await
        ));
        assert!(absent == state(&db, &c).await);

        let live = handle_of(
            acquire_prepared_attempt(&mut db, &receipt).await,
            "acquire live work",
        );
        let before = state(&db, &c).await;

        // Different requests, same job and owner: stale predecessor number
        // and exact-predecessor number against LIVE work. Neither recovers
        // nor replaces it.
        for expected in [0, 1] {
            let wrong = prepare_for(&db, &c, expected).await;
            assert!(is_refused(
                &acquire_prepared_attempt(&mut other, &wrong).await
            ));
            assert!(is_stale(
                &recover_attempt_acquisition(&mut other, &wrong).await
            ));
            assert!(before == state(&db, &c).await);
        }

        // Identity drift: same job with another valid owner, and another
        // job (never acquired) with the right owner.
        let stranger = seed_user(&db, &format!("{}-drift", c.uuid)).await;
        let drift_owner = prepare(&db, &c.job, &stranger, 0).await;
        assert!(is_refused(
            &acquire_prepared_attempt(&mut other, &drift_owner).await
        ));
        assert!(is_stale(
            &recover_attempt_acquisition(&mut other, &drift_owner).await
        ));
        let foreign = case(&db).await;
        let drift_job = prepare(&db, &foreign.job, &c.owner, 0).await;
        assert!(is_refused(
            &acquire_prepared_attempt(&mut other, &drift_job).await
        ));
        assert!(is_stale(
            &recover_attempt_acquisition(&mut other, &drift_job).await
        ));
        assert!(before == state(&db, &c).await);
        assert_eq!(attempt_rows(&db, &foreign.job).await, 0);

        // A stopped job refuses both entries; restoring it recovers again.
        set_status(&db, &c.job, "failed").await;
        let stopped = state(&db, &c).await;
        assert!(is_stale(
            &recover_attempt_acquisition(&mut other, &receipt).await
        ));
        assert!(is_stale(
            &acquire_prepared_attempt(&mut other, &receipt).await
        ));
        assert!(stopped == state(&db, &c).await);
        set_status(&db, &c.job, "running").await;
        assert!(
            handle_of(
                recover_attempt_acquisition(&mut other, &receipt).await,
                "positive control after restore"
            ) == live
        );
    });
}

#[test]
fn postgres_acquisition_expired_receipt_and_superseded_attempts_refuse_without_writes() {
    run_test(async {
        let mut db = connect().await;
        let mut other = connect().await;
        let c = case(&db).await;
        let first_request = prepare_for(&db, &c, 0).await;
        let first = handle_of(
            acquire_prepared_attempt(&mut db, &first_request).await,
            "acquire attempt one",
        );
        expire(&db, &c.job).await;

        // Expired matching request: refuses, never renews or re-acquires.
        let expired = state(&db, &c).await;
        assert!(is_stale(
            &acquire_prepared_attempt(&mut other, &first_request).await
        ));
        assert!(is_stale(
            &recover_attempt_acquisition(&mut other, &first_request).await
        ));
        assert!(expired == state(&db, &c).await);

        // Exact expired predecessor, new request: advances once.
        let second_request = prepare_for(&db, &c, 1).await;
        let second = handle_of(
            acquire_prepared_attempt(&mut db, &second_request).await,
            "acquire attempt two",
        );
        assert!(second != first);
        assert_eq!(attempt_row(&db, &c.job).await.map(|r| r.0), Some(2));
        expire(&db, &c.job).await;
        let third_request = prepare_for(&db, &c, 2).await;
        let third = handle_of(
            acquire_prepared_attempt(&mut db, &third_request).await,
            "acquire attempt three",
        );
        assert!(third != second && third != first);
        let live = state(&db, &c).await;

        // Superseded requests refuse on both entries with no writes.
        for old in [&first_request, &second_request] {
            assert!(is_stale(&acquire_prepared_attempt(&mut other, old).await));
            assert!(is_stale(
                &recover_attempt_acquisition(&mut other, old).await
            ));
            assert!(live == state(&db, &c).await, "superseded request wrote");
        }
        // The current request is untouched and still recovers.
        assert!(
            handle_of(
                recover_attempt_acquisition(&mut other, &third_request).await,
                "current request after refusals"
            ) == third
        );
        assert!(live == state(&db, &c).await);
        assert_eq!(attempt_rows(&db, &c.job).await, 1);
    });
}

#[test]
fn postgres_acquisition_concurrent_same_request_acquires_once_with_identical_handles() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let predecessor = expired_predecessor(&mut db, &c).await;
        let (old_attempt, old_token, _) = attempt_row(&db, &c.job).await.expect("predecessor");
        let receipt = prepare_for(&db, &c, old_attempt).await;

        let outcomes = race(vec![receipt.clone(); 4]).await;
        let handles: Vec<AttemptHandle> = outcomes
            .into_iter()
            .map(|o| handle_of(o, "same-request racer"))
            .collect();
        assert!(handles.iter().all(|h| *h == handles[0]));
        assert!(handles[0] != predecessor);

        let (attempt, token, _) = attempt_row(&db, &c.job).await.expect("advanced attempt");
        assert_eq!(attempt, old_attempt + 1, "advanced more than once");
        assert!(token != old_token, "token was not rotated");
        assert_eq!(attempt_rows(&db, &c.job).await, 1);
        let settled = state(&db, &c).await;
        assert!(
            handle_of(
                recover_attempt_acquisition(&mut db, &receipt).await,
                "recover after race"
            ) == handles[0]
        );
        assert!(settled == state(&db, &c).await);
    });
}

#[test]
fn postgres_acquisition_concurrent_distinct_requests_have_one_winner_and_no_shared_token() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let _predecessor = expired_predecessor(&mut db, &c).await;
        let (old_attempt, old_token, _) = attempt_row(&db, &c.job).await.expect("predecessor");
        let mut receipts = Vec::new();
        for _ in 0..3 {
            receipts.push(prepare_for(&db, &c, old_attempt).await);
        }

        let outcomes = race(receipts.clone()).await;
        let winners: Vec<usize> = (0..outcomes.len())
            .filter(|i| is_acquired(&outcomes[*i]))
            .collect();
        assert_eq!(winners.len(), 1, "expected exactly one acquisition winner");
        for (i, outcome) in outcomes.iter().enumerate() {
            if i != winners[0] {
                assert!(is_refused(outcome), "a losing request was not refused");
            }
        }
        let (attempt, token, _) = attempt_row(&db, &c.job).await.expect("winner attempt");
        assert_eq!(attempt, old_attempt + 1);
        assert!(token != old_token);
        assert_eq!(attempt_rows(&db, &c.job).await, 1);

        // Only the winner's request is bound to the persisted token: the
        // winner's handle is recoverable, no loser can recover or share it.
        let settled = state(&db, &c).await;
        let mut reader = connect().await;
        let mut winner_handle = None;
        for (i, receipt) in receipts.iter().enumerate() {
            let recovered = recover_attempt_acquisition(&mut reader, receipt).await;
            if i == winners[0] {
                winner_handle = Some(handle_of(recovered, "winner recovery"));
            } else {
                assert!(
                    is_stale(&recovered),
                    "a loser recovered the winner's attempt"
                );
            }
        }
        let mut outcomes = outcomes;
        let raced = handle_of(outcomes.swap_remove(winners[0]), "raced winner");
        assert!(
            winner_handle == Some(raced),
            "winner handle is not the persisted one"
        );
        assert!(settled == state(&db, &c).await);
    });
}

#[test]
fn postgres_acquisition_recovery_rechecks_job_and_attempt_lock_waits_on_the_database_clock() {
    run_test(async {
        for held in [Held::Job, Held::Attempt] {
            // Positive control first: an unchanged lock wait still recovers.
            recover_across_lock_wait(held, false).await;
            // The lease expires on the real database clock during the wait.
            recover_across_lock_wait(held, true).await;
        }
    });
}
