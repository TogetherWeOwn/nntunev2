//! Authenticated account operation envelope (`account_dispatch`).
//!
//! `authorize` is pure and covered by refusal matrices; `dispatch` is driven
//! end to end over in-memory fakes (registry, resolver, scheduler, storage,
//! connector and a scripted NNTP wire). No network or real credentials. These
//! fakes are NOT evidence of live Worker transport, storage or socket closure.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::{pending, Future};
use std::io;
use std::net::SocketAddr;
use std::rc::Rc;

use nntune_core::pool_channel::{self, Channel, CANDIDATE_PATH, OPERATION_PATH};
use nntune_core::transport::Transport;
use nntune_stream::account_dispatch::{authorize, dispatch, OperationReply, OperationRequest};
use nntune_stream::account_operation::{
    AccountConnector, CleanupScheduler, DialSource, Operation, OperationError,
    OperationUnavailable, Output, Served,
};
use nntune_stream::account_owner::AccountOwner;
use nntune_stream::account_pool::{AccountConfig, WorkClass};
use nntune_stream::account_route::{CandidateCall, ChannelKeys, RegistryError, RegistrySource};
use nntune_stream::account_supervisor::{
    AccountJournal, AccountStorage, CloseControl, CloseError, StorageError,
};
use serde_json::{json, Value};
use zeroize::Zeroizing;

const ACCOUNT: &str = "12345678-1234-4234-8234-123456789abc";
const OTHER_ACCOUNT: &str = "52345678-1234-4234-8234-123456789abc";
const SECRET: &str = "22345678-1234-4234-8234-123456789abc";
const ACCOUNT_KEY: [u8; 32] = [5; 32];
const RESOLVER_KEY: [u8; 32] = [9; 32];
const NOW_MS: u64 = 1_700_000_000_000;
const NOW_S: u64 = NOW_MS / 1000;
const ARTICLE: &str = "part1@example.invalid";

// ------------------------------------------------------------ helpers

fn sign_at(path: &str, ts: u64, body: &[u8]) -> String {
    Channel::Account
        .authorization_header(Some(&ACCOUNT_KEY), path, ts, body)
        .expect("signable")
}

fn sign(body: &[u8]) -> String {
    sign_at(OPERATION_PATH, NOW_S, body)
}

fn call<'a>(authorization: &'a str, body: &'a [u8]) -> CandidateCall<'a> {
    CandidateCall {
        method: "POST",
        path: OPERATION_PATH,
        authorization: Some(authorization),
        body,
        browser_context: false,
    }
}

fn keys() -> ChannelKeys<'static> {
    ChannelKeys {
        account: Some(&ACCOUNT_KEY),
        resolver: Some(&RESOLVER_KEY),
    }
}

fn auth(call: CandidateCall<'_>) -> Result<OperationRequest, OperationReply> {
    authorize(ACCOUNT, call, Some(&ACCOUNT_KEY), NOW_MS)
}

fn refused(call: CandidateCall<'_>) -> Option<OperationReply> {
    auth(call).err()
}

fn req_body(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn good() -> Value {
    json!({"account_id": ACCOUNT, "class": "stream",
           "operation": {"kind": "body", "ids": ARTICLE}})
}

fn stat_ids(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("segment-{i:04}-padding-padding@example.invalid"))
        .collect()
}

fn bad(body: &[u8]) -> bool {
    refused(call(&sign(body), body)) == Some(OperationReply::BadRequest)
}

// ----------------------------------------------------- authorize: auth

#[test]
fn valid_body_and_stat_signed_requests_are_accepted() {
    let body = req_body(&good());
    let request = auth(call(&sign(&body), &body)).expect("accepted");
    assert_eq!(request.account_id, ACCOUNT);
    assert_eq!(request.class, WorkClass::Stream);
    assert!(matches!(request.operation, Operation::Body(ref id) if id == ARTICLE));

    let stat = req_body(&json!({"account_id": ACCOUNT, "class": "ingest",
        "operation": {"kind": "stat", "ids": ["a@b", "c@d"]}}));
    let request = auth(call(&sign(&stat), &stat)).expect("accepted");
    assert_eq!(request.class, WorkClass::Ingest);
    assert!(matches!(request.operation, Operation::Stat(ref ids) if ids.len() == 2));

    // Leading JSON whitespace is allowed; the first significant byte is `{`.
    let spaced = [b" \n".as_slice(), &body].concat();
    assert!(auth(call(&sign(&spaced), &spaced)).is_ok());
}

