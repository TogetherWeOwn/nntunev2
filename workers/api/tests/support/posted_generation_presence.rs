//! Real-PostgreSQL tests for the fenced presence (`STAT`) evidence adapter
//! (`prepare_presence_check`, `record_presence_check`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `acquire`, `admitted`,
//! `observe`, `count`, `snapshot`, `recover`, `run_test`). Fixtures stay synthetic sealed byte
//! buffers keyed by database UUIDs: no article identities, keys, names or NZB material. Failures print
//! only static labels; outcomes are matched, never formatted, and sealed
//! values are compared with `==` inside `assert!`, so no assertion can echo
//! handles, permits, digests or encrypted material.
//!
//! Scope: a presence verdict is slot bookkeeping only. `Present` confirms a
//! slot and is never content proof; `Missing` and transport failures
//! unconfirm that slot, drop verification stamps and downgrade an intact
//! content report on that side to unavailable. None of them spends repost
//! budget, writes terminal health, or touches the schedule or other rounds.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, confirm_article, finish_verification, load_checkpoint,
    prepare_content_check, prepare_presence_check, prepare_repost, record_content_check,
    record_presence_check, record_repost, take_over, ArticleSide, ContentCheckReport,
    ContentCheckResult, ContentCheckState, GenerationHandle, MutationOutcome, PresenceCheckPermit,
    RepostPreparation,
};
use nntune_ingest::two_spool::{PostVerdict, StatVerdict};
use std::sync::Arc;
use tokio::sync::Barrier;

const SLOTS: [i32; 3] = [0, 1, 2];
const ARTICLES: i32 = 3;

fn side_of(post: bool) -> ArticleSide {
    if post {
        ArticleSide::Post
    } else {
        ArticleSide::Verify
    }
}

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

fn is_incomplete(outcome: &Result<MutationOutcome, StoreError>) -> bool {
    matches!(outcome, Ok(MutationOutcome::Incomplete))
}

async fn permit(
    db: &mut Client,
    handle: &GenerationHandle,
    slot: i32,
    post: bool,
) -> PresenceCheckPermit {
    match prepare_presence_check(db, handle, slot, side_of(post)).await {
        Ok(Some(permit)) => permit,
        _ => panic!("expected a presence-check permit"),
    }
}

async fn no_permit(db: &mut Client, handle: &GenerationHandle, slot: i32, post: bool) -> bool {
    matches!(
        prepare_presence_check(db, handle, slot, side_of(post)).await,
        Ok(None)
    )
}

