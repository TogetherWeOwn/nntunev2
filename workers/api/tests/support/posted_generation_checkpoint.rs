//! Real-PostgreSQL tests for the per-slot repost accounting and checkpoint
//! snapshot adapter (`prepare_repost`, `record_repost`, `load_checkpoint`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `acquire`, `admitted`,
//! `count`, `snapshot`, `recover`, `observe`). Fixtures stay synthetic sealed
//! byte buffers keyed by database UUIDs: no article identities, keys or names.
//! Failures print only static labels; outcomes are matched, never formatted,
//! so no assertion can echo handles, permits, tokens or encrypted material.
//!
//! Scope: answered-POST accounting and a read-only snapshot. A counted repost
//! never confirms presence, proves content, or authorizes replacement, reuse,
//! promotion or cleanup, and nothing here expects any of those.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, confirm_article, finish_verification, load_checkpoint,
    prepare_repost, record_repost, take_over, ArticleSide, GenerationCheckpoint, GenerationHandle,
    MutationOutcome, RepostPermit, RepostPreparation,
};
use nntune_ingest::two_spool::{PostVerdict, MAX_REPOST_ATTEMPTS_PER_ARTICLE};
use std::sync::Arc;
use tokio::sync::Barrier;

/// The three-article fixture addresses slots 0..3.
const ARTICLES: i32 = 3;

fn applied(outcome: Result<MutationOutcome, StoreError>, ctx: &'static str) -> GenerationHandle {
    match outcome {
        Ok(MutationOutcome::Applied(handle)) => *handle,
        _ => panic!("{ctx}: expected an applied mutation"),
    }
}

fn is_stale(outcome: &Result<MutationOutcome, StoreError>) -> bool {
    matches!(outcome, Ok(MutationOutcome::StaleOrDiverged))
}

fn is_incomplete(outcome: &Result<MutationOutcome, StoreError>) -> bool {
    matches!(outcome, Ok(MutationOutcome::Incomplete))
}

fn ready(outcome: Result<RepostPreparation, StoreError>, ctx: &'static str) -> RepostPermit {
    match outcome {
        Ok(RepostPreparation::Ready(permit)) => *permit,
        _ => panic!("{ctx}: expected a ready repost permit"),
    }
}

fn prep_stale(outcome: &Result<RepostPreparation, StoreError>) -> bool {
    matches!(outcome, Ok(RepostPreparation::StaleOrDiverged))
}

/// Admitted candidate with a freshly begun verification round.
async fn begun(db: &mut Client) -> (Case, AttemptHandle, GenerationHandle) {
    let c = case(db).await;
    let attempt = acquire(db, &c.job, &c.owner, 0).await;
    let admitted_handle = admitted(db, &attempt, &c.sealed).await;
    let handle = applied(
        begin_verification_round(db, &admitted_handle).await,
        "begin first round",
    );
    (c, attempt, handle)
}

async fn permit(
    db: &mut Client,
    handle: &GenerationHandle,
    slot: i32,
    side: ArticleSide,
) -> RepostPermit {
    ready(
        prepare_repost(db, handle, slot, side).await,
        "prepare repost",
    )
}

/// Record an answered POST and return the refreshed handle.
async fn record(db: &mut Client, permit: &RepostPermit, verdict: PostVerdict) -> GenerationHandle {
    applied(record_repost(db, permit, verdict).await, "record repost")
}

async fn checkpoint(db: &mut Client, handle: &GenerationHandle) -> GenerationCheckpoint {
    match load_checkpoint(db, handle).await {
        Ok(Some(snapshot)) => snapshot,
        _ => panic!("expected a current checkpoint"),
    }
}

/// `(confirmed, repost_attempts)` for one slot and side of a snapshot.
fn cell(cp: &GenerationCheckpoint, slot: usize, side: ArticleSide) -> (bool, u32) {
    let progress = cp.slot(slot, side).expect("slot inside the sealed plan");
    (progress.confirmed, progress.repost_attempts)
}

