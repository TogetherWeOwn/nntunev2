//! Independent, native, raw-row acceptance for the inert subscription reader.
//!
//! Required production seam (not a test-side implementation):
//! `billing_reader::{SubscriptionRow, SubscriptionSource,
//! decode_subscription_row, read_effective_plan}`. The decoder returns an owned
//! projection with the six `SubscriptionView` fields; its strings are `String`.
//! Row accessors are `text(&self, &str) -> Result<&str, StoreError>`,
//! `boolean(&self, &str) -> Result<bool, StoreError>`, and
//! `nullable_i64(&self, &str) -> Result<Option<i64>, StoreError>`.
//! `SubscriptionSource` has associated `Row: SubscriptionRow` and async
//! `select_subscription(&self, sql: &str, params: &[&str])
//! -> Result<Vec<Self::Row>, StoreError>`. Its operation includes connecting.
//! `read_effective_plan(&source, authoritative_user_id, now_unix, timeout_ms)`
//! encloses that operation, cardinality and shared decoding in one deadline,
//! then calls the existing pure resolver. No fake decodes or resolves a row.
//!
//! Breaks caught: identity/SQL substitution, error-to-absence fallback, epoch
//! coercion, decoder bypass, policy drift, per-phase budget reset, late success,
//! and retained I/O after timeout/caller cancellation. No DB/Worker/Stripe calls.
//! This suite does NOT prove PostgreSQL wire or Hyperdrive runtime behavior.

use nntune_api::billing::{resolve_effective_plan, EffectivePlan, SubscriptionView};
use nntune_api::billing_reader::{
    decode_subscription_row, read_effective_plan, SubscriptionRow, SubscriptionSource,
};
use nntune_api::handlers::StoreError;
use nntune_api::plan_limits::PlanId;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

const SQL: &str = "SELECT plan_id, status, cancel_at_period_end, EXTRACT(EPOCH FROM trial_end)::BIGINT AS trial_end_unix, EXTRACT(EPOCH FROM paid_through_at)::BIGINT AS paid_through_at_unix, EXTRACT(EPOCH FROM grace_until)::BIGINT AS grace_until_unix FROM subscriptions WHERE user_id = $1 AND status IN ('incomplete', 'trialing', 'active', 'past_due', 'unpaid', 'paused')";
const EPOCHS: [&str; 3] = ["trial_end_unix", "paid_through_at_unix", "grace_until_unix"];
const BUDGET_MS: u64 = 500;

#[derive(Clone)]
enum CellValue {
    Text(String),
    Bool(bool),
    Epoch(i64),
    Null,
}

struct RawRow {
    cells: BTreeMap<&'static str, CellValue>,
    decode_delay: Duration,
    delayed: Cell<bool>,
    drops: Option<Rc<Cell<usize>>>,
}

impl RawRow {
    fn new(plan: &str, status: &str) -> Self {
        Self {
            cells: BTreeMap::from([
                ("plan_id", CellValue::Text(plan.to_owned())),
                ("status", CellValue::Text(status.to_owned())),
                ("cancel_at_period_end", CellValue::Bool(false)),
                ("trial_end_unix", CellValue::Null),
                ("paid_through_at_unix", CellValue::Null),
                ("grace_until_unix", CellValue::Null),
            ]),
            decode_delay: Duration::ZERO,
            delayed: Cell::new(false),
            drops: None,
        }
    }

    fn epoch(mut self, column: &'static str, value: Option<i64>) -> Self {
        self.cells
            .insert(column, value.map_or(CellValue::Null, CellValue::Epoch));
        self
    }

    fn cancel(mut self, value: bool) -> Self {
        self.cells
            .insert("cancel_at_period_end", CellValue::Bool(value));
        self
    }

    fn before_decode(&self) {
        if !self.delayed.replace(true) {
            std::thread::sleep(self.decode_delay);
        }
    }
}

impl Drop for RawRow {
    fn drop(&mut self) {
        if let Some(drops) = &self.drops {
            drops.set(drops.get() + 1);
        }
    }
}

