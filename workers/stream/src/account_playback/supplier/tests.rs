//! Deterministic supplier tests: generated fixtures, fake catalogue/registry/
//! account sources and injected signals. Futures are polled by hand; nothing
//! here touches a network, database, container or real timer.

use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    future::pending,
    pin::Pin,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Wake, Waker},
};

use nntune_core::pool_channel::{Channel, CANDIDATE_PATH, OPERATION_PATH, REGISTRY_PATH};
use serde_json::{json, Value};

use crate::{
    account_operation::{OperationError, Served},
    account_playback::CatalogueUnavailable,
    account_pool::AccountConfig,
    account_route::REGISTRY_BODY,
    grant,
};

const NOW: u64 = 1_700_000_000_125;
const GRANT_EXP: u64 = NOW / 1000 + 60;
const GRANT_EXP_MS: u64 = GRANT_EXP * 1000;
const TRACK: &str = "00000001-1234-4234-8234-123456789abc";
const OTHER_TRACK: &str = "00000002-1234-4234-8234-123456789abc";
const GRANT_KEY: &[u8] = &[7; 32];
const ACCOUNT_KEY: &[u8] = &[5; 32];
const RESOLVER_KEY: &[u8] = &[9; 32];
const MAX_BODY: usize = 16 << 20;

// ------------------------------------------------------------- signals

#[derive(Default)]
struct SignalState {
    ready: Cell<bool>,
    dropped: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}
impl SignalState {
    fn fire(&self) {
        self.ready.set(true);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}
struct TestSignal(Rc<SignalState>);
impl Future for TestSignal {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        *self.0.waker.borrow_mut() = Some(cx.waker().clone());
        if self.0.ready.get() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}
impl Drop for TestSignal {
    fn drop(&mut self) {
        self.0.dropped.set(true);
    }
}
fn signal<'a>(state: &Rc<SignalState>) -> Signal<'a> {
    Box::pin(TestSignal(state.clone()))
}

#[derive(Default)]
struct Wakes(AtomicUsize);
impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// Sets a flag when the owning future is dropped (cancellation witness).
struct DropFlag(Rc<Cell<bool>>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

fn poll<T>(future: &mut Pin<Box<impl Future<Output = T>>>, waker: &Waker) -> Poll<T> {
    future.as_mut().poll(&mut Context::from_waker(waker))
}

/// Completes a future that must not need a second poll.
fn ready<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("future unexpectedly pending"),
    }
}

// ------------------------------------------------------------ fixtures

fn uid(n: u32) -> String {
    format!("{n:08x}-1234-4234-8234-123456789abc")
}

fn reference(i: usize) -> String {
    format!("part{i}@example.invalid")
}

fn source(track: &str, articles: usize) -> OriginalSource {
    let rows: Vec<Value> = (0..articles)
        .map(|i| json!({"number": i + 1, "reference": reference(i), "encoded_bytes": 100}))
        .collect();
    let bytes = serde_json::to_vec(&json!({
        "track_id": track,
        "source_job_id": "job-1",
        "file_ordinal": 0,
        "file_size": 100 * articles as u64,
        "article_count": articles,
        "articles": rows,
    }))
    .unwrap();
    OriginalSource::parse(1, &bytes, track, "job-1").unwrap()
}

fn registry_json(rows: &[(u32, u32)]) -> Vec<u8> {
    let accounts: Vec<AccountConfig> = rows
        .iter()
        .map(|&(n, tier)| AccountConfig {
            id: uid(n),
            revision: 1,
            secret_ref: uid(0x1000 + n),
            can_read: true,
            max_connections: 4,
            stream_reserve: 1,
            ingest_limit: 2,
            tier,
            enabled: true,
        })
        .collect();
    serde_json::to_vec(&json!({ "accounts": accounts })).unwrap()
}

fn hint_json(n: u32, tier: u32) -> Vec<u8> {
    serde_json::to_vec(&json!({"id": uid(n), "tier": tier, "in_flight": 0, "eligible": true}))
        .unwrap()
}

fn keys() -> ChannelKeys<'static> {
    ChannelKeys {
        account: Some(ACCOUNT_KEY),
        resolver: Some(RESOLVER_KEY),
    }
}

// --------------------------------------------------------------- fakes

type Hook = RefCell<Option<Box<dyn Fn(usize)>>>;

fn set_hook(hook: &Hook, f: impl Fn(usize) + 'static) {
    *hook.borrow_mut() = Some(Box::new(f));
}

fn run_hook(hook: &Hook, call: usize) {
    if let Some(f) = hook.borrow().as_ref() {
        f(call);
    }
}

#[derive(Clone)]
enum CatMode {
    Source(OriginalSource),
    Missing,
    Error,
    Pending,
}

struct FakeCatalogue {
    mode: RefCell<CatMode>,
    tracks: RefCell<Vec<String>>,
    dropped: Rc<Cell<bool>>,
}

