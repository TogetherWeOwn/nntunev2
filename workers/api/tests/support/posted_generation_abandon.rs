//! Real-PostgreSQL tests for explicit abandonment of a pending candidate
//! (`abandon_candidate`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `case`, `acquire`,
//! `admitted`, `observe`, `snapshot`, `count`, `recover`, `run_test`, `Quiet`).
//! Fixtures are synthetic sealed byte buffers keyed by database UUIDs.
//! Failures print only static labels; outcomes are matched or compared as
//! payload-free enums, so no assertion can echo handles, claim tokens,
//! encrypted material or row data.
//!
//! Scope: explicitly stopping a pending candidate. Abandonment is NOT terminal
//! loss and NOT replacement eligibility: health stays `unknown`, the unique
//! plaintext hash and exact custody remain, and nothing here promotes, proves
//! usable, cleans up or replaces anything.

use super::*;
use nntune_ingest::posted_generations::{
    abandon_candidate, begin_verification_round, confirm_article, finish_verification,
    prepare_retained_check, take_over, AbandonmentOutcome, ArticleSide, GenerationHandle,
    MutationOutcome, RetainedReferenceKind,
};
use std::sync::Arc;
use tokio::sync::Barrier;

const REGISTRY_LOCK: &str =
    "SELECT revision FROM posted_file_registry WHERE plaintext_sha256 = $1 FOR UPDATE";
const CHECKED_AT: i64 = 1_700_000_000;

fn applied(outcome: Result<MutationOutcome, StoreError>, ctx: &'static str) -> GenerationHandle {
    match outcome {
        Ok(MutationOutcome::Applied(handle)) => *handle,
        _ => panic!("{ctx}: expected an applied mutation"),
    }
}

fn is_mutation_stale(outcome: &Result<MutationOutcome, StoreError>) -> bool {
    matches!(outcome, Ok(MutationOutcome::StaleOrDiverged))
}

struct Pending {
    case: Case,
    attempt: AttemptHandle,
    /// Handle of the admitted candidate (round zero) or, when `verifying` was
    /// requested, of the latest round with two confirmed articles.
    handle: GenerationHandle,
}

