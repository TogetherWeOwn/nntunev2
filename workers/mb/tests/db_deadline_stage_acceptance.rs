//! Acceptance tests: bound every Hyperdrive connect/query with a finite
//! deadline and label database failures by stage.
//!
//! Wished-for public seam (the implementing change adds exactly this,
//! mirroring the api worker's deadline race — `worker::Delay` on wasm,
//! `futures_timer::Delay` natively, deadline polled before the operation
//! so it is armed before dispatch, both futures dropped on completion):
//!
//! ```ignore
//! pub enum RepoStage { Connect, Query, Decode, Timeout } // Copy, Eq, Debug
//! impl RepoStage { pub fn as_str(self) -> &'static str } // "connect"|"query"|"decode"|"timeout"
//! pub struct RepoError { /* Debug; stage only: no driver text, SQL or params */ }
//! impl RepoError { pub fn new(stage: RepoStage) -> Self; pub fn stage(&self) -> RepoStage }
//! pub const DB_OPERATION_DEADLINE_MS: u64; // production per-operation budget
//! pub async fn with_db_deadline<T>(budget: Duration, op: impl Future<Output = Result<T, RepoError>>) -> Result<T, RepoError>;
//! pub const DB_FAILED_EVENT: &str = "mb.db.failed";
//! pub fn db_failed_fields(stage: RepoStage, build_sha: &str) -> serde_json::Value; // exactly {"stage": .., "build_sha": ..}
//! ```
//!
//! Production wiring required (pinned by the companion
//! `db_deadline_stage_wiring_acceptance` target): `PgRepo::connect`
//! wraps `connect_raw`, and `lookup` / `search` / `release_recordings`
//! wrap each query separately, all via `with_db_deadline` with
//! `DB_OPERATION_DEADLINE_MS`; connect/driver/decode failures map to
//! `Connect` / `Query` / `Decode`; exactly one `mb.db.failed` line is
//! emitted per failed repository call, through `DB_FAILED_EVENT` +
//! `db_failed_fields`, carrying the stage only.
//!
//! Behavior pinned here:
//! - a never-resolving or over-budget operation fails as
//!   `RepoStage::Timeout` at (not long after, never before) the injected
//!   budget, and the abandoned operation future is dropped;
//! - a fast success/failure returns promptly (the deadline is released,
//!   not awaited) and a fast failure keeps its own stage;
//! - the production default is finite and at least the search SLO
//!   (600 ms), so a legitimate slow-but-in-SLO query is not killed;
//! - every stage renders one six-key `mb.db.failed` line through the
//!   real `nntune-logging` emitter, with the stage string as the only
//!   operational field beyond the build identity — no SQL, parameters,
//!   connection details, driver text or user input (sentinel scan);
//! - handlers keep the existing mapping for every stage, timeout
//!   included: the generic 500 `internal_error` body, never a 404,
//!   never cached (anti-enumeration and wire shape unchanged).
//!
//! No real database, Hyperdrive or network. Protected acceptance file:
//! the implementing change must pass it unchanged.

use nntune_mb::{
    db_failed_fields, handle_lookup, handle_release_recordings, handle_search_paged,
    with_db_deadline, Entity, Kind, MbRepo, Recording, RepoError, RepoStage, SearchHit,
    SuccessCache, DB_FAILED_EVENT, DB_OPERATION_DEADLINE_MS,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

// --- Executor ---------------------------------------------------------------

/// Poll-with-retry driver: re-polls every millisecond until ready, so a
/// timer-based deadline (which wakes through its own waker) and the
/// `Instant`-based slow futures below both make progress without any
/// async runtime. Panics past a generous ceiling so an unbounded
/// production future fails loudly instead of hanging the suite.
fn drive<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut future = Box::pin(future);
    let started = Instant::now();
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "future did not complete within 10s: the deadline is not bounding it"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A future that resolves to `value` once `ready_at` passes, and records
/// whether it was dropped (the deadline must drop, not leak or detach,
/// an abandoned operation).
struct Slow<T> {
    ready_at: Instant,
    value: Option<Result<T, RepoError>>,
    dropped: Rc<Cell<bool>>,
}

impl<T: Unpin> Future for Slow<T> {
    type Output = Result<T, RepoError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if Instant::now() >= self.ready_at {
            Poll::Ready(self.value.take().expect("polled after completion"))
        } else {
            Poll::Pending
        }
    }
}

