//! Real-PostgreSQL tests for the bounded recheck driver (`drive_recheck`).
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `case`, `acquire`,
//! `admitted`, `recover`, `count`, `snapshot`, `expect_db_error`,
//! `run_blocking`, `run_test`, `MIGRATION_0040`). The synthetic transport
//! implements `GenerationTransport` against the stored sealed bundle it is
//! handed: it records only scalar calls (operation, slot, side) and a single
//! boolean saying every bundle it saw equalled the exact `Sealed` fixture.
//! Fixtures carry no article identities, bytes, keys or names. Time is
//! advanced only by setting the persisted `not_before` to zero; nothing
//! sleeps. Failures print only static labels; outcomes are matched, never
//! formatted, and sealed values are compared with `==` inside `assert!`.
//!
//! Network-hook tests mutate durable state from a second connection while the
//! driver is suspended inside a transport call. Every hook first takes
//! `FOR UPDATE NOWAIT` on each row the fence locks, so a transaction held
//! across the network await would fail the probe instead of passing quietly.
//!
//! Scope: the driver is bounded bookkeeping. Presence completion is never
//! content, selection or cleanup authority, and no outcome here may write a
//! negative health, touch custody or the registry.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, confirm_article, drive_recheck, finish_verification,
    prepare_presence_check, prepare_recheck, record_presence_check, ArticleSide, GenerationHandle,
    GenerationTransport, MutationOutcome, RecheckStep, StoredGeneration,
};
use nntune_ingest::two_spool::{
    recheck_delay_ms, PostVerdict, StatVerdict, MAX_RECHECKS, MAX_REPOST_ATTEMPTS_PER_ARTICLE,
    STAT_BATCH_SIZE,
};
use std::collections::{HashMap, VecDeque};

const ARTICLES: i32 = 3;
/// Slack for the database clock moving between a probe and the write it brackets.
const CLOCK_SLACK: i64 = 5;

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

fn progress_handle(step: Result<RecheckStep, StoreError>, ctx: &'static str) -> GenerationHandle {
    match step {
        Ok(RecheckStep::Progress(handle)) => *handle,
        _ => panic!("{ctx}: expected progress"),
    }
}

fn complete_handle(step: Result<RecheckStep, StoreError>, ctx: &'static str) -> GenerationHandle {
    match step {
        Ok(RecheckStep::PresenceComplete(handle)) => *handle,
        _ => panic!("{ctx}: expected presence completion"),
    }
}

// ---- synthetic transport ---------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Stat,
    Repost,
}

/// `(operation, slot, post side?)` -- scalars only.
type Call = (Op, i32, bool);

fn st(slot: i32, post: bool) -> Call {
    (Op::Stat, slot, post)
}

fn rp(slot: i32, post: bool) -> Call {
    (Op::Repost, slot, post)
}

#[derive(Debug, Clone, Copy)]
enum Fault {
    Revision,
    OperationLease,
    JobStatus,
    AttemptSuperseded,
    NewRound,
}

/// A deterministic mutation from a second connection, fired once when the
/// driver is suspended inside the chosen transport call.
struct Hook {
    db: Client,
    attempt: AttemptHandle,
    job: String,
    owner: String,
    sealed: Sealed,
    fault: Fault,
    at_repost: bool,
    fired: bool,
}

struct Transport {
    expected: Sealed,
    stat: HashMap<(i32, bool), VecDeque<StatVerdict>>,
    post: HashMap<(i32, bool), VecDeque<PostVerdict>>,
    calls: Vec<Call>,
    /// Every bundle handed to the transport equalled the exact sealed fixture.
    exact: bool,
    hook: Option<Hook>,
}

impl Transport {
    fn new(expected: &Sealed) -> Self {
        Transport {
            expected: expected.clone(),
            stat: HashMap::new(),
            post: HashMap::new(),
            calls: Vec::new(),
            exact: true,
            hook: None,
        }
    }

    /// Scripted STAT answers for one slot/side; unscripted answers are Present.
    fn script_stat(&mut self, slot: i32, post: bool, verdicts: &[StatVerdict]) {
        self.stat
            .insert((slot, post), verdicts.iter().copied().collect());
    }