/// Record one verdict through a freshly prepared permit and return the
/// refreshed handle.
async fn observe_slot(
    db: &mut Client,
    handle: &GenerationHandle,
    slot: i32,
    post: bool,
    verdict: StatVerdict,
) -> GenerationHandle {
    let p = permit(db, handle, slot, post).await;
    applied(
        record_presence_check(db, &p, verdict).await,
        "record presence verdict",
    )
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

/// Confirm every slot on both sides with the existing confirm path and finish.
async fn finished(db: &mut Client, mut handle: GenerationHandle) -> GenerationHandle {
    for slot in SLOTS {
        for post in [true, false] {
            handle = applied(
                confirm_article(db, &handle, slot, side_of(post)).await,
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

async fn confirmed(db: &mut Client, handle: &GenerationHandle, slot: usize, post: bool) -> bool {
    match load_checkpoint(db, handle).await {
        Ok(Some(cp)) => {
            cp.slot(slot, side_of(post))
                .expect("slot inside the sealed plan")
                .confirmed
        }
        _ => panic!("expected a current checkpoint"),
    }
}

async fn content_state(
    db: &mut Client,
    handle: &GenerationHandle,
    post: bool,
) -> Option<ContentCheckState> {
    match load_checkpoint(db, handle).await {
        Ok(Some(cp)) => cp.content_check(side_of(post)),
        _ => panic!("expected a current checkpoint"),
    }
}

fn intact(s: &Sealed, post: bool) -> ContentCheckResult<'_> {
    ContentCheckResult::Intact(ContentCheckReport {
        backbone: lane(post),
        plan_sha256: &s.plan,
        plaintext_sha256: &s.hash,
        plaintext_size: 1024,
        checked_articles: 3,
    })
}

async fn record_content(
    db: &mut Client,
    handle: &GenerationHandle,
    post: bool,
    result: ContentCheckResult<'_>,
) -> GenerationHandle {
    let p = match prepare_content_check(db, handle, side_of(post)).await {
        Ok(Some(p)) => p,
        _ => panic!("expected a content-check permit"),
    };
    applied(
        record_content_check(db, &p, result).await,
        "record content fixture",
    )
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

/// `(state, failure_kind, verified_at, retention_checked_at, round)`.
async fn health(
    db: &Client,
    s: &Sealed,
) -> (String, Option<String>, Option<i64>, Option<i64>, i64) {
    let row = db
        .query_one(
            "SELECT state, failure_kind, verified_at, retention_checked_at, round \
             FROM posted_file_health WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("read health");
    (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4))
}

/// `(round, slot, side, confirmed, repost_attempts)` for every stored row.
async fn progress(db: &Client, s: &Sealed) -> Vec<(i64, i32, i16, bool, i32)> {
    db.query(
        "SELECT round, slot, side, confirmed, repost_attempts \
         FROM posted_file_article_progress WHERE generation_id = $1 \
         ORDER BY round, slot, side",
        &[&s.generation],
    )
    .await
    .quiet("read progress rows")
    .iter()
    .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4)))
    .collect()
}

async fn repost_sum(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT COALESCE(sum(repost_attempts), 0)::int8 \
         FROM posted_file_article_progress WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
}

async fn schedule_digest(db: &Client, s: &Sealed) -> Option<String> {
    db.query_one(
        "SELECT md5(string_agg(t::text, ',' ORDER BY t.round)) \
         FROM posted_file_recheck_schedule t WHERE t.generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("digest schedule rows")
    .get(0)
}

async fn content_rows(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_content_checks WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
}

async fn expire_attempt_and_operation(db: &Client, c: &Case) {
    db.execute(
        "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
        &[&c.job],
    )
    .await
    .quiet("expire attempt");
    db.execute(
        "UPDATE posted_file_operations SET lease_until = 0 WHERE id = $1",
        &[&c.sealed.op],
    )
    .await
    .quiet("expire operation");
}

#[test]
fn postgres_presence_permits_confirm_every_slot_and_side_without_content_proof() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;

        // Preparing is read only and available as soon as a round has begun.
        let before = snapshot(&db, s).await;
        let before_rows = progress(&db, s).await;
        let first = permit(&mut db, &begun, 0, true).await;
        assert!(
            format!("{first:?}") == "PresenceCheckPermit([REDACTED])",
            "permit debug must be redacted"
        );
        assert!(!format!("{first:?}").contains(&s.generation));
        assert!(snapshot(&db, s).await == before, "prepare must not mutate");
        assert!(progress(&db, s).await == before_rows);

        // Every (slot, side) is covered by its own permit; each applies once
        // and confirms exactly that cell.
        let mut handle = begun;
        let mut done = 0usize;
        for slot in SLOTS {
            for post in [true, false] {
                let rev = health_revision(&db, s).await;
                handle = observe_slot(&mut db, &handle, slot, post, StatVerdict::Present).await;
                done += 1;
                assert_eq!(health_revision(&db, s).await, rev + 1);
                assert!(confirmed(&mut db, &handle, slot as usize, post).await);
                let rows = progress(&db, s).await;
                assert_eq!(rows.iter().filter(|r| r.3).count(), done);
                assert!(rows.iter().all(|r| r.4 == 0), "no repost spend");
            }
        }
        assert_eq!(repost_sum(&db, s).await, 0);

        // Presence alone never proves content or finishes verification.
        assert_eq!(content_rows(&db, s).await, 0);
        assert_eq!(content_state(&mut db, &handle, true).await, None);
        assert_eq!(content_state(&mut db, &handle, false).await, None);
        let (state, kind, verified_at, retention, _) = health(&db, s).await;
        assert_eq!(state, "verifying");
        assert_eq!(kind, None);
        assert!(verified_at.is_none() && retention.is_none());

        // The ordinary finish accepts the permit-confirmed bank, and prepare
        // is still available (and still read only) afterwards.
        let done_handle = applied(finish_verification(&mut db, &handle).await, "finish");
        assert!(
            health(&db, s).await.2.is_some(),
            "finish stamps verified_at"
        );
        let before = snapshot(&db, s).await;
        let _after_finish = permit(&mut db, &done_handle, 2, false).await;
        assert!(snapshot(&db, s).await == before);
        assert_eq!(content_rows(&db, s).await, 0);
        assert!(!posting_fence_is_current(&mut db, &done_handle)
            .await
            .expect("fence after presence"));
    });
}

