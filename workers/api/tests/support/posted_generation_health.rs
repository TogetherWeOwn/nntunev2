//! Real-PostgreSQL tests for the fenced verification-round adapter
//! (`begin_verification_round`, `confirm_article`, `finish_verification`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `acquire`, `admitted`,
//! `count`, `snapshot`, `recover`). Fixtures stay synthetic sealed byte
//! buffers keyed by database UUIDs: no article identities, keys or names.
//! Failures print only static labels; outcomes are matched, never formatted,
//! so no assertion can echo handles, tokens or encrypted material.
//!
//! Scope: verification evidence only. A finished round stamps `verified_at`
//! and nothing else; these tests never expect a usable state, a retention
//! stamp or a registry selection.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, confirm_article, finish_verification, ArticleSide, GenerationHandle,
    MutationOutcome,
};
use std::sync::Arc;
use tokio::sync::Barrier;

/// Slots addressable by the three-article fixture (`article_count = 3`).
const SLOTS: [i32; 3] = [0, 1, 2];

fn side(post: bool) -> ArticleSide {
    if post {
        ArticleSide::Post
    } else {
        ArticleSide::Verify
    }
}

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

struct Health {
    revision: i64,
    round: i64,
    token: String,
    state: String,
    verified_at: Option<i64>,
    retention_checked_at: Option<i64>,
}

async fn health(db: &Client, s: &Sealed) -> Health {
    let row = db
        .query_one(
            "SELECT revision, round, round_token, state, verified_at, retention_checked_at \
             FROM posted_file_health WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("read health row");
    Health {
        revision: row.get(0),
        round: row.get(1),
        token: row.get(2),
        state: row.get(3),
        verified_at: row.get(4),
        retention_checked_at: row.get(5),
    }
}

/// Confirmed progress rows across every round, retained by design.
async fn progress(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_article_progress \
         WHERE generation_id = $1 AND confirmed",
        &[&s.generation],
    )
    .await
}

/// Confirmed progress rows for the round currently stored in health.
async fn current_progress(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_article_progress p \
         JOIN posted_file_health h ON h.generation_id = p.generation_id AND h.round = p.round \
         WHERE p.generation_id = $1 AND p.confirmed",
        &[&s.generation],
    )
    .await
}

/// Admitted candidate with a freshly begun verification round.
async fn verifying(db: &mut Client) -> (Case, AttemptHandle, GenerationHandle) {
    let c = case(db).await;
    let attempt = acquire(db, &c.job, &c.owner, 0).await;
    let admitted_handle = admitted(db, &attempt, &c.sealed).await;
    let begun = applied(
        begin_verification_round(db, &admitted_handle).await,
        "begin first round",
    );
    (c, attempt, begun)
}

/// Confirm all three slots on both sides, chaining the refreshed handles.
async fn confirm_all(db: &mut Client, mut handle: GenerationHandle) -> GenerationHandle {
    for slot in SLOTS {
        for post in [true, false] {
            handle = applied(
                confirm_article(db, &handle, slot, side(post)).await,
                "confirm fixture article",
            );
        }
    }
    handle
}

/// Every mutation with `handle`, expecting each to be rejected as stale.
async fn assert_all_stale(db: &mut Client, handle: &GenerationHandle, label: &'static str) {
    assert!(
        is_stale(&confirm_article(db, handle, 0, ArticleSide::Post).await),
        "{label}: confirm must be stale"
    );
    assert!(
        is_stale(&confirm_article(db, handle, 2, ArticleSide::Verify).await),
        "{label}: confirm must be stale"
    );
    assert!(
        is_stale(&finish_verification(db, handle).await),
        "{label}: finish must be stale"
    );
    assert!(
        is_stale(&begin_verification_round(db, handle).await),
        "{label}: begin must be stale"
    );
}

