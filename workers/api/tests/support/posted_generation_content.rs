//! Real-PostgreSQL tests for the fenced content-check evidence adapter
//! (`prepare_content_check`, `record_content_check`) and the checkpoint's
//! `content_check` read.
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `acquire`, `admitted`,
//! `observe`, `count`, `snapshot`, `recover`, `run_blocking`, `MIGRATION_0039`).
//! Fixtures stay synthetic sealed byte buffers keyed by database UUIDs: no
//! article identities, keys, names or NZB material. Failures print only static
//! labels; outcomes are matched, never formatted, so no assertion can echo
//! handles, permits, digests or encrypted material.
//!
//! Scope: content evidence only. A content result is stored per round and
//! side and never grants usability, retention, selection, repost, terminal
//! health or cleanup authority; these tests never expect any of those.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, confirm_article, finish_verification, load_checkpoint,
    prepare_content_check, record_content_check, take_over, ArticleSide, ContentCheckPermit,
    ContentCheckReport, ContentCheckResult, ContentCheckState, GenerationHandle, MutationOutcome,
};
use std::sync::Arc;
use tokio::sync::Barrier;

#[path = "posted_generation_content_driver.rs"]
mod driver;

const SLOTS: [i32; 3] = [0, 1, 2];

fn content_side(post: bool) -> ArticleSide {
    if post {
        ArticleSide::Post
    } else {
        ArticleSide::Verify
    }
}

/// Sealed fixture lanes: post is `pool-a`, verify is `pool-b`.
fn lane(post: bool) -> &'static str {
    if post {
        "pool-a"
    } else {
        "pool-b"
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

/// An exact, matching report for the sealed fixture and lane.
fn report(s: &Sealed, post: bool) -> ContentCheckReport<'_> {
    ContentCheckReport {
        backbone: lane(post),
        plan_sha256: &s.plan,
        plaintext_sha256: &s.hash,
        plaintext_size: 1024,
        checked_articles: 3,
    }
}

fn intact(s: &Sealed, post: bool) -> ContentCheckResult<'_> {
    ContentCheckResult::Intact(report(s, post))
}

fn nonaffirmative(kind: ContentCheckState) -> ContentCheckResult<'static> {
    match kind {
        ContentCheckState::Unavailable => ContentCheckResult::Unavailable,
        ContentCheckState::AuthenticationFailed => ContentCheckResult::AuthenticationFailed,
        ContentCheckState::IntegrityFailed => ContentCheckResult::IntegrityFailed,
        ContentCheckState::Intact => panic!("intact is not a nonaffirmative result"),
    }
}

async fn permit(db: &mut Client, handle: &GenerationHandle, post: bool) -> ContentCheckPermit {
    match prepare_content_check(db, handle, content_side(post)).await {
        Ok(Some(permit)) => permit,
        _ => panic!("expected a content-check permit"),
    }
}

async fn no_permit(db: &mut Client, handle: &GenerationHandle, post: bool) -> bool {
    matches!(
        prepare_content_check(db, handle, content_side(post)).await,
        Ok(None)
    )
}

/// Current-round state for one side, as seen through a fresh checkpoint.
async fn content_state(
    db: &mut Client,
    handle: &GenerationHandle,
    post: bool,
) -> Option<ContentCheckState> {
    match load_checkpoint(db, handle).await {
        Ok(Some(checkpoint)) => checkpoint.content_check(content_side(post)),
        _ => panic!("expected a current checkpoint"),
    }
}

