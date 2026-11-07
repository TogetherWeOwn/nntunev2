//! One-shot account operation composition (`account_operation::run`).
//!
//! Deterministic and single-threaded: fake registry/resolver, connector,
//! scripted NNTP transport, cleanup scheduler and in-memory journal. Pending
//! states are produced by flags and futures are advanced by hand with a no-op
//! waker. Synthetic `.invalid` names and a public example address only; there
//! is no network. These fakes are NOT evidence of live Worker transport,
//! storage or socket-closure behaviour.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::{poll_fn, ready, Future};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use nntune_core::pool_channel::{self, AuthError};
use nntune_core::transport::Transport;
use nntune_stream::account_operation::{
    run, AccountConnector, CleanupScheduler, DialSource, Operation, OperationError,
    OperationUnavailable, Output, Served, MAX_DIAL_BYTES,
};
use nntune_stream::account_owner::AccountOwner;
use nntune_stream::account_pool::{AccountConfig, Candidate, WorkClass, SESSION_TTL_MS};
use nntune_stream::account_route::{RegistryError, RegistrySource};
use nntune_stream::account_supervisor::{
    AccountJournal, AccountStorage, CleanupReport, CloseControl, CloseError, StorageError,
};
use nntune_stream::probe::MAX_PROBE_SEGMENTS;
use serde_json::{json, Value};
use zeroize::Zeroizing;

const ACCOUNT: &str = "12345678-1234-4234-8234-123456789abc";
const OTHER_ACCOUNT: &str = "52345678-1234-4234-8234-123456789abc";
const SECRET: &str = "22345678-1234-4234-8234-123456789abc";
const OTHER_SECRET: &str = "32345678-1234-4234-8234-123456789abc";
const KEY: [u8; 32] = [7; 32];
const NOW_MS: u64 = 1_700_000_000_000;
const HOST: &str = "news.example.invalid";
const PEER: &str = "93.184.216.34";
const USER: &str = "synthetic-user";
const PASS: &str = "synthetic-pass";
const ARTICLE: &str = "part1@example.invalid";

// ---------------------------------------------------------------- fakes

#[derive(Clone, Copy, Default)]
enum CloseMode {
    #[default]
    Confirm,
    Unconfirmed,
    Fail,
}

#[derive(Default)]
struct World {
    clock: Cell<u64>,
    registry: RefCell<Vec<u8>>,
    registry_calls: RefCell<Vec<String>>,
    dial_reply: RefCell<Option<Vec<u8>>>,
    dial_calls: RefCell<Vec<(String, Vec<u8>)>>,
    scheduler_fail: Cell<bool>,
    scheduler_calls: Cell<u32>,
    opens: RefCell<Vec<(SocketAddr, String)>>,
    hold_connect: Cell<bool>,
    hold_pass_read: Cell<bool>,
    replies: RefCell<VecDeque<Vec<u8>>>,
    written: RefCell<Vec<String>>,
    jump_clock_on_user: Cell<Option<u64>>,
    block_save_on_pass: Cell<bool>,
    close_mode: Cell<CloseMode>,
    close_calls: Cell<u32>,
    closed: Cell<bool>,
    journal: RefCell<Option<AccountJournal>>,
    block_save: Cell<bool>,
}

/// Registry, resolver, scheduler, connector and storage in one shared world.
#[derive(Clone)]
struct Fake(Rc<World>);

impl RegistrySource for Fake {
    async fn fetch(&self, authorization: &str) -> Result<Vec<u8>, RegistryError> {
        self.0
            .registry_calls
            .borrow_mut()
            .push(authorization.into());
        Ok(self.0.registry.borrow().clone())
    }
}

impl DialSource for Fake {
    async fn resolve(
        &self,
        authorization: &str,
        body: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, OperationUnavailable> {
        self.0
            .dial_calls
            .borrow_mut()
            .push((authorization.into(), body.to_vec()));
        match self.0.dial_reply.borrow().clone() {
            Some(raw) => Ok(Zeroizing::new(raw)),
            None => Err(OperationUnavailable),
        }
    }
}

impl CleanupScheduler for Fake {
    async fn ensure_cleanup(&self) -> Result<(), OperationUnavailable> {
        self.0.scheduler_calls.set(self.0.scheduler_calls.get() + 1);
        if self.0.scheduler_fail.get() {
            Err(OperationUnavailable)
        } else {
            Ok(())
        }
    }
}

impl AccountStorage for Fake {
    async fn load(&mut self) -> Result<Option<AccountJournal>, StorageError> {
        Ok(self.0.journal.borrow().clone())
    }