impl<T> Drop for Slow<T> {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}

fn slow<T>(delay: Duration, value: Result<T, RepoError>, dropped: Rc<Cell<bool>>) -> Slow<T> {
    Slow {
        ready_at: Instant::now() + delay,
        value: Some(value),
        dropped,
    }
}

// --- Deadline behavior ------------------------------------------------------

#[test]
fn never_resolving_operation_times_out_at_the_injected_budget() {
    let budget = Duration::from_millis(30);
    let started = Instant::now();
    let result: Result<(), RepoError> = drive(with_db_deadline(budget, std::future::pending()));
    let elapsed = started.elapsed();
    assert_eq!(
        result
            .expect_err("a never-resolving operation must fail")
            .stage(),
        RepoStage::Timeout,
        "deadline expiry must be staged as timeout"
    );
    assert!(
        elapsed >= Duration::from_millis(25),
        "returned in {elapsed:?}: the deadline must actually wait for its budget"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "returned in {elapsed:?}: the deadline must fire at its budget, not long after"
    );
}

#[test]
fn the_budget_is_injectable_a_larger_budget_waits_longer() {
    let started = Instant::now();
    let result: Result<(), RepoError> = drive(with_db_deadline(
        Duration::from_millis(180),
        std::future::pending(),
    ));
    let elapsed = started.elapsed();
    assert_eq!(
        result.expect_err("must time out").stage(),
        RepoStage::Timeout
    );
    assert!(
        elapsed >= Duration::from_millis(150),
        "a 180ms budget returned in {elapsed:?}: the budget is not the injected one"
    );
}

#[test]
fn timed_out_operation_future_is_dropped_not_detached() {
    let dropped = Rc::new(Cell::new(false));
    let op = slow(
        Duration::from_secs(60),
        Ok::<_, RepoError>(()),
        Rc::clone(&dropped),
    );
    let result = drive(with_db_deadline(Duration::from_millis(30), op));
    assert_eq!(
        result.expect_err("must time out").stage(),
        RepoStage::Timeout
    );
    assert!(
        dropped.get(),
        "the abandoned operation future must be dropped when the deadline fires"
    );
}

#[test]
fn slow_operation_past_the_budget_times_out_without_waiting_for_it() {
    let dropped = Rc::new(Cell::new(false));
    let op = slow(
        Duration::from_millis(400),
        Ok::<_, RepoError>(7u32),
        Rc::clone(&dropped),
    );
    let started = Instant::now();
    let result = drive(with_db_deadline(Duration::from_millis(30), op));
    assert_eq!(
        result.expect_err("must time out").stage(),
        RepoStage::Timeout
    );
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "must fail at the budget, not when the slow operation eventually lands"
    );
}

#[test]
fn fast_success_returns_promptly_without_waiting_for_the_deadline() {
    let started = Instant::now();
    let result = drive(with_db_deadline(Duration::from_secs(10), async {
        Ok::<_, RepoError>(42u32)
    }));
    assert_eq!(result.expect("fast success passes through"), 42);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the deadline must be released on completion, not awaited"
    );
}

#[test]
fn slow_operation_inside_the_budget_still_succeeds() {
    let dropped = Rc::new(Cell::new(false));
    let op = slow(
        Duration::from_millis(40),
        Ok::<_, RepoError>("row"),
        Rc::clone(&dropped),
    );
    let result = drive(with_db_deadline(Duration::from_secs(10), op));
    assert_eq!(result.expect("in-budget operation passes through"), "row");
}