async fn revision(db: &Client, s: &Sealed) -> i64 {
    db.query_one(
        "SELECT revision FROM posted_file_health WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("read health revision")
    .get(0)
}

async fn health_round(db: &Client, s: &Sealed) -> i64 {
    db.query_one(
        "SELECT round FROM posted_file_health WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("read health round")
    .get(0)
}

/// Digest of every progress row (confirmations and repost counters).
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

async fn repost_sum(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT COALESCE(sum(repost_attempts), 0)::bigint \
         FROM posted_file_article_progress WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
}

async fn confirmed_rows(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_article_progress \
         WHERE generation_id = $1 AND confirmed",
        &[&s.generation],
    )
    .await
}

/// Everything repost accounting must leave alone unless it applied.
type Frozen = (Vec<Option<String>>, Option<String>);

async fn frozen(db: &Client, s: &Sealed) -> Frozen {
    (snapshot(db, s).await, progress_digest(db, s).await)
}

#[test]
fn postgres_repost_counts_first_second_and_cap_per_slot_and_side_without_confirming() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, mut handle) = begun(&mut db).await;
        let s = &c.sealed;

        let start = checkpoint(&mut db, &handle).await;
        for slot in 0..ARTICLES as usize {
            assert!(cell(&start, slot, ArticleSide::Post) == (false, 0));
            assert!(cell(&start, slot, ArticleSide::Verify) == (false, 0));
        }

        // Alternate answered outcomes: both count, neither confirms presence.
        for attempt in 1..=MAX_REPOST_ATTEMPTS_PER_ARTICLE {
            let before = revision(&db, s).await;
            let p = permit(&mut db, &handle, 1, ArticleSide::Post).await;
            let verdict = if attempt % 2 == 1 {
                PostVerdict::Posted
            } else {
                PostVerdict::DuplicatePresent
            };
            handle = record(&mut db, &p, verdict).await;
            assert_eq!(revision(&db, s).await, before + 1, "one revision per count");
            let cp = checkpoint(&mut db, &handle).await;
            assert!(cell(&cp, 1, ArticleSide::Post) == (false, attempt));
            assert!(
                cell(&cp, 1, ArticleSide::Verify) == (false, 0),
                "lanes are independent"
            );
            assert!(
                cell(&cp, 0, ArticleSide::Post) == (false, 0),
                "slots are independent"
            );
            assert!(
                cell(&cp, 2, ArticleSide::Post) == (false, 0),
                "slots are independent"
            );
            assert_eq!(
                confirmed_rows(&db, s).await,
                0,
                "a counted repost never confirms"
            );
        }

        // The cap holds the original: exhausted, no permit, no write.
        let before = frozen(&db, s).await;
        assert!(matches!(
            prepare_repost(&mut db, &handle, 1, ArticleSide::Post).await,
            Ok(RepostPreparation::BudgetExhausted)
        ));
        assert!(frozen(&db, s).await == before, "exhaustion must not mutate");
        assert_eq!(
            repost_sum(&db, s).await,
            i64::from(MAX_REPOST_ATTEMPTS_PER_ARTICLE)
        );

        // The other lane of the same slot still has its own full budget.
        let p = permit(&mut db, &handle, 1, ArticleSide::Verify).await;
        handle = record(&mut db, &p, PostVerdict::Posted).await;
        let cp = checkpoint(&mut db, &handle).await;
        assert!(cell(&cp, 1, ArticleSide::Verify) == (false, 1));
        assert!(cell(&cp, 1, ArticleSide::Post).1 == MAX_REPOST_ATTEMPTS_PER_ARTICLE);
        assert_eq!(confirmed_rows(&db, s).await, 0);
    });
}