    /// Scripted POST answers for one slot/side; unscripted answers are Posted.
    fn script_post(&mut self, slot: i32, post: bool, verdicts: &[PostVerdict]) {
        self.post
            .insert((slot, post), verdicts.iter().copied().collect());
    }

    fn record(&mut self, bundle: &StoredGeneration, op: Op, slot: i32, side: ArticleSide) {
        self.exact &= bundle.generation_id() == self.expected.generation
            && bundle.plan_sha256() == self.expected.plan
            && matches!(bundle.custody(), Ok(c) if c == self.expected.custody());
        self.calls.push((op, slot, side == ArticleSide::Post));
    }

    async fn fire(&mut self, at_repost: bool) {
        let Some(h) = self.hook.as_mut() else {
            return;
        };
        if h.fired || h.at_repost != at_repost {
            return;
        }
        h.fired = true;
        probe_unlocked(&h.db, &h.sealed, &h.job).await;
        match h.fault {
            Fault::Revision => {
                let current = current_handle(&mut h.db, &h.attempt, &h.sealed).await;
                applied(
                    confirm_article(&mut h.db, &current, 2, ArticleSide::Verify).await,
                    "hook revision",
                );
            }
            Fault::OperationLease => {
                h.db.execute(
                    "UPDATE posted_file_operations SET lease_until = 0 WHERE id = $1",
                    &[&h.sealed.op],
                )
                .await
                .quiet("hook expire operation");
            }
            Fault::JobStatus => {
                h.db.execute(
                    "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
                    &[&h.job],
                )
                .await
                .quiet("hook change job status");
            }
            Fault::AttemptSuperseded => {
                h.db.execute(
                    "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
                    &[&h.job],
                )
                .await
                .quiet("hook expire attempt");
                let _newer = acquire(&mut h.db, &h.job, &h.owner, 1).await;
            }
            Fault::NewRound => {
                let mut chain = current_handle(&mut h.db, &h.attempt, &h.sealed).await;
                for slot in 0..ARTICLES {
                    for post in [true, false] {
                        chain = applied(
                            confirm_article(&mut h.db, &chain, slot, side_of(post)).await,
                            "hook confirm",
                        );
                    }
                }
                let done = applied(finish_verification(&mut h.db, &chain).await, "hook finish");
                applied(
                    begin_verification_round(&mut h.db, &done).await,
                    "hook new round",
                );
            }
        }
    }
}

impl GenerationTransport for Transport {
    async fn stat(
        &mut self,
        bundle: &StoredGeneration,
        slot: i32,
        side: ArticleSide,
    ) -> StatVerdict {
        self.record(bundle, Op::Stat, slot, side);
        let verdict = self
            .stat
            .get_mut(&(slot, side == ArticleSide::Post))
            .and_then(VecDeque::pop_front)
            .unwrap_or(StatVerdict::Present);
        self.fire(false).await;
        verdict
    }

    async fn repost(
        &mut self,
        bundle: &StoredGeneration,
        slot: i32,
        side: ArticleSide,
    ) -> PostVerdict {
        self.record(bundle, Op::Repost, slot, side);
        let verdict = self
            .post
            .get_mut(&(slot, side == ArticleSide::Post))
            .and_then(VecDeque::pop_front)
            .unwrap_or(PostVerdict::Posted);
        self.fire(true).await;
        verdict
    }
}

/// Take `FOR UPDATE NOWAIT` on every row the fence locks. A driver that held a
/// transaction across the network await would make this fail (and the test
/// panic) instead of letting the hook proceed.
async fn probe_unlocked(db: &Client, s: &Sealed, job: &str) {
    let probes: [(&str, &str); 4] = [
        (
            "SELECT 1 FROM posted_file_job_attempts WHERE job_id = $1 FOR UPDATE NOWAIT",
            job,
        ),
        (
            "SELECT 1 FROM posted_file_registry WHERE plaintext_sha256 = $1 FOR UPDATE NOWAIT",
            &s.hash,
        ),
        (
            "SELECT 1 FROM posted_file_operations WHERE id = $1 FOR UPDATE NOWAIT",
            &s.op,
        ),
        (
            "SELECT 1 FROM posted_file_health WHERE generation_id = $1 FOR UPDATE NOWAIT",
            &s.generation,
        ),
    ];
    for (sql, key) in probes {
        let rows = db
            .query(sql, &[&key])
            .await
            .quiet("a lock was held across the transport await");
        assert!(!rows.is_empty(), "probe target must exist");
    }
}

