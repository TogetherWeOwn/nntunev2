//! Real-PostgreSQL tests for conclusive candidate-verification failure
//! (`fail_candidate_verification`) and its recheck-driver wiring
//! (`RecheckStep::VerificationFailed`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `case`, `acquire`,
//! `admitted`, `observe`, `snapshot`, `count`, `custody_rows`, `operation_rows`,
//! `run_test`, `Quiet`). Fixtures are synthetic sealed byte buffers keyed by
//! database UUIDs. The scripted transport answers only with payload-free
//! verdicts and records scalar call counts. Failures print only static labels;
//! outcomes are matched or compared as payload-free enums and sealed values
//! are compared with `==` inside `assert!`, so no assertion can echo handles,
//! claim tokens, encrypted material or row data. Time is advanced only by
//! setting the persisted `not_before` to zero; nothing sleeps.
//!
//! Scope: a verification failure is NOT proof the data is lost, NOT replacement
//! eligibility and NOT cleanup authority. Every case that applies it also
//! checks that custody, references, evidence, budgets and the schedule are
//! preserved and that nothing is selected, promoted or cleaned up.

use super::*;
use nntune_ingest::posted_generations::{
    abandon_candidate, begin_verification_round, confirm_article, drive_recheck,
    fail_candidate_verification, prepare_presence_check, record_presence_check, take_over,
    AbandonmentOutcome, ArticleSide, GenerationHandle, GenerationTransport, MutationOutcome,
    RecheckStep, StoredGeneration, VerificationFailureOutcome,
};
use nntune_ingest::two_spool::{
    PostVerdict, StatVerdict, MAX_RECHECKS, MAX_REPOST_ATTEMPTS_PER_ARTICLE, STAT_BATCH_SIZE,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Barrier;

const ARTICLES: i32 = 3;
const CHECKED_AT: i64 = 1_700_000_000;
const REGISTRY_LOCK: &str =
    "SELECT revision FROM posted_file_registry WHERE plaintext_sha256 = $1 FOR UPDATE";

fn applied(outcome: Result<MutationOutcome, StoreError>, ctx: &'static str) -> GenerationHandle {
    match outcome {
        Ok(MutationOutcome::Applied(handle)) => *handle,
        _ => panic!("{ctx}: expected an applied mutation"),
    }
}

fn is_mutation_stale(outcome: &Result<MutationOutcome, StoreError>) -> bool {
    matches!(outcome, Ok(MutationOutcome::StaleOrDiverged))
}

// ---- scripted transport ------------------------------------------------------

/// Deterministic answers per `(slot, post side?)`. Unscripted STAT answers are
/// Present and unscripted POST answers are Posted. Only scalar counts are kept.
struct Script {
    stat: HashMap<(i32, bool), VecDeque<StatVerdict>>,
    post: HashMap<(i32, bool), VecDeque<PostVerdict>>,
    stats: usize,
    reposts: usize,
}

impl Script {
    fn new() -> Self {
        Script {
            stat: HashMap::new(),
            post: HashMap::new(),
            stats: 0,
            reposts: 0,
        }
    }

    fn script_stat(&mut self, slot: i32, post: bool, verdicts: &[StatVerdict]) {
        self.stat
            .insert((slot, post), verdicts.iter().copied().collect());
    }

    fn script_post(&mut self, slot: i32, post: bool, verdicts: &[PostVerdict]) {
        self.post
            .insert((slot, post), verdicts.iter().copied().collect());
    }

    fn calls(&self) -> (usize, usize) {
        (self.stats, self.reposts)
    }
}

impl GenerationTransport for Script {
    async fn stat(
        &mut self,
        _bundle: &StoredGeneration,
        slot: i32,
        side: ArticleSide,
    ) -> StatVerdict {
        self.stats += 1;
        self.stat
            .get_mut(&(slot, side == ArticleSide::Post))
            .and_then(VecDeque::pop_front)
            .unwrap_or(StatVerdict::Present)
    }

    async fn repost(
        &mut self,
        _bundle: &StoredGeneration,
        slot: i32,
        side: ArticleSide,
    ) -> PostVerdict {
        self.reposts += 1;
        self.post
            .get_mut(&(slot, side == ArticleSide::Post))
            .and_then(VecDeque::pop_front)
            .unwrap_or(PostVerdict::Posted)
    }
}

// ---- fixtures ------------------------------------------------------------------

struct Run {
    case: Case,
    handle: GenerationHandle,
}

/// Real admission (round zero) plus fixture-only retained references so every
/// stop has custody references to preserve.
async fn admit_case(db: &mut Client) -> Run {
    let c = case(db).await;
    let s = &c.sealed;
    let attempt = acquire(db, &c.job, &c.owner, 0).await;
    let handle = admitted(db, &attempt, s).await;
    db.execute(
        "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
         SELECT kind, $1, $2, $3 FROM unnest(ARRAY['legacy_pin', 'serving', 'rollback']) AS kind",
        &[&s.op, &s.generation, &s.hash],
    )
    .await
    .quiet("seed retained-reference controls");
    Run { case: c, handle }
}

async fn begun(db: &mut Client) -> Run {
    let mut run = admit_case(db).await;
    run.handle = applied(
        begin_verification_round(db, &run.handle).await,
        "begin first round",
    );
    run
}

/// Make the current round's waiting schedule due without sleeping.
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

/// Run `n` full bounded passes; each must report progress (not completion).
async fn run_passes(
    db: &mut Client,
    s: &Sealed,
    mut handle: GenerationHandle,
    t: &mut Script,
    n: u32,
) -> GenerationHandle {
    for _ in 0..n {
        make_due(db, s).await;
        handle = match drive_recheck(db, &handle, t, STAT_BATCH_SIZE).await {
            Ok(RecheckStep::Progress(next)) => *next,
            _ => panic!("expected a recheck pass to report progress"),
        };
    }
    handle
}

/// Begun round driven `passes` times where each listed cell is STATed Missing
/// every pass and every other cell is Present. `answers` scripts the POST
/// answers for the Missing cells (default Posted).
async fn drive_missing(
    db: &mut Client,
    missing: &[(i32, bool)],
    answers: &[PostVerdict],
    passes: u32,
) -> Run {
    let mut run = begun(db).await;
    let mut t = Script::new();
    for &(slot, post) in missing {
        t.script_stat(slot, post, &[StatVerdict::Missing; MAX_RECHECKS as usize]);
        if !answers.is_empty() {
            t.script_post(slot, post, answers);
        }
    }
    run.handle = run_passes(db, &run.case.sealed, run.handle.clone(), &mut t, passes).await;
    run
}

/// Fixture-only settled history for isolated fence/receipt controls. The golden
/// application and driver cases separately construct this through real callbacks.
/// Avoid repeating five network-script passes per fault within a 20-second case.
async fn exhausted(db: &mut Client) -> Run {
    let run = begun(db).await;
    let s = &run.case.sealed;
    db.execute(
        "INSERT INTO posted_file_article_progress \
         (generation_id, round, slot, side, confirmed, repost_attempts, \
          presence_verdict, presence_repost_attempts) \
         SELECT $1, 1, slot, side::smallint, NOT (slot = 0 AND side = 0), \
             CASE WHEN slot = 0 AND side = 0 THEN $2::integer ELSE 0 END, \
             CASE WHEN slot = 0 AND side = 0 THEN 'missing' ELSE 'present' END, \
             CASE WHEN slot = 0 AND side = 0 THEN $2::integer ELSE 0 END \
         FROM generate_series(0, 2) AS slots(slot) CROSS JOIN generate_series(0, 1) AS sides(side)",
        &[&s.generation, &(MAX_REPOST_ATTEMPTS_PER_ARTICLE as i32)],
    )
    .await
    .quiet("seed settled-slot history");
    db.execute(
        "UPDATE posted_file_recheck_schedule SET completed_passes = $2, \
         state = 'exhausted', not_before = NULL WHERE generation_id = $1 AND round = 1",
        &[&s.generation, &(MAX_RECHECKS as i32)],
    )
    .await
    .quiet("seed exhausted schedule");
    run
}

/// Fixture-only content observations for the current round: `(side, intact)`.
/// Intact is the affirmative full-set read; anything else is a content failure.
async fn seed_content(db: &Client, s: &Sealed, round: i64, sides: &[(i16, bool)]) {
    for &(side, intact) in sides {
        let (state, articles) = if intact {
            ("intact", 3_i32)
        } else {
            ("integrity_failed", 0_i32)
        };
        let backbone = if side == 0 { "pool-a" } else { "pool-b" };
        db.execute(
            "INSERT INTO posted_file_content_checks \
             (generation_id, round, side, backbone, state, checked_articles, checked_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[
                &s.generation,
                &round,
                &side,
                &backbone,
                &state,
                &articles,
                &CHECKED_AT,
            ],
        )
        .await
        .quiet("seed content check");
    }
}

