//! Auth-throttle 429 as the standard error envelope (tests-only).
//!
//! Contract under test: `POST /v1/auth/passkey/register/start` denied by
//! the per-IP auth-start budgets answers `throttled()`
//! (`workers/api/src/handlers.rs`, the `admit_auth_start` denial in
//! `register_start`) with the worker's standard error envelope
//! (`docs/api-errors.md`, rendered by `workers/api/src/api_error.rs`):
//!
//! ```json
//! {"code": "rate_limited", "message": "<non-empty>", "retryable": true}
//! ```
//!
//! at status 429, plus the denying budget's `Retry-After` (the
//! `Response.retry_after` field `worker_entry` renders as the header),
//! identical for every email, with zero identity material (no email, IP,
//! or account data) anywhere in the body.
//!
//! Defect pinned here: `throttled()` still renders the legacy shape
//! `{"error":"rate_limited"}`, which carries neither `code` nor
//! `retryable` (and no `message` for clients to show). The paired fix is
//! to render the canonical envelope there — e.g.
//! `ApiError::rate_limited(...).body_json()` — while keeping the status,
//! the `Retry-After` value, and the email-independent fixed body
//! unchanged. The `message` wording is the Builder's choice: this suite
//! pins `code`, `retryable`, the exact 3-key shape, and a non-empty
//! message, never the wording.
//!
//! Out of scope (deliberate): `rate_limited_body()` in
//! `workers/api/src/rate_limit.rs` is pinned but unserved reserve — this
//! suite drives the served `throttled()` path through the public
//! `handlers::handle` route with a denying fake store, never that
//! helper. Throttle thresholds are untouched.
//!
//! Builder notes (pre-existing pins, not this file):
//! - the in-module pins in `handlers.rs` asserting
//!   `r#"{"error":"rate_limited"}"#` (four, in the register-start
//!   throttle tests) and the one in `tests/auth_mail_deadline.rs`
//!   asserting `json!({"error": "rate_limited"})` predate this card and
//!   contradict the envelope contract; the paired build must update
//!   those assertions to the envelope shape while keeping their
//!   status / Retry-After / email-independence legs.
//! - `workers/api/README.md` describes the legacy 429 body; update it
//!   with the fix.
//! - This file is acceptance-owned: do not edit it to make the build
//!   pass. Rebase onto this branch instead.
//!
//! RED/GREEN expectation: on the pre-fix tree the three envelope tests
//! fail by assertion (missing `code`/`retryable`/`message`, legacy
//! `error` key); the Retry-After and email-independence/no-leak
//! controls already pass and must keep passing after the fix.
//! Everything uses public API, so RED is an assertion failure, never a
//! compile error.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/api/Cargo.toml --test auth_throttle_envelope
//! ```
//!
//! Canaries are synthetic (`198.51.100.9` TEST-NET-2, example.com
//! emails): no real IPs or credential material anywhere in this file.

use std::cell::RefCell;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use nntune_api::api_error::{spec, ApiError};
use nntune_api::handlers::PairAdmission;
use nntune_api::rate_limit::RateLimitConfig;
use nntune_api::{auth, handlers, session, vault};

const NOW: u64 = 1_700_000_000;
const SOURCE_IP: &str = "198.51.100.9";
/// Pinned literal for the private hourly window (`AUTH_START_WINDOW_SECS`
/// in `handlers.rs`): a first-budget (hourly) denial reports this window.
const HOURLY_WINDOW_SECS: u64 = 3600;
/// Distinctive burst window configured on the test `Config`, so a
/// second-budget denial proving `Retry-After == 42` cannot pass via a
/// hard-coded 3600/60.
const BURST_WINDOW_SECS: u64 = 42;

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

// --- Denying fake store ---------------------------------------------------------
// `register_start` reaches `throttled()` on an `admit_auth_start` denial,
// which is exactly one `reserve_rate_limit_pair` call. The fake records
// the budget actions (proving the served admission path ran) and answers
// with the configured denial; every other store method panics, because a
// denied start must not touch users, challenges, mail, or sessions.