impl OriginalCatalogue for FakeCatalogue {
    async fn source(&self, track_id: &str) -> Result<Option<OriginalSource>, CatalogueUnavailable> {
        self.tracks.borrow_mut().push(track_id.to_owned());
        let mode = self.mode.borrow().clone();
        match mode {
            CatMode::Source(source) => Ok(Some(source)),
            CatMode::Missing => Ok(None),
            CatMode::Error => Err(CatalogueUnavailable),
            CatMode::Pending => {
                let _flag = DropFlag(self.dropped.clone());
                pending::<Result<Option<OriginalSource>, CatalogueUnavailable>>().await
            }
        }
    }
}

struct FakeRegistry {
    bytes: RefCell<Option<Vec<u8>>>,
    pending: Cell<bool>,
    hook: Hook,
    auths: RefCell<Vec<String>>,
    dropped: Rc<Cell<bool>>,
}

impl RegistrySource for FakeRegistry {
    async fn fetch(&self, authorization: &str) -> Result<Vec<u8>, RegistryError> {
        self.auths.borrow_mut().push(authorization.to_owned());
        let call = self.auths.borrow().len();
        run_hook(&self.hook, call);
        if self.pending.get() {
            let _flag = DropFlag(self.dropped.clone());
            return pending::<Result<Vec<u8>, RegistryError>>().await;
        }
        self.bytes.borrow().clone().ok_or(RegistryError)
    }
}

type Call = (String, String, Vec<u8>);
type ReplyFn = Box<dyn Fn(&str, &[u8]) -> OperationReply>;

struct FakeAccounts {
    hints: RefCell<HashMap<String, Vec<u8>>>,
    candidate_pending: Cell<bool>,
    candidate_hook: Hook,
    candidate_calls: RefCell<Vec<Call>>,
    candidate_dropped: Rc<Cell<bool>>,
    execute_pending: Cell<bool>,
    execute_hook: Hook,
    execute_calls: RefCell<Vec<Call>>,
    execute_dropped: Rc<Cell<bool>>,
    reply: RefCell<ReplyFn>,
}

impl AccountSource for FakeAccounts {
    async fn candidate(
        &self,
        account_id: &str,
        authorization: &str,
        body: &[u8],
    ) -> Result<Vec<u8>, RegistryError> {
        self.candidate_calls.borrow_mut().push((
            account_id.to_owned(),
            authorization.to_owned(),
            body.to_vec(),
        ));
        let call = self.candidate_calls.borrow().len();
        run_hook(&self.candidate_hook, call);
        if self.candidate_pending.get() {
            let _flag = DropFlag(self.candidate_dropped.clone());
            return pending::<Result<Vec<u8>, RegistryError>>().await;
        }
        self.hints
            .borrow()
            .get(account_id)
            .cloned()
            .ok_or(RegistryError)
    }

    async fn execute(&self, account_id: &str, authorization: &str, body: &[u8]) -> OperationReply {
        self.execute_calls.borrow_mut().push((
            account_id.to_owned(),
            authorization.to_owned(),
            body.to_vec(),
        ));
        let call = self.execute_calls.borrow().len();
        run_hook(&self.execute_hook, call);
        if self.execute_pending.get() {
            let _flag = DropFlag(self.execute_dropped.clone());
            return pending::<OperationReply>().await;
        }
        let reply = self.reply.borrow();
        (**reply)(account_id, body)
    }
}

/// Default reply echoes the signed request body as the encoded article, so a
/// returned payload identifies exactly which request produced it.
fn echo(account_id: &str, body: &[u8]) -> OperationReply {
    OperationReply::Served(Served {
        provider_id: account_id.to_owned(),
        output: Output::Body(body.to_vec()),
    })
}

struct Rig {
    now: Rc<Cell<u64>>,
    abort: Rc<SignalState>,
    req_timer: Rc<SignalState>,
    op_timer: Rc<SignalState>,
    req_timer_ms: Cell<u64>,
    op_timer_ms: RefCell<Vec<u64>>,
    catalogue: FakeCatalogue,
    registry: FakeRegistry,
    accounts: FakeAccounts,
}

impl Rig {
    fn new(articles: usize) -> Self {
        Self::with_rows(articles, &[(1, 0), (2, 1)])
    }

    fn with_rows(articles: usize, rows: &[(u32, u32)]) -> Self {
        let hints = rows
            .iter()
            .map(|&(n, tier)| (uid(n), hint_json(n, tier)))
            .collect();
        Self {
            now: Rc::new(Cell::new(NOW)),
            abort: Rc::default(),
            req_timer: Rc::default(),
            op_timer: Rc::default(),
            req_timer_ms: Cell::new(0),
            op_timer_ms: RefCell::new(Vec::new()),
            catalogue: FakeCatalogue {
                mode: RefCell::new(CatMode::Source(source(TRACK, articles))),
                tracks: RefCell::new(Vec::new()),
                dropped: Rc::default(),
            },
            registry: FakeRegistry {
                bytes: RefCell::new(Some(registry_json(rows))),
                pending: Cell::new(false),
                hook: RefCell::new(None),
                auths: RefCell::new(Vec::new()),
                dropped: Rc::default(),
            },
            accounts: FakeAccounts {
                hints: RefCell::new(hints),
                candidate_pending: Cell::new(false),
                candidate_hook: RefCell::new(None),
                candidate_calls: RefCell::new(Vec::new()),
                candidate_dropped: Rc::default(),
                execute_pending: Cell::new(false),
                execute_hook: RefCell::new(None),
                execute_calls: RefCell::new(Vec::new()),
                execute_dropped: Rc::default(),
                reply: RefCell::new(Box::new(echo)),
            },
        }
    }

