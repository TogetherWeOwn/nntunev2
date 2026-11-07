//! Account candidate route: authenticated, read-only scheduling hint.
//!
//! Deterministic native tests over in-memory fakes: a registry source that
//! verifies the resolver HMAC with the shared core protocol, an atomic
//! clone-journal storage, a close control and a hand-driven clock. Every future
//! completes without suspending, so a no-op waker drives them. The fakes are NOT
//! evidence of real Worker, service-binding or Durable Object behaviour.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use nntune_core::pool_channel::{self, Channel};
use nntune_stream::account_owner::AccountOwner;
use nntune_stream::account_pool::{AccountConfig, Candidate, WorkClass};
use nntune_stream::account_route::{
    candidate, parse_registry, CandidateCall, CandidateReply, CandidateRequest, ChannelKeys,
    RegistryError, RegistrySource, MAX_ACCOUNTS, MAX_REGISTRY_BYTES, REGISTRY_BODY,
};
use nntune_stream::account_supervisor::{
    AccountJournal, AccountStorage, CloseControl, CloseError, SessionResult, StorageError,
};
use serde_json::{json, Value};

const ACCOUNT: &str = "12345678-1234-4234-8234-123456789abc";
const OTHER_ACCOUNT: &str = "52345678-1234-4234-8234-123456789abc";
const SECRET_A: &str = "22345678-1234-4234-8234-123456789abc";
const SECRET_B: &str = "32345678-1234-4234-8234-123456789abc";

const ACCOUNT_KEY: &[u8] = b"account-channel-key-0123456789abcdef";
const RESOLVER_KEY: &[u8] = b"resolver-channel-key-0123456789abcde";
const START_MS: u64 = 1_000_000;

// ---------------------------------------------------------------- fakes

struct StoreState {
    journal: Option<AccountJournal>,
    loads: u32,
    saves: u32,
}

#[derive(Clone)]
struct FakeStorage(Rc<RefCell<StoreState>>);

impl FakeStorage {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(StoreState {
            journal: None,
            loads: 0,
            saves: 0,
        })))
    }
    fn touches(&self) -> (u32, u32) {
        let s = self.0.borrow();
        (s.loads, s.saves)
    }
    fn has_journal(&self) -> bool {
        self.0.borrow().journal.is_some()
    }
    fn json(&self) -> Value {
        serde_json::to_value(self.0.borrow().journal.as_ref().expect("journal saved")).unwrap()
    }
}

impl AccountStorage for FakeStorage {
    async fn load(&mut self) -> Result<Option<AccountJournal>, StorageError> {
        let mut s = self.0.borrow_mut();
        s.loads += 1;
        Ok(s.journal.clone())
    }
    async fn save(&mut self, journal: &AccountJournal) -> Result<(), StorageError> {
        let mut s = self.0.borrow_mut();
        s.saves += 1;
        s.journal = Some(journal.clone());
        Ok(())
    }
}

#[derive(Clone)]
struct FakeControl(Rc<Cell<bool>>);

impl CloseControl for FakeControl {
    async fn close(&self) -> Result<(), CloseError> {
        self.0.set(true);
        Ok(())
    }
    fn confirmed_closed(&self) -> bool {
        self.0.get()
    }
}

type Owner = AccountOwner<FakeStorage, FakeControl>;

struct SourceState {
    /// Key the fake service binding verifies the resolver credential with.
    key: Option<Vec<u8>>,
    reply: Result<Vec<u8>, RegistryError>,
    fetches: u32,
    seen_auth: Vec<String>,
    /// Registry latency: the shared clock moves forward after verification.
    latency_ms: u64,
}

#[derive(Clone)]
struct FakeSource {
    state: Rc<RefCell<SourceState>>,
    clock: Rc<Cell<u64>>,
}

impl FakeSource {
    fn set_registry(&self, rows: &[AccountConfig]) {
        self.state.borrow_mut().reply = Ok(registry(rows));
    }
    fn set_raw(&self, raw: &[u8]) {
        self.state.borrow_mut().reply = Ok(raw.to_vec());
    }
    fn fetches(&self) -> u32 {
        self.state.borrow().fetches
    }
}

impl RegistrySource for FakeSource {
    async fn fetch(&self, authorization: &str) -> Result<Vec<u8>, RegistryError> {
        let mut s = self.state.borrow_mut();
        s.fetches += 1;
        s.seen_auth.push(authorization.to_string());
        // Same protocol the API service binding enforces.
        pool_channel::verify(
            s.key.as_deref(),
            pool_channel::REGISTRY_PATH,
            Some(authorization),
            REGISTRY_BODY,
            self.clock.get() / 1000,
        )
        .map_err(|_| RegistryError)?;
        self.clock.set(self.clock.get() + s.latency_ms);
        s.reply.clone()
    }
}

