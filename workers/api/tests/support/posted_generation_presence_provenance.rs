//! Real-PostgreSQL regressions for durable presence (`STAT`) provenance on
//! `posted_file_article_progress` (`presence_verdict`,
//! `presence_repost_attempts`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`; it reuses that
//! harness and carries its own small helpers. Fixtures are synthetic sealed
//! buffers keyed by database UUIDs. Failures print only static labels:
//! outcomes and verdicts are matched, never formatted, and database errors are
//! only tested for being errors, so no assertion can echo handles, permits,
//! digests, encrypted material, query text or driver errors.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, confirm_article, finish_verification, load_checkpoint,
    prepare_presence_check, prepare_repost, record_presence_check, record_repost, take_over,
    ArticleSide, GenerationHandle, MutationOutcome, PresenceCheckPermit, RepostPreparation,
};
use nntune_ingest::two_spool::{PostVerdict, StatVerdict};

fn side_of(post: bool) -> ArticleSide {
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
        "record presence",
    )
}

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

/// Checkpoint presence for one cell, as a static tag: `None`, "present",
/// "missing" or "unavailable".
async fn tag(
    db: &mut Client,
    handle: &GenerationHandle,
    slot: usize,
    post: bool,
) -> Option<&'static str> {
    let cp = match load_checkpoint(db, handle).await {
        Ok(Some(cp)) => cp,
        _ => panic!("expected a current checkpoint"),
    };
    match cp
        .slot(slot, side_of(post))
        .expect("slot inside the sealed plan")
        .presence
    {
        None => None,
        Some(StatVerdict::Present) => Some("present"),
        Some(StatVerdict::Missing) => Some("missing"),
        Some(StatVerdict::Transport) => Some("unavailable"),
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

async fn health_revision(db: &Client, s: &Sealed) -> i64 {
    db.query_one(
        "SELECT revision FROM posted_file_health WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("read health revision")
    .get(0)
}

async fn repost_outcome(
    db: &mut Client,
    handle: &GenerationHandle,
    slot: i32,
    post: bool,
    verdict: PostVerdict,
) -> Result<MutationOutcome, StoreError> {
    let p = match prepare_repost(db, handle, slot, side_of(post)).await {
        Ok(RepostPreparation::Ready(p)) => *p,
        _ => panic!("expected a ready repost permit"),
    };
    record_repost(db, &p, verdict).await
}

/// Stored `(presence_verdict, presence_repost_attempts, repost_attempts)` of
/// the first row, or `None` when no row exists.
async fn stored(db: &Client, s: &Sealed) -> Option<(Option<String>, Option<i32>, i32)> {
    db.query(
        "SELECT presence_verdict, presence_repost_attempts, repost_attempts \
         FROM posted_file_article_progress WHERE generation_id = $1 \
         ORDER BY round, slot, side LIMIT 1",
        &[&s.generation],
    )
    .await
    .quiet("read stored provenance")
    .first()
    .map(|r| (r.get(0), r.get(1), r.get(2)))
}

#[test]
fn postgres_changed_negative_provenance_retires_older_callbacks_but_identical_does_not() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;

        let handle = observe_slot(&mut db, &begun, 0, true, StatVerdict::Missing).await;
        assert_eq!(tag(&mut db, &handle, 0, true).await, Some("missing"));
        assert!(!confirmed(&mut db, &handle, 0, true).await);

        // Two permits at one revision: an identical fresh Missing is a no-op
        // that leaves the sibling permit usable; a changed Transport applies
        // once and retires every older callback, in both directions.
        let identical = permit(&mut db, &handle, 0, true).await;
        let changed = permit(&mut db, &handle, 0, true).await;
        let rev = health_revision(&db, s).await;
        let _ = applied(
            record_presence_check(&mut db, &identical, StatVerdict::Missing).await,
            "identical missing",
        );
        assert_eq!(health_revision(&db, s).await, rev);
        let handle = applied(
            record_presence_check(&mut db, &changed, StatVerdict::Transport).await,
            "missing to transport",
        );
        assert_eq!(health_revision(&db, s).await, rev + 1);
        assert_eq!(tag(&mut db, &handle, 0, true).await, Some("unavailable"));
        for verdict in [
            StatVerdict::Missing,
            StatVerdict::Transport,
            StatVerdict::Present,
        ] {
            assert!(is_stale(
                &record_presence_check(&mut db, &identical, verdict).await
            ));
            assert!(is_stale(
                &record_presence_check(&mut db, &changed, verdict).await
            ));
        }
        assert_eq!(health_revision(&db, s).await, rev + 1);
        assert_eq!(tag(&mut db, &handle, 0, true).await, Some("unavailable"));

        // And back: Transport to Missing retires the Transport-era permit.
        let old = permit(&mut db, &handle, 0, true).await;
        let handle = observe_slot(&mut db, &handle, 0, true, StatVerdict::Missing).await;
        assert!(is_stale(
            &record_presence_check(&mut db, &old, StatVerdict::Transport).await
        ));
        assert_eq!(tag(&mut db, &handle, 0, true).await, Some("missing"));
        assert_eq!(health_revision(&db, s).await, rev + 2);
    });
}