// These accessors model only typed driver retrieval, not the production decoder.
// In particular, absent columns/type errors MUST NOT become SQL NULL.
impl SubscriptionRow for RawRow {
    fn text(&self, column: &str) -> Result<&str, StoreError> {
        self.before_decode();
        match self.cells.get(column) {
            Some(CellValue::Text(value)) => Ok(value),
            _ => Err(StoreError),
        }
    }

    fn boolean(&self, column: &str) -> Result<bool, StoreError> {
        self.before_decode();
        match self.cells.get(column) {
            Some(CellValue::Bool(value)) => Ok(*value),
            _ => Err(StoreError),
        }
    }

    fn nullable_i64(&self, column: &str) -> Result<Option<i64>, StoreError> {
        self.before_decode();
        match self.cells.get(column) {
            Some(CellValue::Epoch(value)) => Ok(Some(*value)),
            Some(CellValue::Null) => Ok(None),
            _ => Err(StoreError),
        }
    }
}

#[derive(Clone, Copy)]
enum Phase {
    Ready,
    Error,
    Stall,
    Delay(u64),
}

async fn phase(value: Phase) -> Result<(), StoreError> {
    match value {
        Phase::Ready => Ok(()),
        Phase::Error => Err(StoreError),
        Phase::Stall => std::future::pending().await,
        Phase::Delay(ms) => {
            futures_timer::Delay::new(Duration::from_millis(ms)).await;
            Ok(())
        }
    }
}

struct OperationDrop(Rc<Cell<usize>>);
impl Drop for OperationDrop {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

struct RawSource {
    rows: RefCell<Option<Vec<RawRow>>>,
    calls: RefCell<Vec<(String, Vec<String>)>>,
    connect: Phase,
    query: Phase,
    stage: Cell<u8>,
    operation_drops: Rc<Cell<usize>>,
    gate: Option<Rc<Cell<bool>>>,
}

impl RawSource {
    fn new(rows: Vec<RawRow>) -> Self {
        Self {
            rows: RefCell::new(Some(rows)),
            calls: RefCell::new(Vec::new()),
            connect: Phase::Ready,
            query: Phase::Ready,
            stage: Cell::new(0),
            operation_drops: Rc::new(Cell::new(0)),
            gate: None,
        }
    }
}

impl SubscriptionSource for RawSource {
    type Row = RawRow;

    async fn select_subscription(
        &self,
        sql: &str,
        params: &[&str],
    ) -> Result<Vec<Self::Row>, StoreError> {
        let _operation = OperationDrop(self.operation_drops.clone());
        self.calls.borrow_mut().push((
            sql.to_owned(),
            params.iter().map(|value| (*value).to_owned()).collect(),
        ));
        self.stage.set(1);
        phase(self.connect).await?;
        self.stage.set(2);
        phase(self.query).await?;
        if let Some(gate) = &self.gate {
            std::future::poll_fn(|_| {
                if gate.get() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
        }
        self.stage.set(3);
        Ok(self
            .rows
            .borrow_mut()
            .take()
            .expect("reader must not retry"))
    }
}

struct ThreadWake(std::thread::Thread);
impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn finish<F: Future>(mut future: Pin<&mut F>) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let watchdog = Instant::now() + Duration::from_secs(3);
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return result;
        }
        assert!(
            Instant::now() < watchdog,
            "reader did not finish within watchdog"
        );
        std::thread::park_timeout(Duration::from_millis(10));
    }
}

fn run<F: Future>(future: F) -> F::Output {
    finish(std::pin::pin!(future))
}

fn want(plan: PlanId, entitled: bool) -> EffectivePlan {
    EffectivePlan { plan, entitled }
}

fn assert_opaque(error: StoreError) {
    let rendered = format!("{error:?}");
    for forbidden in [
        "SELECT",
        "subscriptions",
        "sentinel-user",
        "sentinel-plan",
        "postgres://",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "opaque error leaked {forbidden}"
        );
    }
}

#[test]
fn no_row_is_a_successful_closed_free_result_not_an_error() {
    let source = RawSource::new(vec![]);
    let result = run(read_effective_plan(
        &source,
        "sentinel-user",
        100,
        BUDGET_MS,
    ))
    .unwrap();
    assert_eq!(result, want(PlanId::Free, false));
    assert_eq!(
        source.calls.borrow().as_slice(),
        &[(SQL.to_owned(), vec!["sentinel-user".to_owned()])]
    );
}