// ---- readers ---------------------------------------------------------------------

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

/// `(repost_attempts, presence_repost_attempts, presence_verdict)` of one cell.
async fn cell(
    db: &Client,
    s: &Sealed,
    slot: i32,
    post: bool,
) -> (i32, Option<i32>, Option<String>) {
    let side = if post { 0_i16 } else { 1_i16 };
    let r = db
        .query_one(
            "SELECT repost_attempts, presence_repost_attempts, presence_verdict \
             FROM posted_file_article_progress \
             WHERE generation_id = $1 AND slot = $2 AND side = $3 \
             ORDER BY round DESC LIMIT 1",
            &[&s.generation, &slot, &side],
        )
        .await
        .quiet("read cell evidence");
    (r.get(0), r.get(1), r.get(2))
}

async fn schedule_state(db: &Client, s: &Sealed) -> (String, i32) {
    let r = db
        .query_one(
            "SELECT r.state, r.completed_passes FROM posted_file_recheck_schedule r \
             JOIN posted_file_health h \
               ON h.generation_id = r.generation_id AND h.round = r.round \
             WHERE r.generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("read schedule state");
    (r.get(0), r.get(1))
}

/// Every table a refused or replayed stop must leave untouched.
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

/// Call failure while another connection holds the registry row, then commit
/// newer evidence (a health revision bump, optionally with a full-set Intact
/// read of the failed lane) before releasing it. The blocked call must observe
/// the committed state after its lock wait, not the state it started from.
async fn fail_after_newer_commit(
    db: &Client,
    run: &Run,
    intact: bool,
) -> Result<VerificationFailureOutcome, StoreError> {
    let s = &run.case.sealed;
    let mut blocker = connect().await;
    let tx = blocker
        .transaction()
        .await
        .quiet("hold registry transaction");
    tx.query_one(REGISTRY_LOCK, &[&s.hash])
        .await
        .quiet("hold registry row");
    if intact {
        tx.execute(
            "INSERT INTO posted_file_content_checks \
             (generation_id, round, side, backbone, state, checked_articles, checked_at) \
             VALUES ($1, 1, 0, 'pool-a', 'intact', 3, $2)",
            &[&s.generation, &CHECKED_AT],
        )
        .await
        .quiet("stage intact read");
    }
    tx.execute(
        "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("stage newer revision");
    let mut worker = connect().await;
    let pid: i32 = worker
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .quiet("read worker pid")
        .get(0);
    let (outcome, ()) = tokio::join!(
        fail_candidate_verification(&mut worker, &run.handle),
        async {
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
            tx.commit().await.quiet("release registry row");
        }
    );
    outcome
}

/// The candidate is still live: verifying, no failure, pending, unselected.
async fn assert_still_pending(db: &Client, s: &Sealed) {
    let h = health(db, s).await;
    assert!(h.state == "verifying", "health must stay verifying");
    assert!(h.failure_kind.is_none(), "no failure may be recorded");
    assert!(op(db, s).await.state == "pending");
    let r = registry(db, s).await;
    assert!(r.pending.as_deref() == Some(s.op.as_str()));
    assert!(r.selected.is_none());
    assert_eq!(custody_rows(db, s).await, 1);
    assert_eq!(operation_rows(db, s).await, 1);
}

/// A refused call: no outcome other than the expected one, and not one write.
async fn assert_refused_without_writes(db: &Client, s: &Sealed, before: &Frozen, bh: &Health) {
    assert!(frozen(db, s).await == *before, "refusal must not write");
    assert!(health(db, s).await == *bh, "refusal must not touch health");
}

/// A failure applied exactly once: operation stopped, health terminal with the
/// verification kind, registry pending cleared without selection, and custody,
/// references, evidence, budgets and the schedule all preserved.
async fn assert_failure_applied(
    db: &Client,
    s: &Sealed,
    before: &Frozen,
    bh: &Health,
    br: &Reg,
    before_op: &Op,
    budget: i64,
) {
    let after_op = op(db, s).await;
    assert!(after_op.state == "abandoned", "operation is stopped");
    assert!(after_op.fixed == before_op.fixed, "operation identity held");
    let after_reg = registry(db, s).await;
    assert!(after_reg.pending.is_none(), "pending is cleared");
    assert_eq!(after_reg.revision, br.revision + 1);
    assert!(after_reg.selected == br.selected && after_reg.selected.is_none());
    let h = health(db, s).await;
    assert!(h.state == "terminal_failed");
    assert!(h.failure_kind.as_deref() == Some("verification"));
    assert!(h.verified_at.is_none() && h.retention_checked_at.is_none());
    assert_eq!(h.revision, bh.revision + 1);
    assert_eq!(h.round, bh.round + 1);
    assert!(h.token != bh.token, "the round token rotates");
    let after = frozen(db, s).await;
    assert!(after.0[0] == before.0[0], "custody must not change");
    assert!(after.0[4] == before.0[4], "references must not change");
    assert!(after.1 == before.1, "evidence and schedule must not change");
    assert!(after.2 == before.2);
    assert_eq!(repost_budget(db, s).await, budget);
    assert_eq!(in_flight_refs(db, s).await, 1, "in-flight reference kept");
    assert_eq!(custody_rows(db, s).await, 1, "custody is never cleaned up");
    assert_eq!(operation_rows(db, s).await, 1);
}

// ---- tests --------------------------------------------------------------------------

#[test]
fn postgres_failure_definitive_applies_atomically_with_receipt_and_preserves_everything() {
    run_test(async {
        let mut db = connect().await;
        // Two unresolved cells (one per side) Missing at the cap, the rest confirmed.
        let run = drive_missing(&mut db, &[(0, true), (1, false)], &[], MAX_RECHECKS).await;
        let s = &run.case.sealed;
        // Content failures on both lanes are history, never a veto.
        seed_content(&db, s, 1, &[(0, false), (1, false)]).await;
        let expected_budget = 2 * i64::from(MAX_REPOST_ATTEMPTS_PER_ARTICLE);
        assert_eq!(repost_budget(&db, s).await, expected_budget);
        assert!(schedule_state(&db, s).await == ("exhausted".to_owned(), MAX_RECHECKS as i32));

        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        let br = registry(&db, s).await;
        let before_op = op(&db, s).await;
        assert!(bh.state == "verifying" && bh.round == 1);
        assert!(before_op.state == "pending");
        assert!(br.pending.as_deref() == Some(s.op.as_str()));
        assert_eq!(in_flight_refs(&db, s).await, 1);

        assert_eq!(
            fail_candidate_verification(&mut db, &run.handle).await,
            Ok(VerificationFailureOutcome::Applied)
        );
        assert_failure_applied(&db, s, &before, &bh, &br, &before_op, expected_budget).await;

        // The original handle replays as an acknowledgment, with no write.
        let settled = frozen(&db, s).await;
        let settled_health = health(&db, s).await;
        let settled_reg = registry(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &run.handle).await,
            Ok(VerificationFailureOutcome::AlreadyFailed)
        );
        assert_refused_without_writes(&db, s, &settled, &settled_health).await;
        assert_eq!(registry(&db, s).await.revision, settled_reg.revision);

        // The receipt confers no authority: no fence, mutation or driver work.
        assert!(!posting_fence_is_current(&mut db, &run.handle)
            .await
            .expect("old fence"));
        assert!(is_mutation_stale(
            &confirm_article(&mut db, &run.handle, 1, ArticleSide::Verify).await
        ));
        let mut t = Script::new();
        assert!(matches!(
            drive_recheck(&mut db, &run.handle, &mut t, STAT_BATCH_SIZE).await,
            Ok(RecheckStep::VerificationFailed)
        ));
        assert_eq!(t.calls(), (0, 0), "a stopped candidate reaches no network");
        assert!(frozen(&db, s).await == settled);

        // An expired attempt refuses even the receipt.
        expire_attempt(&db, &run.case.job).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &run.handle).await,
            Ok(VerificationFailureOutcome::StaleOrDiverged)
        );
        assert_refused_without_writes(&db, s, &settled, &settled_health).await;
    });
}