#[test]
fn large_stat_batch_over_512_bytes_is_accepted_only_on_the_operation_path() {
    let ids = stat_ids(500);
    let body = req_body(&json!({"account_id": ACCOUNT, "class": "stream",
        "operation": {"kind": "stat", "ids": ids}}));
    assert!(body.len() > pool_channel::MAX_REQUEST_BYTES);
    assert!(body.len() <= pool_channel::MAX_OPERATION_BYTES);
    let request = auth(call(&sign(&body), &body)).expect("accepted");
    assert!(matches!(request.operation, Operation::Stat(ref v) if v.len() == 500));

    // The candidate path stays at the original 512-byte limit: no signature
    // can be made, and the operation signature does not verify there.
    assert!(Channel::Account
        .authorization_header(Some(&ACCOUNT_KEY), CANDIDATE_PATH, NOW_S, &body)
        .is_none());
    let header = sign(&body);
    let wrong_path = CandidateCall {
        path: CANDIDATE_PATH,
        ..call(&header, &body)
    };
    assert_eq!(refused(wrong_path), Some(OperationReply::NotFound));

    // Over the operation ceiling is refused even with a header for a smaller body.
    let huge = vec![b' '; pool_channel::MAX_OPERATION_BYTES + 1];
    assert!(Channel::Account
        .authorization_header(Some(&ACCOUNT_KEY), OPERATION_PATH, NOW_S, &huge)
        .is_none());
    assert_eq!(
        refused(call(&sign(&body), &huge)),
        Some(OperationReply::NotFound)
    );
}

#[test]
fn authentication_refusal_matrix_is_not_found() {
    let body = req_body(&good());
    let header = sign(&body);
    let nf = Some(OperationReply::NotFound);

    for method in ["GET", "PUT", "DELETE", "HEAD", "post", ""] {
        let c = CandidateCall {
            method,
            ..call(&header, &body)
        };
        assert_eq!(refused(c), nf, "method {method:?}");
    }
    for path in [
        "/candidate",
        "/operation/",
        "/Operation",
        "/operation?x=1",
        "",
        "/internal/v1/pool-providers",
    ] {
        let c = CandidateCall {
            path,
            ..call(&header, &body)
        };
        assert_eq!(refused(c), nf, "path {path:?}");
    }
    let c = CandidateCall {
        browser_context: true,
        ..call(&header, &body)
    };
    assert_eq!(refused(c), nf, "browser context");

    // Missing / malformed header.
    let none = CandidateCall {
        authorization: None,
        ..call(&header, &body)
    };
    assert_eq!(refused(none), nf);
    for garbage in [
        "",
        "PoolAccount",
        "PoolAccount 123",
        "PoolAccount x.y",
        "Bearer abc",
    ] {
        assert_eq!(refused(call(garbage, &body)), nf, "header {garbage:?}");
    }

    // Cross-channel: resolver scheme, resolver key, and a header signed for
    // the candidate path.
    let renamed = header.replacen("PoolAccount", "PoolResolver", 1);
    assert_eq!(refused(call(&renamed, &body)), nf);
    let other_key = Channel::Account
        .authorization_header(Some(&RESOLVER_KEY), OPERATION_PATH, NOW_S, &body)
        .unwrap();
    assert_eq!(refused(call(&other_key, &body)), nf);
    let small = b"{}";
    let candidate_sig = sign_at(CANDIDATE_PATH, NOW_S, small);
    assert_eq!(refused(call(&candidate_sig, small)), nf);
    let candidate_sig = sign_at(CANDIDATE_PATH, NOW_S, &body[..0]);
    assert_eq!(refused(call(&candidate_sig, &body)), nf);

    // Tampered body (one byte changed after signing).
    let mut tampered = body.clone();
    let last = tampered.len() - 2;
    tampered[last] ^= 1;
    assert_eq!(refused(call(&header, &tampered)), nf);

    // Timestamp skew: 30 s either way is accepted, 31 s is not.
    for ts in [NOW_S - 30, NOW_S + 30] {
        let h = sign_at(OPERATION_PATH, ts, &body);
        assert!(auth(call(&h, &body)).is_ok(), "ts {ts}");
    }
    for ts in [NOW_S - 31, NOW_S + 31, 0] {
        let h = sign_at(OPERATION_PATH, ts, &body);
        assert_eq!(refused(call(&h, &body)), nf, "ts {ts}");
    }
}

