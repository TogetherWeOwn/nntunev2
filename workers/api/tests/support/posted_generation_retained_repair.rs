//! Real-PostgreSQL tests for the bounded selected-retained repair collector
//! (`start_retained_repair`, `recover_retained_repair`, `drive_retained_repair`).
//! Promotion stays explicitly SEEDED by the parent fixture (`retained`, then a
//! real `invalidate_retained_check` for the negative origin); no promotion path
//! is claimed. Everything the collector itself owns (progress rows, repost
//! budgets, the recheck schedule and the conclusion) is produced ONLY by the real
//! APIs. Time is advanced solely by setting the waiting schedule's `not_before`
//! to zero before a pass; nothing sleeps and no settled evidence is seeded.
//! Assertions count scripted transport calls and compare payload-free enums and
//! scalars, so failures print only static labels and never custody or identities.

use super::*;
use nntune_ingest::posted_generations::{
    drive_retained_repair, recover_retained_repair, start_retained_repair, GenerationTransport,
    RetainedRepairStep, StoredGeneration,
};
use nntune_ingest::two_spool::{PostVerdict, StatVerdict, MAX_RECHECKS};
use std::sync::Arc;
use tokio::sync::Notify;

/// The synthetic sealed bundle has three articles: six STAT cursors per pass.
const CURSORS: usize = 6;
const PASSES: usize = MAX_RECHECKS as usize;
const BUMP_HEALTH: &str =
    "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1";

/// Constant answers plus scalar call counters. Optional hooks run one fixture
/// statement on an independent session in the middle of a transport callback, or
/// park the callback forever after announcing it (cancellation tests).
struct Script {
    stat: StatVerdict,
    post: PostVerdict,
    stats: usize,
    reposts: usize,
    aux: Option<Arc<Client>>,
    generation: String,
    hook_stat: Option<&'static str>,
    hook_post: Option<&'static str>,
    hang_stat: bool,
    hang_post: bool,
    lock_health: bool,
    entered: Arc<Notify>,
}

impl Script {
    fn new(stat: StatVerdict, post: PostVerdict) -> Self {
        Script {
            stat,
            post,
            stats: 0,
            reposts: 0,
            aux: None,
            generation: String::new(),
            hook_stat: None,
            hook_post: None,
            hang_stat: false,
            hang_post: false,
            lock_health: false,
            entered: Arc::new(Notify::new()),
        }
    }

    fn calls(&self) -> (usize, usize) {
        (self.stats, self.reposts)
    }

    async fn with_aux(mut self, f: &Fixture) -> Self {
        self.aux = Some(Arc::new(connect().await));
        self.generation = f.case.sealed.generation.clone();
        self
    }

    async fn hook(&self, sql: &'static str) {
        self.aux
            .as_ref()
            .expect("hook needs an auxiliary session")
            .execute(sql, &[&self.generation])
            .await
            .quiet("run callback-time fixture mutation");
    }
}

