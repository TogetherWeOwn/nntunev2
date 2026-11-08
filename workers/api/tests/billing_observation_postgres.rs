//! Billing reconcile ordering, rollback and lock behaviour through the real
//! production `PgStore::reconcile_billing_subscription` on CI's disposable
//! PostgreSQL. Run with --features postgres-tests --test
//! billing_observation_postgres. Missing test configuration fails; ordinary
//! unit tests do not require a database.
//!
//! Only connection acquisition and the Stripe transport are injected. The
//! store opens its own READ COMMITTED transaction, takes the per-user
//! advisory lock, loads its inputs, calls the provider fake, folds, writes
//! and commits, all with the production SQL and decoders. The provider fakes
//! are deterministic: every hold is a channel-style gate the test opens, and
//! the ordering facts are read from the server (`pg_locks`,
//! `pg_stat_activity`) or from rows a fake observes on a separate connection
//! at the instant the second pass reaches the provider. Nothing depends on a
//! sleep for correctness; every wait is a poll on server state or a gate
//! under an outer timeout, so a hang fails instead of stalling CI.
//!
//! Each test owns a private schema (migrations 0002, 0021, 0026, 0029) and
//! unique user ids, because advisory locks are database-wide.
use nntune_api::billing_reconcile::{FailClosedReason, ReconcileOutcome, RENEWAL_GRACE_SECS};
use nntune_api::handlers::{Store, StoreError};
use nntune_api::stripe::{
    CheckoutSession, CreateCheckoutSession, CreateCustomer, Customer, Invoice, InvoiceLine,
    InvoiceLinePeriod, InvoiceLines, InvoiceStatusTransitions, ListInvoices, StripeError,
    StripeResult, StripeTransport, Subscription, SubscriptionItem, SubscriptionItems,
};
use nntune_api::worker_entry::billing_store_for_postgres_test;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::task::{JoinHandle, LocalSet};
use tokio_postgres::{Client, Config, NoTls};

static SCHEMA: AtomicUsize = AtomicUsize::new(0);

const T0: i64 = 1_780_000_000;
const MONTH: i64 = 30 * 86_400;
const E1: i64 = T0 + MONTH;
const E2: i64 = T0 + 2 * MONTH;
const E3: i64 = T0 + 3 * MONTH;
const NOW: i64 = E1 + 3_600;
const PRICE_PLUS: &str = "price_plus_obs";
const PRICE_PRO: &str = "price_pro_obs";

// --- Database fixture -----------------------------------------------------------------

// Same local-throwaway-only guard as session_refresh_postgres: the driver's
// effective settings, including `hostaddr`, must be loopback.
fn disposable_config(url: &str) -> Result<Config, &'static str> {
    let config: Config = url.parse().map_err(|_| "invalid test database URL")?;
    if config.get_dbname() != Some("nntune_ci") {
        return Err("test database must be disposable");
    }
    if config.get_hosts().is_empty()
        || !config.get_hosts().iter().all(|host| match host {
            tokio_postgres::config::Host::Tcp(host) => {
                host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            }
            _ => false,
        })
        || !config.get_hostaddrs().iter().all(|addr| addr.is_loopback())
    {
        return Err("test database hosts and addresses must be local");
    }
    Ok(config)
}

async fn connect(config: &Config, schema: &str) -> Client {
    let (client, connection) = config
        .connect(NoTls)
        .await
        .expect("connect scratch PostgreSQL");
    // A killed or dropped peer ends the driver with an error; that is an
    // expected outcome of the cancellation tests, never a test failure.
    tokio::spawn(async move {
        let _ = connection.await;
    });
    if !schema.is_empty() {
        client
            .batch_execute(&format!("SET search_path TO {schema}"))
            .await
            .unwrap();
    }
    client
}

struct Fixture {
    config: Config,
    schema: String,
    /// Observer connection: never the connection under test.
    db: Rc<Client>,
}

#[derive(Clone)]
struct Acct {
    tag: String,
    user: String,
    customer: String,
    sub: String,
    item: String,
}

struct Seed {
    status: &'static str,
    plan: &'static str,
    paid_through: Option<i64>,
    anchor: Option<String>,
    grace: Option<i64>,
    cancel: bool,
}

impl Seed {
    fn active(paid_through: i64, anchor: Option<String>) -> Self {
        Seed {
            status: "active",
            plan: "plus",
            paid_through: Some(paid_through),
            anchor,
            grace: None,
            cancel: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Stored {
    status: String,
    plan: String,
    paid_through: Option<i64>,
    anchor: Option<String>,
    grace: Option<i64>,
    cancel: bool,
    customer: String,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("NNTUNE_TEST_DATABASE_URL")
            .expect("NNTUNE_TEST_DATABASE_URL must name CI's disposable database");
        let config = disposable_config(&url).expect("scratch database configuration");
        let schema = format!(
            "billing_obs_{}_{}",
            std::process::id(),
            SCHEMA.fetch_add(1, Ordering::Relaxed)
        );
        let db = connect(&config, "").await;
        db.batch_execute(&format!(
            "CREATE SCHEMA {schema}; SET search_path TO {schema}"
        ))
        .await
        .unwrap();
        for migration in [
            include_str!("../../../migrations/0002_auth_vault.sql"),
            include_str!("../../../migrations/0021_billing_schema.sql"),
            include_str!("../../../migrations/0026_plan_reseed_no_trial.sql"),
            include_str!("../../../migrations/0029_paid_invoice_history.sql"),
        ] {
            db.batch_execute(migration).await.unwrap();
        }
        db.batch_execute(&format!(
            "INSERT INTO plan_prices (stripe_price_id, plan_id, currency, price_cents, billing_interval, livemode, selectable) VALUES
             ('{PRICE_PLUS}', 'plus', 'usd', 800, 'month', FALSE, TRUE),
             ('{PRICE_PRO}', 'pro', 'usd', 1500, 'month', FALSE, TRUE)"
        ))
        .await
        .unwrap();
        Fixture {
            config,
            schema,
            db: Rc::new(db),
        }
    }

    async fn client(&self) -> Client {
        connect(&self.config, &self.schema).await
    }

    /// A user (and, when `bound`, its billing account with the Stripe
    /// customer already adopted). User ids embed the schema name so that the
    /// database-wide advisory lock key is unique per test.
    async fn account(&self, tag: &str, bound: bool) -> Acct {
        let acct = Acct {
            tag: tag.to_owned(),
            user: format!("{}_{tag}", self.schema),
            customer: format!("cus_{tag}"),
            sub: format!("sub_{tag}"),
            item: format!("si_{tag}"),
        };
        self.db
            .execute(
                "INSERT INTO users (id, email, created_at) VALUES ($1, $2, 1700000000)",
                &[&acct.user, &format!("{}@example.test", acct.user)],
            )
            .await
            .unwrap();
        if bound {
            self.db
                .execute(
                    "INSERT INTO billing_accounts (user_id, stripe_customer_id) VALUES ($1, $2)",
                    &[&acct.user, &acct.customer],
                )
                .await
                .unwrap();
        }
        acct
    }

    async fn seed(&self, acct: &Acct, seed: Seed) {
        self.db
            .execute(
                "INSERT INTO subscriptions (user_id, stripe_subscription_id, stripe_customer_id, stripe_subscription_item_id, stripe_price_id, plan_id, status, current_period_end, cancel_at_period_end, paid_through_at, paid_invoice_id, grace_until) VALUES ($1, $2, $3, $4, $5, $6, $7, to_timestamp($8::bigint), $9, to_timestamp($10::bigint), $11, to_timestamp($12::bigint))",
                &[
                    &acct.user,
                    &acct.sub,
                    &acct.customer,
                    &acct.item,
                    &PRICE_PLUS,
                    &seed.plan,
                    &seed.status,
                    &E1,
                    &seed.cancel,
                    &seed.paid_through,
                    &seed.anchor,
                    &seed.grace,
                ],
            )
            .await
            .unwrap();
    }
}

async fn stored(db: &Client, user: &str) -> Option<Stored> {
    let rows = db
        .query(
            "SELECT status, plan_id, EXTRACT(EPOCH FROM paid_through_at)::bigint, paid_invoice_id, EXTRACT(EPOCH FROM grace_until)::bigint, cancel_at_period_end, stripe_customer_id FROM subscriptions WHERE user_id = $1",
            &[&user],
        )
        .await
        .unwrap();
    assert!(rows.len() <= 1, "one subscription row per test user");
    rows.first().map(|row| Stored {
        status: row.get(0),
        plan: row.get(1),
        paid_through: row.get(2),
        anchor: row.get(3),
        grace: row.get(4),
        cancel: row.get(5),
        customer: row.get(6),
    })
}

async fn subscription_rows(db: &Client, user: &str) -> i64 {
    db.query_one(
        "SELECT count(*) FROM subscriptions WHERE user_id = $1",
        &[&user],
    )
    .await
    .unwrap()
    .get(0)
}

/// (invoice id, plan, period start, period end, amount cents, currency)
type HistoryRow = (String, String, i64, i64, i64, String);

async fn history(db: &Client, user: &str) -> Vec<HistoryRow> {
    db.query(
        "SELECT stripe_invoice_id, plan_id, EXTRACT(EPOCH FROM period_start)::bigint, EXTRACT(EPOCH FROM period_end)::bigint, amount_cents, currency FROM paid_invoice_history WHERE user_id = $1 ORDER BY stripe_invoice_id",
        &[&user],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5)))
    .collect()
}

