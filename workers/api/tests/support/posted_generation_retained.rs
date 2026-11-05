//! Real-PostgreSQL tests for negative-only health checks of an already retained
//! generation (`prepare_retained_check`, `invalidate_retained_check`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `case`, `acquire`,
//! `admitted`, `snapshot`, `count`, `run_test`, `Quiet`). Fixtures are
//! synthetic sealed byte buffers keyed by database UUIDs.
//!
//! Scope: there is no promotion API in this adapter and none is claimed or
//! exercised here. A retained (promoted, referenced, optionally usable) state
//! is seeded ONLY by explicit fixture SQL after a real admission. These tests
//! therefore show the retained-check fences and the negative-only write; they
//! do not show that any promotion path produces such a state. Time is never
//! slept except one bounded DB-clock wait for the lease-expiry recheck.
//! Failures print only static labels; outcomes are matched, never formatted,
//! so no assertion can echo handles, permits, tokens, rows or custody.

use super::*;
use nntune_ingest::posted_generations::{
    invalidate_retained_check, prepare_retained_check, prepare_retained_content_check, ArticleSide,
    RetainedCheckPermit, RetainedCustody, RetainedInvalidation, RetainedReferenceKind,
};

#[path = "posted_generation_retained_content.rs"]
mod content;

type Prepared = Option<(RetainedCheckPermit, RetainedCustody)>;

const REGISTRY_LOCK: &str =
    "SELECT revision FROM posted_file_registry WHERE plaintext_sha256 = $1 FOR UPDATE";
const HEALTH_LOCK: &str =
    "SELECT revision FROM posted_file_health WHERE generation_id = $1 FOR UPDATE";
const VERIFIED_AT: i64 = 1_700_000_000;

/// An admitted generation turned into a retained one by explicit fixture SQL.
struct Fixture {
    case: Case,
    attempt: AttemptHandle,
    /// Owner id of the serving reference.
    owner: String,
}

/// Real admission, then fixture-only SQL: operation promoted, registry
/// selecting the generation, in-flight reference swapped for a serving one and,
/// when `usable`, a usable health row with evidence timestamps and round 1.
async fn retained(db: &mut Client, usable: bool) -> Fixture {
    let c = case(db).await;
    let attempt = acquire(db, &c.job, &c.owner, 0).await;
    admitted(db, &attempt, &c.sealed).await;
    let s = &c.sealed;
    let owner = format!("pg-serving-{}", c.uuid);
    db.execute(
        "UPDATE posted_file_operations SET state = 'promoted' WHERE id = $1",
        &[&s.op],
    )
    .await
    .quiet("fixture promote operation");
    db.execute(
        "UPDATE posted_file_registry SET selected_generation = $2, pending_operation = NULL, \
         revision = revision + 1 WHERE plaintext_sha256 = $1",
        &[&s.hash, &s.generation],
    )
    .await
    .quiet("fixture select generation");
    db.execute(
        "DELETE FROM posted_file_references WHERE generation_id = $1 AND kind = 'in_flight'",
        &[&s.generation],
    )
    .await
    .quiet("fixture drop in-flight reference");
    add_reference(db, s, "serving", &owner).await;
    if usable {
        db.execute(
            "UPDATE posted_file_health SET state = 'usable', round = 1, revision = revision + 1, \
             verified_at = $2, retention_checked_at = $2 WHERE generation_id = $1",
            &[&s.generation, &VERIFIED_AT],
        )
        .await
        .quiet("fixture usable health");
    }
    Fixture {
        case: c,
        attempt,
        owner,
    }
}

async fn add_reference(db: &Client, s: &Sealed, kind: &str, owner: &str) {
    db.execute(
        "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
         VALUES ($1, $2, $3, $4)",
        &[&kind, &owner, &s.generation, &s.hash],
    )
    .await
    .quiet("fixture reference");
}