/// Stored evidence rows for the generation across every round.
async fn content_rows(db: &Client, s: &Sealed) -> Vec<(i64, String, i32)> {
    db.query(
        "SELECT round::int8, backbone::text, checked_articles::int4 \
         FROM posted_file_content_checks WHERE generation_id = $1 ORDER BY round, backbone",
        &[&s.generation],
    )
    .await
    .quiet("read content rows")
    .iter()
    .map(|row| (row.get(0), row.get(1), row.get(2)))
    .collect()
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

/// Everything content evidence must never move: terminal health, the repost
/// budget, retention, selection, custody, references and confirmed progress.
#[derive(PartialEq)]
struct Guard {
    state: String,
    verified_at: Option<i64>,
    retention_checked_at: Option<i64>,
    round: i64,
    repost_budget: i64,
    confirmed: i64,
    selected: i64,
    custody: Option<String>,
    registry: Option<String>,
    references: Option<String>,
}

async fn guard(db: &Client, s: &Sealed) -> Guard {
    let row = db
        .query_one(
            "SELECT state, verified_at, retention_checked_at, round \
             FROM posted_file_health WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("read guarded health");
    let snap = snapshot(db, s).await;
    Guard {
        state: row.get(0),
        verified_at: row.get(1),
        retention_checked_at: row.get(2),
        round: row.get(3),
        repost_budget: count(
            db,
            "SELECT coalesce(sum(repost_attempts), 0)::int8 \
             FROM posted_file_article_progress WHERE generation_id = $1",
            &[&s.generation],
        )
        .await,
        confirmed: count(
            db,
            "SELECT count(*) FROM posted_file_article_progress \
             WHERE generation_id = $1 AND confirmed",
            &[&s.generation],
        )
        .await,
        selected: count(
            db,
            "SELECT count(*) FROM posted_file_registry \
             WHERE plaintext_sha256 = $1 AND selected_generation IS NOT NULL",
            &[&s.hash],
        )
        .await,
        custody: snap[0].clone(),
        registry: snap[1].clone(),
        references: snap[4].clone(),
    }
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

/// Confirm every slot on both sides, then finish the presence round.
async fn finished(db: &mut Client, mut handle: GenerationHandle) -> GenerationHandle {
    for slot in SLOTS {
        for post in [true, false] {
            handle = applied(
                confirm_article(db, &handle, slot, content_side(post)).await,
                "confirm fixture article",
            );
        }
    }
    applied(finish_verification(db, &handle).await, "finish presence")
}

async fn fresh_handle(db: &mut Client, attempt: &AttemptHandle, s: &Sealed) -> GenerationHandle {
    match recover(db, attempt, &s.hash, &s.op).await {
        Ok(AdmissionOutcome::ResumedSameCandidate(handle, _)) => handle,
        _ => panic!("recovery did not resume the candidate"),
    }
}

#[test]
fn postgres_presence_finish_alone_proves_no_content_and_two_intact_reports_store_only_evidence() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;

        // A permit is available as soon as a round has begun, before presence
        // is finished, and preparing it writes nothing.
        let before = snapshot(&db, s).await;
        assert!(!no_permit(&mut db, &begun, true).await);
        assert!(!no_permit(&mut db, &begun, false).await);
        assert!(snapshot(&db, s).await == before, "prepare must not mutate");
        assert!(content_rows(&db, s).await.is_empty());

        // Finishing presence stores no content evidence of any kind.
        let done = finished(&mut db, begun).await;
        assert!(content_rows(&db, s).await.is_empty());
        assert_eq!(content_state(&mut db, &done, true).await, None);
        assert_eq!(content_state(&mut db, &done, false).await, None);
        let held = guard(&db, s).await;
        assert!(
            held.verified_at.is_some(),
            "presence finish stamps verified_at"
        );
        assert_eq!(held.state, "verifying");

        // Prepare is also available after presence finish, still writing nothing.
        let before = snapshot(&db, s).await;
        let post_permit = permit(&mut db, &done, true).await;
        assert_eq!(format!("{post_permit:?}"), "ContentCheckPermit([REDACTED])");
        assert!(snapshot(&db, s).await == before);

        // Each applied report refreshes the handle; the second side uses it.
        let after_post = applied(
            record_content_check(&mut db, &post_permit, intact(s, true)).await,
            "record post content",
        );
        assert_eq!(content_rows(&db, s).await.len(), 1);
        assert_eq!(
            content_state(&mut db, &after_post, true).await,
            Some(ContentCheckState::Intact)
        );
        assert_eq!(content_state(&mut db, &after_post, false).await, None);
        let verify_permit = permit(&mut db, &after_post, false).await;
        let after_both = applied(
            record_content_check(&mut db, &verify_permit, intact(s, false)).await,
            "record verify content",
        );
        let rows = content_rows(&db, s).await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.2 == 3));
        assert!(rows.iter().any(|row| row.1 == lane(true)));
        assert!(rows.iter().any(|row| row.1 == lane(false)));
        assert_eq!(
            content_state(&mut db, &after_both, true).await,
            Some(ContentCheckState::Intact)
        );
        assert_eq!(
            content_state(&mut db, &after_both, false).await,
            Some(ContentCheckState::Intact)
        );

        // Two intact reports are stored evidence only: not usable, not
        // retained, not selected, no repost or progress movement.
        assert!(
            guard(&db, s).await == held,
            "content must grant no authority"
        );
        assert!(!posting_fence_is_current(&mut db, &after_both)
            .await
            .expect("fence after content"));
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
    });
}