/// Real admission. With `verifying`, a round begins, two articles are
/// confirmed and fixture-only budget/content rows are seeded so abandonment has
/// historical evidence to preserve.
async fn pending(db: &mut Client, verifying: bool) -> Pending {
    let c = case(db).await;
    let s = &c.sealed;
    let attempt = acquire(db, &c.job, &c.owner, 0).await;
    let mut handle = admitted(db, &attempt, s).await;
    // Explicit fixture-only references exercise preservation and the operation
    // guard, not implemented publication or serving eligibility.
    db.execute(
        "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
         SELECT kind, $1, $2, $3 FROM unnest(ARRAY['legacy_pin', 'serving', 'rollback']) AS kind",
        &[&s.op, &s.generation, &s.hash],
    )
    .await
    .quiet("seed retained-reference controls");
    if verifying {
        handle = applied(begin_verification_round(db, &handle).await, "begin round");
        for (slot, side) in [(0, ArticleSide::Post), (0, ArticleSide::Verify)] {
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
        db.execute(
            "INSERT INTO posted_file_content_checks \
             (generation_id, round, side, backbone, state, checked_articles, checked_at) \
             VALUES ($1, 1, 0, 'pool-a', 'intact', 3, $2)",
            &[&s.generation, &CHECKED_AT],
        )
        .await
        .quiet("seed content check");
    }
    Pending {
        case: c,
        attempt,
        handle,
    }
}

#[derive(PartialEq)]
struct Health {
    state: String,
    failure_kind: Option<String>,
    verified_at: Option<i64>,
    retention_checked_at: Option<i64>,
    revision: i64,
    round: i64,
    token: String,
}

async fn health(db: &Client, s: &Sealed) -> Health {
    let r = db
        .query_one(
            "SELECT state, failure_kind, verified_at, retention_checked_at, revision, round, \
             round_token FROM posted_file_health WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("read health");
    Health {
        state: r.get(0),
        failure_kind: r.get(1),
        verified_at: r.get(2),
        retention_checked_at: r.get(3),
        revision: r.get(4),
        round: r.get(5),
        token: r.get(6),
    }
}

struct Reg {
    selected: Option<String>,
    revision: i64,
    pending: Option<String>,
}

async fn registry(db: &Client, s: &Sealed) -> Reg {
    let r = db
        .query_one(
            "SELECT selected_generation, revision, pending_operation \
             FROM posted_file_registry WHERE plaintext_sha256 = $1",
            &[&s.hash],
        )
        .await
        .quiet("read registry");
    Reg {
        selected: r.get(0),
        revision: r.get(1),
        pending: r.get(2),
    }
}

struct Op {
    state: String,
    /// Identity, claim, attempt, lease and predecessor columns (not state).
    fixed: String,
}

async fn op(db: &Client, s: &Sealed) -> Op {
    let r = db
        .query_one(
            "SELECT state, md5(row(id, plaintext_sha256, generation_id, job_id, attempt, \
             attempt_token, claim_token, lease_until, predecessor, \
             predecessor_health_revision, registry_revision)::text) \
             FROM posted_file_operations WHERE id = $1",
            &[&s.op],
        )
        .await
        .quiet("read operation row");
    Op {
        state: r.get(0),
        fixed: r.get(1),
    }
}

/// Digests of progress/budgets, content checks and recheck schedule.
async fn evidence(db: &Client, s: &Sealed) -> Vec<Option<String>> {
    let row = db
        .query_one(
            "SELECT (SELECT md5(string_agg(t::text, ',' ORDER BY t.round, t.slot, t.side)) \
                FROM posted_file_article_progress t WHERE t.generation_id = $1), \
             (SELECT md5(string_agg(t::text, ',' ORDER BY t.round, t.side)) \
                FROM posted_file_content_checks t WHERE t.generation_id = $1), \
             (SELECT md5(string_agg(t::text, ',' ORDER BY t.round)) \
                FROM posted_file_recheck_schedule t WHERE t.generation_id = $1)",
            &[&s.generation],
        )
        .await
        .quiet("digest evidence");
    (0..3).map(|i| row.get(i)).collect()
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

async fn in_flight_refs(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_references \
         WHERE generation_id = $1 AND kind = 'in_flight' AND owner_id = $2",
        &[&s.generation, &s.op],
    )
    .await
}

/// Every table a refused or replayed abandonment must leave untouched.
type Frozen = (Vec<Option<String>>, Vec<Option<String>>, String);

async fn frozen(db: &Client, s: &Sealed) -> Frozen {
    (
        snapshot(db, s).await,
        evidence(db, s).await,
        op(db, s).await.fixed,
    )
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

/// Abandonment is never a terminal loss, promotion, usable proof or cleanup.
async fn assert_not_terminal_or_promoted(db: &Client, s: &Sealed) {
    let h = health(db, s).await;
    assert!(
        matches!(h.state.as_str(), "unknown" | "verifying"),
        "no terminal or usable health authority"
    );
    assert!(h.failure_kind.is_none(), "abandonment is not a failure");
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM posted_file_health \
             WHERE generation_id = $1 AND state IN ('terminal_failed', 'usable')",
            &[&s.generation]
        )
        .await,
        0
    );
    let o = op(db, s).await;
    assert!(o.state != "promoted", "abandonment must not promote");
    let r = registry(db, s).await;
    assert!(r.selected.is_none(), "abandonment must not select");
    assert_eq!(custody_rows(db, s).await, 1, "custody is never cleaned up");
    assert_eq!(operation_rows(db, s).await, 1);
}

#[test]
fn postgres_abandon_applies_atomically_preserves_custody_and_evidence_and_replays() {
    run_test(async {
        let mut db = connect().await;
        let p = pending(&mut db, true).await;
        let s = &p.case.sealed;

        let before = frozen(&db, s).await;
        let before_health = health(&db, s).await;
        let before_reg = registry(&db, s).await;
        let before_op = op(&db, s).await;
        let budget = repost_budget(&db, s).await;
        assert_eq!(budget, 2);
        assert!(before_op.state == "pending");
        assert!(before_health.state == "verifying");
        assert_eq!(before_health.round, 1);
        assert!(before_reg.pending.as_deref() == Some(s.op.as_str()));
        assert_eq!(in_flight_refs(&db, s).await, 1);

        assert_eq!(
            abandon_candidate(&mut db, &p.handle).await,
            Ok(AbandonmentOutcome::Applied)
        );

        // Operation: only the state moved; identity, claim and lease held.
        let after_op = op(&db, s).await;
        assert!(after_op.state == "abandoned");
        assert!(after_op.fixed == before_op.fixed, "operation identity held");
        // Registry: pending cleared, revision +1, selection unchanged.
        let after_reg = registry(&db, s).await;
        assert!(after_reg.pending.is_none());
        assert_eq!(after_reg.revision, before_reg.revision + 1);
        assert!(after_reg.selected == before_reg.selected);
        assert!(after_reg.selected.is_none());
        // Health: unknown, no evidence timestamps, new revision/round/token.
        let after_health = health(&db, s).await;
        assert!(after_health.state == "unknown");
        assert!(after_health.failure_kind.is_none());
        assert!(after_health.verified_at.is_none());
        assert!(after_health.retention_checked_at.is_none());
        assert_eq!(after_health.revision, before_health.revision + 1);
        assert_eq!(after_health.round, before_health.round + 1);
        assert!(after_health.token != before_health.token);

        // Custody, every reference (including in-flight), progress, repost
        // budget, content checks and schedule are preserved exactly.
        let after = frozen(&db, s).await;
        assert!(after.0[0] == before.0[0], "custody must not change");
        assert!(after.0[4] == before.0[4], "references must not change");
        assert_eq!(in_flight_refs(&db, s).await, 1, "in-flight reference kept");
        assert!(after.1 == before.1, "historical evidence must not change");
        assert!(after.2 == before.2);
        assert_eq!(repost_budget(&db, s).await, budget);
        assert_not_terminal_or_promoted(&db, s).await;

        // The original handle replays as already abandoned, with no write.
        let settled = frozen(&db, s).await;
        let settled_health = health(&db, s).await;
        assert_eq!(
            abandon_candidate(&mut db, &p.handle).await,
            Ok(AbandonmentOutcome::AlreadyAbandoned)
        );
        assert!(frozen(&db, s).await == settled, "replay must not mutate");
        assert!(health(&db, s).await == settled_health);
        assert_eq!(registry(&db, s).await.revision, after_reg.revision);

        // The old fence confers no authority after abandonment.
        assert!(!posting_fence_is_current(&mut db, &p.handle)
            .await
            .expect("old fence"));
        assert!(is_mutation_stale(
            &confirm_article(&mut db, &p.handle, 1, ArticleSide::Post).await
        ));
        assert!(frozen(&db, s).await == settled);

        // An expired attempt refuses even the replay.
        expire_attempt(&db, &p.case.job).await;
        assert_eq!(
            abandon_candidate(&mut db, &p.handle).await,
            Ok(AbandonmentOutcome::StaleOrDiverged)
        );
        assert!(frozen(&db, s).await == settled);
        assert!(health(&db, s).await == settled_health);
    });
}

#[test]
fn postgres_abandon_round_zero_candidate_and_replay_survives_registry_drift() {
    run_test(async {
        let mut db = connect().await;
        let p = pending(&mut db, false).await;
        let s = &p.case.sealed;
        let before = frozen(&db, s).await;
        let before_health = health(&db, s).await;
        let before_reg = registry(&db, s).await;
        assert_eq!(before_health.round, 0);
        assert_eq!(before_health.revision, 0);

        assert_eq!(
            abandon_candidate(&mut db, &p.handle).await,
            Ok(AbandonmentOutcome::Applied)
        );
        let after_health = health(&db, s).await;
        assert!(after_health.state == "unknown");
        assert_eq!(after_health.round, 1);
        assert_eq!(after_health.revision, 1);
        assert!(after_health.token != before_health.token);
        assert!(after_health.failure_kind.is_none());
        assert!(after_health.verified_at.is_none());
        assert!(after_health.retention_checked_at.is_none());
        assert!(op(&db, s).await.state == "abandoned");
        let after_reg = registry(&db, s).await;
        assert!(after_reg.pending.is_none());
        assert_eq!(after_reg.revision, before_reg.revision + 1);
        let after = frozen(&db, s).await;
        assert!(after.0[0] == before.0[0] && after.0[4] == before.0[4]);
        assert!(after.1 == before.1 && after.2 == before.2);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_article_progress WHERE generation_id = $1",
                &[&s.generation]
            )
            .await,
            0
        );
        assert_not_terminal_or_promoted(&db, s).await;

        // Independent registry drift after abandonment: the replay still
        // reports it, but confers no effect or authority.
        db.execute(
            "UPDATE posted_file_registry SET revision = revision + 1 \
             WHERE plaintext_sha256 = $1",
            &[&s.hash],
        )
        .await
        .quiet("drift registry revision");
        let settled = frozen(&db, s).await;
        let settled_health = health(&db, s).await;
        let drifted = registry(&db, s).await;
        assert_eq!(
            abandon_candidate(&mut db, &p.handle).await,
            Ok(AbandonmentOutcome::AlreadyAbandoned)
        );
        assert!(
            frozen(&db, s).await == settled,
            "drift replay must not write"
        );
        assert!(health(&db, s).await == settled_health);
        assert_eq!(registry(&db, s).await.revision, drifted.revision);
        assert!(registry(&db, s).await.pending.is_none());
        assert!(!posting_fence_is_current(&mut db, &p.handle)
            .await
            .expect("old fence"));
    });
}