/// Fixture-only evidence and budget rows that retained invalidation must keep.
async fn seed_evidence(db: &Client, s: &Sealed) {
    db.execute(
        "INSERT INTO posted_file_article_progress \
         (generation_id, round, slot, side, confirmed, repost_attempts) \
         VALUES ($1, 1, 0, 0, TRUE, 2), ($1, 1, 0, 1, TRUE, 1)",
        &[&s.generation],
    )
    .await
    .quiet("fixture progress");
    db.execute(
        "INSERT INTO posted_file_content_checks \
         (generation_id, round, side, backbone, state, checked_articles, checked_at) \
         VALUES ($1, 1, 0, 'pool-a', 'intact', 3, $2)",
        &[&s.generation, &VERIFIED_AT],
    )
    .await
    .quiet("fixture content check");
    db.execute(
        "INSERT INTO posted_file_recheck_schedule \
         (generation_id, round, completed_passes, state, not_before) \
         VALUES ($1, 1, 2, 'waiting', $2)",
        &[&s.generation, &(VERIFIED_AT + 1000)],
    )
    .await
    .quiet("fixture schedule");
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

/// Everything a retained check must not touch unless a test says otherwise.
async fn frozen(db: &Client, s: &Sealed) -> (Vec<Option<String>>, Vec<Option<String>>) {
    (snapshot(db, s).await, evidence(db, s).await)
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

async fn health(db: &Client, generation: &str) -> Health {
    let r = db
        .query_one(
            "SELECT state, failure_kind, verified_at, retention_checked_at, revision, round, \
             round_token FROM posted_file_health WHERE generation_id = $1",
            &[&generation],
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

async fn prep(
    db: &mut Client,
    f: &Fixture,
    kind: RetainedReferenceKind,
    owner: &str,
) -> Result<Prepared, StoreError> {
    prepare_retained_check(db, &f.attempt, &f.case.sealed.generation, kind, owner).await
}

async fn serving(db: &mut Client, f: &Fixture, ctx: &'static str) -> RetainedCheckPermit {
    let owner = f.owner.clone();
    minted(
        prep(db, f, RetainedReferenceKind::Serving, &owner).await,
        ctx,
    )
    .0
}

fn minted(
    outcome: Result<Prepared, StoreError>,
    ctx: &'static str,
) -> (RetainedCheckPermit, RetainedCustody) {
    match outcome {
        Ok(Some(pair)) => pair,
        _ => panic!("{ctx}: expected a retained permit"),
    }
}

fn refused(outcome: &Result<Prepared, StoreError>) -> bool {
    matches!(outcome, Ok(None))
}

fn is_stale(outcome: &Result<RetainedInvalidation, StoreError>) -> bool {
    matches!(outcome, Ok(RetainedInvalidation::StaleOrMissing))
}

fn is_applied(outcome: &Result<RetainedInvalidation, StoreError>) -> bool {
    matches!(outcome, Ok(RetainedInvalidation::Applied))
}

/// Hold `lock_sql` on an independent connection, wait until `call` is observed
/// blocked behind it, then sleep on the DATABASE clock past `deadline` and
/// release. `call` must already be bound to the backend `pid`.
async fn expire_during_lock_wait<T>(
    db: &Client,
    lock_sql: &str,
    key: &str,
    pid: i32,
    deadline: i64,
    call: impl Future<Output = T>,
) -> T {
    let mut blocker = connect().await;
    let tx = blocker.transaction().await.quiet("hold lock transaction");
    tx.query_one(lock_sql, &[&key])
        .await
        .quiet("hold contended row");
    let (outcome, ()) = tokio::join!(call, async {
        loop {
            let blocked: bool = db
                .query_one("SELECT cardinality(pg_blocking_pids($1)) > 0", &[&pid])
                .await
                .quiet("observe blocked callback")
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
        tx.commit().await.quiet("release contended row");
    });
    outcome
}

/// The reference incarnation token. Callers only compare it, never print it.
async fn reference_token(db: &Client, s: &Sealed, kind: &str, owner: &str) -> String {
    db.query_one(
        "SELECT reference_token FROM posted_file_references \
         WHERE generation_id = $1 AND kind = $2 AND owner_id = $3",
        &[&s.generation, &kind, &owner],
    )
    .await
    .quiet("read reference token")
    .get(0)
}

/// Same-key delete then reinsert of the serving reference (an ABA).
async fn reinsert_reference(db: &Client, s: &Sealed, owner: &str) {
    db.execute(
        "DELETE FROM posted_file_references WHERE generation_id = $1 \
         AND kind = 'serving' AND owner_id = $2",
        &[&s.generation, &owner],
    )
    .await
    .quiet("fixture delete reference for reinsert");
    add_reference(db, s, "serving", owner).await;
}

/// Require the reference trigger, not an unrelated FK/unique/transport error.
fn db_rejected<T>(result: Result<T, tokio_postgres::Error>) -> bool {
    matches!(result, Err(ref e) if e.as_db_error().is_some_and(|e|
        e.code() == &SqlState::RAISE_EXCEPTION
            && e.message() == "posted generation reference is immutable"))
}

/// An independent session deletes and reinserts the serving reference in one
/// open transaction. `call` (bound to backend `pid`) must be observed blocked
/// by exactly that session before it commits, so the callback is waiting on the
/// reference row and is released only after the new incarnation exists.
async fn reinsert_during_reference_wait<T>(
    db: &Client,
    s: &Sealed,
    owner: &str,
    pid: i32,
    call: impl Future<Output = T>,
) -> T {
    let mut blocker = connect().await;
    let tx = blocker
        .transaction()
        .await
        .quiet("open reinsert transaction");
    let blocker_pid: i32 = tx
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .quiet("read blocker pid")
        .get(0);
    tx.execute(
        "DELETE FROM posted_file_references WHERE generation_id = $1 \
         AND kind = 'serving' AND owner_id = $2",
        &[&s.generation, &owner],
    )
    .await
    .quiet("delete reference in held transaction");
    tx.execute(
        "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
         VALUES ('serving', $1, $2, $3)",
        &[&owner, &s.generation, &s.hash],
    )
    .await
    .quiet("reinsert reference in held transaction");
    let (outcome, ()) = tokio::join!(call, async {
        loop {
            let blocked: bool = db
                .query_one(
                    "SELECT $1::int4 = ANY(pg_blocking_pids($2::int4))",
                    &[&blocker_pid, &pid],
                )
                .await
                .quiet("observe callback blocked on reference")
                .get(0);
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
        tx.commit().await.quiet("commit reference reinsert");
    });
    outcome
}

async fn backend_pid(client: &Client) -> i32 {
    client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .quiet("read worker pid")
        .get(0)
}

#[test]
fn postgres_retained_prepare_binds_exact_custody_is_redacted_and_dropped_permit_writes_nothing() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let rollback = format!("pg-rollback-{}", f.case.uuid);
        add_reference(&db, &f.case.sealed, "rollback", &rollback).await;
        seed_evidence(&db, &f.case.sealed).await;
        let s = f.case.sealed.clone();
        let before = frozen(&db, &s).await;
        let health_before = health(&db, &s.generation).await;

        let owner = f.owner.clone();
        for (kind, owner) in [
            (RetainedReferenceKind::Serving, owner.as_str()),
            (RetainedReferenceKind::Rollback, rollback.as_str()),
        ] {
            let (permit, custody) = minted(prep(&mut db, &f, kind, owner).await, "prepare");
            assert!(
                custody.custody().expect("retained custody") == s.custody(),
                "custody must be the exact stored bundle"
            );
            assert_eq!(format!("{permit:?}"), "RetainedCheckPermit([REDACTED])");
            assert_eq!(format!("{custody:?}"), "RetainedCustody([REDACTED])");
            assert_eq!(
                format!("{:?}", permit.clone()),
                "RetainedCheckPermit([REDACTED])"
            );
            drop(permit);
            drop(custody);
        }
        assert!(
            before == frozen(&db, &s).await,
            "read-only prepare and a dropped permit must write nothing"
        );
        assert!(health_before == health(&db, &s.generation).await);

        // A dropped permit leaves nothing behind: a fresh prepare still works.
        let _again = serving(&mut db, &f, "prepare after dropped permit").await;
        assert!(before == frozen(&db, &s).await);
    });
}

#[test]
fn postgres_retained_nonaffirmative_invalidation_only_withdraws_health_and_fences_old_observations()
{
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = &f.case.sealed;
        seed_evidence(&db, s).await;
        // Global selection may move on while this exact serving/rollback
        // reference still owns its generation. Invalidation must not reselect it.
        db.execute(
            "UPDATE posted_file_registry SET selected_generation = NULL, revision = revision + 1 \
             WHERE plaintext_sha256 = $1",
            &[&s.hash],
        )
        .await
        .quiet("fixture deselect retained generation");
        let permit = serving(&mut db, &f, "prepare").await;
        let before = frozen(&db, s).await;
        let h0 = health(&db, &s.generation).await;
        assert_eq!(h0.state, "usable");
        assert_eq!(h0.verified_at, Some(VERIFIED_AT));

        let outcome = invalidate_retained_check(&mut db, &permit).await;
        assert!(is_applied(&outcome), "the original permit applies once");

        let h1 = health(&db, &s.generation).await;
        assert_eq!(h1.state, "unknown");
        assert_eq!(h1.failure_kind, None);
        assert_eq!(h1.verified_at, None);
        assert_eq!(h1.retention_checked_at, None);
        assert_eq!(h1.revision, h0.revision + 1);
        assert_eq!(h1.round, h0.round + 1);
        assert!(h1.token != h0.token, "the round token must rotate");

        let after = frozen(&db, s).await;
        // snapshot order: custody, registry, operation, health, references.
        for index in [0, 1, 2, 4] {
            assert!(
                before.0[index] == after.0[index],
                "custody, registry, operation and references are unchanged"
            );
        }
        assert!(before.0[3] != after.0[3], "only the health row changes");
        assert!(
            before.1 == after.1,
            "evidence, content checks, schedule and repost budgets are unchanged"
        );
        assert_eq!(
            count(
                &db,
                "SELECT COALESCE(sum(repost_attempts), 0)::bigint \
                 FROM posted_file_article_progress WHERE generation_id = $1",
                &[&s.generation]
            )
            .await,
            3
        );

        // Replay of the applied permit is stale and mutates nothing.
        assert!(is_stale(&invalidate_retained_check(&mut db, &permit).await));
        assert!(after == frozen(&db, s).await);
        assert!(h1 == health(&db, &s.generation).await);

        // The reference still owns the custody: a fresh permit is current and
        // the old one stays dead after the new one applies.
        let fresh = serving(&mut db, &f, "prepare after invalidation").await;
        assert!(is_applied(
            &invalidate_retained_check(&mut db, &fresh).await
        ));
        assert!(is_stale(&invalidate_retained_check(&mut db, &permit).await));
        assert_eq!(health(&db, &s.generation).await.revision, h1.revision + 1);
    });
}

#[test]
fn postgres_retained_pending_candidates_and_non_retainable_health_are_refused() {
    run_test(async {
        let mut db = connect().await;

        // A pending candidate is refused even with fake serving/rollback refs.
        let c = case(&db).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        admitted(&mut db, &attempt, &c.sealed).await;
        let s = &c.sealed;
        let serving_owner = format!("pg-serving-{}", c.uuid);
        let rollback_owner = format!("pg-rollback-{}", c.uuid);
        add_reference(&db, s, "serving", &serving_owner).await;
        add_reference(&db, s, "rollback", &rollback_owner).await;
        let before = frozen(&db, s).await;
        for (kind, owner) in [
            (RetainedReferenceKind::Serving, serving_owner.as_str()),
            (RetainedReferenceKind::Rollback, rollback_owner.as_str()),
        ] {
            let outcome =
                prepare_retained_check(&mut db, &attempt, &s.generation, kind, owner).await;
            assert!(refused(&outcome), "pending candidates must be refused");
        }
        assert!(before == frozen(&db, s).await, "refusal must not write");

        // A permit minted while promoted dies when the operation is not promoted.
        for label in ["pending", "abandoned"] {
            let f = retained(&mut db, false).await;
            let s = &f.case.sealed;
            let permit = serving(&mut db, &f, "prepare while promoted").await;
            db.execute(
                "UPDATE posted_file_operations SET state = $2 WHERE id = $1",
                &[&s.op, &label],
            )
            .await
            .quiet("fixture demote operation");
            let before = frozen(&db, s).await;
            assert!(
                is_stale(&invalidate_retained_check(&mut db, &permit).await),
                "{label}: old permit must be stale"
            );
            let owner = f.owner.clone();
            assert!(
                refused(&prep(&mut db, &f, RetainedReferenceKind::Serving, &owner).await),
                "{label}: prepare must be refused"
            );
            assert!(before == frozen(&db, s).await, "{label}: no writes");
        }

        // Verifying and terminal health are not retained-check eligible.
        let f = retained(&mut db, false).await;
        let s = &f.case.sealed;
        let permit = serving(&mut db, &f, "prepare while unknown").await;
        for (label, sql) in [
            (
                "verifying",
                "UPDATE posted_file_health SET state = 'verifying' WHERE generation_id = $1",
            ),
            (
                "terminal",
                "UPDATE posted_file_health SET state = 'terminal_failed', \
                 failure_kind = 'verification' WHERE generation_id = $1",
            ),
        ] {
            db.execute(sql, &[&s.generation])
                .await
                .quiet("fixture non-retainable health");
            let before = frozen(&db, s).await;
            assert!(
                is_stale(&invalidate_retained_check(&mut db, &permit).await),
                "{label}: old permit must be stale"
            );
            let owner = f.owner.clone();
            assert!(
                refused(&prep(&mut db, &f, RetainedReferenceKind::Serving, &owner).await),
                "{label}: prepare must be refused"
            );
            assert!(before == frozen(&db, s).await, "{label}: no writes");
        }
    });
}

#[test]
fn postgres_retained_wrong_missing_and_removed_references_are_refused() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, false).await;
        let s = &f.case.sealed;
        let before = frozen(&db, s).await;
        let owner = f.owner.clone();
        let other_owner = format!("pg-other-{}", f.case.uuid);

        for (label, kind, who) in [
            (
                "wrong kind rollback",
                RetainedReferenceKind::Rollback,
                &owner,
            ),
            (
                "wrong kind legacy",
                RetainedReferenceKind::LegacyPin,
                &owner,
            ),
            ("wrong owner", RetainedReferenceKind::Serving, &other_owner),
        ] {
            assert!(
                refused(&prep(&mut db, &f, kind, who).await),
                "{label}: must be refused"
            );
        }
        let missing = format!("pg-gen-{}-none", f.case.uuid);
        let outcome = prepare_retained_check(
            &mut db,
            &f.attempt,
            &missing,
            RetainedReferenceKind::Serving,
            &owner,
        )
        .await;
        assert!(refused(&outcome), "an unknown generation is refused");
        for (generation, who) in [("", owner.as_str()), (s.generation.as_str(), "bad owner")] {
            let outcome = prepare_retained_check(
                &mut db,
                &f.attempt,
                generation,
                RetainedReferenceKind::Serving,
                who,
            )
            .await;
            assert!(
                matches!(outcome, Err(StoreError::InvalidInput)),
                "malformed identities are invalid input"
            );
        }
        assert!(before == frozen(&db, s).await, "refusals must not write");

        // A reference removed after the permit was minted retires the permit.
        let permit = serving(&mut db, &f, "prepare").await;
        db.execute(
            "DELETE FROM posted_file_references WHERE generation_id = $1 \
             AND kind = 'serving' AND owner_id = $2",
            &[&s.generation, &owner],
        )
        .await
        .quiet("fixture remove reference");
        let after_removal = frozen(&db, s).await;
        assert!(is_stale(&invalidate_retained_check(&mut db, &permit).await));
        assert!(refused(
            &prep(&mut db, &f, RetainedReferenceKind::Serving, &owner).await
        ));
        assert!(after_removal == frozen(&db, s).await, "no writes");
    });
}