async fn current_handle(db: &mut Client, attempt: &AttemptHandle, s: &Sealed) -> GenerationHandle {
    match recover(db, attempt, &s.hash, &s.op).await {
        Ok(AdmissionOutcome::ResumedSameCandidate(handle, _)) => handle,
        _ => panic!("recovery did not resume the candidate"),
    }
}

// ---- fixtures and readers ---------------------------------------------------

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

/// Durable work cursor of the current round.
async fn cursor(db: &Client, s: &Sealed) -> i32 {
    db.query_one(
        "SELECT r.work_cursor FROM posted_file_recheck_schedule r \
         JOIN posted_file_health h \
           ON h.generation_id = r.generation_id AND h.round = r.round \
         WHERE r.generation_id = $1",
        &[&s.generation],
    )
    .await
    .quiet("read work cursor")
    .get(0)
}

/// Highest cursor across every round of the generation.
async fn max_cursor(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT COALESCE(max(work_cursor), 0)::int8 \
         FROM posted_file_recheck_schedule WHERE generation_id = $1",
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

/// `(slot, side, confirmed, repost_attempts)` of every stored row.
async fn rows(db: &Client, s: &Sealed) -> Vec<(i32, i16, bool, i32)> {
    db.query(
        "SELECT slot, side, confirmed, repost_attempts \
         FROM posted_file_article_progress WHERE generation_id = $1 \
         ORDER BY round, slot, side",
        &[&s.generation],
    )
    .await
    .quiet("read progress rows")
    .iter()
    .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
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

async fn content_rows(db: &Client, s: &Sealed) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_content_checks WHERE generation_id = $1",
        &[&s.generation],
    )
    .await
}

/// Digest of custody, registry, operation, health, references, progress and
/// schedule rows: any mutation changes it.
async fn frozen(db: &Client, s: &Sealed) -> Vec<Option<String>> {
    let mut all = snapshot(db, s).await;
    let row = db
        .query_one(
            "SELECT (SELECT md5(string_agg(t::text, ',' ORDER BY t.round, t.slot, t.side)) \
                FROM posted_file_article_progress t WHERE t.generation_id = $1), \
             (SELECT md5(string_agg(t::text, ',' ORDER BY t.round)) \
                FROM posted_file_recheck_schedule t WHERE t.generation_id = $1)",
            &[&s.generation],
        )
        .await
        .quiet("digest progress and schedule");
    all.push(row.get(0));
    all.push(row.get(1));
    all
}

// ---- tests ------------------------------------------------------------------

#[test]
fn postgres_driver_invalid_bounds_and_waiting_schedule_make_no_network_or_mutation() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        let mut t = Transport::new(s);
        let due = schedule(&db, s)
            .await
            .3
            .expect("waiting schedule has a due time");
        let before = frozen(&db, s).await;

        for bound in [0, STAT_BATCH_SIZE + 1, usize::MAX] {
            assert!(
                matches!(
                    drive_recheck(&mut db, &handle, &mut t, bound).await,
                    Err(StoreError::InvalidInput)
                ),
                "an out-of-range bound must be rejected"
            );
        }
        for bound in [1, STAT_BATCH_SIZE] {
            match drive_recheck(&mut db, &handle, &mut t, bound).await {
                Ok(RecheckStep::Waiting { not_before }) => assert_eq!(not_before, due),
                _ => panic!("an early pass must report the persisted due time"),
            }
        }
        assert!(
            t.calls.is_empty(),
            "waiting and invalid calls use no network"
        );
        assert!(before == frozen(&db, s).await, "waiting must not mutate");
        assert_eq!(cursor(&db, s).await, 0);

        // The bound is validated before the schedule, even when work is due.
        make_due(&db, s).await;
        let due_state = frozen(&db, s).await;
        assert!(matches!(
            drive_recheck(&mut db, &handle, &mut t, 0).await,
            Err(StoreError::InvalidInput)
        ));
        assert!(t.calls.is_empty());
        assert!(due_state == frozen(&db, s).await);
        assert!(t.exact);
    });
}

