//! Acceptance handoff: an admitted mail send must not strand public auth starts.
//!
//! All requests exercise `handlers::handle`; only storage and outbound I/O are
//! synthetic. The watchdog is a generous test ceiling, NOT the send deadline:
//! production must own a shorter cancellable deadline. No executor timeout is
//! allowed to turn a pending handler into an apparent successful response.
//!
//! The drop witness is checked while the request future is still alive, before
//! test cleanup can cancel it. This proves native send-future cancellation, not
//! that a Worker fetch's AbortSignal is wired: the Builder must separately prove
//! actual webhook abort with a controlled local runtime peer. No real mail,
//! credentials, network, retries, implementation edits, or standalone RED merge.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::{pending, Future};
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use nntune_api::handlers::Store;
use nntune_api::{auth, handlers, session, vault};
use serde_json::{json, Value};

const NOW: u64 = 1_700_000_000;
const ORIGIN: &str = "https://passkey.example.test";
const EMAIL: &str = "mail-deadline@example.test";
const IP: &str = "198.51.100.9";
const WATCHDOG: Duration = Duration::from_secs(30);

// Deliberately polls even if an I/O fake never wakes. A real timer can use its
// own waker; a permanently pending Mailer cannot keep the test process stuck.
fn complete_by<F: Future>(mut future: Pin<&mut F>, label: &str) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut cx = Context::from_waker(waker);
    let until = Instant::now() + WATCHDOG;
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        assert!(
            Instant::now() < until,
            "{label}: auth handler still pending after 30s with an admitted, permanently pending Mailer; missing server-side send deadline"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn ready<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    complete_by(future.as_mut(), "immediate synthetic operation")
}

#[derive(Default)]
struct FakeStore {
    users: RefCell<HashMap<String, handlers::UserRecord>>,
    credentials: RefCell<Vec<auth::PasskeyCredential>>,
    challenges: RefCell<HashMap<String, handlers::ChallengeRow>>,
    magic: RefCell<HashMap<String, auth::MagicLinkRecord>>,
    slots: RefCell<HashMap<(String, String, u32), u64>>,
    qa_codes: RefCell<Vec<(String, String, u64)>>,
    audits: RefCell<Vec<vault::AuditEvent>>,
}

impl handlers::Store for FakeStore {
    async fn user_by_email(
        &self,
        email: &str,
    ) -> Result<Option<handlers::UserRecord>, handlers::StoreError> {
        Ok(self
            .users
            .borrow()
            .values()
            .find(|u| u.email == email)
            .cloned())
    }

    async fn insert_user(&self, user: &handlers::UserRecord) -> Result<(), handlers::StoreError> {
        if self.users.borrow().values().any(|u| u.email == user.email) {
            return Err(handlers::StoreError);
        }
        self.users
            .borrow_mut()
            .insert(user.id.clone(), user.clone());
        Ok(())
    }

    async fn insert_challenge(
        &self,
        challenge: &auth::PasskeyChallenge,
        user_id: Option<&str>,
        purpose: &str,
    ) -> Result<(), handlers::StoreError> {
        self.challenges.borrow_mut().insert(
            challenge.id.clone(),
            handlers::ChallengeRow {
                challenge: challenge.clone(),
                user_id: user_id.map(str::to_string),
                purpose: purpose.to_string(),
            },
        );
        Ok(())
    }

    async fn challenge_by_id(
        &self,
        id: &str,
    ) -> Result<Option<handlers::ChallengeRow>, handlers::StoreError> {
        Ok(self.challenges.borrow().get(id).cloned())
    }

    async fn insert_magic(&self, row: &auth::MagicLinkRecord) -> Result<(), handlers::StoreError> {
        self.magic
            .borrow_mut()
            .insert(row.token_hash.clone(), row.clone());
        Ok(())
    }

    async fn magic_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<auth::MagicLinkRecord>, handlers::StoreError> {
        Ok(self.magic.borrow().get(token_hash).cloned())
    }

    async fn record_qa_issued_code(
        &self,
        email: &str,
        token: &str,
        now: u64,
    ) -> Result<(), handlers::StoreError> {
        self.qa_codes
            .borrow_mut()
            .push((email.into(), token.into(), now));
        Ok(())
    }