// --- Server-side lock observation -------------------------------------------------------

/// Every backend holding (`true`) or waiting for (`false`) the per-user
/// advisory lock, read from the server's lock table. The key is the one the
/// production statement takes: `pg_advisory_xact_lock(hashtext(user_id))`.
async fn locks(db: &Client, user: &str) -> Vec<(i32, bool)> {
    db.query(
        "SELECT pid, granted FROM pg_locks WHERE locktype = 'advisory' AND objsubid = 1 AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND classid::bigint = ((hashtext($1)::bigint >> 32) & 4294967295) AND objid::bigint = (hashtext($1)::bigint & 4294967295) ORDER BY pid",
        &[&user],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect()
}

async fn wait_locks(db: &Client, user: &str, what: &str, ok: impl Fn(&[(i32, bool)]) -> bool) {
    let waited = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if ok(&locks(db, user).await) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    if waited.is_err() {
        panic!(
            "lock state never reached: {what}; saw {:?}",
            locks(db, user).await
        );
    }
}

async fn wait_unlocked(db: &Client, user: &str, what: &str) {
    wait_locks(db, user, what, |l| l.is_empty()).await;
}

/// One pass is parked at its provider gate: it holds the advisory lock and
/// its backend sits idle inside its single open transaction.
async fn assert_parked(fx: &Fixture, acct: &Acct, pid: i32) {
    wait_locks(&fx.db, &acct.user, "parked pass holds the lock", |l| {
        l.contains(&(pid, true))
    })
    .await;
    let waited = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state: Option<String> = fx
                .db
                .query_opt("SELECT state FROM pg_stat_activity WHERE pid = $1", &[&pid])
                .await
                .unwrap()
                .and_then(|r| r.get(0));
            if state.as_deref() == Some("idle in transaction") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        waited.is_ok(),
        "a pass parked at the provider must hold its transaction open"
    );
}

// --- Gates and the provider fake -----------------------------------------------------

#[derive(Default)]
struct GateState {
    open: Cell<bool>,
    wakers: RefCell<Vec<std::task::Waker>>,
}

/// One-shot latch. Single-threaded on purpose: the production store future
/// is not `Send`, so every scenario runs on one `LocalSet`.
#[derive(Clone, Default)]
struct Gate(Rc<GateState>);

impl Gate {
    fn open(&self) {
        self.0.open.set(true);
        let wakers = std::mem::take(&mut *self.0.wakers.borrow_mut());
        for waker in wakers {
            waker.wake();
        }
    }

    fn is_open(&self) -> bool {
        self.0.open.get()
    }