#[test]
fn missing_or_short_account_key_is_not_found() {
    let body = req_body(&good());
    let header = sign(&body);
    for key in [None, Some(&ACCOUNT_KEY[..31]), Some(&b""[..])] {
        assert_eq!(
            authorize(ACCOUNT, call(&header, &body), key, NOW_MS).err(),
            Some(OperationReply::NotFound)
        );
    }
}

// --------------------------------------------- authorize: body shape

#[test]
fn structural_and_field_validation_is_bad_request() {
    let long_id = format!("{}@b", "a".repeat(249)); // 251 bytes
    let max_id = format!("{}@b", "a".repeat(248)); // 250 bytes
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("array root", b"[]".to_vec()),
        (
            "tuple array root",
            br#"[ "12345678-1234-4234-8234-123456789abc", "stream", {} ]"#.to_vec(),
        ),
        ("empty body", Vec::new()),
        ("null", b"null".to_vec()),
        ("string root", b"\"x\"".to_vec()),
        ("empty object", b"{}".to_vec()),
        ("trailing garbage", [req_body(&good()), b"x".to_vec()].concat()),
        (
            "unknown top-level field",
            req_body(&json!({"account_id": ACCOUNT, "class": "stream",
                "operation": {"kind": "body", "ids": ARTICLE}, "extra": 1})),
        ),
        (
            "unknown operation field",
            req_body(&json!({"account_id": ACCOUNT, "class": "stream",
                "operation": {"kind": "body", "ids": ARTICLE, "extra": 1}})),
        ),
        (
            "duplicate account_id",
            format!(
                r#"{{"account_id":"{ACCOUNT}","account_id":"{ACCOUNT}","class":"stream","operation":{{"kind":"body","ids":"{ARTICLE}"}}}}"#
            )
            .into_bytes(),
        ),
        (
            "duplicate class",
            format!(
                r#"{{"account_id":"{ACCOUNT}","class":"stream","class":"ingest","operation":{{"kind":"body","ids":"{ARTICLE}"}}}}"#
            )
            .into_bytes(),
        ),
        (
            "duplicate operation",
            format!(
                r#"{{"account_id":"{ACCOUNT}","class":"stream","operation":{{"kind":"body","ids":"{ARTICLE}"}},"operation":{{"kind":"body","ids":"{ARTICLE}"}}}}"#
            )
            .into_bytes(),
        ),
        (
            "duplicate kind",
            format!(
                r#"{{"account_id":"{ACCOUNT}","class":"stream","operation":{{"kind":"body","kind":"body","ids":"{ARTICLE}"}}}}"#
            )
            .into_bytes(),
        ),
        (
            "duplicate ids",
            format!(
                r#"{{"account_id":"{ACCOUNT}","class":"stream","operation":{{"kind":"body","ids":"{ARTICLE}","ids":"{ARTICLE}"}}}}"#
            )
            .into_bytes(),
        ),
        (
            "missing account_id",
            req_body(&json!({"class": "stream", "operation": {"kind": "body", "ids": ARTICLE}})),
        ),
        (
            "missing class",
            req_body(&json!({"account_id": ACCOUNT, "operation": {"kind": "body", "ids": ARTICLE}})),
        ),
        (
            "missing operation",
            req_body(&json!({"account_id": ACCOUNT, "class": "stream"})),
        ),
        (
            "operation as tuple array",
            req_body(&json!({"account_id": ACCOUNT, "class": "stream",
                "operation": ["body", ARTICLE]})),
        ),
        (
            "operation as string",
            req_body(&json!({"account_id": ACCOUNT, "class": "stream", "operation": "body"})),
        ),
        (
            "other account id",
            req_body(&json!({"account_id": OTHER_ACCOUNT, "class": "stream",
                "operation": {"kind": "body", "ids": ARTICLE}})),
        ),
        (
            "uppercase account id",
            req_body(&json!({"account_id": ACCOUNT.to_uppercase(), "class": "stream",
                "operation": {"kind": "body", "ids": ARTICLE}})),
        ),
        (
            "unknown class",
            req_body(&json!({"account_id": ACCOUNT, "class": "admin",
                "operation": {"kind": "body", "ids": ARTICLE}})),
        ),
        (
            "capitalised class",
            req_body(&json!({"account_id": ACCOUNT, "class": "Stream",
                "operation": {"kind": "body", "ids": ARTICLE}})),
        ),
        (
            "unknown op",
            req_body(&json!({"account_id": ACCOUNT, "class": "stream",
                "operation": {"kind": "post", "ids": ARTICLE}})),
        ),
        (
            "body id as array",
            req_body(&json!({"account_id": ACCOUNT, "class": "stream",
                "operation": {"kind": "body", "ids": [ARTICLE]}})),
        ),
        (
            "stat ids as string",
            req_body(&json!({"account_id": ACCOUNT, "class": "stream",
                "operation": {"kind": "stat", "ids": ARTICLE}})),
        ),
        (
            "stat empty",
            req_body(&json!({"account_id": ACCOUNT, "class": "stream",
                "operation": {"kind": "stat", "ids": []}})),
        ),
        (
            "stat 501 ids",
            req_body(&json!({"account_id": ACCOUNT, "class": "stream",
                "operation": {"kind": "stat", "ids": stat_ids(501)}})),
        ),
        (
            "stat one invalid id",
            req_body(&json!({"account_id": ACCOUNT, "class": "stream",
                "operation": {"kind": "stat", "ids": ["a@b", "nope"]}})),
        ),
    ];
    for (label, body) in &cases {
        assert!(bad(body), "{label} should be BadRequest");
    }

    for id in [
        "",
        "noat",
        "@b",
        "a@",
        "a@b@c",
        "<a@b>",
        "a b@c",
        "a\u{e9}@b",
        "a\n@b",
        long_id.as_str(),
    ] {
        for op in [
            json!({"kind": "body", "ids": id}),
            json!({"kind": "stat", "ids": [id]}),
        ] {
            let body =
                req_body(&json!({"account_id": ACCOUNT, "class": "stream", "operation": op}));
            assert!(bad(&body), "id {id:?}");
        }
    }

    // Boundary-valid ids and batch size are accepted.
    for op in [
        json!({"kind": "body", "ids": max_id}),
        json!({"kind": "stat", "ids": stat_ids(500)}),
        json!({"kind": "stat", "ids": ["a@b"]}),
    ] {
        let body = req_body(&json!({"account_id": ACCOUNT, "class": "stream", "operation": op}));
        assert!(auth(call(&sign(&body), &body)).is_ok());
    }
}

