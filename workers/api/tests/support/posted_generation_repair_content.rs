//! Real-PostgreSQL regressions for content-evidence invalidation by an
//! accounted repost (`record_repost`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`; it reuses that
//! harness (`Case`, `Sealed`, `connect`, `case`, `acquire`, `admitted`,
//! `snapshot`, `count`, `recover`, `observe`, `run_test`) and carries its own
//! small helpers. Fixtures are synthetic sealed buffers keyed by database
//! UUIDs. Failures print only static labels: outcomes, states and rows are
//! matched or compared, never formatted, and database errors are only tested
//! for being errors, so no assertion can echo handles, permits, digests,
//! encrypted material, query text or driver errors.
//!
//! Contract under test: after a counted `Posted` or `DuplicatePresent`
//! callback, a prior current-round Intact content row for the reposted side
//! becomes Unavailable with zero checked articles and a database-clock
//! timestamp, and the health revision rises exactly once per callback. Other
//! sides, other rounds, failed or absent content, custody, references and the
//! schedule are untouched. Transport and Refused verdicts and stale callbacks
//! write nothing. Invalidation never grants terminal loss, selection or
//! cleanup authority, and these tests never expect any.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, confirm_article, finish_verification, load_checkpoint,
    prepare_content_check, prepare_repost, record_content_check, record_repost, take_over,
    ArticleSide, ContentCheckReport, ContentCheckResult, ContentCheckState, GenerationHandle,
    MutationOutcome, RepostPermit, RepostPreparation,
};
use nntune_ingest::two_spool::PostVerdict;

const SLOTS: [i32; 3] = [0, 1, 2];

/// `(round, side, state, checked_articles, checked_at)` of one stored row.
type Row = (i64, i32, String, i32, i64);

fn side_of(post: bool) -> ArticleSide {
    if post {
        ArticleSide::Post
    } else {
        ArticleSide::Verify
    }
}