#[test]
fn postgres_confirmed_slot_skips_repost_while_the_other_lane_stays_ready() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;

        let handle = applied(
            confirm_article(&mut db, &handle, 0, ArticleSide::Post).await,
            "confirm one lane",
        );
        let cp = checkpoint(&mut db, &handle).await;
        assert!(cell(&cp, 0, ArticleSide::Post) == (true, 0));
        assert!(cell(&cp, 0, ArticleSide::Verify) == (false, 0));

        let before = frozen(&db, s).await;
        let rev = revision(&db, s).await;
        assert!(matches!(
            prepare_repost(&mut db, &handle, 0, ArticleSide::Post).await,
            Ok(RepostPreparation::AlreadyConfirmed)
        ));
        // Preparing is a read: it neither writes nor advances the fence.
        let p = permit(&mut db, &handle, 0, ArticleSide::Verify).await;
        let _again = permit(&mut db, &handle, 0, ArticleSide::Verify).await;
        assert!(frozen(&db, s).await == before, "prepare must not mutate");
        assert_eq!(revision(&db, s).await, rev);
        assert!(posting_fence_is_current(&mut db, &handle)
            .await
            .expect("fence after prepare"));

        // Counting the open lane leaves the confirmed lane and its flag alone.
        let handle = record(&mut db, &p, PostVerdict::Posted).await;
        let cp = checkpoint(&mut db, &handle).await;
        assert!(cell(&cp, 0, ArticleSide::Post) == (true, 0));
        assert!(cell(&cp, 0, ArticleSide::Verify) == (false, 1));
        assert_eq!(confirmed_rows(&db, s).await, 1);
    });
}

#[test]
fn postgres_transport_and_refused_outcomes_leave_counts_health_and_permit_untouched() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        let p = permit(&mut db, &handle, 2, ArticleSide::Verify).await;

        let before = frozen(&db, s).await;
        let rev = revision(&db, s).await;
        for verdict in [PostVerdict::Transport, PostVerdict::Refused] {
            assert!(
                is_incomplete(&record_repost(&mut db, &p, verdict).await),
                "unanswered or refused outcomes are incomplete"
            );
        }
        assert!(frozen(&db, s).await == before, "no counter or health write");
        assert_eq!(revision(&db, s).await, rev, "no revision movement");
        assert_eq!(repost_sum(&db, s).await, 0, "no budget spent");
        let (state, verified): (String, Option<i64>) = {
            let row = db
                .query_one(
                    "SELECT state, verified_at FROM posted_file_health WHERE generation_id = $1",
                    &[&s.generation],
                )
                .await
                .quiet("read health outcome");
            (row.get(0), row.get(1))
        };
        assert_eq!(state, "verifying", "no negative health");
        assert!(verified.is_none());
        assert!(posting_fence_is_current(&mut db, &handle)
            .await
            .expect("fence after incomplete"));

        // The unspent permit still counts a later answered outcome once.
        let handle = record(&mut db, &p, PostVerdict::Posted).await;
        let cp = checkpoint(&mut db, &handle).await;
        assert!(cell(&cp, 2, ArticleSide::Verify) == (false, 1));
        assert_eq!(repost_sum(&db, s).await, 1);
    });
}

#[test]
fn postgres_duplicate_and_concurrent_callbacks_with_one_permit_count_once() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;

        // Sequential duplicate with the original permit.
        let p = permit(&mut db, &handle, 0, ArticleSide::Post).await;
        let fresh = record(&mut db, &p, PostVerdict::Posted).await;
        let before = frozen(&db, s).await;
        let rev = revision(&db, s).await;
        assert!(is_stale(
            &record_repost(&mut db, &p, PostVerdict::Posted).await
        ));
        assert!(is_stale(
            &record_repost(&mut db, &p, PostVerdict::DuplicatePresent).await
        ));
        assert!(frozen(&db, s).await == before, "duplicate must not mutate");
        assert_eq!(revision(&db, s).await, rev);
        assert!(cell(&checkpoint(&mut db, &fresh).await, 0, ArticleSide::Post) == (false, 1));

        // Two independent connections race one permit: one wins, one is stale.
        let p = permit(&mut db, &fresh, 1, ArticleSide::Verify).await;
        let mut left = connect().await;
        let mut right = connect().await;
        let (p_left, p_right) = (p.clone(), p);
        let barrier = Arc::new(Barrier::new(2));
        let (gate_left, gate_right) = (barrier.clone(), barrier);
        let (out_left, out_right) = tokio::join!(
            async {
                gate_left.wait().await;
                record_repost(&mut left, &p_left, PostVerdict::Posted).await
            },
            async {
                gate_right.wait().await;
                record_repost(&mut right, &p_right, PostVerdict::Posted).await
            }
        );
        let winner = match (out_left, out_right) {
            (Ok(MutationOutcome::Applied(h)), Ok(MutationOutcome::StaleOrDiverged))
            | (Ok(MutationOutcome::StaleOrDiverged), Ok(MutationOutcome::Applied(h))) => *h,
            _ => panic!("one concurrent callback must win the revision fence"),
        };
        let cp = checkpoint(&mut db, &winner).await;
        assert!(
            cell(&cp, 1, ArticleSide::Verify) == (false, 1),
            "race counts once"
        );
        assert_eq!(repost_sum(&db, s).await, 2, "one count per slot");
        assert_eq!(confirmed_rows(&db, s).await, 0);
        assert!(posting_fence_is_current(&mut db, &winner)
            .await
            .expect("winning fence"));
    });
}