#[test]
fn postgres_retained_stale_health_revision_round_or_token_rejects_the_original_callback() {
    run_test(async {
        let mut db = connect().await;
        for (label, sql) in [
            (
                "revision",
                "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
            ),
            (
                "round",
                "UPDATE posted_file_health SET round = round + 1 WHERE generation_id = $1",
            ),
            (
                "token",
                "UPDATE posted_file_health SET round_token = gen_random_uuid()::text \
                 WHERE generation_id = $1",
            ),
        ] {
            let f = retained(&mut db, true).await;
            let s = &f.case.sealed;
            seed_evidence(&db, s).await;
            let old = serving(&mut db, &f, "prepare").await;
            db.execute(sql, &[&s.generation])
                .await
                .quiet("fixture independent drift");
            let before = frozen(&db, s).await;
            let health_before = health(&db, &s.generation).await;
            assert!(
                is_stale(&invalidate_retained_check(&mut db, &old).await),
                "{label}: drifted fence must reject the original callback"
            );
            assert!(before == frozen(&db, s).await, "{label}: no writes");
            assert!(health_before == health(&db, &s.generation).await);

            // Positive control: a permit prepared after the drift is current,
            // and applying it does not revive the old one.
            let current = serving(&mut db, &f, "prepare current").await;
            assert!(is_applied(
                &invalidate_retained_check(&mut db, &current).await
            ));
            assert!(is_stale(&invalidate_retained_check(&mut db, &old).await));
        }
    });
}