#[test]
fn postgres_driver_max_slots_bounds_cursor_and_resumes_remaining_slots_to_presence_only_finish() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        make_due(&db, s).await;
        let before = snapshot(&db, s).await;
        let mut t = Transport::new(s);

        // Two slot/side pairs only, then a durable cursor.
        let first = progress_handle(
            drive_recheck(&mut db, &handle, &mut t, 2).await,
            "first bounded slice",
        );
        assert!(t.calls == [st(0, true), st(0, false)]);
        assert_eq!(cursor(&db, s).await, 2);
        assert!(rows(&db, s).await == [(0, 0, true, 0), (0, 1, true, 0)]);
        let (_, passes, state, _) = schedule(&db, s).await;
        assert_eq!((passes, state.as_str()), (0, "waiting"));

        // A lost acknowledgment: recover by operation identity and resume.
        let resumed = current_handle(&mut db, &attempt, s).await;
        let second = progress_handle(
            drive_recheck(&mut db, &resumed, &mut t, 3).await,
            "second bounded slice",
        );
        assert!(t.calls[2..] == [st(1, true), st(1, false), st(2, true)]);
        assert_eq!(cursor(&db, s).await, 5);

        // The pre-recovery handle is stale and reaches no network.
        let seen = t.calls.len();
        assert!(matches!(
            drive_recheck(&mut db, &first, &mut t, 1).await,
            Ok(RecheckStep::StaleOrDiverged)
        ));
        assert_eq!(t.calls.len(), seen);

        let done = complete_handle(
            drive_recheck(&mut db, &second, &mut t, STAT_BATCH_SIZE).await,
            "final slice",
        );
        assert!(
            t.calls
                == [
                    st(0, true),
                    st(0, false),
                    st(1, true),
                    st(1, false),
                    st(2, true),
                    st(2, false)
                ],
            "each slot/side is STATed exactly once, in cursor order, with no repost"
        );
        assert!(t.exact, "the transport saw the exact stored sealed bundle");
        assert_eq!(
            cursor(&db, s).await,
            0,
            "a completed pass resets the cursor"
        );
        assert_eq!(schedule(&db, s).await.1, 1);
        assert_eq!(repost_sum(&db, s).await, 0);

        // Presence only: verified, but no negative state, retention, content,
        // selection, custody or reference change.
        let health = health_outcome(&db, s).await;
        assert_eq!(health.0, "verifying");
        assert!(health.1.is_none() && health.2.is_some() && health.3.is_none());
        assert_eq!(content_rows(&db, s).await, 0);
        let after = snapshot(&db, s).await;
        for index in [0, 1, 4] {
            assert!(
                before[index] == after[index],
                "custody, registry, references"
            );
        }
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
        let seen = t.calls.len();
        let _ = complete_handle(
            drive_recheck(&mut db, &done, &mut t, 1).await,
            "replay after finish",
        );
        assert_eq!(t.calls.len(), seen);
    });
}

#[test]
fn postgres_driver_missing_reposts_same_slot_side_and_posted_or_duplicate_waits_for_later_stat() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        make_due(&db, s).await;
        let mut t = Transport::new(s);
        t.script_stat(0, true, &[StatVerdict::Missing, StatVerdict::Present]);
        t.script_post(0, true, &[PostVerdict::Posted]);
        t.script_stat(1, false, &[StatVerdict::Missing, StatVerdict::Present]);
        t.script_post(1, false, &[PostVerdict::DuplicatePresent]);

        let next = progress_handle(
            drive_recheck(&mut db, &handle, &mut t, STAT_BATCH_SIZE).await,
            "first pass",
        );
        assert!(
            t.calls
                == [
                    st(0, true),
                    rp(0, true),
                    st(0, false),
                    st(1, true),
                    st(1, false),
                    rp(1, false),
                    st(2, true),
                    st(2, false)
                ],
            "a repost follows only an answered Missing and names the same slot/side"
        );
        assert!(t.exact);
        // Each answered repost counts once and confirms nothing.
        assert!(
            rows(&db, s).await
                == [
                    (0, 0, false, 1),
                    (0, 1, true, 0),
                    (1, 0, true, 0),
                    (1, 1, false, 1),
                    (2, 0, true, 0),
                    (2, 1, true, 0)
                ]
        );
        assert_eq!(repost_sum(&db, s).await, 2);
        assert_eq!(cursor(&db, s).await, 0);
        assert_eq!(schedule(&db, s).await.1, 1);
        assert!(
            health_outcome(&db, s).await.2.is_none(),
            "not presence complete"
        );

        // Only the next scheduled STAT can confirm; confirmed slots are skipped.
        make_due(&db, s).await;
        t.calls.clear();
        let _ = complete_handle(
            drive_recheck(&mut db, &next, &mut t, STAT_BATCH_SIZE).await,
            "second pass",
        );
        assert!(t.calls == [st(0, true), st(1, false)]);
        assert_eq!(repost_sum(&db, s).await, 2, "later STATs spend nothing");
        assert!(rows(&db, s).await.iter().all(|r| r.2));
        assert!(health_outcome(&db, s).await.2.is_some());
    });
}