#[test]
fn postgres_stale_round_revision_lease_job_and_attempt_fences_reject_repost_calls() {
    run_test(async {
        let mut db = connect().await;

        // A confirmation, a finished round and a new round each retire permits.
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        let on_confirm = permit(&mut db, &handle, 0, ArticleSide::Post).await;
        let on_round = permit(&mut db, &handle, 1, ArticleSide::Post).await;
        let mut chain = record(&mut db, &on_round, PostVerdict::Posted).await;
        assert!(
            is_stale(&record_repost(&mut db, &on_confirm, PostVerdict::Posted).await),
            "a revision moved by another slot retires the permit"
        );
        let on_round = permit(&mut db, &chain, 2, ArticleSide::Post).await;
        for slot in 0..ARTICLES {
            for side in [ArticleSide::Post, ArticleSide::Verify] {
                chain = applied(
                    confirm_article(&mut db, &chain, slot, side).await,
                    "confirm fixture article",
                );
            }
        }
        let finished = applied(
            finish_verification(&mut db, &chain).await,
            "finish first round",
        );
        let round_one = health_round(&db, s).await;
        let next = applied(
            begin_verification_round(&mut db, &finished).await,
            "begin second round",
        );
        assert!(health_round(&db, s).await > round_one);
        let before = frozen(&db, s).await;
        assert!(is_stale(
            &record_repost(&mut db, &on_round, PostVerdict::Posted).await
        ));
        assert!(prep_stale(
            &prepare_repost(&mut db, &chain, 0, ArticleSide::Post).await
        ));
        assert!(matches!(load_checkpoint(&mut db, &chain).await, Ok(None)));
        assert!(
            frozen(&db, s).await == before,
            "old-round calls must not mutate"
        );
        // Counters are scoped to the round: the new round starts at zero.
        let cp = checkpoint(&mut db, &next).await;
        assert!(cp.round() == health_round(&db, s).await);
        assert!(cell(&cp, 1, ArticleSide::Post) == (false, 0));
        assert!(cell(&cp, 2, ArticleSide::Post) == (false, 0));

        // Every lease, job and attempt fence refuses prepare, record and load.
        for label in [
            "expired attempt lease",
            "expired operation lease",
            "job status changed",
            "attempt superseded",
        ] {
            let (c, _attempt, handle) = begun(&mut db).await;
            let s = &c.sealed;
            let p = permit(&mut db, &handle, 0, ArticleSide::Verify).await;
            match label {
                "expired operation lease" => {
                    db.execute(
                        "UPDATE posted_file_operations SET lease_until = 0 WHERE id = $1",
                        &[&s.op],
                    )
                    .await
                    .quiet("expire operation");
                }
                "job status changed" => {
                    db.execute(
                        "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
                        &[&c.job],
                    )
                    .await
                    .quiet("change job status");
                }
                _ => {
                    db.execute(
                        "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
                        &[&c.job],
                    )
                    .await
                    .quiet("expire attempt");
                    if label == "attempt superseded" {
                        let _newer = acquire(&mut db, &c.job, &c.owner, 1).await;
                    }
                }
            }
            let before = frozen(&db, s).await;
            assert!(
                is_stale(&record_repost(&mut db, &p, PostVerdict::Posted).await),
                "{label}: record must be stale"
            );
            assert!(
                prep_stale(&prepare_repost(&mut db, &handle, 0, ArticleSide::Verify).await),
                "{label}: prepare must be stale"
            );
            assert!(
                matches!(load_checkpoint(&mut db, &handle).await, Ok(None)),
                "{label}: load must be stale"
            );
            assert!(
                frozen(&db, s).await == before,
                "{label}: fenced calls must not mutate"
            );
            assert_eq!(repost_sum(&db, s).await, 0, "{label}: budget changed");
        }
    });
}

