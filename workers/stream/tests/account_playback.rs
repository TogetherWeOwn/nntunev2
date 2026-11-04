//! Grant-first catalogue-selected playback article source
//! (`account_playback::fetch`).
//!
//! Synthetic fakes only: a catalogue, a registry source and an `AccountSource`
//! that record every call. No network, credentials or Durable Object. These are
//! native composition tests, NOT evidence of real Worker service-binding
//! behaviour or of a wired public route.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use nntune_core::pool_channel::{self, Channel};
use nntune_stream::account_dispatch::{OperationReply, OperationRequest};
use nntune_stream::account_operation::{OperationError, Output, Served};
use nntune_stream::account_playback::{
    fetch, ArticleRequest, CatalogueUnavailable, EncodedArticle, PlaybackCatalogue,
};
use nntune_stream::account_pool::{AccountConfig, WorkClass};
use nntune_stream::account_route::{ChannelKeys, RegistryError, RegistrySource, REGISTRY_BODY};
use nntune_stream::account_selection::AccountSource;
use nntune_stream::api_error::ApiError;
use nntune_stream::body::MAX_BODY_ARTICLE_BYTES;
use nntune_stream::grant;
use serde_json::{json, Value};

const ACCOUNT_KEY: [u8; 32] = [5; 32];
const RESOLVER_KEY: [u8; 32] = [9; 32];
const SECRET: &[u8] = b"playback-grant-secret";
const NOW_MS: u64 = 1_700_000_000_000;
const NOW_S: u64 = NOW_MS / 1000;
const ARTICLE: &str = "part3@example.invalid";
const TRACK: &str = "track-a";
const OTHER_TRACK: &str = "track-b";
const USER: &str = "user-1";
const PLAY: &str = "play-1";
const EPOCH: u64 = 7;
const SHORT: Duration = Duration::from_millis(40);
const GUARD: Duration = Duration::from_secs(10);

fn uid(n: u32) -> String {
    format!("{n:08x}-1234-4234-8234-123456789abc")
}

fn row(n: u32) -> AccountConfig {
    AccountConfig {
        id: uid(n),
        revision: 1,
        secret_ref: uid(0x1000 + n),
        can_read: true,
        max_connections: 4,
        stream_reserve: 1,
        ingest_limit: 2,
        tier: 0,
        enabled: true,
    }
}

fn hint(n: u32) -> Vec<u8> {
    serde_json::to_vec(&json!({"id": uid(n), "tier": 0, "in_flight": 0, "eligible": true})).unwrap()
}