#[test]
fn postgres_mismatched_reports_are_rejected_without_mutation_or_burning_the_permit() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let wrong_hash = digest(&db, "wrong-content", &c.uuid).await;
        let wrong_plan = digest(&db, "wrong-plan", &c.uuid).await;
        let post_permit = permit(&mut db, &begun, true).await;
        let before = snapshot(&db, s).await;
        let before_guard = guard(&db, s).await;

        let cases: Vec<(&'static str, ContentCheckReport<'_>)> = vec![
            (
                "wrong plaintext digest",
                ContentCheckReport {
                    plaintext_sha256: &wrong_hash,
                    ..report(s, true)
                },
            ),
            (
                "wrong plan digest",
                ContentCheckReport {
                    plan_sha256: &wrong_plan,
                    ..report(s, true)
                },
            ),
            (
                "plan swapped with content digest",
                ContentCheckReport {
                    plan_sha256: &s.hash,
                    plaintext_sha256: &s.plan,
                    ..report(s, true)
                },
            ),
            (
                "short size",
                ContentCheckReport {
                    plaintext_size: 1023,
                    ..report(s, true)
                },
            ),
            (
                "long size",
                ContentCheckReport {
                    plaintext_size: 1025,
                    ..report(s, true)
                },
            ),
            (
                "zero size",
                ContentCheckReport {
                    plaintext_size: 0,
                    ..report(s, true)
                },
            ),
            (
                "negative size",
                ContentCheckReport {
                    plaintext_size: -1,
                    ..report(s, true)
                },
            ),
            (
                "too few articles",
                ContentCheckReport {
                    checked_articles: 2,
                    ..report(s, true)
                },
            ),
            (
                "too many articles",
                ContentCheckReport {
                    checked_articles: 4,
                    ..report(s, true)
                },
            ),
            (
                "negative articles",
                ContentCheckReport {
                    checked_articles: -1,
                    ..report(s, true)
                },
            ),
            (
                "other side's backbone",
                ContentCheckReport {
                    backbone: lane(false),
                    ..report(s, true)
                },
            ),
            (
                "unknown backbone",
                ContentCheckReport {
                    backbone: "pool-c",
                    ..report(s, true)
                },
            ),
            (
                "empty backbone",
                ContentCheckReport {
                    backbone: "",
                    ..report(s, true)
                },
            ),
            (
                "uppercase backbone",
                ContentCheckReport {
                    backbone: "POOL-A",
                    ..report(s, true)
                },
            ),
            (
                "empty digests",
                ContentCheckReport {
                    plan_sha256: "",
                    plaintext_sha256: "",
                    ..report(s, true)
                },
            ),
        ];
        for (label, bad) in cases {
            let outcome =
                record_content_check(&mut db, &post_permit, ContentCheckResult::Intact(bad)).await;
            assert!(
                matches!(outcome, Err(StoreError::InvalidInput)),
                "{label}: malformed proof must be invalid input"
            );
            assert!(
                snapshot(&db, s).await == before,
                "{label}: rejected report must not mutate"
            );
            assert!(
                content_rows(&db, s).await.is_empty(),
                "{label}: row written"
            );
        }
        assert!(guard(&db, s).await == before_guard);

        // The permit is not burned: the exact report still applies once.
        let handle = applied(
            record_content_check(&mut db, &post_permit, intact(s, true)).await,
            "exact report after rejections",
        );
        assert_eq!(content_rows(&db, s).await.len(), 1);
        assert_eq!(
            content_state(&mut db, &handle, true).await,
            Some(ContentCheckState::Intact)
        );
    });
}