#[test]
fn postgres_abandon_with_stale_health_revision_refuses_all_writes() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let admitted_handle = admitted(&mut db, &attempt, s).await;
        let stale = applied(
            begin_verification_round(&mut db, &admitted_handle).await,
            "begin round",
        );
        // A later evidence write advances the health revision only.
        let current = applied(
            confirm_article(&mut db, &stale, 0, ArticleSide::Post).await,
            "advance health revision",
        );
        let before = frozen(&db, s).await;
        let before_health = health(&db, s).await;
        let before_reg = registry(&db, s).await;

        assert_eq!(
            abandon_candidate(&mut db, &stale).await,
            Ok(AbandonmentOutcome::StaleOrDiverged)
        );
        assert!(
            frozen(&db, s).await == before,
            "stale revision must not write"
        );
        assert!(health(&db, s).await == before_health);
        assert_eq!(registry(&db, s).await.revision, before_reg.revision);
        assert!(registry(&db, s).await.pending.as_deref() == Some(s.op.as_str()));
        assert!(op(&db, s).await.state == "pending");
        assert_not_terminal_or_promoted(&db, s).await;

        // Direct health drift beyond the handle is refused the same way.
        db.execute(
            "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("drift health revision");
        let drifted = frozen(&db, s).await;
        let drifted_health = health(&db, s).await;
        assert_eq!(
            abandon_candidate(&mut db, &current).await,
            Ok(AbandonmentOutcome::StaleOrDiverged)
        );
        assert!(frozen(&db, s).await == drifted);
        assert!(health(&db, s).await == drifted_health);
        assert!(op(&db, s).await.state == "pending");
    });
}