    // Same expiring-slot model as the existing native auth acceptance fixture.
    // Real handler code chooses the scope/action/limit; the fake enforces it.
    async fn reserve_rate_limit(
        &self,
        scope: &str,
        action: &str,
        limit: u64,
        now: u64,
        window_secs: u64,
    ) -> Result<bool, handlers::StoreError> {
        if limit == 0 || window_secs == 0 {
            return Ok(false);
        }
        let mut slots = self.slots.borrow_mut();
        slots.retain(|_, expires| *expires > now);
        for slot in 0..limit {
            let slot = u32::try_from(slot).map_err(|_| handlers::StoreError)?;
            let key = (scope.to_string(), action.to_string(), slot);
            if let std::collections::hash_map::Entry::Vacant(entry) = slots.entry(key) {
                entry.insert(now.saturating_add(window_secs));
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn prune_expired_challenges(&self, now: u64) -> Result<(), handlers::StoreError> {
        self.challenges
            .borrow_mut()
            .retain(|_, row| row.challenge.expires_at > now);
        Ok(())
    }

    // Auth-start may read credential presence, but cannot mutate credentials,
    // sessions, vaults, or dial state.
    async fn insert_credential(
        &self,
        _: &auth::PasskeyCredential,
    ) -> Result<(), handlers::StoreError> {
        panic!("auth-start must not insert a credential")
    }
    async fn credential_by_id(
        &self,
        _: &str,
    ) -> Result<Option<auth::PasskeyCredential>, handlers::StoreError> {
        panic!("auth-start must not look up a credential")
    }
    async fn credentials_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<auth::PasskeyCredential>, handlers::StoreError> {
        Ok(self
            .credentials
            .borrow()
            .iter()
            .filter(|c| c.user_id == user_id)
            .cloned()
            .collect())
    }
    async fn advance_sign_count(
        &self,
        _: &str,
        _: u32,
        _: u32,
    ) -> Result<bool, handlers::StoreError> {
        panic!("auth-start must not advance a credential")
    }
    async fn acquire_test_connection_lease(
        &self,
        _: &str,
        _: &str,
        _: u64,
        _: u32,
    ) -> Result<bool, handlers::StoreError> {
        panic!("auth-start must not acquire a probe lease")
    }
    async fn release_test_connection_lease(
        &self,
        _: &str,
        _: &str,
    ) -> Result<(), handlers::StoreError> {
        panic!("auth-start must not release a probe lease")
    }
    async fn invite_by_hash(
        &self,
        _: &str,
    ) -> Result<Option<auth::InviteRecord>, handlers::StoreError> {
        Ok(None)
    }
    async fn insert_session(&self, _: &session::SessionRecord) -> Result<(), handlers::StoreError> {
        panic!("auth-start must not issue a session")
    }
    async fn session_by_id(
        &self,
        _: &str,
    ) -> Result<Option<session::SessionRecord>, handlers::StoreError> {
        Ok(None)
    }
    async fn rotate_session_row(
        &self,
        _: &str,
        _: u32,
        _: u32,
        _: u64,
    ) -> Result<bool, handlers::StoreError> {
        panic!("auth-start must not rotate a session")
    }
    async fn revoke_session(&self, _: &str) -> Result<(), handlers::StoreError> {
        panic!("auth-start must not revoke a session")
    }
    async fn revoke_all_sessions(&self, _: &str) -> Result<(), handlers::StoreError> {
        panic!("auth-start must not revoke sessions")
    }
    async fn vault_by_user(
        &self,
        _: &str,
    ) -> Result<Option<handlers::VaultRow>, handlers::StoreError> {
        panic!("auth-start must not read a vault")
    }
    async fn upsert_vault(
        &self,
        _: &str,
        _: &handlers::VaultRow,
    ) -> Result<(), handlers::StoreError> {
        panic!("auth-start must not write a vault")
    }
    async fn delete_vault(&self, _: &str) -> Result<(), handlers::StoreError> {
        panic!("auth-start must not delete a vault")
    }
    async fn insert_audit(&self, event: &vault::AuditEvent) -> Result<(), handlers::StoreError> {
        self.audits.borrow_mut().push(vault::AuditEvent {
            user_id: event.user_id.clone(),
            action: event.action.clone(),
            ok: event.ok,
            source_ip: event.source_ip.clone(),
            at: event.at,
        });
        Ok(())
    }
}

impl auth::SingleUseStore for FakeStore {
    async fn claim(&self, _: auth::SingleUseClaim<'_>) -> Result<u64, auth::AuthError> {
        panic!("auth-start must not consume a token")
    }
}

#[derive(Clone, Copy)]
enum Delivery {
    Pending,
    Success,
    Failure,
}

struct FakeMailer {
    delivery: Cell<Delivery>,
    attempts: RefCell<Vec<(String, String)>>,
    settled: Cell<usize>,
    cancelled: Cell<usize>,
}

struct PendingSend<'a>(&'a Cell<usize>);
impl Drop for PendingSend<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

impl handlers::Mailer for FakeMailer {
    async fn send_magic_link(&self, email: &str, token: &str) -> Result<(), handlers::StoreError> {
        self.attempts
            .borrow_mut()
            .push((email.into(), token.into()));
        match self.delivery.get() {
            Delivery::Pending => {
                let _witness = PendingSend(&self.cancelled);
                pending::<()>().await;
                unreachable!("permanently pending mail must never settle")
            }
            Delivery::Success => {
                self.settled.set(self.settled.get() + 1);
                Ok(())
            }
            Delivery::Failure => {
                self.settled.set(self.settled.get() + 1);
                Err(handlers::StoreError)
            }
        }
    }
}

struct NoNetwork;
impl handlers::Dialer for NoNetwork {
    async fn probe(&self, _: &vault::ValidatedTarget, _: u16, _: bool) -> bool {
        panic!("auth-start must not dial a provider")
    }
}
impl handlers::AsyncDns for NoNetwork {
    fn resolve<'a>(&'a self, _: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>> {
        panic!("auth-start must not resolve a provider")
    }
}

struct Harness {
    cfg: handlers::Config,
    store: FakeStore,
    mailer: FakeMailer,
}

impl Harness {
    fn new(delivery: Delivery, known: bool, qa: bool) -> Self {
        let h = Self {
            cfg: handlers::Config::new(
                "passkey.example.test",
                ORIGIN,
                b"synthetic-mail-deadline-session-secret".to_vec(),
                [7; 32],
                1,
                3600,
                false,
                false,
                None,
            )
            .expect("synthetic config")
            .with_qa_code_seam(qa),
            store: FakeStore::default(),
            mailer: FakeMailer {
                delivery: Cell::new(delivery),
                attempts: RefCell::new(Vec::new()),
                settled: Cell::new(0),
                cancelled: Cell::new(0),
            },
        };
        if known {
            ready(h.store.insert_user(&handlers::UserRecord {
                id: "known-user".into(),
                email: EMAIL.into(),
                created_at: NOW,
            }))
            .expect("seed synthetic user");
        }
        h
    }

    fn start(&self, register: bool, email: &str, ip: &str, now: u64) -> handlers::Response {
        let ctx = handlers::Ctx {
            cfg: &self.cfg,
            store: &self.store,
            mailer: &self.mailer,
            dialer: &NoNetwork,
            dns: &NoNetwork,
            now,
            source_ip: ip.into(),
        };
        let request = handlers::Request {
            method: "POST".into(),
            path: if register {
                "/v1/auth/passkey/register/start"
            } else {
                "/v1/auth/magic/start"
            }
            .into(),
            cookie: None,
            origin: Some(ORIGIN.into()),
            authorization: None,
            body: json!({"email": email}).to_string().into_bytes(),
        };
        let attempts_before = self.mailer.attempts.borrow().len();
        let cancelled_before = self.mailer.cancelled.get();
        let settled_before = self.mailer.settled.get();
        let mut future = Box::pin(handlers::handle(&ctx, &request));
        let response = complete_by(
            future.as_mut(),
            if register {
                "register/start"
            } else {
                "magic/start"
            },
        );
        // Keep the handler future alive until after the cancellation observation.
        // Dropping it in this helper must not manufacture the cancellation proof.
        if matches!(self.mailer.delivery.get(), Delivery::Pending) {
            assert_eq!(
                self.mailer.cancelled.get() - cancelled_before,
                self.mailer.attempts.borrow().len() - attempts_before,
                "every started send must be cancelled before the public request finishes"
            );
            assert_eq!(
                self.mailer.settled.get(),
                settled_before,
                "timeout is not send success"
            );
        }
        for (email, token) in self.mailer.attempts.borrow().iter() {
            assert!(
                !response.body.contains(email),
                "public response leaks recipient"
            );
            assert!(
                !response.body.contains(token),
                "public response leaks token"
            );
            for event in self.store.audits.borrow().iter() {
                for field in [&event.action, &event.user_id, &event.source_ip] {
                    assert!(!field.contains(email), "audit leaks recipient");
                    assert!(!field.contains(token), "audit leaks token");
                }
            }
        }
        response
    }
}

fn assert_public(response: &handlers::Response, register: bool) {
    assert_eq!(response.status, 200, "auth-start response status");
    assert!(
        response.set_cookie.is_none(),
        "starting auth must not authenticate"
    );
    let body: Value = serde_json::from_str(&response.body).expect("public JSON envelope");
    if register {
        let object = body.as_object().expect("registration envelope");
        assert_eq!(
            object.len(),
            3,
            "registration must keep the existing public shape"
        );
        for key in ["challenge", "rp_id", "user_id"] {
            assert!(
                object.contains_key(key),
                "registration envelope missing {key}"
            );
        }
        assert_eq!(body["rp_id"], "passkey.example.test");
        assert!(body["challenge"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(body["user_id"].as_str().is_some_and(|s| !s.is_empty()));
    } else {
        assert_eq!(body, json!({"ok": true}), "magic-start must remain generic");
    }
}

fn assert_one_issued_token(h: &Harness, email: &str) {
    let attempts = h.mailer.attempts.borrow();
    assert_eq!(attempts.len(), 1, "no retries or duplicate sends");
    // Never format raw mailbox/token values in assertion diagnostics.
    assert!(
        attempts[0].0 == email,
        "send addressed to the issued identity"
    );
    let hash = auth::hash_magic_token(&attempts[0].1);
    let magic = h.store.magic.borrow();
    assert_eq!(
        magic.len(),
        1,
        "one token row, no replacement token on timeout"
    );
    let row = magic
        .get(&hash)
        .expect("attempted token must remain persisted");
    assert!(row.email == email, "persisted identity matches recipient");
    assert_eq!(row.issued_at, NOW);
    assert_eq!(row.expires_at, NOW + auth::MAGIC_LINK_TTL_SECS);
    assert!(!row.used, "mail timeout must not consume the issued token");
    assert!(
        h.store.users.borrow().contains_key(&row.user_id),
        "token retains its user binding"
    );
}

fn pending_start(register: bool, known: bool) {
    let h = Harness::new(Delivery::Pending, known, false);
    let response = h.start(register, EMAIL, IP, NOW);
    assert_public(&response, register);
    assert_one_issued_token(&h, EMAIL);
    assert_eq!(
        h.mailer.cancelled.get(),
        1,
        "in-flight send must be dropped"
    );
    assert!(
        h.store.qa_codes.borrow().is_empty(),
        "production seam remains off"
    );
    // A timed-out attempt still spends admission. Two immediate failures use
    // the remaining shared address slots; the next attempt must never send.
    h.mailer.delivery.set(Delivery::Failure);
    for _ in 0..2 {
        assert_public(&h.start(false, EMAIL, IP, NOW), false);
    }
    h.mailer.delivery.set(Delivery::Pending);
    assert_public(&h.start(false, EMAIL, IP, NOW), false);
    assert_eq!(
        h.mailer.attempts.borrow().len(),
        3,
        "timeout must not refund the address budget"
    );
    assert_eq!(
        h.store.magic.borrow().len(),
        3,
        "denial must not issue another token"
    );
    assert_eq!(h.mailer.cancelled.get(), 1, "denied send must not start");
}

#[test]
fn deadline_registration_for_new_email_cancels_pending_mail() {
    pending_start(true, false);
}

#[test]
fn deadline_registration_retry_for_unverified_email_cancels_pending_mail() {
    pending_start(true, true);
}

#[test]
fn deadline_magic_start_for_known_email_cancels_pending_mail() {
    pending_start(false, true);
}

#[test]
fn unknown_magic_email_remains_generic_without_sending_or_issuing() {
    let h = Harness::new(Delivery::Pending, false, false);
    assert_public(&h.start(false, EMAIL, IP, NOW), false);
    assert!(
        h.mailer.attempts.borrow().is_empty(),
        "unknown magic-start must not send"
    );
    assert!(
        h.store.magic.borrow().is_empty(),
        "unknown magic-start must not issue"
    );
    assert!(
        h.store.users.borrow().is_empty(),
        "unknown magic-start must not create users"
    );
}

#[test]
fn fast_success_keeps_response_and_single_token_semantics() {
    for register in [true, false] {
        let h = Harness::new(Delivery::Success, true, false);
        assert_public(&h.start(register, EMAIL, IP, NOW), register);
        assert_one_issued_token(&h, EMAIL);
        assert_eq!(h.mailer.settled.get(), 1);
        assert_eq!(h.mailer.cancelled.get(), 0);
    }
}

#[test]
fn fast_failure_is_ignored_without_retry_or_token_reissue() {
    for register in [true, false] {
        let h = Harness::new(Delivery::Failure, true, false);
        assert_public(&h.start(register, EMAIL, IP, NOW), register);
        assert_one_issued_token(&h, EMAIL);
        assert_eq!(h.mailer.settled.get(), 1);
        assert_eq!(h.mailer.cancelled.get(), 0);
    }
}

#[test]
fn registered_email_keeps_registration_shape_without_sending_or_issuing() {
    let h = Harness::new(Delivery::Pending, true, false);
    // Only credential presence is read on start; no authenticator verification.
    h.store
        .credentials
        .borrow_mut()
        .push(auth::PasskeyCredential {
            cred_id_b64url: "synthetic-existing-credential".into(),
            user_id: "known-user".into(),
            public_key: vec![1],
            sign_count: 0,
            registered_at: NOW,
        });
    let response = h.start(true, EMAIL, IP, NOW);
    assert_public(&response, true);
    assert!(
        h.mailer.attempts.borrow().is_empty(),
        "taken registration must not send"
    );
    assert!(
        h.store.magic.borrow().is_empty(),
        "taken registration must not issue"
    );
    assert!(
        h.store.challenges.borrow().is_empty(),
        "taken registration must not store a challenge"
    );
    assert_eq!(h.store.users.borrow().len(), 1);
}

#[test]
fn address_budget_is_shared_by_registration_and_magic_even_when_mail_fails() {
    let h = Harness::new(Delivery::Failure, false, false);
    for register in [true, false, true] {
        assert_public(&h.start(register, EMAIL, IP, NOW), register);
    }
    assert_eq!(h.mailer.attempts.borrow().len(), 3);
    assert_eq!(h.store.magic.borrow().len(), 3);
    h.mailer.delivery.set(Delivery::Pending);
    for register in [true, false] {
        assert_public(&h.start(register, EMAIL, IP, NOW), register);
    }
    assert_eq!(
        h.mailer.attempts.borrow().len(),
        3,
        "exhausted address budget must not send"
    );
    assert_eq!(
        h.store.magic.borrow().len(),
        3,
        "exhausted address budget must not issue"
    );
    assert_eq!(
        h.mailer.cancelled.get(),
        0,
        "denied mail must not start a send future"
    );
    h.mailer.delivery.set(Delivery::Failure);
    assert_public(
        &h.start(false, EMAIL, IP, NOW + auth::MAGIC_LINK_TTL_SECS + 1),
        false,
    );
    assert_eq!(
        h.mailer.attempts.borrow().len(),
        4,
        "address window expiry readmits once"
    );
}

#[test]
fn ip_budget_still_denies_before_mail_and_recovers_after_its_window() {
    let h = Harness::new(Delivery::Failure, false, false);
    // Space starts beyond the short burst window, but keep all 30 in one hour.
    for n in 0..30 {
        let email = format!("ip-budget-{n}@example.test");
        assert_public(&h.start(true, &email, IP, NOW + n * 61), true);
    }
    assert_eq!(h.mailer.attempts.borrow().len(), 30);
    assert_eq!(h.store.magic.borrow().len(), 30);
    h.mailer.delivery.set(Delivery::Pending);
    for email in ["ip-budget-0@example.test", "unknown@example.test"] {
        let response = h.start(true, email, IP, NOW + 29 * 61);
        assert_eq!(
            response.status, 429,
            "IP refusal must not distinguish known from unknown"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&response.body).unwrap(),
            json!({"error": "rate_limited"})
        );
        assert_public(&h.start(false, email, IP, NOW + 29 * 61), false);
    }
    assert_eq!(h.mailer.attempts.borrow().len(), 30);
    assert_eq!(h.store.magic.borrow().len(), 30);
    assert_eq!(h.mailer.cancelled.get(), 0);
    h.mailer.delivery.set(Delivery::Failure);
    assert_public(
        &h.start(false, "ip-budget-0@example.test", IP, NOW + 3601),
        false,
    );
    assert_eq!(
        h.mailer.attempts.borrow().len(),
        31,
        "IP expiry readmits once"
    );
}

#[test]
fn global_mail_budget_denies_sends_without_blocking_registration_challenges() {
    let h = Harness::new(Delivery::Failure, false, false);
    for n in 0..100 {
        let email = format!("global-budget-{n}@example.test");
        let ip = format!("198.51.100.{}", n + 10);
        assert_public(&h.start(true, &email, &ip, NOW), true);
    }
    assert_eq!(h.mailer.attempts.borrow().len(), 100);
    assert_eq!(h.store.magic.borrow().len(), 100);
    h.mailer.delivery.set(Delivery::Pending);
    let challenges = h.store.challenges.borrow().len();
    assert_public(&h.start(true, EMAIL, "198.51.100.200", NOW), true);
    assert_eq!(
        h.store.challenges.borrow().len(),
        challenges + 1,
        "mail exhaustion must not strand challenge issuance"
    );
    assert_public(
        &h.start(false, "global-budget-0@example.test", "198.51.100.201", NOW),
        false,
    );
    assert_public(
        &h.start(false, "unknown@example.test", "198.51.100.202", NOW),
        false,
    );
    assert_eq!(h.mailer.attempts.borrow().len(), 100);
    assert_eq!(h.store.magic.borrow().len(), 100);
    assert_eq!(h.mailer.cancelled.get(), 0);
    h.mailer.delivery.set(Delivery::Failure);
    assert_public(
        &h.start(
            false,
            "global-budget-0@example.test",
            "198.51.100.203",
            NOW + 301,
        ),
        false,
    );
    assert_eq!(
        h.mailer.attempts.borrow().len(),
        101,
        "global expiry readmits once"
    );
}

fn assert_qa_capture(h: &Harness) {
    let codes = h.store.qa_codes.borrow();
    let attempts = h.mailer.attempts.borrow();
    assert_eq!(
        codes.len(),
        1,
        "one capture despite ignored delivery outcome"
    );
    assert!(codes[0].0 == EMAIL, "capture uses the synthetic mailbox");
    assert!(
        codes[0].1 == attempts[0].1,
        "capture preserves the issued token, no reissue"
    );
    assert_eq!(codes[0].2, NOW);
}

#[test]
fn synthetic_code_capture_keeps_fast_success_and_failure_behavior() {
    for register in [true, false] {
        for delivery in [Delivery::Success, Delivery::Failure] {
            let h = Harness::new(delivery, true, true);
            assert_public(&h.start(register, EMAIL, IP, NOW), register);
            assert_one_issued_token(&h, EMAIL);
            assert_qa_capture(&h);
        }
    }
}

#[test]
fn code_capture_is_off_without_seam_or_outside_synthetic_mailboxes() {
    for (qa, email) in [(false, EMAIL), (true, "not-synthetic@example.invalid")] {
        let h = Harness::new(Delivery::Failure, false, qa);
        assert_public(&h.start(true, email, IP, NOW), true);
        assert_one_issued_token(&h, email);
        assert!(
            h.store.qa_codes.borrow().is_empty(),
            "capture must remain restricted to the armed synthetic seam"
        );
    }
}

#[test]
fn deadline_registration_preserves_synthetic_capture_on_timeout() {
    let h = Harness::new(Delivery::Pending, false, true);
    assert_public(&h.start(true, EMAIL, IP, NOW), true);
    assert_one_issued_token(&h, EMAIL);
    assert_qa_capture(&h);
}

#[test]
fn deadline_magic_start_preserves_synthetic_capture_on_timeout() {
    let h = Harness::new(Delivery::Pending, true, true);
    assert_public(&h.start(false, EMAIL, IP, NOW), false);
    assert_one_issued_token(&h, EMAIL);
    assert_qa_capture(&h);
}