#[test]
fn postgres_driver_transport_answers_never_post_or_spend_budget() {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        make_due(&db, s).await;
        let frozen_custody = snapshot(&db, s).await;
        let mut t = Transport::new(s);
        // STAT transport: unanswered, so it must never reach POST.
        t.script_stat(0, true, &[StatVerdict::Transport]);
        t.script_post(0, true, &[PostVerdict::Posted]);
        // POST transport and refusal are answered Missing but spend nothing.
        t.script_stat(0, false, &[StatVerdict::Missing]);
        t.script_post(0, false, &[PostVerdict::Transport]);
        t.script_stat(1, true, &[StatVerdict::Missing]);
        t.script_post(1, true, &[PostVerdict::Refused]);

        let next = progress_handle(
            drive_recheck(&mut db, &handle, &mut t, STAT_BATCH_SIZE).await,
            "transport pass",
        );
        assert!(
            t.calls
                == [
                    st(0, true),
                    st(0, false),
                    rp(0, false),
                    st(1, true),
                    rp(1, true),
                    st(1, false),
                    st(2, true),
                    st(2, false)
                ],
            "no POST follows a STAT transport answer"
        );
        assert!(
            rows(&db, s).await
                == [
                    (0, 0, false, 0),
                    (0, 1, false, 0),
                    (1, 0, false, 0),
                    (1, 1, true, 0),
                    (2, 0, true, 0),
                    (2, 1, true, 0)
                ]
        );
        assert_eq!(repost_sum(&db, s).await, 0);
        assert_eq!(cursor(&db, s).await, 0);
        assert_eq!(schedule(&db, s).await.1, 1);
        let health = health_outcome(&db, s).await;
        assert!(health.0 == "verifying" && health.1.is_none() && health.2.is_none());
        assert!(frozen_custody[0] == snapshot(&db, s).await[0]);

        // Later answered STATs confirm the three slots with no budget spent.
        make_due(&db, s).await;
        t.calls.clear();
        let _ = complete_handle(
            drive_recheck(&mut db, &next, &mut t, STAT_BATCH_SIZE).await,
            "recovery pass",
        );
        assert!(t.calls == [st(0, true), st(0, false), st(1, true)]);
        assert_eq!(repost_sum(&db, s).await, 0);
        assert!(t.exact);
    });
}