#[test]
fn exact_sql_has_one_authoritative_binding_even_for_sql_shaped_user_data() {
    for user in ["user-a", "user-b", "'; SELECT 'sentinel-user' --"] {
        let source = RawSource::new(vec![]);
        run(read_effective_plan(&source, user, 100, BUDGET_MS)).unwrap();
        assert_eq!(
            source.calls.borrow().as_slice(),
            &[(SQL.to_owned(), vec![user.to_owned()])]
        );
    }
}

struct IdentitySource;
impl SubscriptionSource for IdentitySource {
    type Row = RawRow;
    async fn select_subscription(
        &self,
        sql: &str,
        params: &[&str],
    ) -> Result<Vec<Self::Row>, StoreError> {
        assert_eq!(sql, SQL);
        match params {
            ["user-a"] => Ok(vec![
                RawRow::new("plus", "active").epoch("paid_through_at_unix", Some(200))
            ]),
            ["user-b"] => Ok(vec![
                RawRow::new("pro-20x", "active").epoch("paid_through_at_unix", Some(200))
            ]),
            ["user-none"] => Ok(vec![]),
            _ => Err(StoreError),
        }
    }
}

#[test]
fn changing_authoritative_identity_changes_the_actual_resolved_row() {
    for (user, expected) in [
        ("user-a", want(PlanId::Plus, true)),
        ("user-b", want(PlanId::Pro20x, true)),
        ("user-none", want(PlanId::Free, false)),
    ] {
        assert_eq!(
            run(read_effective_plan(&IdentitySource, user, 100, BUDGET_MS)).unwrap(),
            expected
        );
    }
}

#[test]
fn connection_and_query_failures_are_errors_never_absence_or_entitlement() {
    for (connect, query) in [(Phase::Error, Phase::Ready), (Phase::Ready, Phase::Error)] {
        let mut source = RawSource::new(vec![
            RawRow::new("plus", "active").epoch("paid_through_at_unix", Some(200))
        ]);
        source.connect = connect;
        source.query = query;
        assert_opaque(
            run(read_effective_plan(
                &source,
                "sentinel-user",
                100,
                BUDGET_MS,
            ))
            .unwrap_err(),
        );
        assert_eq!(source.calls.borrow().len(), 1, "no reconnect/retry");
    }
}

#[test]
fn multiple_rows_are_an_opaque_error_not_first_row_or_absence() {
    for count in [2, 3] {
        let source = RawSource::new(
            (0..count)
                .map(|_| RawRow::new("plus", "active").epoch("paid_through_at_unix", Some(200)))
                .collect(),
        );
        assert_opaque(
            run(read_effective_plan(
                &source,
                "sentinel-user",
                100,
                BUDGET_MS,
            ))
            .unwrap_err(),
        );
    }
}

#[test]
fn each_nullable_epoch_preserves_null_zero_signed_and_full_i64_values() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/subscription_reader_epochs.json")).unwrap();
    let columns = fixture["nullable_epochs"].as_array().unwrap();
    assert_eq!(columns, &EPOCHS.map(Value::from));
    for (index, column) in EPOCHS.into_iter().enumerate() {
        for raw in fixture["values"].as_array().unwrap() {
            let value = if raw.is_null() {
                None
            } else {
                Some(raw.as_i64().unwrap())
            };
            let row = RawRow::new("plus", "active").epoch(column, value);
            let owned = decode_subscription_row(&row).unwrap();
            let mut expected = [None, None, None];
            expected[index] = value;
            assert_eq!(
                [
                    owned.trial_end_unix,
                    owned.paid_through_at_unix,
                    owned.grace_until_unix
                ],
                expected,
                "{column}: {raw}"
            );
        }
    }
}