#[test]
fn postgres_retained_expired_superseded_attempts_and_stopped_jobs_reject_prepare_and_callback() {
    run_test(async {
        let mut db = connect().await;
        for label in ["expired attempt lease", "superseded attempt", "stopped job"] {
            let mut f = retained(&mut db, true).await;
            let c_job = f.case.job.clone();
            let c_owner = f.case.owner.clone();
            let permit = serving(&mut db, &f, "prepare").await;
            if label == "stopped job" {
                db.execute(
                    "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
                    &[&c_job],
                )
                .await
                .quiet("fixture stop job");
            } else {
                db.execute(
                    "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
                    &[&c_job],
                )
                .await
                .quiet("fixture expire attempt");
            }
            let newer = if label == "superseded attempt" {
                Some(acquire(&mut db, &c_job, &c_owner, 1).await)
            } else {
                None
            };
            let s = f.case.sealed.clone();
            let before = frozen(&db, &s).await;
            assert!(
                is_stale(&invalidate_retained_check(&mut db, &permit).await),
                "{label}: callback must be stale"
            );
            let owner = f.owner.clone();
            assert!(
                refused(&prep(&mut db, &f, RetainedReferenceKind::Serving, &owner).await),
                "{label}: prepare by the original attempt must be refused"
            );
            assert!(before == frozen(&db, &s).await, "{label}: no writes");

            if let Some(newer) = newer {
                // Positive control: only the current attempt may prepare, and
                // the old permit is still dead afterwards.
                f.attempt = newer;
                let fresh = serving(&mut db, &f, "prepare by newer attempt").await;
                assert!(before == frozen(&db, &s).await);
                assert!(is_stale(&invalidate_retained_check(&mut db, &permit).await));
                assert!(is_applied(
                    &invalidate_retained_check(&mut db, &fresh).await
                ));
            }
        }
    });
}

