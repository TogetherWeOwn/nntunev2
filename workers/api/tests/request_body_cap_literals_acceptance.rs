//! Request body cap literal pins (tests-only).
//!
//! Contract under test (`workers/api/src/handlers.rs:53`):
//!
//! - `MAX_BODY_BYTES == 32_768` (`32 * 1024`). The transport suite aliases
//!   the cap symbolically (`handlers::MAX_BODY_BYTES`), so `32768` appears
//!   nowhere — a silent cap change would keep that suite green. This file
//!   pins the literal.
//! - The boundary is enforced through the public helpers: exactly-at-cap
//!   bodies pass the size gate while cap+1 fails closed, both at the Worker
//!   collection seam ([`collect_body`]) and at the in-handler gate
//!   ([`handlers::handle`], `handlers.rs:3050`), which re-checks
//!   `req.body.len()` as defense in depth behind the Worker entry.
//!
//! Sensitivity (verified by construction, production code untouched):
//!
//! - Lowering the cap (e.g. to `16 * 1024`) turns every at-cap test red:
//!   collection reports `Oversized` and the handler answers 400.
//! - Raising the cap (e.g. to `64 * 1024`) turns every cap+1 test red:
//!   collection reports `Complete` and the handler routes past the gate.
//! - Renaming or moving the cap without updating this suite fails the
//!   `max_body_bytes_is_exactly_32768` literal pin.
//!
//! Out of scope: ingest route envelopes (TOG-30757, different worker);
//! short-read transport mode; any route behavior change. Fixtures are
//! synthetic only; no DB, provider, or network is touched.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-api --test request_body_cap_literals_acceptance
//! ```

use std::cell::Cell;
use std::collections::VecDeque;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context as TaskContext, Poll};

use nntune_api::worker_entry::{
    collect_body, dispatch_collected, BodyChunkStream, BodyReadError, BodySource, CollectedBody,
};
use nntune_api::{auth, handlers, session, vault};

/// The pinned literal: 32 KiB. Every boundary below is spelled against this
/// number, never against `handlers::MAX_BODY_BYTES`, so the suite — not the
/// symbol — owns the value.
const CAP: usize = 32_768;

const REJECT_STATUS: u16 = 400;
const REJECT_BODY: &str = r#"{"error":"invalid request"}"#;

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

struct FakeStream {
    chunks: VecDeque<Vec<u8>>,
    polls: Rc<Cell<usize>>,
}

