//! API 400 `invalid_request` as the standard error envelope (tests-only).
//!
//! Contract under test: the api Worker's uniform pre-routing rejection
//! (`handlers::invalid_request`, `workers/api/src/handlers.rs`) must answer
//! 400 with the standard error envelope (`docs/api-errors.md`, rendered via
//! `workers/api/src/api_error.rs`):
//!
//! ```json
//! {"code": "invalid_request", "message": "invalid request", "retryable": false}
//! ```
//!
//! with no `set_cookie` and zero raw request-body bytes in the response.
//! Every caller of that rejection inherits the shape, including:
//!
//! - the in-handler oversize gate (`handlers::handle` rejects
//!   `req.body.len() > MAX_BODY_BYTES` before routing), and
//! - the Worker-entry dispatch gate (`dispatch_collected` maps
//!   `CollectedBody::Oversized` / `TransportFailure` to the same rejection
//!   and invokes no handler).
//!
//! Defect pinned here: `invalid_request()` still returns the legacy shape
//! `{"error":"invalid request"}`, which carries neither `code` nor
//! `retryable`, and `invalid_request` is not yet in the api Worker's
//! `api_error` registry (`spec("invalid_request")` falls back to the
//! 500 `internal_error` generic). The paired build registers the code
//! (400, `retryable: false`), renders the rejection as that envelope while
//! keeping the status and the user-facing wording unchanged, and adds
//! `api` to the `invalid_request` row's owning workers in
//! `docs/api-errors.md` so the registry/docs sync test stays green.
//!
//! Builder notes:
//!
//! - This file is acceptance-owned: do not edit it to make the build pass.
//!   Build on this branch; test corrections come back to the Test Author.
//! - Pre-existing pins of the legacy body (not this file) contradict the
//!   envelope contract and must be updated to the envelope shape by the
//!   paired build; their other assertions (no dispatch, no storage, no
//!   cookie) stay valid. Verified under the canonical fix: exactly
//!   11 such tests fail (7 transport + 2 cap-literals + 1 short-read +
//!   1 pr30), each on its file's single body constant/assertion, while
//!   the crate's 232 lib tests stay green once the docs row gains `api`:
//!   `tests/request_body_transport_acceptance.rs` (`REJECT_BODY`),
//!   `tests/request_body_cap_literals_acceptance.rs` (`REJECT_BODY`),
//!   `tests/request_body_short_read.rs` (short-read gate body), and
//!   `tests/pr30_findings_acceptance.rs` (oversized vault PUT body).
//!   The `worker_entry` unit tests that use the legacy string as a
//!   synthetic *input* fixture (codeless-body log mapping, header
//!   rendering) do not pin this rejection's output and need no change.
//! - The body-cap value itself is out of scope here: boundaries are
//!   spelled against `handlers::MAX_BODY_BYTES`, never a literal.
//!
//! RED/GREEN expectation: on the pre-fix tree the envelope and registry
//! tests below fail by assertion (missing `code`/`retryable`, legacy
//! `error` key, unregistered code); the status, no-cookie, no-dispatch,
//! no-raw-body, at-cap, and complete-body controls already pass and must
//! keep passing after the fix. Everything uses the existing public API,
//! so RED is an assertion failure, never a compile error.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-api --test invalid_request_envelope_acceptance
//! ```
//!
//! Fixtures are synthetic only; no DB, provider, or network is touched.

use std::cell::Cell;
use std::collections::VecDeque;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context as TaskContext, Poll};

use nntune_api::api_error::spec;
use nntune_api::worker_entry::{
    collect_body, dispatch_collected, BodyChunkStream, BodyReadError, BodySource, CollectedBody,
};
use nntune_api::{auth, handlers, session, vault};

const MAX: usize = handlers::MAX_BODY_BYTES;
const REJECT_STATUS: u16 = 400;
/// Distinctive marker woven through oversize bodies: it must never appear
/// in a rejection body (an oversize response echoes no raw request bytes).
const CANARY: &str = "CANARY-BODY-BYTES-7f3a";

fn canary_body(len: usize) -> Vec<u8> {
    let mut body = Vec::with_capacity(len);
    while body.len() < len {
        body.extend_from_slice(CANARY.as_bytes());
    }
    body.truncate(len);
    body
}

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