#[test]
fn postgres_finish_requires_every_slot_on_both_sides_and_only_stamps_verified_at() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;

        let started = health(&db, s).await;
        assert_eq!(started.state, "verifying");
        assert!(started.verified_at.is_none());
        assert!(started.retention_checked_at.is_none());

        // Nothing confirmed yet.
        let before = snapshot(&db, s).await;
        assert!(
            is_incomplete(&finish_verification(&mut db, &begun).await),
            "empty round must be incomplete"
        );
        assert!(
            snapshot(&db, s).await == before,
            "incomplete finish must not mutate"
        );

        // Post side complete, verify side empty.
        let mut handle = begun;
        for slot in SLOTS {
            handle = applied(
                confirm_article(&mut db, &handle, slot, ArticleSide::Post).await,
                "confirm post side",
            );
        }
        assert_eq!(current_progress(&db, s).await, 3);
        let before = snapshot(&db, s).await;
        assert!(
            is_incomplete(&finish_verification(&mut db, &handle).await),
            "one side only must be incomplete"
        );
        assert!(
            snapshot(&db, s).await == before,
            "one-sided finish must not mutate"
        );

        // Verify side missing slot 2.
        for slot in [0, 1] {
            handle = applied(
                confirm_article(&mut db, &handle, slot, ArticleSide::Verify).await,
                "confirm verify side",
            );
        }
        assert_eq!(current_progress(&db, s).await, 5);
        let before = snapshot(&db, s).await;
        assert!(
            is_incomplete(&finish_verification(&mut db, &handle).await),
            "a missing slot must be incomplete"
        );
        assert!(
            snapshot(&db, s).await == before,
            "missing-slot finish must not mutate"
        );
        assert!(health(&db, s).await.verified_at.is_none());

        // The last slot completes the set.
        handle = applied(
            confirm_article(&mut db, &handle, 2, ArticleSide::Verify).await,
            "confirm last article",
        );
        assert_eq!(current_progress(&db, s).await, 6);
        let before = snapshot(&db, s).await;
        let pre = health(&db, s).await;
        let finished = applied(
            finish_verification(&mut db, &handle).await,
            "finish complete set",
        );
        let post = health(&db, s).await;
        assert!(post.verified_at.is_some(), "finish must stamp verified_at");
        assert_eq!(post.state, "verifying", "finish must not mark usable");
        assert!(
            post.retention_checked_at.is_none(),
            "finish must not stamp retention"
        );
        assert!(post.revision > pre.revision, "finish must refresh revision");
        assert_eq!(post.round, pre.round);
        assert!(post.token == pre.token);
        assert!(!posting_fence_is_current(&mut db, &finished)
            .await
            .expect("finished evidence must not authorize more posting"));

        // Only health moved: custody, registry selection and references hold.
        let after = snapshot(&db, s).await;
        assert!(after[0] == before[0], "custody row must not change");
        assert!(after[1] == before[1], "registry must not be selected");
        assert!(after[4] == before[4], "references must not change");
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_registry \
                 WHERE plaintext_sha256 = $1 AND selected_generation IS NOT NULL",
                &[&s.hash]
            )
            .await,
            0
        );
        assert_eq!(progress(&db, s).await, 6);

        // Any confirm after the round finished is stale.
        let before = snapshot(&db, s).await;
        assert!(is_stale(
            &confirm_article(&mut db, &finished, 0, ArticleSide::Post).await
        ));
        assert!(is_stale(
            &confirm_article(&mut db, &handle, 0, ArticleSide::Post).await
        ));
        assert!(
            snapshot(&db, s).await == before,
            "late confirm must not mutate"
        );
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
    });
}

#[test]
fn postgres_duplicate_and_concurrent_confirmations_are_idempotent_per_slot_and_side() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;

        // Sequential duplicate through the refreshed handle.
        let first = applied(
            confirm_article(&mut db, &begun, 0, ArticleSide::Post).await,
            "first confirm",
        );
        assert_eq!(progress(&db, s).await, 1);
        let before = health(&db, s).await;
        let snap = snapshot(&db, s).await;
        let again = applied(
            confirm_article(&mut db, &first, 0, ArticleSide::Post).await,
            "duplicate confirm",
        );
        let after = health(&db, s).await;
        assert_eq!(after.revision, before.revision, "duplicate keeps revision");
        assert_eq!(progress(&db, s).await, 1, "duplicate must not inflate");
        assert!(snapshot(&db, s).await == snap, "duplicate must not mutate");
        assert!(posting_fence_is_current(&mut db, &again)
            .await
            .expect("fence after duplicate"));

        // The same slot on the other side is independent evidence.
        let other_side = applied(
            confirm_article(&mut db, &again, 0, ArticleSide::Verify).await,
            "other side confirm",
        );
        assert_eq!(progress(&db, s).await, 2);

        // Two independent connections race one identical callback.
        let mut left = connect().await;
        let mut right = connect().await;
        let handle_left = other_side.clone();
        let handle_right = other_side;
        let barrier = Arc::new(Barrier::new(2));
        let (gate_left, gate_right) = (barrier.clone(), barrier);
        let (out_left, out_right) = tokio::join!(
            async {
                gate_left.wait().await;
                confirm_article(&mut left, &handle_left, 1, ArticleSide::Post).await
            },
            async {
                gate_right.wait().await;
                confirm_article(&mut right, &handle_right, 1, ArticleSide::Post).await
            }
        );
        // One revision wins. The other callback is stale, even when it would
        // confirm the same slot. It must recover before retrying idempotently.
        let winner = match (out_left, out_right) {
            (Ok(MutationOutcome::Applied(h)), Ok(MutationOutcome::StaleOrDiverged))
            | (Ok(MutationOutcome::StaleOrDiverged), Ok(MutationOutcome::Applied(h))) => h,
            _ => panic!("one concurrent callback must win the revision fence"),
        };
        assert_eq!(progress(&db, s).await, 3, "race must record one row");
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_article_progress \
                 WHERE generation_id = $1 AND slot = 1 AND side = 0",
                &[&s.generation]
            )
            .await,
            1
        );
        assert!(posting_fence_is_current(&mut db, &winner)
            .await
            .expect("winning fence"));
    });
}