#[test]
fn owner_account_that_is_not_uuid_shaped_is_refused_even_when_it_matches() {
    let id = "not-a-uuid";
    let body = req_body(&json!({"account_id": id, "class": "stream",
        "operation": {"kind": "body", "ids": ARTICLE}}));
    let header = sign(&body);
    assert_eq!(
        authorize(id, call(&header, &body), Some(&ACCOUNT_KEY), NOW_MS).err(),
        Some(OperationReply::BadRequest)
    );
}

#[test]
fn authentication_precedes_shape_checks() {
    // A malformed body with a bad signature is indistinguishable from any
    // other unauthenticated call.
    let body = b"[]";
    let header = sign(b"{}");
    assert_eq!(refused(call(&header, body)), Some(OperationReply::NotFound));
}

// ------------------------------------------------- dispatch (fakes)

#[derive(Default)]
struct World {
    registry_calls: Cell<u32>,
    dial_calls: Cell<u32>,
    scheduler_calls: Cell<u32>,
    opens: RefCell<Vec<(SocketAddr, String)>>,
    loads: Cell<u32>,
    saves: Cell<u32>,
    written: RefCell<Vec<String>>,
    replies: RefCell<VecDeque<Vec<u8>>>,
    journal: RefCell<Option<AccountJournal>>,
    closed: Cell<bool>,
}

impl World {
    fn side_effects(&self) -> usize {
        self.registry_calls.get() as usize
            + self.dial_calls.get() as usize
            + self.scheduler_calls.get() as usize
            + self.opens.borrow().len()
            + self.loads.get() as usize
            + self.saves.get() as usize
            + self.written.borrow().len()
    }
}

#[derive(Clone)]
struct Fake(Rc<World>);

impl RegistrySource for Fake {
    async fn fetch(&self, authorization: &str) -> Result<Vec<u8>, RegistryError> {
        self.0.registry_calls.set(self.0.registry_calls.get() + 1);
        pool_channel::verify(
            Some(&RESOLVER_KEY),
            pool_channel::REGISTRY_PATH,
            Some(authorization),
            b"{}",
            NOW_S,
        )
        .map_err(|_| RegistryError)?;
        let config = AccountConfig {
            id: ACCOUNT.into(),
            revision: 3,
            secret_ref: SECRET.into(),
            can_read: true,
            max_connections: 2,
            stream_reserve: 1,
            ingest_limit: 1,
            tier: 1,
            enabled: true,
        };
        Ok(serde_json::to_vec(&json!({ "accounts": [config] })).unwrap())
    }
}