fn side_code(post: bool) -> i32 {
    if post {
        0
    } else {
        1
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

fn is_incomplete(outcome: &Result<MutationOutcome, StoreError>) -> bool {
    matches!(outcome, Ok(MutationOutcome::Incomplete))
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

/// Prepare and record one content result for a side, returning the new handle.
async fn seed(
    db: &mut Client,
    handle: &GenerationHandle,
    post: bool,
    result: ContentCheckResult<'_>,
) -> GenerationHandle {
    let permit = match prepare_content_check(db, handle, side_of(post)).await {
        Ok(Some(permit)) => permit,
        _ => panic!("expected a content-check permit"),
    };
    applied(
        record_content_check(db, &permit, result).await,
        "seed content",
    )
}

async fn repost_permit(
    db: &mut Client,
    handle: &GenerationHandle,
    slot: i32,
    post: bool,
) -> RepostPermit {
    match prepare_repost(db, handle, slot, side_of(post)).await {
        Ok(RepostPreparation::Ready(permit)) => *permit,
        _ => panic!("expected a ready repost permit"),
    }
}

fn prep_stale(outcome: &Result<RepostPreparation, StoreError>) -> bool {
    matches!(outcome, Ok(RepostPreparation::StaleOrDiverged))
}

/// Admitted candidate with a freshly begun verification round.
async fn verifying(db: &mut Client) -> (Case, AttemptHandle, GenerationHandle) {
    let c = case(db).await;
    let attempt = acquire(db, &c.job, &c.owner, 0).await;
    let admitted_handle = admitted(db, &attempt, &c.sealed).await;
    let begun = applied(
        begin_verification_round(db, &admitted_handle).await,
        "begin round",
    );
    (c, attempt, begun)
}

/// Confirm every slot on both sides, then finish the presence round.
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

async fn expire_claim(db: &Client, c: &Case) {
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

/// Stored content rows for the generation across every round.
async fn rows(db: &Client, s: &Sealed) -> Vec<Row> {
    db.query(
        "SELECT round::int8, side::int4, state, checked_articles::int4, checked_at \
         FROM posted_file_content_checks WHERE generation_id = $1 ORDER BY round, side",
        &[&s.generation],
    )
    .await
    .quiet("read content rows")
    .iter()
    .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4)))
    .collect()
}

/// Progress rows as `(round, slot, side, confirmed, repost_attempts)`.
async fn progress(db: &Client, s: &Sealed) -> Vec<(i64, i32, i32, bool, i32)> {
    db.query(
        "SELECT round::int8, slot, side::int4, confirmed, repost_attempts \
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

async fn health_revision(db: &Client, s: &Sealed) -> i64 {
    db.query_one(
        "SELECT revision FROM posted_file_health WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("read health revision")
    .get(0)
}

/// `(state, verified_at, retention_checked_at, round)` of the health row.
async fn health(db: &Client, s: &Sealed) -> (String, Option<i64>, Option<i64>, i64) {
    let row = db
        .query_one(
            "SELECT state, verified_at, retention_checked_at, round \
             FROM posted_file_health WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("read health");
    (row.get(0), row.get(1), row.get(2), row.get(3))
}

async fn db_now(db: &Client) -> i64 {
    db.query_one(
        "SELECT floor(extract(epoch FROM clock_timestamp()))::int8",
        &[],
    )
    .await
    .quiet("read database clock")
    .get(0)
}

async fn content_state(
    db: &mut Client,
    handle: &GenerationHandle,
    post: bool,
) -> Option<ContentCheckState> {
    match load_checkpoint(db, handle).await {
        Ok(Some(checkpoint)) => checkpoint.content_check(side_of(post)),
        _ => panic!("expected a current checkpoint"),
    }
}

/// Everything except the health row, which legitimately gains a revision:
/// custody, registry, operation and references fingerprints.
fn untouched(snap: &[Option<String>]) -> [Option<String>; 4] {
    [
        snap[0].clone(),
        snap[1].clone(),
        snap[2].clone(),
        snap[4].clone(),
    ]
}

fn row_of(all: &[Row], round: i64, post: bool) -> Option<Row> {
    all.iter()
        .find(|r| r.0 == round && r.1 == side_code(post))
        .cloned()
}

#[test]
fn postgres_answered_reposts_invalidate_prior_intact_content_for_their_side_only() {
    run_test(async {
        let mut db = connect().await;
        for (verdict, post) in [
            (PostVerdict::Posted, true),
            (PostVerdict::DuplicatePresent, true),
            (PostVerdict::Posted, false),
            (PostVerdict::DuplicatePresent, false),
        ] {
            let (c, _attempt, begun) = verifying(&mut db).await;
            let s = &c.sealed;
            let handle = seed(&mut db, &begun, true, intact(s, true)).await;
            let handle = seed(&mut db, &handle, false, intact(s, false)).await;
            let before_rows = rows(&db, s).await;
            assert_eq!(before_rows.len(), 2, "both sides hold intact evidence");
            let before_snap = snapshot(&db, s).await;
            let before_rev = health_revision(&db, s).await;
            let before_health = health(&db, s).await;

            let permit = repost_permit(&mut db, &handle, 1, post).await;
            let t0 = db_now(&db).await;
            let handle = applied(
                record_repost(&mut db, &permit, verdict).await,
                "answered repost",
            );
            let t1 = db_now(&db).await;

            // The reposted side is Unavailable with zero articles and a
            // database-clock stamp; the other side is byte-for-byte as it was.
            let after = rows(&db, s).await;
            assert_eq!(after.len(), 2, "invalidation must not add or drop rows");
            let mine = row_of(&after, 1, post).expect("reposted side row");
            assert_eq!(mine.2, "unavailable", "reposted side must be unavailable");
            assert_eq!(mine.3, 0, "unavailable evidence checks no articles");
            assert!(
                mine.4 >= t0 && mine.4 <= t1,
                "invalidation must stamp the database clock"
            );
            assert!(
                row_of(&after, 1, !post) == row_of(&before_rows, 1, !post),
                "the other side must keep its evidence"
            );
            assert_eq!(
                content_state(&mut db, &handle, post).await,
                Some(ContentCheckState::Unavailable)
            );
            assert_eq!(
                content_state(&mut db, &handle, !post).await,
                Some(ContentCheckState::Intact)
            );

            // One accounted repost, one revision, nothing terminal.
            assert_eq!(health_revision(&db, s).await, before_rev + 1);
            let prog = progress(&db, s).await;
            assert!(
                prog == vec![(1, 1, side_code(post), false, 1)],
                "exactly one counted repost on the reposted cell"
            );
            let after_health = health(&db, s).await;
            assert!(after_health == before_health, "no health state movement");
            assert_eq!(after_health.0, "verifying");
            assert!(after_health.1.is_none() && after_health.2.is_none());
            assert!(
                untouched(&snapshot(&db, s).await) == untouched(&before_snap),
                "custody, registry, operation and references must be preserved"
            );
            assert_eq!(custody_rows(&db, s).await, 1);
            assert_eq!(operation_rows(&db, s).await, 1);

            // A second answered repost on the same side finds nothing intact
            // to invalidate: the row stays Unavailable, the revision still
            // rises exactly once for the new counted attempt.
            let permit = repost_permit(&mut db, &handle, 2, post).await;
            let handle = applied(
                record_repost(&mut db, &permit, verdict).await,
                "second repost",
            );
            assert_eq!(health_revision(&db, s).await, before_rev + 2);
            assert_eq!(
                content_state(&mut db, &handle, post).await,
                Some(ContentCheckState::Unavailable)
            );
            assert_eq!(rows(&db, s).await.len(), 2);
        }
    });
}

#[test]
fn postgres_unanswered_reposts_and_absent_or_failed_content_are_never_rewritten() {
    run_test(async {
        let mut db = connect().await;

        // Transport and Refused spend no budget and write nothing at all,
        // even with intact evidence present on the side.
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let handle = seed(&mut db, &begun, true, intact(s, true)).await;
        let handle = seed(&mut db, &handle, false, intact(s, false)).await;
        let before_rows = rows(&db, s).await;
        let before_snap = snapshot(&db, s).await;
        let before_rev = health_revision(&db, s).await;
        for post in [true, false] {
            for verdict in [PostVerdict::Transport, PostVerdict::Refused] {
                let permit = repost_permit(&mut db, &handle, 0, post).await;
                let outcome = record_repost(&mut db, &permit, verdict).await;
                assert!(
                    is_incomplete(&outcome),
                    "an unanswered verdict must be incomplete"
                );
            }
        }
        assert!(
            rows(&db, s).await == before_rows,
            "unanswered verdicts must not touch content"
        );
        assert!(
            snapshot(&db, s).await == before_snap,
            "unanswered verdicts must not write"
        );
        assert_eq!(health_revision(&db, s).await, before_rev);
        assert!(progress(&db, s).await.is_empty());
        assert_eq!(
            content_state(&mut db, &handle, true).await,
            Some(ContentCheckState::Intact)
        );

        // Absent content: a counted repost creates no content row on either
        // side, whatever the verdict.
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let permit = repost_permit(&mut db, &begun, 0, true).await;
        let handle = applied(
            record_repost(&mut db, &permit, PostVerdict::Posted).await,
            "repost without content",
        );
        let permit = repost_permit(&mut db, &handle, 0, false).await;
        let handle = applied(
            record_repost(&mut db, &permit, PostVerdict::DuplicatePresent).await,
            "second repost without content",
        );
        assert!(
            rows(&db, s).await.is_empty(),
            "a repost must never create content evidence"
        );
        assert_eq!(content_state(&mut db, &handle, true).await, None);
        assert_eq!(content_state(&mut db, &handle, false).await, None);

        // Only the other side holds evidence: it is not touched, and the
        // reposted side stays absent.
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let handle = seed(&mut db, &begun, false, intact(s, false)).await;
        let before_rows = rows(&db, s).await;
        let permit = repost_permit(&mut db, &handle, 0, true).await;
        let handle = applied(
            record_repost(&mut db, &permit, PostVerdict::Posted).await,
            "repost beside other-side evidence",
        );
        assert!(
            rows(&db, s).await == before_rows,
            "absent content must not be created or the other side rewritten"
        );
        assert_eq!(content_state(&mut db, &handle, true).await, None);

        // Failed content keeps its kind and its timestamp: only Intact is
        // invalidated, never rewritten from one failure to another.
        for kind in [
            ContentCheckResult::Unavailable,
            ContentCheckResult::AuthenticationFailed,
            ContentCheckResult::IntegrityFailed,
        ] {
            let (c, _attempt, begun) = verifying(&mut db).await;
            let s = &c.sealed;
            let handle = seed(&mut db, &begun, true, kind).await;
            let before_rows = rows(&db, s).await;
            let before_state = content_state(&mut db, &handle, true).await;
            assert_eq!(before_rows.len(), 1);
            assert_eq!(before_rows[0].3, 0);
            let permit = repost_permit(&mut db, &handle, 0, true).await;
            let handle = applied(
                record_repost(&mut db, &permit, PostVerdict::Posted).await,
                "repost over failed content",
            );
            assert!(
                rows(&db, s).await == before_rows,
                "failed content must not be rewritten"
            );
            assert_eq!(content_state(&mut db, &handle, true).await, before_state);
        }
    });
}

#[test]
fn postgres_repost_invalidation_preserves_historical_rounds_and_the_other_side() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;

        // Round one: intact evidence on both sides, presence finished.
        let handle = seed(&mut db, &begun, true, intact(s, true)).await;
        let handle = seed(&mut db, &handle, false, intact(s, false)).await;
        let done = finished(&mut db, handle).await;
        let round_one = rows(&db, s).await;
        assert_eq!(round_one.len(), 2);

        // Round two: fresh intact evidence on both sides.
        let second = applied(
            begin_verification_round(&mut db, &done).await,
            "begin second round",
        );
        let handle = seed(&mut db, &second, true, intact(s, true)).await;
        let handle = seed(&mut db, &handle, false, intact(s, false)).await;
        let both = rows(&db, s).await;
        assert_eq!(both.len(), 4);
        assert!(
            both[..2] == round_one[..],
            "round one evidence is retained as it was"
        );
        let before_snap = snapshot(&db, s).await;

        let permit = repost_permit(&mut db, &handle, 0, true).await;
        let handle = applied(
            record_repost(&mut db, &permit, PostVerdict::Posted).await,
            "round two repost",
        );

        let after = rows(&db, s).await;
        assert_eq!(after.len(), 4, "no row may be added or removed");
        assert!(
            after[..2] == round_one[..],
            "historical round evidence must be preserved exactly"
        );
        let current = row_of(&after, 2, true).expect("current post row");
        assert_eq!(current.2, "unavailable");
        assert_eq!(current.3, 0);
        assert!(
            row_of(&after, 2, false) == row_of(&both, 2, false),
            "the other side of the current round must be preserved"
        );
        assert_eq!(
            content_state(&mut db, &handle, true).await,
            Some(ContentCheckState::Unavailable)
        );
        assert_eq!(
            content_state(&mut db, &handle, false).await,
            Some(ContentCheckState::Intact)
        );
        assert!(untouched(&snapshot(&db, s).await) == untouched(&before_snap));
        let (state, verified, retained, round) = health(&db, s).await;
        assert_eq!(state, "verifying");
        assert!(verified.is_none() && retained.is_none());
        assert_eq!(round, 2);
    });
}

#[test]
fn postgres_stale_permits_and_duplicate_callbacks_cannot_resurrect_invalidated_content() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let handle = seed(&mut db, &begun, true, intact(s, true)).await;

        // Two repost permits and one content permit, all at the same fence.
        let first = repost_permit(&mut db, &handle, 0, true).await;
        let duplicate = repost_permit(&mut db, &handle, 0, true).await;
        let old_content = match prepare_content_check(&mut db, &handle, ArticleSide::Post).await {
            Ok(Some(permit)) => permit,
            _ => panic!("expected a content-check permit"),
        };
        let new_handle = applied(
            record_repost(&mut db, &first, PostVerdict::Posted).await,
            "first repost",
        );
        let invalidated = rows(&db, s).await;
        let snap = snapshot(&db, s).await;
        let rev = health_revision(&db, s).await;
        let prog = progress(&db, s).await;
        assert_eq!(invalidated[0].2, "unavailable");

        // A duplicate callback, whatever its verdict, neither counts again nor
        // touches evidence; the same permit replayed is stale too.
        for verdict in [
            PostVerdict::Posted,
            PostVerdict::DuplicatePresent,
            PostVerdict::Transport,
        ] {
            assert!(
                is_stale(&record_repost(&mut db, &duplicate, verdict).await),
                "a duplicate callback must be stale"
            );
        }
        assert!(is_stale(
            &record_repost(&mut db, &first, PostVerdict::DuplicatePresent).await
        ));

        // A content permit minted before the repost cannot re-record the old
        // Intact result, nor any other, over the invalidation.
        assert!(is_stale(
            &record_content_check(&mut db, &old_content, intact(s, true)).await
        ));
        assert!(is_stale(
            &record_content_check(&mut db, &old_content, ContentCheckResult::IntegrityFailed).await
        ));
        assert!(prep_stale(
            &prepare_repost(&mut db, &handle, 0, ArticleSide::Post).await
        ));
        assert!(matches!(
            prepare_content_check(&mut db, &handle, ArticleSide::Post).await,
            Ok(None)
        ));
        assert!(matches!(load_checkpoint(&mut db, &handle).await, Ok(None)));

        assert!(
            rows(&db, s).await == invalidated,
            "stale callbacks must not resurrect or rewrite evidence"
        );
        assert!(
            snapshot(&db, s).await == snap,
            "stale callbacks must not write"
        );
        assert_eq!(health_revision(&db, s).await, rev);
        assert!(progress(&db, s).await == prog, "no double counting");
        assert_eq!(
            content_state(&mut db, &new_handle, true).await,
            Some(ContentCheckState::Unavailable)
        );

        // Only a fresh read at the current fence may record new Intact
        // evidence; it then stands as ordinary current evidence.
        let renewed = seed(&mut db, &new_handle, true, intact(s, true)).await;
        assert_eq!(
            content_state(&mut db, &renewed, true).await,
            Some(ContentCheckState::Intact)
        );
        assert!(is_stale(
            &record_repost(&mut db, &duplicate, PostVerdict::Posted).await
        ));
        assert_eq!(
            content_state(&mut db, &renewed, true).await,
            Some(ContentCheckState::Intact),
            "a stale repost must not invalidate fresh evidence"
        );
    });
}