    fn control<'a>(
        &self,
        clock: &'a dyn Fn() -> u64,
        request_deadline_ms: u64,
        grant_exp: u64,
    ) -> RequestControl<'a> {
        let token = grant::issue_v2("user", TRACK, "play", 1, grant_exp, GRANT_KEY);
        RequestControl::new(
            &token,
            TRACK,
            GRANT_KEY,
            clock,
            request_deadline_ms,
            signal(&self.abort),
            |ms| {
                self.req_timer_ms.set(ms);
                signal(&self.req_timer)
            },
        )
        .unwrap()
    }

    fn lookups(&self) -> usize {
        self.catalogue.tracks.borrow().len()
    }
    fn registry_calls(&self) -> usize {
        self.registry.auths.borrow().len()
    }
    fn hint_calls(&self) -> usize {
        self.accounts.candidate_calls.borrow().len()
    }
    fn body_calls(&self) -> usize {
        self.accounts.execute_calls.borrow().len()
    }
    /// (lookups, registry, hints, bodies, operation timers)
    fn io(&self) -> (usize, usize, usize, usize, usize) {
        (
            self.lookups(),
            self.registry_calls(),
            self.hint_calls(),
            self.body_calls(),
            self.op_timer_ms.borrow().len(),
        )
    }
}

macro_rules! harness {
    ($rig:ident, $clock:ident, $timer:ident) => {
        let $clock = || $rig.now.get();
        let $timer = |ms: u64| {
            $rig.op_timer_ms.borrow_mut().push(ms);
            signal(&$rig.op_timer)
        };
    };
}

macro_rules! prepare {
    ($rig:ident, $control:expr, $timer:ident) => {
        ArticleSupplier::prepare(
            &$rig.catalogue,
            &$rig.registry,
            &$rig.accounts,
            keys(),
            $control,
            &$timer,
        )
    };
}

macro_rules! setup {
    ($rig:ident, $clock:ident, $timer:ident, $control:ident, $observer:ident, $supplier:ident) => {
        setup!(
            $rig,
            $clock,
            $timer,
            $control,
            $observer,
            $supplier,
            NOW + 120_000,
            GRANT_EXP
        );
    };
    ($rig:ident, $clock:ident, $timer:ident, $control:ident, $observer:ident, $supplier:ident,
     $deadline:expr, $exp:expr) => {
        harness!($rig, $clock, $timer);
        let $control = $rig.control(&$clock, $deadline, $exp);
        #[allow(unused_variables)]
        let $observer = $control.clone();
        #[allow(unused_mut)]
        let mut $supplier = ready(prepare!($rig, $control, $timer)).unwrap();
    };
}

fn poisoned(control: &RequestControl<'_>) -> bool {
    control.clone().check_live() == Err(SourceError::Cancelled)
}
fn live(control: &RequestControl<'_>) -> bool {
    control.clone().check_live() == Ok(())
}

// ------------------------------------------------------------- prepare

#[test]
fn prepare_with_revoked_control_does_no_catalogue_io() {
    for mode in 0..3 {
        let rig = Rig::new(2);
        harness!(rig, clock, timer);
        match mode {
            0 => {}
            1 => rig.abort.fire(),
            _ => rig.req_timer.fire(),
        }
        let control = rig.control(&clock, NOW + 120_000, GRANT_EXP);
        let observer = control.clone();
        if mode == 0 {
            control.stop();
        }
        let result = ready(prepare!(rig, control, timer));
        assert_eq!(result.err(), Some(SourceError::Cancelled), "mode {mode}");
        assert_eq!(rig.io(), (0, 0, 0, 0, 0), "mode {mode}");
        assert!(poisoned(&observer));
    }
}

#[test]
fn prepare_without_usable_keys_does_no_lookup_and_poisons() {
    let short_account = &ACCOUNT_KEY[..31];
    let short_resolver = &RESOLVER_KEY[..31];
    let cases = [
        (None, Some(RESOLVER_KEY)),
        (Some(short_account), Some(RESOLVER_KEY)),
        (Some(ACCOUNT_KEY), None),
        (Some(ACCOUNT_KEY), Some(short_resolver)),
    ];
    for (account, resolver) in cases {
        let rig = Rig::new(2);
        harness!(rig, clock, timer);
        let control = rig.control(&clock, NOW + 120_000, GRANT_EXP);
        let observer = control.clone();
        let result = ready(ArticleSupplier::prepare(
            &rig.catalogue,
            &rig.registry,
            &rig.accounts,
            ChannelKeys { account, resolver },
            control,
            &timer,
        ));
        assert_eq!(result.err(), Some(SourceError::Transport));
        assert_eq!(rig.io(), (0, 0, 0, 0, 0));
        assert!(poisoned(&observer));
    }
}

