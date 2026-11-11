//! Real-PostgreSQL tests for the persisted verification recheck schedule
//! (`prepare_recheck`, `complete_recheck`, `GenerationCheckpoint::recheck_schedule`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `case`, `acquire`,
//! `admitted`, `observe`, `recover`, `count`, `snapshot`, `run_blocking`,
//! `MIGRATION_0038`). Fixtures stay synthetic sealed byte buffers keyed by
//! database UUIDs. Time is advanced only by setting the persisted
//! `not_before` to zero; nothing sleeps. Failures print only static labels;
//! outcomes are matched, never formatted, so no assertion can echo handles,
//! permits, tokens, rows or encrypted material.
//!
//! Scope: a recheck pass is bookkeeping only. It never writes a negative
//! health, verified/retention evidence, a selection or a repost count.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, complete_recheck, confirm_article, finish_verification,
    load_checkpoint, prepare_recheck, take_over, ArticleSide, GenerationHandle, MutationOutcome,
    RecheckPermit, RecheckPreparation, RecheckSchedule,
};
use nntune_ingest::two_spool::{recheck_delay_ms, MAX_RECHECKS};

const ARTICLES: i32 = 3;
/// Slack for the database clock moving between a probe and the write it brackets.
const CLOCK_SLACK: i64 = 5;

fn applied_handle(
    outcome: Result<MutationOutcome, StoreError>,
    ctx: &'static str,
) -> GenerationHandle {
    match outcome {
        Ok(MutationOutcome::Applied(handle)) => *handle,
        _ => panic!("{ctx}: expected an applied mutation"),
    }
}

fn is_stale(outcome: &Result<MutationOutcome, StoreError>) -> bool {
    matches!(outcome, Ok(MutationOutcome::StaleOrDiverged))
}

fn ready_permit(
    outcome: Result<RecheckPreparation, StoreError>,
    ctx: &'static str,
) -> RecheckPermit {
    match outcome {
        Ok(RecheckPreparation::Ready(permit)) => *permit,
        _ => panic!("{ctx}: expected a ready recheck permit"),
    }
}

fn prep_stale(outcome: &Result<RecheckPreparation, StoreError>) -> bool {
    matches!(outcome, Ok(RecheckPreparation::StaleOrDiverged))
}

async fn begun(db: &mut Client) -> (Case, AttemptHandle, GenerationHandle) {
    let c = case(db).await;
    let attempt = acquire(db, &c.job, &c.owner, 0).await;
    let admitted_handle = admitted(db, &attempt, &c.sealed).await;
    let handle = applied_handle(
        begin_verification_round(db, &admitted_handle).await,
        "begin first round",
    );
    (c, attempt, handle)
}