impl DialSource for Fake {
    async fn resolve(
        &self,
        authorization: &str,
        body: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, OperationUnavailable> {
        self.0.dial_calls.set(self.0.dial_calls.get() + 1);
        pool_channel::verify(
            Some(&RESOLVER_KEY),
            pool_channel::RESOLVE_PATH,
            Some(authorization),
            body,
            NOW_S,
        )
        .map_err(|_| OperationUnavailable)?;
        Ok(Zeroizing::new(
            serde_json::to_vec(&json!({
                "provider_id": ACCOUNT, "revision": 3, "secret_ref": SECRET,
                "host": "news.example.invalid", "port": 563, "tls": true,
                "peer": "93.184.216.34", "expires_at": NOW_S + 3,
                "username": "synthetic-user", "password": "synthetic-pass",
            }))
            .unwrap(),
        ))
    }
}

impl CleanupScheduler for Fake {
    async fn ensure_cleanup(&self) -> Result<(), OperationUnavailable> {
        self.0.scheduler_calls.set(self.0.scheduler_calls.get() + 1);
        Ok(())
    }
}

impl AccountStorage for Fake {
    async fn load(&mut self) -> Result<Option<AccountJournal>, StorageError> {
        self.0.loads.set(self.0.loads.get() + 1);
        Ok(self.0.journal.borrow().clone())
    }

    async fn save(&mut self, journal: &AccountJournal) -> Result<(), StorageError> {
        self.0.saves.set(self.0.saves.get() + 1);
        *self.0.journal.borrow_mut() = Some(journal.clone());
        Ok(())
    }
}

#[derive(Clone)]
struct Ctl(Rc<World>);

impl CloseControl for Ctl {
    async fn close(&self) -> Result<(), CloseError> {
        self.0.closed.set(true);
        Ok(())
    }

    fn confirmed_closed(&self) -> bool {
        self.0.closed.get()
    }
}

struct Wire {
    world: Rc<World>,
    inbox: VecDeque<u8>,
}

impl Transport for Wire {
    async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.inbox.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "script"));
        }
        let n = buf.len().min(self.inbox.len());
        for (slot, byte) in buf.iter_mut().zip(self.inbox.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }

    async fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        let line = String::from_utf8_lossy(buf).trim_end().to_owned();
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
            let greeting = world.replies.borrow_mut().pop_front().unwrap_or_default();
            Ok(Wire {
                world,
                inbox: greeting.into(),
            })
        }))
    }
}

type Owner = AccountOwner<Fake, Ctl>;