#[test]
fn postgres_same_permit_race_applies_once_and_lost_ack_recovery_is_idempotent_with_fresh_permits() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let original = permit(&mut db, &begun, true).await;

        // Two independent sessions race one identical callback.
        let mut left = connect().await;
        let mut right = connect().await;
        let (permit_left, permit_right) = (original.clone(), original.clone());
        let barrier = Arc::new(Barrier::new(2));
        let (gate_left, gate_right) = (barrier.clone(), barrier);
        let (out_left, out_right) = tokio::join!(
            async {
                gate_left.wait().await;
                record_content_check(&mut left, &permit_left, intact(s, true)).await
            },
            async {
                gate_right.wait().await;
                record_content_check(&mut right, &permit_right, intact(s, true)).await
            }
        );
        let winner = match (out_left, out_right) {
            (Ok(MutationOutcome::Applied(h)), Ok(MutationOutcome::StaleOrDiverged))
            | (Ok(MutationOutcome::StaleOrDiverged), Ok(MutationOutcome::Applied(h))) => *h,
            _ => panic!("exactly one concurrent callback must apply"),
        };
        assert_eq!(
            content_rows(&db, s).await.len(),
            1,
            "race must store one row"
        );
        assert_eq!(
            content_state(&mut db, &winner, true).await,
            Some(ContentCheckState::Intact)
        );

        // Lost acknowledgment: only the stale handle survives. Recovery hands
        // back the current revision, and a fresh permit repeats the same
        // result idempotently, with no revision bump and no extra row.
        drop(winner);
        let revision = health_revision(&db, s).await;
        let before = snapshot(&db, s).await;
        let rows = content_rows(&db, s).await;
        let recovered = fresh_handle(&mut db, &attempt, s).await;
        assert!(snapshot(&db, s).await == before, "recovery must not mutate");
        let fresh = permit(&mut db, &recovered, true).await;
        let replayed = applied(
            record_content_check(&mut db, &fresh, intact(s, true)).await,
            "fresh permit replay",
        );
        assert_eq!(
            health_revision(&db, s).await,
            revision,
            "replay bumps nothing"
        );
        assert!(snapshot(&db, s).await == before, "replay must not mutate");
        assert!(content_rows(&db, s).await == rows);
        assert!(!no_permit(&mut db, &replayed, true).await);

        // The original callback, replayed after the write, is stale.
        assert!(is_stale(
            &record_content_check(&mut db, &original, intact(s, true)).await
        ));
        assert!(snapshot(&db, s).await == before);
        assert!(content_rows(&db, s).await == rows);
        assert_eq!(health_revision(&db, s).await, revision);
    });
}

#[test]
fn postgres_nonaffirmative_results_replace_intact_without_health_budget_or_cleanup_writes() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let mut handle = finished(&mut db, begun).await;
        let held = guard(&db, s).await;

        // A failure with no prior row creates evidence on that side only.
        let p = permit(&mut db, &handle, false).await;
        handle = applied(
            record_content_check(&mut db, &p, nonaffirmative(ContentCheckState::Unavailable)).await,
            "unavailable without prior row",
        );
        assert_eq!(
            content_state(&mut db, &handle, false).await,
            Some(ContentCheckState::Unavailable)
        );
        assert_eq!(content_state(&mut db, &handle, true).await, None);

        // Intact on both sides, then every failure kind replaces it on one.
        for post in [false, true] {
            let p = permit(&mut db, &handle, post).await;
            handle = applied(
                record_content_check(&mut db, &p, intact(s, post)).await,
                "intact report",
            );
        }
        for kind in [
            ContentCheckState::Unavailable,
            ContentCheckState::AuthenticationFailed,
            ContentCheckState::IntegrityFailed,
            ContentCheckState::Unavailable,
        ] {
            let p = permit(&mut db, &handle, true).await;
            handle = applied(
                record_content_check(&mut db, &p, nonaffirmative(kind)).await,
                "nonaffirmative report",
            );
            assert_eq!(content_state(&mut db, &handle, true).await, Some(kind));
            assert_eq!(
                content_state(&mut db, &handle, false).await,
                Some(ContentCheckState::Intact),
                "the other side's evidence is independent"
            );
            assert_eq!(content_rows(&db, s).await.len(), 2, "one row per side");
            assert!(
                guard(&db, s).await == held,
                "failure must write nothing else"
            );
        }

        // A later intact report replaces the failure; the failure kinds also
        // replace each other and land on the verify side the same way.
        let p = permit(&mut db, &handle, true).await;
        handle = applied(
            record_content_check(&mut db, &p, intact(s, true)).await,
            "intact after failure",
        );
        assert_eq!(
            content_state(&mut db, &handle, true).await,
            Some(ContentCheckState::Intact)
        );
        for kind in [
            ContentCheckState::AuthenticationFailed,
            ContentCheckState::IntegrityFailed,
        ] {
            let p = permit(&mut db, &handle, false).await;
            handle = applied(
                record_content_check(&mut db, &p, nonaffirmative(kind)).await,
                "verify side failure",
            );
            assert_eq!(content_state(&mut db, &handle, false).await, Some(kind));
            assert_eq!(
                content_state(&mut db, &handle, true).await,
                Some(ContentCheckState::Intact)
            );
        }
        assert!(guard(&db, s).await == held);
        assert_eq!(content_rows(&db, s).await.len(), 2);
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
    });
}