#[test]
fn postgres_accounted_repost_makes_old_negative_unavailable_until_a_fresh_stat() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;

        let handle = observe_slot(&mut db, &begun, 1, false, StatVerdict::Missing).await;
        assert_eq!(tag(&mut db, &handle, 1, false).await, Some("missing"));

        // Verdicts that spend no budget are Incomplete and leave the
        // observation (and the handle) current.
        let mut handle = handle;
        for verdict in [PostVerdict::Transport, PostVerdict::Refused] {
            let outcome = repost_outcome(&mut db, &handle, 1, false, verdict).await;
            assert!(
                matches!(outcome, Ok(MutationOutcome::Incomplete)),
                "a non-spending repost verdict must be incomplete"
            );
            assert_eq!(
                tag(&mut db, &handle, 1, false).await,
                Some("missing"),
                "a non-spending repost verdict must not stale the observation"
            );
        }
        let row = stored(&db, s).await.expect("progress row");
        assert!(row.0.is_some() && row.1 == Some(row.2) && row.2 == 0);

        // An answered repost increments the cell count: the older STAT no
        // longer describes the article and is not served as provenance.
        handle = applied(
            repost_outcome(&mut db, &handle, 1, false, PostVerdict::Posted).await,
            "answered repost",
        );
        assert_eq!(tag(&mut db, &handle, 1, false).await, None);
        let row = stored(&db, s).await.expect("progress row");
        assert!(
            row.2 == 1 && row.1 == Some(0),
            "snapshot keeps the old count"
        );

        // A fresh STAT at the new count restores provenance and re-snapshots.
        handle = observe_slot(&mut db, &handle, 1, false, StatVerdict::Transport).await;
        assert_eq!(tag(&mut db, &handle, 1, false).await, Some("unavailable"));
        let row = stored(&db, s).await.expect("progress row");
        assert!(row.1 == Some(1) && row.2 == 1);
        handle = observe_slot(&mut db, &handle, 1, false, StatVerdict::Present).await;
        assert_eq!(tag(&mut db, &handle, 1, false).await, Some("present"));
        assert!(confirmed(&mut db, &handle, 1, false).await);
    });
}