impl GenerationTransport for Script {
    async fn stat(&mut self, _b: &StoredGeneration, _slot: i32, _side: ArticleSide) -> StatVerdict {
        self.stats += 1;
        if self.lock_health {
            self.aux
                .as_ref()
                .expect("lock needs auxiliary session")
                .batch_execute("BEGIN; LOCK TABLE posted_file_health IN SHARE MODE")
                .await
                .quiet("hold final health update relation barrier");
        }
        if let Some(sql) = self.hook_stat.take() {
            self.hook(sql).await;
        }
        if self.hang_stat {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        self.stat
    }

    async fn repost(
        &mut self,
        _b: &StoredGeneration,
        _slot: i32,
        _side: ArticleSide,
    ) -> PostVerdict {
        self.reposts += 1;
        if let Some(sql) = self.hook_post.take() {
            self.hook(sql).await;
        }
        if self.hang_post {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        self.post
    }
}

type Step = Result<RetainedRepairStep, StoreError>;

fn progress(step: Step, ctx: &'static str) -> RetainedCheckPermit {
    match step {
        Ok(RetainedRepairStep::Progress(permit)) => *permit,
        _ => panic!("{ctx}: expected repair progress"),
    }
}

fn is_stale(step: &Step) -> bool {
    matches!(step, Ok(RetainedRepairStep::StaleOrDiverged))
}

fn is_waiting(step: &Step) -> bool {
    matches!(step, Ok(RetainedRepairStep::Waiting { .. }))
}

fn is_keep(step: &Step) -> bool {
    matches!(step, Ok(RetainedRepairStep::KeepOriginal))
}

/// Selected retained generation with a real negative withdrawal as its origin.
async fn withdrawn(db: &mut Client) -> Fixture {
    let f = retained(db, true).await;
    let original = serving(db, &f, "prepare negative origin").await;
    assert!(is_applied(&invalidate_retained_check(db, &original).await));
    f
}

/// Withdrawn fixture whose repair schedule was started by the real API.
async fn started(db: &mut Client) -> (Fixture, RetainedCheckPermit) {
    let f = withdrawn(db).await;
    let permit = serving(db, &f, "prepare repair permit").await;
    assert!(
        matches!(start_retained_repair(db, &permit).await, Ok(true)),
        "start repair after negative origin"
    );
    (f, permit)
}

/// The only clock acceleration: make the current waiting schedule due.
async fn accelerate(db: &Client, s: &Sealed) {
    let changed = db
        .execute(
            "UPDATE posted_file_recheck_schedule SET not_before = 0 \
             WHERE generation_id = $1 AND state = 'waiting'",
            &[&s.generation],
        )
        .await
        .quiet("make waiting schedule due");
    assert!(changed == 1, "exactly one waiting schedule accelerated");
}

/// (work_cursor, completed_passes, state) of the newest schedule row.
async fn schedule(db: &Client, s: &Sealed) -> (i32, i32, String) {
    let r = db
        .query_one(
            "SELECT work_cursor, completed_passes, state FROM posted_file_recheck_schedule \
             WHERE generation_id = $1 ORDER BY round DESC LIMIT 1",
            &[&s.generation],
        )
        .await
        .quiet("read schedule");
    (r.get(0), r.get(1), r.get(2))
}

async fn schedule_rows(db: &Client, s: &Sealed) -> i64 {
    db.query_one(
        "SELECT count(*) FROM posted_file_recheck_schedule WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("count schedule rows")
    .get(0)
}

async fn repost_total(db: &Client, s: &Sealed) -> i64 {
    db.query_one(
        "SELECT COALESCE(sum(repost_attempts), 0)::bigint FROM posted_file_article_progress \
         WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("sum spent reposts")
    .get(0)
}

async fn progress_where(db: &Client, s: &Sealed, predicate: &str) -> i64 {
    db.query_one(
        &format!(
            "SELECT count(*) FROM posted_file_article_progress WHERE generation_id = $1 AND {predicate}"
        ),
        &[&s.generation],
    )
    .await
    .quiet("count progress rows")
    .get(0)
}

async fn decisions(db: &Client, s: &Sealed) -> i64 {
    db.query_one(
        "SELECT count(*) FROM posted_file_retained_failures WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("count failure receipts")
    .get(0)
}

/// Run `n` accelerated full passes, chaining each returned permit.
async fn passes(
    db: &mut Client,
    s: &Sealed,
    mut permit: RetainedCheckPermit,
    script: &mut Script,
    n: usize,
) -> RetainedCheckPermit {
    for _ in 0..n {
        accelerate(db, s).await;
        permit = progress(
            drive_retained_repair(db, &permit, script, CURSORS).await,
            "full accelerated pass",
        );
    }
    permit
}

#[test]
fn postgres_retained_repair_start_is_idempotent_refuses_without_origin_and_never_resets_progress() {
    run_test(async {
        let mut db = connect().await;
        // A usable (never withdrawn) generation has no repair authority.
        let f = retained(&mut db, true).await;
        let s = &f.case.sealed;
        let usable = serving(&mut db, &f, "prepare usable permit").await;
        let mut script = Script::new(StatVerdict::Missing, PostVerdict::Posted);
        assert!(matches!(
            start_retained_repair(&mut db, &usable).await,
            Ok(false)
        ));
        assert!(matches!(
            recover_retained_repair(&mut db, &usable).await,
            Ok(None)
        ));
        assert!(is_stale(
            &drive_retained_repair(&mut db, &usable, &mut script, 1).await
        ));
        assert!(schedule_rows(&db, s).await == 0 && script.calls() == (0, 0));

        // Before start there is no schedule: nothing to recover or collect.
        let f = withdrawn(&mut db).await;
        let s = &f.case.sealed;
        let permit = serving(&mut db, &f, "prepare repair permit").await;
        assert!(matches!(
            recover_retained_repair(&mut db, &permit).await,
            Ok(None)
        ));
        assert!(is_keep(
            &drive_retained_repair(&mut db, &permit, &mut script, 1).await
        ));
        assert!(schedule_rows(&db, s).await == 0 && script.calls() == (0, 0));

        // Start creates exactly one waiting, not-yet-due round and is repeatable.
        assert!(matches!(
            start_retained_repair(&mut db, &permit).await,
            Ok(true)
        ));
        let due: bool = db
            .query_one(
                "SELECT not_before > floor(extract(epoch FROM clock_timestamp()))::bigint \
                 FROM posted_file_recheck_schedule WHERE generation_id = $1",
                &[&s.generation],
            )
            .await
            .quiet("read start delay")
            .get(0);
        assert!(due && schedule(&db, s).await == (0, 0, "waiting".into()));
        let before = (evidence(&db, s).await, health(&db, &s.generation).await);
        assert!(matches!(
            start_retained_repair(&mut db, &permit).await,
            Ok(true)
        ));
        assert!(
            before == (evidence(&db, s).await, health(&db, &s.generation).await),
            "second start changes nothing, not even the delay or health revision"
        );
        let waiting = drive_retained_repair(&mut db, &permit, &mut script, 2).await;
        let stored: i64 = db
            .query_one(
                "SELECT not_before FROM posted_file_recheck_schedule WHERE generation_id = $1",
                &[&s.generation],
            )
            .await
            .quiet("read stored not_before")
            .get(0);
        assert!(
            matches!(waiting, Ok(RetainedRepairStep::Waiting { not_before }) if not_before == stored)
                && script.calls() == (0, 0),
            "not-yet-due schedule waits with no IO"
        );

        // Restarting after real progress keeps cursor, budgets and evidence.
        accelerate(&db, s).await;
        let p1 = progress(
            drive_retained_repair(&mut db, &permit, &mut script, 2).await,
            "two slots",
        );
        let mid = evidence(&db, s).await;
        assert!(
            schedule(&db, s).await == (2, 0, "waiting".into()) && repost_total(&db, s).await == 2
        );
        assert!(matches!(
            start_retained_repair(&mut db, &p1).await,
            Ok(true)
        ));
        assert!(
            mid == evidence(&db, s).await,
            "start after progress never resets schedule or budgets"
        );
        assert!(
            matches!(start_retained_repair(&mut db, &permit).await, Ok(false)),
            "the superseded permit cannot start"
        );
    });
}

#[test]
fn postgres_retained_repair_all_missing_answered_reposts_hit_cap_then_five_passes_conclude_failure()
{
    run_test(async {
        let mut db = connect().await;
        let (f, mut permit) = started(&mut db).await;
        let s = &f.case.sealed;
        let mut script = Script::new(StatVerdict::Missing, PostVerdict::Posted);
        let expected_reposts = [6, 12, 18, 18, 18];
        for (pass, expected) in expected_reposts.into_iter().enumerate() {
            let before = script.calls();
            permit = passes(&mut db, s, permit, &mut script, 1).await;
            assert!(
                script.calls() == (before.0 + CURSORS, expected),
                "one STAT per cursor; POST only below the per-article cap"
            );
            assert!(repost_total(&db, s).await == expected as i64);
            if pass + 1 < PASSES {
                // The next pass is not due: a second drive spends nothing.
                let held = script.calls();
                let step = drive_retained_repair(&mut db, &permit, &mut script, CURSORS).await;
                assert!(
                    is_waiting(&step) && script.calls() == held,
                    "propagation wait"
                );
            }
        }
        assert!(schedule(&db, s).await == (0, MAX_RECHECKS as i32, "exhausted".into()));
        assert!(
            progress_where(&db, s, "presence_verdict = 'missing' AND NOT confirmed").await
                == CURSORS as i64
                && progress_where(&db, s, "repost_attempts <> presence_repost_attempts").await == 0,
            "cap reached and the final pass observed every slot still missing"
        );
        assert!(
            health(&db, &s.generation).await.state == "unknown" && decisions(&db, s).await == 0,
            "exhaustion alone concludes nothing until the next drive decides"
        );
        let held = script.calls();
        let done = drive_retained_repair(&mut db, &permit, &mut script, CURSORS).await;
        assert!(matches!(done, Ok(RetainedRepairStep::RetentionFailed)));
        let after = health(&db, &s.generation).await;
        assert!(
            after.state == "terminal_failed" && after.failure_kind.as_deref() == Some("retention"),
            "real retention failure"
        );
        assert!(
            decisions(&db, s).await == 1 && script.calls() == held,
            "decided without IO"
        );
        let replay = drive_retained_repair(&mut db, &permit, &mut script, CURSORS).await;
        assert!(
            matches!(replay, Ok(RetainedRepairStep::RetentionFailed))
                && decisions(&db, s).await == 1
                && script.calls() == held,
            "replay writes nothing and does no IO"
        );
    });
}

#[test]
fn postgres_retained_repair_transport_and_refused_posts_never_spend_or_conclude_failure() {
    run_test(async {
        let scenarios = [
            // (stat, post, expected POST calls over five passes)
            (StatVerdict::Transport, PostVerdict::Posted, 0),
            (StatVerdict::Missing, PostVerdict::Refused, PASSES * CURSORS),
            (
                StatVerdict::Missing,
                PostVerdict::Transport,
                PASSES * CURSORS,
            ),
        ];
        let mut db = connect().await;
        for (stat, post, posts) in scenarios {
            let (f, permit) = started(&mut db).await;
            let s = &f.case.sealed;
            let mut script = Script::new(stat, post);
            let permit = passes(&mut db, s, permit, &mut script, PASSES).await;
            assert!(
                script.calls() == (PASSES * CURSORS, posts),
                "scripted IO counts"
            );
            assert!(
                repost_total(&db, s).await == 0,
                "only answered/duplicate replies spend the cap"
            );
            assert!(schedule(&db, s).await == (0, MAX_RECHECKS as i32, "exhausted".into()));
            let held = script.calls();
            let step = drive_retained_repair(&mut db, &permit, &mut script, CURSORS).await;
            assert!(
                is_keep(&step),
                "exhausted but unproven history keeps the original"
            );
            let h = health(&db, &s.generation).await;
            assert!(
                h.state == "unknown" && h.failure_kind.is_none() && decisions(&db, s).await == 0,
                "no failure manufactured from timers or transport"
            );
            assert!(script.calls() == held, "keep-original does no IO");
        }
    });
}

#[test]
fn postgres_retained_repair_presence_complete_is_not_publication_or_failure_authority() {
    run_test(async {
        let mut db = connect().await;
        let (f, permit) = started(&mut db).await;
        let s = &f.case.sealed;
        let mut script = Script::new(StatVerdict::Present, PostVerdict::Posted);
        let permit = passes(&mut db, s, permit, &mut script, 1).await;
        assert!(progress_where(&db, s, "confirmed").await == CURSORS as i64);
        let step = drive_retained_repair(&mut db, &permit, &mut script, CURSORS).await;
        assert!(matches!(step, Ok(RetainedRepairStep::PresenceComplete)));
        assert!(script.calls() == (CURSORS, 0), "no further IO and no POST");
        let h = health(&db, &s.generation).await;
        assert!(
            h.state == "unknown" && h.verified_at.is_none() && h.retention_checked_at.is_none(),
            "presence grants no usability or retention timestamp"
        );
        assert!(decisions(&db, s).await == 0 && repost_total(&db, s).await == 0);
    });
}

#[test]
fn postgres_retained_repair_bounded_cursor_restarts_and_read_only_recovery_retires_old_callbacks() {
    run_test(async {
        let mut db = connect().await;
        let (f, original) = started(&mut db).await;
        let s = &f.case.sealed;
        let mut script = Script::new(StatVerdict::Missing, PostVerdict::Posted);
        for bad in [0, nntune_ingest::two_spool::STAT_BATCH_SIZE + 1] {
            let step = drive_retained_repair(&mut db, &original, &mut script, bad).await;
            assert!(matches!(step, Err(StoreError::InvalidInput)));
        }
        assert!(script.calls() == (0, 0) && schedule(&db, s).await == (0, 0, "waiting".into()));

        accelerate(&db, s).await;
        let p1 = progress(
            drive_retained_repair(&mut db, &original, &mut script, 2).await,
            "first bounded slice",
        );
        assert!(script.calls() == (2, 2) && schedule(&db, s).await == (2, 0, "waiting".into()));

        // Restart recovery from the pre-progress permit is read-only.
        let before = (frozen(&db, s).await, health(&db, &s.generation).await);
        let recovered = recover_retained_repair(&mut db, &original).await;
        assert!(
            before == (frozen(&db, s).await, health(&db, &s.generation).await),
            "recovery writes nothing"
        );
        let Ok(Some(refreshed)) = recovered else {
            panic!("recover the same repair round after a revision advance");
        };
        assert!(is_stale(
            &drive_retained_repair(&mut db, &original, &mut script, 2).await
        ));
        assert!(
            script.calls() == (2, 2),
            "the superseded permit stops before IO"
        );
        // The recovered and the returned permit are the same current fence; the
        // first to commit wins and retires the other.
        let p2 = progress(
            drive_retained_repair(&mut db, &refreshed, &mut script, 2).await,
            "second bounded slice",
        );
        assert!(script.calls() == (4, 4) && schedule(&db, s).await == (4, 0, "waiting".into()));
        assert!(is_stale(
            &drive_retained_repair(&mut db, &p1, &mut script, 2).await
        ));
        assert!(script.calls() == (4, 4));
        let p3 = progress(
            drive_retained_repair(&mut db, &p2, &mut script, 2).await,
            "final slice completes the pass",
        );
        assert!(script.calls() == (6, 6) && schedule(&db, s).await == (0, 1, "waiting".into()));
        assert!(is_waiting(
            &drive_retained_repair(&mut db, &p3, &mut script, 2).await
        ));
        assert!(
            repost_total(&db, s).await == CURSORS as i64,
            "one spend per slot, none replayed"
        );
    });
}

#[test]
fn postgres_retained_repair_selection_aba_and_reference_recreation_block_everything() {
    run_test(async {
        let mut db = connect().await;
        for reference_change in [false, true] {
            let (f, original) = started(&mut db).await;
            let s = &f.case.sealed;
            accelerate(&db, s).await;
            if reference_change {
                reinsert_reference(&db, s, &f.owner).await;
            } else {
                db.execute("UPDATE posted_file_registry SET selected_generation = NULL, revision = revision + 1 WHERE plaintext_sha256 = $1", &[&s.hash]).await.quiet("fixture deselect");
                db.execute("UPDATE posted_file_registry SET selected_generation = $2, revision = revision + 1 WHERE plaintext_sha256 = $1", &[&s.hash, &s.generation]).await.quiet("fixture reselect");
            }
            let before = (frozen(&db, s).await, health(&db, &s.generation).await);
            let mut script = Script::new(StatVerdict::Missing, PostVerdict::Posted);
            assert!(is_stale(
                &drive_retained_repair(&mut db, &original, &mut script, CURSORS).await
            ));
            assert!(matches!(
                recover_retained_repair(&mut db, &original).await,
                Ok(None)
            ));
            assert!(matches!(
                start_retained_repair(&mut db, &original).await,
                Ok(false)
            ));
            // A permit for the new incarnation does not inherit the old origin.
            let fresh = serving(&mut db, &f, "prepare after incarnation change").await;
            assert!(matches!(
                start_retained_repair(&mut db, &fresh).await,
                Ok(false)
            ));
            assert!(is_stale(
                &drive_retained_repair(&mut db, &fresh, &mut script, CURSORS).await
            ));
            assert!(
                script.calls() == (0, 0),
                "no transport work on a retired fence"
            );
            assert!(
                before == (frozen(&db, s).await, health(&db, &s.generation).await)
                    && decisions(&db, s).await == 0,
                "retired incarnations write nothing"
            );
        }
    });
}

#[test]
fn postgres_retained_repair_health_mutation_during_stat_or_post_stales_callback_and_keeps_state() {
    run_test(async {
        let mut db = connect().await;

        // Health advances while STAT is outstanding: the answer is discarded.
        let (f, original) = started(&mut db).await;
        let s = &f.case.sealed;
        accelerate(&db, s).await;
        let (ev, rev) = (
            evidence(&db, s).await,
            health(&db, &s.generation).await.revision,
        );
        let mut script = Script::new(StatVerdict::Missing, PostVerdict::Posted)
            .with_aux(&f)
            .await;
        script.hook_stat = Some(BUMP_HEALTH);
        assert!(is_stale(
            &drive_retained_repair(&mut db, &original, &mut script, CURSORS).await
        ));
        assert!(script.calls() == (1, 0), "stopped at the first callback");
        assert!(health(&db, &s.generation).await.revision == rev + 1);
        assert!(
            ev == evidence(&db, s).await,
            "no answer, cursor or budget recorded"
        );
        assert!(is_stale(
            &drive_retained_repair(&mut db, &original, &mut script, 1).await
        ));
        assert!(script.calls() == (1, 0), "old permit stops before IO");
        let Ok(Some(refreshed)) = recover_retained_repair(&mut db, &original).await else {
            panic!("recover after unrelated revision advance");
        };
        progress(
            drive_retained_repair(&mut db, &refreshed, &mut script, 1).await,
            "recovered permit continues",
        );

        // Health advances while POST is outstanding: the spend is not recorded.
        let (f, original) = started(&mut db).await;
        let s = &f.case.sealed;
        accelerate(&db, s).await;
        let rev = health(&db, &s.generation).await.revision;
        let mut script = Script::new(StatVerdict::Missing, PostVerdict::Posted)
            .with_aux(&f)
            .await;
        script.hook_post = Some(BUMP_HEALTH);
        assert!(is_stale(
            &drive_retained_repair(&mut db, &original, &mut script, CURSORS).await
        ));
        assert!(script.calls() == (1, 1));
        assert!(
            health(&db, &s.generation).await.revision == rev + 2,
            "committed STAT phase plus the concurrent bump only"
        );
        assert!(repost_total(&db, s).await == 0 && schedule(&db, s).await.0 == 0);
        assert!(progress_where(&db, s, "presence_verdict = 'missing'").await == 1);
        let Ok(Some(refreshed)) = recover_retained_repair(&mut db, &original).await else {
            panic!("recover after post-time revision advance");
        };
        progress(
            drive_retained_repair(&mut db, &refreshed, &mut script, 1).await,
            "replayed phase",
        );
        assert!(
            repost_total(&db, s).await == 1
                && progress_where(&db, s, "slot = 0 AND side = 0 AND repost_attempts = 1").await
                    == 1,
            "the cap is spent once by the replay, never twice"
        );
    });
}

#[test]
fn postgres_retained_repair_expiry_behind_final_health_update_rolls_back_evidence() {
    run_blocking(true, async {
        let mut db = connect().await;
        let observer = connect().await;
        let (f, original) = started(&mut db).await;
        let s = &f.case.sealed;
        accelerate(&db, s).await;
        let deadline: i64 = db.query_one(
            "UPDATE posted_file_job_attempts SET lease_until = floor(extract(epoch FROM clock_timestamp()))::bigint + 3 WHERE job_id = $1 RETURNING lease_until",
            &[&f.case.job],
        ).await.quiet("short live attempt").get(0);
        let before = frozen(&db, s).await;
        let pid: i32 = db
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .quiet("worker pid")
            .get(0);
        let mut script = Script::new(StatVerdict::Present, PostVerdict::Posted)
            .with_aux(&f)
            .await;
        script.lock_health = true;
        let blocker = script.aux.as_ref().expect("auxiliary blocker").clone();
        let (outcome, ()) = tokio::join!(
            drive_retained_repair(&mut db, &original, &mut script, 1),
            async {
                loop {
                    let blocked: bool = observer.query_one(
                        "SELECT cardinality(pg_blocking_pids($1)) > 0 AND query LIKE 'UPDATE posted_file_health SET revision%' FROM pg_stat_activity WHERE pid = $1",
                        &[&pid],
                    ).await.quiet("observe final update wait").get(0);
                    if blocked {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                observer.query_one(
                    "SELECT pg_sleep(GREATEST(0.0, $1::bigint::double precision - extract(epoch FROM clock_timestamp())::double precision) + 1.0)",
                    &[&deadline],
                ).await.quiet("expire behind final relation barrier");
                blocker
                    .batch_execute("COMMIT")
                    .await
                    .quiet("release final update barrier");
            }
        );
        assert!(
            is_stale(&outcome),
            "expired callback refused after final update wait"
        );
        assert!(script.calls() == (1, 0), "one original read and no repost");
        assert!(
            before == frozen(&db, s).await,
            "all evidence and revision changes rolled back"
        );
    });
}

#[test]
fn postgres_retained_repair_cancelled_callbacks_record_no_answer_and_keep_the_fence() {
    run_test(async {
        let mut db = connect().await;

        // Cancel while STAT is parked: nothing at all was recorded.
        let (f, original) = started(&mut db).await;
        let s = &f.case.sealed;
        accelerate(&db, s).await;
        let before = (frozen(&db, s).await, health(&db, &s.generation).await);
        let mut script = Script::new(StatVerdict::Missing, PostVerdict::Posted);
        script.hang_stat = true;
        let entered = script.entered.clone();
        tokio::select! {
            _ = drive_retained_repair(&mut db, &original, &mut script, CURSORS) => {
                panic!("a parked STAT cannot finish");
            }
            _ = entered.notified() => {}
        }
        assert!(script.calls() == (1, 0));
        assert!(
            before == (frozen(&db, s).await, health(&db, &s.generation).await),
            "cancellation before an answer changes nothing"
        );
        let mut fresh = Script::new(StatVerdict::Missing, PostVerdict::Posted);
        progress(
            drive_retained_repair(&mut db, &original, &mut fresh, 1).await,
            "same permit still holds the fence",
        );

        // Cancel while POST is parked: the committed STAT phase stays, no spend.
        let (f, original) = started(&mut db).await;
        let s = &f.case.sealed;
        accelerate(&db, s).await;
        let rev = health(&db, &s.generation).await.revision;
        let mut script = Script::new(StatVerdict::Missing, PostVerdict::Posted);
        script.hang_post = true;
        let entered = script.entered.clone();
        tokio::select! {
            _ = drive_retained_repair(&mut db, &original, &mut script, CURSORS) => {
                panic!("a parked POST cannot finish");
            }
            _ = entered.notified() => {}
        }
        assert!(script.calls() == (1, 1));
        assert!(health(&db, &s.generation).await.revision == rev + 1);
        assert!(repost_total(&db, s).await == 0 && schedule(&db, s).await.0 == 0);
        assert!(is_stale(
            &drive_retained_repair(&mut db, &original, &mut script, 1).await
        ));
        assert!(
            script.calls() == (1, 1),
            "the old permit stops before more IO"
        );
        let Ok(Some(refreshed)) = recover_retained_repair(&mut db, &original).await else {
            panic!("recover after cancelled POST");
        };
        let mut fresh = Script::new(StatVerdict::Missing, PostVerdict::Posted);
        progress(
            drive_retained_repair(&mut db, &refreshed, &mut fresh, 1).await,
            "replay earlier phase",
        );
        assert!(fresh.calls() == (1, 1) && repost_total(&db, s).await == 1);
    });
}