#[test]
fn postgres_driver_passes_reset_cursor_and_backoff_exhausts_to_keep_original_without_negative_effects(
) {
    run_test(async {
        let mut db = connect().await;
        let (c, _attempt, mut handle) = begun(&mut db).await;
        let s = &c.sealed;
        let baseline = health_outcome(&db, s).await;
        assert_eq!(baseline.0, "verifying");
        let custody = snapshot(&db, s).await;
        let mut t = Transport::new(s);
        t.script_stat(0, true, &[StatVerdict::Missing; MAX_RECHECKS as usize]);

        for done in 0..MAX_RECHECKS {
            make_due(&db, s).await;
            t.calls.clear();
            let before = db_now(&db).await;
            handle = progress_handle(
                drive_recheck(&mut db, &handle, &mut t, STAT_BATCH_SIZE).await,
                "recheck pass",
            );
            let after = db_now(&db).await;

            if done >= 1 {
                // Only the still-missing slot is STATed; its budget caps reposts.
                let expect: Vec<Call> = if done < MAX_REPOST_ATTEMPTS_PER_ARTICLE {
                    vec![st(0, true), rp(0, true)]
                } else {
                    vec![st(0, true)]
                };
                assert!(t.calls == expect, "budget cap must stop further reposts");
            }
            assert_eq!(
                cursor(&db, s).await,
                0,
                "a completed pass resets the cursor"
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
                assert!(not_before.is_none());
            }
            assert_eq!(
                repost_sum(&db, s).await,
                i64::from((done + 1).min(MAX_REPOST_ATTEMPTS_PER_ARTICLE))
            );
            assert!(
                health_outcome(&db, s).await == baseline,
                "no negative health"
            );
            let now = snapshot(&db, s).await;
            for index in [0, 1, 4] {
                assert!(
                    custody[index] == now[index],
                    "custody, registry, references"
                );
            }
        }

        // The cap keeps the original: no network, no mutation.
        let settled = frozen(&db, s).await;
        t.calls.clear();
        assert!(matches!(
            drive_recheck(&mut db, &handle, &mut t, STAT_BATCH_SIZE).await,
            Ok(RecheckStep::KeepOriginal)
        ));
        assert!(t.calls.is_empty());
        assert!(settled == frozen(&db, s).await);
        assert!(t.exact);
        assert_eq!(content_rows(&db, s).await, 0);
    });
}

#[test]
fn postgres_driver_resume_after_stat_callback_crash_skips_confirmed_slot_without_remint() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        make_due(&db, s).await;
        let custody = snapshot(&db, s).await;

        // Simulated crash: the STAT callback committed, the cursor never did.
        let permit = match prepare_presence_check(&mut db, &handle, 0, ArticleSide::Post).await {
            Ok(Some(permit)) => permit,
            _ => panic!("expected a presence permit"),
        };
        applied(
            record_presence_check(&mut db, &permit, StatVerdict::Present).await,
            "record presence before crash",
        );
        assert_eq!(cursor(&db, s).await, 0);

        let resumed = current_handle(&mut db, &attempt, s).await;
        let mut t = Transport::new(s);
        let next = progress_handle(
            drive_recheck(&mut db, &resumed, &mut t, 2).await,
            "resume after crash",
        );
        assert!(
            t.calls == [st(0, false)],
            "the already confirmed slot is skipped, not re-STATed or reposted"
        );
        assert_eq!(cursor(&db, s).await, 2);

        let _ = complete_handle(
            drive_recheck(&mut db, &next, &mut t, STAT_BATCH_SIZE).await,
            "finish resumed pass",
        );
        assert!(
            t.calls
                == [
                    st(0, false),
                    st(1, true),
                    st(1, false),
                    st(2, true),
                    st(2, false)
                ]
        );
        assert!(t.exact, "the stored sealed bundle was used, never reminted");
        assert_eq!(custody_rows(&db, s).await, 1);
        assert_eq!(operation_rows(&db, s).await, 1);
        let after = snapshot(&db, s).await;
        for index in [0, 1, 4] {
            assert!(custody[index] == after[index]);
        }
        assert_eq!(repost_sum(&db, s).await, 0);
    });
}