#[test]
fn postgres_lost_ack_recovery_and_takeover_keep_counts_and_retire_old_permits() {
    run_test(async {
        let mut db = connect().await;

        // Lost acknowledgment: only the original permit and handle survive.
        let (c, attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        let p = permit(&mut db, &handle, 0, ArticleSide::Post).await;
        let _lost = record(&mut db, &p, PostVerdict::Posted).await;
        let (fresh, bundle) = match recover(&mut db, &attempt, &s.hash, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(h, b)) => (h, b),
            _ => panic!("recovery did not resume the candidate"),
        };
        let cp = checkpoint(&mut db, &fresh).await;
        assert!(
            cell(&cp, 0, ArticleSide::Post) == (false, 1),
            "persisted count survives"
        );
        assert_eq!(cp.generation_id(), bundle.generation_id());
        assert_eq!(cp.plan_sha256(), bundle.plan_sha256());
        let before = frozen(&db, s).await;
        assert!(is_stale(
            &record_repost(&mut db, &p, PostVerdict::Posted).await
        ));
        assert!(prep_stale(
            &prepare_repost(&mut db, &handle, 0, ArticleSide::Post).await
        ));
        assert!(matches!(load_checkpoint(&mut db, &handle).await, Ok(None)));
        assert!(
            frozen(&db, s).await == before,
            "replay must not count twice"
        );
        let p2 = permit(&mut db, &fresh, 0, ArticleSide::Post).await;
        let fresh = record(&mut db, &p2, PostVerdict::DuplicatePresent).await;
        assert!(cell(&checkpoint(&mut db, &fresh).await, 0, ArticleSide::Post) == (false, 2));

        // Same-candidate takeover preserves data; the old permit fails.
        let (c, _first, handle) = begun(&mut db).await;
        let s = &c.sealed;
        let p = permit(&mut db, &handle, 1, ArticleSide::Verify).await;
        let handle = record(&mut db, &p, PostVerdict::Posted).await;
        let old_permit = permit(&mut db, &handle, 1, ArticleSide::Verify).await;
        db.execute(
            "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
            &[&c.job],
        )
        .await
        .quiet("expire attempt");
        db.execute(
            "UPDATE posted_file_operations SET lease_until = 0 WHERE id = $1",
            &[&s.op],
        )
        .await
        .quiet("expire operation");
        let next = acquire(&mut db, &c.job, &c.owner, 1).await;
        let before = frozen(&db, s).await;
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");
        let (taken, _) = match take_over(&mut db, &next, &expected, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(h, b)) => (h, b),
            _ => panic!("takeover did not resume the candidate"),
        };
        let after = frozen(&db, s).await;
        assert!(after.1 == before.1, "takeover must keep every counter");
        for index in [0, 1, 3, 4] {
            assert!(
                after.0[index] == before.0[index],
                "takeover preserves custody and health"
            );
        }
        assert!(
            after.0[2] != before.0[2],
            "takeover rotates operation authority"
        );
        let before = after;
        assert!(is_stale(
            &record_repost(&mut db, &old_permit, PostVerdict::Posted).await
        ));
        assert!(prep_stale(
            &prepare_repost(&mut db, &handle, 1, ArticleSide::Verify).await
        ));
        assert!(matches!(load_checkpoint(&mut db, &handle).await, Ok(None)));
        assert!(cell(&checkpoint(&mut db, &taken).await, 1, ArticleSide::Verify) == (false, 1));
        assert!(frozen(&db, s).await == before, "old permit must not count");
        let p3 = permit(&mut db, &taken, 1, ArticleSide::Verify).await;
        let taken = record(&mut db, &p3, PostVerdict::Posted).await;
        assert!(cell(&checkpoint(&mut db, &taken).await, 1, ArticleSide::Verify) == (false, 2));
    });
}

