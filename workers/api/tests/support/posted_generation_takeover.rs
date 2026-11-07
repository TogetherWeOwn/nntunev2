//! Real-PostgreSQL tests for the fenced pending-candidate takeover adapter
//! (`take_over`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `acquire`, `admitted`,
//! `observe`, `count`, `snapshot`, `recover`). Fixtures stay synthetic sealed
//! byte buffers keyed by database UUIDs. Failures print only static labels;
//! outcomes are matched, never formatted, so no assertion can echo handles,
//! claim tokens or encrypted material.
//!
//! Scope: resuming the exact stored pending candidate under a newer persisted
//! attempt of the same job and owner. Nothing here mints, replaces, reuses,
//! promotes, proves content or cleans up, and health must stay
//! unknown/verifying throughout.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, confirm_article, take_over, ArticleSide, GenerationHandle,
    MutationOutcome, StoredGeneration,
};
use std::sync::Arc;
use tokio::sync::Barrier;

fn applied(outcome: Result<MutationOutcome, StoreError>, ctx: &'static str) -> GenerationHandle {
    match outcome {
        Ok(MutationOutcome::Applied(handle)) => *handle,
        _ => panic!("{ctx}: expected an applied mutation"),
    }
}

fn resumed(
    outcome: Result<AdmissionOutcome, StoreError>,
    ctx: &'static str,
) -> (GenerationHandle, StoredGeneration) {
    match outcome {
        Ok(AdmissionOutcome::ResumedSameCandidate(handle, bundle)) => (handle, bundle),
        _ => panic!("{ctx}: expected the resumed stored candidate"),
    }
}

/// Stale, diverged, inconsistent or missing: refused without a candidate.
fn is_refused(outcome: &Result<AdmissionOutcome, StoreError>) -> bool {
    matches!(
        outcome,
        Ok(AdmissionOutcome::StaleOrDiverged) | Ok(AdmissionOutcome::InconsistentOrMissing)
    )
}

fn is_busy(outcome: &Result<AdmissionOutcome, StoreError>) -> bool {
    matches!(outcome, Ok(AdmissionOutcome::ClaimBusy))
}

struct Op {
    claim_token: String,
    attempt: i64,
    attempt_token: String,
    live: bool,
    fixed: String,
}

async fn op(db: &Client, s: &Sealed) -> Op {
    let row = db
        .query_one(
            "SELECT claim_token, attempt, attempt_token, \
             lease_until > floor(extract(epoch FROM clock_timestamp()))::bigint, \
             md5(row(id, plaintext_sha256, generation_id, job_id, predecessor, \
                     predecessor_health_revision, registry_revision, state)::text) \
             FROM posted_file_operations WHERE id = $1",
            &[&s.op],
        )
        .await
        .quiet("read operation row");
    Op {
        claim_token: row.get(0),
        attempt: row.get(1),
        attempt_token: row.get(2),
        live: row.get(3),
        fixed: row.get(4),
    }
}

async fn attempt_token(db: &Client, job: &str) -> String {
    db.query_one(
        "SELECT token FROM posted_file_job_attempts WHERE job_id = $1",
        &[&job],
    )
    .await
    .quiet("read attempt token")
    .get(0)
}

/// Digest of every progress row (confirmations and repost budgets).
async fn progress_digest(db: &Client, s: &Sealed) -> Option<String> {
    db.query_one(
        "SELECT md5(string_agg(t::text, ',' ORDER BY t.round, t.slot, t.side)) \
         FROM posted_file_article_progress t WHERE t.generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("digest progress rows")
    .get(0)
}

async fn confirmed(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_article_progress \
         WHERE generation_id = $1 AND confirmed",
        &[&s.generation],
    )
    .await
}

async fn repost_budget(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT COALESCE(sum(repost_attempts), 0)::bigint \
         FROM posted_file_article_progress WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
}