#[test]
fn fast_failure_keeps_its_own_stage_and_returns_promptly() {
    for stage in [RepoStage::Connect, RepoStage::Query, RepoStage::Decode] {
        let started = Instant::now();
        let result: Result<(), RepoError> =
            drive(with_db_deadline(Duration::from_secs(10), async move {
                Err(RepoError::new(stage))
            }));
        assert_eq!(
            result.expect_err("fast failure passes through").stage(),
            stage,
            "a failure inside the budget keeps its stage (never relabelled timeout)"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a fast failure must not wait for the deadline"
        );
    }
}

#[test]
fn production_default_budget_is_finite_and_covers_the_search_slo() {
    // Documented SLOs: lookup <50ms hit / <250ms miss, search <600ms.
    // The per-operation default must leave a legitimate in-SLO query
    // alive, and must still be a real bound (not minutes).
    assert!(
        DB_OPERATION_DEADLINE_MS >= 600,
        "default {DB_OPERATION_DEADLINE_MS}ms kills queries the 600ms search SLO allows"
    );
    assert!(
        DB_OPERATION_DEADLINE_MS <= 10_000,
        "default {DB_OPERATION_DEADLINE_MS}ms is not a meaningful bound"
    );
}

// --- Stage labels and the structured failure line ---------------------------

#[test]
fn stage_strings_are_the_documented_labels() {
    assert_eq!(RepoStage::Connect.as_str(), "connect");
    assert_eq!(RepoStage::Query.as_str(), "query");
    assert_eq!(RepoStage::Decode.as_str(), "decode");
    assert_eq!(RepoStage::Timeout.as_str(), "timeout");
    assert_eq!(DB_FAILED_EVENT, "mb.db.failed");
}

const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";

#[test]
fn each_stage_renders_one_six_key_mb_db_failed_line_carrying_only_the_stage() {
    use nntune_logging::{Level, LogRecord, Worker as LogWorker};

    for stage in [
        RepoStage::Connect,
        RepoStage::Query,
        RepoStage::Decode,
        RepoStage::Timeout,
    ] {
        let fields_value = db_failed_fields(stage, "unknown");
        let fields_obj = fields_value
            .as_object()
            .expect("db_failed_fields must be a JSON object");
        let mut keys: Vec<&str> = fields_obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["build_sha", "stage"],
            "fields must carry the stage and the build identity only"
        );
        assert_eq!(fields_obj["stage"], stage.as_str());

        let fields = nntune_logging::fields_from_json(fields_value)
            .expect("staged failure fields must pass emitter validation");
        let record = LogRecord::new(
            LogWorker::Mb,
            Level::Error,
            TRACE,
            DB_FAILED_EVENT,
            fields,
            1_792_094_400_123,
        )
        .expect("mb.db.failed must render through the real emitter");
        let line = record.to_json_line();
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        let object = parsed.as_object().unwrap();
        assert_eq!(object.len(), 6, "exactly the six envelope keys: {line}");
        assert_eq!(object["worker"], "mb");
        assert_eq!(object["level"], "error");
        assert_eq!(object["event"], "mb.db.failed");
        assert_eq!(object["trace_id"], TRACE);
        assert_eq!(object["fields"]["stage"], stage.as_str());
        assert_eq!(object["fields"]["build_sha"], "unknown");
        assert_eq!(record.trace_id(), TRACE);
        assert_eq!(record.event(), DB_FAILED_EVENT);
    }
}