fn harness(tail: &[String]) -> (Fake, Owner) {
    let world = World::default();
    let mut replies: VecDeque<Vec<u8>> = [
        "200 ready\r\n",
        "200 reader\r\n",
        "381 pass\r\n",
        "281 ok\r\n",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();
    replies.extend(tail.iter().map(|s| s.as_bytes().to_vec()));
    *world.replies.borrow_mut() = replies;
    let fake = Fake(Rc::new(world));
    let owner = Owner::new(ACCOUNT.into(), fake.clone()).unwrap();
    (fake, owner)
}

async fn go(fake: &Fake, owner: &Owner, call: CandidateCall<'_>) -> OperationReply {
    dispatch(
        owner,
        fake,
        fake,
        fake,
        call,
        keys(),
        || NOW_MS,
        pending::<()>(),
    )
    .await
}

#[tokio::test]
async fn refused_calls_cause_no_registry_storage_resolver_or_connector_access() {
    let (fake, owner) = harness(&[]);
    let body = req_body(&good());
    let header = sign(&body);

    let cross = sign_at(CANDIDATE_PATH, NOW_S, b"{}");
    let wrong_account = req_body(&json!({"account_id": OTHER_ACCOUNT, "class": "stream",
        "operation": {"kind": "body", "ids": ARTICLE}}));
    let wrong_account_header = sign(&wrong_account);
    let unknown = req_body(&json!({"account_id": ACCOUNT, "class": "stream",
        "operation": {"kind": "body", "ids": ARTICLE}, "x": 1}));
    let unknown_header = sign(&unknown);
    let stale = sign_at(OPERATION_PATH, NOW_S - 60, &body);

    let cases = vec![
        (
            "get",
            CandidateCall {
                method: "GET",
                ..call(&header, &body)
            },
            OperationReply::NotFound,
        ),
        (
            "path",
            CandidateCall {
                path: CANDIDATE_PATH,
                ..call(&header, &body)
            },
            OperationReply::NotFound,
        ),
        (
            "browser",
            CandidateCall {
                browser_context: true,
                ..call(&header, &body)
            },
            OperationReply::NotFound,
        ),
        (
            "cross-channel",
            call(&cross, &body),
            OperationReply::NotFound,
        ),
        ("stale", call(&stale, &body), OperationReply::NotFound),
        (
            "unsigned",
            CandidateCall {
                authorization: None,
                ..call(&header, &body)
            },
            OperationReply::NotFound,
        ),
        (
            "wrong account",
            call(&wrong_account_header, &wrong_account),
            OperationReply::BadRequest,
        ),
        (
            "unknown field",
            call(&unknown_header, &unknown),
            OperationReply::BadRequest,
        ),
    ];
    for (label, c, expected) in cases {
        assert_eq!(go(&fake, &owner, c).await, expected, "{label}");
        assert_eq!(fake.0.side_effects(), 0, "{label}: side effects");
    }

    // Missing account key: nothing is touched either.
    let reply = dispatch(
        &owner,
        &fake,
        &fake,
        &fake,
        call(&header, &body),
        ChannelKeys {
            account: None,
            resolver: Some(&RESOLVER_KEY),
        },
        || NOW_MS,
        pending::<()>(),
    )
    .await;
    assert_eq!(reply, OperationReply::NotFound);
    assert_eq!(fake.0.side_effects(), 0);
}

#[tokio::test]
async fn valid_body_request_runs_end_to_end_with_the_admitted_account() {
    let reply = format!("222 0 <{ARTICLE}>\r\nhello\r\n.\r\n");
    let (fake, owner) = harness(&[reply]);
    let body = req_body(&good());
    let header = sign(&body);
    let got = go(&fake, &owner, call(&header, &body)).await;
    assert_eq!(
        got,
        OperationReply::Served(Served {
            provider_id: ACCOUNT.into(),
            output: Output::Body(b"hello\r\n".to_vec()),
        })
    );
    assert_eq!(fake.0.registry_calls.get(), 1);
    assert_eq!(fake.0.dial_calls.get(), 1);
    assert_eq!(fake.0.opens.borrow().len(), 1);
    assert_eq!(fake.0.opens.borrow()[0].1, "news.example.invalid");
    assert!(fake.0.closed.get(), "physical close before output escapes");
}

#[tokio::test]
async fn valid_large_stat_batch_runs_end_to_end() {
    let ids = stat_ids(20);
    let mut tail = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        tail.push(if i % 2 == 0 {
            format!("223 0 <{id}>\r\n")
        } else {
            "430 no such article\r\n".to_string()
        });
    }
    let (fake, owner) = harness(&tail);
    let body = req_body(&json!({"account_id": ACCOUNT, "class": "ingest",
        "operation": {"kind": "stat", "ids": ids}}));
    assert!(body.len() > pool_channel::MAX_REQUEST_BYTES);
    let header = sign(&body);
    let got = go(&fake, &owner, call(&header, &body)).await;
    let present: Vec<String> = ids.iter().step_by(2).cloned().collect();
    let missing: Vec<String> = ids.iter().skip(1).step_by(2).cloned().collect();
    assert_eq!(
        got,
        OperationReply::Served(Served {
            provider_id: ACCOUNT.into(),
            output: Output::Stat { present, missing },
        })
    );
}

#[tokio::test]
async fn provider_missing_article_is_a_typed_failure() {
    let (fake, owner) = harness(&["430 no such article\r\n".to_string()]);
    let body = req_body(&good());
    let header = sign(&body);
    assert_eq!(
        go(&fake, &owner, call(&header, &body)).await,
        OperationReply::Failed(OperationError::Missing)
    );
    assert!(fake.0.closed.get());
}

// ------------------------------------------------- response codec

use nntune_stream::account_dispatch::{decode_response, PROVIDER_HEADER};

const GENERIC_ERROR: &[u8] = br#"{"error":{"code":"account_operation_failed"}}"#;