#[test]
fn postgres_retained_same_permit_on_two_connections_applies_exactly_once() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = &f.case.sealed;
        seed_evidence(&db, s).await;
        let permit = serving(&mut db, &f, "prepare").await;
        let h0 = health(&db, &s.generation).await;

        let mut left = connect().await;
        let mut right = connect().await;
        let (a, b) = tokio::join!(
            invalidate_retained_check(&mut left, &permit),
            invalidate_retained_check(&mut right, &permit)
        );
        assert_eq!(
            [&a, &b].iter().filter(|r| is_applied(r)).count(),
            1,
            "exactly one callback applies"
        );
        assert!(is_stale(&a) || is_stale(&b), "the loser is stale");
        let h1 = health(&db, &s.generation).await;
        assert_eq!(h1.revision, h0.revision + 1, "one revision bump");
        assert_eq!(h1.round, h0.round + 1, "one round bump");
        assert_eq!(h1.state, "unknown");
    });
}

#[test]
fn postgres_retained_attempt_expiry_during_registry_or_health_lock_wait_rejects_the_callback() {
    run_test(async {
        let mut db = connect().await;
        for (label, lock_sql, by_hash) in [
            ("registry wait", REGISTRY_LOCK, true),
            ("health wait", HEALTH_LOCK, false),
        ] {
            let f = retained(&mut db, true).await;
            let s = &f.case.sealed;
            seed_evidence(&db, s).await;
            let permit = serving(&mut db, &f, "prepare").await;
            // The callback will hold the attempt row while it waits, so the
            // short lease must be seeded BEFORE it starts.
            let deadline: i64 = db
                .query_one(
                    "UPDATE posted_file_job_attempts SET lease_until = \
                     extract(epoch FROM clock_timestamp())::bigint + 3 \
                     WHERE job_id = $1 RETURNING lease_until",
                    &[&f.case.job],
                )
                .await
                .quiet("bound attempt lease")
                .get(0);
            let before = frozen(&db, s).await;
            let health_before = health(&db, &s.generation).await;
            let mut worker = connect().await;
            let pid: i32 = worker
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .quiet("read worker pid")
                .get(0);
            let key = if by_hash { &s.hash } else { &s.generation };
            let outcome = expire_during_lock_wait(
                &db,
                lock_sql,
                key,
                pid,
                deadline,
                invalidate_retained_check(&mut worker, &permit),
            )
            .await;
            assert!(
                is_stale(&outcome),
                "{label}: a callback whose attempt expired while waiting must be stale"
            );
            assert!(before == frozen(&db, s).await, "{label}: no writes");
            assert!(health_before == health(&db, &s.generation).await);
        }
    });
}

