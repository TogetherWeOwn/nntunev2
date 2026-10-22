//! Acceptance tests: audit single-session logout (tests-only).
//!
//! Written first (TDD RED) for the paired build slice "api: audit
//! single-session logout". The builder implements against these pins and
//! must not edit this file.
//!
//! Contract pinned here, at the HTTP handler layer (`handlers::handle`):
//!
//! - A successful `POST /v1/session/logout` writes exactly one audit row
//!   with action `session.logout` ([`session::LOGOUT_AUDIT_ACTION`]) under
//!   the verified token's `claims.user_id` — never a caller-supplied user
//!   id from the request body. This mirrors the sibling
//!   `session.revoke_all` audit on `POST /v1/session/revoke-all`.
//! - No audit row is written when authentication fails (401: no cookie, a
//!   garbage cookie, or an already-revoked session) or when the store's
//!   `revoke_session` errors (500).
//! - The 200 body and the clearing `Set-Cookie` are unchanged, and only
//!   the presented session is revoked (the user's other sessions and other
//!   users' sessions survive), so adding the audit must not widen logout
//!   into a logout-everywhere.
//! - A repeat logout with the same cookie writes no second audit row.
//!
//! RED expectation on the pre-build tree: the success and repeat tests
//! fail on the audit-count assertions (no `session.logout` row is written
//! yet), not on compile or import errors — every symbol used here already
//! exists. Intentionally RED: this file must not merge alone; it lands
//! with the paired build card.
//!
//! Fixtures are synthetic only: fixed test keys, documentation addresses,
//! no live store or provider.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --manifest-path workers/api/Cargo.toml \
//!     --test session_logout_audit_acceptance
//! ```

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use nntune_api::handlers::Store;
use nntune_api::{auth, handlers, session, vault};

// --- Constants -----------------------------------------------------------------

const NOW: u64 = 1_700_000_000;
const RP: &str = "passkey.example.com";
const ORIGIN: &str = "https://passkey.example.com";
const SOURCE_IP: &str = "198.51.100.9";
const PUBLIC_HOST: &str = "news.example.com";
const PUBLIC_IP: &str = "93.184.216.34";

const PRIMARY_SECRET: &[u8] = b"acceptance-primary-secret-fits-32-bytes!";

const USER: &str = "user-123";
const OTHER_USER: &str = "user-999";

const CLEARING_COOKIE: &str =
    "__Host-nntune-session=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0";

// --- async helper ----------------------------------------------------------------

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut context = TaskContext::from_waker(waker);
    let mut future = Box::pin(future);
    for _ in 0..1_000 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
    }
    panic!("deterministic fake unexpectedly awaited external work");
}

// --- Recording fake store ----------------------------------------------------------

#[derive(Default)]
struct FakeStore {
    users: RefCell<HashMap<String, handlers::UserRecord>>,
    challenges: RefCell<HashMap<String, handlers::ChallengeRow>>,
    credentials: RefCell<HashMap<String, auth::PasskeyCredential>>,
    magic: RefCell<HashMap<String, auth::MagicLinkRecord>>,
    invites: RefCell<HashMap<String, auth::InviteRecord>>,
    sessions: RefCell<HashMap<String, session::SessionRecord>>,
    vaults: RefCell<HashMap<String, handlers::VaultRow>>,
    audits: RefCell<Vec<vault::AuditEvent>>,
    rate_limit_slots: RefCell<HashMap<(String, String, u32), u64>>,
    fail_revoke_session: Cell<bool>,
}

impl FakeStore {
    fn audits_with(&self, action: &str) -> usize {
        self.audits
            .borrow()
            .iter()
            .filter(|event| event.action == action)
            .count()
    }