    async fn wait(&self) {
        std::future::poll_fn(|cx| {
            if self.0.open.get() {
                Poll::Ready(())
            } else {
                self.0.wakers.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    async fn reached(&self, what: &str) {
        if tokio::time::timeout(Duration::from_secs(10), self.wait())
            .await
            .is_err()
        {
            panic!("never reached: {what}");
        }
    }
}

type Log = Rc<RefCell<Vec<String>>>;
type Truth = Rc<RefCell<Remote>>;

/// The provider's current truth. A fake snapshots it when the pass enters
/// `retrieve_subscription` (the worst case for the design: a response that
/// stays in flight while the provider moves on).
#[derive(Clone)]
struct Remote {
    sub: Subscription,
    invoices: Vec<Invoice>,
    /// Continuation line pages by invoice id, served in order.
    more_lines: HashMap<String, Vec<InvoiceLines>>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Stage {
    Retrieve,
    List,
    Lines,
}

struct Hold {
    stage: Stage,
    entered: Gate,
    release: Gate,
}

struct Fake {
    name: &'static str,
    log: Log,
    truth: Truth,
    observer: Option<(Rc<Client>, String)>,
    hold: Option<Hold>,
    fail_at: Option<Stage>,
    retrieved: Gate,
    snapshot: RefCell<Option<Remote>>,
    /// The user's stored row as committed when this pass reached the
    /// provider, read on the observer connection.
    seen: RefCell<Option<Option<Stored>>>,
    pages: RefCell<HashMap<String, VecDeque<InvoiceLines>>>,
}

fn truth(remote: Remote) -> Truth {
    Rc::new(RefCell::new(remote))
}

impl Fake {
    fn new(name: &'static str, log: &Log, truth: &Truth) -> Self {
        Fake {
            name,
            log: log.clone(),
            truth: truth.clone(),
            observer: None,
            hold: None,
            fail_at: None,
            retrieved: Gate::default(),
            snapshot: RefCell::new(None),
            seen: RefCell::new(None),
            pages: RefCell::new(HashMap::new()),
        }
    }

    fn observe(mut self, fx: &Fixture, acct: &Acct) -> Self {
        self.observer = Some((fx.db.clone(), acct.user.clone()));
        self
    }

    fn with_hold(mut self, stage: Stage) -> Self {
        self.hold = Some(Hold {
            stage,
            entered: Gate::default(),
            release: Gate::default(),
        });
        self
    }

    fn failing(mut self, stage: Stage) -> Self {
        self.fail_at = Some(stage);
        self
    }

    fn hold_entered(&self) -> Gate {
        self.hold.as_ref().expect("hold").entered.clone()
    }

    fn release_hold(&self) {
        self.hold.as_ref().expect("hold").release.open();
    }

    fn calls(&self) -> usize {
        let prefix = format!("{}:", self.name);
        self.log
            .borrow()
            .iter()
            .filter(|entry| entry.starts_with(&prefix))
            .count()
    }

    fn snapshot_cancel(&self) -> bool {
        self.snapshot
            .borrow()
            .as_ref()
            .expect("retrieved")
            .sub
            .cancel_at_period_end
    }

    fn seen(&self) -> Option<Stored> {
        self.seen
            .borrow()
            .clone()
            .expect("observer recorded at retrieve")
    }

    async fn at(&self, stage: Stage) -> StripeResult<()> {
        self.log
            .borrow_mut()
            .push(format!("{}:{stage:?}", self.name));
        if stage == Stage::Retrieve {
            self.retrieved.open();
        }
        if let Some(hold) = &self.hold {
            if hold.stage == stage {
                hold.entered.open();
                hold.release.wait().await;
            }
        }
        if self.fail_at == Some(stage) {
            return Err(StripeError::transport());
        }
        Ok(())
    }
}

fn unsupported<'a, T: 'a>() -> Pin<Box<dyn Future<Output = StripeResult<T>> + 'a>> {
    Box::pin(async { Err(StripeError::transport()) })
}

impl StripeTransport for Fake {
    fn create_customer(
        &self,
        _req: CreateCustomer,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Customer>> + '_>> {
        unsupported()
    }
    fn create_checkout_session(
        &self,
        _req: CreateCheckoutSession,
    ) -> Pin<Box<dyn Future<Output = StripeResult<CheckoutSession>> + '_>> {
        unsupported()
    }
    fn retrieve_checkout_session(
        &self,
        _session_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<CheckoutSession>> + '_>> {
        unsupported()
    }
    fn expire_checkout_session(
        &self,
        _session_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<CheckoutSession>> + '_>> {
        unsupported()
    }
    fn retrieve_subscription(
        &self,
        _subscription_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Subscription>> + '_>> {
        Box::pin(async move {
            let snapshot = self.truth.borrow().clone();
            *self.pages.borrow_mut() = snapshot
                .more_lines
                .iter()
                .map(|(id, pages)| (id.clone(), pages.iter().cloned().collect()))
                .collect();
            *self.snapshot.borrow_mut() = Some(snapshot.clone());
            if let Some((db, user)) = &self.observer {
                let row = stored(db, user).await;
                *self.seen.borrow_mut() = Some(row);
            }
            self.at(Stage::Retrieve).await?;
            Ok(snapshot.sub)
        })
    }
    fn retrieve_invoice(
        &self,
        _invoice_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Invoice>> + '_>> {
        unsupported()
    }
    fn list_invoice_lines(
        &self,
        invoice_id: String,
        _starting_after: Option<String>,
        _limit: u8,
    ) -> Pin<Box<dyn Future<Output = StripeResult<InvoiceLines>> + '_>> {
        Box::pin(async move {
            self.at(Stage::Lines).await?;
            let page = self
                .pages
                .borrow_mut()
                .get_mut(&invoice_id)
                .and_then(VecDeque::pop_front);
            Ok(page.unwrap_or(InvoiceLines {
                data: Vec::new(),
                has_more: false,
            }))
        })
    }
    fn list_invoices(
        &self,
        _req: ListInvoices,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Vec<Invoice>>> + '_>> {
        Box::pin(async move {
            self.at(Stage::List).await?;
            Ok(self
                .snapshot
                .borrow()
                .as_ref()
                .expect("subscription retrieved first")
                .invoices
                .clone())
        })
    }
    fn cancel_subscription(
        &self,
        _subscription_id: String,
    ) -> Pin<Box<dyn Future<Output = StripeResult<Subscription>> + '_>> {
        unsupported()
    }
}

// --- Provider object builders -----------------------------------------------------------

fn line(id: &str, price: Option<&str>, amount: i64, start: i64, end: i64) -> InvoiceLine {
    InvoiceLine {
        id: id.to_owned(),
        amount,
        currency: Some("usd".to_owned()),
        price: price.map(str::to_owned),
        period: Some(InvoiceLinePeriod {
            start: Some(start),
            end: Some(end),
        }),
    }
}

impl Acct {
    fn inv_id(&self, n: &str) -> String {
        format!("in_{}_{n}", self.tag)
    }

    fn subscription(&self, status: &str, cancel: bool) -> Subscription {
        Subscription {
            id: self.sub.clone(),
            customer: Some(self.customer.clone()),
            status: status.to_owned(),
            cancel_at_period_end: cancel,
            current_period_end: Some(E1),
            items: Some(SubscriptionItems {
                data: vec![SubscriptionItem {
                    id: self.item.clone(),
                    price: Some(PRICE_PLUS.to_owned()),
                }],
            }),
            livemode: false,
        }
    }

    /// A paid invoice for one monthly plus period, paid at the period start.
    fn invoice(&self, n: &str, start: i64, end: i64) -> Invoice {
        let id = self.inv_id(n);
        self.invoice_lines(
            n,
            start,
            vec![line(&format!("{id}-li"), Some(PRICE_PLUS), 800, start, end)],
            false,
        )
    }

    fn invoice_lines(
        &self,
        n: &str,
        paid_at: i64,
        lines: Vec<InvoiceLine>,
        has_more: bool,
    ) -> Invoice {
        Invoice {
            id: self.inv_id(n),
            customer: Some(self.customer.clone()),
            subscription: Some(self.sub.clone()),
            status: "paid".to_owned(),
            amount_paid: 800,
            currency: Some("usd".to_owned()),
            status_transitions: Some(InvoiceStatusTransitions {
                paid_at: Some(paid_at),
            }),
            lines: Some(InvoiceLines {
                data: lines,
                has_more,
            }),
            livemode: false,
        }
    }

    fn remote(&self, status: &str, cancel: bool, invoices: Vec<Invoice>) -> Remote {
        Remote {
            sub: self.subscription(status, cancel),
            invoices,
            more_lines: HashMap::new(),
        }
    }

    /// Provider truth for a fresh renewal on top of invoice 1.
    fn renewed(&self) -> Remote {
        self.remote(
            "active",
            false,
            vec![self.invoice("1", T0, E1), self.invoice("2", E1, E2)],
        )
    }