async fn legacy_rows(db: &Client, id: &str, hash: &str) -> Vec<Option<String>> {
    let r = db
        .query_one(
            "SELECT (SELECT md5(t::text) FROM posted_files t WHERE t.id = $1), \
             (SELECT md5(t::text) FROM posted_file_registry t WHERE t.plaintext_sha256 = $2), \
             (SELECT md5(string_agg(t::text, ',' ORDER BY t.kind, t.owner_id)) \
                FROM posted_file_references t WHERE t.generation_id = $1)",
            &[&id, &hash],
        )
        .await
        .quiet("digest legacy rows");
    (0..3).map(|i| r.get(i)).collect()
}

#[test]
fn postgres_retained_legacy_row_without_plan_hash_exposes_custody_only_and_invalidates() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let legacy_hash = digest(&db, "legacy-retained", &c.uuid).await;
        let legacy = Sealed::new(&c.uuid, &legacy_hash, &c.sealed.plan, 0x66);
        let p = legacy.custody();
        let post_copy = spool_copy_state_db_str(p.post_copy);
        let verify_copy = spool_copy_state_db_str(p.verify_copy);
        let params: [&(dyn ToSql + Sync); 25] = [
            &p.plaintext_sha256,
            &p.backbone_post,
            &p.backbone_verify,
            &p.article_count,
            &p.verified_count,
            &p.original_nzb_sha256,
            &post_copy,
            &verify_copy,
            &p.two_spool_verified,
            &p.repost_exhausted,
            &p.plaintext_size,
            &p.article_payload_size,
            &p.header_nonce,
            &p.file_key_encrypted,
            &p.file_key_kek_version,
            &p.file_key_dek_wrapped_nonce,
            &p.file_key_dek_wrapped,
            &p.file_key_payload_nonce,
            &p.file_key_payload_format,
            &p.manifest_encrypted,
            &p.manifest_kek_version,
            &p.manifest_dek_wrapped_nonce,
            &p.manifest_dek_wrapped,
            &p.manifest_payload_nonce,
            &p.manifest_payload_format,
        ];
        assert_eq!(
            db.execute(insert_posted_file_sql(), &params)
                .await
                .quiet("insert legacy-shaped row"),
            1
        );
        let legacy_id: String = db
            .query_one(select_posted_file_id_by_sha_sql(), &[&legacy_hash])
            .await
            .quiet("read legacy id")
            .get(0);
        // Explicit fixture SQL mirrors the migration's conservative backfill.
        db.execute(
            "INSERT INTO posted_file_registry (plaintext_sha256, selected_generation) \
             VALUES ($1, $2) ON CONFLICT (plaintext_sha256) DO NOTHING",
            &[&legacy_hash, &legacy_id],
        )
        .await
        .quiet("fixture legacy registry");
        db.execute(
            "INSERT INTO posted_file_health (generation_id) VALUES ($1) \
             ON CONFLICT (generation_id) DO NOTHING",
            &[&legacy_id],
        )
        .await
        .quiet("fixture legacy health");
        db.execute(
            "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
             VALUES ('legacy_pin', $1, $1, $2) ON CONFLICT DO NOTHING",
            &[&legacy_id, &legacy_hash],
        )
        .await
        .quiet("fixture legacy pin");
        let null_plan = count(
            &db,
            "SELECT count(*) FROM posted_files WHERE id = $1 AND plan_sha256 IS NULL \
             AND generation_operation IS NULL",
            &[&legacy_id],
        )
        .await;
        assert_eq!(
            null_plan, 1,
            "the fixture row has no plan hash or operation"
        );

        // A standing round does not upgrade legacy custody into a managed plan.
        // Avoid masking the null-plan refusal behind the separate round-zero gate.
        db.execute(
            "UPDATE posted_file_health SET round = 1 WHERE generation_id = $1",
            &[&legacy_id],
        )
        .await
        .quiet("fixture legacy standing round");
        let before = legacy_rows(&db, &legacy_id, &legacy_hash).await;
        let h0 = health(&db, &legacy_id).await;

        let outcome = prepare_retained_check(
            &mut db,
            &attempt,
            &legacy_id,
            RetainedReferenceKind::LegacyPin,
            &legacy_id,
        )
        .await;
        let (permit, custody) = minted(outcome, "prepare legacy");
        assert!(
            custody.custody().expect("legacy custody") == legacy.custody(),
            "legacy custody is exposed exactly"
        );
        assert_eq!(format!("{custody:?}"), "RetainedCustody([REDACTED])");
        assert!(
            before == legacy_rows(&db, &legacy_id, &legacy_hash).await,
            "prepare writes nothing"
        );
        assert!(h0 == health(&db, &legacy_id).await);

        // A pin does not match another reference kind.
        let wrong = prepare_retained_check(
            &mut db,
            &attempt,
            &legacy_id,
            RetainedReferenceKind::Serving,
            &legacy_id,
        )
        .await;
        assert!(refused(&wrong));

        // Content reads need the sealed plan hash: a null-plan legacy row is
        // refused on both sides without any write.
        for side in [ArticleSide::Post, ArticleSide::Verify] {
            let content = prepare_retained_content_check(
                &mut db,
                &attempt,
                &legacy_id,
                RetainedReferenceKind::LegacyPin,
                &legacy_id,
                side,
            )
            .await;
            assert!(matches!(content, Ok(None)), "null plan: content refused");
        }
        assert!(
            before == legacy_rows(&db, &legacy_id, &legacy_hash).await,
            "content refusal writes nothing"
        );
        assert!(h0 == health(&db, &legacy_id).await);

        assert!(is_applied(
            &invalidate_retained_check(&mut db, &permit).await
        ));
        let h1 = health(&db, &legacy_id).await;
        assert_eq!(h1.state, "unknown");
        assert_eq!(h1.revision, h0.revision + 1);
        assert_eq!(h1.round, h0.round + 1);
        assert!(h1.token != h0.token);
        assert!(
            before == legacy_rows(&db, &legacy_id, &legacy_hash).await,
            "custody, registry and the pin are unchanged"
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_files WHERE id = $1 AND plan_sha256 IS NULL",
                &[&legacy_id]
            )
            .await,
            1,
            "invalidation never invents a plan hash"
        );
        assert!(is_stale(&invalidate_retained_check(&mut db, &permit).await));
    });
}