#[test]
fn postgres_failure_missing_below_cap_or_before_the_last_repost_is_insufficient_until_fresh_stat() {
    run_test(async {
        let mut db = connect().await;

        // Below the cap: reposts are never answered, so Missing is fresh but the
        // answered-repost count stays short of the cap.
        let short = drive_missing(
            &mut db,
            &[(0, true)],
            &[PostVerdict::Transport; MAX_RECHECKS as usize],
            MAX_RECHECKS,
        )
        .await;
        let s = &short.case.sealed;
        assert_eq!(cell(&db, s, 0, true).await.0, 0);
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &short.handle).await,
            Ok(VerificationFailureOutcome::InsufficientEvidence)
        );
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert_still_pending(&db, s).await;

        // At the cap, but the last answered repost came after the last STAT: the
        // stored Missing is pre-repair history, not current evidence.
        let answers = [
            PostVerdict::Posted,
            PostVerdict::Posted,
            PostVerdict::Transport,
            PostVerdict::Transport,
            PostVerdict::Posted,
        ];
        let stale = drive_missing(&mut db, &[(0, true)], &answers, MAX_RECHECKS).await;
        let s = &stale.case.sealed;
        assert!(schedule_state(&db, s).await == ("exhausted".to_owned(), MAX_RECHECKS as i32));
        let (attempts, observed, verdict) = cell(&db, s, 0, true).await;
        assert_eq!(attempts, MAX_REPOST_ATTEMPTS_PER_ARTICLE as i32);
        assert_eq!(observed, Some(MAX_REPOST_ATTEMPTS_PER_ARTICLE as i32 - 1));
        assert!(verdict.as_deref() == Some("missing"));
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &stale.handle).await,
            Ok(VerificationFailureOutcome::InsufficientEvidence)
        );
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert_still_pending(&db, s).await;

        // A fresh Missing after the final repost makes the evidence current.
        let permit =
            match prepare_presence_check(&mut db, &stale.handle, 0, ArticleSide::Post).await {
                Ok(Some(permit)) => permit,
                _ => panic!("expected a presence permit"),
            };
        let fresh = applied(
            record_presence_check(&mut db, &permit, StatVerdict::Missing).await,
            "record fresh Missing",
        );
        let (attempts, observed, _) = cell(&db, s, 0, true).await;
        assert_eq!(observed, Some(attempts));
        // The pre-stat handle is stale; the refreshed one concludes.
        assert_eq!(
            fail_candidate_verification(&mut db, &stale.handle).await,
            Ok(VerificationFailureOutcome::StaleOrDiverged)
        );
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        let br = registry(&db, s).await;
        let before_op = op(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &fresh).await,
            Ok(VerificationFailureOutcome::Applied)
        );
        assert_failure_applied(
            &db,
            s,
            &before,
            &bh,
            &br,
            &before_op,
            i64::from(MAX_REPOST_ATTEMPTS_PER_ARTICLE),
        )
        .await;
    });
}