#[test]
fn postgres_new_round_rotates_token_and_rejects_every_old_health_callback() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let h0 = admitted(&mut db, &attempt, s).await;
        let admitted_health = health(&db, s).await;

        let h1 = applied(
            begin_verification_round(&mut db, &h0).await,
            "begin first round",
        );
        let first = health(&db, s).await;
        assert!(first.round > admitted_health.round, "begin advances round");
        assert!(first.token != admitted_health.token, "begin rotates token");
        assert_eq!(first.state, "verifying");
        assert!(first.revision > admitted_health.revision);

        // The admission handle is dead once the round starts.
        let before = snapshot(&db, s).await;
        assert_all_stale(&mut db, &h0, "admission handle").await;
        assert!(snapshot(&db, s).await == before);
        assert!(!posting_fence_is_current(&mut db, &h0)
            .await
            .expect("old fence"));

        // Complete and finish round one.
        let h1_full = confirm_all(&mut db, h1.clone()).await;
        let h1_done = applied(
            finish_verification(&mut db, &h1_full).await,
            "finish first round",
        );
        assert!(health(&db, s).await.verified_at.is_some());
        assert_eq!(progress(&db, s).await, 6);

        // A second round clears timestamps and keeps old progress.
        let h2 = applied(
            begin_verification_round(&mut db, &h1_done).await,
            "begin second round",
        );
        let second = health(&db, s).await;
        assert!(second.round > first.round);
        assert!(second.token != first.token);
        assert_eq!(second.state, "verifying");
        assert!(second.verified_at.is_none(), "begin clears verified_at");
        assert!(
            second.retention_checked_at.is_none(),
            "begin clears retention"
        );
        assert_eq!(progress(&db, s).await, 6, "old progress is retained");
        assert_eq!(current_progress(&db, s).await, 0);

        // Every earlier handle, in any order, is rejected without mutation.
        let before = snapshot(&db, s).await;
        for (label, old) in [
            ("round-one handle", &h1_done),
            ("round-one completed handle", &h1_full),
            ("round-one start handle", &h1),
            ("admission handle", &h0),
        ] {
            assert_all_stale(&mut db, old, label).await;
        }
        assert!(
            snapshot(&db, s).await == before,
            "old callbacks must not change the snapshot"
        );
        assert_eq!(progress(&db, s).await, 6);

        // Old-round evidence never satisfies the new round.
        assert!(
            is_incomplete(&finish_verification(&mut db, &h2).await),
            "new round starts empty"
        );
        assert!(snapshot(&db, s).await == before);

        // The current handle still works, and old ones stay dead afterwards.
        let h2b = applied(
            confirm_article(&mut db, &h2, 0, ArticleSide::Post).await,
            "confirm in new round",
        );
        assert_eq!(current_progress(&db, s).await, 1);
        assert_all_stale(&mut db, &h1_done, "round-one handle after new confirm").await;
        assert!(posting_fence_is_current(&mut db, &h2b)
            .await
            .expect("current fence"));
    });
}

#[test]
fn postgres_lease_status_and_attempt_fences_gate_every_health_mutation() {
    run_test(async {
        let mut db = connect().await;

        for label in [
            "expired attempt lease",
            "expired operation lease",
            "job status changed",
            "attempt superseded",
        ] {
            let (c, _attempt, begun) = verifying(&mut db).await;
            let s = &c.sealed;
            // A complete set means finish would succeed if the fence held.
            let full = confirm_all(&mut db, begun).await;
            match label {
                "expired attempt lease" => {
                    db.execute(
                        "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
                        &[&c.job],
                    )
                    .await
                    .quiet("expire attempt");
                }
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
                    let _newer = acquire(&mut db, &c.job, &c.owner, 1).await;
                }
            }
            let before = snapshot(&db, s).await;
            let rows = progress(&db, s).await;
            assert_all_stale(&mut db, &full, label).await;
            assert!(
                snapshot(&db, s).await == before,
                "{label}: fenced callbacks must not mutate"
            );
            assert_eq!(progress(&db, s).await, rows, "{label}: progress changed");
            assert!(
                health(&db, s).await.verified_at.is_none(),
                "{label}: verified_at must stay unset"
            );
        }
    });
}