struct DenyingStore {
    admission: PairAdmission,
    pair_actions: RefCell<Vec<(String, String)>>,
}

impl DenyingStore {
    fn denying(admission: PairAdmission) -> Self {
        Self {
            admission,
            pair_actions: RefCell::new(Vec::new()),
        }
    }
}

macro_rules! unreachable_store {
    ($($name:ident($($arg:ident : $ty:ty),*) -> $ret:ty),* $(,)?) => {
        $(fn $name(&self, $($arg: $ty),*) -> impl Future<Output = $ret> {
            async move {
                let _ = ($($arg,)*);
                panic!("a throttled register/start must not touch this store method");
            }
        })*
    };
}

impl handlers::Store for DenyingStore {
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

    async fn reserve_rate_limit_pair(
        &self,
        first: handlers::RateLimitBudget<'_>,
        second: handlers::RateLimitBudget<'_>,
        _now: u64,
    ) -> Result<PairAdmission, handlers::StoreError> {
        self.pair_actions
            .borrow_mut()
            .push((first.action.to_string(), second.action.to_string()));
        Ok(self.admission)
    }
}

impl auth::SingleUseStore for DenyingStore {
    async fn claim(&self, _candidate: auth::SingleUseClaim<'_>) -> Result<u64, auth::AuthError> {
        panic!("a throttled register/start must not claim single-use rows");
    }
}

struct UnreachableMailer;

impl handlers::Mailer for UnreachableMailer {
    async fn send_magic_link(
        &self,
        _email: &str,
        _token: &str,
    ) -> Result<(), handlers::StoreError> {
        panic!("a throttled register/start must send no mail");
    }
}

struct UnreachableDialer;

impl handlers::Dialer for UnreachableDialer {
    async fn probe(&self, _target: &vault::ValidatedTarget, _port: u16, _tls: bool) -> bool {
        panic!("a throttled register/start must not dial");
    }
}

struct UnreachableDns;

impl handlers::AsyncDns for UnreachableDns {
    fn resolve<'a>(&'a self, _host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>> {
        panic!("a throttled register/start must not resolve DNS");
    }
}

// --- Route driver ---------------------------------------------------------------

/// One denied `register/start` for `email`, driven through the public
/// router. Also proves the denial came from the auth-start budget pair
/// (the only route into `throttled()`), not some earlier rejection.
fn throttled_register_start(admission: PairAdmission, email: &str) -> handlers::Response {
    let rate_limits = RateLimitConfig {
        auth_window_secs: BURST_WINDOW_SECS,
        ..RateLimitConfig::default()
    };
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
    .expect("test config is valid")
    .with_rate_limits(rate_limits);
    let store = DenyingStore::denying(admission);
    let mailer = UnreachableMailer;
    let dialer = UnreachableDialer;
    let dns = UnreachableDns;
    let ctx = handlers::Ctx {
        cfg: &config,
        store: &store,
        mailer: &mailer,
        dialer: &dialer,
        dns: &dns,
        now: NOW,
        source_ip: SOURCE_IP.to_string(),
    };
    let request = handlers::Request {
        method: "POST".to_string(),
        path: "/v1/auth/passkey/register/start".to_string(),
        cookie: None,
        origin: None,
        authorization: None,
        body: serde_json::json!({"email": email}).to_string().into_bytes(),
    };
    let response = block_on(handlers::handle(&ctx, &request));
    assert_eq!(
        store.pair_actions.borrow().as_slice(),
        [(
            "auth.start_ip".to_string(),
            "auth.start_ip_burst".to_string()
        )],
        "the denial must be the auth-start budget pair behind throttled()"
    );
    assert_eq!(response.status, 429, "denied start body: {}", response.body);
    assert!(
        response.set_cookie.is_none(),
        "a throttled start must not set a session cookie"
    );
    response
}

fn parsed_body(response: &handlers::Response) -> serde_json::Value {
    serde_json::from_str(&response.body).expect("throttle 429 body must be JSON")
}

// --- The envelope itself (RED before the paired fix) ----------------------------