#[test]
fn all_epoch_nullability_combinations_are_independent() {
    for mask in 0..8 {
        let expected = [
            (mask & 1 != 0).then_some(-7),
            (mask & 2 != 0).then_some(0),
            (mask & 4 != 0).then_some(19),
        ];
        let mut row = RawRow::new("pro-5x", "past_due");
        for (column, value) in EPOCHS.into_iter().zip(expected) {
            row = row.epoch(column, value);
        }
        let owned = decode_subscription_row(&row).unwrap();
        assert_eq!(
            [
                owned.trial_end_unix,
                owned.paid_through_at_unix,
                owned.grace_until_unix
            ],
            expected
        );
    }
}

#[test]
fn projection_owns_strings_and_preserves_boolean_after_driver_row_drops() {
    for canceled in [false, true] {
        let drops = Rc::new(Cell::new(0));
        let owned = {
            let mut row = RawRow::new("sentinel-plan-\u{00e9}", "sentinel-status").cancel(canceled);
            row.drops = Some(drops.clone());
            decode_subscription_row(&row).unwrap()
        };
        assert_eq!(drops.get(), 1);
        let _: &String = &owned.plan_id;
        let _: &String = &owned.status;
        assert_eq!(owned.plan_id, "sentinel-plan-\u{00e9}");
        assert_eq!(owned.status, "sentinel-status");
        assert_eq!(owned.cancel_at_period_end, canceled);
    }
}

#[test]
fn missing_null_or_wrong_mandatory_fields_fail_both_decoder_and_reader() {
    for column in ["plan_id", "status", "cancel_at_period_end"] {
        for bad in [None, Some(CellValue::Null), Some(CellValue::Epoch(1))] {
            let mut row = RawRow::new("plus", "active").epoch("paid_through_at_unix", Some(200));
            match bad {
                Some(value) => {
                    row.cells.insert(column, value);
                }
                None => {
                    row.cells.remove(column);
                }
            }
            assert!(decode_subscription_row(&row).is_err(), "mandatory {column}");
            let source = RawSource::new(vec![row]);
            assert_opaque(
                run(read_effective_plan(
                    &source,
                    "sentinel-user",
                    100,
                    BUDGET_MS,
                ))
                .unwrap_err(),
            );
        }
    }
    // A string spelling of a boolean is not a PostgreSQL boolean.
    let mut row = RawRow::new("plus", "active");
    row.cells
        .insert("cancel_at_period_end", CellValue::Text("false".to_owned()));
    assert!(decode_subscription_row(&row).is_err());
}

#[test]
fn missing_or_wrong_epoch_fields_are_decode_errors_not_sql_null() {
    for column in EPOCHS {
        for bad in [
            None,
            Some(CellValue::Text("0".to_owned())),
            Some(CellValue::Bool(false)),
        ] {
            let mut row = RawRow::new("plus", "trialing").epoch("trial_end_unix", Some(200));
            match bad {
                Some(value) => {
                    row.cells.insert(column, value);
                }
                None => {
                    row.cells.remove(column);
                }
            }
            assert!(decode_subscription_row(&row).is_err(), "epoch {column}");
            assert_opaque(
                run(read_effective_plan(
                    &RawSource::new(vec![row]),
                    "sentinel-user",
                    100,
                    BUDGET_MS,
                ))
                .unwrap_err(),
            );
        }
    }
}

#[test]
fn supplied_clock_controls_trial_before_at_and_after_end_even_for_unknown_plan() {
    for plan in ["plus", "unknown-tier"] {
        for (now, entitled) in [(99, true), (100, false), (101, false)] {
            let source = RawSource::new(vec![
                RawRow::new(plan, "trialing").epoch("trial_end_unix", Some(100))
            ]);
            assert_eq!(
                run(read_effective_plan(&source, "user-a", now, BUDGET_MS)).unwrap(),
                want(PlanId::Free, entitled)
            );
        }
    }
    let source = RawSource::new(vec![RawRow::new("pro-20x", "trialing")]);
    assert_eq!(
        run(read_effective_plan(&source, "user-a", 0, BUDGET_MS)).unwrap(),
        want(PlanId::Free, false)
    );
}