#[test]
fn postgres_abandon_with_old_round_refuses_and_changes_nothing() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let round_zero = admitted(&mut db, &attempt, s).await;
        let round_one = applied(
            begin_verification_round(&mut db, &round_zero).await,
            "begin round",
        );
        let before = frozen(&db, s).await;
        let before_health = health(&db, s).await;

        assert_eq!(
            abandon_candidate(&mut db, &round_zero).await,
            Ok(AbandonmentOutcome::StaleOrDiverged)
        );
        assert!(frozen(&db, s).await == before, "old round must not write");
        assert!(health(&db, s).await == before_health);
        assert!(op(&db, s).await.state == "pending");

        // A round and token moved independently of the revision also refuses.
        db.execute(
            "UPDATE posted_file_health SET round = round + 1, \
             round_token = gen_random_uuid()::text WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("drift round and token");
        let drifted = frozen(&db, s).await;
        let drifted_health = health(&db, s).await;
        assert_eq!(
            abandon_candidate(&mut db, &round_one).await,
            Ok(AbandonmentOutcome::StaleOrDiverged)
        );
        assert!(frozen(&db, s).await == drifted);
        assert!(health(&db, s).await == drifted_health);
        assert!(registry(&db, s).await.pending.as_deref() == Some(s.op.as_str()));
        assert!(op(&db, s).await.state == "pending");
        assert_not_terminal_or_promoted(&db, s).await;
    });
}