#[test]
fn postgres_failure_transport_positive_and_conflicting_intact_content_never_conclude() {
    run_test(async {
        let mut db = connect().await;

        // A cell Missing at the cap beside one that only ever answered transport
        // is a partial/unknown set: the timer alone concludes nothing.
        let mut run = begun(&mut db).await;
        let mut t = Script::new();
        t.script_stat(0, true, &[StatVerdict::Missing; MAX_RECHECKS as usize]);
        t.script_stat(1, false, &[StatVerdict::Transport; MAX_RECHECKS as usize]);
        run.handle = run_passes(
            &mut db,
            &run.case.sealed,
            run.handle.clone(),
            &mut t,
            MAX_RECHECKS,
        )
        .await;
        let s = &run.case.sealed;
        assert!(schedule_state(&db, s).await.0 == "exhausted");
        assert_eq!(
            cell(&db, s, 1, false).await.0,
            0,
            "transport spends nothing"
        );
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &run.handle).await,
            Ok(VerificationFailureOutcome::InsufficientEvidence)
        );
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert_still_pending(&db, s).await;

        // Every cell transport-only through the whole schedule.
        let mut all = begun(&mut db).await;
        let mut t = Script::new();
        for slot in 0..ARTICLES {
            for post in [true, false] {
                t.script_stat(slot, post, &[StatVerdict::Transport; MAX_RECHECKS as usize]);
            }
        }
        all.handle = run_passes(
            &mut db,
            &all.case.sealed,
            all.handle.clone(),
            &mut t,
            MAX_RECHECKS,
        )
        .await;
        let s = &all.case.sealed;
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &all.handle).await,
            Ok(VerificationFailureOutcome::InsufficientEvidence)
        );
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert_still_pending(&db, s).await;

        // Positive presence for every cell finishes verification; it is the
        // opposite of failure.
        let mut ok = begun(&mut db).await;
        make_due(&db, &ok.case.sealed).await;
        let mut t = Script::new();
        ok.handle = match drive_recheck(&mut db, &ok.handle, &mut t, STAT_BATCH_SIZE).await {
            Ok(RecheckStep::PresenceComplete(done)) => *done,
            _ => panic!("expected presence completion"),
        };
        let s = &ok.case.sealed;
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        assert!(bh.verified_at.is_some());
        assert_eq!(
            fail_candidate_verification(&mut db, &ok.handle).await,
            Ok(VerificationFailureOutcome::InsufficientEvidence)
        );
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert!(health(&db, s).await.failure_kind.is_none());

        // An exhausted schedule whose every cell was later confirmed has no failed
        // slot, and content-integrity failures are never a failure by themselves.
        let mut conf = exhausted(&mut db).await;
        let s = &conf.case.sealed;
        conf.handle = applied(
            confirm_article(&mut db, &conf.handle, 0, ArticleSide::Post).await,
            "confirm the unresolved cell",
        );
        seed_content(&db, s, 1, &[(0, false), (1, false)]).await;
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        assert!(bh.verified_at.is_none(), "verification was never finished");
        assert_eq!(
            fail_candidate_verification(&mut db, &conf.handle).await,
            Ok(VerificationFailureOutcome::InsufficientEvidence)
        );
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert_still_pending(&db, s).await;

        // A full-set Intact read of the failed lane, landing after the last
        // Missing without changing presence, is stronger affirmative evidence
        // and vetoes failure for that lane.
        let vetoed = exhausted(&mut db).await;
        let s = &vetoed.case.sealed;
        seed_content(&db, s, 1, &[(0, true)]).await;
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &vetoed.handle).await,
            Ok(VerificationFailureOutcome::InsufficientEvidence)
        );
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert_still_pending(&db, s).await;

        // Intact on a different, fully confirmed lane does not block the lane
        // that genuinely failed; a content failure on the failed lane is no veto.
        let other = exhausted(&mut db).await;
        let s = &other.case.sealed;
        seed_content(&db, s, 1, &[(0, false), (1, true)]).await;
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        let br = registry(&db, s).await;
        let before_op = op(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &other.handle).await,
            Ok(VerificationFailureOutcome::Applied)
        );
        assert_failure_applied(
            &db,
            s,
            &before,
            &bh,
            &br,
            &before_op,
            i64::from(MAX_REPOST_ATTEMPTS_PER_ARTICLE),
        )
        .await;
    });
}