/// Sets a flag when the owning future is dropped (cancellation witness).
struct DropFlag(Rc<Cell<bool>>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

// ------------------------------------------------------------ catalogue

#[derive(Clone)]
enum Lookup {
    Article(String),
    Missing,
    Error,
    Hang,
}

struct Catalogue {
    lookup: Lookup,
    calls: RefCell<Vec<(String, usize)>>,
    /// Added to the shared clock when called (a slow lookup).
    advance_ms: u64,
    clock: Rc<Cell<u64>>,
    dropped: Rc<Cell<bool>>,
}

impl Catalogue {
    fn new(lookup: Lookup, clock: &Rc<Cell<u64>>) -> Self {
        Self {
            lookup,
            calls: RefCell::new(Vec::new()),
            advance_ms: 0,
            clock: clock.clone(),
            dropped: Rc::new(Cell::new(false)),
        }
    }

    fn serving(clock: &Rc<Cell<u64>>) -> Self {
        Self::new(Lookup::Article(ARTICLE.into()), clock)
    }
}

impl PlaybackCatalogue for Catalogue {
    async fn article(
        &self,
        track_id: &str,
        index: usize,
    ) -> Result<Option<String>, CatalogueUnavailable> {
        self.calls.borrow_mut().push((track_id.into(), index));
        self.clock.set(self.clock.get() + self.advance_ms);
        match &self.lookup {
            Lookup::Article(id) => Ok(Some(id.clone())),
            Lookup::Missing => Ok(None),
            Lookup::Error => Err(CatalogueUnavailable),
            Lookup::Hang => {
                let _flag = DropFlag(self.dropped.clone());
                std::future::pending::<()>().await;
                unreachable!()
            }
        }
    }
}

// ----------------------------------------------------------------- fake

#[derive(Default)]
struct State {
    registry: RefCell<Option<Vec<u8>>>,
    registry_auth: RefCell<Vec<String>>,
    hints: RefCell<HashMap<String, Vec<u8>>>,
    candidate_calls: RefCell<Vec<(String, String, Vec<u8>)>>,
    execute_calls: RefCell<Vec<(String, String, Vec<u8>)>>,
    reply: RefCell<Option<OperationReply>>,
    bodies: RefCell<HashMap<String, OperationReply>>,
    after_execute: RefCell<Option<Box<dyn Fn()>>>,
    execute_hangs: Cell<bool>,
    execute_dropped: Rc<Cell<bool>>,
}

#[derive(Clone, Default)]
struct Fake(Rc<State>);

impl RegistrySource for Fake {
    async fn fetch(&self, authorization: &str) -> Result<Vec<u8>, RegistryError> {
        self.0.registry_auth.borrow_mut().push(authorization.into());
        self.0.registry.borrow().clone().ok_or(RegistryError)
    }
}

impl AccountSource for Fake {
    async fn candidate(
        &self,
        account_id: &str,
        authorization: &str,
        body: &[u8],
    ) -> Result<Vec<u8>, RegistryError> {
        self.0.candidate_calls.borrow_mut().push((
            account_id.into(),
            authorization.into(),
            body.to_vec(),
        ));
        self.0
            .hints
            .borrow()
            .get(account_id)
            .cloned()
            .ok_or(RegistryError)
    }

    async fn execute(&self, account_id: &str, authorization: &str, body: &[u8]) -> OperationReply {
        self.0.execute_calls.borrow_mut().push((
            account_id.into(),
            authorization.into(),
            body.to_vec(),
        ));
        if self.0.execute_hangs.get() {
            let _flag = DropFlag(self.0.execute_dropped.clone());
            std::future::pending::<()>().await;
        }
        if let Some(after) = self.0.after_execute.borrow().as_ref() {
            after();
        }
        let request: OperationRequest = serde_json::from_slice(body).unwrap();
        if let nntune_stream::account_operation::Operation::Body(reference) = request.operation {
            if let Some(reply) = self.0.bodies.borrow_mut().remove(&reference) {
                return reply;
            }
        }
        self.0
            .reply
            .borrow_mut()
            .take()
            .unwrap_or(OperationReply::NotFound)
    }
}

impl Fake {
    fn new(rows: &[AccountConfig], hints: &[(u32, Vec<u8>)]) -> Self {
        let fake = Self::default();
        *fake.0.registry.borrow_mut() =
            Some(serde_json::to_vec(&json!({ "accounts": rows })).unwrap());
        for (n, raw) in hints {
            fake.0.hints.borrow_mut().insert(uid(*n), raw.clone());
        }
        fake
    }

    /// One healthy read account (id 1) that replies with `reply`.
    fn one(reply: OperationReply) -> Self {
        let fake = Self::new(&[row(1)], &[(1, hint(1))]);
        *fake.0.reply.borrow_mut() = Some(reply);
        fake
    }

    fn executes(&self) -> usize {
        self.0.execute_calls.borrow().len()
    }

    fn reads(&self) -> usize {
        self.0.registry_auth.borrow().len()
            + self.0.candidate_calls.borrow().len()
            + self.executes()
    }
}

fn served(n: u32, output: Output) -> OperationReply {
    OperationReply::Served(Served {
        provider_id: uid(n),
        output,
    })
}

// -------------------------------------------------------------- helpers

fn clock() -> Rc<Cell<u64>> {
    Rc::new(Cell::new(NOW_MS))
}

fn v2_token() -> String {
    grant::issue_v2(USER, TRACK, PLAY, EPOCH, NOW_S + 600, SECRET)
}

fn keys() -> ChannelKeys<'static> {
    ChannelKeys {
        account: Some(&ACCOUNT_KEY),
        resolver: Some(&RESOLVER_KEY),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_with(
    cat: &Catalogue,
    fake: &Fake,
    clk: &Rc<Cell<u64>>,
    secret: &[u8],
    token: &str,
    track: &str,
    index: usize,
    deadline: impl Future<Output = ()>,
) -> Result<EncodedArticle, ApiError> {
    let clk = clk.clone();
    tokio::time::timeout(
        GUARD,
        fetch(
            cat,
            fake,
            fake,
            keys(),
            secret,
            ArticleRequest {
                token,
                track_id: track,
                index,
            },
            move || clk.get(),
            deadline,
        ),
    )
    .await
    .expect("fetch must finish inside the test guard")
}

async fn run(
    cat: &Catalogue,
    fake: &Fake,
    clk: &Rc<Cell<u64>>,
    token: &str,
) -> Result<EncodedArticle, ApiError> {
    run_with(
        cat,
        fake,
        clk,
        SECRET,
        token,
        TRACK,
        0,
        std::future::pending::<()>(),
    )
    .await
}

fn assert_no_reads(cat: &Catalogue, fake: &Fake) {
    assert!(cat.calls.borrow().is_empty(), "catalogue must not be read");
    assert_eq!(fake.reads(), 0, "registry/accounts must not be read");
}

fn generic_upstream(err: &ApiError) {
    assert_eq!(err, &ApiError::upstream_unavailable());
    assert_eq!(
        err.body_json(),
        ApiError::upstream_unavailable().body_json()
    );
}

// ------------------------------------------------------- grant first

#[tokio::test]
async fn bad_absent_expired_bad_signature_and_v1_grants_touch_nothing() {
    let valid = v2_token();
    let (head, sig) = valid.rsplit_once('.').unwrap();
    let flipped = if sig.starts_with('A') { 'B' } else { 'A' };
    let bad_sig = format!("{head}.{flipped}{}", &sig[1..]);
    let cases: Vec<(&str, String)> = vec![
        ("absent", String::new()),
        ("garbage", "not-a-token".into()),
        ("unknown version", "v3.abc.def".into()),
        ("truncated", valid[..valid.len() / 2].into()),
        ("bad signature", bad_sig),
        (
            "wrong secret",
            grant::issue_v2(USER, TRACK, PLAY, EPOCH, NOW_S + 600, b"other-secret"),
        ),
        (
            "expired",
            grant::issue_v2(USER, TRACK, PLAY, EPOCH, NOW_S - 1000, SECRET),
        ),
        ("v1", grant::issue_at(USER, TRACK, NOW_S, 600, SECRET)),
    ];
    for (name, token) in cases {
        let clk = clock();
        let cat = Catalogue::serving(&clk);
        let fake = Fake::one(served(1, Output::Body(b"x".to_vec())));
        let err = run(&cat, &fake, &clk, &token).await.err().expect(name);
        assert_eq!(err, ApiError::unauthorized(), "{name}");
        assert_no_reads(&cat, &fake);
    }
}

#[tokio::test]
async fn invalid_grant_is_rejected_even_when_the_deadline_is_already_ready() {
    let clk = clock();
    let cat = Catalogue::serving(&clk);
    let fake = Fake::one(served(1, Output::Body(b"x".to_vec())));
    let err = run_with(&cat, &fake, &clk, SECRET, "bad", TRACK, 0, async {})
        .await
        .err()
        .unwrap();
    assert_eq!(err, ApiError::unauthorized());
    assert_no_reads(&cat, &fake);
}

#[tokio::test]
async fn v1_for_another_track_is_unauthorized_not_forbidden() {
    let clk = clock();
    let cat = Catalogue::serving(&clk);
    let fake = Fake::one(served(1, Output::Body(b"x".to_vec())));
    let token = grant::issue_at(USER, OTHER_TRACK, NOW_S, 600, SECRET);
    let err = run(&cat, &fake, &clk, &token).await.err().unwrap();
    assert_eq!(err, ApiError::unauthorized());
    assert_no_reads(&cat, &fake);
}

#[tokio::test]
async fn v2_for_another_track_is_forbidden_with_no_reads() {
    let clk = clock();
    let cat = Catalogue::serving(&clk);
    let fake = Fake::one(served(1, Output::Body(b"x".to_vec())));
    let token = grant::issue_v2(USER, OTHER_TRACK, PLAY, EPOCH, NOW_S + 600, SECRET);
    let err = run(&cat, &fake, &clk, &token).await.err().unwrap();
    assert_eq!(err, ApiError::forbidden());
    assert_no_reads(&cat, &fake);
}

#[tokio::test]
async fn empty_signing_secret_is_internal_error_even_for_an_empty_key_token() {
    let empty_signed = grant::issue_v2(USER, TRACK, PLAY, EPOCH, NOW_S + 600, b"");
    for token in [empty_signed, v2_token(), String::new()] {
        let clk = clock();
        let cat = Catalogue::serving(&clk);
        let fake = Fake::one(served(1, Output::Body(b"x".to_vec())));
        let err = run_with(
            &cat,
            &fake,
            &clk,
            b"",
            &token,
            TRACK,
            0,
            std::future::pending::<()>(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err, ApiError::internal_error());
        assert_no_reads(&cat, &fake);
    }
}

// -------------------------------------------------------------- success

#[tokio::test]
async fn success_signs_stream_work_and_retains_actual_account_bytes_and_meter_identity() {
    let clk = clock();
    let cat = Catalogue::serving(&clk);
    let payload = vec![0u8, 1, 2, 250, 255, b'\r', b'\n'];
    let fake = Fake::one(served(1, Output::Body(payload.clone())));
    let token = v2_token();
    let got = run_with(
        &cat,
        &fake,
        &clk,
        SECRET,
        &token,
        TRACK,
        4,
        std::future::pending::<()>(),
    )
    .await
    .expect("served");

    assert_eq!(got.bytes, payload);
    assert_eq!(got.provider_id, uid(1));
    assert_eq!(got.grant.user_id, USER);
    assert_eq!(got.grant.track_id, TRACK);
    assert_eq!(got.grant.play_id.as_deref(), Some(PLAY));
    assert_eq!(got.grant.lease_epoch, Some(EPOCH));
    assert_eq!(got.grant.exp_unix, NOW_S + 600);

    // Catalogue: exactly the requested track and index, once.
    assert_eq!(*cat.calls.borrow(), vec![(TRACK.to_string(), 4)]);

    // Registry: resolver channel, exact body.
    let reg = fake.0.registry_auth.borrow();
    assert_eq!(reg.len(), 1);
    assert_eq!(
        pool_channel::verify(
            Some(&RESOLVER_KEY),
            pool_channel::REGISTRY_PATH,
            Some(&reg[0]),
            REGISTRY_BODY,
            NOW_S
        ),
        Ok(())
    );

    // Candidate: account channel, stream class.
    let cands = fake.0.candidate_calls.borrow();
    assert_eq!(cands.len(), 1);
    let (account, authorization, body) = &cands[0];
    assert_eq!(account, &uid(1));
    assert_eq!(
        Channel::Account.verify(
            Some(&ACCOUNT_KEY),
            pool_channel::CANDIDATE_PATH,
            Some(authorization),
            body,
            NOW_S
        ),
        Ok(())
    );
    assert_eq!(
        serde_json::from_slice::<Value>(body).unwrap(),
        json!({"account_id": uid(1), "class": "stream"})
    );

    // Operation: account channel, stream class, catalogue-selected BODY.
    let execs = fake.0.execute_calls.borrow();
    assert_eq!(execs.len(), 1);
    let (account, authorization, body) = &execs[0];
    assert_eq!(account, &uid(1));
    assert_eq!(
        Channel::Account.verify(
            Some(&ACCOUNT_KEY),
            pool_channel::OPERATION_PATH,
            Some(authorization),
            body,
            NOW_S
        ),
        Ok(())
    );
    assert_eq!(
        serde_json::from_slice::<Value>(body).unwrap(),
        json!({"account_id": uid(1), "class": "stream",
               "operation": {"kind": "body", "ids": ARTICLE}})
    );
    let parsed: OperationRequest = serde_json::from_slice(body).unwrap();
    assert_eq!(parsed.class, WorkClass::Stream);

    // No user, play or token material reaches any account RPC.
    let mut rpc: Vec<String> = reg.clone();
    for (a, auth, b) in cands.iter().chain(execs.iter()) {
        rpc.push(a.clone());
        rpc.push(auth.clone());
        rpc.push(String::from_utf8_lossy(b).into_owned());
    }
    for secret in [USER, PLAY, token.as_str()] {
        assert!(
            rpc.iter().all(|s| !s.contains(secret)),
            "{secret} leaked into an account RPC"
        );
    }
}

#[tokio::test]
async fn read_account_with_zero_ingest_limit_still_serves_stream() {
    let clk = clock();
    let cat = Catalogue::serving(&clk);
    let mut only = row(1);
    only.ingest_limit = 0;
    let fake = Fake::new(&[only], &[(1, hint(1))]);
    *fake.0.reply.borrow_mut() = Some(served(1, Output::Body(b"ok".to_vec())));
    let got = run(&cat, &fake, &clk, &v2_token()).await.ok().unwrap();
    assert_eq!(got.bytes, b"ok");
    assert_eq!(got.provider_id, uid(1));
    assert_eq!(fake.executes(), 1);
}

// ------------------------------------------------------------ catalogue

#[tokio::test]
async fn missing_error_or_invalid_catalogue_id_never_touches_the_pool() {
    let long = format!("{}@b", "a".repeat(600));
    let lookups = [
        Lookup::Missing,
        Lookup::Error,
        Lookup::Article(String::new()),
        Lookup::Article("no-at-sign".into()),
        Lookup::Article("<a@example.invalid>".into()),
        Lookup::Article("has space@example.invalid".into()),
        Lookup::Article("a@b@c".into()),
        Lookup::Article("a\r\nQUIT@example.invalid".into()),
        Lookup::Article(long),
    ];
    for lookup in lookups {
        let clk = clock();
        let cat = Catalogue::new(lookup, &clk);
        let fake = Fake::one(served(1, Output::Body(b"x".to_vec())));
        let err = run(&cat, &fake, &clk, &v2_token()).await.err().unwrap();
        generic_upstream(&err);
        assert_eq!(cat.calls.borrow().len(), 1);
        assert_eq!(fake.reads(), 0, "pool touched");
    }
}

#[tokio::test]
async fn catalogue_receives_the_exact_requested_track_and_index() {
    for index in [0usize, 1, 17, usize::MAX] {
        let clk = clock();
        let cat = Catalogue::serving(&clk);
        let fake = Fake::one(served(1, Output::Body(vec![])));
        run_with(
            &cat,
            &fake,
            &clk,
            SECRET,
            &v2_token(),
            TRACK,
            index,
            std::future::pending::<()>(),
        )
        .await
        .ok()
        .unwrap();
        assert_eq!(*cat.calls.borrow(), vec![(TRACK.to_string(), index)]);
    }
}

#[tokio::test]
async fn grant_expiring_during_a_slow_catalogue_blocks_the_pool() {
    let clk = clock();
    let mut cat = Catalogue::serving(&clk);
    cat.advance_ms = 2_000_000; // far past exp (600s) plus skew
    let fake = Fake::one(served(1, Output::Body(b"x".to_vec())));
    let err = run(&cat, &fake, &clk, &v2_token()).await.err().unwrap();
    assert_eq!(err, ApiError::unauthorized());
    assert_eq!(cat.calls.borrow().len(), 1);
    assert_eq!(fake.reads(), 0, "expired grant reached the pool");
}

// ------------------------------------------------------------ operation

#[tokio::test]
async fn every_operation_error_is_generic_upstream_with_a_single_attempt() {
    let errors = [
        OperationError::Invalid,
        OperationError::Unavailable,
        OperationError::Deadline,
        OperationError::Missing,
        OperationError::TooLarge,
    ];
    let mut replies: Vec<(String, OperationReply)> = errors
        .into_iter()
        .map(|e| (format!("{e:?}"), OperationReply::Failed(e)))
        .collect();
    replies.push(("not found".into(), OperationReply::NotFound));
    replies.push(("bad request".into(), OperationReply::BadRequest));
    for (name, reply) in replies {
        let clk = clock();
        let cat = Catalogue::serving(&clk);
        // A second healthy account must never be tried after a failure.
        let fake = Fake::new(&[row(1), row(2)], &[(1, hint(1)), (2, hint(2))]);
        *fake.0.reply.borrow_mut() = Some(reply);
        let err = run(&cat, &fake, &clk, &v2_token())
            .await
            .err()
            .expect(&name);
        generic_upstream(&err);
        assert_ne!(err, ApiError::provider_auth_expired(), "{name}");
        assert_eq!(fake.executes(), 1, "{name}: one attempt only");
        assert_eq!(cat.calls.borrow().len(), 1, "{name}");
    }
}

#[tokio::test]
async fn malformed_served_output_or_identity_is_generic_upstream_with_a_single_attempt() {
    let replies = vec![
        (
            "wrong output kind",
            served(
                1,
                Output::Stat {
                    present: vec![ARTICLE.into()],
                    missing: vec![],
                },
            ),
        ),
        ("identity mismatch", served(2, Output::Body(b"x".to_vec()))),
        ("unlisted identity", served(99, Output::Body(b"x".to_vec()))),
        (
            "oversized body",
            served(1, Output::Body(vec![0; MAX_BODY_ARTICLE_BYTES + 1])),
        ),
        (
            "empty identity",
            OperationReply::Served(Served {
                provider_id: String::new(),
                output: Output::Body(b"x".to_vec()),
            }),
        ),
    ];
    for (name, reply) in replies {
        let clk = clock();
        let cat = Catalogue::serving(&clk);
        let fake = Fake::one(reply);
        let err = run(&cat, &fake, &clk, &v2_token()).await.err().expect(name);
        generic_upstream(&err);
        assert_eq!(fake.executes(), 1, "{name}: one attempt only");
    }
}

// ------------------------------------------------------------- deadline

#[tokio::test]
async fn pending_catalogue_is_bounded_and_cancelled_by_the_deadline() {
    let clk = clock();
    let cat = Catalogue::new(Lookup::Hang, &clk);
    let fake = Fake::one(served(1, Output::Body(b"x".to_vec())));
    let err = run_with(
        &cat,
        &fake,
        &clk,
        SECRET,
        &v2_token(),
        TRACK,
        0,
        tokio::time::sleep(SHORT),
    )
    .await
    .err()
    .unwrap();
    generic_upstream(&err);
    assert_eq!(cat.calls.borrow().len(), 1);
    assert!(cat.dropped.get(), "catalogue lookup must be cancelled");
    assert_eq!(fake.reads(), 0);
}

#[tokio::test]
async fn pending_operation_is_bounded_and_cancelled_by_the_deadline() {
    let clk = clock();
    let cat = Catalogue::serving(&clk);
    let fake = Fake::one(served(1, Output::Body(b"x".to_vec())));
    fake.0.execute_hangs.set(true);
    let err = run_with(
        &cat,
        &fake,
        &clk,
        SECRET,
        &v2_token(),
        TRACK,
        0,
        tokio::time::sleep(SHORT),
    )
    .await
    .err()
    .unwrap();
    generic_upstream(&err);
    assert_eq!(fake.executes(), 1, "no retry after the deadline");
    assert!(fake.0.execute_dropped.get(), "operation must be cancelled");
}

// ------------------------------------------------------ route pin (read-only)

#[test]
fn playback_adapter_is_not_wired_into_any_runtime_or_route_source() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let runtime = fs::read_to_string(src.join("runtime.rs")).unwrap();
    assert!(
        runtime.contains("502 upstream_unavailable"),
        "runtime NNTP miss must stay disabled"
    );
    for entry in fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name == "account_playback.rs" || name == "lib.rs" {
            continue;
        }
        if path.extension().is_some_and(|e| e == "rs") {
            let text = fs::read_to_string(&path).unwrap();
            assert!(
                !text.contains("account_playback"),
                "{name} must not reference the unwired playback adapter"
            );
        }
    }
}

#[path = "account_playback/original_range.rs"]
mod original_range;