#[test]
fn postgres_driver_concurrent_mutation_during_network_makes_callback_stale_without_lock_held() {
    run_test(async {
        let mut db = connect().await;
        for fault in [
            Fault::Revision,
            Fault::OperationLease,
            Fault::JobStatus,
            Fault::AttemptSuperseded,
            Fault::NewRound,
        ] {
            for at_repost in [false, true] {
                let label = format!("{fault:?} at_repost={at_repost}");
                let (c, attempt, handle) = begun(&mut db).await;
                let s = &c.sealed;
                make_due(&db, s).await;
                let mut t = Transport::new(s);
                t.script_stat(0, true, &[StatVerdict::Missing]);
                t.hook = Some(Hook {
                    db: connect().await,
                    attempt,
                    job: c.job.clone(),
                    owner: c.owner.clone(),
                    sealed: s.clone(),
                    fault,
                    at_repost,
                    fired: false,
                });

                let result = drive_recheck(&mut db, &handle, &mut t, STAT_BATCH_SIZE).await;
                assert!(
                    t.hook.as_ref().is_some_and(|h| h.fired),
                    "{label}: hook must fire"
                );
                assert!(
                    matches!(result, Ok(RecheckStep::StaleOrDiverged)),
                    "{label}: the callback must be stale"
                );
                let expected: Vec<Call> = if at_repost {
                    vec![st(0, true), rp(0, true)]
                } else {
                    vec![st(0, true)]
                };
                assert!(t.calls == expected, "{label}: no further network");
                assert!(t.exact, "{label}: exact stored bundle");
                assert_eq!(repost_sum(&db, s).await, 0, "{label}: no repost spent");
                assert_eq!(max_cursor(&db, s).await, 0, "{label}: cursor advanced");
                assert_eq!(custody_rows(&db, s).await, 1, "{label}: custody changed");
                // The stale answer is never attached to the changed state.
                let stored = rows(&db, s).await;
                match (fault, at_repost) {
                    (Fault::NewRound, _) => {}
                    (Fault::Revision, false) => {
                        assert!(stored == [(2, 1, true, 0)], "{label}: rows")
                    }
                    (Fault::Revision, true) => {
                        assert!(
                            stored == [(0, 0, false, 0), (2, 1, true, 0)],
                            "{label}: rows"
                        )
                    }
                    (_, false) => assert!(stored.is_empty(), "{label}: rows"),
                    (_, true) => assert!(stored == [(0, 0, false, 0)], "{label}: rows"),
                }
            }
        }
    });
}

#[test]
fn postgres_migration_0040_replay_preserves_cursor_and_budget_and_corrupt_cursor_fails_closed() {
    run_blocking(true, async {
        let mut db = connect().await;
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        make_due(&db, s).await;
        let mut t = Transport::new(s);
        t.script_stat(0, true, &[StatVerdict::Missing]);
        let partial = progress_handle(
            drive_recheck(&mut db, &handle, &mut t, 2).await,
            "partial pass",
        );
        assert!(t.calls == [st(0, true), rp(0, true), st(0, false)]);
        assert_eq!(cursor(&db, s).await, 2);
        assert_eq!(repost_sum(&db, s).await, 1);
        let kept = (schedule(&db, s).await, rows(&db, s).await);
        let digest = frozen(&db, s).await;

        let ddl = connect().await;
        ddl.batch_execute(MIGRATION_0040)
            .await
            .quiet("replay migration 0040");
        ddl.batch_execute(MIGRATION_0040)
            .await
            .quiet("replay migration 0040 again");
        assert_eq!(
            cursor(&db, s).await,
            2,
            "replay must keep the partial cursor"
        );
        assert!(kept == (schedule(&db, s).await, rows(&db, s).await));
        assert!(
            digest == frozen(&db, s).await,
            "budget and custody are intact"
        );

        // A negative cursor is rejected by the schema.
        expect_db_error(
            db.execute(
                "UPDATE posted_file_recheck_schedule SET work_cursor = -1 \
                 WHERE generation_id = $1",
                &[&s.generation],
            )
            .await,
            &SqlState::CHECK_VIOLATION,
            None,
            "negative cursor",
        );
        assert_eq!(cursor(&db, s).await, 2);

        // An oversized cursor passes the CHECK but must fail closed.
        db.execute(
            "UPDATE posted_file_recheck_schedule SET work_cursor = $2 WHERE generation_id = $1",
            &[&s.generation, &(ARTICLES * 2 + 1)],
        )
        .await
        .quiet("corrupt cursor");
        let corrupt = frozen(&db, s).await;
        let seen = t.calls.len();
        assert!(matches!(
            drive_recheck(&mut db, &partial, &mut t, STAT_BATCH_SIZE).await,
            Err(StoreError::Inconsistent)
        ));
        assert!(matches!(
            prepare_recheck(&mut db, &partial).await,
            Err(StoreError::Inconsistent)
        ));
        assert_eq!(t.calls.len(), seen, "a corrupt cursor reaches no network");
        assert!(corrupt == frozen(&db, s).await, "and mutates nothing");

        // Repairing the cursor resumes the pass from the stored position.
        db.execute(
            "UPDATE posted_file_recheck_schedule SET work_cursor = 2 WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("repair cursor");
        let _ = progress_handle(
            drive_recheck(&mut db, &partial, &mut t, STAT_BATCH_SIZE).await,
            "resume repaired cursor",
        );
        assert!(
            t.calls[seen..] == [st(1, true), st(1, false), st(2, true), st(2, false)],
            "slots before the stored cursor are not repeated"
        );
        assert_eq!(cursor(&db, s).await, 0);
        assert_eq!(schedule(&db, s).await.1, 1);
        assert_eq!(repost_sum(&db, s).await, 1);
        assert!(t.exact);
    });
}

#[test]
fn postgres_driver_end_cursor_recovers_completion_without_network_or_false_confirmation() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, mut handle) = begun(&mut db).await;
        let s = &c.sealed;
        make_due(&db, s).await;
        // Model the last cursor commit succeeding before pass completion was
        // attempted. One transport-only slot remains unconfirmed in this pass.
        for slot in 0..ARTICLES {
            for post in [true, false] {
                if slot != 0 || !post {
                    handle = applied(
                        confirm_article(&mut db, &handle, slot, side_of(post)).await,
                        "confirm pre-crash evidence",
                    );
                }
            }
        }
        db.execute(
            "UPDATE posted_file_recheck_schedule SET work_cursor = $2 WHERE generation_id = $1",
            &[&s.generation, &(ARTICLES * 2)],
        )
        .await
        .quiet("model committed final cursor");
        let resumed = current_handle(&mut db, &attempt, s).await;
        let mut t = Transport::new(s);
        let next = progress_handle(
            drive_recheck(&mut db, &resumed, &mut t, 1).await,
            "recover completed cursor",
        );
        assert!(t.calls.is_empty());
        assert_eq!(cursor(&db, s).await, 0);
        assert_eq!(schedule(&db, s).await.1, 1);
        assert!(health_outcome(&db, s).await.2.is_none());
        make_due(&db, s).await;
        let _ = complete_handle(
            drive_recheck(&mut db, &next, &mut t, STAT_BATCH_SIZE).await,
            "confirm the remaining slot",
        );
        assert!(t.calls == [st(0, true)]);
        assert_eq!(repost_sum(&db, s).await, 0);
        assert!(t.exact);
    });
}