#[test]
fn postgres_new_round_rejects_old_permits_and_keeps_old_content_out_of_the_current_checkpoint() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let done = finished(&mut db, begun).await;
        let first_permit = permit(&mut db, &done, true).await;
        let first = applied(
            record_content_check(&mut db, &first_permit, intact(s, true)).await,
            "round one content",
        );
        let outstanding = permit(&mut db, &first, false).await;
        let round_one = content_rows(&db, s).await;
        assert_eq!(round_one.len(), 1);

        // A new round: every earlier permit and handle is dead.
        let second = applied(
            begin_verification_round(&mut db, &first).await,
            "begin second round",
        );
        let before = snapshot(&db, s).await;
        assert!(is_stale(
            &record_content_check(&mut db, &outstanding, intact(s, false)).await
        ));
        assert!(is_stale(
            &record_content_check(&mut db, &first_permit, intact(s, true)).await
        ));
        assert!(is_stale(
            &record_content_check(
                &mut db,
                &outstanding,
                nonaffirmative(ContentCheckState::IntegrityFailed)
            )
            .await
        ));
        assert!(no_permit(&mut db, &first, true).await);
        assert!(no_permit(&mut db, &done, false).await);
        assert!(
            snapshot(&db, s).await == before,
            "old permits must not mutate"
        );

        // Old content is retained but is not current evidence.
        assert!(content_rows(&db, s).await == round_one);
        assert_eq!(content_state(&mut db, &second, true).await, None);
        assert_eq!(content_state(&mut db, &second, false).await, None);

        // The new round takes fresh evidence beside the retained row.
        let p = permit(&mut db, &second, true).await;
        let third = applied(
            record_content_check(&mut db, &p, intact(s, true)).await,
            "round two content",
        );
        let rows = content_rows(&db, s).await;
        assert_eq!(rows.len(), 2);
        assert!(
            rows[0] == round_one[0],
            "old round evidence must not change"
        );
        assert!(rows[1].0 > rows[0].0);
        assert_eq!(
            content_state(&mut db, &third, true).await,
            Some(ContentCheckState::Intact)
        );
        assert_eq!(content_state(&mut db, &third, false).await, None);
        let g = guard(&db, s).await;
        assert_eq!(g.state, "verifying");
        assert!(g.verified_at.is_none(), "new round clears verified_at");
        assert!(g.retention_checked_at.is_none());
    });
}

