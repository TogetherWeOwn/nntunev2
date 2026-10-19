//! Regression: concurrent single-session logouts write exactly one audit row.
//!
//! The paired acceptance file pins sequential behavior (success audits once,
//! repeat-after-revoke is 401 with no second row) and must not be edited by
//! the builder. This file pins the interleaving the sequential suite cannot
//! express: two `POST /v1/session/logout` requests that both authenticated
//! against the pre-revoke snapshot. Only the revoke winner audits; the loser
//! still clears its (now dead) cookie but writes no second `session.logout`
//! row. It also pins the owner binding of the conditional revoke.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --manifest-path workers/api/Cargo.toml \
//!     --test session_logout_concurrent_regression
//! ```

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use nntune_api::handlers::Store;
use nntune_api::{auth, handlers, session, vault};

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
    /// When true, `session_by_id` serves the pre-revoke snapshot (revoked
    /// forced to false), modelling two in-flight requests that both
    /// authenticated before either revocation committed.
    stale_auth: Cell<bool>,
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
        Ok(self.sessions.borrow().get(id).cloned().map(|mut row| {
            if self.stale_auth.get() {
                // Serve the pre-revoke snapshot: the row as it looked when
                // both concurrent requests authenticated.
                row.revoked = false;
            }
            row
        }))
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
        if let Some(row) = self.sessions.borrow_mut().get_mut(id) {
            row.revoked = true;
        }
        Ok(())
    }

    async fn revoke_single_session(
        &self,
        id: &str,
        user_id: &str,
    ) -> Result<bool, handlers::StoreError> {
        let mut sessions = self.sessions.borrow_mut();
        match sessions.get_mut(id) {
            Some(row) if row.user_id == user_id && !row.revoked => {
                row.revoked = true;
                Ok(true)
            }
            _ => Ok(false),
        }
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
    async fn probe(&self, _target: &vault::ValidatedTarget, _port: u16, _tls: bool) -> bool {
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

    fn logout(&self, cookie: Option<String>) -> handlers::Response {
        let request = handlers::Request {
            method: "POST".to_string(),
            path: "/v1/session/logout".to_string(),
            cookie,
            origin: Some(ORIGIN.to_string()),
            authorization: None,
            body: b"{}".to_vec(),
        };
        block_on(handlers::handle(&self.ctx(), &request))
    }
}

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

#[test]
fn concurrent_logouts_sharing_a_pre_revoke_snapshot_write_exactly_one_audit_row() {
    let harness = harness();
    let (cookie, session_id) = seed_session(&harness, USER);
    // Both in-flight requests authenticated before either revocation
    // committed: serve the pre-revoke snapshot for both authentications.
    harness.store.stale_auth.set(true);

    let first = harness.logout(Some(cookie.clone()));
    assert_eq!(first.status, 200, "{}", first.body);
    assert_eq!(first.body, r#"{"ok":true}"#);
    assert_eq!(first.set_cookie.as_deref(), Some(CLEARING_COOKIE));

    let second = harness.logout(Some(cookie));
    assert_eq!(
        second.status, 200,
        "the losing request still clears its now-dead cookie: {}",
        second.body
    );
    assert_eq!(second.set_cookie.as_deref(), Some(CLEARING_COOKIE));

    let audits = harness.store.audits.borrow();
    assert_eq!(
        audits.len(),
        1,
        "concurrent logouts of one session must write exactly one audit row, got {audits:?}"
    );
    assert_eq!(audits[0].action, session::LOGOUT_AUDIT_ACTION);
    assert_eq!(audits[0].user_id, USER);
    assert!(audits[0].ok);
    drop(audits);

    assert!(
        harness
            .store
            .sessions
            .borrow()
            .get(&session_id)
            .expect("seeded session row")
            .revoked,
        "the session is revoked after the race"
    );
}

#[test]
fn revoke_single_session_binds_owner_and_reports_the_transition() {
    let harness = harness();
    let (_, session_id) = seed_session(&harness, USER);
    let (_, foreign_session_id) = seed_session(&harness, OTHER_USER);

    // A mismatched owner transitions nothing and revokes nothing.
    assert!(
        !block_on(harness.store.revoke_single_session(&session_id, OTHER_USER))
            .expect("store answers"),
        "revoking under another user's id must not transition"
    );
    assert!(
        !harness
            .store
            .sessions
            .borrow()
            .get(&session_id)
            .expect("seeded session row")
            .revoked,
        "a mismatched owner must not revoke the row"
    );

    assert!(
        block_on(harness.store.revoke_single_session(&session_id, USER)).expect("store answers"),
        "the owner transitions its own session exactly once"
    );
    assert!(
        !block_on(harness.store.revoke_single_session(&session_id, USER)).expect("store answers"),
        "a repeat revoke transitions nothing"
    );
    assert!(
        !harness
            .store
            .sessions
            .borrow()
            .get(&foreign_session_id)
            .expect("seeded foreign row")
            .revoked,
        "another user's session survives"
    );
}