fn decode(status: u16, ct: &str, provider: Option<&str>, body: &[u8]) -> OperationReply {
    decode_response(status, ct, provider, body.to_vec())
}

fn unavailable() -> OperationReply {
    OperationReply::Failed(OperationError::Unavailable)
}

#[test]
fn binary_body_round_trips_without_json_encoding() {
    assert_eq!(PROVIDER_HEADER, "x-nntune-pool-provider");
    let bytes: Vec<u8> = (0..=255).collect();
    let response = OperationReply::Served(Served {
        provider_id: ACCOUNT.into(),
        output: Output::Body(bytes.clone()),
    })
    .into_response();
    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, "application/octet-stream");
    assert_eq!(response.provider_id.as_deref(), Some(ACCOUNT));
    assert_eq!(response.body, bytes);
    assert_eq!(
        decode_response(
            response.status,
            response.content_type,
            response.provider_id.as_deref(),
            response.body
        ),
        OperationReply::Served(Served {
            provider_id: ACCOUNT.into(),
            output: Output::Body(bytes),
        })
    );
    // An empty article is a valid body.
    assert!(matches!(
        decode(200, "application/octet-stream", Some(ACCOUNT), b""),
        OperationReply::Served(_)
    ));
}

#[test]
fn stat_round_trips_as_a_json_object() {
    let output = Output::Stat {
        present: vec!["a@b".into(), "a@b".into()],
        missing: vec!["c@d".into()],
    };
    let response = OperationReply::Served(Served {
        provider_id: ACCOUNT.into(),
        output,
    })
    .into_response();
    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, "application/json");
    assert_eq!(
        serde_json::from_slice::<Value>(&response.body).unwrap(),
        json!({"present": ["a@b", "a@b"], "missing": ["c@d"]})
    );
    assert_eq!(
        decode_response(200, response.content_type, Some(ACCOUNT), response.body),
        OperationReply::Served(Served {
            provider_id: ACCOUNT.into(),
            output: Output::Stat {
                present: vec!["a@b".into(), "a@b".into()],
                missing: vec!["c@d".into()],
            },
        })
    );
}

#[test]
fn failures_encode_generic_status_without_provider_or_text() {
    let cases = [
        (OperationReply::NotFound, 404),
        (OperationReply::BadRequest, 400),
        (OperationReply::Failed(OperationError::Invalid), 400),
        (OperationReply::Failed(OperationError::Unavailable), 503),
        (OperationReply::Failed(OperationError::Deadline), 504),
        (OperationReply::Failed(OperationError::Missing), 410),
        (OperationReply::Failed(OperationError::TooLarge), 413),
    ];
    for (reply, status) in cases {
        let label = format!("{reply:?}");
        let response = reply.into_response();
        assert_eq!(response.status, status, "{label}");
        assert_eq!(response.content_type, "application/json", "{label}");
        assert!(response.provider_id.is_none(), "{label}");
        assert_eq!(response.body, GENERIC_ERROR, "{label}");
    }
}

#[test]
fn error_statuses_decode_to_typed_failures_ignoring_body_and_provider() {
    let hostile = b"provider said: password=hunter2";
    let expected = [
        (400, OperationReply::BadRequest),
        (404, OperationReply::NotFound),
        (410, OperationReply::Failed(OperationError::Missing)),
        (413, OperationReply::Failed(OperationError::TooLarge)),
        (504, OperationReply::Failed(OperationError::Deadline)),
        (503, unavailable()),
        (500, unavailable()),
        (302, unavailable()),
        (201, unavailable()),
        (204, unavailable()),
        (206, unavailable()),
        (0, unavailable()),
    ];
    for (status, reply) in expected {
        assert_eq!(
            decode(status, "application/json", Some(ACCOUNT), hostile),
            reply,
            "status {status}"
        );
    }
}

#[test]
fn success_requires_uuid_shaped_provider_identity_and_exact_content_type() {
    let body = br#"{"present":[],"missing":[]}"#;
    for provider in [
        None,
        Some(""),
        Some("not-a-uuid"),
        Some(&ACCOUNT.to_uppercase()[..]),
        Some("12345678-1234-4234-8234-123456789ab"),
    ] {
        assert_eq!(
            decode(200, "application/json", provider, body),
            unavailable(),
            "provider {provider:?}"
        );
        assert_eq!(
            decode(200, "application/octet-stream", provider, b"x"),
            unavailable()
        );
    }
    for ct in [
        "",
        "text/plain",
        "application/json; charset=utf-8",
        "Application/JSON",
        "application/octet-stream; x=1",
        "application/xml",
    ] {
        assert_eq!(
            decode(200, ct, Some(ACCOUNT), body),
            unavailable(),
            "content type {ct:?}"
        );
    }
    // A JSON body under the binary type (and vice versa) is just opaque bytes
    // or a parse failure, never reinterpreted.
    assert!(matches!(
        decode(200, "application/octet-stream", Some(ACCOUNT), body),
        OperationReply::Served(Served {
            output: Output::Body(_),
            ..
        })
    ));
    assert_eq!(
        decode(200, "application/json", Some(ACCOUNT), b"binary\x00"),
        unavailable()
    );
}