#[test]
fn postgres_abandon_after_newer_attempt_or_takeover_rejects_old_claim_and_current_claim_works() {
    run_test(async {
        let mut db = connect().await;
        let p = pending(&mut db, true).await;
        let s = &p.case.sealed;
        let c = &p.case;
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");

        // Attempt 2 is persisted, but nothing has taken the candidate over.
        expire_attempt(&db, &c.job).await;
        expire_operation(&db, s).await;
        let next = acquire(&mut db, &c.job, &c.owner, 1).await;
        let before = frozen(&db, s).await;
        let before_health = health(&db, s).await;
        assert_eq!(
            abandon_candidate(&mut db, &p.handle).await,
            Ok(AbandonmentOutcome::StaleOrDiverged)
        );
        assert!(
            frozen(&db, s).await == before,
            "superseded attempt must not write"
        );
        assert!(health(&db, s).await == before_health);
        assert!(op(&db, s).await.state == "pending");

        // After takeover the old claim stays refused; the new claim abandons.
        let current = match take_over(&mut db, &next, &expected, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(handle, _)) => handle,
            _ => panic!("takeover did not resume the candidate"),
        };
        let taken = frozen(&db, s).await;
        let taken_health = health(&db, s).await;
        assert_eq!(
            abandon_candidate(&mut db, &p.handle).await,
            Ok(AbandonmentOutcome::StaleOrDiverged)
        );
        assert!(frozen(&db, s).await == taken, "old claim must not write");
        assert!(health(&db, s).await == taken_health);
        assert!(op(&db, s).await.state == "pending");

        assert_eq!(
            abandon_candidate(&mut db, &current).await,
            Ok(AbandonmentOutcome::Applied)
        );
        let after = frozen(&db, s).await;
        assert!(after.0[0] == taken.0[0] && after.0[4] == taken.0[4]);
        assert!(after.1 == taken.1);
        assert!(after.2 == taken.2, "takeover claim identity preserved");
        assert!(op(&db, s).await.state == "abandoned");
        let after_health = health(&db, s).await;
        assert!(after_health.state == "unknown");
        assert_eq!(after_health.revision, taken_health.revision + 1);
        assert_eq!(after_health.round, taken_health.round + 1);
        assert_eq!(repost_budget(&db, s).await, 2);

        // The old claim never gains authority; the current one replays.
        assert_eq!(
            abandon_candidate(&mut db, &p.handle).await,
            Ok(AbandonmentOutcome::StaleOrDiverged)
        );
        assert_eq!(
            abandon_candidate(&mut db, &current).await,
            Ok(AbandonmentOutcome::AlreadyAbandoned)
        );
        assert_not_terminal_or_promoted(&db, s).await;
    });
}

#[test]
fn postgres_abandon_attempt_expiry_during_registry_lock_wait_refuses() {
    run_test(async {
        let mut db = connect().await;
        let p = pending(&mut db, true).await;
        let s = &p.case.sealed;
        // Bound both leases to a near DB-clock deadline so the post-lock DB
        // clock recheck is the only thing that can refuse.
        let deadline: i64 = db
            .query_one(
                "UPDATE posted_file_job_attempts SET lease_until = \
                 extract(epoch FROM clock_timestamp())::bigint + 5 \
                 WHERE job_id = $1 RETURNING lease_until",
                &[&p.case.job],
            )
            .await
            .quiet("bound attempt lease")
            .get(0);
        db.execute(
            "UPDATE posted_file_operations SET lease_until = $2 WHERE id = $1",
            &[&s.op, &deadline],
        )
        .await
        .quiet("bound operation lease");
        let before = frozen(&db, s).await;
        let before_health = health(&db, s).await;

        let mut blocker = connect().await;
        let tx = blocker
            .transaction()
            .await
            .quiet("hold registry transaction");
        tx.query_one(REGISTRY_LOCK, &[&s.hash])
            .await
            .quiet("hold registry row");
        let mut worker = connect().await;
        let pid: i32 = worker
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .quiet("read worker pid")
            .get(0);
        let (outcome, ()) = tokio::join!(abandon_candidate(&mut worker, &p.handle), async {
            // Observe the actual blocked backend, not a guessed delay. The
            // outer harness and statement timeouts bound this loop.
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
        });
        assert_eq!(outcome, Ok(AbandonmentOutcome::StaleOrDiverged));
        assert!(
            frozen(&db, s).await == before,
            "expired waiter must not write"
        );
        assert!(health(&db, s).await == before_health);
        assert!(op(&db, s).await.state == "pending");
        assert!(registry(&db, s).await.pending.as_deref() == Some(s.op.as_str()));
        assert_eq!(repost_budget(&db, s).await, 2);
    });
}