#[test]
fn throttled_body_is_the_rate_limited_error_envelope() {
    let response = throttled_register_start(PairAdmission::DeniedFirst, "denied@example.com");
    let parsed = parsed_body(&response);
    assert_eq!(
        parsed["code"], "rate_limited",
        "429 body must carry the registry code, got: {parsed}"
    );
    assert_eq!(
        parsed["retryable"], true,
        "a throttle denial is retryable once Retry-After elapses, got: {parsed}"
    );
    assert!(
        parsed.get("error").is_none(),
        "the legacy {{\"error\": ...}} shape must be gone, got: {parsed}"
    );
}

#[test]
fn throttled_envelope_has_exactly_code_message_retryable_keys() {
    let response = throttled_register_start(PairAdmission::DeniedFirst, "denied@example.com");
    let parsed = parsed_body(&response);
    let object = parsed
        .as_object()
        .unwrap_or_else(|| panic!("429 body must be a JSON object, got: {parsed}"));
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["code", "message", "retryable"],
        "envelope is exactly {{code, message, retryable}}: no extra field \
         can smuggle identity material into the 429 body"
    );
    let message = parsed["message"]
        .as_str()
        .unwrap_or_else(|| panic!("envelope message must be a string, got: {parsed}"));
    assert!(
        !message.is_empty(),
        "clients show the envelope message; it must not be empty"
    );
}

#[test]
fn throttled_body_matches_the_canonical_api_error_envelope() {
    // Registry agreement: the served 429 is the `rate_limited` row.
    let registry = spec("rate_limited");
    assert_eq!((registry.status, registry.retryable), (429, true));

    // The served body must be the same envelope `ApiError` renders for
    // whatever message the fix chose — not a second hand-rolled shape
    // that can drift from the registry.
    let response = throttled_register_start(PairAdmission::DeniedFirst, "denied@example.com");
    let parsed = parsed_body(&response);
    let message = parsed["message"].as_str().unwrap_or_else(|| {
        panic!("envelope must carry a string message to compare canonically, got: {parsed}")
    });
    let canonical = ApiError::rate_limited(message);
    assert_eq!((canonical.status, canonical.retryable), (429, true));
    let canonical_body: serde_json::Value =
        serde_json::from_str(&canonical.body_json()).expect("ApiError body must be JSON");
    assert_eq!(
        parsed, canonical_body,
        "throttled() 429 body must equal ApiError::rate_limited's envelope"
    );
}

// --- Retry-After preserved (controls: pass before and after the fix) ------------

#[test]
fn retry_after_reports_the_denying_budgets_window() {
    // Hourly (first) budget denies: Retry-After is its 3600s window.
    let hourly = throttled_register_start(PairAdmission::DeniedFirst, "denied@example.com");
    assert_eq!(
        hourly.retry_after,
        Some(HOURLY_WINDOW_SECS),
        "first-budget denial must carry the hourly window"
    );
    // Burst (second) budget denies: Retry-After is the configured burst
    // window, not the hourly one.
    let burst = throttled_register_start(PairAdmission::DeniedSecond, "denied@example.com");
    assert_eq!(
        burst.retry_after,
        Some(BURST_WINDOW_SECS),
        "second-budget denial must carry the configured burst window"
    );
}

// --- Oracle / leak controls (pass before and after the fix) ---------------------

#[test]
fn throttled_body_is_email_independent_and_leaks_nothing() {
    // The budget is keyed only on the client prefix, so taken vs fresh
    // emails must get byte-identical 429s (no account-existence oracle),
    // and neither the email nor the source IP may appear in the body.
    let taken = throttled_register_start(PairAdmission::DeniedFirst, "taken-canary@example.com");
    let fresh = throttled_register_start(PairAdmission::DeniedFirst, "fresh-canary@example.com");
    assert_eq!(
        taken.body, fresh.body,
        "throttled bodies must be identical across emails"
    );
    assert_eq!(taken.retry_after, fresh.retry_after);
    for canary in [
        "taken-canary@example.com",
        "fresh-canary@example.com",
        SOURCE_IP,
        "100.9",
    ] {
        assert!(
            !taken.body.contains(canary),
            "429 body must not echo per-request data ({canary}): {}",
            taken.body
        );
    }
}