#[test]
fn postgres_takeover_preserves_content_evidence_and_invalidates_the_old_permit() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let first_attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let admitted_handle = admitted(&mut db, &first_attempt, s).await;
        let begun = applied(
            begin_verification_round(&mut db, &admitted_handle).await,
            "begin round",
        );
        let done = finished(&mut db, begun).await;
        let p = permit(&mut db, &done, true).await;
        let old_handle = applied(
            record_content_check(&mut db, &p, intact(s, true)).await,
            "content before takeover",
        );
        let old_permit = permit(&mut db, &old_handle, false).await;

        // The first attempt and its operation lease expire; attempt two takes over.
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
        let next_attempt = acquire(&mut db, &c.job, &c.owner, 1).await;
        let rows = content_rows(&db, s).await;
        let held = guard(&db, s).await;
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");
        let new_handle = match take_over(&mut db, &next_attempt, &expected, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(handle, _)) => handle,
            _ => panic!("takeover did not resume the candidate"),
        };

        // Takeover preserves evidence exactly and writes nothing guarded.
        assert!(
            content_rows(&db, s).await == rows,
            "takeover changed evidence"
        );
        assert!(guard(&db, s).await == held);
        assert_eq!(
            content_state(&mut db, &new_handle, true).await,
            Some(ContentCheckState::Intact)
        );
        assert_eq!(content_state(&mut db, &new_handle, false).await, None);

        // The old claim's permit and handles are dead.
        let before = snapshot(&db, s).await;
        assert!(is_stale(
            &record_content_check(&mut db, &old_permit, intact(s, false)).await
        ));
        assert!(is_stale(
            &record_content_check(
                &mut db,
                &old_permit,
                nonaffirmative(ContentCheckState::Unavailable)
            )
            .await
        ));
        assert!(no_permit(&mut db, &old_handle, false).await);
        assert!(no_permit(&mut db, &old_handle, true).await);
        assert!(snapshot(&db, s).await == before);
        assert!(content_rows(&db, s).await == rows);

        // The new claim's handle prepares and records.
        let p = permit(&mut db, &new_handle, false).await;
        let handle = applied(
            record_content_check(&mut db, &p, intact(s, false)).await,
            "content after takeover",
        );
        assert_eq!(content_rows(&db, s).await.len(), 2);
        assert_eq!(
            content_state(&mut db, &handle, false).await,
            Some(ContentCheckState::Intact)
        );
        assert_eq!(guard(&db, s).await.state, "verifying");
    });
}

#[test]
fn postgres_lease_status_attempt_and_revision_fences_gate_content_preparation_and_callbacks() {
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
            // A permit prepared while every fence still held.
            let held_permit = permit(&mut db, &begun, true).await;
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
            let held = guard(&db, s).await;
            for post in [true, false] {
                assert!(
                    no_permit(&mut db, &begun, post).await,
                    "{label}: prepare must be stale"
                );
            }
            assert!(
                is_stale(&record_content_check(&mut db, &held_permit, intact(s, true)).await),
                "{label}: intact callback must be stale"
            );
            assert!(
                is_stale(
                    &record_content_check(
                        &mut db,
                        &held_permit,
                        nonaffirmative(ContentCheckState::IntegrityFailed)
                    )
                    .await
                ),
                "{label}: failure callback must be stale"
            );
            assert!(
                snapshot(&db, s).await == before,
                "{label}: fenced calls must not mutate"
            );
            assert!(guard(&db, s).await == held);
            assert!(
                content_rows(&db, s).await.is_empty(),
                "{label}: row written"
            );
        }

        // An unrelated revision bump retires the original permit even though
        // the content callback itself would otherwise be valid.
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let original = permit(&mut db, &begun, true).await;
        let bumped = applied(
            confirm_article(&mut db, &begun, 0, ArticleSide::Post).await,
            "unrelated confirm",
        );
        let before = snapshot(&db, s).await;
        assert!(is_stale(
            &record_content_check(&mut db, &original, intact(s, true)).await
        ));
        assert!(no_permit(&mut db, &begun, true).await);
        assert!(snapshot(&db, s).await == before);
        assert!(content_rows(&db, s).await.is_empty());
        let fresh = permit(&mut db, &bumped, true).await;
        applied(
            record_content_check(&mut db, &fresh, intact(s, true)).await,
            "fresh permit after unrelated revision",
        );
        assert_eq!(content_rows(&db, s).await.len(), 1);
    });
}