    async fn save(&mut self, journal: &AccountJournal) -> Result<(), StorageError> {
        poll_fn(|_| {
            if self.0.block_save.get() {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        *self.0.journal.borrow_mut() = Some(journal.clone());
        Ok(())
    }
}

/// Retained, independent close authority (owned by the supervisor).
#[derive(Clone)]
struct Ctl(Rc<World>);

impl CloseControl for Ctl {
    async fn close(&self) -> Result<(), CloseError> {
        self.0.close_calls.set(self.0.close_calls.get() + 1);
        match self.0.close_mode.get() {
            CloseMode::Confirm => {
                self.0.closed.set(true);
                Ok(())
            }
            CloseMode::Unconfirmed => Ok(()),
            CloseMode::Fail => Err(CloseError),
        }
    }

    fn confirmed_closed(&self) -> bool {
        self.0.closed.get()
    }
}

/// Scripted server: the greeting is available at once, and every client write
/// releases the next scripted reply. Reading past the script is EOF.
struct Wire {
    world: Rc<World>,
    inbox: VecDeque<u8>,
}

impl Transport for Wire {
    async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        poll_fn(|_| {
            if self.world.hold_pass_read.get()
                && self
                    .world
                    .written
                    .borrow()
                    .last()
                    .is_some_and(|line| line.starts_with("AUTHINFO PASS"))
            {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        if self.inbox.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "script exhausted",
            ));
        }
        let n = buf.len().min(self.inbox.len());
        for (slot, byte) in buf.iter_mut().zip(self.inbox.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }

    async fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        let line = String::from_utf8_lossy(buf).trim_end().to_owned();
        if line.starts_with("AUTHINFO USER") {
            if let Some(t) = self.world.jump_clock_on_user.get() {
                self.world.clock.set(t);
            }
        }
        if line.starts_with("AUTHINFO PASS") && self.world.block_save_on_pass.get() {
            self.world.block_save.set(true);
        }
        self.world.written.borrow_mut().push(line);
        if let Some(reply) = self.world.replies.borrow_mut().pop_front() {
            self.inbox.extend(reply);
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AccountConnector for Fake {
    type Control = Ctl;
    type Connection = Wire;

    fn open(
        &self,
        peer: SocketAddr,
        tls_name: String,
    ) -> io::Result<(Ctl, impl Future<Output = io::Result<Wire>>)> {
        self.0.opens.borrow_mut().push((peer, tls_name));
        let world = self.0.clone();
        Ok((Ctl(self.0.clone()), async move {
            poll_fn(|_| {
                if world.hold_connect.get() {
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            })
            .await;
            let greeting = world.replies.borrow_mut().pop_front().unwrap_or_default();
            Ok(Wire {
                world,
                inbox: greeting.into(),
            })
        }))
    }
}

// ------------------------------------------------------------- fixtures

type Owner = AccountOwner<Fake, Ctl>;
type Outcome = Result<Served, OperationError>;

fn config() -> AccountConfig {
    AccountConfig {
        id: ACCOUNT.into(),
        revision: 3,
        secret_ref: SECRET.into(),
        can_read: true,
        max_connections: 2,
        stream_reserve: 1,
        ingest_limit: 1,
        tier: 1,
        enabled: true,
    }
}

fn dial_material() -> Value {
    json!({
        "provider_id": ACCOUNT,
        "revision": 3,
        "secret_ref": SECRET,
        "host": HOST,
        "port": 563,
        "tls": true,
        "peer": PEER,
        "expires_at": NOW_MS / 1000 + 3,
        "username": USER,
        "password": PASS,
    })
}

fn registry_bytes(config: &AccountConfig) -> Vec<u8> {
    serde_json::to_vec(&json!({ "accounts": [config] })).unwrap()
}

/// Greeting, MODE READER, both AUTHINFO replies, then `tail`.
fn script(tail: &[&str]) -> VecDeque<Vec<u8>> {
    [
        "200 ready\r\n",
        "200 reader\r\n",
        "381 pass\r\n",
        "281 ok\r\n",
    ]
    .iter()
    .chain(tail)
    .map(|r| r.as_bytes().to_vec())
    .collect()
}

fn body_reply() -> String {
    format!("222 0 <{ARTICLE}>\r\nhello\r\n.\r\n")
}

fn stat_reply() -> String {
    format!("223 0 <{ARTICLE}>\r\n")
}

struct Harness {
    fake: Fake,
    owner: Owner,
    config: AccountConfig,
}

impl Harness {
    fn new(config: AccountConfig, tail: &[&str]) -> Self {
        let world = World::default();
        world.clock.set(NOW_MS);
        *world.registry.borrow_mut() = registry_bytes(&config);
        *world.dial_reply.borrow_mut() = Some(serde_json::to_vec(&dial_material()).unwrap());
        *world.replies.borrow_mut() = script(tail);
        let fake = Fake(Rc::new(world));
        let owner = Owner::new(ACCOUNT.into(), fake.clone()).unwrap();
        Self {
            fake,
            owner,
            config,
        }
    }

    fn body() -> Self {
        Self::new(config(), &[&body_reply()])
    }

    /// A fresh owner over the same durable journal and world (DO restart).
    fn restarted(&self) -> Self {
        Self {
            fake: self.fake.clone(),
            owner: Owner::new(ACCOUNT.into(), self.fake.clone()).unwrap(),
            config: self.config.clone(),
        }
    }

    fn world(&self) -> &World {
        &self.fake.0
    }

    fn set_dial(&self, material: &Value) {
        self.set_dial_bytes(serde_json::to_vec(material).unwrap());
    }

    fn set_dial_bytes(&self, raw: Vec<u8>) {
        *self.world().dial_reply.borrow_mut() = Some(raw);
    }

    fn exec<'a, D: Future<Output = ()> + 'a>(
        &'a self,
        class: WorkClass,
        operation: Operation,
        deadline: D,
    ) -> impl Future<Output = Outcome> + 'a {
        let world = &self.fake.0;
        run(
            &self.owner,
            &self.fake,
            &self.fake,
            &self.fake,
            Some(&KEY[..]),
            class,
            operation,
            move || world.clock.get(),
            deadline,
        )
    }

    async fn run_body(&self) -> Outcome {
        self.exec(
            WorkClass::Stream,
            Operation::Body(ARTICLE.into()),
            pending(),
        )
        .await
    }

    fn journal(&self) -> Value {
        serde_json::to_value(
            self.world()
                .journal
                .borrow()
                .as_ref()
                .expect("journal saved"),
        )
        .unwrap()
    }

    fn reservations(&self) -> usize {
        self.journal()["reservations"].as_object().unwrap().len()
    }

    fn quarantined(&self) -> Value {
        self.journal()["quarantined_secret"].clone()
    }

    async fn candidate(&self, class: WorkClass) -> Candidate {
        self.owner
            .lock(self.config.clone(), self.world().clock.get())
            .await
            .unwrap()
            .candidate(class)
    }

    /// No provider contact and no capacity left behind.
    async fn assert_never_dialed(&self, label: &str) {
        assert!(self.world().opens.borrow().is_empty(), "{label}: dialed");
        assert!(self.world().written.borrow().is_empty(), "{label}: wrote");
        assert_eq!(self.reservations(), 0, "{label}: reservation left");
        assert_eq!(
            self.candidate(WorkClass::Stream).await.in_flight,
            0,
            "{label}"
        );
    }
}

fn pending() -> impl Future<Output = ()> {
    std::future::pending()
}

fn flag(flag: &Rc<Cell<bool>>) -> impl Future<Output = ()> {
    let flag = flag.clone();
    poll_fn(move |_| {
        if flag.get() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
}

fn poll<F: Future + ?Sized>(f: &mut Pin<Box<F>>) -> Poll<F::Output> {
    f.as_mut().poll(&mut Context::from_waker(Waker::noop()))
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn body_success_is_attributed_to_the_admitted_account_with_exact_signed_requests() {
    let h = Harness::body();
    let served = h.run_body().await.unwrap();
    assert_eq!(served.provider_id, ACCOUNT);
    assert_eq!(served.output, Output::Body(b"hello\r\n".to_vec()));

    let now_secs = NOW_MS / 1000;
    let registry = h.world().registry_calls.borrow();
    assert_eq!(registry.len(), 1);
    assert_eq!(
        pool_channel::verify(
            Some(&KEY),
            pool_channel::REGISTRY_PATH,
            Some(&registry[0]),
            b"{}",
            now_secs
        ),
        Ok(())
    );

    let dial = h.world().dial_calls.borrow();
    assert_eq!(dial.len(), 1);
    let (authorization, body) = &dial[0];
    assert_eq!(
        pool_channel::verify(
            Some(&KEY),
            pool_channel::RESOLVE_PATH,
            Some(authorization),
            body,
            now_secs
        ),
        Ok(())
    );
    // The signature binds these exact bytes, not another path or body.
    assert_eq!(
        pool_channel::verify(
            Some(&KEY),
            pool_channel::REGISTRY_PATH,
            Some(authorization),
            body,
            now_secs
        ),
        Err(AuthError::Unauthorized)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(body).unwrap(),
        json!({"provider_id": ACCOUNT, "revision": 3, "secret_ref": SECRET, "class": "stream"})
    );

    assert_eq!(
        *h.world().opens.borrow(),
        vec![(
            SocketAddr::new(PEER.parse().unwrap(), 563),
            HOST.to_string()
        )]
    );
    assert_eq!(
        *h.world().written.borrow(),
        vec![
            "MODE READER".to_string(),
            format!("AUTHINFO USER {USER}"),
            format!("AUTHINFO PASS {PASS}"),
            format!("BODY <{ARTICLE}>"),
        ]
    );
    assert_eq!(h.world().scheduler_calls.get(), 1);
    assert!(h.world().closed.get());
    assert_eq!(h.reservations(), 0);
    assert_eq!(h.quarantined(), Value::Null);
}

#[tokio::test]
async fn ingest_class_is_signed_into_the_resolve_request() {
    let h = Harness::new(config(), &[&body_reply()]);
    let served = h
        .exec(
            WorkClass::Ingest,
            Operation::Body(ARTICLE.into()),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(served.provider_id, ACCOUNT);
    let dial = h.world().dial_calls.borrow();
    let body: Value = serde_json::from_slice(&dial[0].1).unwrap();
    assert_eq!(body["class"], "ingest");
}

#[tokio::test]
async fn proof_expiring_exactly_at_the_ttl_bound_is_accepted() {
    let h = Harness::body();
    let mut material = dial_material();
    material["expires_at"] = json!(NOW_MS / 1000 + 5);
    h.set_dial(&material);
    assert!(h.run_body().await.is_ok());
}

#[tokio::test]
async fn stat_separates_present_from_missing() {
    let missing = "430 no such article\r\n";
    let h = Harness::new(config(), &[&stat_reply(), missing]);
    let ids = vec![ARTICLE.to_string(), "gone@example.invalid".to_string()];
    let served = h
        .exec(WorkClass::Stream, Operation::Stat(ids), pending())
        .await
        .unwrap();
    assert_eq!(
        served.output,
        Output::Stat {
            present: vec![ARTICLE.into()],
            missing: vec!["gone@example.invalid".into()],
        }
    );
    assert_eq!(h.world().written.borrow().len(), 5);
    assert!(h.world().closed.get());
    assert_eq!(h.reservations(), 0);
}

#[tokio::test]
async fn stat_with_every_article_missing_is_still_a_success() {
    let h = Harness::new(config(), &["430 no\r\n"]);
    let served = h
        .exec(
            WorkClass::Stream,
            Operation::Stat(vec![ARTICLE.into()]),
            pending(),
        )
        .await
        .unwrap();
    assert_eq!(
        served.output,
        Output::Stat {
            present: vec![],
            missing: vec![ARTICLE.into()]
        }
    );
    assert_eq!(h.quarantined(), Value::Null);
}

#[tokio::test]
async fn missing_body_is_missing_not_a_provider_failure() {
    let h = Harness::new(config(), &["430 no such article\r\n"]);
    assert_eq!(h.run_body().await.unwrap_err(), OperationError::Missing);
    assert!(h.world().closed.get());
    assert_eq!(h.reservations(), 0);
    assert_eq!(h.quarantined(), Value::Null);
    assert!(h.candidate(WorkClass::Stream).await.eligible);
}

#[tokio::test]
async fn invalid_operations_touch_nothing() {
    let too_many = (0..=MAX_PROBE_SEGMENTS)
        .map(|i| format!("id{i}@example.invalid"))
        .collect();
    let cases = [
        ("empty body id", Operation::Body(String::new())),
        ("bracketed id", Operation::Body(format!("<{ARTICLE}>"))),
        ("no at sign", Operation::Body("plain".into())),
        ("line break", Operation::Body("a@b\r\nQUIT".into())),
        ("empty stat", Operation::Stat(vec![])),
        (
            "bad stat member",
            Operation::Stat(vec![ARTICLE.into(), "bad id@x".into()]),
        ),
        ("oversized stat", Operation::Stat(too_many)),
    ];
    for (label, operation) in cases {
        let h = Harness::body();
        let result = h.exec(WorkClass::Stream, operation, pending()).await;
        assert_eq!(result.unwrap_err(), OperationError::Invalid, "{label}");
        let w = h.world();
        assert!(
            w.registry_calls.borrow().is_empty(),
            "{label}: registry read"
        );
        assert!(w.dial_calls.borrow().is_empty(), "{label}: resolve");
        assert!(w.opens.borrow().is_empty(), "{label}: dial");
        assert_eq!(w.scheduler_calls.get(), 0, "{label}: scheduler");
        assert!(w.journal.borrow().is_none(), "{label}: durable write");
    }
}

#[tokio::test]
async fn disabled_account_neither_resolves_nor_dials() {
    let mut disabled = config();
    disabled.enabled = false;
    let h = Harness::new(disabled, &[&body_reply()]);
    assert_eq!(h.run_body().await.unwrap_err(), OperationError::Unavailable);
    assert!(h.world().dial_calls.borrow().is_empty());
    h.assert_never_dialed("disabled").await;
}

#[tokio::test]
async fn exhausted_class_cap_neither_resolves_nor_dials() {
    let mut no_ingest = config();
    no_ingest.ingest_limit = 0;
    let h = Harness::new(no_ingest, &[&body_reply()]);
    let result = h
        .exec(
            WorkClass::Ingest,
            Operation::Body(ARTICLE.into()),
            pending(),
        )
        .await;
    assert_eq!(result.unwrap_err(), OperationError::Unavailable);
    assert!(h.world().dial_calls.borrow().is_empty());
    h.assert_never_dialed("ingest cap").await;
    // The same account still serves playback.
    assert!(h.run_body().await.is_ok());
}

#[tokio::test]
async fn registry_failures_stop_before_any_durable_or_provider_step() {
    let missing_account = serde_json::to_vec(&json!({ "accounts": [] })).unwrap();
    for (label, raw) in [
        ("garbage", b"not json".to_vec()),
        ("array", b"[]".to_vec()),
        ("absent account", missing_account),
    ] {
        let h = Harness::body();
        *h.world().registry.borrow_mut() = raw;
        assert_eq!(
            h.run_body().await.unwrap_err(),
            OperationError::Unavailable,
            "{label}"
        );
        let w = h.world();
        assert_eq!(w.scheduler_calls.get(), 0, "{label}");
        assert!(w.dial_calls.borrow().is_empty(), "{label}");
        assert!(w.opens.borrow().is_empty(), "{label}");
    }
}

#[tokio::test]
async fn scheduler_failure_stops_before_reserve_resolve_and_dial() {
    let h = Harness::body();
    h.world().scheduler_fail.set(true);
    assert_eq!(h.run_body().await.unwrap_err(), OperationError::Unavailable);
    assert_eq!(h.world().scheduler_calls.get(), 1);
    assert!(h.world().dial_calls.borrow().is_empty());
    h.assert_never_dialed("scheduler").await;
}

#[tokio::test]
async fn resolver_failure_releases_the_reservation_without_dialing() {
    let h = Harness::body();
    *h.world().dial_reply.borrow_mut() = None;
    assert_eq!(h.run_body().await.unwrap_err(), OperationError::Unavailable);
    assert_eq!(h.world().dial_calls.borrow().len(), 1);
    h.assert_never_dialed("resolver").await;
}

#[tokio::test]
async fn refused_dial_material_never_dials_or_leaves_capacity() {
    type Mutation = fn(&mut Value);
    let material_cases: &[(&str, Mutation)] = &[
        // Substituted or stale identity.
        ("other provider", |m| {
            m["provider_id"] = json!(OTHER_ACCOUNT)
        }),
        ("older revision", |m| m["revision"] = json!(2)),
        ("newer revision", |m| m["revision"] = json!(4)),
        ("other secret", |m| m["secret_ref"] = json!(OTHER_SECRET)),
        // Proof lifetime.
        ("expired now", |m| m["expires_at"] = json!(NOW_MS / 1000)),
        ("expired earlier", |m| {
            m["expires_at"] = json!(NOW_MS / 1000 - 60)
        }),
        ("too far ahead", |m| {
            m["expires_at"] = json!(NOW_MS / 1000 + 6)
        }),
        ("overflowing expiry", |m| m["expires_at"] = json!(u64::MAX)),
        // TLS and port.
        ("no tls", |m| m["tls"] = json!(false)),
        ("port 80", |m| m["port"] = json!(80)),
        ("port 8080", |m| m["port"] = json!(8080)),
        ("port 0", |m| m["port"] = json!(0)),
        // Peer address policy.
        ("loopback", |m| m["peer"] = json!("127.0.0.1")),
        ("private", |m| {
            m["peer"] = json!(std::net::Ipv4Addr::new(10, 0, 0, 8).to_string())
        }),
        ("metadata", |m| {
            m["peer"] = json!(std::net::Ipv4Addr::new(169, 254, 169, 254).to_string())
        }),
        ("documentation v4", |m| m["peer"] = json!("203.0.113.9")),
        ("loopback v6", |m| m["peer"] = json!("::1")),
        ("unique local v6", |m| m["peer"] = json!("fd00::1")),
        ("mapped v6", |m| m["peer"] = json!("::ffff:93.184.216.34")),
        ("hostname peer", |m| m["peer"] = json!(HOST)),
        ("empty peer", |m| m["peer"] = json!("")),
        // TLS name.
        ("ip literal host", |m| m["host"] = json!(PEER)),
        ("empty host", |m| m["host"] = json!("")),
        ("spaced host", |m| m["host"] = json!("bad host.invalid")),
        ("leading hyphen", |m| {
            m["host"] = json!("-bad.example.invalid")
        }),
        ("underscore host", |m| {
            m["host"] = json!("bad_name.example.invalid")
        }),
        ("empty label", |m| m["host"] = json!("a..invalid")),
        // Credentials.
        ("empty username", |m| m["username"] = json!("")),
        ("empty password", |m| m["password"] = json!("")),
        ("control in username", |m| {
            m["username"] = json!("user\nname")
        }),
        ("control in password", |m| m["password"] = json!("pass\r")),
        ("long username", |m| m["username"] = json!("u".repeat(256))),
        ("long password", |m| m["password"] = json!("p".repeat(1025))),
        // Shape.
        ("unknown field", |m| m["extra"] = json!(1)),
        ("missing password", |m| {
            m.as_object_mut().unwrap().remove("password");
        }),
        ("missing peer", |m| {
            m.as_object_mut().unwrap().remove("peer");
        }),
        ("string port", |m| m["port"] = json!("563")),
        ("string revision", |m| m["revision"] = json!("3")),
        ("null tls", |m| m["tls"] = Value::Null),
        ("negative expiry", |m| m["expires_at"] = json!(-1)),
        ("numeric peer", |m| m["peer"] = json!(16909060)),
    ];
    for &(label, mutate) in material_cases {
        let h = Harness::body();
        let mut material = dial_material();
        mutate(&mut material);
        h.set_dial(&material);
        assert_eq!(
            h.run_body().await.unwrap_err(),
            OperationError::Unavailable,
            "{label}"
        );
        assert_eq!(
            h.world().dial_calls.borrow().len(),
            1,
            "{label}: second resolve"
        );
        h.assert_never_dialed(label).await;
    }

    let valid = serde_json::to_string(&dial_material()).unwrap();
    let raw_cases = [
        ("not json", b"not json".to_vec()),
        ("empty", Vec::new()),
        ("array", b"[]".to_vec()),
        ("tuple row", format!(r#"["{ACCOUNT}"]"#).into_bytes()),
        ("truncated", valid.as_bytes()[..valid.len() - 1].to_vec()),
        ("leading junk", format!("x{valid}").into_bytes()),
        (
            "oversized",
            format!("{valid}{}", " ".repeat(MAX_DIAL_BYTES)).into_bytes(),
        ),
    ];
    for (label, raw) in raw_cases {
        let h = Harness::body();
        h.set_dial_bytes(raw);
        assert_eq!(
            h.run_body().await.unwrap_err(),
            OperationError::Unavailable,
            "{label}"
        );
        h.assert_never_dialed(label).await;
    }
}

#[tokio::test]
async fn auth_rejection_persists_quarantine_and_forbids_a_second_dial() {
    let h = Harness::new(config(), &[]);
    // Replace the PASS acceptance with a rejection.
    h.world().replies.borrow_mut()[3] = b"481 rejected\r\n".to_vec();
    assert_eq!(h.run_body().await.unwrap_err(), OperationError::Unavailable);
    assert!(h.world().closed.get());
    assert_eq!(h.reservations(), 0);
    assert_eq!(h.quarantined(), json!(SECRET));
    assert_eq!(
        h.world().written.borrow().len(),
        3,
        "no BODY after rejection"
    );

    // Same owner: no new resolve or dial.
    assert_eq!(h.run_body().await.unwrap_err(), OperationError::Unavailable);
    assert_eq!(h.world().dial_calls.borrow().len(), 1);
    assert_eq!(h.world().opens.borrow().len(), 1);

    // A restarted owner recovers the persisted quarantine, not a retry.
    let restarted = h.restarted();
    assert_eq!(
        restarted.run_body().await.unwrap_err(),
        OperationError::Unavailable
    );
    assert_eq!(h.world().dial_calls.borrow().len(), 1);
    assert_eq!(h.world().opens.borrow().len(), 1);
    assert!(!restarted.candidate(WorkClass::Stream).await.eligible);
}

#[tokio::test]
async fn blocked_result_storage_cannot_hold_the_socket_open() {
    let h = Harness::body();
    h.world().replies.borrow_mut()[3] = b"481 rejected\r\n".to_vec();
    h.world().block_save_on_pass.set(true);
    let mut op = Box::pin(h.run_body());
    assert!(poll(&mut op).is_pending());
    // Quarantine is not durable yet, but the physical close already finished.
    assert!(h.world().closed.get());
    assert!(h.world().close_calls.get() >= 1);
    assert_eq!(h.quarantined(), Value::Null);

    h.world().block_save.set(false);
    let result = match poll(&mut op) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("operation must finish once storage resumes"),
    };
    assert_eq!(result.unwrap_err(), OperationError::Unavailable);
    drop(op);
    assert_eq!(h.quarantined(), json!(SECRET));
    assert_eq!(h.reservations(), 0);
}

#[tokio::test]
async fn unconfirmed_close_retains_capacity_and_withholds_output() {
    for mode in [CloseMode::Unconfirmed, CloseMode::Fail] {
        let h = Harness::body();
        h.world().close_mode.set(mode);
        // A successful BODY must not escape without confirmed closure.
        assert_eq!(h.run_body().await.unwrap_err(), OperationError::Unavailable);
        assert!(!h.world().closed.get());
        assert_eq!(h.reservations(), 1);
        let candidate = h.candidate(WorkClass::Stream).await;
        assert_eq!(candidate.in_flight, 1);

        // Capacity returns only after later confirmed closure.
        h.world().close_mode.set(CloseMode::Confirm);
        let report = h.owner.cleanup(h.world().clock.get()).await.unwrap();
        assert_eq!(
            report,
            CleanupReport {
                retired: 1,
                retained: 0
            }
        );
        assert!(h.world().closed.get());
        assert_eq!(h.reservations(), 0);
        assert_eq!(h.candidate(WorkClass::Stream).await.in_flight, 0);
    }
}

#[tokio::test]
async fn concurrency_cap_refuses_before_resolve_while_a_transport_is_pending() {
    let mut single = config();
    single.max_connections = 1;
    single.ingest_limit = 0;
    let h = Harness::new(single, &[&body_reply()]);
    h.world().hold_connect.set(true);

    let mut first = Box::pin(h.run_body());
    assert!(poll(&mut first).is_pending());
    assert_eq!(h.world().opens.borrow().len(), 1);
    assert_eq!(h.reservations(), 1);

    // The second caller is refused at reserve: no resolve, no second dial.
    assert_eq!(h.run_body().await.unwrap_err(), OperationError::Unavailable);
    assert_eq!(h.world().dial_calls.borrow().len(), 1);
    assert_eq!(h.world().opens.borrow().len(), 1);
    assert_eq!(h.reservations(), 1);

    h.world().hold_connect.set(false);
    let served = match poll(&mut first) {
        Poll::Ready(result) => result.unwrap(),
        Poll::Pending => panic!("first operation must finish once connected"),
    };
    drop(first);
    assert_eq!(served.output, Output::Body(b"hello\r\n".to_vec()));
    assert_eq!(h.reservations(), 0);

    // Capacity is back after confirmed retirement.
    *h.world().replies.borrow_mut() = script(&[&body_reply()]);
    h.world().closed.set(false);
    assert!(h.run_body().await.is_ok());
    assert_eq!(h.world().opens.borrow().len(), 2);
}

#[tokio::test]
async fn proof_expiry_before_password_refuses_the_second_authinfo() {
    let h = Harness::body();
    h.world().jump_clock_on_user.set(Some(NOW_MS + 3000));
    assert_eq!(h.run_body().await.unwrap_err(), OperationError::Unavailable);
    assert_eq!(
        *h.world().written.borrow(),
        vec!["MODE READER".to_string(), format!("AUTHINFO USER {USER}")],
        "PASS must never be written on an expired proof"
    );
    assert!(h.world().closed.get());
    assert_eq!(h.reservations(), 0);
    // An expired proof is not evidence the credentials are bad.
    assert_eq!(h.quarantined(), Value::Null);
}

#[tokio::test]
async fn repeated_local_auth_refusals_do_not_trip_the_provider_breaker() {
    let h = Harness::body();
    for offset in [0, 10_000, 20_000] {
        let at = NOW_MS + offset;
        h.world().clock.set(at);
        h.world().closed.set(false);
        h.world().jump_clock_on_user.set(Some(at + 3000));
        *h.world().replies.borrow_mut() = script(&[&body_reply()]);
        let mut material = dial_material();
        material["expires_at"] = json!(at / 1000 + 3);
        h.set_dial(&material);
        assert_eq!(h.run_body().await.unwrap_err(), OperationError::Unavailable);
        assert!(h.world().closed.get());
        assert_eq!(h.reservations(), 0);
    }
    assert_eq!(h.world().opens.borrow().len(), 3);
    assert!(h.candidate(WorkClass::Stream).await.eligible);
    assert_eq!(h.quarantined(), Value::Null);
}

#[tokio::test]
async fn deadline_leaves_a_terminal_session_for_independent_cleanup() {
    let h = Harness::body();
    h.world().hold_connect.set(true);
    let fire = Rc::new(Cell::new(false));
    let mut op = Box::pin(h.exec(
        WorkClass::Stream,
        Operation::Body(ARTICLE.into()),
        flag(&fire),
    ));
    assert!(poll(&mut op).is_pending());
    assert!(!h.world().closed.get());
    assert_eq!(h.reservations(), 1);

    fire.set(true);
    let result = match poll(&mut op) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("deadline must end the operation"),
    };
    assert_eq!(result.unwrap_err(), OperationError::Deadline);
    drop(op);
    assert_eq!(
        h.reservations(),
        1,
        "no post-deadline implicit capacity release"
    );
    assert!(h.owner.needs_cleanup().await);
    assert_eq!(h.owner.cleanup(NOW_MS).await.unwrap().retired, 1);
    assert!(h.world().closed.get());
    assert!(h.world().written.borrow().is_empty());
    assert_eq!(h.reservations(), 0);
    assert!(h.candidate(WorkClass::Stream).await.eligible);
}

#[tokio::test]
async fn cancelling_a_known_auth_failure_behind_the_owner_lock_cannot_lose_quarantine() {
    let h = Harness::body();
    *h.world().replies.borrow_mut() = script(&[]);
    *h.world().replies.borrow_mut().back_mut().unwrap() = b"481 rejected\r\n".to_vec();
    h.world().hold_pass_read.set(true);
    let mut op = Box::pin(h.run_body());
    assert!(poll(&mut op).is_pending());
    let guard = h.owner.lock(h.config.clone(), NOW_MS).await.unwrap();
    h.world().hold_pass_read.set(false);
    assert!(
        poll(&mut op).is_pending(),
        "known failure waits for owner finish lock"
    );
    assert!(
        !guard.candidate(WorkClass::Stream).eligible,
        "latch gates admission even on an existing guard"
    );
    drop(op);
    drop(guard);
    assert_eq!(h.owner.cleanup(NOW_MS).await.unwrap().retired, 1);
    assert_eq!(h.quarantined(), json!(SECRET));
    let before = h.world().opens.borrow().len();
    assert_eq!(h.run_body().await.unwrap_err(), OperationError::Unavailable);
    assert_eq!(h.world().opens.borrow().len(), before);
}

#[tokio::test]
async fn the_whole_deadline_also_bounds_final_retirement_storage() {
    let h = Harness::body();
    h.world().block_save_on_pass.set(true);
    let fire = Rc::new(Cell::new(false));
    let mut op = Box::pin(h.exec(
        WorkClass::Stream,
        Operation::Body(ARTICLE.into()),
        flag(&fire),
    ));
    assert!(poll(&mut op).is_pending());
    assert!(
        h.world().closed.get(),
        "physical closure precedes final storage"
    );
    fire.set(true);
    assert!(matches!(
        poll(&mut op),
        Poll::Ready(Err(OperationError::Deadline))
    ));
    drop(op);
    h.world().block_save.set(false);
    assert_eq!(
        h.reservations(),
        1,
        "uncertain retirement cannot release capacity"
    );
    assert!(h.owner.lock(h.config.clone(), NOW_MS).await.is_err());
}

#[tokio::test]
async fn authenticated_registry_absence_revokes_before_ttl_but_parse_failure_does_not() {
    for absent in [false, true] {
        let h = Harness::body();
        h.world().hold_connect.set(true);
        let mut op = Box::pin(h.run_body());
        assert!(poll(&mut op).is_pending());
        drop(op);
        *h.world().registry.borrow_mut() = if absent {
            b"{\"accounts\":[]}".to_vec()
        } else {
            b"invalid".to_vec()
        };
        let refreshed =
            nntune_stream::account_route::refresh(&h.owner, &h.fake, Some(&KEY), || NOW_MS).await;
        assert_eq!(refreshed.is_ok(), absent);
        let report = h.owner.cleanup(NOW_MS).await.unwrap();
        assert_eq!(report.retired, u32::from(absent));
        assert_eq!(h.world().closed.get(), absent);
        if absent {
            assert!(
                !h.candidate(WorkClass::Stream).await.eligible,
                "stale membership cannot restore the instance"
            );
        }
    }
}

#[tokio::test]
async fn an_already_expired_deadline_does_no_work() {
    let h = Harness::body();
    let result = h
        .exec(
            WorkClass::Stream,
            Operation::Body(ARTICLE.into()),
            ready(()),
        )
        .await;
    assert_eq!(result.unwrap_err(), OperationError::Deadline);
    assert!(h.world().registry_calls.borrow().is_empty());
    assert!(h.world().dial_calls.borrow().is_empty());
    assert!(h.world().opens.borrow().is_empty());
}

#[tokio::test]
async fn caller_drop_leaves_the_session_for_retained_cleanup() {
    let h = Harness::body();
    h.world().hold_connect.set(true);
    let mut op = Box::pin(h.run_body());
    assert!(poll(&mut op).is_pending());
    drop(op);

    // The cleanup wake was established before the dial, and the dropped future
    // closed nothing by itself.
    assert_eq!(h.world().scheduler_calls.get(), 1);
    assert!(!h.world().closed.get());
    assert_eq!(h.reservations(), 1);

    // A live session inside its TTL is not touched by the sweep.
    let report = h.owner.cleanup(NOW_MS + 1).await.unwrap();
    assert_eq!(
        report,
        CleanupReport {
            retired: 0,
            retained: 1
        }
    );
    assert!(!h.world().closed.get());

    // Past the TTL, the retained control force-closes and capacity returns.
    let later = NOW_MS + SESSION_TTL_MS + 1;
    h.world().clock.set(later);
    let report = h.owner.cleanup(later).await.unwrap();
    assert_eq!(
        report,
        CleanupReport {
            retired: 1,
            retained: 0
        }
    );
    assert!(h.world().closed.get());
    assert_eq!(h.reservations(), 0);
    assert_eq!(h.candidate(WorkClass::Stream).await.in_flight, 0);
}