    fn session_revoked(&self, id: &str) -> bool {
        self.sessions
            .borrow()
            .get(id)
            .expect("seeded session row")
            .revoked
    }
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
            .find(|user| user.email == email)
            .cloned())
    }

    async fn insert_user(&self, user: &handlers::UserRecord) -> Result<(), handlers::StoreError> {
        let mut users = self.users.borrow_mut();
        if users.values().any(|row| row.email == user.email) {
            return Err(handlers::StoreError);
        }
        users.insert(user.id.clone(), user.clone());
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

    async fn insert_credential(
        &self,
        credential: &auth::PasskeyCredential,
    ) -> Result<(), handlers::StoreError> {
        let mut credentials = self.credentials.borrow_mut();
        if credentials.contains_key(&credential.cred_id_b64url) {
            return Err(handlers::StoreError);
        }
        credentials.insert(credential.cred_id_b64url.clone(), credential.clone());
        Ok(())
    }

    async fn credential_by_id(
        &self,
        cred_id_b64url: &str,
    ) -> Result<Option<auth::PasskeyCredential>, handlers::StoreError> {
        Ok(self.credentials.borrow().get(cred_id_b64url).cloned())
    }

    async fn credentials_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<auth::PasskeyCredential>, handlers::StoreError> {
        Ok(self
            .credentials
            .borrow()
            .values()
            .filter(|credential| credential.user_id == user_id)
            .cloned()
            .collect())
    }

    async fn advance_sign_count(
        &self,
        cred_id_b64url: &str,
        expected: u32,
        new: u32,
    ) -> Result<bool, handlers::StoreError> {
        match self.credentials.borrow_mut().get_mut(cred_id_b64url) {
            Some(credential) if credential.sign_count == expected => {
                credential.sign_count = new;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn insert_magic(
        &self,
        record: &auth::MagicLinkRecord,
    ) -> Result<(), handlers::StoreError> {
        self.magic
            .borrow_mut()
            .insert(record.token_hash.clone(), record.clone());
        Ok(())
    }

    async fn magic_by_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<auth::MagicLinkRecord>, handlers::StoreError> {
        Ok(self.magic.borrow().get(token_hash).cloned())
    }

    async fn reserve_rate_limit(
        &self,
        scope_key: &str,
        action: &str,
        limit: u64,
        now: u64,
        window_secs: u64,
    ) -> Result<bool, handlers::StoreError> {
        if limit == 0 || window_secs == 0 {
            return Ok(false);
        }
        let expires_at = now.saturating_add(window_secs);
        let mut slots = self.rate_limit_slots.borrow_mut();
        slots.retain(|_, expires| *expires > now);
        for slot in 0..limit {
            let slot = u32::try_from(slot).map_err(|_| handlers::StoreError)?;
            let key = (scope_key.to_string(), action.to_string(), slot);
            if let std::collections::hash_map::Entry::Vacant(entry) = slots.entry(key) {
                entry.insert(expires_at);
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

    async fn acquire_test_connection_lease(
        &self,
        _user_id: &str,
        _token: &str,
        _lease_secs: u64,
        _max_concurrency: u32,
    ) -> Result<bool, handlers::StoreError> {
        Ok(true)
    }

    async fn release_test_connection_lease(
        &self,
        _user_id: &str,
        _token: &str,
    ) -> Result<(), handlers::StoreError> {
        Ok(())
    }

    async fn invite_by_hash(
        &self,
        code_hash: &str,
    ) -> Result<Option<auth::InviteRecord>, handlers::StoreError> {
        Ok(self.invites.borrow().get(code_hash).cloned())
    }

    async fn insert_session(
        &self,
        record: &session::SessionRecord,
    ) -> Result<(), handlers::StoreError> {
        self.sessions
            .borrow_mut()
            .insert(record.id.clone(), record.clone());
        Ok(())
    }

    async fn session_by_id(
        &self,
        id: &str,
    ) -> Result<Option<session::SessionRecord>, handlers::StoreError> {
        Ok(self.sessions.borrow().get(id).cloned())
    }

    async fn rotate_session_row(
        &self,
        id: &str,
        expected_counter: u32,
        new_counter: u32,
        new_expires_at: u64,
    ) -> Result<bool, handlers::StoreError> {
        let mut sessions = self.sessions.borrow_mut();
        match sessions.get_mut(id) {
            Some(row) if row.rotation_counter == expected_counter && !row.revoked => {
                row.rotation_counter = new_counter;
                row.expires_at = new_expires_at;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn revoke_session(&self, id: &str) -> Result<(), handlers::StoreError> {
        if self.fail_revoke_session.get() {
            return Err(handlers::StoreError);
        }
        if let Some(row) = self.sessions.borrow_mut().get_mut(id) {
            row.revoked = true;
        }
        Ok(())
    }

    async fn revoke_all_sessions(&self, user_id: &str) -> Result<(), handlers::StoreError> {
        for row in self.sessions.borrow_mut().values_mut() {
            if row.user_id == user_id {
                row.revoked = true;
            }
        }
        Ok(())
    }

    async fn vault_by_user(
        &self,
        user_id: &str,
    ) -> Result<Option<handlers::VaultRow>, handlers::StoreError> {
        Ok(self.vaults.borrow().get(user_id).cloned())
    }

    async fn upsert_vault(
        &self,
        user_id: &str,
        row: &handlers::VaultRow,
    ) -> Result<(), handlers::StoreError> {
        self.vaults
            .borrow_mut()
            .insert(user_id.to_string(), row.clone());
        Ok(())
    }

    async fn delete_vault(&self, user_id: &str) -> Result<(), handlers::StoreError> {
        self.vaults.borrow_mut().remove(user_id);
        Ok(())
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
    async fn claim(&self, _candidate: auth::SingleUseClaim<'_>) -> Result<u64, auth::AuthError> {
        Ok(0)
    }
}

// --- Fake mailer / dialer / DNS ------------------------------------------------------

#[derive(Default)]
struct FakeMailer;

impl handlers::Mailer for FakeMailer {
    async fn send_magic_link(
        &self,
        _email: &str,
        _token: &str,
    ) -> Result<(), handlers::StoreError> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeDialer;

impl handlers::Dialer for FakeDialer {
    async fn probe(&self, _target: &vault::ValidatedTarget, _tls: bool) -> bool {
        false
    }
}

struct FakeDns {
    map: HashMap<String, Vec<IpAddr>>,
}

impl handlers::AsyncDns for FakeDns {
    fn resolve<'a>(&'a self, host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>> {
        let addresses = self.map.get(host).cloned().unwrap_or_default();
        Box::pin(async move { addresses })
    }
}

// --- Harness -------------------------------------------------------------------------

struct Harness {
    config: handlers::Config,
    store: FakeStore,
    mailer: FakeMailer,
    dialer: FakeDialer,
    dns: FakeDns,
}

fn harness() -> Harness {
    Harness {
        config: handlers::Config::new(
            RP,
            ORIGIN,
            PRIMARY_SECRET.to_vec(),
            [7u8; 32],
            1,
            3600,
            false,
            false,
            None,
        )
        .expect("test config is valid"),
        store: FakeStore::default(),
        mailer: FakeMailer,
        dialer: FakeDialer,
        dns: FakeDns {
            map: HashMap::from([(
                PUBLIC_HOST.to_string(),
                vec![PUBLIC_IP.parse().expect("public fixture ip")],
            )]),
        },
    }
}

impl Harness {
    fn ctx(&self) -> handlers::Ctx<'_, FakeStore, FakeMailer, FakeDialer, FakeDns> {
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

    fn logout(&self, cookie: Option<String>, body: &str) -> handlers::Response {
        let request = handlers::Request {
            method: "POST".to_string(),
            path: "/v1/session/logout".to_string(),
            cookie,
            origin: Some(ORIGIN.to_string()),
            authorization: None,
            body: body.as_bytes().to_vec(),
        };
        block_on(handlers::handle(&self.ctx(), &request))
    }
}

/// One stored login for `user_id`: inserts the session row the handler
/// joins the presented token against and returns `(cookie, session_id)`.
fn seed_session(harness: &Harness, user_id: &str) -> (String, String) {
    let token =
        session::issue_session(PRIMARY_SECRET, user_id, NOW, 3600).expect("login issues a session");
    let claims =
        session::verify_session(PRIMARY_SECRET, &token, NOW).expect("issued token verifies");
    let record = session::SessionRecord {
        id: claims.session_id.clone(),
        user_id: claims.user_id,
        created_at: claims.created_at,
        expires_at: claims.expires_at,
        rotation_counter: claims.rotation_counter,
        revoked: false,
    };
    block_on(harness.store.insert_session(&record)).expect("seed session row");
    (
        format!("{}={token}", session::COOKIE_NAME),
        claims.session_id,
    )
}

// --- Pins ------------------------------------------------------------------------------

#[test]
fn logout_route_and_audit_action_constants_are_pinned() {
    assert_eq!(session::LOGOUT_PATH, "/v1/session/logout");
    assert_eq!(
        session::LOGOUT_AUDIT_ACTION,
        "session.logout",
        "single-session logout must audit under its own action"
    );
    assert_ne!(
        session::LOGOUT_AUDIT_ACTION,
        session::LOGOUT_EVERYWHERE_AUDIT_ACTION,
        "single-session logout and logout-everywhere must stay distinguishable in the audit log"
    );
    assert_eq!(session::clear_cookie_header(), CLEARING_COOKIE);
}

#[test]
fn successful_logout_writes_exactly_one_audit_row_for_the_verified_user() {
    let harness = harness();
    // The presented session, a second session for the same user, and a
    // session for another user: only the presented one may be revoked.
    let (cookie, session_id) = seed_session(&harness, USER);
    let (_other_cookie, other_session_id) = seed_session(&harness, USER);
    let (_foreign_cookie, foreign_session_id) = seed_session(&harness, OTHER_USER);

    // A caller-supplied user id in the body must never become the audited
    // identity: the audit row belongs to the verified claims only.
    let response = harness.logout(Some(cookie), &format!(r#"{{"user_id":"{OTHER_USER}"}}"#));

    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(
        response.body, r#"{"ok":true}"#,
        "the 200 body must be unchanged by adding the audit"
    );
    assert_eq!(
        response.set_cookie.as_deref(),
        Some(CLEARING_COOKIE),
        "the clearing Set-Cookie must be unchanged by adding the audit"
    );

    let audits = harness.store.audits.borrow();
    assert_eq!(
        audits.len(),
        1,
        "a successful logout writes exactly one audit row, got {audits:?}"
    );
    let row = &audits[0];
    assert_eq!(
        row.action,
        session::LOGOUT_AUDIT_ACTION,
        "the audit row must use the session.logout action"
    );
    assert_eq!(row.action, "session.logout");
    assert_eq!(
        row.user_id, USER,
        "the audit row must be under the verified claims.user_id, not the body's user_id"
    );
    assert!(row.ok, "a successful logout audits as ok");
    drop(audits);
    assert_eq!(
        harness
            .store
            .audits_with(session::LOGOUT_EVERYWHERE_AUDIT_ACTION),
        0,
        "single-session logout must not audit as a logout-everywhere"
    );

    assert!(
        harness.store.session_revoked(&session_id),
        "the presented session is revoked"
    );
    assert!(
        !harness.store.session_revoked(&other_session_id),
        "the user's other session survives a single-session logout"
    );
    assert!(
        !harness.store.session_revoked(&foreign_session_id),
        "another user's session survives someone else's logout"
    );
}

#[test]
fn unauthenticated_logout_writes_no_audit_row() {
    let harness = harness();
    let (cookie, session_id) = seed_session(&harness, USER);

    // No cookie at all.
    let response = harness.logout(None, "{}");
    assert_eq!(response.status, 401, "{}", response.body);
    assert!(
        harness.store.audits.borrow().is_empty(),
        "a cookieless logout must not audit"
    );

    // A garbage cookie value.
    let response = harness.logout(
        Some(format!("{}=not-a-real-token", session::COOKIE_NAME)),
        "{}",
    );
    assert_eq!(response.status, 401, "{}", response.body);
    assert!(
        harness.store.audits.borrow().is_empty(),
        "a garbage-cookie logout must not audit"
    );
    assert!(
        !harness.store.session_revoked(&session_id),
        "failed authentication must not revoke the stored session"
    );

    // A well-formed token whose stored session is already revoked.
    harness
        .store
        .sessions
        .borrow_mut()
        .get_mut(&session_id)
        .expect("seeded session row")
        .revoked = true;
    let response = harness.logout(Some(cookie), "{}");
    assert_eq!(response.status, 401, "{}", response.body);
    assert!(
        harness.store.audits.borrow().is_empty(),
        "a revoked-session logout must not audit"
    );
    assert_eq!(
        harness.store.audits_with(session::LOGOUT_AUDIT_ACTION),
        0,
        "no session.logout row may be written when authentication fails"
    );
}

#[test]
fn revoke_session_error_returns_500_and_writes_no_audit_row() {
    let harness = harness();
    let (cookie, session_id) = seed_session(&harness, USER);
    harness.store.fail_revoke_session.set(true);

    let response = harness.logout(Some(cookie), "{}");

    assert_eq!(response.status, 500, "{}", response.body);
    assert!(
        harness.store.audits.borrow().is_empty(),
        "no audit row may be written when revoke_session errors"
    );
    assert_eq!(
        harness.store.audits_with(session::LOGOUT_AUDIT_ACTION),
        0,
        "a failed revocation must not be recorded as a session.logout"
    );
    assert!(
        !harness.store.session_revoked(&session_id),
        "the session row stays live when its revocation fails"
    );
}

#[test]
fn repeat_logout_with_the_same_cookie_writes_no_second_audit_row() {
    let harness = harness();
    let (cookie, _session_id) = seed_session(&harness, USER);

    let first = harness.logout(Some(cookie.clone()), "{}");
    assert_eq!(first.status, 200, "{}", first.body);
    assert_eq!(
        harness.store.audits_with(session::LOGOUT_AUDIT_ACTION),
        1,
        "the first logout audits exactly once"
    );

    // The same cookie no longer authenticates (its row is revoked), so the
    // repeat must not mint a second audit row.
    let second = harness.logout(Some(cookie), "{}");
    assert_eq!(second.status, 401, "{}", second.body);
    assert_eq!(
        harness.store.audits.borrow().len(),
        1,
        "a repeat logout must not write a second audit row"
    );
    assert_eq!(
        harness.store.audits_with(session::LOGOUT_AUDIT_ACTION),
        1,
        "exactly one session.logout row across both attempts"
    );
}