/// The rejection body, parsed. Every envelope assertion works on the
/// parsed value so a formatting-only difference (key order, spacing)
/// cannot mask a shape change or fake one.
fn assert_invalid_request_envelope(response: &handlers::Response, context: &str) {
    assert_eq!(
        response.status, REJECT_STATUS,
        "{context}: status must stay 400, body: {}",
        response.body
    );
    assert!(
        response.set_cookie.is_none(),
        "{context}: the rejection must never set a cookie"
    );
    assert!(
        !response.body.contains(CANARY),
        "{context}: rejection must never echo raw request-body bytes: {}",
        response.body
    );
    let parsed: serde_json::Value = serde_json::from_str(&response.body)
        .unwrap_or_else(|_| panic!("{context}: rejection body must be JSON: {}", response.body));
    assert_eq!(
        parsed["code"], "invalid_request",
        "{context}: body must carry the registry code, got: {parsed}"
    );
    assert_eq!(
        parsed["retryable"], false,
        "{context}: a malformed/oversize request is never retryable as sent, got: {parsed}"
    );
    assert_eq!(
        parsed["message"], "invalid request",
        "{context}: the envelope change must keep the existing wording, got: {parsed}"
    );
    assert!(
        parsed.get("error").is_none(),
        "{context}: the legacy {{\"error\": ...}} shape must be gone, got: {parsed}"
    );
    let object = parsed
        .as_object()
        .unwrap_or_else(|| panic!("{context}: body must be a JSON object, got: {parsed}"));
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["code", "message", "retryable"],
        "{context}: envelope is exactly {{code, message, retryable}} — no extra \
         field can smuggle body bytes or request detail into the 400 body"
    );
}

// --- Registry (RED before the paired fix) --------------------------------------

#[test]
fn invalid_request_is_registered_as_400_not_retryable() {
    let registered = spec("invalid_request");
    assert_eq!(
        registered.code, "invalid_request",
        "the api registry must own invalid_request instead of falling back \
         to the internal_error generic"
    );
    assert_eq!(
        (registered.status, registered.retryable),
        (400, false),
        "invalid_request is a 400 the client must not retry unchanged"
    );
}

// --- In-handler oversize gate (`handlers::handle`) ------------------------------
// Defense in depth behind the Worker entry: the handler re-checks
// `req.body.len()` itself and answers through `invalid_request()`, so this
// path pins that function's output directly.

/// Store fake that must never be touched: the oversize gate answers before
/// routing and `GET /healthz` answers before any store, mailer, DNS, or
/// dialer work, so any call is a test bug.
struct UnreachableStore;

macro_rules! unreachable_store {
    ($($name:ident($($arg:ident : $ty:ty),*) -> $ret:ty),* $(,)?) => {
        $(fn $name(&self, $($arg: $ty),*) -> impl Future<Output = $ret> {
            async move {
                let _ = ($($arg,)*);
                panic!("oversize gate / healthz must not touch the store");
            }
        })*
    };
}

impl handlers::Store for UnreachableStore {
    unreachable_store! {
        user_by_email(email: &str) -> Result<Option<handlers::UserRecord>, handlers::StoreError>,
        insert_user(user: &handlers::UserRecord) -> Result<(), handlers::StoreError>,
        insert_challenge(challenge: &auth::PasskeyChallenge, user_id: Option<&str>, purpose: &str) -> Result<(), handlers::StoreError>,
        challenge_by_id(id: &str) -> Result<Option<handlers::ChallengeRow>, handlers::StoreError>,
        insert_credential(credential: &auth::PasskeyCredential) -> Result<(), handlers::StoreError>,
        credential_by_id(cred_id_b64url: &str) -> Result<Option<auth::PasskeyCredential>, handlers::StoreError>,
        credentials_for_user(user_id: &str) -> Result<Vec<auth::PasskeyCredential>, handlers::StoreError>,
        advance_sign_count(cred_id_b64url: &str, expected: u32, new: u32) -> Result<bool, handlers::StoreError>,
        insert_magic(record: &auth::MagicLinkRecord) -> Result<(), handlers::StoreError>,
        magic_by_hash(token_hash: &str) -> Result<Option<auth::MagicLinkRecord>, handlers::StoreError>,
        reserve_rate_limit(scope_key: &str, action: &str, limit: u64, now: u64, window_secs: u64) -> Result<bool, handlers::StoreError>,
        prune_expired_challenges(now: u64) -> Result<(), handlers::StoreError>,
        acquire_test_connection_lease(user_id: &str, token: &str, lease_secs: u64, max_concurrency: u32) -> Result<bool, handlers::StoreError>,
        release_test_connection_lease(user_id: &str, token: &str) -> Result<(), handlers::StoreError>,
        invite_by_hash(code_hash: &str) -> Result<Option<auth::InviteRecord>, handlers::StoreError>,
        insert_session(record: &session::SessionRecord) -> Result<(), handlers::StoreError>,
        session_by_id(id: &str) -> Result<Option<session::SessionRecord>, handlers::StoreError>,
        rotate_session_row(id: &str, expected_counter: u32, new_counter: u32, new_expires_at: u64) -> Result<bool, handlers::StoreError>,
        revoke_session(id: &str) -> Result<(), handlers::StoreError>,
        revoke_all_sessions(user_id: &str) -> Result<(), handlers::StoreError>,
        vault_by_user(user_id: &str) -> Result<Option<handlers::VaultRow>, handlers::StoreError>,
        upsert_vault(user_id: &str, row: &handlers::VaultRow) -> Result<(), handlers::StoreError>,
        delete_vault(user_id: &str) -> Result<(), handlers::StoreError>,
        insert_audit(event: &vault::AuditEvent) -> Result<(), handlers::StoreError>
    }
}