impl FakeStream {
    fn of_sizes(sizes: &[usize], fill: u8) -> (Self, Rc<Cell<usize>>) {
        let polls = Rc::new(Cell::new(0));
        (
            Self {
                chunks: sizes.iter().map(|len| vec![fill; *len]).collect(),
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

fn collect(declared: Option<usize>, chunks: &[usize]) -> (CollectedBody, usize) {
    let (stream, polls) = FakeStream::of_sizes(chunks, 0x61);
    let collected = block_on(collect_body(declared, BodySource::Stream(stream)));
    (collected, polls.get())
}

// --- Literal pin ---------------------------------------------------------------

#[test]
fn max_body_bytes_is_exactly_32768() {
    assert_eq!(
        handlers::MAX_BODY_BYTES,
        CAP,
        "request body cap must stay exactly 32_768 bytes (32 KiB); \
         a new limit needs a threat-model update plus this pin"
    );
    assert_eq!(
        handlers::MAX_BODY_BYTES,
        32 * 1024,
        "the cap must stay expressible as 32 KiB"
    );
}

// --- Collection boundary ---------------------------------------------------------

#[test]
fn exactly_at_cap_single_chunk_is_complete() {
    match collect(None, &[CAP]) {
        (CollectedBody::Complete(bytes), _) => assert_eq!(bytes.len(), CAP),
        (other, _) => panic!("exactly CAP bytes must be Complete, got {other:?}"),
    }
}

#[test]
fn exactly_at_cap_chunked_is_complete() {
    // Four 8 KiB chunks: the cap counts accumulated bytes, not chunk sizes.
    match collect(Some(CAP), &[8192, 8192, 8192, 8192]) {
        (CollectedBody::Complete(bytes), _) => assert_eq!(bytes.len(), CAP),
        (other, _) => panic!("chunked exactly-CAP bytes must be Complete, got {other:?}"),
    }
}

#[test]
fn cap_plus_one_single_chunk_is_oversized() {
    assert_eq!(
        collect(None, &[CAP + 1]).0,
        CollectedBody::Oversized,
        "one byte over the cap must fail closed at collection"
    );
}

#[test]
fn cap_plus_one_chunked_is_oversized() {
    assert_eq!(
        collect(None, &[8192, 8192, 8192, 8192, 1]).0,
        CollectedBody::Oversized,
        "crossing the cap on the final byte must fail closed at collection"
    );
}

#[test]
fn declared_cap_plus_one_rejects_without_polling() {
    let (collected, polls) = collect(Some(CAP + 1), &[CAP + 1]);
    assert_eq!(collected, CollectedBody::Oversized);
    assert_eq!(
        polls, 0,
        "an oversized declared length short-circuits before any buffering"
    );
}

// --- Dispatch gate boundary ------------------------------------------------------

fn template_request(method: &str, path: &str) -> handlers::Request {
    handlers::Request {
        method: method.to_string(),
        path: path.to_string(),
        cookie: None,
        origin: None,
        authorization: None,
        body: Vec::new(),
    }
}

#[test]
fn at_cap_body_dispatches_through_the_public_gate() {
    let (collected, _) = collect(None, &[CAP]);
    let calls = Rc::new(Cell::new(0usize));
    let seen = Rc::clone(&calls);
    let response = block_on(dispatch_collected(
        collected,
        template_request("GET", "/healthz"),
        move |req| {
            seen.set(seen.get() + 1);
            let len = req.body.len();
            async move {
                handlers::Response {
                    status: 200,
                    body: format!(r#"{{"ok":true,"body_len":{len}}}"#),
                    set_cookie: None,
                    retry_after: None,
                }
            }
        },
    ));
    assert_eq!(calls.get(), 1, "an at-cap body must reach the handler");
    assert_eq!(response.status, 200);
    assert_eq!(
        response.body,
        format!(r#"{{"ok":true,"body_len":{CAP}}}"#),
        "the handler must see exactly the collected cap-sized bytes"
    );
}

#[test]
fn cap_plus_one_body_fails_closed_through_the_public_gate() {
    let (collected, _) = collect(None, &[CAP + 1]);
    assert_eq!(collected, CollectedBody::Oversized);
    let calls = Rc::new(Cell::new(0usize));
    let seen = Rc::clone(&calls);
    let response = block_on(dispatch_collected(
        collected,
        template_request("PUT", "/v1/vault"),
        move |_req| {
            seen.set(seen.get() + 1);
            async {
                handlers::Response {
                    status: 200,
                    body: "HANDLER-RAN".to_string(),
                    set_cookie: None,
                    retry_after: None,
                }
            }
        },
    ));
    assert_eq!(calls.get(), 0, "an over-cap body must dispatch nothing");
    assert_eq!(response.status, REJECT_STATUS);
    assert_eq!(
        response.body, REJECT_BODY,
        "rejection is the uniform generic body — no size detail"
    );
    assert!(response.set_cookie.is_none());
}

// --- In-handler gate boundary ----------------------------------------------------
// Defense in depth behind the Worker entry (`handlers.rs:3050`): the handler
// re-checks `req.body.len()` itself, so even a caller that bypasses
// `collect_body` (native tests, future adapters) hits the same cap.

/// Store fake that must never be touched: `GET /healthz` answers before any
/// store, mailer, DNS, or dialer work, so any call is a test bug.
struct UnreachableStore;

macro_rules! unreachable_store {
    ($($name:ident($($arg:ident : $ty:ty),*) -> $ret:ty),* $(,)?) => {
        $(fn $name(&self, $($arg: $ty),*) -> impl Future<Output = $ret> {
            async move {
                let _ = ($($arg,)*);
                panic!("healthz must not touch the store");
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
        panic!("healthz must not touch the store");
    }
}

struct UnreachableMailer;

impl handlers::Mailer for UnreachableMailer {
    async fn send_magic_link(
        &self,
        _email: &str,
        _token: &str,
    ) -> Result<(), handlers::StoreError> {
        panic!("healthz must not touch the mailer");
    }
}

struct UnreachableDialer;

impl handlers::Dialer for UnreachableDialer {
    async fn probe(&self, _target: &vault::ValidatedTarget, _port: u16, _tls: bool) -> bool {
        panic!("healthz must not touch the dialer");
    }
}

struct UnreachableDns;

impl handlers::AsyncDns for UnreachableDns {
    fn resolve<'a>(&'a self, _host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>> {
        panic!("healthz must not touch DNS");
    }
}

fn handle_with_body_len(body_len: usize) -> handlers::Response {
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
        body: vec![0x61; body_len],
    };
    block_on(handlers::handle(&ctx, &request))
}

#[test]
fn handler_gate_passes_exactly_at_cap() {
    let response = handle_with_body_len(CAP);
    assert_eq!(
        response.status, 200,
        "an exactly-at-cap body must pass the in-handler size gate: {}",
        response.body
    );
    assert_eq!(response.body, r#"{"ok":true}"#);
}

#[test]
fn handler_gate_fails_closed_at_cap_plus_one() {
    let response = handle_with_body_len(CAP + 1);
    assert_eq!(response.status, REJECT_STATUS);
    assert_eq!(
        response.body, REJECT_BODY,
        "one byte over the cap must fail closed with the uniform generic body"
    );
    assert!(response.set_cookie.is_none());
}