#[test]
fn postgres_retained_reference_reinsert_after_prepare_stales_negative_callback_and_keeps_state() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = f.case.sealed.clone();
        seed_evidence(&db, &s).await;
        let old_token = reference_token(&db, &s, "serving", &f.owner).await;
        let permit = serving(&mut db, &f, "prepare").await;

        reinsert_reference(&db, &s, &f.owner).await;
        assert!(
            reference_token(&db, &s, "serving", &f.owner).await != old_token,
            "a reinserted reference must be a new incarnation"
        );
        let before = frozen(&db, &s).await;
        let h0 = health(&db, &s.generation).await;
        assert_eq!(h0.state, "usable");

        assert!(
            is_stale(&invalidate_retained_check(&mut db, &permit).await),
            "the callback of a replaced reference must be stale"
        );
        assert!(before == frozen(&db, &s).await, "stale callback: no writes");
        assert!(h0 == health(&db, &s.generation).await, "health unchanged");
        assert_eq!(custody_rows(&db, &s).await, 1);

        // A permit prepared against the new incarnation is current; the old
        // one stays dead.
        let fresh = serving(&mut db, &f, "prepare new incarnation").await;
        assert!(is_applied(
            &invalidate_retained_check(&mut db, &fresh).await
        ));
        assert_eq!(health(&db, &s.generation).await.state, "unknown");
        assert!(is_stale(&invalidate_retained_check(&mut db, &permit).await));
    });
}

#[test]
fn postgres_retained_reference_reinsert_while_callback_waits_on_reference_row_refuses_it() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = f.case.sealed.clone();
        seed_evidence(&db, &s).await;
        let permit = serving(&mut db, &f, "prepare").await;
        let before = frozen(&db, &s).await;
        let h0 = health(&db, &s.generation).await;

        let mut worker = connect().await;
        let pid = backend_pid(&worker).await;
        let owner = f.owner.clone();
        let outcome = reinsert_during_reference_wait(
            &db,
            &s,
            &owner,
            pid,
            invalidate_retained_check(&mut worker, &permit),
        )
        .await;
        assert!(
            is_stale(&outcome),
            "a callback released after a reference reinsert must be stale"
        );
        // Everything except the replaced reference row is unchanged.
        let after = frozen(&db, &s).await;
        for index in [0, 1, 2, 3] {
            assert!(before.0[index] == after.0[index], "no state change");
        }
        assert!(before.1 == after.1, "evidence unchanged");
        assert!(h0 == health(&db, &s.generation).await, "health unchanged");
        assert!(is_stale(&invalidate_retained_check(&mut db, &permit).await));
        assert!(after == frozen(&db, &s).await, "replay writes nothing");

        let fresh = serving(&mut db, &f, "prepare new incarnation").await;
        assert!(is_applied(
            &invalidate_retained_check(&mut db, &fresh).await
        ));
    });
}

#[test]
fn postgres_retained_noop_reference_upsert_preserves_the_original_permit() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = f.case.sealed.clone();
        seed_evidence(&db, &s).await;
        let token = reference_token(&db, &s, "serving", &f.owner).await;
        let permit = serving(&mut db, &f, "prepare").await;
        let before = frozen(&db, &s).await;

        // Both no-op shapes: DO NOTHING and an identical-value DO UPDATE.
        db.execute(
            "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
             VALUES ('serving', $1, $2, $3) ON CONFLICT DO NOTHING",
            &[&f.owner, &s.generation, &s.hash],
        )
        .await
        .quiet("no-op reference do nothing");
        db.execute(
            "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
             VALUES ('serving', $1, $2, $3) ON CONFLICT (kind, owner_id, generation_id) \
             DO UPDATE SET plaintext_sha256 = EXCLUDED.plaintext_sha256",
            &[&f.owner, &s.generation, &s.hash],
        )
        .await
        .quiet("no-op reference upsert");
        assert!(
            reference_token(&db, &s, "serving", &f.owner).await == token,
            "a true no-op must keep the incarnation token"
        );
        assert!(before == frozen(&db, &s).await, "no-op writes nothing");

        assert!(
            is_applied(&invalidate_retained_check(&mut db, &permit).await),
            "the original permit survives a no-op upsert"
        );
        assert_eq!(health(&db, &s.generation).await.state, "unknown");
    });
}