#[test]
fn postgres_lost_ack_recovery_and_takeover_preserve_the_invalidation() {
    run_test(async {
        let mut db = connect().await;

        // Lost acknowledgment: the commit happened, only the reply is lost.
        let (c, attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let handle = seed(&mut db, &begun, true, intact(s, true)).await;
        let handle = seed(&mut db, &handle, false, intact(s, false)).await;
        let permit = repost_permit(&mut db, &handle, 0, true).await;
        let _lost = record_repost(&mut db, &permit, PostVerdict::Posted).await;
        let fresh = fresh_handle(&mut db, &attempt, s).await;
        assert_eq!(
            content_state(&mut db, &fresh, true).await,
            Some(ContentCheckState::Unavailable),
            "recovery must observe the committed invalidation"
        );
        assert_eq!(
            content_state(&mut db, &fresh, false).await,
            Some(ContentCheckState::Intact)
        );
        let rev = health_revision(&db, s).await;
        let kept = rows(&db, s).await;
        let snap = snapshot(&db, s).await;
        let prog = progress(&db, s).await;

        // Replaying the lost callback neither counts nor changes evidence.
        assert!(is_stale(
            &record_repost(&mut db, &permit, PostVerdict::Posted).await
        ));
        assert!(prep_stale(
            &prepare_repost(&mut db, &handle, 0, ArticleSide::Post).await
        ));
        assert!(rows(&db, s).await == kept);
        assert!(snapshot(&db, s).await == snap);
        assert!(progress(&db, s).await == prog);
        assert_eq!(health_revision(&db, s).await, rev);

        // A fresh permit counts again; the invalidation simply stays.
        let permit = repost_permit(&mut db, &fresh, 0, true).await;
        let fresh = applied(
            record_repost(&mut db, &permit, PostVerdict::DuplicatePresent).await,
            "repost after recovery",
        );
        assert_eq!(health_revision(&db, s).await, rev + 1);
        assert_eq!(
            content_state(&mut db, &fresh, true).await,
            Some(ContentCheckState::Unavailable)
        );
        assert_eq!(
            content_state(&mut db, &fresh, false).await,
            Some(ContentCheckState::Intact)
        );
        assert!(progress(&db, s).await == vec![(1, 0, 0, false, 2)]);

        // Takeover: the invalidated side survives, the old claim is dead,
        // and the new claim can invalidate the other side in turn.
        let (c, _first, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let handle = seed(&mut db, &begun, true, intact(s, true)).await;
        let handle = seed(&mut db, &handle, false, intact(s, false)).await;
        let permit = repost_permit(&mut db, &handle, 1, false).await;
        let old_handle = applied(
            record_repost(&mut db, &permit, PostVerdict::Posted).await,
            "repost before takeover",
        );
        let old_permit = repost_permit(&mut db, &old_handle, 2, true).await;
        expire_claim(&db, &c).await;
        let next = acquire(&mut db, &c.job, &c.owner, 1).await;
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");
        let taken = match take_over(&mut db, &next, &expected, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(handle, _)) => handle,
            _ => panic!("takeover did not resume the candidate"),
        };
        assert_eq!(
            content_state(&mut db, &taken, false).await,
            Some(ContentCheckState::Unavailable),
            "takeover must preserve the invalidation"
        );
        assert_eq!(
            content_state(&mut db, &taken, true).await,
            Some(ContentCheckState::Intact)
        );
        let kept = rows(&db, s).await;
        let prog = progress(&db, s).await;
        assert!(is_stale(
            &record_repost(&mut db, &old_permit, PostVerdict::Posted).await
        ));
        assert!(prep_stale(
            &prepare_repost(&mut db, &old_handle, 2, ArticleSide::Post).await
        ));
        assert!(
            rows(&db, s).await == kept,
            "the old claim must not change evidence"
        );
        assert!(progress(&db, s).await == prog);

        let permit = repost_permit(&mut db, &taken, 2, true).await;
        let taken = applied(
            record_repost(&mut db, &permit, PostVerdict::Posted).await,
            "repost after takeover",
        );
        assert_eq!(
            content_state(&mut db, &taken, true).await,
            Some(ContentCheckState::Unavailable)
        );
        assert_eq!(
            content_state(&mut db, &taken, false).await,
            Some(ContentCheckState::Unavailable)
        );
        assert_eq!(health(&db, s).await.0, "verifying");
    });
}

#[test]
fn postgres_invalid_stored_content_identity_rejects_the_repost_and_rolls_back_accounting() {
    run_test(async {
        let mut db = connect().await;

        // (label, stored side, corrupt assignment, repair assignment,
        // reposted side). The row may belong to either side: the whole round
        // is validated, not only the row about to be invalidated.
        let variants: [(&'static str, bool, &'static str, &'static str, bool); 4] = [
            (
                "foreign backbone on the other side",
                false,
                "backbone = 'pool-z'",
                "backbone = 'pool-b'",
                true,
            ),
            (
                "foreign backbone on the reposted side",
                true,
                "backbone = 'pool-z'",
                "backbone = 'pool-a'",
                true,
            ),
            (
                "short article count on the other side",
                false,
                "checked_articles = 2",
                "checked_articles = 3",
                true,
            ),
            (
                "short article count on the reposted side",
                true,
                "checked_articles = 2",
                "checked_articles = 3",
                true,
            ),
        ];
        for (label, stored_post, corrupt, repair, repost_post) in variants {
            let (c, _attempt, begun) = verifying(&mut db).await;
            let s = &c.sealed;
            let handle = seed(&mut db, &begun, stored_post, intact(s, stored_post)).await;
            let permit = repost_permit(&mut db, &handle, 0, repost_post).await;

            db.execute(
                &format!(
                    "UPDATE posted_file_content_checks SET {corrupt} \
                     WHERE generation_id = $1 AND side = $2::int4::smallint"
                ),
                &[&s.generation, &side_code(stored_post)],
            )
            .await
            .quiet("corrupt stored content identity");
            let corrupted_rows = rows(&db, s).await;
            let snap = snapshot(&db, s).await;
            let rev = health_revision(&db, s).await;
            assert!(progress(&db, s).await.is_empty());

            let outcome = record_repost(&mut db, &permit, PostVerdict::Posted).await;
            assert!(
                outcome.is_err(),
                "{label}: a corrupted content row must reject the repost"
            );
            assert!(
                progress(&db, s).await.is_empty(),
                "{label}: accounting must roll back"
            );
            assert!(
                rows(&db, s).await == corrupted_rows,
                "{label}: evidence must be left untouched"
            );
            assert!(
                snapshot(&db, s).await == snap,
                "{label}: no bundle write may survive"
            );
            assert_eq!(
                health_revision(&db, s).await,
                rev,
                "{label}: revision must not move"
            );

            // The rejection burned nothing: once the row is repaired, the
            // very same permit still applies and invalidates.
            db.execute(
                &format!(
                    "UPDATE posted_file_content_checks SET {repair} \
                     WHERE generation_id = $1 AND side = $2::int4::smallint"
                ),
                &[&s.generation, &side_code(stored_post)],
            )
            .await
            .quiet("repair stored content identity");
            let handle = applied(
                record_repost(&mut db, &permit, PostVerdict::Posted).await,
                "repost after repair",
            );
            assert_eq!(health_revision(&db, s).await, rev + 1, "{label}");
            assert_eq!(progress(&db, s).await.len(), 1, "{label}");
            if stored_post == repost_post {
                assert_eq!(
                    content_state(&mut db, &handle, repost_post).await,
                    Some(ContentCheckState::Unavailable),
                    "{label}: repaired row must be invalidated"
                );
            } else {
                assert_eq!(
                    content_state(&mut db, &handle, stored_post).await,
                    Some(ContentCheckState::Intact),
                    "{label}: other side must be preserved"
                );
            }
        }
    });
}

#[test]
fn postgres_concurrent_answered_reposts_invalidate_and_count_exactly_once() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        let handle = seed(&mut db, &begun, true, intact(s, true)).await;
        let permit = repost_permit(&mut db, &handle, 0, true).await;
        let revision = health_revision(&db, s).await;
        let before = snapshot(&db, s).await;
        let schedule: String = db
            .query_one(
                "SELECT md5(t::text) FROM posted_file_recheck_schedule t \
                 WHERE generation_id = $1 AND round = 1",
                &[&s.generation],
            )
            .await
            .quiet("read original schedule")
            .get(0);
        let mut other = connect().await;
        let (left, right) = tokio::join!(
            record_repost(&mut db, &permit, PostVerdict::Posted),
            record_repost(&mut other, &permit, PostVerdict::DuplicatePresent),
        );
        assert!(
            matches!(
                (&left, &right),
                (
                    Ok(MutationOutcome::Applied(_)),
                    Ok(MutationOutcome::StaleOrDiverged)
                ) | (
                    Ok(MutationOutcome::StaleOrDiverged),
                    Ok(MutationOutcome::Applied(_))
                )
            ),
            "one callback must apply and the other must be stale"
        );
        let current = fresh_handle(&mut db, &attempt, s).await;
        assert_eq!(health_revision(&db, s).await, revision + 1);
        assert!(progress(&db, s).await == vec![(1, 0, 0, false, 1)]);
        assert_eq!(
            content_state(&mut db, &current, true).await,
            Some(ContentCheckState::Unavailable)
        );
        assert!(untouched(&snapshot(&db, s).await) == untouched(&before));
        let unchanged_schedule: String = db
            .query_one(
                "SELECT md5(t::text) FROM posted_file_recheck_schedule t \
                 WHERE generation_id = $1 AND round = 1",
                &[&s.generation],
            )
            .await
            .quiet("read unchanged schedule")
            .get(0);
        assert!(
            schedule == unchanged_schedule,
            "repair must not reset scheduling"
        );
    });
}
