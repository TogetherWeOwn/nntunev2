//! Acceptance tests: a request-body *transport* failure must be rejected
//! before handler dispatch — never converted into an empty or truncated
//! body that the handlers then act on.
//!
//! Defect being pinned (in the Worker entry's body read): a body-stream
//! setup error currently falls through as an empty body and a chunk error
//! breaks the read loop, dispatching whatever partial bytes arrived. An
//! empty body is *meaningful* downstream (stored-credential vault testing
//! dials the saved provider for an empty `POST /v1/vault/test` body), so a
//! swallowed transport failure can invoke the handler, store writes, the
//! mailer, or the vault dialer on input the client never successfully sent.
//! No production failure or exploit is claimed; the static control flow is
//! the defect.
//!
//! Test files only: no implementation code is touched here. The Builder
//! owns all runtime wiring and must never edit this file.
//!
//! Failing command (this card, RED until the seam below exists):
//!   `cargo test --locked --manifest-path workers/api/Cargo.toml \
//!     --test request_body_transport_acceptance`
//!
//! ## Minimal seam contract (Builder: implement exactly this surface)
//!
//! In `nntune_api::worker_entry`:
//!
//! ```ignore
//! /// Opaque chunk-read failure. Carries no message by construction, so no
//! /// internal/runtime error detail can ever reach a response body.
//! #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//! pub struct BodyReadError;
//!
//! /// One pollable source of body chunks (std-only so fake streams are
//! /// native-testable; the Worker adapter wraps the runtime byte stream
//! /// and maps every runtime chunk error to `BodyReadError`).
//! pub trait BodyChunkStream {
//!     fn poll_next_chunk(
//!         &mut self,
//!         cx: &mut std::task::Context<'_>,
//!     ) -> std::task::Poll<Option<Result<Vec<u8>, BodyReadError>>>;
//! }
//!
//! /// Where the body comes from, classified by the Worker entry using the
//! /// actual workers-rs contract:
//! /// - `Bodyless`: the edge request has no body at all
//! ///   (`req.inner().body().is_none()`) — a legitimate bodyless request
//! ///   (e.g. GET/DELETE, or a POST sent without a body).
//! /// - `SetupFailed`: a body IS present but obtaining its stream failed
//! ///   (`req.stream()` returned `Err`).
//! /// - `Stream(s)`: a body is present and its stream was obtained.
//! pub enum BodySource<S> {
//!     Bodyless,
//!     SetupFailed,
//!     Stream(S),
//! }
//!
//! /// The collection decision. Only `Complete` may reach the handlers.
//! #[derive(Debug, PartialEq, Eq)]
//! pub enum CollectedBody {
//!     Complete(Vec<u8>),
//!     Oversized,
//!     TransportFailure,
//! }
//!
//! /// Collect one request body with the existing hard cap
//! /// (`handlers::MAX_BODY_BYTES`, the single source of truth — no second
//! /// size constant). Rules:
//! /// - `declared_len` is the parsed `Content-Length` (`None` when absent
//! ///   or unparsable). `Some(len) if len > MAX` is `Oversized` and the
//! ///   stream must not be polled at all.
//! /// - `Bodyless` is `Complete(vec![])`; an empty stream is too (an
//! ///   explicitly sent empty body stays valid vault-test input).
//! /// - `SetupFailed` is `TransportFailure`.
//! /// - Any chunk `Err` is `TransportFailure`, however many valid bytes
//! ///   (even a complete valid JSON document) arrived before it. Partial
//! ///   bytes are dropped, never returned.
//! /// - Buffering is bounded: once the cap is exceeded the result is
//! ///   `Oversized` and collection stops pulling chunks.
//! /// - `Poll::Pending` is awaited, never treated as end-of-body.
//! pub async fn collect_body<S: BodyChunkStream>(
//!     declared_len: Option<usize>,
//!     source: BodySource<S>,
//! ) -> CollectedBody;
//!
//! /// The dispatch gate. `Complete(bytes)` replaces `request.body` with
//! /// exactly `bytes` and invokes `handler` once. `Oversized` and
//! /// `TransportFailure` invoke NOTHING and answer the handlers' existing
//! /// uniform pre-routing rejection: status 400, body
//! /// `{"error":"invalid request"}`, no `set_cookie`. The Worker entry
//! /// passes a handler that calls `handlers::handle`, so a rejected body
//! /// can cause no store, mailer, DNS, or dialer side effect.
//! pub async fn dispatch_collected<F, Fut>(
//!     collected: CollectedBody,
//!     request: handlers::Request,
//!     handler: F,
//! ) -> handlers::Response
//! where
//!     F: FnOnce(handlers::Request) -> Fut,
//!     Fut: std::future::Future<Output = handlers::Response>;
//! ```
//!
//! Wiring requirement: `worker_entry::dispatch` must reach the handlers
//! ONLY through `collect_body` + `dispatch_collected` (pinned below), so
//! the decision exercised here is the one the Worker actually uses.
//!
//! Fixtures are synthetic only; no DB, provider, or network is touched.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context as TaskContext, Poll};