async fn health_state(db: &Client, s: &Sealed) -> String {
    db.query_one(
        "SELECT state FROM posted_file_health WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("read health state")
    .get(0)
}

async fn expire_attempt(db: &Client, job: &str) {
    db.execute(
        "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
        &[&job],
    )
    .await
    .quiet("expire attempt");
}

async fn expire_operation(db: &Client, s: &Sealed) {
    db.execute(
        "UPDATE posted_file_operations SET lease_until = 0 WHERE id = $1",
        &[&s.op],
    )
    .await
    .quiet("expire operation");
}

/// Everything takeover must never touch, plus the operation row and progress.
type Frozen = (Vec<Option<String>>, Option<String>, String);

async fn frozen(db: &Client, s: &Sealed) -> Frozen {
    (
        snapshot(db, s).await,
        progress_digest(db, s).await,
        op(db, s).await.claim_token,
    )
}

struct Prepared {
    case: Case,
    first_attempt: AttemptHandle,
    old_handle: GenerationHandle,
    next_attempt: AttemptHandle,
}

/// Attempt 1 admitted a candidate, began a round and confirmed three articles
/// (one with a nonzero repost budget). Only the attempt lease is expired when
/// `expire_op` is false; with it true the operation lease is expired too.
/// Attempt 2 is then persisted and holds the job.
async fn prepared(db: &mut Client, expire_op: bool) -> Prepared {
    let c = case(db).await;
    let s = &c.sealed;
    let first_attempt = acquire(db, &c.job, &c.owner, 0).await;
    let admitted_handle = admitted(db, &first_attempt, s).await;
    let mut handle = applied(
        begin_verification_round(db, &admitted_handle).await,
        "begin round",
    );
    for (slot, side) in [
        (0, ArticleSide::Post),
        (0, ArticleSide::Verify),
        (1, ArticleSide::Post),
    ] {
        handle = applied(
            confirm_article(db, &handle, slot, side).await,
            "confirm fixture article",
        );
    }
    db.execute(
        "UPDATE posted_file_article_progress SET repost_attempts = 2 \
         WHERE generation_id = $1 AND slot = 0 AND side = 0",
        &[&s.generation],
    )
    .await
    .quiet("seed repost budget");
    expire_attempt(db, &c.job).await;
    if expire_op {
        expire_operation(db, s).await;
    }
    let next_attempt = acquire(db, &c.job, &c.owner, 1).await;
    Prepared {
        case: c,
        first_attempt,
        old_handle: handle,
        next_attempt,
    }
}

#[test]
fn postgres_expired_takeover_resumes_exact_bundle_and_keeps_progress_and_repost_budget() {
    run_test(async {
        let mut db = connect().await;
        let p = prepared(&mut db, true).await;
        let s = &p.case.sealed;

        let before = frozen(&db, s).await;
        let before_op = op(&db, s).await;
        assert!(!before_op.live, "fixture operation lease must be expired");
        assert_eq!(before_op.attempt, 1);
        assert_eq!(confirmed(&db, s).await, 3);
        assert_eq!(repost_budget(&db, s).await, 2);
        assert_eq!(health_state(&db, s).await, "verifying");

        let expected = observe(&db, &s.hash).await.expect("observe pending hash");
        let (handle, bundle) = resumed(
            take_over(&mut db, &p.next_attempt, &expected, &s.op).await,
            "takeover of expired operation",
        );
        assert_eq!(handle.generation_id(), s.generation);
        assert_eq!(handle.operation_id(), s.op);
        assert_eq!(bundle.generation_id(), s.generation);
        assert_eq!(bundle.plan_sha256(), s.plan);
        assert!(bundle.custody().expect("stored custody") == s.custody());

        // The claim moved to the new attempt and rotated; nothing else did.
        let after_op = op(&db, s).await;
        assert_eq!(after_op.attempt, 2);
        assert!(after_op.attempt_token == attempt_token(&db, &p.case.job).await);
        assert!(after_op.attempt_token != before_op.attempt_token);
        assert!(after_op.claim_token != before_op.claim_token);
        assert!(after_op.live, "takeover must install a live lease");
        assert!(after_op.fixed == before_op.fixed, "operation identity held");

        let after = frozen(&db, s).await;
        assert!(after.0[0] == before.0[0], "custody must not change");
        assert!(after.0[1] == before.0[1], "registry must not change");
        assert!(after.0[3] == before.0[3], "health must not change");
        assert!(after.0[4] == before.0[4], "references must not change");
        assert!(after.1 == before.1, "progress and repost budget must hold");
        assert_eq!(confirmed(&db, s).await, 3);
        assert_eq!(repost_budget(&db, s).await, 2);
        assert_eq!(health_state(&db, s).await, "verifying");
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);

        // Prior attempt and claim callbacks are stale; the returned handle works.
        assert!(matches!(
            confirm_article(&mut db, &p.old_handle, 2, ArticleSide::Post).await,
            Ok(MutationOutcome::StaleOrDiverged)
        ));
        assert!(!posting_fence_is_current(&mut db, &p.old_handle)
            .await
            .expect("old fence"));
        assert!(matches!(
            recover(&mut db, &p.first_attempt, &s.hash, &s.op).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        assert_eq!(confirmed(&db, s).await, 3, "stale callbacks must not write");
        assert!(posting_fence_is_current(&mut db, &handle)
            .await
            .expect("new fence"));
        applied(
            confirm_article(&mut db, &handle, 2, ArticleSide::Post).await,
            "confirm through the taken-over handle",
        );
        assert_eq!(confirmed(&db, s).await, 4);
        assert_eq!(repost_budget(&db, s).await, 2);
    });
}

#[test]
fn postgres_live_operation_lease_is_busy_and_stale_inputs_do_not_mutate() {
    run_test(async {
        let mut db = connect().await;
        // The operation lease is still live: a newer attempt must wait.
        let p = prepared(&mut db, false).await;
        let s = &p.case.sealed;
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");

        let before = frozen(&db, s).await;
        let live = op(&db, s).await;
        assert!(live.live, "fixture operation lease must be live");
        assert!(
            is_busy(&take_over(&mut db, &p.next_attempt, &expected, &s.op).await),
            "a live old operation must be busy"
        );
        assert!(frozen(&db, s).await == before, "busy must not mutate");
        assert!(op(&db, s).await.fixed == live.fixed);

        expire_operation(&db, s).await;
        let before = frozen(&db, s).await;

        // Wrong operation, unregistered hash, stale observation, wrong job and
        // a superseded attempt are all refused without any change.
        let other_op = format!("{}-x", s.op);
        assert!(
            is_refused(&take_over(&mut db, &p.next_attempt, &expected, &other_op).await),
            "wrong operation must be refused"
        );
        let other_hash = digest(&db, "unregistered", &p.case.uuid).await;
        let unknown = observe(&db, &other_hash)
            .await
            .expect("observe absent hash");
        assert!(
            is_refused(&take_over(&mut db, &p.next_attempt, &unknown, &s.op).await),
            "wrong hash must be refused"
        );
        assert!(
            is_refused(&take_over(&mut db, &p.first_attempt, &expected, &s.op).await),
            "a superseded attempt must be refused"
        );

        let other = case(&db).await;
        let foreign = acquire(&mut db, &other.job, &other.owner, 0).await;
        assert!(
            is_refused(&take_over(&mut db, &foreign, &expected, &s.op).await),
            "another job must be refused"
        );

        // An observation older than the admission is stale.
        let fresh = case(&db).await;
        let pre_admission = observe(&db, &fresh.sealed.hash)
            .await
            .expect("observe absent hash");
        let fresh_attempt = acquire(&mut db, &fresh.job, &fresh.owner, 0).await;
        admitted(&mut db, &fresh_attempt, &fresh.sealed).await;
        expire_attempt(&db, &fresh.job).await;
        expire_operation(&db, &fresh.sealed).await;
        let fresh_next = acquire(&mut db, &fresh.job, &fresh.owner, 1).await;
        let fresh_before = frozen(&db, &fresh.sealed).await;
        assert!(
            is_refused(&take_over(&mut db, &fresh_next, &pre_admission, &fresh.sealed.op).await),
            "a stale observation must be refused"
        );
        assert!(
            frozen(&db, &fresh.sealed).await == fresh_before,
            "stale observation must not mutate"
        );

        assert!(frozen(&db, s).await == before, "refusals must not mutate");
        assert_eq!(health_state(&db, s).await, "verifying");
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);

        // The same inputs with the exact operation do succeed, so the
        // refusals above were caused by the inputs and not by the fixture.
        resumed(
            take_over(&mut db, &p.next_attempt, &expected, &s.op).await,
            "takeover with exact inputs",
        );
        assert_eq!(op(&db, s).await.attempt, 2);
    });
}

#[test]
fn postgres_concurrent_takeover_by_one_new_attempt_is_idempotent() {
    run_test(async {
        let mut db = connect().await;
        let p = prepared(&mut db, true).await;
        let s = &p.case.sealed;
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");
        let before = frozen(&db, s).await;
        let original = op(&db, s).await;

        let mut left = connect().await;
        let mut right = connect().await;
        let (attempt_left, attempt_right) = (p.next_attempt.clone(), p.next_attempt.clone());
        let (expected_left, expected_right) = (expected.clone(), expected);
        let barrier = Arc::new(Barrier::new(2));
        let (gate_left, gate_right) = (barrier.clone(), barrier);
        let (out_left, out_right) = tokio::join!(
            async {
                gate_left.wait().await;
                take_over(&mut left, &attempt_left, &expected_left, &s.op).await
            },
            async {
                gate_right.wait().await;
                take_over(&mut right, &attempt_right, &expected_right, &s.op).await
            }
        );
        let (handle_left, bundle_left) = resumed(out_left, "left takeover");
        let (handle_right, bundle_right) = resumed(out_right, "right takeover");
        assert_eq!(handle_left.generation_id(), handle_right.generation_id());
        assert_eq!(handle_left.operation_id(), s.op);
        assert!(bundle_left.custody().expect("left custody") == s.custody());
        assert!(bundle_right.custody().expect("right custody") == s.custody());

        // One stored claim: both returned handles are the current fence, and
        // the token rotated exactly once away from the original.
        let stored = op(&db, s).await;
        assert!(stored.claim_token != original.claim_token);
        assert_eq!(stored.attempt, 2);
        assert!(posting_fence_is_current(&mut db, &handle_left)
            .await
            .expect("left fence"));
        assert!(posting_fence_is_current(&mut db, &handle_right)
            .await
            .expect("right fence"));
        assert!(!posting_fence_is_current(&mut db, &p.old_handle)
            .await
            .expect("old fence"));

        let after = frozen(&db, s).await;
        assert!(after.0[0] == before.0[0] && after.0[1] == before.0[1]);
        assert!(after.0[3] == before.0[3] && after.0[4] == before.0[4]);
        assert!(after.1 == before.1, "race must not touch progress");
        assert_eq!(repost_budget(&db, s).await, 2);
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
        assert_eq!(health_state(&db, s).await, "verifying");
    });
}

#[test]
fn postgres_lost_ack_replay_and_recover_do_not_rotate_the_claim_again() {
    run_test(async {
        let mut db = connect().await;
        let p = prepared(&mut db, true).await;
        let s = &p.case.sealed;
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");

        // The first result is lost: the caller never sees this handle.
        let lost = take_over(&mut db, &p.next_attempt, &expected, &s.op).await;
        assert!(matches!(
            lost,
            Ok(AdmissionOutcome::ResumedSameCandidate(..))
        ));
        drop(lost);
        let settled = frozen(&db, s).await;
        let stored = op(&db, s).await;

        // Replaying the same takeover and rereading by identity both return
        // the same stored candidate and leave every row, token and counter.
        let (replayed, replay_bundle) = resumed(
            take_over(&mut db, &p.next_attempt, &expected, &s.op).await,
            "replayed takeover",
        );
        assert!(replay_bundle.custody().expect("replay custody") == s.custody());
        assert!(frozen(&db, s).await == settled, "replay must not mutate");
        let (recovered, recover_bundle) = resumed(
            recover(&mut db, &p.next_attempt, &s.hash, &s.op).await,
            "recover after takeover",
        );
        assert!(recover_bundle.custody().expect("recover custody") == s.custody());
        assert!(frozen(&db, s).await == settled, "recover must not mutate");
        let again = op(&db, s).await;
        assert!(again.claim_token == stored.claim_token);
        assert!(again.attempt_token == stored.attempt_token);
        assert!(again.fixed == stored.fixed);

        assert!(posting_fence_is_current(&mut db, &replayed)
            .await
            .expect("replayed fence"));
        assert!(posting_fence_is_current(&mut db, &recovered)
            .await
            .expect("recovered fence"));
        assert!(!posting_fence_is_current(&mut db, &p.old_handle)
            .await
            .expect("old fence"));
        assert!(matches!(
            confirm_article(&mut db, &p.old_handle, 2, ArticleSide::Verify).await,
            Ok(MutationOutcome::StaleOrDiverged)
        ));
        assert!(matches!(
            recover(&mut db, &p.first_attempt, &s.hash, &s.op).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        assert!(
            frozen(&db, s).await == settled,
            "stale callbacks must not mutate"
        );
        assert_eq!(confirmed(&db, s).await, 3);
        assert_eq!(repost_budget(&db, s).await, 2);
        assert_eq!(health_state(&db, s).await, "verifying");
    });
}

#[test]
fn postgres_attempt_expiry_during_registry_lock_wait_refuses_takeover() {
    run_test(async {
        let mut db = connect().await;
        let p = prepared(&mut db, true).await;
        let s = &p.case.sealed;
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");
        let deadline: i64 = db
            .query_one(
                "UPDATE posted_file_job_attempts SET lease_until = \
                 extract(epoch FROM clock_timestamp())::bigint + 5 \
                 WHERE job_id = $1 RETURNING lease_until",
                &[&p.case.job],
            )
            .await
            .quiet("bound takeover attempt lease")
            .get(0);
        let before = frozen(&db, s).await;
        let mut blocker = connect().await;
        let tx = blocker
            .transaction()
            .await
            .quiet("hold registry transaction");
        tx.query_one(
            "SELECT revision FROM posted_file_registry WHERE plaintext_sha256 = $1 FOR UPDATE",
            &[&s.hash],
        )
        .await
        .quiet("hold registry row");
        let mut worker = connect().await;
        let pid: i32 = worker
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .quiet("read worker pid")
            .get(0);
        let (outcome, ()) = tokio::join!(
            take_over(&mut worker, &p.next_attempt, &expected, &s.op),
            async {
                // Observe the actual blocked backend, not a guessed scheduling
                // delay. The outer harness and statement timeouts bound this.
                loop {
                    let blocked: bool = db
                        .query_one("SELECT cardinality(pg_blocking_pids($1)) > 0", &[&pid])
                        .await
                        .quiet("observe registry waiter")
                        .get(0);
                    if blocked {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                db.query_one(
                    "SELECT pg_sleep(GREATEST(0.0, $1::bigint::double precision - \
                     extract(epoch FROM clock_timestamp())::double precision) + 1.0)",
                    &[&deadline],
                )
                .await
                .quiet("wait for DB-clock lease expiry");
                tx.commit().await.quiet("release registry row");
            }
        );
        assert!(is_refused(&outcome), "expired waiter must not take over");
        assert!(
            frozen(&db, s).await == before,
            "expired waiter must not mutate"
        );
        assert_eq!(op(&db, s).await.attempt, 1);
        assert_eq!(repost_budget(&db, s).await, 2);
    });
}

#[test]
fn postgres_expired_new_attempt_and_job_status_or_owner_fence_refuse_takeover() {
    run_test(async {
        let mut db = connect().await;
        let p = prepared(&mut db, true).await;
        let s = &p.case.sealed;
        let c = &p.case;
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");
        let before = frozen(&db, s).await;

        // The new attempt's own lease is expired: it cannot take over.
        expire_attempt(&db, &c.job).await;
        assert!(
            is_refused(&take_over(&mut db, &p.next_attempt, &expected, &s.op).await),
            "an expired new attempt must be refused"
        );
        assert!(
            frozen(&db, s).await == before,
            "expired attempt must not mutate"
        );

        // Persist attempt 3. Attempt 2 stays refused, and the job status and
        // owner fences refuse attempt 3 until restored.
        let third = acquire(&mut db, &c.job, &c.owner, 2).await;
        assert!(
            is_refused(&take_over(&mut db, &p.next_attempt, &expected, &s.op).await),
            "a superseded attempt must be refused"
        );
        let moved_owner = seed_user(&db, &format!("{}-t", c.uuid)).await;
        db.execute(
            "UPDATE ingest_jobs SET user_id = $2 WHERE id = $1",
            &[&c.job, &moved_owner],
        )
        .await
        .quiet("move job owner");
        assert!(
            is_refused(&take_over(&mut db, &third, &expected, &s.op).await),
            "a moved owner must be refused"
        );
        db.execute(
            "UPDATE ingest_jobs SET user_id = $2 WHERE id = $1",
            &[&c.job, &c.owner],
        )
        .await
        .quiet("restore job owner");
        for status in ["queued", "failed", "done"] {
            db.execute(
                "UPDATE ingest_jobs SET status = $2 WHERE id = $1",
                &[&c.job, &status],
            )
            .await
            .quiet("change job status");
            assert!(
                is_refused(&take_over(&mut db, &third, &expected, &s.op).await),
                "a non-running job must be refused"
            );
        }
        assert!(frozen(&db, s).await == before, "fences must not mutate");
        assert_eq!(op(&db, s).await.attempt, 1);

        // Restored, attempt 3 takes over; once its lease lapses, the replay
        // is refused and the stored claim is left as taken over.
        db.execute(
            "UPDATE ingest_jobs SET status = 'running' WHERE id = $1",
            &[&c.job],
        )
        .await
        .quiet("restore job status");
        resumed(
            take_over(&mut db, &third, &expected, &s.op).await,
            "takeover once the fences hold",
        );
        let settled = frozen(&db, s).await;
        assert_eq!(op(&db, s).await.attempt, 3);
        expire_attempt(&db, &c.job).await;
        assert!(
            is_refused(&take_over(&mut db, &third, &expected, &s.op).await),
            "an expired current attempt must be refused on replay"
        );
        assert!(
            is_refused(&recover(&mut db, &third, &s.hash, &s.op).await),
            "an expired current attempt cannot recover"
        );
        assert!(
            frozen(&db, s).await == settled,
            "expired replay must not mutate"
        );
        assert_eq!(repost_budget(&db, s).await, 2);
        assert_eq!(health_state(&db, s).await, "verifying");
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
    });
}