#[test]
fn postgres_failure_round_zero_waiting_and_untracked_schedules_are_not_terminal() {
    run_test(async {
        let mut db = connect().await;

        // Round zero has no verification history at all.
        let zero = admit_case(&mut db).await;
        let s = &zero.case.sealed;
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        assert_eq!(bh.round, 0);
        assert_eq!(
            fail_candidate_verification(&mut db, &zero.handle).await,
            Ok(VerificationFailureOutcome::InsufficientEvidence)
        );
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert!(op(&db, s).await.state == "pending");

        // Fresh Missing at the cap but the schedule still waits for a last pass.
        let waiting = drive_missing(&mut db, &[(0, true)], &[], MAX_RECHECKS - 1).await;
        let s = &waiting.case.sealed;
        assert!(schedule_state(&db, s).await == ("waiting".to_owned(), MAX_RECHECKS as i32 - 1));
        let (attempts, observed, verdict) = cell(&db, s, 0, true).await;
        assert_eq!(attempts, MAX_REPOST_ATTEMPTS_PER_ARTICLE as i32);
        assert_eq!(observed, Some(attempts));
        assert!(verdict.as_deref() == Some("missing"));
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &waiting.handle).await,
            Ok(VerificationFailureOutcome::InsufficientEvidence)
        );
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert_still_pending(&db, s).await;

        // A schedule without durable history never authorizes failure, and the
        // driver keeps the original without reaching the network.
        let untracked = exhausted(&mut db).await;
        let s = &untracked.case.sealed;
        db.execute(
            "UPDATE posted_file_recheck_schedule \
             SET state = 'untracked', completed_passes = 0, not_before = NULL \
             WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("mark schedule untracked");
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &untracked.handle).await,
            Ok(VerificationFailureOutcome::InsufficientEvidence)
        );
        assert_refused_without_writes(&db, s, &before, &bh).await;
        let mut t = Script::new();
        assert!(matches!(
            drive_recheck(&mut db, &untracked.handle, &mut t, STAT_BATCH_SIZE).await,
            Ok(RecheckStep::KeepOriginal)
        ));
        assert_eq!(t.calls(), (0, 0));
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert_still_pending(&db, s).await;
    });
}