// -------------------------------------------------------------- helpers

fn cfg() -> AccountConfig {
    AccountConfig {
        id: ACCOUNT.into(),
        revision: 1,
        secret_ref: SECRET_A.into(),
        can_read: true,
        max_connections: 3,
        stream_reserve: 1,
        ingest_limit: 2,
        tier: 0,
        enabled: true,
    }
}

fn registry(rows: &[AccountConfig]) -> Vec<u8> {
    serde_json::to_vec(&json!({ "accounts": rows })).unwrap()
}

fn drive<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("future suspended; the fakes never block"),
    }
}

struct Harness {
    store: FakeStorage,
    source: FakeSource,
    owner: Owner,
    clock: Rc<Cell<u64>>,
    clock_reads: Rc<Cell<u32>>,
}

impl Harness {
    fn new() -> Self {
        Self::over(FakeStorage::new())
    }

    /// A fresh owner (a new Durable Object lifetime) over the given storage.
    fn over(store: FakeStorage) -> Self {
        let clock = Rc::new(Cell::new(START_MS));
        let source = FakeSource {
            state: Rc::new(RefCell::new(SourceState {
                key: Some(RESOLVER_KEY.to_vec()),
                reply: Ok(registry(&[cfg()])),
                fetches: 0,
                seen_auth: Vec::new(),
                latency_ms: 0,
            })),
            clock: clock.clone(),
        };
        let owner = Owner::new(ACCOUNT.into(), store.clone()).expect("owner");
        Self {
            store,
            source,
            owner,
            clock,
            clock_reads: Rc::new(Cell::new(0)),
        }
    }

    /// Everything the route could have touched: registry, storage, close.
    fn untouched(&self) -> bool {
        self.source.fetches() == 0 && self.store.touches() == (0, 0) && !self.store.has_journal()
    }
}

#[derive(Clone)]
enum Auth {
    Sign,
    Missing,
    Raw(String),
}

#[derive(Clone)]
struct Req {
    method: &'static str,
    path: &'static str,
    auth: Auth,
    body: Vec<u8>,
    browser: bool,
    account_key: Option<Vec<u8>>,
    resolver_key: Option<Vec<u8>>,
}

impl Req {
    fn with_body(body: &[u8]) -> Self {
        Self {
            method: "POST",
            path: pool_channel::CANDIDATE_PATH,
            auth: Auth::Sign,
            body: body.to_vec(),
            browser: false,
            account_key: Some(ACCOUNT_KEY.to_vec()),
            resolver_key: Some(RESOLVER_KEY.to_vec()),
        }
    }

    fn new(account: &str, class: &str) -> Self {
        Self::with_body(
            serde_json::to_string(&json!({ "account_id": account, "class": class }))
                .unwrap()
                .as_bytes(),
        )
    }

    fn valid() -> Self {
        Self::new(ACCOUNT, "stream")
    }

    fn run(&self, h: &Harness) -> CandidateReply {
        let authorization = match &self.auth {
            Auth::Sign => Channel::Account.authorization_header(
                Some(ACCOUNT_KEY),
                pool_channel::CANDIDATE_PATH,
                h.clock.get() / 1000,
                &self.body,
            ),
            Auth::Missing => None,
            Auth::Raw(raw) => Some(raw.clone()),
        };
        let reads = h.clock_reads.clone();
        let clock = h.clock.clone();
        drive(candidate(
            &h.owner,
            &h.source,
            CandidateCall {
                method: self.method,
                path: self.path,
                authorization: authorization.as_deref(),
                body: &self.body,
                browser_context: self.browser,
            },
            ChannelKeys {
                account: self.account_key.as_deref(),
                resolver: self.resolver_key.as_deref(),
            },
            move || {
                reads.set(reads.get() + 1);
                clock.get()
            },
        ))
    }
}

fn hint(reply: CandidateReply) -> Candidate {
    match reply {
        CandidateReply::Hint(c) => c,
        other => panic!("expected a hint, got {other:?}"),
    }
}

fn reservations(h: &Harness) -> usize {
    h.store.json()["reservations"].as_object().unwrap().len()
}

fn uuid(n: usize) -> String {
    format!("{n:08x}-1234-4234-8234-123456789abc")
}

fn row(n: usize) -> AccountConfig {
    AccountConfig {
        id: uuid(n),
        ..cfg()
    }
}

// ----------------------------------------- rejection before any I/O