#[test]
fn postgres_abandon_concurrent_original_handle_applies_once_and_replays_once() {
    run_test(async {
        let mut db = connect().await;
        let p = pending(&mut db, true).await;
        let s = &p.case.sealed;
        let before = frozen(&db, s).await;
        let before_health = health(&db, s).await;
        let before_reg = registry(&db, s).await;

        let mut left = connect().await;
        let mut right = connect().await;
        let (handle_left, handle_right) = (p.handle.clone(), p.handle.clone());
        let barrier = Arc::new(Barrier::new(2));
        let (gate_left, gate_right) = (barrier.clone(), barrier);
        let (out_left, out_right) = tokio::join!(
            async {
                gate_left.wait().await;
                abandon_candidate(&mut left, &handle_left).await
            },
            async {
                gate_right.wait().await;
                abandon_candidate(&mut right, &handle_right).await
            }
        );
        let outcomes = [out_left, out_right];
        let applied_count = outcomes
            .iter()
            .filter(|o| **o == Ok(AbandonmentOutcome::Applied))
            .count();
        let replayed_count = outcomes
            .iter()
            .filter(|o| **o == Ok(AbandonmentOutcome::AlreadyAbandoned))
            .count();
        assert_eq!(applied_count, 1, "exactly one racer applies");
        assert_eq!(replayed_count, 1, "the other racer replays");

        // One transition: each counter moved exactly once.
        let after_health = health(&db, s).await;
        assert_eq!(after_health.revision, before_health.revision + 1);
        assert_eq!(after_health.round, before_health.round + 1);
        assert!(after_health.state == "unknown");
        assert_eq!(registry(&db, s).await.revision, before_reg.revision + 1);
        assert!(op(&db, s).await.state == "abandoned");
        let after = frozen(&db, s).await;
        assert!(after.0[0] == before.0[0] && after.0[4] == before.0[4]);
        assert!(after.1 == before.1 && after.2 == before.2);
        assert_eq!(repost_budget(&db, s).await, 2);
        assert_not_terminal_or_promoted(&db, s).await;
    });
}

#[test]
fn postgres_abandoned_candidate_loses_authority_and_unique_hash_blocks_replacement() {
    run_test(async {
        let mut db = connect().await;
        let p = pending(&mut db, true).await;
        let s = &p.case.sealed;
        let c = &p.case;
        assert_eq!(
            abandon_candidate(&mut db, &p.handle).await,
            Ok(AbandonmentOutcome::Applied)
        );
        let settled = frozen(&db, s).await;
        let settled_health = health(&db, s).await;
        let settled_reg = registry(&db, s).await;

        // Posting fence, recovery and every candidate mutation are refused.
        assert!(!posting_fence_is_current(&mut db, &p.handle)
            .await
            .expect("posting fence"));
        assert!(matches!(
            recover(&mut db, &p.attempt, &s.hash, &s.op).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        assert!(is_mutation_stale(
            &confirm_article(&mut db, &p.handle, 1, ArticleSide::Post).await
        ));
        assert!(is_mutation_stale(
            &begin_verification_round(&mut db, &p.handle).await
        ));
        assert!(is_mutation_stale(
            &finish_verification(&mut db, &p.handle).await
        ));
        for kind in [
            RetainedReferenceKind::LegacyPin,
            RetainedReferenceKind::Serving,
            RetainedReferenceKind::Rollback,
        ] {
            assert!(
                matches!(
                    prepare_retained_check(&mut db, &p.attempt, &s.generation, kind, &s.op).await,
                    Ok(None)
                ),
                "an abandoned candidate is not retained"
            );
        }
        assert!(frozen(&db, s).await == settled, "refusals must not write");
        assert!(health(&db, s).await == settled_health);

        // The unique plaintext hash remains: a new admission for the same
        // content cannot replace or duplicate the abandoned custody.
        let mut other = Sealed::new(&c.uuid, &s.hash, &s.plan, 0x40);
        other.op = format!("{}-n", s.op);
        other.generation = format!("{}-n", s.generation);
        other.plan = digest(&db, "replacement-plan", &c.uuid).await;
        let attempted = try_admit(&mut db, &p.attempt, &other).await;
        assert!(
            matches!(attempted, Ok(AdmissionOutcome::InconsistentOrMissing)),
            "abandonment is not replacement eligibility"
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_files WHERE plaintext_sha256 = $1",
                &[&s.hash]
            )
            .await,
            1,
            "the unique hash keeps exactly the abandoned custody"
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_files WHERE id = $1",
                &[&other.generation]
            )
            .await,
            0
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_operations WHERE id = $1 OR generation_id = $2",
                &[&other.op, &other.generation]
            )
            .await,
            0
        );
        assert!(
            frozen(&db, s).await == settled,
            "failed admission must not write"
        );
        assert!(health(&db, s).await == settled_health);
        let after_reg = registry(&db, s).await;
        assert_eq!(after_reg.revision, settled_reg.revision);
        assert!(after_reg.pending.is_none() && after_reg.selected.is_none());
        assert_not_terminal_or_promoted(&db, s).await;
    });
}