    async fn seed_active(&self, fx: &Fixture) {
        fx.seed(self, Seed::active(E1, Some(self.inv_id("1"))))
            .await;
    }
}

// --- Pass driver ---------------------------------------------------------------------

type PassResult = Result<ReconcileOutcome, StoreError>;

struct Pass {
    pid: i32,
    task: JoinHandle<PassResult>,
}

/// Run one production pass on its own connection (the test constructor
/// consumes exactly one client, as the Worker consumes one Hyperdrive
/// connection per call).
async fn start(fx: &Fixture, acct: &Acct, fake: &Rc<Fake>) -> Pass {
    start_after(fx, acct, fake, None).await
}

async fn start_after(fx: &Fixture, acct: &Acct, fake: &Rc<Fake>, admission: Option<Gate>) -> Pass {
    let client = fx.client().await;
    let pid: i32 = client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let user = acct.user.clone();
    let sub = acct.sub.clone();
    let fake = fake.clone();
    let task = tokio::task::spawn_local(async move {
        if let Some(admission) = admission {
            admission.wait().await;
        }
        let store = billing_store_for_postgres_test(client);
        // Bind before returning: the borrowed future must drop before `store`.
        #[allow(clippy::let_and_return)]
        let outcome = store
            .reconcile_billing_subscription(&user, &sub, &*fake, NOW)
            .await;
        outcome
    });
    Pass { pid, task }
}

impl Pass {
    async fn done(self) -> PassResult {
        tokio::time::timeout(Duration::from_secs(25), self.task)
            .await
            .expect("pass did not finish")
            .expect("pass task panicked")
    }
}

async fn solo(fx: &Fixture, acct: &Acct, fake: &Rc<Fake>) -> PassResult {
    start(fx, acct, fake).await.done().await
}

/// First pass parks at its provider gate holding the lock; the second pass
/// is started, proven queued behind it on the server, and proven not to have
/// touched the provider; only then is the first released. `while_parked`
/// runs while both are in that state (the provider moves on).
async fn gated_pair(
    fx: &Fixture,
    acct: &Acct,
    first: &Rc<Fake>,
    second: &Rc<Fake>,
    while_parked: impl FnOnce(),
) -> (ReconcileOutcome, ReconcileOutcome) {
    gated_pair_in_arrival_order(fx, acct, first, second, false, while_parked).await
}

async fn gated_pair_in_arrival_order(
    fx: &Fixture,
    acct: &Acct,
    first: &Rc<Fake>,
    second: &Rc<Fake>,
    later_holder_started_first: bool,
    while_parked: impl FnOnce(),
) -> (ReconcileOutcome, ReconcileOutcome) {
    // A delayed caller may arrive first yet acquire second. It cannot carry
    // an observation across admission: the production seam retrieves afresh.
    let admission = Gate::default();
    let delayed = if later_holder_started_first {
        Some(start_after(fx, acct, second, Some(admission.clone())).await)
    } else {
        None
    };
    let a = start(fx, acct, first).await;
    first
        .hold_entered()
        .reached("first pass parked at its provider gate")
        .await;
    assert_parked(fx, acct, a.pid).await;
    let b = match delayed {
        Some(pass) => {
            admission.open();
            pass
        }
        None => start(fx, acct, second).await,
    };
    wait_locks(&fx.db, &acct.user, "second pass queued behind first", |l| {
        l.contains(&(a.pid, true)) && l.contains(&(b.pid, false))
    })
    .await;
    assert!(
        !second.retrieved.is_open() && second.calls() == 0,
        "second pass reached the provider before the first committed"
    );
    while_parked();
    first.release_hold();
    let first_outcome = a.done().await.expect("first pass");
    let second_outcome = b.done().await.expect("second pass");
    wait_unlocked(&fx.db, &acct.user, "both passes released the lock").await;
    (first_outcome, second_outcome)
}

async fn reconcile_once(
    fx: &Fixture,
    acct: &Acct,
    log: &Log,
    truth: &Truth,
    name: &'static str,
) -> ReconcileOutcome {
    let fake = Rc::new(Fake::new(name, log, truth));
    solo(fx, acct, &fake).await.expect(name)
}

async fn run(limit_secs: u64, body: impl Future<Output = ()>) {
    LocalSet::new()
        .run_until(async {
            tokio::time::timeout(Duration::from_secs(limit_secs), body)
                .await
                .expect("scenario exceeded its outer timeout")
        })
        .await
}

fn new_log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

// --- Equal evidence, cancellation flag, ABA ---------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn equal_evidence_cancellation_serializes_in_lock_order() {
    run(90, async {
        let fx = Fixture::new().await;
        // (stored, first observes, second observes): false->true, true->false
        // and both ABA shapes. The evidence (invoice 1, active) is identical in
        // every pass, so only lock order can decide the outcome.
        let cases = [
            (false, true, true),
            (true, false, false),
            (false, true, false),
            (true, false, true),
        ];
        for delayed in [false, true] {
            for (i, (stored_cancel, first_obs, second_obs)) in cases.into_iter().enumerate() {
                let acct = fx.account(&format!("cancel{i}_{delayed}"), true).await;
                fx.seed(
                    &acct,
                    Seed {
                        cancel: stored_cancel,
                        ..Seed::active(E1, Some(acct.inv_id("1")))
                    },
                )
                .await;
                let log = new_log();
                let t = truth(acct.remote("active", first_obs, vec![acct.invoice("1", T0, E1)]));
                let first = Rc::new(
                    Fake::new("A", &log, &t)
                        .observe(&fx, &acct)
                        .with_hold(Stage::Retrieve),
                );
                let second = Rc::new(Fake::new("B", &log, &t).observe(&fx, &acct));
                let moved = t.clone();
                let (a, b) =
                    gated_pair_in_arrival_order(&fx, &acct, &first, &second, delayed, move || {
                        moved.borrow_mut().sub.cancel_at_period_end = second_obs;
                    })
                    .await;
                assert!(a.fail_closed.is_none() && b.fail_closed.is_none());
                assert_eq!(first.snapshot_cancel(), first_obs);
                assert_eq!(second.snapshot_cancel(), second_obs);
                let before = first.seen().expect("row existed before the first pass");
                assert_eq!(before.cancel, stored_cancel);
                // The second pass reached the provider only after the first
                // commit: it saw the first pass's flag in the committed row.
                let committed = second.seen().expect("first pass committed the row");
                assert_eq!(committed.cancel, first_obs, "case {i}");
                assert_eq!(committed.paid_through, Some(E1));
                let last = stored(&fx.db, &acct.user).await.unwrap();
                assert_eq!(last.cancel, second_obs, "case {i}: last lock holder wins");
                assert_eq!(last.status, "active");
                assert_eq!(last.plan, "plus");
                assert_eq!(last.paid_through, Some(E1));
                assert_eq!(last.anchor, Some(acct.inv_id("1")));
                // The second pass's input load saw what the first committed.
                assert_eq!(b.read_anchor, Some(acct.inv_id("1")));
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn lock_order_not_caller_order_decides_and_retrieves_late() {
    run(60, async {
        let fx = Fixture::new().await;
        let acct = fx.account("reversed", true).await;
        acct.seed_active(&fx).await;
        let log = new_log();
        let t = truth(acct.remote("active", false, vec![acct.invoice("1", T0, E1)]));
        // An external holder keeps the lock while two callers queue. The
        // newer event's caller queues first and the older event's caller
        // second, so the older event's pass runs last.
        let blocker = fx.client().await;
        let blocker_pid: i32 = blocker
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        blocker.batch_execute("BEGIN").await.unwrap();
        blocker
            .query("SELECT pg_advisory_xact_lock(hashtext($1))", &[&acct.user])
            .await
            .unwrap();
        let newer = Rc::new(Fake::new("newer", &log, &t).observe(&fx, &acct));
        let older = Rc::new(Fake::new("older", &log, &t).observe(&fx, &acct));
        let first = start(&fx, &acct, &newer).await;
        wait_locks(&fx.db, &acct.user, "newer queued", |l| {
            l.contains(&(blocker_pid, true)) && l.contains(&(first.pid, false))
        })
        .await;
        let second = start(&fx, &acct, &older).await;
        wait_locks(&fx.db, &acct.user, "older queued", |l| {
            l.contains(&(second.pid, false)) && l.contains(&(first.pid, false))
        })
        .await;
        assert!(log.borrow().is_empty(), "no provider call while queued");
        // The provider moves on while both callers are blocked. Neither has
        // observed anything yet, so neither can write a view older than this.
        t.borrow_mut().sub.cancel_at_period_end = true;
        blocker.batch_execute("COMMIT").await.unwrap();
        first.done().await.expect("newer pass");
        second.done().await.expect("older pass");
        wait_unlocked(&fx.db, &acct.user, "all released").await;
        assert!(newer.snapshot_cancel() && older.snapshot_cancel());
        // Whoever won the queue ran start to finish before the other
        // retrieved: the provider calls never interleave.
        let log = log.borrow().clone();
        assert_eq!(log.len(), 4, "{log:?}");
        let (x, y) = (
            log[0].split(':').next().unwrap(),
            log[2].split(':').next().unwrap(),
        );
        assert_ne!(x, y);
        assert_eq!(
            log,
            [
                format!("{x}:Retrieve"),
                format!("{x}:List"),
                format!("{y}:Retrieve"),
                format!("{y}:List")
            ]
        );
        let (won, lost) = if x == "newer" {
            (&newer, &older)
        } else {
            (&older, &newer)
        };
        assert!(!won.seen().unwrap().cancel, "first holder saw the old row");
        assert!(lost.seen().unwrap().cancel, "second saw the first's commit");
        assert!(stored(&fx.db, &acct.user).await.unwrap().cancel);
    })
    .await;
}

// --- First row, independent users -----------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn absent_first_row_second_pass_reads_the_first_commit() {
    run(60, async {
        let fx = Fixture::new().await;
        // No billing account and no subscription row: the first pass
        // creates the row; the second must see it rather than a second
        // "no live row" view.
        let acct = fx.account("first", false).await;
        let log = new_log();
        let t = truth(acct.remote("active", false, vec![acct.invoice("1", T0, E1)]));
        let first = Rc::new(
            Fake::new("A", &log, &t)
                .observe(&fx, &acct)
                .with_hold(Stage::Retrieve),
        );
        let second = Rc::new(Fake::new("B", &log, &t).observe(&fx, &acct));
        let (a, b) = gated_pair(&fx, &acct, &first, &second, || {}).await;
        assert_eq!(first.seen(), None, "row was absent for the first pass");
        let seen = second.seen().expect("committed by the first pass");
        assert_eq!(seen.status, "active");
        assert_eq!(seen.plan, "plus");
        assert_eq!(seen.paid_through, Some(E1));
        assert_eq!(a.read_plan, None);
        assert_eq!(a.read_anchor, None);
        assert_eq!(b.read_plan.as_deref(), Some("plus"));
        assert_eq!(b.read_anchor, Some(acct.inv_id("1")));
        assert_eq!(subscription_rows(&fx.db, &acct.user).await, 1);
        let last = stored(&fx.db, &acct.user).await.unwrap();
        assert_eq!(last.customer, acct.customer);
        assert_eq!(last.paid_through, Some(E1));
        // Both passes emitted the history row; the invoice id is the key.
        assert_eq!(a.history.len(), 1);
        assert_eq!(b.history.len(), 1);
        assert_eq!(history(&fx.db, &acct.user).await.len(), 1);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn different_users_do_not_contend() {
    run(60, async {
        let fx = Fixture::new().await;
        let one = fx.account("indep1", true).await;
        let two = fx.account("indep2", true).await;
        one.seed_active(&fx).await;
        two.seed_active(&fx).await;
        let log = new_log();
        let t1 = truth(one.renewed());
        let t2 = truth(two.renewed());
        let parked = Rc::new(Fake::new("one", &log, &t1).with_hold(Stage::Retrieve));
        let other = Rc::new(Fake::new("two", &log, &t2));
        let a = start(&fx, &one, &parked).await;
        parked.hold_entered().reached("user one parked").await;
        assert_parked(&fx, &one, a.pid).await;
        // User two completes while user one still holds its own lock.
        let c = solo(&fx, &two, &other).await.expect("user two pass");
        assert_eq!(c.row.paid_through_unix, Some(E2));
        assert!(!a.task.is_finished());
        assert!(locks(&fx.db, &two.user).await.is_empty());
        assert!(locks(&fx.db, &one.user).await.contains(&(a.pid, true)));
        assert_eq!(
            stored(&fx.db, &one.user).await.unwrap().paid_through,
            Some(E1)
        );
        parked.release_hold();
        a.done().await.expect("user one pass");
        assert_eq!(
            stored(&fx.db, &one.user).await.unwrap().paid_through,
            Some(E2)
        );
        assert_eq!(
            stored(&fx.db, &two.user).await.unwrap().paid_through,
            Some(E2)
        );
    })
    .await;
}

// --- Unknown and known status acquisition order ------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Start {
    Missing,
    Present,
    NoAnchor,
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_and_known_status_follow_lock_order() {
    run(150, async {
        let fx = Fixture::new().await;
        let mut n = 0;
        for start in [Start::Missing, Start::Present, Start::NoAnchor] {
            // `trialing` is an unknown Checkout-shape status: it fails
            // closed AND leaves the live slot, so the next pass loads no
            // live row at all. Both acquisition orders must still land the
            // last holder's status on the one row.
            for (first_status, second_status) in [("trialing", "active"), ("active", "trialing")] {
                n += 1;
                let acct = fx.account(&format!("status{n}"), true).await;
                match start {
                    Start::Missing => {}
                    Start::Present => acct.seed_active(&fx).await,
                    Start::NoAnchor => fx.seed(&acct, Seed::active(E1, None)).await,
                }
                let ctx = format!("{start:?} {first_status}->{second_status}");
                let log = new_log();
                let t = truth(acct.remote(first_status, false, vec![acct.invoice("1", T0, E1)]));
                let first = Rc::new(
                    Fake::new("A", &log, &t)
                        .observe(&fx, &acct)
                        .with_hold(Stage::Retrieve),
                );
                let second = Rc::new(Fake::new("B", &log, &t).observe(&fx, &acct));
                let moved = t.clone();
                let (a, b) = gated_pair(&fx, &acct, &first, &second, move || {
                    moved.borrow_mut().sub.status = second_status.to_owned();
                })
                .await;
                let expect = |status: &str| {
                    (status == "trialing").then_some(FailClosedReason::UnknownStatus)
                };
                assert_eq!(a.fail_closed, expect(first_status), "{ctx}");
                assert_eq!(b.fail_closed, expect(second_status), "{ctx}");
                assert_eq!(a.disable_new_purchases(), first_status == "trialing");
                assert_eq!(b.disable_new_purchases(), second_status == "trialing");
                // The second pass reached the provider after the first
                // commit and saw its verbatim status.
                assert_eq!(second.seen().expect(&ctx).status, first_status, "{ctx}");
                assert_eq!(subscription_rows(&fx.db, &acct.user).await, 1, "{ctx}");
                let last = stored(&fx.db, &acct.user).await.unwrap();
                assert_eq!(last.status, second_status, "{ctx}");
                assert_eq!(last.plan, "plus", "{ctx}");
                assert_eq!(last.paid_through, Some(E1), "{ctx}");
                assert_eq!(last.customer, acct.customer, "{ctx}");
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_price_precedes_anchor_containment_in_both_acquisition_orders() {
    run(180, async {
        let fx = Fixture::new().await;
        let mut n = 0;
        for initial in [Start::Missing, Start::Present, Start::NoAnchor] {
            for missing_anchor in [false, true] {
                for unknown_first in [false, true] {
                    for delayed in [false, true] {
                        n += 1;
                        let acct = fx.account(&format!("price{n}"), true).await;
                        match initial {
                            Start::Missing => {}
                            Start::Present | Start::NoAnchor => {
                                fx.seed(
                                    &acct,
                                    Seed {
                                        status: "past_due",
                                        grace: Some(E1 + RENEWAL_GRACE_SECS),
                                        ..Seed::active(
                                            E1,
                                            matches!(initial, Start::Present)
                                                .then(|| acct.inv_id("1")),
                                        )
                                    },
                                )
                                .await
                            }
                        }
                        let known = acct.renewed();
                        let mut unknown = acct.remote(
                            "past_due",
                            true,
                            if missing_anchor {
                                vec![acct.invoice("3", E2, E3)]
                            } else {
                                vec![
                                    acct.invoice("1", T0, E1),
                                    acct.invoice("2", E1, E2),
                                    acct.invoice("3", E2, E3),
                                ]
                            },
                        );
                        unknown.sub.items.as_mut().unwrap().data[0].price =
                            Some("price_unmapped".into());
                        let (first_remote, second_remote) = if unknown_first {
                            (unknown, known)
                        } else {
                            (known, unknown)
                        };
                        let log = new_log();
                        let t = truth(first_remote);
                        // This stale prefetched value is deliberately retained
                        // by the delayed caller, but the production API accepts
                        // only an id and transport, so it must retrieve again.
                        let prefetched = t.borrow().sub.clone();
                        let first = Rc::new(
                            Fake::new("A", &log, &t)
                                .observe(&fx, &acct)
                                .with_hold(Stage::Retrieve),
                        );
                        let second = Rc::new(Fake::new("B", &log, &t).observe(&fx, &acct));
                        let moved = t.clone();
                        let (a, b) = gated_pair_in_arrival_order(
                            &fx,
                            &acct,
                            &first,
                            &second,
                            delayed,
                            move || *moved.borrow_mut() = second_remote,
                        )
                        .await;
                        let (bad, good) = if unknown_first { (&a, &b) } else { (&b, &a) };
                        assert_eq!(
                            bad.fail_closed,
                            Some(FailClosedReason::UnknownPrice),
                            "case {n}"
                        );
                        assert!(bad.disable_new_purchases() && bad.history.is_empty());
                        assert!(good.fail_closed.is_none() && !good.disable_new_purchases());
                        let expected_bad_end = if !unknown_first {
                            Some(E2)
                        } else if matches!(initial, Start::Missing) {
                            None
                        } else {
                            Some(E1)
                        };
                        assert_eq!(bad.row.paid_through_unix, expected_bad_end, "case {n}");
                        assert_eq!(
                            bad.row.plan_id,
                            if expected_bad_end.is_some() {
                                "plus"
                            } else {
                                "free"
                            }
                        );
                        assert_eq!(
                            bad.row.grace_until_unix,
                            (unknown_first && !matches!(initial, Start::Missing))
                                .then_some(E1 + RENEWAL_GRACE_SECS),
                            "unknown retains only its previous deadline"
                        );
                        let last = stored(&fx.db, &acct.user).await.unwrap();
                        assert_eq!(last.paid_through, Some(E2), "case {n}");
                        assert_eq!(last.plan, "plus");
                        assert_eq!(
                            last.status,
                            if unknown_first { "active" } else { "past_due" }
                        );
                        assert_eq!(last.cancel, !unknown_first);
                        let observed_price: String = fx
                            .db
                            .query_one(
                                "SELECT stripe_price_id FROM subscriptions WHERE user_id = $1",
                                &[&acct.user],
                            )
                            .await
                            .unwrap()
                            .get(0);
                        assert_eq!(
                            observed_price,
                            if unknown_first {
                                PRICE_PLUS
                            } else {
                                "price_unmapped"
                            }
                        );
                        assert_eq!(last.anchor, Some(acct.inv_id("2")));
                        assert_eq!(history(&fx.db, &acct.user).await.len(), 2);
                        let reread = second.snapshot.borrow();
                        let reread = &reread.as_ref().unwrap().sub;
                        assert_ne!(
                            prefetched.items.as_ref().unwrap().data[0].price,
                            reread.items.as_ref().unwrap().data[0].price
                        );
                    }
                }
            }
        }
    })
    .await;
}

// --- Wrong customer, mode, subscription --------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn wrong_customer_mode_and_subscription_never_advance_coverage() {
    run(120, async {
        let fx = Fixture::new().await;
        let log = new_log();

        // Customer other than the bound one: fail closed, binding kept.
        let acct = fx.account("wrongcus", true).await;
        acct.seed_active(&fx).await;
        let before = stored(&fx.db, &acct.user).await.unwrap();
        let mut remote = acct.renewed();
        remote.sub.customer = Some("cus_foreign".to_owned());
        remote.sub.status = "past_due".to_owned();
        let fake = Rc::new(Fake::new("w1", &log, &truth(remote)));
        let outcome = solo(&fx, &acct, &fake).await.expect("recorded, not erred");
        assert_eq!(
            outcome.fail_closed,
            Some(FailClosedReason::CustomerMismatch)
        );
        assert!(outcome.disable_new_purchases() && outcome.history.is_empty());
        let after = stored(&fx.db, &acct.user).await.unwrap();
        assert_eq!(after.customer, acct.customer);
        assert_eq!(after.paid_through, before.paid_through);
        assert_eq!(after.anchor, before.anchor);
        assert_eq!(after.plan, before.plan);
        assert!(history(&fx.db, &acct.user).await.is_empty());

        // Live-mode subscription on a test-mode worker.
        let acct = fx.account("livesub", true).await;
        acct.seed_active(&fx).await;
        let mut remote = acct.renewed();
        remote.sub.livemode = true;
        let fake = Rc::new(Fake::new("w2", &log, &truth(remote)));
        let outcome = solo(&fx, &acct, &fake).await.expect("recorded");
        assert_eq!(outcome.fail_closed, Some(FailClosedReason::WrongLivemode));
        let after = stored(&fx.db, &acct.user).await.unwrap();
        assert_eq!(
            (after.paid_through, after.plan.as_str()),
            (Some(E1), "plus")
        );
        assert!(history(&fx.db, &acct.user).await.is_empty());

        // Live-mode invoice inside an otherwise valid listing: nothing from
        // the pass lands, including the valid invoice's history.
        let acct = fx.account("liveinv", true).await;
        acct.seed_active(&fx).await;
        let mut live_invoice = acct.invoice("2", E1, E2);
        live_invoice.livemode = true;
        let remote = acct.remote(
            "active",
            false,
            vec![acct.invoice("1", T0, E1), live_invoice],
        );
        let fake = Rc::new(Fake::new("w3", &log, &truth(remote)));
        let outcome = solo(&fx, &acct, &fake).await.expect("recorded");
        assert_eq!(outcome.fail_closed, Some(FailClosedReason::WrongLivemode));
        assert_eq!(
            stored(&fx.db, &acct.user).await.unwrap().paid_through,
            Some(E1)
        );
        assert!(history(&fx.db, &acct.user).await.is_empty());

        // A different subscription id than the one requested is an error
        // (the observation is not for this target): rolled back, lock free,
        // and the account is still reconcilable afterwards.
        let acct = fx.account("wrongsub", true).await;
        acct.seed_active(&fx).await;
        let before = stored(&fx.db, &acct.user).await.unwrap();
        let mut remote = acct.renewed();
        remote.sub.id = "sub_someone_else".to_owned();
        let fake = Rc::new(Fake::new("w4", &log, &truth(remote)));
        assert!(solo(&fx, &acct, &fake).await.is_err());
        wait_unlocked(&fx.db, &acct.user, "lock released after wrong subscription").await;
        assert_eq!(stored(&fx.db, &acct.user).await.unwrap(), before);
        assert!(history(&fx.db, &acct.user).await.is_empty());
        let fake = Rc::new(Fake::new("w5", &log, &truth(acct.renewed())));
        solo(&fx, &acct, &fake).await.expect("retry after error");
        assert_eq!(
            stored(&fx.db, &acct.user).await.unwrap().paid_through,
            Some(E2)
        );

        // Invoices bound to another subscription or customer, or unpaid,
        // are skipped: no coverage, no history.
        let acct = fx.account("foreigninv", true).await;
        acct.seed_active(&fx).await;
        let mut other_sub = acct.invoice("sub", E1, E3);
        other_sub.subscription = Some("sub_other".to_owned());
        let mut other_customer = acct.invoice("cus", E1, E3);
        other_customer.customer = Some("cus_other".to_owned());
        let mut unpaid = acct.invoice("open", E1, E3);
        unpaid.status = "open".to_owned();
        let remote = acct.remote(
            "active",
            false,
            vec![acct.invoice("1", T0, E1), other_sub, other_customer, unpaid],
        );
        let fake = Rc::new(Fake::new("w6", &log, &truth(remote)));
        let outcome = solo(&fx, &acct, &fake).await.expect("clean skip");
        assert!(outcome.fail_closed.is_none());
        assert_eq!(
            stored(&fx.db, &acct.user).await.unwrap().paid_through,
            Some(E1)
        );
        let rows = history(&fx.db, &acct.user).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, acct.inv_id("1"));
    })
    .await;
}

// --- Fresh renewal, stale guards, history idempotency ---------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn renewal_grace_stale_guard_and_history_are_idempotent() {
    run(90, async {
        let fx = Fixture::new().await;
        let acct = fx.account("renew", true).await;
        acct.seed_active(&fx).await;
        let log = new_log();
        let t = truth(acct.renewed());
        let pass = |name: &'static str| reconcile_once(&fx, &acct, &log, &t, name);

        // Fresh renewal: coverage and anchor advance, both invoices row.
        let outcome = pass("r1").await;
        assert_eq!(outcome.history.len(), 2);
        let renewed = stored(&fx.db, &acct.user).await.unwrap();
        assert_eq!(renewed.paid_through, Some(E2));
        assert_eq!(renewed.anchor, Some(acct.inv_id("2")));
        assert_eq!(
            (renewed.status.as_str(), renewed.plan.as_str()),
            ("active", "plus")
        );
        assert_eq!(renewed.grace, None);
        let rows = history(&fx.db, &acct.user).await;
        assert_eq!(
            rows,
            vec![
                (
                    acct.inv_id("1"),
                    "plus".to_owned(),
                    T0,
                    E1,
                    800,
                    "usd".to_owned()
                ),
                (
                    acct.inv_id("2"),
                    "plus".to_owned(),
                    E1,
                    E2,
                    800,
                    "usd".to_owned()
                ),
            ]
        );

        // Replaying the same snapshot changes nothing.
        pass("r2").await;
        assert_eq!(stored(&fx.db, &acct.user).await.unwrap(), renewed);
        assert_eq!(history(&fx.db, &acct.user).await, rows);

        // First failed renewal sets the grace once; a retry never extends it.
        t.borrow_mut().sub.status = "past_due".to_owned();
        pass("r3").await;
        let failing = stored(&fx.db, &acct.user).await.unwrap();
        assert_eq!(failing.status, "past_due");
        assert_eq!(failing.grace, Some(E2 + RENEWAL_GRACE_SECS));
        assert_eq!(failing.paid_through, Some(E2));
        pass("r4").await;
        assert_eq!(stored(&fx.db, &acct.user).await.unwrap(), failing);

        // A snapshot that does not contain the stored anchor is stale: it
        // cannot regress status, grace, anchor or coverage.
        *t.borrow_mut() = acct.remote("active", false, vec![acct.invoice("1", T0, E1)]);
        pass("r5").await;
        assert_eq!(stored(&fx.db, &acct.user).await.unwrap(), failing);
        assert_eq!(history(&fx.db, &acct.user).await, rows);

        // Recovery with new money clears the grace and advances.
        *t.borrow_mut() = acct.remote(
            "active",
            false,
            vec![
                acct.invoice("1", T0, E1),
                acct.invoice("2", E1, E2),
                acct.invoice("3", E2, E3),
            ],
        );
        pass("r6").await;
        let recovered = stored(&fx.db, &acct.user).await.unwrap();
        assert_eq!(recovered.status, "active");
        assert_eq!(recovered.grace, None);
        assert_eq!(recovered.paid_through, Some(E3));
        assert_eq!(recovered.anchor, Some(acct.inv_id("3")));
        assert_eq!(history(&fx.db, &acct.user).await.len(), 3);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_renewals_converge_with_one_history_row_per_invoice() {
    run(60, async {
        let fx = Fixture::new().await;
        let acct = fx.account("race", true).await;
        acct.seed_active(&fx).await;
        let log = new_log();
        let t = truth(acct.renewed());
        let first = Rc::new(
            Fake::new("A", &log, &t)
                .observe(&fx, &acct)
                .with_hold(Stage::Retrieve),
        );
        let second = Rc::new(Fake::new("B", &log, &t).observe(&fx, &acct));
        let (a, b) = gated_pair(&fx, &acct, &first, &second, || {}).await;
        assert_eq!(first.seen().unwrap().paid_through, Some(E1));
        let committed = second.seen().unwrap();
        assert_eq!(committed.paid_through, Some(E2));
        assert_eq!(committed.anchor, Some(acct.inv_id("2")));
        assert_eq!(a.history.len(), 2);
        assert_eq!(b.history.len(), 2);
        let last = stored(&fx.db, &acct.user).await.unwrap();
        assert_eq!(last, committed);
        assert_eq!(history(&fx.db, &acct.user).await.len(), 2);
    })
    .await;
}

// --- Lock spans complete invoice lines ------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn lock_is_held_through_complete_invoice_line_pagination() {
    run(60, async {
        let fx = Fixture::new().await;
        let acct = fx.account("lines", true).await;
        acct.seed_active(&fx).await;
        let log = new_log();
        // Invoice 2's first page carries only a one-off line; the service
        // line that earns coverage arrives on the continuation page.
        let id = acct.inv_id("2");
        let mut remote = acct.remote(
            "active",
            false,
            vec![
                acct.invoice("1", T0, E1),
                acct.invoice_lines(
                    "2",
                    E1,
                    vec![line(&format!("{id}-oneoff"), None, 500, E1, E2)],
                    true,
                ),
            ],
        );
        remote.more_lines.insert(
            id.clone(),
            vec![InvoiceLines {
                data: vec![line(&format!("{id}-plus"), Some(PRICE_PLUS), 800, E1, E2)],
                has_more: false,
            }],
        );
        let t = truth(remote);
        let first = Rc::new(
            Fake::new("A", &log, &t)
                .observe(&fx, &acct)
                .with_hold(Stage::Lines),
        );
        let second = Rc::new(Fake::new("B", &log, &t).observe(&fx, &acct));
        let (a, b) = gated_pair(&fx, &acct, &first, &second, || {
            assert_eq!(
                *log.borrow(),
                ["A:Retrieve", "A:List", "A:Lines"],
                "first pass was mid-pagination when the second queued"
            );
        })
        .await;
        assert_eq!(a.row.paid_through_unix, Some(E2));
        assert_eq!(a.row.paid_invoice_id, Some(id.clone()));
        assert_eq!(second.seen().unwrap().paid_through, Some(E2));
        assert_eq!(b.row.paid_through_unix, Some(E2));
        assert_eq!(stored(&fx.db, &acct.user).await.unwrap().anchor, Some(id));
    })
    .await;
}

// --- Errors, cancellation, bounded wait -----------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn provider_and_store_errors_roll_back_and_release_the_lock() {
    run(120, async {
        let fx = Fixture::new().await;
        let log = new_log();
        for (i, name) in [
            "retrieve fails",
            "listing fails",
            "incomplete pagination",
            "continuation fails",
            "write-time constraint violation",
        ]
        .into_iter()
        .enumerate()
        {
            let acct = fx.account(&format!("err{i}"), true).await;
            acct.seed_active(&fx).await;
            let before = stored(&fx.db, &acct.user).await.unwrap();
            let mut remote = acct.renewed();
            let mut fail = None;
            match i {
                0 => fail = Some(Stage::Retrieve),
                1 => fail = Some(Stage::List),
                2 | 3 => {
                    // Invoice 2 claims a continuation page that never
                    // arrives (empty) or errors: not an exhausted invoice.
                    let id = acct.inv_id("2");
                    remote.invoices[1] = acct.invoice_lines(
                        "2",
                        E1,
                        vec![line(&format!("{id}-oneoff"), None, 500, E1, E2)],
                        true,
                    );
                    if i == 3 {
                        fail = Some(Stage::Lines);
                    }
                }
                _ => {
                    // The subscription upsert and invoice 1's history insert
                    // succeed inside the transaction, then invoice 2's
                    // history row violates amount_cents >= 0.
                    remote.invoices[1].amount_paid = -1;
                }
            }
            let mut fake = Fake::new("bad", &log, &truth(remote));
            if let Some(stage) = fail {
                fake = fake.failing(stage);
            }
            assert!(solo(&fx, &acct, &Rc::new(fake)).await.is_err(), "{name}");
            wait_unlocked(&fx.db, &acct.user, "lock released after error").await;
            assert_eq!(stored(&fx.db, &acct.user).await.unwrap(), before, "{name}");
            assert!(history(&fx.db, &acct.user).await.is_empty(), "{name}");

            // The same user reacquires the lock and reconciles normally.
            let good = Rc::new(Fake::new("good", &log, &truth(acct.renewed())));
            solo(&fx, &acct, &good).await.expect(name);
            let after = stored(&fx.db, &acct.user).await.unwrap();
            assert_eq!(after.paid_through, Some(E2), "{name}");
            assert_eq!(history(&fx.db, &acct.user).await.len(), 2, "{name}");
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn commit_failure_never_returns_success_or_partial_history() {
    run(60, async {
        let fx = Fixture::new().await;
        let acct = fx.account("commitfail", true).await;
        acct.seed_active(&fx).await;
        let before = stored(&fx.db, &acct.user).await.unwrap();
        fx.db.batch_execute(
            "CREATE FUNCTION reject_billing_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic deferred failure'; END $$;
             CREATE CONSTRAINT TRIGGER reject_billing_commit AFTER INSERT OR UPDATE ON subscriptions DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_billing_commit()"
        ).await.unwrap();
        let log = new_log();
        let t = truth(acct.renewed());
        let fake = Rc::new(Fake::new("badcommit", &log, &t));
        assert!(solo(&fx, &acct, &fake).await.is_err());
        wait_unlocked(&fx.db, &acct.user, "failed commit releases transaction lock").await;
        assert_eq!(stored(&fx.db, &acct.user).await.unwrap(), before);
        assert!(history(&fx.db, &acct.user).await.is_empty());
        fx.db.batch_execute("DROP TRIGGER reject_billing_commit ON subscriptions").await.unwrap();
        let retry = Rc::new(Fake::new("retrycommit", &log, &t));
        solo(&fx, &acct, &retry).await.expect("retry rereads and commits");
        assert_eq!(history(&fx.db, &acct.user).await.len(), 2);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn lock_wait_and_provider_pagination_share_one_attempt_deadline() {
    run(60, async {
        let fx = Fixture::new().await;
        let acct = fx.account("sharedbudget", true).await;
        acct.seed_active(&fx).await;
        let before = stored(&fx.db, &acct.user).await.unwrap();
        let blocker = fx.client().await;
        blocker.batch_execute("BEGIN").await.unwrap();
        blocker
            .query("SELECT pg_advisory_xact_lock(hashtext($1))", &[&acct.user])
            .await
            .unwrap();
        let mut remote = acct.renewed();
        remote.invoices[1].lines.as_mut().unwrap().has_more = true;
        let log = new_log();
        let t = truth(remote);
        let fake = Rc::new(Fake::new("budget", &log, &t).with_hold(Stage::Lines));
        let started = Instant::now();
        let pass = start(&fx, &acct, &fake).await;
        wait_locks(&fx.db, &acct.user, "unit is waiting for lock", |l| {
            l.contains(&(pass.pid, false))
        })
        .await;
        // Deliberate budget expenditure, not a correctness synchronization:
        // server lock evidence above establishes exactly where the unit is.
        tokio::time::sleep(Duration::from_secs(8)).await;
        blocker.batch_execute("COMMIT").await.unwrap();
        fake.hold_entered()
            .reached("unit reached continuation page")
            .await;
        assert!(pass.done().await.is_err());
        assert!(
            started.elapsed() < Duration::from_secs(18),
            "lock/page deadline was reset"
        );
        wait_unlocked(&fx.db, &acct.user, "timed-out page releases lock").await;
        assert_eq!(stored(&fx.db, &acct.user).await.unwrap(), before);
        assert!(history(&fx.db, &acct.user).await.is_empty());
        *t.borrow_mut() = acct.renewed();
        let retry = Rc::new(Fake::new("budgetretry", &log, &t));
        solo(&fx, &acct, &retry)
            .await
            .expect("retry after whole-attempt timeout");
        assert_eq!(history(&fx.db, &acct.user).await.len(), 2);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_future_rolls_back_and_the_lock_is_reacquirable() {
    run(60, async {
        let fx = Fixture::new().await;
        let acct = fx.account("dropped", true).await;
        acct.seed_active(&fx).await;
        let before = stored(&fx.db, &acct.user).await.unwrap();
        let log = new_log();
        let t = truth(acct.renewed());
        let parked = Rc::new(Fake::new("A", &log, &t).with_hold(Stage::Retrieve));
        let a = start(&fx, &acct, &parked).await;
        parked.hold_entered().reached("pass parked").await;
        assert_parked(&fx, &acct, a.pid).await;
        // The caller goes away (request cancelled / budget fired): the
        // store future is dropped with the transaction still open.
        a.task.abort();
        let joined = tokio::time::timeout(Duration::from_secs(10), a.task)
            .await
            .expect("aborted task settles");
        assert!(joined.expect_err("pass was cancelled").is_cancelled());
        wait_unlocked(&fx.db, &acct.user, "dropped pass released the lock").await;
        assert_eq!(stored(&fx.db, &acct.user).await.unwrap(), before);
        assert!(history(&fx.db, &acct.user).await.is_empty());
        let good = Rc::new(Fake::new("B", &log, &t));
        solo(&fx, &acct, &good)
            .await
            .expect("reacquired after drop");
        assert_eq!(
            stored(&fx.db, &acct.user).await.unwrap().paid_through,
            Some(E2)
        );
        assert_eq!(history(&fx.db, &acct.user).await.len(), 2);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn lock_wait_is_bounded_and_never_reaches_the_provider() {
    // Spends the production 15 second budget once, by design.
    run(90, async {
        let fx = Fixture::new().await;
        let acct = fx.account("bounded", true).await;
        acct.seed_active(&fx).await;
        let before = stored(&fx.db, &acct.user).await.unwrap();
        let blocker = fx.client().await;
        blocker.batch_execute("BEGIN").await.unwrap();
        blocker
            .query("SELECT pg_advisory_xact_lock(hashtext($1))", &[&acct.user])
            .await
            .unwrap();
        let log = new_log();
        let t = truth(acct.renewed());
        let waiter = Rc::new(Fake::new("A", &log, &t));
        let started = Instant::now();
        let pass = start(&fx, &acct, &waiter).await;
        wait_locks(&fx.db, &acct.user, "waiter queued", |l| {
            l.contains(&(pass.pid, false))
        })
        .await;
        assert!(pass.done().await.is_err(), "bounded wait fails closed");
        assert!(started.elapsed() < Duration::from_secs(24));
        assert_eq!(waiter.calls(), 0, "no provider call without the lock");
        assert_eq!(stored(&fx.db, &acct.user).await.unwrap(), before);
        // The timed-out waiter left no queue entry behind the holder.
        wait_locks(&fx.db, &acct.user, "only the holder remains", |l| {
            l.len() == 1 && l[0].1
        })
        .await;
        blocker.batch_execute("COMMIT").await.unwrap();
        let good = Rc::new(Fake::new("B", &log, &t));
        solo(&fx, &acct, &good).await.expect("lock reacquired");
        assert_eq!(
            stored(&fx.db, &acct.user).await.unwrap().paid_through,
            Some(E2)
        );
    })
    .await;
}