async fn reference_tokens(db: &Client, s: &Sealed) -> Vec<String> {
    db.query(
        "SELECT reference_token FROM posted_file_references WHERE generation_id = $1 \
         ORDER BY kind, owner_id",
        &[&s.generation],
    )
    .await
    .quiet("read reference tokens")
    .iter()
    .map(|r| r.get(0))
    .collect()
}

#[test]
fn postgres_retained_reference_token_survives_0044_and_historical_0037_replay() {
    run_blocking(true, async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = f.case.sealed.clone();
        let rollback = format!("pg-rollback-{}", f.case.uuid);
        add_reference(&db, &s, "rollback", &rollback).await;
        seed_evidence(&db, &s).await;
        let permit = serving(&mut db, &f, "prepare before replay").await;
        let tokens = reference_tokens(&db, &s).await;
        assert_eq!(tokens.len(), 2);
        let before = frozen(&db, &s).await;
        let h0 = health(&db, &s.generation).await;

        let ddl = connect().await;
        ddl.batch_execute(MIGRATION_0044)
            .await
            .quiet("replay migration 0044");
        ddl.batch_execute(MIGRATION_0044)
            .await
            .quiet("replay migration 0044 again");
        ddl.batch_execute(MIGRATION_0037)
            .await
            .quiet("replay historical migration 0037");
        ddl.batch_execute(MIGRATION_0044)
            .await
            .quiet("replay migration 0044 after historical 0037");

        assert!(
            tokens == reference_tokens(&db, &s).await,
            "replay must preserve every reference token"
        );
        assert!(before == frozen(&db, &s).await, "replay preserves state");
        assert!(h0 == health(&db, &s.generation).await);
        assert_eq!(custody_rows(&db, &s).await, 1);
        assert!(
            db_rejected(
                db.execute(
                    "UPDATE posted_file_references SET reference_token = gen_random_uuid()::text \
                     WHERE generation_id = $1",
                    &[&s.generation],
                )
                .await
            ),
            "the immutability trigger survives replay"
        );
        assert!(
            is_applied(&invalidate_retained_check(&mut db, &permit).await),
            "a permit prepared before replay is still current"
        );
    });
}

#[test]
fn postgres_retained_reference_identity_and_token_are_immutable_and_tokens_are_distinct() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = f.case.sealed.clone();
        let rollback = format!("pg-rollback-{}", f.case.uuid);
        let pin = format!("pg-pin-{}", f.case.uuid);
        let in_flight = format!("pg-flight-{}", f.case.uuid);
        add_reference(&db, &s, "rollback", &rollback).await;
        add_reference(&db, &s, "legacy_pin", &pin).await;
        add_reference(&db, &s, "in_flight", &in_flight).await;

        let tokens = reference_tokens(&db, &s).await;
        assert_eq!(tokens.len(), 4);
        let mut unique = tokens.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 4, "default tokens must be distinct");
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_references WHERE generation_id = $1 \
                 AND reference_token ~ \
                 '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'",
                &[&s.generation]
            )
            .await,
            4,
            "every token has UUID shape"
        );
        let other_hash = digest(&db, "other-reference-identity", &f.case.uuid).await;
        let before = snapshot(&db, &s).await;

        let owner = f.owner.clone();
        let updates: [(&str, &str); 6] = [
            (
                "kind",
                "UPDATE posted_file_references SET kind = 'rollback'",
            ),
            (
                "owner",
                "UPDATE posted_file_references SET owner_id = owner_id || 'x'",
            ),
            (
                "generation",
                "UPDATE posted_file_references SET generation_id = generation_id || 'x'",
            ),
            (
                "hash",
                "UPDATE posted_file_references SET plaintext_sha256 = $3",
            ),
            (
                "token",
                "UPDATE posted_file_references SET reference_token = gen_random_uuid()::text",
            ),
            (
                "token copy",
                "UPDATE posted_file_references SET reference_token = \
                 (SELECT reference_token FROM posted_file_references \
                  WHERE generation_id = $1 AND kind = 'rollback')",
            ),
        ];
        for (label, head) in updates {
            let sql = format!(
                "{head} WHERE generation_id = $1 AND kind = 'serving' AND owner_id = $2 \
                 AND $3::text IS NOT NULL"
            );
            assert!(
                db_rejected(
                    db.execute(sql.as_str(), &[&s.generation, &owner, &other_hash])
                        .await
                ),
                "{label}: identity update must be rejected"
            );
            assert!(before == snapshot(&db, &s).await, "{label}: no change");
        }
        assert!(tokens == reference_tokens(&db, &s).await);

        // A true no-op update is permitted and keeps the token.
        db.execute(
            "UPDATE posted_file_references SET owner_id = owner_id, \
             reference_token = reference_token WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("true no-op reference update");
        assert!(tokens == reference_tokens(&db, &s).await);

        // Delete and reinsert of the same key yields a new default token.
        reinsert_reference(&db, &s, &owner).await;
        let after = reference_tokens(&db, &s).await;
        assert_eq!(after.len(), 4);
        assert!(
            after.iter().filter(|t| tokens.contains(t)).count() == 3,
            "only the reinserted reference gets a new token"
        );
    });
}