impl auth::SingleUseStore for UnreachableStore {
    async fn claim(&self, _candidate: auth::SingleUseClaim<'_>) -> Result<u64, auth::AuthError> {
        panic!("oversize gate / healthz must not touch the store");
    }
}

struct UnreachableMailer;

impl handlers::Mailer for UnreachableMailer {
    async fn send_magic_link(
        &self,
        _email: &str,
        _token: &str,
    ) -> Result<(), handlers::StoreError> {
        panic!("oversize gate / healthz must not touch the mailer");
    }
}

struct UnreachableDialer;

impl handlers::Dialer for UnreachableDialer {
    async fn probe(&self, _target: &vault::ValidatedTarget, _port: u16, _tls: bool) -> bool {
        panic!("oversize gate / healthz must not touch the dialer");
    }
}

struct UnreachableDns;

impl handlers::AsyncDns for UnreachableDns {
    fn resolve<'a>(&'a self, _host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>> {
        panic!("oversize gate / healthz must not touch DNS");
    }
}

fn handle(body: Vec<u8>) -> handlers::Response {
    let config = handlers::Config::new(
        "passkey.example.com",
        "https://passkey.example.com",
        b"acceptance-primary-secret-fits-32-bytes!".to_vec(),
        [7u8; 32],
        1,
        3600,
        false,
        false,
        None,
    )
    .expect("test config is valid");
    let store = UnreachableStore;
    let mailer = UnreachableMailer;
    let dialer = UnreachableDialer;
    let dns = UnreachableDns;
    let ctx = handlers::Ctx {
        cfg: &config,
        store: &store,
        mailer: &mailer,
        dialer: &dialer,
        dns: &dns,
        now: 1_700_000_000,
        source_ip: "198.51.100.9".to_string(),
    };
    let request = handlers::Request {
        method: "GET".to_string(),
        path: "/healthz".to_string(),
        cookie: None,
        origin: None,
        authorization: None,
        body,
    };
    block_on(handlers::handle(&ctx, &request))
}

#[test]
fn handler_oversize_gate_answers_the_invalid_request_envelope() {
    let response = handle(canary_body(MAX + 1));
    assert_invalid_request_envelope(&response, "in-handler oversize gate");
}

#[test]
fn handler_oversize_rejection_is_identical_regardless_of_body_content() {
    // A generic rejection cannot vary per request: two different oversize
    // bodies render byte-identically, so nothing about the body (content
    // or exact length past the cap) can be encoded in the response.
    let canary = handle(canary_body(MAX + 1));
    let zeros = handle(vec![0u8; MAX + 4096]);
    assert_eq!(canary.status, zeros.status);
    assert_eq!(
        canary.body, zeros.body,
        "oversize rejections must be one constant body, never an echo"
    );
    assert_invalid_request_envelope(&canary, "in-handler oversize gate (canary body)");
    assert_invalid_request_envelope(&zeros, "in-handler oversize gate (zero body)");
}