#[test]
fn missing_failing_or_wrong_track_source_poisons_every_clone() {
    let cases = [
        (CatMode::Missing, SourceError::Transport),
        (CatMode::Error, SourceError::Transport),
        (
            CatMode::Source(source(OTHER_TRACK, 2)),
            SourceError::Binding,
        ),
    ];
    for (mode, expected) in cases {
        let rig = Rig::new(2);
        *rig.catalogue.mode.borrow_mut() = mode;
        harness!(rig, clock, timer);
        let control = rig.control(&clock, NOW + 120_000, GRANT_EXP);
        let observer = control.clone();
        let result = ready(prepare!(rig, control, timer));
        assert_eq!(result.err(), Some(expected));
        assert_eq!(rig.io(), (1, 0, 0, 0, 0));
        assert!(poisoned(&observer));
    }
}

#[test]
fn prepare_does_one_track_bound_lookup_and_no_account_io() {
    let rig = Rig::new(3);
    setup!(rig, clock, timer, control, observer, supplier);
    assert_eq!(*rig.catalogue.tracks.borrow(), vec![TRACK.to_owned()]);
    assert_eq!(supplier.source().track_id(), TRACK);
    assert_eq!(supplier.source().article_count(), 3);
    assert!(supplier.serving_accounts().is_empty());
    assert_eq!(rig.io(), (1, 0, 0, 0, 0));
    assert!(live(&observer));
}

#[test]
fn unpolled_prepare_and_open_do_no_io_and_do_not_poison() {
    let rig = Rig::new(2);
    harness!(rig, clock, timer);
    let control = rig.control(&clock, NOW + 120_000, GRANT_EXP);
    let observer = control.clone();
    drop(prepare!(rig, control.clone(), timer));
    assert_eq!(rig.io(), (0, 0, 0, 0, 0));
    assert!(live(&observer));

    let mut supplier = ready(prepare!(rig, control, timer)).unwrap();
    let first = reference(0);
    drop(supplier.open(0, &first));
    assert_eq!(rig.io(), (1, 0, 0, 0, 0));
    assert!(live(&observer));
    // The dropped, unpolled call did not consume the article.
    let body = ready(supplier.open(0, &first)).unwrap();
    assert!(!body.is_empty());
    assert_eq!(rig.body_calls(), 1);
}

// ---------------------------------------------------------------- open

fn request_value(account: &str, reference: &str) -> Value {
    json!({
        "account_id": account,
        "class": "stream",
        "operation": {"kind": "body", "ids": reference},
    })
}