/// Persisted schedule of the current round: `(round, passes, state, not_before)`.
async fn schedule(db: &Client, s: &Sealed) -> (i64, i32, String, Option<i64>) {
    let row = db
        .query_one(
            "SELECT r.round, r.completed_passes, r.state, r.not_before \
             FROM posted_file_recheck_schedule r \
             JOIN posted_file_health h \
               ON h.generation_id = r.generation_id AND h.round = r.round \
             WHERE r.generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("read recheck schedule");
    (row.get(0), row.get(1), row.get(2), row.get(3))
}

async fn schedule_rows(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_recheck_schedule WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
}

async fn db_now(db: &Client) -> i64 {
    db.query_one(
        "SELECT floor(extract(epoch FROM clock_timestamp()))::bigint",
        &[],
    )
    .await
    .quiet("read database clock")
    .get(0)
}

/// Make the current round's schedule due without sleeping.
async fn make_due(db: &Client, s: &Sealed) {
    let changed = db
        .execute(
            "UPDATE posted_file_recheck_schedule SET not_before = 0 \
             WHERE generation_id = $1 AND state = 'waiting'",
            &[&s.generation],
        )
        .await
        .quiet("advance schedule");
    assert!(changed >= 1, "a waiting schedule must exist to advance");
}

async fn health_revision(db: &Client, s: &Sealed) -> i64 {
    db.query_one(
        "SELECT revision FROM posted_file_health WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("read health revision")
    .get(0)
}

/// `(state, failure_kind, verified_at, retention_checked_at)` of the health row.
async fn health_outcome(
    db: &Client,
    s: &Sealed,
) -> (String, Option<String>, Option<i64>, Option<i64>) {
    let row = db
        .query_one(
            "SELECT state, failure_kind, verified_at, retention_checked_at \
             FROM posted_file_health WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("read health outcome");
    (row.get(0), row.get(1), row.get(2), row.get(3))
}

async fn repost_sum(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT COALESCE(sum(repost_attempts), 0)::bigint \
         FROM posted_file_article_progress WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
}

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

/// Prepare, complete and return the refreshed handle (schedule must be due).
async fn pass(db: &mut Client, handle: &GenerationHandle) -> GenerationHandle {
    let permit = ready_permit(prepare_recheck(db, handle).await, "prepare recheck");
    applied_handle(
        complete_recheck(db, handle, &permit).await,
        "complete recheck",
    )
}

async fn fresh_handle(db: &mut Client, attempt: &AttemptHandle, s: &Sealed) -> GenerationHandle {
    match recover(db, attempt, &s.hash, &s.op).await {
        Ok(AdmissionOutcome::ResumedSameCandidate(handle, _)) => handle,
        _ => panic!("recovery did not resume the candidate"),
    }
}

async fn expire_attempt_lease(db: &Client, job: &str) {
    db.execute(
        "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
        &[&job],
    )
    .await
    .quiet("expire attempt");
}

async fn expire_operation_lease(db: &Client, s: &Sealed) {
    db.execute(
        "UPDATE posted_file_operations SET lease_until = 0 WHERE id = $1",
        &[&s.op],
    )
    .await
    .quiet("expire operation");
}

#[test]
fn postgres_begun_round_persists_waiting_schedule_and_early_prepare_mutates_nothing() {
    run_blocking(false, async {
        let mut db = connect().await;
        let before_begin = db_now(&db).await;
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        let after_begin = db_now(&db).await;

        let (round, passes, state, not_before) = schedule(&db, s).await;
        assert!(round >= 1);
        assert_eq!(passes, 0);
        assert_eq!(state, "waiting");
        let not_before = not_before.expect("waiting schedule carries a due time");
        assert!(not_before >= before_begin + 60, "initial propagation floor");
        assert!(not_before <= after_begin + 60 + CLOCK_SLACK);

        let frozen = (
            snapshot(&db, s).await,
            progress_digest(&db, s).await,
            health_revision(&db, s).await,
        );
        match prepare_recheck(&mut db, &handle).await {
            Ok(RecheckPreparation::Waiting { not_before: due }) => assert_eq!(due, not_before),
            _ => panic!("an early prepare must report the persisted due time"),
        }
        assert!(
            frozen
                == (
                    snapshot(&db, s).await,
                    progress_digest(&db, s).await,
                    health_revision(&db, s).await
                ),
            "waiting must not mutate"
        );
        assert_eq!(schedule(&db, s).await.1, 0);

        // The read-only checkpoint exposes the same nonsecret schedule scalars.
        let cp = match load_checkpoint(&mut db, &handle).await {
            Ok(Some(cp)) => cp,
            _ => panic!("expected a current checkpoint"),
        };
        assert!(
            cp.recheck_schedule()
                == Some(RecheckSchedule::Waiting {
                    completed_passes: 0,
                    not_before
                })
        );
    });
}

#[test]
fn postgres_recheck_passes_back_off_to_the_cap_without_negative_health_or_repost_counts() {
    run_blocking(false, async {
        let mut db = connect().await;
        let (c, _attempt, mut handle) = begun(&mut db).await;
        let s = &c.sealed;
        let frozen = snapshot(&db, s).await;
        let baseline_health = health_outcome(&db, s).await;
        assert_eq!(baseline_health.0, "verifying");

        for done in 0..MAX_RECHECKS {
            make_due(&db, s).await;
            let revision = health_revision(&db, s).await;
            let before = db_now(&db).await;
            handle = pass(&mut db, &handle).await;
            let after = db_now(&db).await;

            assert_eq!(
                health_revision(&db, s).await,
                revision + 1,
                "one pass moves the health revision once"
            );
            let (_, passes, state, not_before) = schedule(&db, s).await;
            assert_eq!(passes as u32, done + 1);
            if done + 1 < MAX_RECHECKS {
                assert_eq!(state, "waiting");
                let delay = (recheck_delay_ms(done) / 1000) as i64;
                let due = not_before.expect("waiting schedule has a due time");
                assert!(due >= before + delay, "backoff uses the old pass count");
                assert!(due <= after + delay + CLOCK_SLACK);
            } else {
                assert_eq!(state, "exhausted");
                assert!(not_before.is_none(), "exhausted schedule has no due time");
            }
            // Bookkeeping only: never negative, verified, retained or selected.
            assert!(health_outcome(&db, s).await == baseline_health);
            assert_eq!(repost_sum(&db, s).await, 0);
            let now = snapshot(&db, s).await;
            for index in [0, 1, 4] {
                assert!(now[index] == frozen[index], "custody, registry, references");
            }
        }

        assert!(matches!(
            prepare_recheck(&mut db, &handle).await,
            Ok(RecheckPreparation::Exhausted)
        ));
        let cp = match load_checkpoint(&mut db, &handle).await {
            Ok(Some(cp)) => cp,
            _ => panic!("expected a current checkpoint"),
        };
        assert!(cp.recheck_schedule() == Some(RecheckSchedule::Exhausted));
        assert_eq!(schedule(&db, s).await.1 as u32, MAX_RECHECKS);
    });
}

#[test]
fn postgres_lost_ack_and_duplicate_or_parallel_completion_count_exactly_once() {
    run_blocking(false, async {
        let mut db = connect().await;

        // Lost acknowledgment: the answer is dropped, the count is persisted.
        let (c, attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        make_due(&db, s).await;
        let permit = ready_permit(prepare_recheck(&mut db, &handle).await, "prepare");
        let _lost = applied_handle(
            complete_recheck(&mut db, &handle, &permit).await,
            "complete",
        );
        assert_eq!(schedule(&db, s).await.1, 1);
        let revision = health_revision(&db, s).await;

        let fresh = fresh_handle(&mut db, &attempt, s).await;
        let cp = match load_checkpoint(&mut db, &fresh).await {
            Ok(Some(cp)) => cp,
            _ => panic!("expected a current checkpoint"),
        };
        assert!(matches!(
            cp.recheck_schedule(),
            Some(RecheckSchedule::Waiting {
                completed_passes: 1,
                ..
            })
        ));
        let frozen = (schedule(&db, s).await, snapshot(&db, s).await);
        assert!(is_stale(&complete_recheck(&mut db, &handle, &permit).await));
        assert!(
            is_stale(&complete_recheck(&mut db, &fresh, &permit).await),
            "a recovered handle must not revive the original permit"
        );
        assert!(
            frozen == (schedule(&db, s).await, snapshot(&db, s).await),
            "replay must not count twice"
        );
        assert_eq!(health_revision(&db, s).await, revision);

        // Parallel completion of one permit on separate sessions.
        make_due(&db, s).await;
        let permit = ready_permit(prepare_recheck(&mut db, &fresh).await, "prepare second");
        let mut left = connect().await;
        let mut right = connect().await;
        let (a, b) = tokio::join!(
            complete_recheck(&mut left, &fresh, &permit),
            complete_recheck(&mut right, &fresh, &permit)
        );
        let applied_count = [&a, &b]
            .iter()
            .filter(|r| matches!(r, Ok(MutationOutcome::Applied(_))))
            .count();
        assert_eq!(applied_count, 1, "exactly one completion applies");
        assert!(
            is_stale(&a) || is_stale(&b),
            "the loser is rejected as stale"
        );
        assert_eq!(schedule(&db, s).await.1, 2);
        assert_eq!(health_revision(&db, s).await, revision + 1);
    });
}

#[test]
fn postgres_confirmations_refresh_the_handle_within_a_pass_and_old_revisions_are_rejected() {
    run_blocking(false, async {
        let mut db = connect().await;
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        make_due(&db, s).await;
        let permit = ready_permit(prepare_recheck(&mut db, &handle).await, "prepare");

        // A confirmation moves the health revision; the refreshed handle may
        // still complete the pass the permit was issued for.
        let refreshed = applied_handle(
            confirm_article(&mut db, &handle, 0, ArticleSide::Post).await,
            "confirm article",
        );
        let revision = health_revision(&db, s).await;
        let chain = applied_handle(
            complete_recheck(&mut db, &refreshed, &permit).await,
            "complete with refreshed handle",
        );
        assert_eq!(schedule(&db, s).await.1, 1);
        assert_eq!(health_revision(&db, s).await, revision + 1);

        // The pre-confirmation handle is an old revision for the next pass.
        make_due(&db, s).await;
        let next = ready_permit(prepare_recheck(&mut db, &chain).await, "prepare next");
        let before = (schedule(&db, s).await, health_revision(&db, s).await);
        assert!(is_stale(&complete_recheck(&mut db, &handle, &next).await));
        assert!(prep_stale(&prepare_recheck(&mut db, &handle).await));
        assert!(before == (schedule(&db, s).await, health_revision(&db, s).await));

        // A revision moved after the permit retires the old handle.
        let moved = applied_handle(
            confirm_article(&mut db, &chain, 1, ArticleSide::Post).await,
            "confirm second article",
        );
        assert!(is_stale(&complete_recheck(&mut db, &chain, &next).await));
        applied_handle(
            complete_recheck(&mut db, &moved, &next).await,
            "complete after refresh",
        );
        assert_eq!(schedule(&db, s).await.1, 2);
    });
}

#[test]
fn postgres_takeover_preserves_schedule_and_count_and_invalidates_the_old_permit() {
    run_blocking(false, async {
        let mut db = connect().await;
        let (c, _first, handle) = begun(&mut db).await;
        let s = &c.sealed;
        make_due(&db, s).await;
        let handle = pass(&mut db, &handle).await;
        make_due(&db, s).await;
        let old_permit = ready_permit(prepare_recheck(&mut db, &handle).await, "prepare");

        expire_attempt_lease(&db, &c.job).await;
        expire_operation_lease(&db, s).await;
        let next = acquire(&mut db, &c.job, &c.owner, 1).await;
        let before = (schedule(&db, s).await, snapshot(&db, s).await);
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");
        let taken = match take_over(&mut db, &next, &expected, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(h, _)) => h,
            _ => panic!("takeover did not resume the candidate"),
        };
        assert!(
            before.0 == schedule(&db, s).await,
            "takeover keeps schedule"
        );
        let after = snapshot(&db, s).await;
        for index in [0, 1, 3, 4] {
            assert!(
                before.1[index] == after[index],
                "only operation claim changes"
            );
        }
        assert!(
            before.1[2] != after[2],
            "takeover rotates the operation claim"
        );
        assert_eq!(schedule(&db, s).await.1, 1);

        assert!(is_stale(
            &complete_recheck(&mut db, &taken, &old_permit).await
        ));
        assert!(is_stale(
            &complete_recheck(&mut db, &handle, &old_permit).await
        ));
        assert!(prep_stale(&prepare_recheck(&mut db, &handle).await));
        assert_eq!(schedule(&db, s).await.1, 1, "stale calls must not count");

        let permit = ready_permit(prepare_recheck(&mut db, &taken).await, "prepare taken");
        applied_handle(
            complete_recheck(&mut db, &taken, &permit).await,
            "complete under the new attempt",
        );
        assert_eq!(schedule(&db, s).await.1, 2);
    });
}

#[test]
fn postgres_round_verification_lease_and_job_fences_reject_recheck_calls() {
    run_blocking(false, async {
        let mut db = connect().await;

        // An admitted candidate without a begun round has no schedule.
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let unbegun = admitted(&mut db, &attempt, s).await;
        assert!(prep_stale(&prepare_recheck(&mut db, &unbegun).await));
        assert_eq!(schedule_rows(&db, s).await, 0);

        // A finished verification and a new round retire the old permit.
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        make_due(&db, s).await;
        let permit = ready_permit(prepare_recheck(&mut db, &handle).await, "prepare");
        let mut chain = handle;
        for slot in 0..ARTICLES {
            for side in [ArticleSide::Post, ArticleSide::Verify] {
                chain = applied_handle(
                    confirm_article(&mut db, &chain, slot, side).await,
                    "confirm fixture article",
                );
            }
        }
        let finished = applied_handle(
            finish_verification(&mut db, &chain).await,
            "finish verification",
        );
        let frozen = (schedule(&db, s).await, snapshot(&db, s).await);
        assert!(prep_stale(&prepare_recheck(&mut db, &finished).await));
        assert!(is_stale(
            &complete_recheck(&mut db, &finished, &permit).await
        ));
        assert!(
            frozen == (schedule(&db, s).await, snapshot(&db, s).await),
            "finished verification must not be rechecked"
        );

        let first_round = schedule(&db, s).await.0;
        let next = applied_handle(
            begin_verification_round(&mut db, &finished).await,
            "begin second round",
        );
        assert_eq!(schedule_rows(&db, s).await, 2, "one schedule per round");
        let (round, passes, state, _) = schedule(&db, s).await;
        assert!(round > first_round);
        assert_eq!((passes, state.as_str()), (0, "waiting"));
        assert!(is_stale(&complete_recheck(&mut db, &next, &permit).await));
        assert!(is_stale(&complete_recheck(&mut db, &chain, &permit).await));
        assert_eq!(schedule(&db, s).await.1, 0, "old permits must not count");

        // Every lease, job and attempt fence refuses prepare and complete.
        for label in [
            "expired attempt lease",
            "expired operation lease",
            "job status changed",
            "attempt superseded",
        ] {
            let (c, _attempt, handle) = begun(&mut db).await;
            let s = &c.sealed;
            make_due(&db, s).await;
            let permit = ready_permit(prepare_recheck(&mut db, &handle).await, "prepare");
            match label {
                "expired operation lease" => expire_operation_lease(&db, s).await,
                "job status changed" => {
                    db.execute(
                        "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
                        &[&c.job],
                    )
                    .await
                    .quiet("change job status");
                }
                _ => {
                    expire_attempt_lease(&db, &c.job).await;
                    if label == "attempt superseded" {
                        let _newer = acquire(&mut db, &c.job, &c.owner, 1).await;
                    }
                }
            }
            let before = (schedule(&db, s).await, snapshot(&db, s).await);
            assert!(
                is_stale(&complete_recheck(&mut db, &handle, &permit).await),
                "{label}: complete must be stale"
            );
            assert!(
                prep_stale(&prepare_recheck(&mut db, &handle).await),
                "{label}: prepare must be stale"
            );
            assert!(
                before == (schedule(&db, s).await, snapshot(&db, s).await),
                "{label}: fenced calls must not mutate"
            );
            assert_eq!(schedule(&db, s).await.1, 0, "{label}: count changed");
        }
    });
}

#[test]
fn postgres_schema_replay_preserves_schedule_and_backfills_missing_rows_as_untracked() {
    run_blocking(true, async {
        let mut db = connect().await;
        let (c, attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        make_due(&db, s).await;
        let handle = pass(&mut db, &handle).await;
        let kept = schedule(&db, s).await;
        let health = health_outcome(&db, s).await;
        assert_eq!(kept.1, 1);

        let ddl = connect().await;
        ddl.batch_execute(MIGRATION_0038)
            .await
            .quiet("replay migration 0038");
        ddl.batch_execute(MIGRATION_0038)
            .await
            .quiet("replay migration 0038 again");
        assert!(
            kept == schedule(&db, s).await,
            "replay must preserve the persisted schedule"
        );
        assert!(health == health_outcome(&db, s).await);
        let handle = {
            let _ = handle;
            fresh_handle(&mut db, &attempt, s).await
        };
        match prepare_recheck(&mut db, &handle).await {
            Ok(RecheckPreparation::Waiting { not_before }) => {
                assert_eq!(Some(not_before), kept.3)
            }
            _ => panic!("a preserved schedule must keep waiting"),
        }

        // A live round with no schedule row is backfilled conservatively: it
        // is never treated as waiting, due or fresh.
        db.execute(
            "DELETE FROM posted_file_recheck_schedule WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("remove schedule row");
        ddl.batch_execute(MIGRATION_0038)
            .await
            .quiet("replay migration 0038 backfill");
        let (_, _, state, _) = schedule(&db, s).await;
        assert_eq!(state, "untracked");
        assert!(matches!(
            prepare_recheck(&mut db, &handle).await,
            Ok(RecheckPreparation::Untracked)
        ));
        let cp = match load_checkpoint(&mut db, &handle).await {
            Ok(Some(cp)) => cp,
            _ => panic!("expected a current checkpoint"),
        };
        assert!(cp.recheck_schedule() == Some(RecheckSchedule::Untracked));
        assert_eq!(health_outcome(&db, s).await, health);
        ddl.batch_execute(MIGRATION_0038)
            .await
            .quiet("replay migration 0038 after backfill");
        assert_eq!(schedule(&db, s).await.2, "untracked");
    });
}