#[test]
fn postgres_failure_stale_revision_round_lease_job_and_attempt_fences_reject_without_writes() {
    run_test(async {
        let mut db = connect().await;
        for fault in 0..5 {
            let run = exhausted(&mut db).await;
            let s = &run.case.sealed;
            let (sql, key): (&str, &String) = match fault {
                0 => (
                    "UPDATE posted_file_health SET revision = revision + 1 \
                     WHERE generation_id = $1",
                    &s.generation,
                ),
                1 => (
                    "UPDATE posted_file_health SET round = round + 1, \
                     round_token = gen_random_uuid()::text WHERE generation_id = $1",
                    &s.generation,
                ),
                2 => (
                    "UPDATE posted_file_operations SET lease_until = 0 WHERE id = $1",
                    &s.op,
                ),
                3 => (
                    "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
                    &run.case.job,
                ),
                _ => (
                    "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
                    &run.case.job,
                ),
            };
            db.execute(sql, &[key]).await.quiet("apply stale fault");
            let before = frozen(&db, s).await;
            let bh = health(&db, s).await;
            let br = registry(&db, s).await;
            assert_eq!(
                fail_candidate_verification(&mut db, &run.handle).await,
                Ok(VerificationFailureOutcome::StaleOrDiverged),
                "each fence must refuse the original handle"
            );
            assert_refused_without_writes(&db, s, &before, &bh).await;
            let r = registry(&db, s).await;
            assert_eq!(r.revision, br.revision);
            assert!(r.pending.as_deref() == Some(s.op.as_str()));
            assert!(health(&db, s).await.failure_kind.is_none());
            assert!(op(&db, s).await.state == "pending");
            assert!(health(&db, s).await.state == "verifying");
        }

        // Newer evidence committed during the registry lock wait retires the
        // original call: it is stale and adds no write of its own.
        for intact in [false, true] {
            let run = exhausted(&mut db).await;
            let s = &run.case.sealed;
            let bh = health(&db, s).await;
            let br = registry(&db, s).await;
            assert_eq!(
                fail_after_newer_commit(&db, &run, intact).await,
                Ok(VerificationFailureOutcome::StaleOrDiverged),
                "newer committed evidence must retire the waiting call"
            );
            let h = health(&db, s).await;
            assert_eq!(h.revision, bh.revision + 1, "only the newer commit wrote");
            assert_eq!(h.round, bh.round);
            assert!(h.token == bh.token);
            assert_eq!(registry(&db, s).await.revision, br.revision);
            assert_still_pending(&db, s).await;
            assert_eq!(
                repost_budget(&db, s).await,
                i64::from(MAX_REPOST_ATTEMPTS_PER_ARTICLE)
            );
        }
    });
}

#[test]
fn postgres_failure_after_newer_attempt_or_takeover_rejects_old_claim_and_current_claim_works() {
    run_test(async {
        let mut db = connect().await;
        let run = exhausted(&mut db).await;
        let s = &run.case.sealed;
        let c = &run.case;
        let expected = observe(&db, &s.hash).await.expect("observe pending hash");

        // Attempt 2 is persisted, but nothing has taken the candidate over.
        expire_attempt(&db, &c.job).await;
        expire_operation(&db, s).await;
        let next = acquire(&mut db, &c.job, &c.owner, 1).await;
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &run.handle).await,
            Ok(VerificationFailureOutcome::StaleOrDiverged)
        );
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert!(op(&db, s).await.state == "pending");

        // After takeover the old claim stays refused; the new claim concludes.
        let current = match take_over(&mut db, &next, &expected, &s.op).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(handle, _)) => handle,
            _ => panic!("takeover did not resume the candidate"),
        };
        let taken = frozen(&db, s).await;
        let taken_health = health(&db, s).await;
        let taken_reg = registry(&db, s).await;
        let taken_op = op(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &run.handle).await,
            Ok(VerificationFailureOutcome::StaleOrDiverged)
        );
        assert_refused_without_writes(&db, s, &taken, &taken_health).await;

        assert_eq!(
            fail_candidate_verification(&mut db, &current).await,
            Ok(VerificationFailureOutcome::Applied)
        );
        assert_failure_applied(
            &db,
            s,
            &taken,
            &taken_health,
            &taken_reg,
            &taken_op,
            i64::from(MAX_REPOST_ATTEMPTS_PER_ARTICLE),
        )
        .await;

        // The old claim never gains authority; the current claim replays, and
        // only while its own attempt is live.
        let settled = frozen(&db, s).await;
        let settled_health = health(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &run.handle).await,
            Ok(VerificationFailureOutcome::StaleOrDiverged)
        );
        assert_eq!(
            fail_candidate_verification(&mut db, &current).await,
            Ok(VerificationFailureOutcome::AlreadyFailed)
        );
        assert_refused_without_writes(&db, s, &settled, &settled_health).await;
        expire_attempt(&db, &c.job).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &current).await,
            Ok(VerificationFailureOutcome::StaleOrDiverged)
        );
        assert_refused_without_writes(&db, s, &settled, &settled_health).await;
    });
}