#[test]
fn postgres_invalid_slots_and_unbegun_round_are_refused_and_snapshot_is_bound_and_redacted() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let h0 = admitted(&mut db, &attempt, s).await;

        // Round zero has no verification round to repost into.
        let before = frozen(&db, s).await;
        assert!(prep_stale(
            &prepare_repost(&mut db, &h0, 0, ArticleSide::Post).await
        ));
        assert!(frozen(&db, s).await == before);

        let handle = applied(
            begin_verification_round(&mut db, &h0).await,
            "begin first round",
        );
        let before = frozen(&db, s).await;
        let rev = revision(&db, s).await;
        for slot in [-1, i32::MIN, ARTICLES, 4, 1_000_000, i32::MAX] {
            for side in [ArticleSide::Post, ArticleSide::Verify] {
                assert!(
                    matches!(
                        prepare_repost(&mut db, &handle, slot, side).await,
                        Err(StoreError::InvalidInput)
                    ),
                    "out-of-range slot must be rejected"
                );
            }
        }
        assert!(
            frozen(&db, s).await == before,
            "invalid slots must not mutate"
        );
        assert_eq!(revision(&db, s).await, rev);

        // The rejected calls did not burn the handle; the last valid slot works.
        let p = permit(&mut db, &handle, ARTICLES - 1, ArticleSide::Post).await;
        assert!(
            format!("{p:?}") == "RepostPermit([REDACTED])",
            "permit debug must be redacted"
        );

        // Read-only snapshot bound to the stored generation, plan and round.
        let before = frozen(&db, s).await;
        let cp = checkpoint(&mut db, &handle).await;
        assert!(frozen(&db, s).await == before, "load must not mutate");
        assert_eq!(revision(&db, s).await, rev);
        assert_eq!(cp.generation_id(), s.generation);
        assert_eq!(cp.plan_sha256(), s.plan);
        assert!(cp.round() == health_round(&db, s).await);
        assert_eq!(cp.article_count(), ARTICLES as usize);
        for side in [ArticleSide::Post, ArticleSide::Verify] {
            assert!(cell(&cp, ARTICLES as usize - 1, side) == (false, 0));
            assert!(cp.slot(ARTICLES as usize, side).is_none());
            assert!(cp.slot(usize::MAX, side).is_none());
        }
        assert!(
            format!("{cp:?}") == "GenerationCheckpoint([REDACTED])",
            "checkpoint debug must be redacted"
        );
        // The pre-round handle is stale, never an empty checkpoint.
        assert!(matches!(load_checkpoint(&mut db, &h0).await, Ok(None)));
    });
}

#[test]
fn postgres_finished_verification_refuses_prepare_and_record_without_counting() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        let early = permit(&mut db, &handle, ARTICLES - 1, ArticleSide::Post).await;

        let mut chain = handle;
        for slot in 0..ARTICLES {
            for side in [ArticleSide::Post, ArticleSide::Verify] {
                chain = applied(
                    confirm_article(&mut db, &chain, slot, side).await,
                    "confirm fixture article",
                );
            }
        }
        let finished = applied(
            finish_verification(&mut db, &chain).await,
            "finish verification",
        );
        let verified: Option<i64> = db
            .query_one(
                "SELECT verified_at FROM posted_file_health WHERE generation_id = $1",
                &[&s.generation],
            )
            .await
            .quiet("read verified_at")
            .get(0);
        assert!(verified.is_some(), "fixture round must be finished");

        let before = frozen(&db, s).await;
        let rev = revision(&db, s).await;
        for slot in 0..ARTICLES {
            for side in [ArticleSide::Post, ArticleSide::Verify] {
                assert!(
                    prep_stale(&prepare_repost(&mut db, &finished, slot, side).await),
                    "finished evidence must not authorize a repost"
                );
            }
        }
        assert!(is_stale(
            &record_repost(&mut db, &early, PostVerdict::Posted).await
        ));
        assert!(frozen(&db, s).await == before, "refusals must not mutate");
        assert_eq!(revision(&db, s).await, rev);
        assert_eq!(repost_sum(&db, s).await, 0, "no repost was counted");
        assert_eq!(confirmed_rows(&db, s).await, i64::from(ARTICLES) * 2);
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
    });
}