use nntune_api::worker_entry::{
    collect_body, dispatch_collected, BodyChunkStream, BodyReadError, BodySource, CollectedBody,
};
use nntune_api::{auth, handlers, session, vault};

type SeenBodies = Rc<RefCell<Vec<(String, String, Vec<u8>)>>>;

// --- Constants -------------------------------------------------------------

const NOW: u64 = 1_700_000_000;
const RP: &str = "passkey.example.com";
const ORIGIN: &str = "https://passkey.example.com";
const SOURCE_IP: &str = "198.51.100.9";
const PUBLIC_HOST: &str = "news.example.com";
const PUBLIC_IP: &str = "93.184.216.34";
const SECRET: &[u8] = b"acceptance-primary-secret-fits-32-bytes!";
const MAX: usize = handlers::MAX_BODY_BYTES;

const REJECT_STATUS: u16 = 400;
const REJECT_BODY: &str = r#"{"error":"invalid request"}"#;
const TEMPLATE_BODY: &[u8] = b"TEMPLATE-BODY-MUST-BE-REPLACED";

// --- async helper ------------------------------------------------------------

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut context = TaskContext::from_waker(waker);
    let mut future = Box::pin(future);
    for _ in 0..10_000 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
    }
    panic!("deterministic fake unexpectedly awaited external work");
}

// --- Fake chunk stream ---------------------------------------------------------

enum Step {
    Chunk(Vec<u8>),
    Fail,
    Pending,
}

/// Deterministic offline body stream. `polls` counts every poll so tests can
/// prove oversized collection stops pulling and a declared-oversized body is
/// never read at all.
struct FakeStream {
    steps: VecDeque<Step>,
    /// When set, yields this many same-sized chunks after `steps` run out.
    repeat: Option<(usize, usize)>, // (chunk_len, remaining)
    polls: Rc<Cell<usize>>,
}

impl FakeStream {
    fn new(steps: Vec<Step>) -> (Self, Rc<Cell<usize>>) {
        let polls = Rc::new(Cell::new(0));
        (
            Self {
                steps: steps.into(),
                repeat: None,
                polls: Rc::clone(&polls),
            },
            polls,
        )
    }

    fn repeating(chunk_len: usize, count: usize) -> (Self, Rc<Cell<usize>>) {
        let (mut stream, polls) = Self::new(Vec::new());
        stream.repeat = Some((chunk_len, count));
        (stream, polls)
    }

    fn of_chunks(chunks: Vec<&[u8]>) -> (Self, Rc<Cell<usize>>) {
        Self::new(
            chunks
                .into_iter()
                .map(|c| Step::Chunk(c.to_vec()))
                .collect(),
        )
    }
}