#[test]
fn postgres_failure_and_abandonment_receipts_cannot_impersonate_each_other() {
    run_test(async {
        let mut db = connect().await;

        // A failed candidate is not an abandoned one.
        let failed = exhausted(&mut db).await;
        let f = &failed.case.sealed;
        assert_eq!(
            fail_candidate_verification(&mut db, &failed.handle).await,
            Ok(VerificationFailureOutcome::Applied)
        );
        let settled = frozen(&db, f).await;
        let settled_health = health(&db, f).await;
        assert_eq!(
            abandon_candidate(&mut db, &failed.handle).await,
            Ok(AbandonmentOutcome::StaleOrDiverged),
            "a failure receipt is not an abandonment receipt"
        );
        assert_refused_without_writes(&db, f, &settled, &settled_health).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &failed.handle).await,
            Ok(VerificationFailureOutcome::AlreadyFailed)
        );
        assert!(settled_health.state == "terminal_failed");
        assert!(settled_health.failure_kind.as_deref() == Some("verification"));
        let mut t = Script::new();
        assert!(matches!(
            drive_recheck(&mut db, &failed.handle, &mut t, STAT_BATCH_SIZE).await,
            Ok(RecheckStep::VerificationFailed)
        ));
        assert_eq!(t.calls(), (0, 0));
        assert_refused_without_writes(&db, f, &settled, &settled_health).await;

        // An abandoned candidate is not a failed one, and stays non-terminal.
        let stopped = exhausted(&mut db).await;
        let a = &stopped.case.sealed;
        assert_eq!(
            abandon_candidate(&mut db, &stopped.handle).await,
            Ok(AbandonmentOutcome::Applied)
        );
        let settled = frozen(&db, a).await;
        let settled_health = health(&db, a).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &stopped.handle).await,
            Ok(VerificationFailureOutcome::StaleOrDiverged),
            "an abandonment receipt is not a failure receipt"
        );
        assert_refused_without_writes(&db, a, &settled, &settled_health).await;
        assert_eq!(
            abandon_candidate(&mut db, &stopped.handle).await,
            Ok(AbandonmentOutcome::AlreadyAbandoned)
        );
        assert!(settled_health.state == "unknown");
        assert!(settled_health.failure_kind.is_none());
        let mut t = Script::new();
        assert!(matches!(
            drive_recheck(&mut db, &stopped.handle, &mut t, STAT_BATCH_SIZE).await,
            Ok(RecheckStep::StaleOrDiverged)
        ));
        assert_eq!(t.calls(), (0, 0));
        assert_refused_without_writes(&db, a, &settled, &settled_health).await;
    });
}

#[test]
fn postgres_failure_concurrent_original_handle_applies_once_and_the_other_does_not_write() {
    run_test(async {
        let mut db = connect().await;
        let run = exhausted(&mut db).await;
        let s = &run.case.sealed;
        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        let br = registry(&db, s).await;
        let before_op = op(&db, s).await;

        let mut left = connect().await;
        let mut right = connect().await;
        let (handle_left, handle_right) = (run.handle.clone(), run.handle.clone());
        let barrier = Arc::new(Barrier::new(2));
        let (gate_left, gate_right) = (barrier.clone(), barrier);
        let (out_left, out_right) = tokio::join!(
            async {
                gate_left.wait().await;
                fail_candidate_verification(&mut left, &handle_left).await
            },
            async {
                gate_right.wait().await;
                fail_candidate_verification(&mut right, &handle_right).await
            }
        );
        let outcomes = [out_left, out_right];
        let applied_count = outcomes
            .iter()
            .filter(|o| **o == Ok(VerificationFailureOutcome::Applied))
            .count();
        let other_count = outcomes
            .iter()
            .filter(|o| {
                **o == Ok(VerificationFailureOutcome::AlreadyFailed)
                    || **o == Ok(VerificationFailureOutcome::StaleOrDiverged)
            })
            .count();
        assert_eq!(applied_count, 1, "exactly one racer applies");
        assert_eq!(other_count, 1, "the other racer is a receipt or stale");

        // One transition: each counter moved exactly once.
        assert_failure_applied(
            &db,
            s,
            &before,
            &bh,
            &br,
            &before_op,
            i64::from(MAX_REPOST_ATTEMPTS_PER_ARTICLE),
        )
        .await;
        let settled = frozen(&db, s).await;
        let settled_health = health(&db, s).await;
        assert_eq!(
            fail_candidate_verification(&mut db, &run.handle).await,
            Ok(VerificationFailureOutcome::AlreadyFailed)
        );
        assert_refused_without_writes(&db, s, &settled, &settled_health).await;
    });
}