#[test]
fn postgres_driver_finished_presence_replays_with_zero_network() {
    run_test(async {
        let mut db = connect().await;
        // Not due on purpose: a lost presence-completion write is repaired
        // from committed evidence without consulting the schedule or network.
        let (c, _attempt, handle) = begun(&mut db).await;
        let s = &c.sealed;
        let mut unfinished = handle;
        for slot in 0..ARTICLES {
            for post in [true, false] {
                unfinished = applied(
                    confirm_article(&mut db, &unfinished, slot, side_of(post)).await,
                    "confirm fixture article",
                );
            }
        }
        assert!(health_outcome(&db, s).await.2.is_none());

        let mut t = Transport::new(s);
        let done = complete_handle(
            drive_recheck(&mut db, &unfinished, &mut t, STAT_BATCH_SIZE).await,
            "repair lost completion",
        );
        assert!(t.calls.is_empty(), "committed evidence needs no network");
        assert!(health_outcome(&db, s).await.2.is_some());
        assert_eq!(schedule(&db, s).await.1, 0, "no pass was counted");
        let settled = frozen(&db, s).await;

        // Replay, including an invalid-bound check that precedes everything.
        let again = complete_handle(
            drive_recheck(&mut db, &done, &mut t, 1).await,
            "replay finished presence",
        );
        assert!(matches!(
            drive_recheck(&mut db, &again, &mut t, 0).await,
            Err(StoreError::InvalidInput)
        ));
        assert!(matches!(
            drive_recheck(&mut db, &unfinished, &mut t, 1).await,
            Ok(RecheckStep::StaleOrDiverged)
        ));
        assert!(t.calls.is_empty());
        assert!(settled == frozen(&db, s).await, "replay must not mutate");
        assert_eq!(repost_sum(&db, s).await, 0);
        assert_eq!(content_rows(&db, s).await, 0);
    });
}