#[test]
fn postgres_missing_and_unavailable_unconfirm_one_slot_and_invalidate_intact_content_only() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;

        // Spend real repost budget on two slots before presence is confirmed.
        let mut handle = begun;
        for (slot, post) in [(1, true), (0, false)] {
            let p = match prepare_repost(&mut db, &handle, slot, side_of(post)).await {
                Ok(RepostPreparation::Ready(p)) => *p,
                _ => panic!("expected a ready repost permit"),
            };
            handle = applied(
                record_repost(&mut db, &p, PostVerdict::Posted).await,
                "spend repost budget",
            );
        }
        let handle = finished(&mut db, handle).await;
        let handle = record_content(&mut db, &handle, true, intact(s, true)).await;
        let mut handle = record_content(&mut db, &handle, false, intact(s, false)).await;
        let budget = progress(&db, s)
            .await
            .into_iter()
            .map(|r| (r.0, r.1, r.2, r.4))
            .collect::<Vec<_>>();
        assert_eq!(repost_sum(&db, s).await, 2);
        let schedule = schedule_digest(&db, s).await;
        let round = health(&db, s).await.4;
        assert!(health(&db, s).await.2.is_some());
        // Fixture-only stamp: this candidate is not usable or selected. Prove
        // that invalidation clears a non-null retention stamp, not only NULL.
        db.execute(
            "UPDATE posted_file_health SET retention_checked_at = \
             floor(extract(epoch FROM clock_timestamp()))::bigint WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("seed retention stamp for invalidation");
        assert!(health(&db, s).await.3.is_some());
        let held = snapshot(&db, s).await;

        // Missing on one post slot: that slot only, post content only.
        let rev = health_revision(&db, s).await;
        handle = observe_slot(&mut db, &handle, 1, true, StatVerdict::Missing).await;
        assert_eq!(health_revision(&db, s).await, rev + 1);
        assert!(!confirmed(&mut db, &handle, 1, true).await);
        for (slot, post) in [(0, true), (2, true), (0, false), (1, false), (2, false)] {
            assert!(
                confirmed(&mut db, &handle, slot, post).await,
                "other cells stay confirmed"
            );
        }
        let (state, kind, verified_at, retention, now_round) = health(&db, s).await;
        assert_eq!(state, "verifying", "missing is never terminal");
        assert_eq!(kind, None);
        assert!(verified_at.is_none() && retention.is_none());
        assert_eq!(now_round, round);
        assert_eq!(
            content_state(&mut db, &handle, true).await,
            Some(ContentCheckState::Unavailable)
        );
        assert_eq!(
            content_state(&mut db, &handle, false).await,
            Some(ContentCheckState::Intact),
            "the opposite side's evidence is untouched"
        );
        assert_eq!(content_rows(&db, s).await, 2);
        let after_budget = progress(&db, s)
            .await
            .into_iter()
            .map(|r| (r.0, r.1, r.2, r.4))
            .collect::<Vec<_>>();
        assert!(after_budget == budget, "no repost spend or reset");
        assert!(schedule_digest(&db, s).await == schedule);

        // A non-intact report is left exactly as it was by a later Missing.
        handle = record_content(
            &mut db,
            &handle,
            true,
            ContentCheckResult::AuthenticationFailed,
        )
        .await;
        handle = observe_slot(&mut db, &handle, 2, true, StatVerdict::Missing).await;
        assert!(!confirmed(&mut db, &handle, 2, true).await);
        assert_eq!(
            content_state(&mut db, &handle, true).await,
            Some(ContentCheckState::AuthenticationFailed)
        );
        assert_eq!(
            content_state(&mut db, &handle, false).await,
            Some(ContentCheckState::Intact)
        );

        // A transport failure on the verify side behaves like Missing there.
        handle = observe_slot(&mut db, &handle, 0, false, StatVerdict::Transport).await;
        assert!(!confirmed(&mut db, &handle, 0, false).await);
        assert_eq!(
            content_state(&mut db, &handle, false).await,
            Some(ContentCheckState::Unavailable)
        );
        assert_eq!(
            content_state(&mut db, &handle, true).await,
            Some(ContentCheckState::AuthenticationFailed)
        );
        assert!(confirmed(&mut db, &handle, 0, true).await);
        assert_eq!(repost_sum(&db, s).await, 2);
        assert!(schedule_digest(&db, s).await == schedule);
        assert_eq!(health(&db, s).await.0, "verifying");

        // Present re-confirms the slot but restores neither stamps nor content.
        handle = observe_slot(&mut db, &handle, 1, true, StatVerdict::Present).await;
        assert!(confirmed(&mut db, &handle, 1, true).await);
        let (_, _, verified_at, retention, _) = health(&db, s).await;
        assert!(verified_at.is_none() && retention.is_none());
        assert_eq!(
            content_state(&mut db, &handle, false).await,
            Some(ContentCheckState::Unavailable)
        );
        assert_eq!(content_rows(&db, s).await, 2);
        assert_eq!(repost_sum(&db, s).await, 2);
        let after = snapshot(&db, s).await;
        for index in [0, 1, 2, 4] {
            assert!(
                after[index] == held[index],
                "custody/registry/claim/reference moved"
            );
        }
    });
}