#[test]
fn malformed_stat_json_is_refused() {
    let long = format!("{}@b", "a".repeat(249));
    let cases: Vec<(&str, String)> = vec![
        ("empty", String::new()),
        ("array root", r#"[["a@b"],[]]"#.into()),
        ("tuple of arrays", r#"[[],[]]"#.into()),
        ("null", "null".into()),
        ("truncated", r#"{"present":["a@b"]"#.into()),
        ("missing field", r#"{"present":[]}"#.into()),
        (
            "unknown field",
            r#"{"present":[],"missing":[],"x":1}"#.into(),
        ),
        (
            "duplicate present",
            r#"{"present":[],"present":[],"missing":[]}"#.into(),
        ),
        (
            "duplicate missing",
            r#"{"present":[],"missing":[],"missing":[]}"#.into(),
        ),
        (
            "object rows",
            r#"{"present":[{"id":"a@b"}],"missing":[]}"#.into(),
        ),
        (
            "object lists",
            r#"{"present":{"a@b":1},"missing":[]}"#.into(),
        ),
        ("non-string id", r#"{"present":[1],"missing":[]}"#.into()),
        ("invalid id", r#"{"present":["nope"],"missing":[]}"#.into()),
        (
            "bracketed id",
            r#"{"present":[],"missing":["<a@b>"]}"#.into(),
        ),
        ("empty id", r#"{"present":[""],"missing":[]}"#.into()),
        (
            "overlong id",
            format!(r#"{{"present":["{long}"],"missing":[]}}"#),
        ),
        ("trailing", r#"{"present":[],"missing":[]} x"#.into()),
    ];
    for (label, body) in cases {
        assert_eq!(
            decode(200, "application/json", Some(ACCOUNT), body.as_bytes()),
            unavailable(),
            "{label}"
        );
    }
    // Leading whitespace before the object is accepted.
    assert!(matches!(
        decode(
            200,
            "application/json",
            Some(ACCOUNT),
            b"\n {\"present\":[],\"missing\":[]}"
        ),
        OperationReply::Served(_)
    ));
}

#[test]
fn size_and_count_boundaries_are_enforced() {
    // 500 ids total is accepted; 501 is not.
    let list = |n: usize| -> Vec<u8> {
        let ids = stat_ids(n);
        serde_json::to_vec(&json!({"present": ids, "missing": []})).unwrap()
    };
    assert!(matches!(
        decode(200, "application/json", Some(ACCOUNT), &list(500)),
        OperationReply::Served(_)
    ));
    assert_eq!(
        decode(200, "application/json", Some(ACCOUNT), &list(501)),
        unavailable()
    );
    // The cap applies to present + missing together.
    let split =
        serde_json::to_vec(&json!({"present": stat_ids(250), "missing": stat_ids(251)})).unwrap();
    assert_eq!(
        decode(200, "application/json", Some(ACCOUNT), &split),
        unavailable()
    );
    // JSON beyond the operation ceiling is refused whatever it contains.
    let mut huge = br#"{"present":[],"missing":[]}"#.to_vec();
    huge.resize(pool_channel::MAX_OPERATION_BYTES + 1, b' ');
    assert_eq!(
        decode(200, "application/json", Some(ACCOUNT), &huge),
        unavailable()
    );
    // BODY: exactly the cap is fine, one byte over is not.
    let cap = nntune_stream::body::MAX_BODY_ARTICLE_BYTES;
    assert!(matches!(
        decode_response(200, "application/octet-stream", Some(ACCOUNT), vec![0; cap]),
        OperationReply::Served(_)
    ));
    assert_eq!(
        decode_response(
            200,
            "application/octet-stream",
            Some(ACCOUNT),
            vec![0; cap + 1]
        ),
        unavailable()
    );
}