#[test]
fn postgres_lost_ack_recovery_returns_current_health_revision_with_exact_evidence() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let h0 = admitted(&mut db, &attempt, s).await;

        // The begin acknowledgment is lost: only the stale handle survives.
        let _lost = applied(
            begin_verification_round(&mut db, &h0).await,
            "begin with lost ack",
        );
        let before = snapshot(&db, s).await;
        let health_before = health(&db, s).await;
        let recovered = match recover(&mut db, &attempt, &s.hash, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(handle, bundle)) => {
                assert!(
                    bundle.custody().expect("stored custody") == s.custody(),
                    "recovered custody must be byte-exact"
                );
                assert_eq!(bundle.generation_id(), s.generation);
                assert_eq!(bundle.plan_sha256(), s.plan);
                handle
            }
            _ => panic!("recovery did not resume the candidate"),
        };
        assert!(snapshot(&db, s).await == before, "recovery must not mutate");
        assert_eq!(health(&db, s).await.revision, health_before.revision);
        assert!(posting_fence_is_current(&mut db, &recovered)
            .await
            .expect("recovered fence"));
        assert!(!posting_fence_is_current(&mut db, &h0)
            .await
            .expect("pre-begin fence"));
        assert!(is_stale(
            &confirm_article(&mut db, &h0, 0, ArticleSide::Post).await
        ));

        // Work continues through the recovered handle; a lost confirm ack
        // is recovered again and sees the same evidence.
        let _lost = applied(
            confirm_article(&mut db, &recovered, 0, ArticleSide::Post).await,
            "confirm with lost ack",
        );
        let recovered = match recover(&mut db, &attempt, &s.hash, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(handle, _)) => handle,
            _ => panic!("second recovery did not resume the candidate"),
        };
        assert_eq!(progress(&db, s).await, 1);
        let full = confirm_all(&mut db, recovered).await;
        let finished = applied(
            finish_verification(&mut db, &full).await,
            "finish after recovery",
        );
        let done = health(&db, s).await;
        assert!(done.verified_at.is_some());
        assert_eq!(done.state, "verifying");

        // Lost finish ack: recovery hands back the post-finish revision, and
        // that handle cannot confirm into the finished round.
        let before = snapshot(&db, s).await;
        let recovered = match recover(&mut db, &attempt, &s.hash, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(handle, bundle)) => {
                assert!(bundle.custody().expect("stored custody") == s.custody());
                handle
            }
            _ => panic!("recovery after finish did not resume the candidate"),
        };
        assert!(snapshot(&db, s).await == before);
        assert!(!posting_fence_is_current(&mut db, &recovered)
            .await
            .expect("finished recovery is not posting authority"));
        assert!(is_stale(
            &confirm_article(&mut db, &recovered, 1, ArticleSide::Post).await
        ));
        assert!(is_stale(
            &confirm_article(&mut db, &finished, 1, ArticleSide::Post).await
        ));
        assert!(snapshot(&db, s).await == before);
        assert_eq!(progress(&db, s).await, 6);
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
    });
}

#[test]
fn postgres_invalid_slots_are_rejected_without_mutation_or_burning_the_handle() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let before = snapshot(&db, s).await;
        let health_before = health(&db, s).await;

        // 3 is article_count and therefore excluded.
        for slot in [-1, i32::MIN, 3, 4, 999_999, 1_000_000, i32::MAX] {
            for post in [true, false] {
                let outcome = confirm_article(&mut db, &begun, slot, side(post)).await;
                assert!(
                    matches!(outcome, Err(StoreError::InvalidInput)),
                    "out-of-range slot must be rejected"
                );
            }
        }
        assert!(
            snapshot(&db, s).await == before,
            "invalid slots must not mutate"
        );
        assert_eq!(progress(&db, s).await, 0);
        let health_after = health(&db, s).await;
        assert_eq!(health_after.revision, health_before.revision);
        assert!(health_after.token == health_before.token);

        // The rejected calls left the handle usable for the highest valid slot.
        let handle = applied(
            confirm_article(&mut db, &begun, 2, ArticleSide::Post).await,
            "valid slot after rejections",
        );
        assert_eq!(progress(&db, s).await, 1);
        assert!(posting_fence_is_current(&mut db, &handle)
            .await
            .expect("fence after valid slot"));
    });
}