#[test]
fn failure_lines_carry_no_sql_params_driver_text_or_user_input_sentinels() {
    use nntune_logging::{Level, LogRecord, Worker as LogWorker};

    // Sentinels standing in for everything the line must never contain.
    // `db_failed_fields` takes only a stage and the build identity, so a
    // green scan here plus the exact-key pin above proves no other
    // detail can reach the rendered line through this seam.
    const SENTINELS: &[&str] = &[
        "SELECT",
        "INSERT",
        "sentinel-sql-fragment",
        "sentinel-param-value",
        "sentinel-driver-error",
        "postgres://",
        "password",
        "host=",
        "123e4567-e89b-12d3-a456-426614174000",
        "sentinel-user-query",
    ];
    for stage in [
        RepoStage::Connect,
        RepoStage::Query,
        RepoStage::Decode,
        RepoStage::Timeout,
    ] {
        let fields = nntune_logging::fields_from_json(db_failed_fields(stage, "unknown"))
            .expect("fields must validate");
        let record = LogRecord::new(
            LogWorker::Mb,
            Level::Error,
            TRACE,
            DB_FAILED_EVENT,
            fields,
            1_792_094_400_123,
        )
        .expect("line must render");
        let line = record.to_json_line();
        for sentinel in SENTINELS {
            assert!(
                !line.contains(sentinel),
                "failure line leaked {sentinel:?}: {line}"
            );
        }
    }
}

// --- Handler mapping: every stage stays the generic 500 ---------------------

const MBID: &str = "123e4567-e89b-12d3-a456-426614174000";

struct FailingRepo {
    stage: RepoStage,
}

impl MbRepo for FailingRepo {
    async fn lookup(&self, _kind: Kind, _mbid: &str) -> Result<Option<Entity>, RepoError> {
        Err(RepoError::new(self.stage))
    }

    async fn search(
        &self,
        _kind: Kind,
        _normalized_q: &str,
        _limit: u32,
        _offset: u32,
    ) -> Result<Vec<SearchHit>, RepoError> {
        Err(RepoError::new(self.stage))
    }

    async fn release_recordings(
        &self,
        _release_mbid: &str,
    ) -> Result<Option<Vec<Recording>>, RepoError> {
        Err(RepoError::new(self.stage))
    }
}

#[derive(Default)]
struct FakeCache {
    stored: RefCell<HashMap<String, String>>,
    puts: RefCell<u32>,
}

impl SuccessCache for FakeCache {
    async fn get(&self, key: &str) -> Option<String> {
        self.stored.borrow().get(key).cloned()
    }

    async fn put(&self, key: &str, body: &str, _ttl_s: u32) {
        *self.puts.borrow_mut() += 1;
        self.stored
            .borrow_mut()
            .insert(key.to_string(), body.to_string());
    }
}

fn assert_generic_500(resp: &nntune_mb::ApiResponse) {
    assert_eq!(
        resp.status, 500,
        "a staged database failure (timeout included) stays the generic 500, \
         never a not-found or a new status"
    );
    assert_eq!(resp.cache_ttl, None, "failures are never cacheable");
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"code": "internal_error", "message": "unexpected error", "retryable": true}),
        "wire body must stay the existing generic internal_error shape"
    );
}

#[test]
fn every_stage_maps_to_the_generic_500_in_lookup() {
    for stage in [
        RepoStage::Connect,
        RepoStage::Query,
        RepoStage::Decode,
        RepoStage::Timeout,
    ] {
        let repo = FailingRepo { stage };
        let cache = FakeCache::default();
        let resp = drive(handle_lookup(&repo, &cache, Kind::Artist, MBID));
        assert_generic_500(&resp);
        assert_eq!(*cache.puts.borrow(), 0, "failures are never cached");
    }
}

#[test]
fn timeout_maps_to_the_generic_500_in_search_and_release_recordings() {
    let repo = FailingRepo {
        stage: RepoStage::Timeout,
    };
    let cache = FakeCache::default();
    let resp = drive(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "radiohead",
        None,
        None,
    ));
    assert_generic_500(&resp);
    let resp = drive(handle_release_recordings(&repo, &cache, MBID));
    assert_generic_500(&resp);
    assert_eq!(*cache.puts.borrow(), 0, "failures are never cached");
}