#[test]
fn active_paid_coverage_cancellation_and_settlement_cutoff_preserve_policy() {
    for (plan, tier) in [
        ("plus", PlanId::Plus),
        ("pro-5x", PlanId::Pro5x),
        ("pro-20x", PlanId::Pro20x),
    ] {
        for (canceled, now, entitled) in [
            (false, 99, true),
            (true, 99, true),
            (false, 100, true),
            (true, 100, false),
            (false, 10_899, true),
            (false, 10_900, false),
            (false, 10_901, false),
        ] {
            let source = RawSource::new(vec![RawRow::new(plan, "active")
                .cancel(canceled)
                .epoch("paid_through_at_unix", Some(100))]);
            assert_eq!(
                run(read_effective_plan(&source, "user-a", now, BUDGET_MS)).unwrap(),
                want(if entitled { tier } else { PlanId::Free }, entitled)
            );
        }
        let source = RawSource::new(vec![
            RawRow::new(plan, "active").epoch("grace_until_unix", Some(99_999))
        ]);
        assert_eq!(
            run(read_effective_plan(&source, "user-a", 100, BUDGET_MS)).unwrap(),
            want(PlanId::Free, false)
        );
    }
}

#[test]
fn past_due_coverage_grace_and_missing_paid_history_preserve_cancellation_semantics() {
    for canceled in [false, true] {
        for (paid, grace, now, entitled) in [
            (Some(100), None, 99, true),
            (Some(100), None, 100, false),
            (Some(100), Some(200), 100, true),
            (Some(100), Some(200), 199, true),
            (Some(100), Some(200), 200, false),
            (Some(100), Some(200), 201, false),
            (None, Some(200), 99, false),
            (None, None, 99, false),
        ] {
            let source = RawSource::new(vec![RawRow::new("pro-5x", "past_due")
                .cancel(canceled)
                .epoch("paid_through_at_unix", paid)
                .epoch("grace_until_unix", grace)]);
            assert_eq!(
                run(read_effective_plan(&source, "user-a", now, BUDGET_MS)).unwrap(),
                want(
                    if entitled {
                        PlanId::Pro5x
                    } else {
                        PlanId::Free
                    },
                    entitled
                )
            );
        }
    }
}

#[test]
fn free_unknown_paid_tiers_and_nonentitled_statuses_never_authorize_service() {
    for plan in ["free", "unknown-tier", "PLUS", " plus"] {
        for status in ["active", "past_due"] {
            let source = RawSource::new(vec![RawRow::new(plan, status)
                .epoch("paid_through_at_unix", Some(200))
                .epoch("grace_until_unix", Some(300))]);
            assert_eq!(
                run(read_effective_plan(&source, "user-a", 100, BUDGET_MS)).unwrap(),
                want(PlanId::Free, false)
            );
        }
    }
    for status in [
        "incomplete",
        "unpaid",
        "paused",
        "canceled",
        "incomplete_expired",
        "unknown-state",
        "ACTIVE",
    ] {
        let source = RawSource::new(vec![RawRow::new("pro-20x", status)
            .epoch("trial_end_unix", Some(200))
            .epoch("paid_through_at_unix", Some(200))
            .epoch("grace_until_unix", Some(300))]);
        assert_eq!(
            run(read_effective_plan(&source, "user-a", 100, BUDGET_MS)).unwrap(),
            want(PlanId::Free, false)
        );
    }
}

#[test]
fn signed_epochs_and_extreme_settlement_do_not_change_resolver_results() {
    for (status, paid, trial, grace, now) in [
        ("trialing", None, Some(0), None, -1),
        ("trialing", None, Some(-1), None, 0),
        ("active", Some(-20), None, None, -21),
        ("past_due", Some(-20), None, Some(0), -1),
        ("active", Some(i64::MAX), None, None, i64::MAX),
        ("active", Some(i64::MIN), None, None, i64::MIN + 10_800),
    ] {
        let view = SubscriptionView {
            plan_id: "plus",
            status,
            cancel_at_period_end: false,
            trial_end_unix: trial,
            paid_through_at_unix: paid,
            grace_until_unix: grace,
        };
        let source = RawSource::new(vec![RawRow::new("plus", status)
            .epoch("trial_end_unix", trial)
            .epoch("paid_through_at_unix", paid)
            .epoch("grace_until_unix", grace)]);
        assert_eq!(
            run(read_effective_plan(&source, "user-a", now, BUDGET_MS)).unwrap(),
            resolve_effective_plan(Some(view), now)
        );
    }
}