#[test]
fn unauthenticated_and_browser_requests_are_not_found_with_no_io() {
    let mut cases: Vec<(&str, Req)> = Vec::new();
    let mut r = Req::valid();
    r.method = "GET";
    cases.push(("GET", r));
    let mut r = Req::valid();
    r.method = "post";
    cases.push(("lowercase method", r));
    let mut r = Req::valid();
    r.path = "/candidate/";
    cases.push(("other path", r));
    let mut r = Req::valid();
    r.path = pool_channel::REGISTRY_PATH;
    cases.push(("registry path", r));
    let mut r = Req::valid();
    r.auth = Auth::Missing;
    cases.push(("no authorization", r));
    let mut r = Req::valid();
    r.auth = Auth::Raw("PoolAccount 1.deadbeef".into());
    cases.push(("bad signature", r));
    let mut r = Req::valid();
    r.auth = Auth::Raw("Bearer abc".into());
    cases.push(("wrong scheme", r));
    let mut r = Req::valid();
    r.browser = true;
    cases.push(("browser context", r));
    let mut r = Req::valid();
    r.account_key = None;
    cases.push(("account key missing", r));
    let mut r = Req::valid();
    r.account_key = Some(ACCOUNT_KEY[..31].to_vec());
    cases.push(("account key too short", r));
    let mut r = Req::valid();
    r.account_key = Some(RESOLVER_KEY.to_vec());
    cases.push(("account key differs from signer", r));
    let mut r = Req::with_body(&[b' '; 513]);
    r.auth = Auth::Raw("PoolAccount 1.00".into());
    cases.push(("oversize body", r));

    for (name, req) in cases {
        let h = Harness::new();
        assert_eq!(req.run(&h), CandidateReply::NotFound, "{name}");
        assert!(h.untouched(), "{name}: registry or storage was touched");
    }
}

#[test]
fn resolver_credential_cannot_authenticate_the_account_channel() {
    let h = Harness::new();
    let req = Req::valid();
    let resolver_header = Channel::Resolver
        .authorization_header(
            Some(ACCOUNT_KEY),
            pool_channel::REGISTRY_PATH,
            h.clock.get() / 1000,
            &req.body,
        )
        .unwrap();
    let mut req = req;
    req.auth = Auth::Raw(resolver_header);
    assert_eq!(req.run(&h), CandidateReply::NotFound);
    assert!(h.untouched());
}

#[test]
fn stale_or_future_timestamps_are_not_found_with_no_io() {
    for skew_secs in [-31i64, 31] {
        let h = Harness::new();
        let mut req = Req::valid();
        let ts = (h.clock.get() as i64 / 1000 + skew_secs) as u64;
        req.auth = Auth::Raw(
            Channel::Account
                .authorization_header(
                    Some(ACCOUNT_KEY),
                    pool_channel::CANDIDATE_PATH,
                    ts,
                    &req.body,
                )
                .unwrap(),
        );
        assert_eq!(req.run(&h), CandidateReply::NotFound, "skew {skew_secs}");
        assert!(h.untouched());
    }
    // The skew boundary itself is accepted.
    let h = Harness::new();
    let mut req = Req::valid();
    req.auth = Auth::Raw(
        Channel::Account
            .authorization_header(
                Some(ACCOUNT_KEY),
                pool_channel::CANDIDATE_PATH,
                h.clock.get() / 1000 - 30,
                &req.body,
            )
            .unwrap(),
    );
    hint(req.run(&h));
}