#[test]
fn postgres_driver_missing_after_three_reposts_through_five_passes_returns_verification_failed() {
    run_test(async {
        let mut db = connect().await;
        let mut run = begun(&mut db).await;
        let s = &run.case.sealed;
        let mut t = Script::new();
        for slot in 0..ARTICLES {
            for post in [true, false] {
                t.script_stat(slot, post, &[StatVerdict::Missing; MAX_RECHECKS as usize]);
            }
        }
        let cells = (ARTICLES * 2) as usize;
        run.handle = run_passes(&mut db, s, run.handle.clone(), &mut t, MAX_RECHECKS).await;
        // Every cell: one STAT per pass, one answered repost per pass up to the cap.
        assert_eq!(
            t.calls(),
            (
                cells * MAX_RECHECKS as usize,
                cells * MAX_REPOST_ATTEMPTS_PER_ARTICLE as usize
            )
        );
        let budget = (cells as i64) * i64::from(MAX_REPOST_ATTEMPTS_PER_ARTICLE);
        assert_eq!(repost_budget(&db, s).await, budget);
        assert!(schedule_state(&db, s).await == ("exhausted".to_owned(), MAX_RECHECKS as i32));
        seed_content(&db, s, 1, &[(0, false), (1, false)]).await;

        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        let br = registry(&db, s).await;
        let before_op = op(&db, s).await;
        let seen = t.calls();
        assert!(matches!(
            drive_recheck(&mut db, &run.handle, &mut t, STAT_BATCH_SIZE).await,
            Ok(RecheckStep::VerificationFailed)
        ));
        assert_eq!(t.calls(), seen, "the conclusion uses no network");
        assert_failure_applied(&db, s, &before, &bh, &br, &before_op, budget).await;

        // Original-handle replay (a lost driver acknowledgment) is the stopped
        // receipt only: the same step, no network, no write.
        let settled = frozen(&db, s).await;
        let settled_health = health(&db, s).await;
        let settled_reg = registry(&db, s).await;
        for _ in 0..2 {
            assert!(matches!(
                drive_recheck(&mut db, &run.handle, &mut t, STAT_BATCH_SIZE).await,
                Ok(RecheckStep::VerificationFailed)
            ));
            assert_eq!(t.calls(), seen, "replay uses no network");
            assert_refused_without_writes(&db, s, &settled, &settled_health).await;
            assert_eq!(registry(&db, s).await.revision, settled_reg.revision);
        }

        // The receipt needs the live attempt; an expired one is stale.
        expire_attempt(&db, &run.case.job).await;
        assert!(matches!(
            drive_recheck(&mut db, &run.handle, &mut t, STAT_BATCH_SIZE).await,
            Ok(RecheckStep::StaleOrDiverged)
        ));
        assert_eq!(t.calls(), seen);
        assert_refused_without_writes(&db, s, &settled, &settled_health).await;
    });
}

#[test]
fn postgres_driver_transport_only_through_five_passes_keeps_original_without_terminal_state() {
    run_test(async {
        let mut db = connect().await;
        let mut run = begun(&mut db).await;
        let s = &run.case.sealed;
        let mut t = Script::new();
        for slot in 0..ARTICLES {
            for post in [true, false] {
                t.script_stat(slot, post, &[StatVerdict::Transport; MAX_RECHECKS as usize]);
            }
        }
        let cells = (ARTICLES * 2) as usize;
        run.handle = run_passes(&mut db, s, run.handle.clone(), &mut t, MAX_RECHECKS).await;
        assert_eq!(
            t.calls(),
            (cells * MAX_RECHECKS as usize, 0),
            "transport answers never repost"
        );
        assert_eq!(repost_budget(&db, s).await, 0);
        assert!(schedule_state(&db, s).await == ("exhausted".to_owned(), MAX_RECHECKS as i32));

        let before = frozen(&db, s).await;
        let bh = health(&db, s).await;
        let seen = t.calls();
        assert!(matches!(
            drive_recheck(&mut db, &run.handle, &mut t, STAT_BATCH_SIZE).await,
            Ok(RecheckStep::KeepOriginal)
        ));
        assert_eq!(t.calls(), seen, "keeping the original uses no network");
        assert_refused_without_writes(&db, s, &before, &bh).await;
        assert_still_pending(&db, s).await;
        assert_eq!(in_flight_refs(&db, s).await, 1);
    });
}