#[test]
fn ready_success_and_ready_failure_do_not_wait_for_the_deadline() {
    let source = RawSource::new(vec![]);
    let start = Instant::now();
    assert!(run(read_effective_plan(&source, "user-a", 100, 2_000)).is_ok());
    assert!(start.elapsed() < Duration::from_secs(1));
    let mut source = RawSource::new(vec![]);
    source.query = Phase::Error;
    assert!(run(read_effective_plan(&source, "user-a", 100, 2_000)).is_err());
}

#[test]
fn stalled_connect_or_query_times_out_once_and_drops_io_before_return() {
    for (connect, query, stage) in [
        (Phase::Stall, Phase::Ready, 1),
        (Phase::Ready, Phase::Stall, 2),
    ] {
        let mut source = RawSource::new(vec![]);
        source.connect = connect;
        source.query = query;
        let mut future = Box::pin(read_effective_plan(&source, "user-a", 100, 20));
        assert!(finish(future.as_mut()).is_err());
        assert_eq!(source.stage.get(), stage);
        assert_eq!(
            source.operation_drops.get(),
            1,
            "drop before outer future is dropped"
        );
        assert_eq!(source.calls.borrow().len(), 1);
    }
}

#[test]
fn prior_connect_time_is_charged_to_the_same_query_deadline() {
    let mut source = RawSource::new(vec![]);
    source.connect = Phase::Delay(60);
    source.query = Phase::Stall;
    let mut future = Box::pin(read_effective_plan(&source, "user-a", 100, 300));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut cx).is_pending());
    std::thread::sleep(Duration::from_millis(130));
    assert!(future.as_mut().poll(&mut cx).is_pending());
    assert_eq!(source.stage.get(), 2);
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        matches!(future.as_mut().poll(&mut cx), Poll::Ready(Err(_))),
        "query must not receive a fresh 300ms budget"
    );
    assert_eq!(source.operation_drops.get(), 1);
}

#[test]
fn expired_deadline_beats_a_row_becoming_ready_on_the_same_poll() {
    let gate = Rc::new(Cell::new(false));
    let mut source = RawSource::new(vec![
        RawRow::new("plus", "active").epoch("paid_through_at_unix", Some(200))
    ]);
    source.gate = Some(gate.clone());
    let mut future = Box::pin(read_effective_plan(&source, "user-a", 100, 20));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut cx).is_pending());
    std::thread::sleep(Duration::from_millis(35));
    gate.set(true);
    assert!(matches!(future.as_mut().poll(&mut cx), Poll::Ready(Err(_))));
    assert_eq!(source.operation_drops.get(), 1);
}

#[test]
fn decoding_cannot_return_late_entitlement_after_consuming_the_budget() {
    let mut row = RawRow::new("plus", "active").epoch("paid_through_at_unix", Some(200));
    row.decode_delay = Duration::from_millis(40);
    let drops = Rc::new(Cell::new(0));
    row.drops = Some(drops.clone());
    let source = RawSource::new(vec![row]);
    let mut future = Box::pin(read_effective_plan(&source, "user-a", 100, 15));
    assert!(
        finish(future.as_mut()).is_err(),
        "a query-only timeout leaves decoding outside the bound"
    );
    assert_eq!(
        drops.get(),
        1,
        "raw row must not remain retained in the completed future"
    );
}

#[test]
fn caller_cancellation_drops_an_inflight_read_without_retry() {
    let mut source = RawSource::new(vec![]);
    source.query = Phase::Stall;
    let mut future = Box::pin(read_effective_plan(&source, "user-a", 100, BUDGET_MS));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(future.as_mut().poll(&mut cx).is_pending());
    assert_eq!(source.operation_drops.get(), 0);
    drop(future);
    assert_eq!(source.operation_drops.get(), 1);
    assert_eq!(source.calls.borrow().len(), 1);
}

#[test]
fn real_postgres_rows_implement_the_same_decoder_input_contract() {
    fn require_shared_decoder<R: SubscriptionRow>() {}
    require_shared_decoder::<tokio_postgres::Row>();
}