#[test]
fn handler_gate_still_passes_exactly_at_cap() {
    // Control: the envelope change must not move the boundary. An at-cap
    // body passes the size gate and routes normally.
    let response = handle(vec![0x61; MAX]);
    assert_eq!(
        response.status, 200,
        "an exactly-at-cap body must pass the in-handler size gate: {}",
        response.body
    );
    assert_eq!(response.body, r#"{"ok":true}"#);
}

// --- Worker-entry dispatch gate (`dispatch_collected`) -------------------------

fn template_request() -> handlers::Request {
    handlers::Request {
        method: "PUT".to_string(),
        path: "/v1/vault".to_string(),
        cookie: None,
        origin: None,
        authorization: None,
        body: canary_body(64),
    }
}

fn dispatch_rejection(collected: CollectedBody) -> handlers::Response {
    let calls = Rc::new(Cell::new(0usize));
    let seen = Rc::clone(&calls);
    let response = block_on(dispatch_collected(
        collected,
        template_request(),
        move |_req| {
            seen.set(seen.get() + 1);
            async {
                handlers::Response {
                    status: 200,
                    body: "HANDLER-RAN".to_string(),
                    set_cookie: Some("must-never-appear".to_string()),
                    retry_after: None,
                }
            }
        },
    ));
    assert_eq!(calls.get(), 0, "a rejected body must dispatch nothing");
    response
}

#[test]
fn oversized_dispatch_gate_answers_the_invalid_request_envelope() {
    let response = dispatch_rejection(CollectedBody::Oversized);
    assert_invalid_request_envelope(&response, "dispatch gate (oversized)");
}

#[test]
fn transport_failure_dispatch_gate_answers_the_same_envelope() {
    // Both non-Complete outcomes share `invalid_request()`: the gate can
    // never drift into two rejection shapes.
    let oversized = dispatch_rejection(CollectedBody::Oversized);
    let transport = dispatch_rejection(CollectedBody::TransportFailure);
    assert_invalid_request_envelope(&transport, "dispatch gate (transport failure)");
    assert_eq!(
        oversized.body, transport.body,
        "oversized and transport-failure rejections are the one uniform body"
    );
    assert_eq!(oversized.status, transport.status);
}

#[test]
fn gate_and_handler_rejections_are_the_one_uniform_body() {
    let via_gate = dispatch_rejection(CollectedBody::Oversized);
    let via_handler = handle(canary_body(MAX + 1));
    assert_eq!(via_gate.status, via_handler.status);
    assert_eq!(
        via_gate.body, via_handler.body,
        "the Worker-entry gate and the in-handler gate must render the \
         same envelope because both go through invalid_request()"
    );
}

#[test]
fn complete_body_still_dispatches_through_the_gate() {
    // Control: only the rejection shape changes. A complete body still
    // reaches the handler exactly once and its response passes through.
    let calls = Rc::new(Cell::new(0usize));
    let seen = Rc::clone(&calls);
    let response = block_on(dispatch_collected(
        CollectedBody::Complete(br#"{"ok":true}"#.to_vec()),
        template_request(),
        move |req| {
            seen.set(seen.get() + 1);
            let body_len = req.body.len();
            async move {
                handlers::Response {
                    status: 200,
                    body: format!(r#"{{"body_len":{body_len}}}"#),
                    set_cookie: None,
                    retry_after: None,
                }
            }
        },
    ));
    assert_eq!(calls.get(), 1);
    assert_eq!(response.status, 200);
    assert_eq!(response.body, r#"{"body_len":11}"#);
}

// --- End-to-end oversize mapping (`collect_body` -> gate) -----------------------

struct FakeStream {
    chunks: VecDeque<Vec<u8>>,
    polls: Rc<Cell<usize>>,
}

impl FakeStream {
    fn one_chunk(bytes: Vec<u8>) -> (Self, Rc<Cell<usize>>) {
        let polls = Rc::new(Cell::new(0));
        (
            Self {
                chunks: [bytes].into(),
                polls: Rc::clone(&polls),
            },
            polls,
        )
    }
}

impl BodyChunkStream for FakeStream {
    fn poll_next_chunk(
        &mut self,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Vec<u8>, BodyReadError>>> {
        self.polls.set(self.polls.get() + 1);
        match self.chunks.pop_front() {
            Some(bytes) => Poll::Ready(Some(Ok(bytes))),
            None => Poll::Ready(None),
        }
    }
}

#[test]
fn declared_oversize_maps_to_the_envelope_without_reading_the_body() {
    let (stream, polls) = FakeStream::one_chunk(canary_body(MAX + 1));
    let collected = block_on(collect_body(Some(MAX + 1), BodySource::Stream(stream)));
    assert_eq!(collected, CollectedBody::Oversized);
    assert_eq!(
        polls.get(),
        0,
        "an oversized declared length short-circuits before any buffering"
    );
    let response = dispatch_rejection(collected);
    assert_invalid_request_envelope(&response, "declared oversize end-to-end");
}

#[test]
fn chunked_oversize_maps_to_the_envelope_and_echoes_no_raw_bytes() {
    // The oversize bytes are actually pulled here (no declared length),
    // so this is the case where an echo could happen: the canary woven
    // through the chunk must not survive into the response.
    let (stream, _polls) = FakeStream::one_chunk(canary_body(MAX + 1));
    let collected = block_on(collect_body(None, BodySource::Stream(stream)));
    assert_eq!(collected, CollectedBody::Oversized);
    let response = dispatch_rejection(collected);
    assert_invalid_request_envelope(&response, "chunked oversize end-to-end");
    assert!(
        response.body.len() < 200,
        "the rejection is the fixed envelope, never the oversize payload: {}",
        response.body
    );
}