#[test]
fn postgres_missing_during_the_round_blocks_finish_until_the_slot_is_reconfirmed() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let mut handle = begun;
        for slot in SLOTS {
            for post in [true, false] {
                handle = observe_slot(&mut db, &handle, slot, post, StatVerdict::Present).await;
            }
        }
        handle = observe_slot(&mut db, &handle, 2, false, StatVerdict::Missing).await;
        assert!(!confirmed(&mut db, &handle, 2, false).await);
        assert_eq!(repost_sum(&db, s).await, 0);
        assert_eq!(health(&db, s).await.0, "verifying");

        let before = snapshot(&db, s).await;
        assert!(is_incomplete(&finish_verification(&mut db, &handle).await));
        assert!(
            snapshot(&db, s).await == before,
            "incomplete must not mutate"
        );
        assert!(health(&db, s).await.2.is_none());

        handle = observe_slot(&mut db, &handle, 2, false, StatVerdict::Present).await;
        let done = applied(finish_verification(&mut db, &handle).await, "finish");
        assert!(health(&db, s).await.2.is_some());
        assert!(confirmed(&mut db, &done, 2, false).await);
    });
}

#[test]
fn postgres_original_permits_are_stale_after_revision_round_takeover_job_and_lease_changes() {
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
            let held = permit(&mut db, &begun, 1, true).await;
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
            for post in [true, false] {
                assert!(
                    no_permit(&mut db, &begun, 1, post).await,
                    "{label}: prepare must be stale"
                );
            }
            for verdict in [
                StatVerdict::Present,
                StatVerdict::Missing,
                StatVerdict::Transport,
            ] {
                assert!(
                    is_stale(&record_presence_check(&mut db, &held, verdict).await),
                    "{label}: callback must be stale"
                );
            }
            assert!(
                snapshot(&db, s).await == before,
                "{label}: fenced calls must not mutate"
            );
            assert!(progress(&db, s).await == rows, "{label}: progress moved");
            assert_eq!(content_rows(&db, s).await, 0);
        }

        // An unrelated revision bump retires the original permit.
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let original = permit(&mut db, &begun, 1, true).await;
        let bumped = applied(
            confirm_article(&mut db, &begun, 0, ArticleSide::Post).await,
            "unrelated confirm",
        );
        let before = snapshot(&db, s).await;
        let rows = progress(&db, s).await;
        for verdict in [StatVerdict::Present, StatVerdict::Missing] {
            assert!(is_stale(
                &record_presence_check(&mut db, &original, verdict).await
            ));
        }
        assert!(no_permit(&mut db, &begun, 1, true).await);
        assert!(snapshot(&db, s).await == before);
        assert!(progress(&db, s).await == rows);
        observe_slot(&mut db, &bumped, 1, true, StatVerdict::Present).await;

        // A new round retires permits and handles; old rounds stay as stored.
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let done = finished(&mut db, begun).await;
        let old_present = permit(&mut db, &done, 0, true).await;
        let old_missing = permit(&mut db, &done, 1, false).await;
        let second = applied(
            begin_verification_round(&mut db, &done).await,
            "begin second round",
        );
        let round_one = progress(&db, s).await;
        let before = snapshot(&db, s).await;
        for verdict in [
            StatVerdict::Present,
            StatVerdict::Missing,
            StatVerdict::Transport,
        ] {
            assert!(is_stale(
                &record_presence_check(&mut db, &old_present, verdict).await
            ));
            assert!(is_stale(
                &record_presence_check(&mut db, &old_missing, verdict).await
            ));
        }
        assert!(no_permit(&mut db, &done, 0, true).await);
        assert!(snapshot(&db, s).await == before);
        assert!(progress(&db, s).await == round_one);
        let current_round = health(&db, s).await.4;
        let handle = observe_slot(&mut db, &second, 0, true, StatVerdict::Present).await;
        observe_slot(&mut db, &handle, 0, true, StatVerdict::Missing).await;
        let rows = progress(&db, s).await;
        let old: Vec<_> = rows.iter().filter(|r| r.0 < current_round).collect();
        assert_eq!(old.len(), round_one.len());
        assert!(
            old.iter().all(|r| r.3),
            "an earlier round's confirmations are untouched"
        );

        // Takeover by a new attempt preserves state and kills the old permit.
        let c = case(&db).await;
        let s = &c.sealed;
        let first_attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let admitted_handle = admitted(&mut db, &first_attempt, s).await;
        let begun = applied(
            begin_verification_round(&mut db, &admitted_handle).await,
            "begin round",
        );
        let old_handle = observe_slot(&mut db, &begun, 0, true, StatVerdict::Present).await;
        let old_permit = permit(&mut db, &old_handle, 0, true).await;
        expire_attempt_and_operation(&db, &c).await;
        let next_attempt = acquire(&mut db, &c.job, &c.owner, 1).await;
        let rows = progress(&db, s).await;
        let schedule = schedule_digest(&db, s).await;
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");
        let new_handle = match take_over(&mut db, &next_attempt, &expected, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(handle, _)) => handle,
            _ => panic!("takeover did not resume the candidate"),
        };
        assert!(progress(&db, s).await == rows);
        assert!(schedule_digest(&db, s).await == schedule);
        let before = snapshot(&db, s).await;
        for verdict in [StatVerdict::Missing, StatVerdict::Present] {
            assert!(is_stale(
                &record_presence_check(&mut db, &old_permit, verdict).await
            ));
        }
        assert!(no_permit(&mut db, &old_handle, 0, true).await);
        assert!(snapshot(&db, s).await == before);
        assert!(progress(&db, s).await == rows);
        assert!(confirmed(&mut db, &new_handle, 0, true).await);
        let handle = observe_slot(&mut db, &new_handle, 0, true, StatVerdict::Missing).await;
        assert!(!confirmed(&mut db, &handle, 0, true).await);
        assert!(schedule_digest(&db, s).await == schedule);
    });
}