#[test]
fn postgres_schema_replay_preserves_content_evidence_without_backfill_and_checks_guard_rows() {
    run_blocking(true, async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let done = finished(&mut db, begun).await;
        let p = permit(&mut db, &done, true).await;
        let after_post = applied(
            record_content_check(&mut db, &p, intact(s, true)).await,
            "post content",
        );
        let p = permit(&mut db, &after_post, false).await;
        let handle = applied(
            record_content_check(&mut db, &p, nonaffirmative(ContentCheckState::Unavailable)).await,
            "verify content",
        );

        // A second live round with presence but no content evidence at all.
        let (bare, bare_attempt, bare_begun) = verifying(&mut db).await;
        let bare_done = finished(&mut db, bare_begun).await;
        drop(bare_done);

        let rows = content_rows(&db, s).await;
        assert_eq!(rows.len(), 2);
        let before = snapshot(&db, s).await;
        let held = guard(&db, s).await;

        // Replay the exact migration on a separate session, twice.
        let ddl = connect().await;
        ddl.batch_execute(MIGRATION_0039)
            .await
            .quiet("replay migration 0039");
        ddl.batch_execute(MIGRATION_0039)
            .await
            .quiet("replay migration 0039 again");
        assert!(
            content_rows(&db, s).await == rows,
            "replay changed evidence"
        );
        assert!(
            snapshot(&db, s).await == before,
            "replay changed the bundle"
        );
        assert!(guard(&db, s).await == held);
        assert_eq!(
            content_state(&mut db, &handle, true).await,
            Some(ContentCheckState::Intact)
        );
        assert_eq!(
            content_state(&mut db, &handle, false).await,
            Some(ContentCheckState::Unavailable)
        );

        // No legacy backfill: a presence-verified generation gains no content.
        assert!(
            content_rows(&db, &bare.sealed).await.is_empty(),
            "replay must never invent content evidence"
        );
        let bare_handle = fresh_handle(&mut db, &bare_attempt, &bare.sealed).await;
        assert_eq!(content_state(&mut db, &bare_handle, true).await, None);
        assert_eq!(content_state(&mut db, &bare_handle, false).await, None);

        // Table safeguards. Each statement is a single autocommit write.
        let unique = &SqlState::UNIQUE_VIOLATION;
        let check = &SqlState::CHECK_VIOLATION;
        expect_db_error(
            db.execute(
                "INSERT INTO posted_file_content_checks \
                 (generation_id, round, side, backbone, state, checked_articles, checked_at) \
                 SELECT generation_id, round, side, backbone, state, checked_articles, checked_at \
                 FROM posted_file_content_checks WHERE generation_id = $1 AND backbone = $2",
                &[&s.generation, &lane(true)],
            )
            .await,
            unique,
            None,
            "duplicate generation, round and side",
        );
        expect_db_error(
            db.execute(
                "UPDATE posted_file_content_checks SET backbone = $2 \
                 WHERE generation_id = $1 AND backbone = $3",
                &[&s.generation, &lane(true), &lane(false)],
            )
            .await,
            unique,
            None,
            "duplicate generation, round and backbone",
        );
        expect_db_error(
            db.execute(
                "UPDATE posted_file_content_checks SET checked_articles = -1 \
                 WHERE generation_id = $1 AND backbone = $2",
                &[&s.generation, &lane(true)],
            )
            .await,
            check,
            None,
            "negative checked article count",
        );
        expect_db_error(
            db.execute(
                "UPDATE posted_file_content_checks SET state = 'unrecognised' \
                 WHERE generation_id = $1 AND backbone = $2",
                &[&s.generation, &lane(true)],
            )
            .await,
            check,
            None,
            "unrecognised state",
        );
        let unknown_generation = format!("pg-gen-{}-none", c.uuid);
        expect_db_error(
            db.execute(
                "INSERT INTO posted_file_content_checks \
                 (generation_id, round, side, backbone, state, checked_articles, checked_at) \
                 SELECT $2, round, side, backbone, state, checked_articles, checked_at \
                 FROM posted_file_content_checks WHERE generation_id = $1 AND backbone = $3",
                &[&s.generation, &unknown_generation, &lane(true)],
            )
            .await,
            &SqlState::FOREIGN_KEY_VIOLATION,
            None,
            "evidence for an unknown generation",
        );
        assert!(
            content_rows(&db, s).await == rows,
            "rejected writes must leave evidence unchanged"
        );
        assert!(snapshot(&db, s).await == before);
    });
}