#[test]
fn authenticated_wrong_owner_unknown_class_and_malformed_bodies_do_no_io() {
    let account_id = format!(r#""account_id":"{ACCOUNT}""#);
    let cases: Vec<(&str, Req)> = vec![
        ("wrong owner", Req::new(OTHER_ACCOUNT, "stream")),
        (
            "uppercase owner",
            Req::new(&ACCOUNT.to_uppercase(), "stream"),
        ),
        ("unknown class", Req::new(ACCOUNT, "playback")),
        ("capitalised class", Req::new(ACCOUNT, "Stream")),
        ("empty body", Req::with_body(b"")),
        ("whitespace body", Req::with_body(b"   ")),
        ("truncated json", Req::with_body(b"{\"account_id\"")),
        ("array body", Req::with_body(b"[]")),
        ("string body", Req::with_body(b"\"x\"")),
        ("null body", Req::with_body(b"null")),
        ("empty object", Req::with_body(b"{}")),
        (
            "missing class",
            Req::with_body(format!("{{{account_id}}}").as_bytes()),
        ),
        ("missing account", Req::with_body(br#"{"class":"stream"}"#)),
        (
            "unknown field",
            Req::with_body(format!(r#"{{{account_id},"class":"stream","extra":1}}"#).as_bytes()),
        ),
        (
            "duplicate field",
            Req::with_body(
                format!(r#"{{{account_id},"class":"stream","class":"ingest"}}"#).as_bytes(),
            ),
        ),
        (
            "tuple array",
            Req::with_body(format!(r#"[{},"stream"]"#, &account_id[13..]).as_bytes()),
        ),
        ("non-utf8", Req::with_body(b"{\"account_id\":\"\xff\"}")),
        (
            "class wrong type",
            Req::with_body(format!(r#"{{{account_id},"class":1}}"#).as_bytes()),
        ),
    ];
    for (name, req) in cases {
        let h = Harness::new();
        assert_eq!(req.run(&h), CandidateReply::BadRequest, "{name}");
        assert!(h.untouched(), "{name}: registry or storage was touched");
    }
}

#[test]
fn leading_whitespace_before_the_object_is_accepted() {
    let h = Harness::new();
    let body = format!(r#"  {{"account_id":"{ACCOUNT}","class":"ingest"}}"#);
    let c = hint(Req::with_body(body.as_bytes()).run(&h));
    assert_eq!(c.id, ACCOUNT);
}

// ------------------------------------------------ happy path / hint only

#[test]
fn valid_request_returns_hint_after_one_signed_registry_read() {
    let h = Harness::new();
    let c = hint(Req::valid().run(&h));
    assert_eq!(
        c,
        Candidate {
            id: ACCOUNT.into(),
            tier: 0,
            in_flight: 0,
            eligible: true
        }
    );
    assert_eq!(h.source.fetches(), 1);
    assert_eq!(h.store.touches().0, 1, "owner opened its storage once");

    // The fake source verified the resolver HMAC for exactly the registry path
    // and body; re-derive the header independently to pin its shape.
    let seen = h.source.state.borrow().seen_auth.clone();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].starts_with("PoolResolver "));
    assert_eq!(
        Some(seen[0].clone()),
        pool_channel::authorization_header(
            Some(RESOLVER_KEY),
            pool_channel::REGISTRY_PATH,
            START_MS / 1000,
            REGISTRY_BODY,
        )
    );

    let c = hint(Req::new(ACCOUNT, "ingest").run(&h));
    assert!(c.eligible);
}

#[test]
fn candidate_is_a_hint_only_and_never_reserves() {
    let h = Harness::new();
    for class in ["stream", "ingest", "stream", "ingest", "stream"] {
        hint(Req::new(ACCOUNT, class).run(&h));
        assert_eq!(reservations(&h), 0, "{class}: no reservation persisted");
    }
    let json = h.store.json();
    assert_eq!(json["next_id"], json!(1), "no session id was allocated");
    assert_eq!(json["quarantined_secret"], Value::Null);
    assert_eq!(json["reservations"], json!({}));
    // Repeated hints do not consume capacity: many more than the cap succeed.
    for _ in 0..10 {
        let c = hint(Req::valid().run(&h));
        assert_eq!(c.in_flight, 0);
        assert!(c.eligible);
    }
}

#[test]
fn journal_holds_only_scheduling_identity_and_no_channel_credentials() {
    let h = Harness::new();
    hint(Req::valid().run(&h));
    let json = h.store.json();
    let keys = |v: &Value| -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(
        keys(&json),
        [
            "config",
            "next_id",
            "quarantined_secret",
            "reservations",
            "version"
        ]
    );
    assert_eq!(
        keys(&json["config"]),
        [
            "can_read",
            "enabled",
            "id",
            "ingest_limit",
            "max_connections",
            "revision",
            "secret_ref",
            "stream_reserve",
            "tier"
        ]
    );
    let text = json.to_string();
    for secret in [ACCOUNT_KEY, RESOLVER_KEY] {
        assert!(!text.contains(std::str::from_utf8(secret).unwrap()));
    }
    assert!(!text.contains("Pool"));
}

#[test]
fn candidate_request_wire_shape_is_snake_case_and_strict() {
    let req = CandidateRequest {
        account_id: ACCOUNT.into(),
        class: WorkClass::Ingest,
    };
    assert_eq!(
        serde_json::to_value(&req).unwrap(),
        json!({"account_id": ACCOUNT, "class": "ingest"})
    );
}

// --------------------------------------------- unavailable (no hint)

#[test]
fn missing_or_short_resolver_key_is_unavailable_without_registry_or_storage() {
    for key in [None, Some(RESOLVER_KEY[..31].to_vec()), Some(Vec::new())] {
        let h = Harness::new();
        let mut req = Req::valid();
        req.resolver_key = key.clone();
        assert_eq!(req.run(&h), CandidateReply::Unavailable, "{key:?}");
        assert!(h.untouched());
    }
}

#[test]
fn registry_source_failure_is_unavailable_without_storage() {
    let h = Harness::new();
    h.source.state.borrow_mut().reply = Err(RegistryError);
    assert_eq!(Req::valid().run(&h), CandidateReply::Unavailable);
    assert_eq!(h.source.fetches(), 1, "no retry");
    assert_eq!(h.store.touches(), (0, 0));
}

#[test]
fn registry_that_rejects_our_resolver_credential_is_unavailable() {
    let h = Harness::new();
    // The service binding holds a different key: its HMAC verification fails.
    h.source.state.borrow_mut().key = Some(ACCOUNT_KEY.to_vec());
    assert_eq!(Req::valid().run(&h), CandidateReply::Unavailable);
    assert_eq!(h.source.fetches(), 1);
    assert_eq!(h.store.touches(), (0, 0));

    // And the route's own resolver credential is not the account credential.
    let h = Harness::new();
    let mut req = Req::valid();
    req.resolver_key = Some(ACCOUNT_KEY.to_vec());
    assert_eq!(req.run(&h), CandidateReply::Unavailable);
    assert_eq!(h.store.touches(), (0, 0));
}

#[test]
fn invalid_registry_is_unavailable_without_storage() {
    let mut bad_row = cfg();
    bad_row.revision = 0;
    let duplicate = [cfg(), cfg()];
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("not json", b"nope".to_vec()),
        ("array", b"[]".to_vec()),
        ("no accounts", b"{}".to_vec()),
        ("invalid row", registry(&[bad_row])),
        ("duplicate ids", registry(&duplicate)),
        (
            "unknown top-level field",
            br#"{"accounts":[],"extra":true}"#.to_vec(),
        ),
        ("oversize", vec![b' '; MAX_REGISTRY_BYTES + 1]),
    ];
    for (name, raw) in cases {
        let h = Harness::new();
        h.source.set_raw(&raw);
        assert_eq!(Req::valid().run(&h), CandidateReply::Unavailable, "{name}");
        assert_eq!(h.source.fetches(), 1, "{name}");
        assert_eq!(h.store.touches(), (0, 0), "{name}");
    }
}

#[test]
fn account_absent_from_registry_is_unavailable_without_storage() {
    let h = Harness::new();
    h.source.set_registry(&[row(7)]);
    assert_eq!(Req::valid().run(&h), CandidateReply::Unavailable);
    h.source.set_registry(&[]);
    assert_eq!(Req::valid().run(&h), CandidateReply::Unavailable);
    assert_eq!(h.store.touches(), (0, 0));
}

#[test]
fn rows_for_other_accounts_never_select_or_poison_the_hint() {
    let h = Harness::new();
    h.source.set_registry(&[row(9), cfg(), row(3)]);
    assert_eq!(hint(Req::valid().run(&h)).id, ACCOUNT);
}

// ----------------------------------------- refresh on every request

#[test]
fn registry_is_refreshed_on_every_request_and_changes_apply() {
    let h = Harness::new();
    assert!(hint(Req::valid().run(&h)).eligible);
    assert_eq!(h.source.fetches(), 1);

    // Disable (higher revision) applies on the very next request.
    let mut next = cfg();
    next.revision = 2;
    next.enabled = false;
    h.source.set_registry(&[next.clone()]);
    assert!(!hint(Req::valid().run(&h)).eligible);
    assert_eq!(h.source.fetches(), 2);
    assert_eq!(h.store.json()["config"]["enabled"], json!(false));
    assert_eq!(h.store.json()["config"]["revision"], json!(2));

    // Tier changes apply too.
    next.revision = 3;
    next.tier = 5;
    h.source.set_registry(&[next]);
    let c = hint(Req::valid().run(&h));
    assert_eq!(c.tier, 5);
    assert_eq!(h.source.fetches(), 3);
    assert_eq!(h.store.json()["config"]["tier"], json!(5));
}

#[test]
fn same_revision_identical_snapshot_is_accepted() {
    let h = Harness::new();
    hint(Req::valid().run(&h));
    hint(Req::valid().run(&h));
    assert_eq!(h.source.fetches(), 2);
}

#[test]
fn stale_revision_is_unavailable_and_leaves_stored_settings() {
    let h = Harness::new();
    let mut current = cfg();
    current.revision = 5;
    current.tier = 2;
    h.source.set_registry(&[current.clone()]);
    hint(Req::valid().run(&h));
    let before = h.store.json();
    let saves = h.store.touches().1;

    let mut stale = cfg();
    stale.revision = 4;
    stale.tier = 9;
    h.source.set_registry(&[stale]);
    assert_eq!(Req::valid().run(&h), CandidateReply::Unavailable);
    assert_eq!(h.store.json(), before);
    assert_eq!(h.store.touches().1, saves, "refusal writes nothing");

    // The owner is not fenced by a stale read: the current snapshot still works.
    h.source.set_registry(&[current]);
    assert_eq!(hint(Req::valid().run(&h)).tier, 2);
}

#[test]
fn same_revision_conflict_is_unavailable_and_leaves_stored_settings() {
    let h = Harness::new();
    hint(Req::valid().run(&h));
    let before = h.store.json();
    let saves = h.store.touches().1;

    let mut conflict = cfg();
    conflict.tier = 7;
    h.source.set_registry(&[conflict]);
    assert_eq!(Req::valid().run(&h), CandidateReply::Unavailable);
    let mut conflict = cfg();
    conflict.secret_ref = SECRET_B.into();
    h.source.set_registry(&[conflict]);
    assert_eq!(Req::valid().run(&h), CandidateReply::Unavailable);
    assert_eq!(h.store.json(), before);
    assert_eq!(h.store.touches().1, saves);

    h.source.set_registry(&[cfg()]);
    assert!(hint(Req::valid().run(&h)).eligible);
}

// ------------------------------------------------- quarantine hint

fn quarantined_store() -> FakeStorage {
    let store = FakeStorage::new();
    let owner = Owner::new(ACCOUNT.into(), store.clone()).unwrap();
    let mut guard = drive(owner.lock(cfg(), 0)).expect("lock");
    let id = drive(guard.reserve(WorkClass::Stream, 0)).expect("reserve");
    let ctl = FakeControl(Rc::new(Cell::new(false)));
    let handed = ctl.clone();
    let _setup = guard
        .start(id, u64::MAX, 0, move || {
            Ok::<_, ()>((handed, std::future::ready(())))
        })
        .expect("start");
    drop(guard);
    drive(owner.record_result(id, SessionResult::AuthFailure, 0)).unwrap();
    drive(owner.close(id)).unwrap();
    assert_eq!(store.json()["quarantined_secret"], json!(SECRET_A));
    assert_eq!(store.json()["reservations"], json!({}));
    store
}

#[test]
fn quarantine_persists_on_a_reopened_owner_and_is_ineligible() {
    let store = quarantined_store();
    let h = Harness::over(store);
    for class in ["stream", "ingest"] {
        let c = hint(Req::new(ACCOUNT, class).run(&h));
        assert!(!c.eligible, "{class}: quarantined secret is not retried");
    }
    assert_eq!(h.store.json()["quarantined_secret"], json!(SECRET_A));
    assert_eq!(reservations(&h), 0);
}

#[test]
fn quarantine_survives_unrelated_revisions_and_clears_only_on_rotation() {
    let h = Harness::over(quarantined_store());

    // Capacity change with the same credential keeps the quarantine.
    let mut next = cfg();
    next.revision = 2;
    next.max_connections = 4;
    next.ingest_limit = 3;
    h.source.set_registry(&[next.clone()]);
    assert!(!hint(Req::valid().run(&h)).eligible);
    assert_eq!(h.store.json()["quarantined_secret"], json!(SECRET_A));

    // Rotation to a new secret applies on the next refresh.
    next.revision = 3;
    next.secret_ref = SECRET_B.into();
    h.source.set_registry(&[next]);
    assert!(hint(Req::valid().run(&h)).eligible);
    assert_eq!(h.store.json()["quarantined_secret"], Value::Null);
    assert_eq!(h.store.json()["config"]["secret_ref"], json!(SECRET_B));
}

// ------------------------------------------------ completion clock

fn open_breaker(store: &FakeStorage) -> Owner {
    let owner = Owner::new(ACCOUNT.into(), store.clone()).unwrap();
    for _ in 0..3 {
        let mut guard = drive(owner.lock(cfg(), 0)).expect("lock");
        let id = drive(guard.reserve(WorkClass::Stream, 0)).expect("reserve");
        let ctl = FakeControl(Rc::new(Cell::new(false)));
        let handed = ctl.clone();
        let _setup = guard
            .start(id, u64::MAX, 0, move || {
                Ok::<_, ()>((handed, std::future::ready(())))
            })
            .expect("start");
        drop(guard);
        drive(owner.record_result(id, SessionResult::ProviderFailure, 0)).unwrap();
        drive(owner.close(id)).unwrap();
    }
    owner
}

fn breaker_harness(latency_ms: u64) -> Harness {
    let store = FakeStorage::new();
    // Retained owner whose breaker opened at t=0 (cooldown 30s).
    let owner = open_breaker(&store);
    let mut h = Harness::over(store);
    h.owner = owner;
    h.clock.set(10_000);
    h.source.state.borrow_mut().latency_ms = latency_ms;
    h
}

#[test]
fn hint_uses_completion_time_after_registry_latency() {
    // Dispatch at 10s, registry returns at 30s: the cooldown has elapsed.
    let h = breaker_harness(20_000);
    assert!(hint(Req::valid().run(&h)).eligible);
    assert_eq!(h.clock.get(), 30_000);
}

#[test]
fn hint_before_cooldown_completion_stays_ineligible() {
    // Same dispatch time, but the response lands one millisecond too early.
    let h = breaker_harness(19_999);
    assert!(!hint(Req::valid().run(&h)).eligible);
    // No latency at all: still inside the cooldown.
    let h = breaker_harness(0);
    assert!(!hint(Req::valid().run(&h)).eligible);
}

#[test]
fn clock_is_read_for_auth_header_and_completion() {
    let h = Harness::new();
    hint(Req::valid().run(&h));
    assert!(
        h.clock_reads.get() >= 3,
        "auth, registry header, completion"
    );

    // Rejected requests read the clock only to check freshness, never again.
    let h = Harness::new();
    let mut req = Req::valid();
    req.browser = true;
    req.run(&h);
    assert_eq!(h.clock_reads.get(), 0);
    let mut req = Req::valid();
    req.account_key = None;
    req.run(&h);
    assert!(h.clock_reads.get() <= 1);
}

// --------------------------------------------------- parse_registry

fn rows_json(rows: &[AccountConfig]) -> String {
    format!(r#"{{"accounts":{}}}"#, serde_json::to_string(rows).unwrap())
}

fn row_json() -> String {
    serde_json::to_string(&cfg()).unwrap()
}

#[test]
fn parser_accepts_empty_and_normal_registries() {
    assert_eq!(parse_registry(br#"{"accounts":[]}"#), Ok(vec![]));
    assert_eq!(parse_registry(b" \n\t{\"accounts\":[]}"), Ok(vec![]));
    assert_eq!(
        parse_registry(rows_json(&[cfg(), row(2)]).as_bytes()),
        Ok(vec![cfg(), row(2)])
    );
}

#[test]
fn parser_rejects_non_objects_and_bad_wrappers() {
    for raw in [
        &b""[..],
        b"   ",
        b"[]",
        b"null",
        b"1",
        b"\"x\"",
        b"{}",
        b"{\"accounts\":null}",
        b"{\"accounts\":{}}",
        b"{\"accounts\":\"x\"}",
        b"{\"accounts\":[]",
        b"{\"accounts\":[]} trailing",
        b"{\"accounts\":[],\"accounts\":[]}",
        b"{\"accounts\":[],\"extra\":1}",
        b"{\"accounts\":[]}\xff",
    ] {
        assert_eq!(
            parse_registry(raw),
            Err(RegistryError),
            "{:?}",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn parser_caps_bytes_exactly() {
    let head = br#"{"accounts":[]"#;
    let mut at_cap = head.to_vec();
    at_cap.resize(MAX_REGISTRY_BYTES - 1, b' ');
    at_cap.push(b'}');
    assert_eq!(at_cap.len(), MAX_REGISTRY_BYTES);
    assert_eq!(parse_registry(&at_cap), Ok(vec![]));

    let mut over = head.to_vec();
    over.resize(MAX_REGISTRY_BYTES, b' ');
    over.push(b'}');
    assert_eq!(over.len(), MAX_REGISTRY_BYTES + 1);
    assert_eq!(parse_registry(&over), Err(RegistryError));
}

#[test]
fn parser_caps_accounts_at_one_thousand() {
    assert_eq!(MAX_ACCOUNTS, 1000);
    let rows: Vec<AccountConfig> = (1..=1000).map(row).collect();
    let raw = rows_json(&rows);
    assert!(
        raw.len() < MAX_REGISTRY_BYTES,
        "row count is the binding cap"
    );
    assert_eq!(parse_registry(raw.as_bytes()).unwrap().len(), 1000);

    let rows: Vec<AccountConfig> = (1..=1001).map(row).collect();
    let raw = rows_json(&rows);
    assert!(raw.len() < MAX_REGISTRY_BYTES);
    assert_eq!(parse_registry(raw.as_bytes()), Err(RegistryError));
}

#[test]
fn parser_rejects_duplicate_account_ids() {
    assert_eq!(
        parse_registry(rows_json(&[cfg(), row(2), cfg()]).as_bytes()),
        Err(RegistryError)
    );
    // Same id, different content is still a duplicate row.
    let mut other = cfg();
    other.revision = 2;
    assert_eq!(
        parse_registry(rows_json(&[cfg(), other]).as_bytes()),
        Err(RegistryError)
    );
}

#[test]
fn parser_rejects_duplicate_and_unknown_row_fields() {
    let good = row_json();
    assert!(parse_registry(format!(r#"{{"accounts":[{good}]}}"#).as_bytes()).is_ok());

    let dup_tier = good.replacen(r#""tier":0"#, r#""tier":0,"tier":1"#, 1);
    assert_ne!(dup_tier, good);
    assert_eq!(
        parse_registry(format!(r#"{{"accounts":[{dup_tier}]}}"#).as_bytes()),
        Err(RegistryError)
    );
    let dup_id = good.replacen('{', &format!(r#"{{"id":"{ACCOUNT}","#), 1);
    assert_eq!(
        parse_registry(format!(r#"{{"accounts":[{dup_id}]}}"#).as_bytes()),
        Err(RegistryError)
    );

    for extra in [
        r#""username":"u""#,
        r#""password":"p""#,
        r#""host":"h""#,
        r#""note":null"#,
    ] {
        let unknown = good.replacen('{', &format!("{{{extra},"), 1);
        assert_eq!(
            parse_registry(format!(r#"{{"accounts":[{unknown}]}}"#).as_bytes()),
            Err(RegistryError),
            "{extra}"
        );
    }

    let missing = good.replacen(r#""tier":0,"#, "", 1);
    assert_ne!(missing, good);
    assert_eq!(
        parse_registry(format!(r#"{{"accounts":[{missing}]}}"#).as_bytes()),
        Err(RegistryError)
    );
}

#[test]
fn parser_is_object_only_including_account_rows() {
    // Tuple rows in field order: serde's struct visitor would accept these.
    let tuple = format!(r#"{{"accounts":[["{ACCOUNT}",1,"{SECRET_A}",true,3,1,2,0,true]]}}"#);
    assert_eq!(parse_registry(tuple.as_bytes()), Err(RegistryError));
    // Wrapper itself as a tuple is not an object.
    assert_eq!(
        parse_registry(format!(r#"[[{}]]"#, row_json()).as_bytes()),
        Err(RegistryError)
    );
    // Mixed: one object row and one tuple row.
    let mixed = format!(
        r#"{{"accounts":[{},["{}",1,"{SECRET_A}",true,3,1,2,0,true]]}}"#,
        row_json(),
        uuid(2)
    );
    assert_eq!(parse_registry(mixed.as_bytes()), Err(RegistryError));
    for row in ["null", "1", "\"x\"", "true", "[]", "[[]]"] {
        assert_eq!(
            parse_registry(format!(r#"{{"accounts":[{row}]}}"#).as_bytes()),
            Err(RegistryError),
            "{row}"
        );
    }
}

#[test]
fn parser_rejects_nested_values_and_wrong_field_types() {
    let good = row_json();
    for (from, to) in [
        (r#""tier":0"#, r#""tier":{"n":0}"#),
        (r#""tier":0"#, r#""tier":[0]"#),
        (r#""tier":0"#, r#""tier":"0""#),
        (r#""tier":0"#, r#""tier":-1"#),
        (r#""tier":0"#, r#""tier":0.5"#),
        (r#""enabled":true"#, r#""enabled":"true""#),
        (r#""can_read":true"#, r#""can_read":1"#),
        (r#""revision":1"#, r#""revision":{"v":1}"#),
        (r#""max_connections":3"#, r#""max_connections":4294967296"#),
    ] {
        let bad = good.replacen(from, to, 1);
        assert_ne!(bad, good, "{from}");
        assert_eq!(
            parse_registry(format!(r#"{{"accounts":[{bad}]}}"#).as_bytes()),
            Err(RegistryError),
            "{to}"
        );
    }
}

#[test]
fn parser_rejects_invalid_account_configs() {
    let mut cases: Vec<(&str, AccountConfig)> = Vec::new();
    let mut c = cfg();
    c.id = "not-a-uuid".into();
    cases.push(("bad id", c));
    let mut c = cfg();
    c.id = ACCOUNT.to_uppercase();
    cases.push(("uppercase id", c));
    let mut c = cfg();
    c.secret_ref = "x".into();
    cases.push(("bad secret_ref", c));
    let mut c = cfg();
    c.revision = 0;
    cases.push(("zero revision", c));
    let mut c = cfg();
    c.revision = i64::MAX as u64 + 1;
    cases.push(("revision above i64", c));
    let mut c = cfg();
    c.tier = 101;
    cases.push(("tier above 100", c));
    let mut c = cfg();
    c.max_connections = 0;
    cases.push(("zero connections", c));
    let mut c = cfg();
    c.stream_reserve = 0;
    cases.push(("reader without stream reserve", c));
    let mut c = cfg();
    c.can_read = false;
    cases.push(("non-reader with ingest limit", c));
    let mut c = cfg();
    c.ingest_limit = 3;
    cases.push(("ingest limit leaves no reserve", c));

    for (name, bad) in cases {
        assert_eq!(
            parse_registry(rows_json(&[row(2), bad]).as_bytes()),
            Err(RegistryError),
            "{name}"
        );
    }
}