#[test]
fn postgres_same_permit_race_applies_once_and_lost_ack_recovery_never_double_spends() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let original = permit(&mut db, &begun, 0, true).await;

        let mut left = connect().await;
        let mut right = connect().await;
        let (pl, pr) = (original.clone(), original.clone());
        let barrier = Arc::new(Barrier::new(2));
        let (gl, gr) = (barrier.clone(), barrier);
        let (out_left, out_right) = tokio::join!(
            async {
                gl.wait().await;
                record_presence_check(&mut left, &pl, StatVerdict::Present).await
            },
            async {
                gr.wait().await;
                record_presence_check(&mut right, &pr, StatVerdict::Present).await
            }
        );
        let winner = match (out_left, out_right) {
            (Ok(MutationOutcome::Applied(h)), Ok(MutationOutcome::StaleOrDiverged))
            | (Ok(MutationOutcome::StaleOrDiverged), Ok(MutationOutcome::Applied(h))) => *h,
            _ => panic!("exactly one concurrent callback must apply"),
        };
        assert!(confirmed(&mut db, &winner, 0, true).await);
        assert_eq!(progress(&db, s).await.iter().filter(|r| r.3).count(), 1);

        // Lost acknowledgment: recovery returns the current revision without
        // writing, and a fresh permit repeats the verdict with no bump.
        drop(winner);
        let rev = health_revision(&db, s).await;
        let before = snapshot(&db, s).await;
        let rows = progress(&db, s).await;
        let recovered = fresh_handle(&mut db, &attempt, s).await;
        assert!(snapshot(&db, s).await == before, "recovery must not mutate");
        let fresh = permit(&mut db, &recovered, 0, true).await;
        let replayed = applied(
            record_presence_check(&mut db, &fresh, StatVerdict::Present).await,
            "fresh permit replay",
        );
        assert_eq!(health_revision(&db, s).await, rev);
        assert!(snapshot(&db, s).await == before);
        assert!(progress(&db, s).await == rows);

        // The original callbacks, replayed after the write, are stale.
        for verdict in [StatVerdict::Present, StatVerdict::Missing] {
            assert!(is_stale(
                &record_presence_check(&mut db, &original, verdict).await
            ));
        }
        assert!(confirmed(&mut db, &replayed, 0, true).await);
        assert_eq!(health_revision(&db, s).await, rev);

        // The same race for a state-changing Missing verdict.
        let missing = permit(&mut db, &replayed, 0, true).await;
        let (pl, pr) = (missing.clone(), missing.clone());
        let barrier = Arc::new(Barrier::new(2));
        let (gl, gr) = (barrier.clone(), barrier);
        let (out_left, out_right) = tokio::join!(
            async {
                gl.wait().await;
                record_presence_check(&mut left, &pl, StatVerdict::Missing).await
            },
            async {
                gr.wait().await;
                record_presence_check(&mut right, &pr, StatVerdict::Missing).await
            }
        );
        let winner = match (out_left, out_right) {
            (Ok(MutationOutcome::Applied(h)), Ok(MutationOutcome::StaleOrDiverged))
            | (Ok(MutationOutcome::StaleOrDiverged), Ok(MutationOutcome::Applied(h))) => *h,
            _ => panic!("exactly one concurrent missing callback must apply"),
        };
        assert!(!confirmed(&mut db, &winner, 0, true).await);
        assert_eq!(health_revision(&db, s).await, rev + 1);
        drop(winner);
        let rev = health_revision(&db, s).await;
        let before = snapshot(&db, s).await;
        let recovered = fresh_handle(&mut db, &attempt, s).await;
        let fresh = permit(&mut db, &recovered, 0, true).await;
        applied(
            record_presence_check(&mut db, &fresh, StatVerdict::Missing).await,
            "fresh missing replay",
        );
        assert_eq!(health_revision(&db, s).await, rev);
        assert!(snapshot(&db, s).await == before);
        assert!(is_stale(
            &record_presence_check(&mut db, &missing, StatVerdict::Missing).await
        ));
        assert_eq!(repost_sum(&db, s).await, 0, "no verdict ever spends budget");
        assert_eq!(health(&db, s).await.0, "verifying");
    });
}

