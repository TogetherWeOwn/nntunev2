//! Magic-link per-window issuance cap pins (tests-only).
//!
//! Contract under test (`workers/api/src/handlers.rs`):
//!
//! - [`handlers::MAGIC_LINKS_PER_WINDOW`] is exactly `3`: at most three magic
//!   links are issuable per email inside one [`auth::MAGIC_LINK_TTL_SECS`]
//!   window. The budget is claimed via `reserve_rate_limit(scope,
//!   "auth.magic_link", MAGIC_LINKS_PER_WINDOW, now, MAGIC_LINK_TTL_SECS)`
//!   before issuance, so an over-window start is refused before any token is
//!   minted or mail sent.
//! - Three starts for one email inside one window are all admitted (one mail
//!   and one magic row each); the fourth is refused before issuance (no new
//!   mail, no new magic row) while keeping the identical `200 {"ok":true}`
//!   shape, so the refusal is not an oracle.
//! - After the window expires a new start is admitted again, proving the
//!   refusal is the window budget rather than a stuck flag.
//!
//! Boundary vs siblings: TTL *length* values are pinned elsewhere; this file
//! pins only the per-window *count* literal and its issuance wiring. Invite,
//! plan-limit and finish-path budgets are different paths and out of scope.
//! No production code is touched.
//!
//! Sensitivity (verified against a temporary cap change, restored before
//! committing): raising the cap (e.g. 3 to 5) turns the fourth-start refusal
//! red; lowering it (e.g. 3 to 2) turns the third-start admission red.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-api --test magic_link_window_cap_acceptance
//! ```

use std::cell::RefCell;
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

const EMAIL: &str = "cap@example.com";

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

// --- Fake store (slot-based rate limits, real magic rows) --------------------------

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
struct FakeMailer {
    sent: RefCell<Vec<(String, String)>>,
}

impl handlers::Mailer for FakeMailer {
    async fn send_magic_link(&self, email: &str, token: &str) -> Result<(), handlers::StoreError> {
        self.sent
            .borrow_mut()
            .push((email.to_string(), token.to_string()));
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
        mailer: FakeMailer::default(),
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
    fn ctx_at(&self, now: u64) -> handlers::Ctx<'_, FakeStore, FakeMailer, FakeDialer, FakeDns> {
        handlers::Ctx {
            cfg: &self.config,
            store: &self.store,
            mailer: &self.mailer,
            dialer: &self.dialer,
            dns: &self.dns,
            now,
            source_ip: SOURCE_IP.to_string(),
        }
    }

    fn start_at(&self, email: &str, now: u64) -> handlers::Response {
        let request = handlers::Request {
            method: "POST".to_string(),
            path: "/v1/auth/magic/start".to_string(),
            cookie: None,
            origin: Some(ORIGIN.to_string()),
            authorization: None,
            body: serde_json::json!({"email": email}).to_string().into_bytes(),
        };
        block_on(handlers::handle(&self.ctx_at(now), &request))
    }

    fn mails_to(&self, email: &str) -> usize {
        self.mailer
            .sent
            .borrow()
            .iter()
            .filter(|(address, _)| address == email)
            .count()
    }
}

fn seed_user(harness: &Harness, id: &str, email: &str) {
    block_on(harness.store.insert_user(&handlers::UserRecord {
        id: id.to_string(),
        email: email.to_string(),
        created_at: NOW,
    }))
    .expect("seed user");
}

// --- Pins ------------------------------------------------------------------------------

#[test]
fn magic_links_per_window_is_exactly_three() {
    assert_eq!(
        handlers::MAGIC_LINKS_PER_WINDOW,
        3,
        "per-email magic-link issuance budget must stay exactly 3 per window; \
         a drift raises the per-email issuance budget while every symbolic \
         in-module test stays green"
    );
}

#[test]
fn three_starts_admitted_fourth_refused_before_issuance() {
    let harness = harness();
    seed_user(&harness, "cap-user", EMAIL);

    for start in 1..=3 {
        let response = harness.start_at(EMAIL, NOW);
        assert_eq!(response.status, 200, "start {start}: {}", response.body);
        assert_eq!(
            response.body,
            serde_json::json!({"ok": true}).to_string(),
            "start {start}: fixed shape"
        );
        assert_eq!(
            harness.mails_to(EMAIL),
            start,
            "start {start} must issue exactly one mail"
        );
        assert_eq!(
            harness.store.magic.borrow().len(),
            start,
            "start {start} must store exactly one magic row"
        );
    }

    // Fourth start inside the same window: refused before issuance, on the
    // identical response shape (no oracle), with no token mailed and no row
    // stored.
    let refused = harness.start_at(EMAIL, NOW);
    assert_eq!(refused.status, 200, "{}", refused.body);
    assert_eq!(
        refused.body,
        serde_json::json!({"ok": true}).to_string(),
        "over-window refusal must keep the fixed shape"
    );
    assert_eq!(
        harness.mails_to(EMAIL),
        3,
        "over-window start must mail nothing"
    );
    assert_eq!(
        harness.store.magic.borrow().len(),
        3,
        "over-window start must store no magic row"
    );
}

#[test]
fn window_expiry_readmits_after_ttl() {
    let harness = harness();
    seed_user(&harness, "cap-user", EMAIL);

    for _ in 0..handlers::MAGIC_LINKS_PER_WINDOW {
        let response = harness.start_at(EMAIL, NOW);
        assert_eq!(response.status, 200, "{}", response.body);
    }
    assert_eq!(harness.mails_to(EMAIL), 3);
    let refused = harness.start_at(EMAIL, NOW);
    assert_eq!(refused.status, 200, "{}", refused.body);
    assert_eq!(
        harness.mails_to(EMAIL),
        3,
        "control: the window is saturated before expiry"
    );

    // Past the TTL window the budget is fresh: issuance resumes. This proves
    // the refusal above is the window budget, not a stuck flag.
    let after = NOW.saturating_add(auth::MAGIC_LINK_TTL_SECS + 1);
    let response = harness.start_at(EMAIL, after);
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(
        harness.mails_to(EMAIL),
        4,
        "a start past the window must be admitted again"
    );
    assert_eq!(
        harness.store.magic.borrow().len(),
        4,
        "a start past the window must store a new magic row"
    );
}