#[test]
fn postgres_direct_confirmation_discards_negative_provenance_and_never_forges_present() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;

        // Direct confirmation of an untouched cell records no provenance.
        let mut handle = applied(
            confirm_article(&mut db, &begun, 2, ArticleSide::Verify).await,
            "confirm untouched",
        );
        assert!(confirmed(&mut db, &handle, 2, false).await);
        assert_eq!(tag(&mut db, &handle, 2, false).await, None);

        for (slot, verdict) in [(0, StatVerdict::Missing), (1, StatVerdict::Transport)] {
            handle = observe_slot(&mut db, &handle, slot, true, verdict).await;
            assert!(tag(&mut db, &handle, slot as usize, true).await.is_some());
            handle = applied(
                confirm_article(&mut db, &handle, slot, ArticleSide::Post).await,
                "confirm over negative",
            );
            assert!(confirmed(&mut db, &handle, slot as usize, true).await);
            assert_eq!(
                tag(&mut db, &handle, slot as usize, true).await,
                None,
                "confirmation must not keep, or turn into, a STAT verdict"
            );
        }

        // A real Present is distinguishable from confirmation. Confirming a
        // cell that is already confirmed is a no-op and preserves it.
        handle = observe_slot(&mut db, &handle, 0, false, StatVerdict::Present).await;
        assert_eq!(tag(&mut db, &handle, 0, false).await, Some("present"));
        handle = applied(
            confirm_article(&mut db, &handle, 0, ArticleSide::Verify).await,
            "no-op confirmation",
        );
        assert!(confirmed(&mut db, &handle, 0, false).await);
        assert_eq!(
            tag(&mut db, &handle, 0, false).await,
            Some("present"),
            "a no-op confirmation must keep a legitimate Present"
        );
        let rows = db
            .query(
                "SELECT presence_verdict, presence_repost_attempts \
                 FROM posted_file_article_progress \
                 WHERE generation_id = $1 AND confirmed AND presence_verdict IS NULL",
                &[&s.generation],
            )
            .await
            .quiet("read confirmed rows")
            .len();
        assert_eq!(rows, 3, "confirmed-without-verdict rows keep both NULL");
    });
}

#[test]
fn postgres_new_round_is_empty_and_takeover_keeps_typed_observation() {
    run_test(async {
        let mut db = connect().await;

        // New round: observations from the finished round do not carry over.
        let (_c, _attempt, begun) = verifying(&mut db).await;
        let mut handle = observe_slot(&mut db, &begun, 0, true, StatVerdict::Missing).await;
        handle = observe_slot(&mut db, &handle, 0, true, StatVerdict::Present).await;
        for slot in 0..3 {
            for post in [true, false] {
                if !(slot == 0 && post) {
                    handle = applied(
                        confirm_article(&mut db, &handle, slot, side_of(post)).await,
                        "confirm fixture",
                    );
                }
            }
        }
        let done = applied(finish_verification(&mut db, &handle).await, "finish");
        assert_eq!(tag(&mut db, &done, 0, true).await, Some("present"));
        let second = applied(
            begin_verification_round(&mut db, &done).await,
            "begin second round",
        );
        for slot in 0..3 {
            for post in [true, false] {
                assert_eq!(tag(&mut db, &second, slot, post).await, None);
                assert!(!confirmed(&mut db, &second, slot, post).await);
            }
        }

        // Takeover: the durable typed observation survives, old permits die.
        let c = case(&db).await;
        let s = &c.sealed;
        let first_attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let admitted_handle = admitted(&mut db, &first_attempt, s).await;
        let begun = applied(
            begin_verification_round(&mut db, &admitted_handle).await,
            "begin round",
        );
        let old_handle = observe_slot(&mut db, &begun, 1, false, StatVerdict::Transport).await;
        let old_permit = permit(&mut db, &old_handle, 1, false).await;
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
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");
        let new_handle = match take_over(&mut db, &next_attempt, &expected, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(handle, _)) => handle,
            _ => panic!("takeover did not resume the candidate"),
        };
        assert_eq!(
            tag(&mut db, &new_handle, 1, false).await,
            Some("unavailable")
        );
        assert!(!confirmed(&mut db, &new_handle, 1, false).await);
        assert!(is_stale(
            &record_presence_check(&mut db, &old_permit, StatVerdict::Missing).await
        ));
        assert_eq!(
            tag(&mut db, &new_handle, 1, false).await,
            Some("unavailable")
        );
    });
}