#[test]
fn final_first_opens_do_one_lookup_and_signed_exact_stream_bodies() {
    let rig = Rig::new(3);
    setup!(rig, clock, timer, control, observer, supplier);
    let order = [2usize, 0, 1];
    for index in order {
        let bytes = ready(supplier.open(index, &reference(index))).unwrap();
        let calls = rig.accounts.execute_calls.borrow();
        let (account, _, body) = calls.last().unwrap();
        assert_eq!(account, &uid(1));
        assert_eq!(bytes, *body);
        let value: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(value, request_value(&uid(1), &reference(index)));
    }
    let refs: Vec<_> = rig
        .accounts
        .execute_calls
        .borrow()
        .iter()
        .map(|(_, _, body)| {
            serde_json::from_slice::<Value>(body).unwrap()["operation"]["ids"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(refs, [reference(2), reference(0), reference(1)]);
    // One lookup in total; one registry read and one hint per account per open.
    assert_eq!(rig.lookups(), 1);
    assert_eq!(rig.registry_calls(), 3);
    assert_eq!(rig.hint_calls(), 6);
    assert_eq!(rig.body_calls(), 3);
    // Telemetry: one actual account per successful article (including proof).
    assert_eq!(supplier.serving_accounts(), [uid(1), uid(1), uid(1)]);
    assert!(live(&observer));
    // Per-open timer is cleaned up with its operation.
    assert!(rig.op_timer.dropped.get());
}

#[test]
fn signatures_bind_channel_path_body_and_selected_account() {
    for (rows, selected) in [([(1, 0), (2, 1)], 1), ([(1, 5), (2, 0)], 2)] {
        let rig = Rig::with_rows(2, &rows);
        setup!(rig, clock, timer, control, observer, supplier);
        ready(supplier.open(1, &reference(1))).unwrap();
        ready(supplier.open(0, &reference(0))).unwrap();
        let now_s = NOW / 1000;

        for auth in rig.registry.auths.borrow().iter() {
            assert!(Channel::Resolver
                .verify(
                    Some(RESOLVER_KEY),
                    REGISTRY_PATH,
                    Some(auth.as_str()),
                    REGISTRY_BODY,
                    now_s
                )
                .is_ok());
            assert!(Channel::Account
                .verify(
                    Some(ACCOUNT_KEY),
                    REGISTRY_PATH,
                    Some(auth.as_str()),
                    REGISTRY_BODY,
                    now_s
                )
                .is_err());
        }
        let mut hinted = Vec::new();
        for (account, auth, body) in rig.accounts.candidate_calls.borrow().iter() {
            assert!(Channel::Account
                .verify(
                    Some(ACCOUNT_KEY),
                    CANDIDATE_PATH,
                    Some(auth.as_str()),
                    body,
                    now_s
                )
                .is_ok());
            let value: Value = serde_json::from_slice(body).unwrap();
            assert_eq!(value, json!({"account_id": account, "class": "stream"}));
            hinted.push(account.clone());
        }
        hinted.sort();
        hinted.dedup();
        assert_eq!(hinted, [uid(1), uid(2)]);

        let calls = rig.accounts.execute_calls.borrow();
        assert_eq!(calls.len(), 2);
        for (i, (account, auth, body)) in calls.iter().enumerate() {
            let index = 1 - i;
            assert_eq!(account, &uid(selected));
            let value: Value = serde_json::from_slice(body).unwrap();
            assert_eq!(value, request_value(&uid(selected), &reference(index)));
            assert!(Channel::Account
                .verify(
                    Some(ACCOUNT_KEY),
                    OPERATION_PATH,
                    Some(auth.as_str()),
                    body,
                    now_s
                )
                .is_ok());
            assert!(Channel::Account
                .verify(
                    Some(RESOLVER_KEY),
                    OPERATION_PATH,
                    Some(auth.as_str()),
                    body,
                    now_s
                )
                .is_err());
            assert!(Channel::Account
                .verify(
                    Some(ACCOUNT_KEY),
                    CANDIDATE_PATH,
                    Some(auth.as_str()),
                    body,
                    now_s
                )
                .is_err());
            // The signature of one article/account request cannot carry another.
            let other = &calls[1 - i].2;
            assert!(Channel::Account
                .verify(
                    Some(ACCOUNT_KEY),
                    OPERATION_PATH,
                    Some(auth.as_str()),
                    other,
                    now_s
                )
                .is_err());
        }
        assert!(live(&observer));
    }
}

#[test]
fn wrong_ordinal_or_reference_opens_no_io_and_poison_is_shared_and_permanent() {
    let wrong_index = [(9usize, reference(9)), (2, reference(2))];
    let mismatched = [
        (0usize, reference(1)),
        (1, reference(0)),
        (0, "other@example.invalid".to_owned()),
        (0, String::new()),
        (1, reference(1).to_uppercase()),
    ];
    for (index, wrong) in wrong_index.iter().chain(&mismatched) {
        let rig = Rig::new(2);
        setup!(rig, clock, timer, control, observer, supplier);
        let before = rig.io();
        let expected = if *index >= 2 {
            SourceError::Bounds
        } else {
            SourceError::Binding
        };
        assert_eq!(ready(supplier.open(*index, wrong)).err(), Some(expected));
        assert_eq!(rig.io(), before, "index {index} ref {wrong:?}");
        assert!(poisoned(&observer));
        // Even a correct request is dead afterwards, with no I/O.
        assert_eq!(
            ready(supplier.open(0, &reference(0))).err(),
            Some(SourceError::Cancelled)
        );
        assert_eq!(rig.io(), before);
        assert!(supplier.serving_accounts().is_empty());
        // Time cannot revive the poisoned authority.
        assert!(poisoned(&observer));
    }
}

#[test]
fn duplicate_open_is_refused_without_io_and_poisons_after_success() {
    let rig = Rig::new(2);
    setup!(rig, clock, timer, control, observer, supplier);
    ready(supplier.open(1, &reference(1))).unwrap();
    let before = rig.io();
    assert_eq!(before, (1, 1, 2, 1, 1));
    assert!(live(&observer));
    assert_eq!(
        ready(supplier.open(1, &reference(1))).err(),
        Some(SourceError::Binding)
    );
    assert_eq!(rig.io(), before);
    assert!(poisoned(&observer));
    assert_eq!(
        ready(supplier.open(0, &reference(0))).err(),
        Some(SourceError::Cancelled)
    );
    assert_eq!(rig.io(), before);
    // The earlier successful article stays recorded.
    assert_eq!(supplier.serving_accounts(), [uid(1)]);
}

#[test]
fn at_most_one_open_per_article() {
    let rig = Rig::new(3);
    setup!(rig, clock, timer, control, observer, supplier);
    for index in 0..3 {
        ready(supplier.open(index, &reference(index))).unwrap();
    }
    for attempt in 0..6 {
        let index = attempt % 3;
        let result = ready(supplier.open(index, &reference(index)));
        assert!(result.is_err());
    }
    assert_eq!(rig.body_calls(), 3);
    assert_eq!(rig.registry_calls(), 3);
    assert_eq!(supplier.serving_accounts().len(), 3);
}

fn bad_reply(case: usize, account: &str, body: &[u8]) -> OperationReply {
    let served = |provider: &str, output| {
        OperationReply::Served(Served {
            provider_id: provider.to_owned(),
            output,
        })
    };
    match case {
        0 => OperationReply::Failed(OperationError::Unavailable),
        1 => OperationReply::Failed(OperationError::Invalid),
        2 => OperationReply::Failed(OperationError::Deadline),
        3 => OperationReply::Failed(OperationError::Missing),
        4 => OperationReply::Failed(OperationError::TooLarge),
        5 => OperationReply::NotFound,
        6 => OperationReply::BadRequest,
        // Served by an account other than the one that was admitted.
        7 => served(&uid(2), Output::Body(body.to_vec())),
        8 => served("not-an-account", Output::Body(body.to_vec())),
        // Wrong output kind.
        9 => served(
            account,
            Output::Stat {
                present: vec![],
                missing: vec![],
            },
        ),
        // One byte over the 16 MiB body bound.
        10 => served(account, Output::Body(vec![1; MAX_BODY + 1])),
        // Empty body.
        11 => served(account, Output::Body(Vec::new())),
        _ => unreachable!(),
    }
}

#[test]
fn bad_oversize_empty_or_misattributed_replies_fail_once_without_retry() {
    for case in 0..12 {
        let rig = Rig::new(2);
        *rig.accounts.reply.borrow_mut() = Box::new(move |id, body| bad_reply(case, id, body));
        setup!(rig, clock, timer, control, observer, supplier);
        assert_eq!(
            ready(supplier.open(0, &reference(0))).err(),
            Some(SourceError::Transport),
            "case {case}"
        );
        assert_eq!(rig.body_calls(), 1, "case {case}");
        assert_eq!(rig.registry_calls(), 1, "case {case}");
        assert!(supplier.serving_accounts().is_empty(), "case {case}");
        assert!(poisoned(&observer), "case {case}");
        // No retry, no failover to the other account, no second attempt.
        let before = rig.io();
        assert_eq!(
            ready(supplier.open(0, &reference(0))).err(),
            Some(SourceError::Cancelled)
        );
        assert_eq!(
            ready(supplier.open(1, &reference(1))).err(),
            Some(SourceError::Cancelled)
        );
        assert_eq!(rig.io(), before, "case {case}");
    }
}

#[test]
fn body_at_exactly_the_bound_is_accepted() {
    let rig = Rig::new(1);
    *rig.accounts.reply.borrow_mut() = Box::new(|id, _| {
        OperationReply::Served(Served {
            provider_id: id.to_owned(),
            output: Output::Body(vec![3; MAX_BODY]),
        })
    });
    setup!(rig, clock, timer, control, observer, supplier);
    let bytes = ready(supplier.open(0, &reference(0))).unwrap();
    assert_eq!(bytes.len(), MAX_BODY);
    assert_eq!(supplier.serving_accounts().len(), 1);
}

#[test]
fn registry_or_hint_failures_never_reach_body_dispatch() {
    for case in 0..7 {
        let rig = Rig::new(2);
        match case {
            0 => *rig.registry.bytes.borrow_mut() = None,
            1 => *rig.registry.bytes.borrow_mut() = Some(b"[]".to_vec()),
            2 => rig.accounts.hints.borrow_mut().clear(),
            3 => {
                for value in rig.accounts.hints.borrow_mut().values_mut() {
                    *value = b"not json".to_vec();
                }
            }
            4 => {
                let ineligible =
                    json!({"id": uid(1), "tier": 0, "in_flight": 0, "eligible": false});
                let other = json!({"id": uid(2), "tier": 1, "in_flight": 0, "eligible": false});
                let mut hints = rig.accounts.hints.borrow_mut();
                hints.insert(uid(1), serde_json::to_vec(&ineligible).unwrap());
                hints.insert(uid(2), serde_json::to_vec(&other).unwrap());
            }
            5 => {
                // Hint tier disagrees with the registry row.
                let mut hints = rig.accounts.hints.borrow_mut();
                hints.insert(uid(1), hint_json(1, 9));
                hints.insert(uid(2), hint_json(2, 9));
            }
            _ => {
                // Hint names a different account than the one asked.
                let mut hints = rig.accounts.hints.borrow_mut();
                hints.insert(uid(1), hint_json(2, 1));
                hints.insert(uid(2), hint_json(1, 0));
            }
        }
        setup!(rig, clock, timer, control, observer, supplier);
        assert_eq!(
            ready(supplier.open(0, &reference(0))).err(),
            Some(SourceError::Transport),
            "case {case}"
        );
        assert_eq!(rig.body_calls(), 0, "case {case}");
        assert!(supplier.serving_accounts().is_empty());
        assert!(poisoned(&observer), "case {case}");
    }
}

#[test]
fn one_unavailable_hint_is_ignored_without_poison_or_retry() {
    let rig = Rig::new(2);
    rig.accounts.hints.borrow_mut().remove(&uid(1));
    setup!(rig, clock, timer, control, observer, supplier);
    ready(supplier.open(0, &reference(0))).unwrap();
    assert_eq!(rig.hint_calls(), 2);
    assert_eq!(rig.body_calls(), 1);
    assert_eq!(rig.accounts.execute_calls.borrow()[0].0, uid(2));
    assert_eq!(supplier.serving_accounts(), [uid(2)]);
    assert!(live(&observer));
}

// --------------------------------------------------------------- timers

#[test]
fn operation_timer_is_independent_starts_on_open_and_takes_the_earliest_cap() {
    // (request deadline, grant expiry, clock advance before open,
    //  expected request-timer ms, expected operation-timer ms)
    let cases = [
        (NOW + 120_000, GRANT_EXP, 0, 59_875, 20_000),
        (NOW + 120_000, GRANT_EXP, 10_000, 59_875, 20_000),
        (NOW + 5_000, GRANT_EXP, 0, 5_000, 5_000),
        (NOW + 120_000, NOW / 1000 + 3, 0, 2_875, 2_875),
        (NOW + 30_000, GRANT_EXP, 15_000, 30_000, 15_000),
    ];
    for (deadline, exp, advance, request_ms, operation_ms) in cases {
        let rig = Rig::new(2);
        setup!(rig, clock, timer, control, observer, supplier, deadline, exp);
        assert_eq!(rig.req_timer_ms.get(), request_ms);
        // Nothing starts at prepare.
        assert!(rig.op_timer_ms.borrow().is_empty());
        rig.now.set(NOW + advance);
        ready(supplier.open(0, &reference(0))).unwrap();
        // One timer per open spanning registry, hints and BODY.
        assert_eq!(*rig.op_timer_ms.borrow(), [operation_ms]);
        ready(supplier.open(1, &reference(1))).unwrap();
        assert_eq!(*rig.op_timer_ms.borrow(), [operation_ms, operation_ms]);
        assert_eq!(rig.registry_calls(), 2);
        assert!(live(&observer));
    }
}

#[test]
fn one_deadline_spans_registry_hints_and_body() {
    // (registry, first hint, body) clock advances, and the expected outcome.
    let cases = [
        ((8_000u64, 0u64, 8_000u64), true),
        ((10_000, 0, 9_999), true),
        ((10_000, 0, 10_000), false),
        ((8_000, 8_000, 8_000), false),
        ((0, 0, 20_000), false),
        ((20_000, 0, 0), false),
    ];
    for ((registry, hint, body), succeeds) in cases {
        let rig = Rig::new(2);
        let advance = |by: u64| {
            let now = rig.now.clone();
            move |_: usize| now.set(now.get() + by)
        };
        set_hook(&rig.registry.hook, advance(registry));
        set_hook(&rig.accounts.candidate_hook, {
            let step = advance(hint);
            move |n| {
                if n == 1 {
                    step(n)
                }
            }
        });
        set_hook(&rig.accounts.execute_hook, advance(body));
        setup!(rig, clock, timer, control, observer, supplier);
        let result = ready(supplier.open(0, &reference(0)));
        assert_eq!(*rig.op_timer_ms.borrow(), [20_000]);
        if succeeds {
            assert!(result.is_ok(), "{registry} {hint} {body}");
            assert_eq!(supplier.serving_accounts().len(), 1);
            assert!(live(&observer));
        } else {
            assert_eq!(result.err(), Some(SourceError::Cancelled));
            assert!(supplier.serving_accounts().is_empty());
            assert!(poisoned(&observer));
        }
    }
}

#[test]
fn ready_operation_timer_wins_before_any_io() {
    let rig = Rig::new(2);
    setup!(rig, clock, timer, control, observer, supplier);
    rig.op_timer.fire();
    assert_eq!(
        ready(supplier.open(0, &reference(0))).err(),
        Some(SourceError::Cancelled)
    );
    assert_eq!(rig.io(), (1, 0, 0, 0, 1));
    assert!(poisoned(&observer));
}

#[derive(Clone, Copy, Debug)]
enum Stage {
    Registry,
    Hint,
    Body,
}

#[derive(Clone, Copy, Debug)]
enum Tie {
    OperationTimer,
    Abort,
    RequestTimer,
    Expiry,
    Rollback,
}

#[test]
fn synchronously_ready_stage_cannot_dispatch_further_after_a_tie() {
    let stages = [Stage::Registry, Stage::Hint, Stage::Body];
    let ties = [
        Tie::OperationTimer,
        Tie::Abort,
        Tie::RequestTimer,
        Tie::Expiry,
        Tie::Rollback,
    ];
    for stage in stages {
        for tie in ties {
            let rig = Rig::new(2);
            let (now, abort, request, operation) = (
                rig.now.clone(),
                rig.abort.clone(),
                rig.req_timer.clone(),
                rig.op_timer.clone(),
            );
            let action: Box<dyn Fn()> = match tie {
                Tie::OperationTimer => Box::new(move || operation.fire()),
                Tie::Abort => Box::new(move || abort.fire()),
                Tie::RequestTimer => Box::new(move || request.fire()),
                Tie::Expiry => Box::new(move || now.set(NOW + SESSION_TTL_MS)),
                Tie::Rollback => Box::new(move || now.set(NOW - 1)),
            };
            match stage {
                Stage::Registry => set_hook(&rig.registry.hook, move |_| action()),
                Stage::Hint => set_hook(&rig.accounts.candidate_hook, move |n| {
                    if n == 1 {
                        action()
                    }
                }),
                Stage::Body => set_hook(&rig.accounts.execute_hook, move |_| action()),
            }
            setup!(rig, clock, timer, control, observer, supplier);
            let result = ready(supplier.open(1, &reference(1)));
            let label = format!("{stage:?} {tie:?}");
            assert_eq!(result.err(), Some(SourceError::Cancelled), "{label}");
            let expected = match stage {
                Stage::Registry => (1, 0, 0),
                Stage::Hint => (1, 1, 0),
                Stage::Body => (1, 2, 1),
            };
            assert_eq!(
                (rig.registry_calls(), rig.hint_calls(), rig.body_calls()),
                expected,
                "{label}"
            );
            // A simultaneous success is discarded and never counted.
            assert!(supplier.serving_accounts().is_empty(), "{label}");
            assert!(poisoned(&observer), "{label}");
        }
    }
}

// ---------------------------------------------- pending / cancellation

#[derive(Clone, Copy, Debug)]
enum Cause {
    Abort,
    RequestTimer,
    OperationTimer,
    Deadline,
    Drop,
}

const CAUSES: [Cause; 5] = [
    Cause::Abort,
    Cause::RequestTimer,
    Cause::OperationTimer,
    Cause::Deadline,
    Cause::Drop,
];

#[test]
fn pending_lookup_is_cancelled_dropped_and_poisons_clones() {
    for cause in CAUSES {
        if matches!(cause, Cause::OperationTimer) {
            continue; // No operation timer exists before an article is opened.
        }
        let rig = Rig::new(2);
        *rig.catalogue.mode.borrow_mut() = CatMode::Pending;
        harness!(rig, clock, timer);
        let control = rig.control(&clock, NOW + 120_000, GRANT_EXP);
        let observer = control.clone();
        let wakes = Arc::new(Wakes::default());
        let waker = Waker::from(wakes.clone());
        let mut future = Box::pin(prepare!(rig, control, timer));
        assert!(poll(&mut future, &waker).is_pending());
        assert_eq!(rig.io(), (1, 0, 0, 0, 0));
        match cause {
            Cause::Drop => drop(future),
            _ => {
                match cause {
                    Cause::Abort => rig.abort.fire(),
                    Cause::RequestTimer => rig.req_timer.fire(),
                    _ => rig.now.set(GRANT_EXP_MS),
                }
                if !matches!(cause, Cause::Deadline) {
                    assert!(wakes.0.load(Ordering::Relaxed) > 0, "{cause:?}");
                }
                match poll(&mut future, &waker) {
                    Poll::Ready(result) => {
                        assert_eq!(result.err(), Some(SourceError::Cancelled), "{cause:?}")
                    }
                    Poll::Pending => panic!("{cause:?} left the lookup pending"),
                }
                drop(future);
            }
        }
        assert!(rig.catalogue.dropped.get(), "{cause:?}");
        assert!(poisoned(&observer), "{cause:?}");
        assert_eq!(rig.io(), (1, 0, 0, 0, 0), "{cause:?}");
    }
}

#[test]
fn pending_registry_hint_or_body_is_cancelled_dropped_and_poisons_clones() {
    for stage in [Stage::Registry, Stage::Hint, Stage::Body] {
        for cause in CAUSES {
            let rig = Rig::new(2);
            let dropped = match stage {
                Stage::Registry => {
                    rig.registry.pending.set(true);
                    rig.registry.dropped.clone()
                }
                Stage::Hint => {
                    rig.accounts.candidate_pending.set(true);
                    rig.accounts.candidate_dropped.clone()
                }
                Stage::Body => {
                    rig.accounts.execute_pending.set(true);
                    rig.accounts.execute_dropped.clone()
                }
            };
            setup!(rig, clock, timer, control, observer, supplier);
            let label = format!("{stage:?} {cause:?}");
            let wakes = Arc::new(Wakes::default());
            let waker = Waker::from(wakes.clone());
            let article = reference(1);
            let mut future = Box::pin(supplier.open(1, &article));
            assert!(poll(&mut future, &waker).is_pending(), "{label}");
            // The pending stage was reached; later stages were not.
            let reached = match stage {
                Stage::Registry => (1, 0, 0),
                Stage::Hint => (1, 2, 0),
                Stage::Body => (1, 2, 1),
            };
            let counts = |rig: &Rig| (rig.registry_calls(), rig.hint_calls(), rig.body_calls());
            assert_eq!(counts(&rig), reached, "{label}");
            assert_eq!(*rig.op_timer_ms.borrow(), [20_000], "{label}");
            match cause {
                Cause::Drop => drop(future),
                _ => {
                    match cause {
                        Cause::Abort => rig.abort.fire(),
                        Cause::RequestTimer => rig.req_timer.fire(),
                        Cause::OperationTimer => rig.op_timer.fire(),
                        _ => rig.now.set(NOW + SESSION_TTL_MS),
                    }
                    if !matches!(cause, Cause::Deadline) {
                        assert!(wakes.0.load(Ordering::Relaxed) > 0, "{label}");
                    }
                    match poll(&mut future, &waker) {
                        Poll::Ready(result) => {
                            assert_eq!(result.err(), Some(SourceError::Cancelled), "{label}")
                        }
                        Poll::Pending => panic!("{label} left the operation pending"),
                    }
                    drop(future);
                }
            }
            assert!(dropped.get(), "{label}");
            assert!(poisoned(&observer), "{label}");
            // Nothing was dispatched or retried during cancellation.
            assert_eq!(counts(&rig), reached, "{label}");
            assert!(supplier.serving_accounts().is_empty(), "{label}");
            assert_eq!(
                ready(supplier.open(0, &reference(0))).err(),
                Some(SourceError::Cancelled),
                "{label}"
            );
            assert_eq!(counts(&rig), reached, "{label}");
        }
    }
}