#[test]
fn postgres_presence_prepare_refuses_invalid_slots_and_unbegun_rounds_without_writes() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let s = &c.sealed;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let h0 = admitted(&mut db, &attempt, s).await;

        // Round zero has no verification round to observe.
        let before = snapshot(&db, s).await;
        assert!(no_permit(&mut db, &h0, 0, true).await);
        assert!(no_permit(&mut db, &h0, 0, false).await);
        assert!(snapshot(&db, s).await == before);
        assert!(progress(&db, s).await.is_empty());

        let handle = applied(
            begin_verification_round(&mut db, &h0).await,
            "begin first round",
        );
        let before = snapshot(&db, s).await;
        let rows = progress(&db, s).await;
        let rev = health_revision(&db, s).await;
        for slot in [-1, i32::MIN, ARTICLES, 4, 1_000_000, i32::MAX] {
            for post in [true, false] {
                assert!(
                    matches!(
                        prepare_presence_check(&mut db, &handle, slot, side_of(post)).await,
                        Err(StoreError::InvalidInput)
                    ),
                    "out-of-range slot must be rejected"
                );
            }
        }
        assert!(
            snapshot(&db, s).await == before,
            "invalid slots must not mutate"
        );
        assert!(progress(&db, s).await == rows);
        assert_eq!(health_revision(&db, s).await, rev);

        // The rejected calls did not burn the handle; the last valid slot works,
        // and the pre-round handle stays stale.
        let p = permit(&mut db, &handle, ARTICLES - 1, false).await;
        assert!(snapshot(&db, s).await == before);
        let after = applied(
            record_presence_check(&mut db, &p, StatVerdict::Present).await,
            "last valid slot",
        );
        assert!(confirmed(&mut db, &after, ARTICLES as usize - 1, false).await);
        assert!(no_permit(&mut db, &h0, 0, true).await);
    });
}