#[test]
fn postgres_presence_provenance_columns_reject_invalid_shapes() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;
        observe_slot(&mut db, &begun, 0, true, StatVerdict::Missing).await;
        let row = stored(&db, s).await.expect("progress row");
        assert!(row.0.as_deref() == Some("missing") && row.1 == Some(0));

        // Each statement violates one invariant and must be refused whole.
        for (label, statement) in [
            ("unknown verdict", "SET presence_verdict = 'bogus'"),
            (
                "verdict without snapshot",
                "SET presence_repost_attempts = NULL",
            ),
            ("snapshot without verdict", "SET presence_verdict = NULL"),
            ("negative snapshot", "SET presence_repost_attempts = -1"),
            (
                "snapshot above count",
                "SET presence_repost_attempts = repost_attempts + 1",
            ),
            (
                "present while unconfirmed",
                "SET presence_verdict = 'present'",
            ),
            ("confirmed while negative", "SET confirmed = true"),
        ] {
            let outcome = db
                .execute(
                    &format!(
                        "UPDATE posted_file_article_progress {statement} \
                         WHERE generation_id = $1"
                    ),
                    &[&s.generation],
                )
                .await;
            match outcome {
                Ok(_) => panic!("{label}: statement was accepted"),
                Err(e) => {
                    let named = e.as_db_error().is_some_and(|d| {
                        d.code() == &SqlState::CHECK_VIOLATION
                            && d.constraint() == Some("posted_file_presence_provenance")
                    });
                    assert!(named, "{label}: not the provenance constraint");
                }
            }
            let after = stored(&db, s).await.expect("progress row");
            assert!(after == row, "{label}: refused statement changed the row");
        }

        // Valid shapes still go through: a different negative, and the
        // no-provenance state with both columns NULL.
        db.execute(
            "UPDATE posted_file_article_progress SET presence_verdict = 'unavailable' \
             WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("valid negative rewrite");
        db.execute(
            "UPDATE posted_file_article_progress \
             SET presence_verdict = NULL, presence_repost_attempts = NULL, confirmed = true \
             WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("valid unproven confirmed shape");
        db.execute(
            "UPDATE posted_file_article_progress \
             SET presence_verdict = 'present', presence_repost_attempts = 0 \
             WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("valid present shape");
    });
}

#[test]
fn postgres_migration_0041_replay_keeps_legacy_null_rows_and_typed_observations() {
    run_blocking(true, async {
        let mut db = connect().await;
        let (c, _attempt, begun) = verifying(&mut db).await;
        let s = &c.sealed;

        // A legacy-shape row (confirmed, both provenance columns NULL) beside
        // a typed negative observation.
        let handle = applied(
            confirm_article(&mut db, &begun, 1, ArticleSide::Post).await,
            "legacy-shape row",
        );
        let handle = observe_slot(&mut db, &handle, 0, true, StatVerdict::Missing).await;
        let dump = |rows: Vec<tokio_postgres::Row>| {
            rows.iter()
                .map(|r| {
                    (
                        r.get::<_, i32>(0),
                        r.get::<_, i16>(1),
                        r.get::<_, bool>(2),
                        r.get::<_, Option<String>>(3),
                        r.get::<_, Option<i32>>(4),
                    )
                })
                .collect::<Vec<_>>()
        };
        async fn read(db: &Client, s: &Sealed) -> Vec<tokio_postgres::Row> {
            db.query(
                "SELECT slot, side, confirmed, presence_verdict, presence_repost_attempts \
                 FROM posted_file_article_progress WHERE generation_id = $1 \
                 ORDER BY round, slot, side",
                &[&s.generation],
            )
            .await
            .quiet("read rows")
        }
        let before = dump(read(&db, s).await);
        assert_eq!(before.len(), 2);
        assert!(before.iter().any(|r| r.2 && r.3.is_none() && r.4.is_none()));
        assert!(before
            .iter()
            .any(|r| !r.2 && r.3.is_some() && r.4.is_some()));

        let ddl = connect().await;
        for _ in 0..2 {
            ddl.batch_execute(MIGRATION_0041)
                .await
                .quiet("replay migration 0041");
        }
        assert!(dump(read(&db, s).await) == before);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM pg_constraint \
                 WHERE conname = 'posted_file_presence_provenance'",
                &[],
            )
            .await,
            1,
            "replay must not duplicate the constraint"
        );
        assert_eq!(tag(&mut db, &handle, 0, true).await, Some("missing"));
        assert_eq!(tag(&mut db, &handle, 1, true).await, None);
    });
}