impl BodyChunkStream for FakeStream {
    fn poll_next_chunk(
        &mut self,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Vec<u8>, BodyReadError>>> {
        self.polls.set(self.polls.get() + 1);
        if let Some(step) = self.steps.pop_front() {
            return match step {
                Step::Chunk(bytes) => Poll::Ready(Some(Ok(bytes))),
                Step::Fail => Poll::Ready(Some(Err(BodyReadError))),
                Step::Pending => Poll::Pending,
            };
        }
        if let Some((chunk_len, remaining)) = &mut self.repeat {
            if *remaining > 0 {
                *remaining -= 1;
                return Poll::Ready(Some(Ok(vec![0x61; *chunk_len])));
            }
        }
        Poll::Ready(None)
    }
}

fn collect(declared: Option<usize>, source: BodySource<FakeStream>) -> CollectedBody {
    block_on(collect_body(declared, source))
}

// --- collect_body: transport failures -------------------------------------------

#[test]
fn setup_failure_before_any_bytes_is_transport_failure() {
    assert_eq!(
        collect(None, BodySource::SetupFailed),
        CollectedBody::TransportFailure,
        "a present body whose stream cannot be obtained is a transport \
         failure, never an empty body"
    );
}

#[test]
fn declared_length_does_not_rescue_a_setup_failure() {
    assert_eq!(
        collect(Some(17), BodySource::SetupFailed),
        CollectedBody::TransportFailure,
        "a plausible Content-Length must not turn a setup failure into a \
         dispatchable empty body"
    );
}

#[test]
fn chunk_error_before_any_bytes_is_transport_failure() {
    let (stream, _polls) = FakeStream::new(vec![Step::Fail]);
    assert_eq!(
        collect(None, BodySource::Stream(stream)),
        CollectedBody::TransportFailure
    );
}

#[test]
fn chunk_error_after_partial_json_prefix_is_transport_failure() {
    let (stream, _polls) = FakeStream::new(vec![
        Step::Chunk(br#"{"email":"partial"#.to_vec()),
        Step::Fail,
    ]);
    assert_eq!(
        collect(None, BodySource::Stream(stream)),
        CollectedBody::TransportFailure,
        "partial bytes followed by a stream error must be dropped whole"
    );
}

#[test]
fn chunk_error_after_a_complete_valid_json_document_is_transport_failure() {
    // Even when the bytes already received would parse as a complete valid
    // payload, a later chunk error means the body the client sent is NOT
    // the bytes received — the handler must never see the prefix.
    let (stream, _polls) = FakeStream::new(vec![
        Step::Chunk(br#"{"email":"valid@example.com"}"#.to_vec()),
        Step::Fail,
    ]);
    assert_eq!(
        collect(None, BodySource::Stream(stream)),
        CollectedBody::TransportFailure,
        "a valid JSON prefix followed by a stream error is still a \
         transport failure, not a successful truncated payload"
    );
}

// --- collect_body: legitimate bodies -------------------------------------------

#[test]
fn bodyless_request_is_complete_empty() {
    let (stream, polls) = FakeStream::new(Vec::new());
    let _ = stream;
    assert_eq!(
        collect(None, BodySource::Bodyless),
        CollectedBody::Complete(Vec::new()),
        "a request with no body at all stays dispatchable (GET/bodyless routes)"
    );
    assert_eq!(polls.get(), 0);
}

#[test]
fn explicitly_empty_stream_is_complete_empty() {
    let (stream, _polls) = FakeStream::new(Vec::new());
    assert_eq!(
        collect(Some(0), BodySource::Stream(stream)),
        CollectedBody::Complete(Vec::new()),
        "an explicitly sent empty body stays valid (stored-credential \
         vault test dispatches on empty input)"
    );
}

#[test]
fn valid_chunked_body_assembles_in_order() {
    let (stream, _polls) = FakeStream::of_chunks(vec![b"{\"em", b"ail\":\"a@b", b".example\"}"]);
    assert_eq!(
        collect(None, BodySource::Stream(stream)),
        CollectedBody::Complete(br#"{"email":"a@b.example"}"#.to_vec())
    );
}

#[test]
fn pending_chunks_are_awaited_not_treated_as_end_of_body() {
    let (stream, _polls) = FakeStream::new(vec![
        Step::Pending,
        Step::Chunk(b"he".to_vec()),
        Step::Pending,
        Step::Chunk(b"llo".to_vec()),
    ]);
    assert_eq!(
        collect(None, BodySource::Stream(stream)),
        CollectedBody::Complete(b"hello".to_vec()),
        "Pending means not-yet, never end-of-body or failure"
    );
}

#[test]
fn pending_then_error_is_transport_failure() {
    let (stream, _polls) = FakeStream::new(vec![Step::Pending, Step::Fail]);
    assert_eq!(
        collect(None, BodySource::Stream(stream)),
        CollectedBody::TransportFailure
    );
}

#[test]
fn exactly_max_bytes_is_complete() {
    let (stream, _polls) = FakeStream::repeating(8192, MAX / 8192);
    match collect(Some(MAX), BodySource::Stream(stream)) {
        CollectedBody::Complete(bytes) => assert_eq!(bytes.len(), MAX),
        other => panic!("exactly MAX bytes must be Complete, got {other:?}"),
    }
}

// --- collect_body: oversized preservation ---------------------------------------

#[test]
fn declared_oversized_rejects_without_polling_the_stream() {
    let (stream, polls) = FakeStream::of_chunks(vec![b"must never be read"]);
    assert_eq!(
        collect(Some(MAX + 1), BodySource::Stream(stream)),
        CollectedBody::Oversized
    );
    assert_eq!(
        polls.get(),
        0,
        "an oversized declared length short-circuits before any buffering"
    );
}

#[test]
fn single_chunk_over_max_is_oversized() {
    let (stream, _polls) = FakeStream::new(vec![Step::Chunk(vec![0x62; MAX + 1])]);
    assert_eq!(
        collect(None, BodySource::Stream(stream)),
        CollectedBody::Oversized
    );
}

#[test]
fn chunked_oversized_rejects_and_stops_buffering() {
    // 1024 chunks of 8 KiB are on offer; a bounded collector must stop
    // pulling soon after the cap (MAX needs 4 full chunks, the 5th crosses
    // it) instead of draining the stream into memory.
    let (stream, polls) = FakeStream::repeating(8192, 1024);
    assert_eq!(
        collect(None, BodySource::Stream(stream)),
        CollectedBody::Oversized
    );
    assert!(
        polls.get() <= MAX / 8192 + 2,
        "collection must stop pulling at the cap, polled {} times",
        polls.get()
    );
}

#[test]
fn many_small_chunks_crossing_max_is_oversized() {
    let (stream, _polls) = FakeStream::repeating(100, MAX / 100 + 2);
    assert_eq!(
        collect(None, BodySource::Stream(stream)),
        CollectedBody::Oversized,
        "the cap counts accumulated bytes, not chunk sizes"
    );
}

// --- dispatch gate: handler invocation ------------------------------------------

fn template_request(method: &str, path: &str) -> handlers::Request {
    handlers::Request {
        method: method.to_string(),
        path: path.to_string(),
        cookie: None,
        origin: Some(ORIGIN.to_string()),
        body: TEMPLATE_BODY.to_vec(),
    }
}

#[test]
fn transport_failure_never_invokes_the_handler() {
    let calls = Rc::new(Cell::new(0usize));
    let seen = Rc::clone(&calls);
    let response = block_on(dispatch_collected(
        CollectedBody::TransportFailure,
        template_request("POST", "/v1/vault/test"),
        move |_req| {
            seen.set(seen.get() + 1);
            async {
                handlers::Response {
                    status: 200,
                    body: "HANDLER-RAN".to_string(),
                    set_cookie: None,
                }
            }
        },
    ));
    assert_eq!(calls.get(), 0, "transport failure must dispatch nothing");
    assert_eq!(response.status, REJECT_STATUS);
    assert_eq!(
        response.body, REJECT_BODY,
        "rejection is the uniform generic body — no internal error detail"
    );
    assert!(response.set_cookie.is_none());
}

#[test]
fn oversized_never_invokes_the_handler() {
    let calls = Rc::new(Cell::new(0usize));
    let seen = Rc::clone(&calls);
    let response = block_on(dispatch_collected(
        CollectedBody::Oversized,
        template_request("PUT", "/v1/vault"),
        move |_req| {
            seen.set(seen.get() + 1);
            async {
                handlers::Response {
                    status: 200,
                    body: "HANDLER-RAN".to_string(),
                    set_cookie: None,
                }
            }
        },
    ));
    assert_eq!(calls.get(), 0);
    assert_eq!(response.status, REJECT_STATUS);
    assert_eq!(response.body, REJECT_BODY);
    assert!(response.set_cookie.is_none());
}

#[test]
fn complete_invokes_the_handler_once_with_exactly_the_collected_bytes() {
    let calls = Rc::new(Cell::new(0usize));
    let bodies: SeenBodies = Rc::new(RefCell::new(Vec::new()));
    let (seen_calls, seen_bodies) = (Rc::clone(&calls), Rc::clone(&bodies));
    let response = block_on(dispatch_collected(
        CollectedBody::Complete(br#"{"email":"a@b.example"}"#.to_vec()),
        template_request("POST", "/v1/auth/magic/start"),
        move |req| {
            seen_calls.set(seen_calls.get() + 1);
            seen_bodies
                .borrow_mut()
                .push((req.method.clone(), req.path.clone(), req.body.clone()));
            async {
                handlers::Response {
                    status: 299,
                    body: "from-handler".to_string(),
                    set_cookie: Some("cookie".to_string()),
                }
            }
        },
    ));
    assert_eq!(calls.get(), 1);
    assert_eq!(
        bodies.borrow()[0],
        (
            "POST".to_string(),
            "/v1/auth/magic/start".to_string(),
            br#"{"email":"a@b.example"}"#.to_vec()
        ),
        "the template body is replaced by the collected bytes, routing \
         fields pass through untouched"
    );
    assert_eq!(response.status, 299, "handler response passes through");
    assert_eq!(response.body, "from-handler");
}

#[test]
fn complete_empty_invokes_the_handler_with_an_empty_body() {
    let bodies: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&bodies);
    let response = block_on(dispatch_collected(
        CollectedBody::Complete(Vec::new()),
        template_request("POST", "/v1/vault/test"),
        move |req| {
            seen.borrow_mut().push(req.body.clone());
            async {
                handlers::Response {
                    status: 200,
                    body: "ok".to_string(),
                    set_cookie: None,
                }
            }
        },
    ));
    assert_eq!(response.status, 200);
    assert_eq!(
        bodies.borrow()[0],
        Vec::<u8>::new(),
        "a legitimate empty body reaches the handler as empty input"
    );
}

// --- Counting handler fakes (real handlers::handle end-to-end) -------------------

#[derive(Default)]
struct CountingStore {
    calls: Cell<usize>,
    users: RefCell<HashMap<String, handlers::UserRecord>>, // by email
    sessions: RefCell<HashMap<String, session::SessionRecord>>,
    vaults: RefCell<HashMap<String, handlers::VaultRow>>,
    magic_inserts: Cell<usize>,
    vault_upserts: Cell<usize>,
    audits: RefCell<Vec<String>>,
}

impl CountingStore {
    fn bump(&self) {
        self.calls.set(self.calls.get() + 1);
    }
    fn reset_calls(&self) {
        self.calls.set(0);
    }
    fn audits_with(&self, action: &str) -> usize {
        self.audits.borrow().iter().filter(|a| *a == action).count()
    }
}

impl handlers::Store for CountingStore {
    async fn user_by_email(
        &self,
        email: &str,
    ) -> Result<Option<handlers::UserRecord>, handlers::StoreError> {
        self.bump();
        Ok(self.users.borrow().get(email).cloned())
    }
    async fn insert_user(&self, user: &handlers::UserRecord) -> Result<(), handlers::StoreError> {
        self.bump();
        self.users
            .borrow_mut()
            .insert(user.email.clone(), user.clone());
        Ok(())
    }
    async fn insert_challenge(
        &self,
        _challenge: &auth::PasskeyChallenge,
        _user_id: Option<&str>,
        _purpose: &str,
    ) -> Result<(), handlers::StoreError> {
        self.bump();
        Ok(())
    }
    async fn challenge_by_id(
        &self,
        _id: &str,
    ) -> Result<Option<handlers::ChallengeRow>, handlers::StoreError> {
        self.bump();
        Ok(None)
    }
    async fn insert_credential(
        &self,
        _credential: &auth::PasskeyCredential,
    ) -> Result<(), handlers::StoreError> {
        self.bump();
        Ok(())
    }
    async fn credential_by_id(
        &self,
        _cred_id_b64url: &str,
    ) -> Result<Option<auth::PasskeyCredential>, handlers::StoreError> {
        self.bump();
        Ok(None)
    }
    async fn credentials_for_user(
        &self,
        _user_id: &str,
    ) -> Result<Vec<auth::PasskeyCredential>, handlers::StoreError> {
        self.bump();
        Ok(Vec::new())
    }
    async fn advance_sign_count(
        &self,
        _cred_id_b64url: &str,
        _expected: u32,
        _new: u32,
    ) -> Result<bool, handlers::StoreError> {
        self.bump();
        Ok(false)
    }
    async fn insert_magic(
        &self,
        _record: &auth::MagicLinkRecord,
    ) -> Result<(), handlers::StoreError> {
        self.bump();
        self.magic_inserts.set(self.magic_inserts.get() + 1);
        Ok(())
    }
    async fn magic_by_hash(
        &self,
        _token_hash: &str,
    ) -> Result<Option<auth::MagicLinkRecord>, handlers::StoreError> {
        self.bump();
        Ok(None)
    }
    async fn reserve_rate_limit(
        &self,
        _scope_key: &str,
        _action: &str,
        _limit: u64,
        _now: u64,
        _window_secs: u64,
    ) -> Result<bool, handlers::StoreError> {
        self.bump();
        Ok(true)
    }
    async fn prune_expired_challenges(&self, _now: u64) -> Result<(), handlers::StoreError> {
        self.bump();
        Ok(())
    }
    async fn acquire_test_connection_lease(
        &self,
        _user_id: &str,
        _token: &str,
        _lease_secs: u64,
        _max_concurrency: u32,
    ) -> Result<bool, handlers::StoreError> {
        self.bump();
        Ok(true)
    }
    async fn release_test_connection_lease(
        &self,
        _user_id: &str,
        _token: &str,
    ) -> Result<(), handlers::StoreError> {
        self.bump();
        Ok(())
    }
    async fn invite_by_hash(
        &self,
        _code_hash: &str,
    ) -> Result<Option<auth::InviteRecord>, handlers::StoreError> {
        self.bump();
        Ok(None)
    }
    async fn insert_session(
        &self,
        record: &session::SessionRecord,
    ) -> Result<(), handlers::StoreError> {
        self.bump();
        self.sessions
            .borrow_mut()
            .insert(record.id.clone(), record.clone());
        Ok(())
    }
    async fn session_by_id(
        &self,
        id: &str,
    ) -> Result<Option<session::SessionRecord>, handlers::StoreError> {
        self.bump();
        Ok(self.sessions.borrow().get(id).cloned())
    }
    async fn rotate_session_row(
        &self,
        _id: &str,
        _expected_counter: u32,
        _new_counter: u32,
        _new_expires_at: u64,
    ) -> Result<bool, handlers::StoreError> {
        self.bump();
        Ok(false)
    }
    async fn revoke_session(&self, _id: &str) -> Result<(), handlers::StoreError> {
        self.bump();
        Ok(())
    }
    async fn revoke_all_sessions(&self, _user_id: &str) -> Result<(), handlers::StoreError> {
        self.bump();
        Ok(())
    }
    async fn vault_by_user(
        &self,
        user_id: &str,
    ) -> Result<Option<handlers::VaultRow>, handlers::StoreError> {
        self.bump();
        Ok(self.vaults.borrow().get(user_id).cloned())
    }
    async fn upsert_vault(
        &self,
        user_id: &str,
        row: &handlers::VaultRow,
    ) -> Result<(), handlers::StoreError> {
        self.bump();
        self.vault_upserts.set(self.vault_upserts.get() + 1);
        self.vaults
            .borrow_mut()
            .insert(user_id.to_string(), row.clone());
        Ok(())
    }
    async fn delete_vault(&self, user_id: &str) -> Result<(), handlers::StoreError> {
        self.bump();
        self.vaults.borrow_mut().remove(user_id);
        Ok(())
    }
    async fn insert_audit(&self, event: &vault::AuditEvent) -> Result<(), handlers::StoreError> {
        self.bump();
        self.audits.borrow_mut().push(event.action.clone());
        Ok(())
    }
}

impl auth::SingleUseStore for CountingStore {
    async fn claim(&self, _candidate: auth::SingleUseClaim<'_>) -> Result<u64, auth::AuthError> {
        self.bump();
        Ok(0)
    }
}

#[derive(Default)]
struct CountingMailer {
    sent: Cell<usize>,
}

impl handlers::Mailer for CountingMailer {
    async fn send_magic_link(
        &self,
        _email: &str,
        _token: &str,
    ) -> Result<(), handlers::StoreError> {
        self.sent.set(self.sent.get() + 1);
        Ok(())
    }
}

#[derive(Default)]
struct CountingDialer {
    probes: Cell<usize>,
}

impl handlers::Dialer for CountingDialer {
    async fn probe(&self, _target: &vault::ValidatedTarget, _port: u16, _tls: bool) -> bool {
        self.probes.set(self.probes.get() + 1);
        true
    }
}

#[derive(Default)]
struct CountingDns {
    resolves: Cell<usize>,
}

impl handlers::AsyncDns for CountingDns {
    fn resolve<'a>(&'a self, host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>> {
        self.resolves.set(self.resolves.get() + 1);
        let addresses = if host == PUBLIC_HOST {
            vec![PUBLIC_IP.parse().expect("fixture ip")]
        } else {
            Vec::new()
        };
        Box::pin(async move { addresses })
    }
}

struct Harness {
    config: handlers::Config,
    store: CountingStore,
    mailer: CountingMailer,
    dialer: CountingDialer,
    dns: CountingDns,
}

fn harness() -> Harness {
    Harness {
        config: handlers::Config::new(
            RP,
            ORIGIN,
            SECRET.to_vec(),
            [7u8; 32],
            1,
            3600,
            false,
            false,
            None,
        )
        .expect("test config is valid"),
        store: CountingStore::default(),
        mailer: CountingMailer::default(),
        dialer: CountingDialer::default(),
        dns: CountingDns::default(),
    }
}

impl Harness {
    fn ctx(&self) -> handlers::Ctx<'_, CountingStore, CountingMailer, CountingDialer, CountingDns> {
        handlers::Ctx {
            cfg: &self.config,
            store: &self.store,
            mailer: &self.mailer,
            dialer: &self.dialer,
            dns: &self.dns,
            now: NOW,
            source_ip: SOURCE_IP.to_string(),
        }
    }

    /// Full Worker-path shape: collect, then the gate around the REAL
    /// `handlers::handle`. Side-effect counters prove what dispatched.
    fn dispatch(
        &self,
        method: &str,
        path: &str,
        cookie: Option<String>,
        declared: Option<usize>,
        source: BodySource<FakeStream>,
    ) -> handlers::Response {
        let request = handlers::Request {
            method: method.to_string(),
            path: path.to_string(),
            cookie,
            origin: Some(ORIGIN.to_string()),
            body: TEMPLATE_BODY.to_vec(),
        };
        let ctx = self.ctx();
        block_on(async {
            let collected = collect_body(declared, source).await;
            dispatch_collected(collected, request, |req| async move {
                handlers::handle(&ctx, &req).await
            })
            .await
        })
    }

    fn assert_no_side_effects(&self, context: &str) {
        assert_eq!(self.store.calls.get(), 0, "{context}: store touched");
        assert_eq!(self.mailer.sent.get(), 0, "{context}: mailer touched");
        assert_eq!(self.dialer.probes.get(), 0, "{context}: dialer touched");
        assert_eq!(self.dns.resolves.get(), 0, "{context}: DNS touched");
    }

    fn seed_session(&self, user_id: &str) -> String {
        let token = session::issue_session(SECRET, user_id, NOW, 3600).expect("issue session");
        let claims = session::verify_session(SECRET, &token, NOW).expect("verify session");
        self.store.sessions.borrow_mut().insert(
            claims.session_id.clone(),
            session::SessionRecord {
                id: claims.session_id,
                user_id: user_id.to_string(),
                created_at: NOW,
                expires_at: NOW + 3600,
                rotation_counter: 0,
                revoked: false,
            },
        );
        format!("{}={}", session::COOKIE_NAME, token)
    }

    fn seed_stored_vault(&self, user_id: &str) {
        let plaintext = serde_json::json!({
            "provider": {
                "host": PUBLIC_HOST,
                "port": 563,
                "tls": true,
                "username": "synthetic-user",
                "password": "synthetic-pass",
                "max_connections": 4
            },
            "indexer": null
        })
        .to_string();
        let blob = vault::encrypt_vault(&[7u8; 32], 1, user_id, plaintext.as_bytes())
            .expect("encrypt synthetic vault");
        self.store.vaults.borrow_mut().insert(
            user_id.to_string(),
            handlers::VaultRow {
                blob,
                indexer_configured: false,
                updated_at: NOW,
            },
        );
    }
}

// --- End-to-end: failures dispatch nothing ----------------------------------------

#[test]
fn e2e_chunk_error_after_valid_json_prefix_dispatches_nothing() {
    let harness = harness();
    harness.store.users.borrow_mut().insert(
        "valid@example.com".to_string(),
        handlers::UserRecord {
            id: "user-1".to_string(),
            email: "valid@example.com".to_string(),
            created_at: NOW,
        },
    );
    let (stream, _polls) = FakeStream::new(vec![
        Step::Chunk(br#"{"email":"valid@example.com"}"#.to_vec()),
        Step::Fail,
    ]);
    let response = harness.dispatch(
        "POST",
        "/v1/auth/magic/start",
        None,
        None,
        BodySource::Stream(stream),
    );
    assert_eq!(response.status, REJECT_STATUS);
    assert_eq!(response.body, REJECT_BODY);
    assert!(response.set_cookie.is_none());
    harness.assert_no_side_effects("valid prefix then chunk error");
    assert_eq!(harness.store.magic_inserts.get(), 0);
}

#[test]
fn e2e_setup_failure_on_vault_test_dispatches_nothing_and_never_dials() {
    // The stored-credential danger case: today a setup failure dispatches
    // an empty body, and empty input tells vault-test to dial the SAVED
    // provider. With a seeded session + stored vault, only the gate stands
    // between a dead stream and an outbound dial.
    let harness = harness();
    let cookie = harness.seed_session("vault-user");
    harness.seed_stored_vault("vault-user");
    harness.store.reset_calls();

    let response = harness.dispatch(
        "POST",
        "/v1/vault/test",
        Some(cookie),
        None,
        BodySource::SetupFailed,
    );
    assert_eq!(response.status, REJECT_STATUS);
    assert_eq!(response.body, REJECT_BODY);
    harness.assert_no_side_effects("setup failure on vault test");
    assert_eq!(harness.store.audits_with("vault.test"), 0);
}

#[test]
fn e2e_chunk_error_on_vault_put_stores_nothing() {
    let harness = harness();
    let cookie = harness.seed_session("vault-user");
    harness.store.reset_calls();
    let candidate = serde_json::json!({
        "provider": {
            "host": PUBLIC_HOST, "port": 563, "tls": true,
            "username": "synthetic-user", "password": "synthetic-pass",
            "max_connections": 4
        },
        "indexer": null
    })
    .to_string();
    let (stream, _polls) = FakeStream::new(vec![Step::Chunk(candidate.into_bytes()), Step::Fail]);
    let response = harness.dispatch(
        "PUT",
        "/v1/vault",
        Some(cookie),
        None,
        BodySource::Stream(stream),
    );
    assert_eq!(response.status, REJECT_STATUS);
    assert_eq!(response.body, REJECT_BODY);
    harness.assert_no_side_effects("chunk error on vault put");
    assert_eq!(harness.store.vault_upserts.get(), 0);
    assert!(harness.store.vaults.borrow().is_empty());
}

#[test]
fn e2e_declared_oversized_dispatches_nothing() {
    let harness = harness();
    let cookie = harness.seed_session("vault-user");
    harness.store.reset_calls();
    let (stream, polls) = FakeStream::of_chunks(vec![b"never read"]);
    let response = harness.dispatch(
        "PUT",
        "/v1/vault",
        Some(cookie),
        Some(MAX + 1),
        BodySource::Stream(stream),
    );
    assert_eq!(response.status, REJECT_STATUS);
    assert_eq!(response.body, REJECT_BODY);
    assert_eq!(polls.get(), 0, "declared oversized is never buffered");
    harness.assert_no_side_effects("declared oversized");
}

#[test]
fn e2e_chunked_oversized_dispatches_nothing() {
    let harness = harness();
    let cookie = harness.seed_session("vault-user");
    harness.store.reset_calls();
    let (stream, _polls) = FakeStream::repeating(8192, 64);
    let response = harness.dispatch(
        "PUT",
        "/v1/vault",
        Some(cookie),
        None,
        BodySource::Stream(stream),
    );
    assert_eq!(response.status, REJECT_STATUS);
    assert_eq!(response.body, REJECT_BODY);
    harness.assert_no_side_effects("chunked oversized");
    assert!(harness.store.vaults.borrow().is_empty());
}

// --- End-to-end: legitimate traffic still dispatches -------------------------------

#[test]
fn e2e_valid_complete_body_dispatches_and_mails() {
    let harness = harness();
    harness.store.users.borrow_mut().insert(
        "valid@example.com".to_string(),
        handlers::UserRecord {
            id: "user-1".to_string(),
            email: "valid@example.com".to_string(),
            created_at: NOW,
        },
    );
    let (stream, _polls) =
        FakeStream::of_chunks(vec![br#"{"email":"valid"#.as_slice(), b"@example.com\"}"]);
    let body_len = br#"{"email":"valid@example.com"}"#.len();
    let response = harness.dispatch(
        "POST",
        "/v1/auth/magic/start",
        None,
        Some(body_len),
        BodySource::Stream(stream),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(response.body, r#"{"ok":true}"#);
    assert_eq!(
        harness.mailer.sent.get(),
        1,
        "control: the same payload, delivered completely, does mail"
    );
    assert_eq!(harness.store.magic_inserts.get(), 1);
    assert!(harness.store.calls.get() > 0);
}

#[test]
fn e2e_bodyless_get_healthz_still_dispatches() {
    let harness = harness();
    let response = harness.dispatch("GET", "/healthz", None, None, BodySource::Bodyless);
    assert_eq!(response.status, 200);
    assert_eq!(response.body, r#"{"ok":true}"#);
}

#[test]
fn e2e_bodyless_authenticated_get_still_dispatches() {
    let harness = harness();
    let cookie = harness.seed_session("vault-user");
    harness.seed_stored_vault("vault-user");
    harness.store.reset_calls();
    let response = harness.dispatch("GET", "/v1/vault", Some(cookie), None, BodySource::Bodyless);
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(
        response.body.contains(r#""provider_configured":true"#),
        "bodyless GET reaches the handler: {}",
        response.body
    );
    assert!(harness.store.calls.get() > 0, "session lookup happened");
}

#[test]
fn e2e_explicitly_empty_vault_test_still_dials_the_stored_credential() {
    // Positive control for the danger case above: a genuinely empty body
    // (stream present, zero bytes) keeps its documented meaning — test the
    // stored credential — and DOES dial. Only transport failures change.
    let harness = harness();
    let cookie = harness.seed_session("vault-user");
    harness.seed_stored_vault("vault-user");
    harness.store.reset_calls();
    let (stream, _polls) = FakeStream::new(Vec::new());
    let response = harness.dispatch(
        "POST",
        "/v1/vault/test",
        Some(cookie),
        Some(0),
        BodySource::Stream(stream),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(
        response.body.contains(r#""ok":true"#),
        "empty-body stored-credential test still works: {}",
        response.body
    );
    assert_eq!(harness.dialer.probes.get(), 1);
    assert_eq!(harness.store.audits_with("vault.test"), 1);
}

#[test]
fn e2e_bodyless_post_vault_test_still_dispatches() {
    // A POST sent with no body at all (no stream on the edge request) is
    // likewise legitimate empty input for the stored-credential test.
    let harness = harness();
    let cookie = harness.seed_session("vault-user");
    harness.seed_stored_vault("vault-user");
    harness.store.reset_calls();
    let response = harness.dispatch(
        "POST",
        "/v1/vault/test",
        Some(cookie),
        None,
        BodySource::Bodyless,
    );
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(response.body.contains(r#""ok":true"#), "{}", response.body);
    assert_eq!(harness.dialer.probes.get(), 1);
}

// --- Wiring pin ----------------------------------------------------------------------

#[test]
fn worker_entry_dispatch_uses_the_tested_collection_and_gate() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/worker_entry.rs");
    let source = std::fs::read_to_string(&path).expect("read worker entry source");
    // Production code only: drop everything from the first unit-test module.
    let production = match source.split("#[cfg(test)]").next() {
        Some(head) => head,
        None => &source,
    };
    // Strip `//` line comments so prose cannot satisfy the pin.
    let code: String = production
        .lines()
        .map(|line| match line.find("//") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let dispatch_at = code
        .find("pub async fn dispatch(")
        .expect("worker dispatch entry exists");
    let dispatch_code = &code[dispatch_at..];
    assert!(
        dispatch_code.contains("collect_body("),
        "Worker dispatch must collect through the tested collect_body seam"
    );
    assert!(
        dispatch_code.contains("dispatch_collected("),
        "Worker dispatch must reach the handlers only through the tested gate"
    );
    assert!(
        dispatch_code.contains("BodySource::Bodyless")
            && dispatch_code.contains("BodySource::SetupFailed")
            && dispatch_code.contains("BodySource::Stream("),
        "Worker dispatch must classify bodyless vs setup-failure vs stream \
         from the actual runtime request"
    );
    assert!(
        !code.contains("if let Ok(mut stream) = req.stream()"),
        "the setup-error-swallowing read must be gone: stream() failure on \
         a present body is SetupFailed, not an empty body"
    );
    assert!(
        !code.contains("Err(_) => break"),
        "the chunk-error-swallowing read must be gone: a chunk error is a \
         transport failure, not a partial-body dispatch"
    );
}