#[test]
fn postgres_fresh_identical_presence_callbacks_are_idempotent_without_a_revision_bump() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;

        // The first negative observation persists an explicit unconfirmed row.
        // Repeating it with nothing further to invalidate changes no state.
        let rev = health_revision(&db, s).await;
        let mut handle = observe_slot(&mut db, &begun, 0, true, StatVerdict::Missing).await;
        assert_eq!(health_revision(&db, s).await, rev + 1);
        let rev = health_revision(&db, s).await;
        let before = snapshot(&db, s).await;
        for verdict in [StatVerdict::Missing, StatVerdict::Transport] {
            handle = observe_slot(&mut db, &handle, 0, true, verdict).await;
            assert_eq!(health_revision(&db, s).await, rev);
            assert!(snapshot(&db, s).await == before);
            assert!(!confirmed(&mut db, &handle, 0, true).await);
        }

        // Present changes state once; identical fresh permits then do not.
        handle = observe_slot(&mut db, &handle, 0, true, StatVerdict::Present).await;
        let rev = health_revision(&db, s).await;
        let before = snapshot(&db, s).await;
        let rows = progress(&db, s).await;
        for _ in 0..2 {
            handle = observe_slot(&mut db, &handle, 0, true, StatVerdict::Present).await;
            assert_eq!(health_revision(&db, s).await, rev);
            assert!(snapshot(&db, s).await == before);
            assert!(progress(&db, s).await == rows);
        }

        // Unavailable changes it back exactly once.
        handle = observe_slot(&mut db, &handle, 0, true, StatVerdict::Transport).await;
        assert_eq!(health_revision(&db, s).await, rev + 1);
        let before = snapshot(&db, s).await;
        let rows = progress(&db, s).await;
        for verdict in [StatVerdict::Transport, StatVerdict::Missing] {
            handle = observe_slot(&mut db, &handle, 0, true, verdict).await;
            assert_eq!(health_revision(&db, s).await, rev + 1);
            assert!(snapshot(&db, s).await == before);
            assert!(progress(&db, s).await == rows);
        }
        assert!(!confirmed(&mut db, &handle, 0, true).await);
        assert_eq!(repost_sum(&db, s).await, 0);
        assert_eq!(content_rows(&db, s).await, 0);
    });
}
