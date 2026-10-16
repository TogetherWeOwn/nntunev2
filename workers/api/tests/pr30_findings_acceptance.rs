//! Acceptance tests: land PR #30 (WebAuthn verification + Worker HTTP +
//! Hyperdrive entry) review findings.
//!
//! TDD RED: this file FAILS on branches without the PR #30 implementation
//! (`nntune_api::handlers` and `nntune_api::webauthn` do not exist there, so
//! the test target does not build). The Builder lands the implementation on
//! this branch; every test below then pins the FIXED behavior. A finding
//! that is still defective fails at runtime with the message naming it, so
//! "fail for the right reason" holds in both states.
//!
//! Test files only: no implementation code is touched here. The Builder owns
//! all fixes and must never edit this file.
//!
//! Failing command (this card):
//!   `cargo test --manifest-path workers/api/Cargo.toml --all-targets`
//! (the new target is red-by-design until the slice lands; the pre-existing
//! `api_tests` and `security_regressions` targets stay green).
//!
//! Finding map (review keys in parentheses; each test fails while its
//! finding is present and passes once fixed):
//! - session-secret rotation window (`rotation-previous-secret-unwired`):
//!   `previous_secret_*` — a token minted under the previous secret
//!   authenticates and refresh-migrates only inside the rotation window,
//!   and is rejected in steady state.
//! - strict secret codec (`secret-codec-flexible-vs-strict-b64`): the
//!   shared-codec contract the env layer must decode through.
//! - plaintext opt-in audit (`late-novel-plaintext-optin-audit-action`):
//!   `plaintext_opt_in_save_audits_insecure_action`.
//! - invite throttle (`late-novel-invite-redemption-unthrottled`):
//!   `invite_budget_*` — the 11th redemption attempt is denied with the
//!   generic error even with a valid code.
//! - test-connection budget (`late-novel-test-connection-limit-10-vs-5`):
//!   `test_connection_enforces_shared_five_per_minute_budget` — the HTTP
//!   layer enforces the single documented 5/minute budget.
//! - purpose confusion (round-2 regression): `register_finish_rejects_*`
//!   and `login_finish_rejects_register_purpose_challenge`.
//! - inbox proof (round-2 regression):
//!   `register_finish_without_inbox_proof_enrolls_nothing`.
//! - enumeration shape (round-2 regression): `auth_starts_keep_fixed_shape*`.
//! - single-use / concurrency (rounds 1-2 regressions): `*_rejects_replay*`,
//!   `login_finish_rejects_replayed_assertion_clone`,
//!   `test_connection_lease_cap_blocks_third_concurrent_probe`.
//! - body cap (round-2 regression): `oversized_body_rejected_before_routing`.
//! - challenge-row growth (rounds 1-2 regressions):
//!   `login_start_budget_stores_no_challenge_past_cap`,
//!   `register_start_for_taken_account_stores_nothing`.
//! - cookie parsing (round-1 regression):
//!   `nameless_cookie_does_not_break_session_auth`.
//! - vault SSRF/validation wiring: `vault_put_*`.
//! - WebAuthn ceremony edges: `attestation_*`, `assertion_*`.
//!
//! Explicitly NOT covered by cargo tests (Worker/deploy surface, no native
//! harness): the Hyperdrive `--caching-disabled` binding requirement and its
//! `check_hyperdrive_cache.sh` gate semantics, the `SESSION_SECRET_ROTATION`
//! fail-closed startup rule in the Worker env layer, and live-DB/DoH/socket
//! behavior. Those stay follow-ups outside this file.
//!
//! Fixtures are synthetic only: fixed test keys, documentation addresses,
//! no live Hyperdrive/DoH, no real authenticator.
//!
//! Hygiene: this file must stay clean under the public-text lint, so it
//! carries no tracker ids, no issue links, and no reserved-range IPv4
//! literals (rejected-address pins use loopback, documentation ranges, or
//! malformed hosts instead).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use nntune_api::handlers::Store;
use nntune_api::{auth, b64, handlers, session, vault, webauthn};

// --- Constants -----------------------------------------------------------------

const NOW: u64 = 1_700_000_000;
const RP: &str = "passkey.example.com";
const ORIGIN: &str = "https://passkey.example.com";
const SOURCE_IP: &str = "198.51.100.9";
const PUBLIC_HOST: &str = "news.example.com";
const PUBLIC_IP: &str = "93.184.216.34";
const LOOPBACK_HOST: &str = "quarantine.example.com";
const INDEXER_HOST: &str = "index.example.com";

const PRIMARY_SECRET: &[u8] = b"acceptance-primary-secret-fits-32-bytes!";
const PREVIOUS_SECRET: &[u8] = b"acceptance-previous-secret-fits-32-byte!";

const GENERIC_AUTH_BODY: &str = r#"{"error":"check your email and try again"}"#;
const GENERIC_VAULT_BODY: &str = r#"{"error":"vault unavailable"}"#;

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

// --- Fake store (mirrors the PR's conditional single-row semantics) ---------------

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
    race_next_sign_count_advance: Cell<bool>,
    rate_limit_slots: RefCell<HashMap<(String, String, u32), u64>>,
    test_connection_leases: RefCell<HashMap<(String, u32), (String, u64)>>,
}

impl FakeStore {
    fn seed_invite(&self, code: &str) {
        self.invites.borrow_mut().insert(
            auth::hash_invite_code(code),
            auth::InviteRecord {
                code_hash: auth::hash_invite_code(code),
                used: false,
                created_at: NOW,
            },
        );
    }

    fn seed_session_row(&self, record: session::SessionRecord) {
        self.sessions.borrow_mut().insert(record.id.clone(), record);
    }

    fn challenge_count(&self) -> usize {
        self.challenges.borrow().len()
    }

    fn audits_with(&self, action: &str) -> usize {
        self.audits
            .borrow()
            .iter()
            .filter(|event| event.action == action)
            .count()
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
        let mut credentials = self.credentials.borrow_mut();
        if self.race_next_sign_count_advance.replace(false) {
            if let Some(credential) = credentials.get_mut(cred_id_b64url) {
                if credential.sign_count == expected {
                    // Model a concurrent conditional update winning after this
                    // request read its snapshot but before its own CAS.
                    credential.sign_count = new;
                }
            }
            return Ok(false);
        }
        match credentials.get_mut(cred_id_b64url) {
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
        user_id: &str,
        token: &str,
        lease_secs: u64,
        max_concurrency: u32,
    ) -> Result<bool, handlers::StoreError> {
        let expires_at = NOW.saturating_add(lease_secs);
        let mut leases = self.test_connection_leases.borrow_mut();
        leases.retain(|_, (_, expires)| *expires > NOW);
        for slot in 0..max_concurrency {
            let key = (user_id.to_string(), slot);
            if let std::collections::hash_map::Entry::Vacant(entry) = leases.entry(key) {
                entry.insert((token.to_string(), expires_at));
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn release_test_connection_lease(
        &self,
        user_id: &str,
        token: &str,
    ) -> Result<(), handlers::StoreError> {
        self.test_connection_leases
            .borrow_mut()
            .retain(|(owner, _), (lease_token, _)| owner != user_id || lease_token != token);
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
    async fn claim(&self, candidate: auth::SingleUseClaim<'_>) -> Result<u64, auth::AuthError> {
        match candidate {
            auth::SingleUseClaim::MagicLink { record, now } => {
                let mut magic = self.magic.borrow_mut();
                match magic.get_mut(&record.token_hash) {
                    Some(row)
                        if row.user_id == record.user_id
                            && row.email == record.email
                            && row.expires_at == record.expires_at
                            && row.expires_at > now
                            && !row.used =>
                    {
                        row.used = true;
                        Ok(1)
                    }
                    _ => Ok(0),
                }
            }
            auth::SingleUseClaim::Challenge { record, now } => {
                let mut challenges = self.challenges.borrow_mut();
                match challenges.get_mut(&record.id) {
                    Some(row)
                        if row.challenge.rp_id == record.rp_id
                            && row.challenge.origin == record.origin
                            && row.challenge.expires_at == record.expires_at
                            && row.challenge.expires_at > now
                            && !row.challenge.consumed =>
                    {
                        row.challenge.consumed = true;
                        Ok(1)
                    }
                    _ => Ok(0),
                }
            }
            auth::SingleUseClaim::Invite { record } => {
                let mut invites = self.invites.borrow_mut();
                match invites.get_mut(&record.code_hash) {
                    Some(row) if !row.used => {
                        row.used = true;
                        Ok(1)
                    }
                    _ => Ok(0),
                }
            }
        }
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
struct FakeDialer {
    ok: bool,
    seen: RefCell<Vec<Vec<IpAddr>>>,
}

impl handlers::Dialer for FakeDialer {
    async fn probe(&self, target: &vault::ValidatedTarget, _port: u16, _tls: bool) -> bool {
        self.seen.borrow_mut().push(target.addresses().to_vec());
        self.ok
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

fn test_config(invites_required: bool, previous: Option<Vec<u8>>) -> handlers::Config {
    handlers::Config::new(
        RP,
        ORIGIN,
        PRIMARY_SECRET.to_vec(),
        [7u8; 32],
        1,
        3600,
        invites_required,
        false,
        previous,
    )
    .expect("test config is valid")
}

fn test_dns() -> FakeDns {
    FakeDns {
        map: HashMap::from([
            (
                PUBLIC_HOST.to_string(),
                vec![PUBLIC_IP.parse().expect("public fixture ip")],
            ),
            (
                INDEXER_HOST.to_string(),
                vec![PUBLIC_IP.parse().expect("public fixture ip")],
            ),
            (
                LOOPBACK_HOST.to_string(),
                vec!["127.0.0.1".parse().expect("loopback fixture ip")],
            ),
        ]),
    }
}

fn harness() -> Harness {
    Harness {
        config: test_config(false, None),
        store: FakeStore::default(),
        mailer: FakeMailer::default(),
        dialer: FakeDialer {
            ok: true,
            seen: RefCell::new(Vec::new()),
        },
        dns: test_dns(),
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

    fn call(&self, method: &str, path: &str, body: serde_json::Value) -> handlers::Response {
        self.call_full(method, path, Some(body.to_string()), None, Some(ORIGIN))
    }

    fn call_full(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
        cookie: Option<String>,
        origin: Option<&str>,
    ) -> handlers::Response {
        let request = handlers::Request {
            method: method.to_string(),
            path: path.to_string(),
            cookie,
            origin: origin.map(str::to_string),
            body: body.map(|text| text.into_bytes()).unwrap_or_default(),
        };
        block_on(handlers::handle(&self.ctx(), &request))
    }
}

fn cookie_of(response: &handlers::Response) -> String {
    response
        .set_cookie
        .clone()
        .expect("response sets a cookie")
        .split(';')
        .next()
        .expect("cookie header has a value")
        .to_string()
}

fn seed_user(harness: &Harness, id: &str, email: &str) {
    block_on(harness.store.insert_user(&handlers::UserRecord {
        id: id.to_string(),
        email: email.to_string(),
        created_at: NOW,
    }))
    .expect("seed user");
}

fn magic_cookie(harness: &Harness, user_id: &str, email: &str) -> String {
    seed_user(harness, user_id, email);
    let response = harness.call(
        "POST",
        "/v1/auth/magic/start",
        serde_json::json!({"email": email}),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let token = harness
        .mailer
        .sent
        .borrow()
        .iter()
        .rev()
        .find(|(address, _)| address == email)
        .expect("magic link mailed")
        .1
        .clone();
    let response = harness.call(
        "POST",
        "/v1/auth/magic/finish",
        serde_json::json!({"token": token}),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    cookie_of(&response)
}

fn public_provider() -> serde_json::Value {
    serde_json::json!({
        "host": PUBLIC_HOST,
        "port": 563,
        "tls": true,
        "username": "synthetic-user",
        "password": "synthetic-pass",
        "max_connections": 4
    })
}

fn plaintext_opt_in_provider() -> serde_json::Value {
    serde_json::json!({
        "host": PUBLIC_HOST,
        "port": 119,
        "tls": false,
        "username": "synthetic-user",
        "password": "synthetic-pass",
        "max_connections": 4,
        "allow_insecure_plaintext": true
    })
}

fn put_vault(
    harness: &Harness,
    cookie: &str,
    provider: serde_json::Value,
    indexer: Option<serde_json::Value>,
) -> handlers::Response {
    harness.call_full(
        "PUT",
        "/v1/vault",
        Some(serde_json::json!({"provider": provider, "indexer": indexer}).to_string()),
        Some(cookie.to_string()),
        Some(ORIGIN),
    )
}

// --- Synthetic WebAuthn fixtures (no real authenticator) -------------------------------

fn cbor_head(major: u8, value: u64, out: &mut Vec<u8>) {
    let marker = major << 5;
    if value < 24 {
        out.push(marker | value as u8);
    } else if value <= u8::MAX as u64 {
        out.push(marker | 24);
        out.push(value as u8);
    } else if value <= u16::MAX as u64 {
        out.push(marker | 25);
        out.extend_from_slice(&(value as u16).to_be_bytes());
    } else {
        out.push(marker | 26);
        out.extend_from_slice(&(value as u32).to_be_bytes());
    }
}

fn cbor_int(value: i64, out: &mut Vec<u8>) {
    if value >= 0 {
        cbor_head(0, value as u64, out);
    } else {
        cbor_head(1, (-1 - value) as u64, out);
    }
}

fn cbor_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    cbor_head(2, bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn cbor_text(text: &str, out: &mut Vec<u8>) {
    cbor_head(3, text.len() as u64, out);
    out.extend_from_slice(text.as_bytes());
}

fn test_signing_key(seed: u8) -> p256::ecdsa::SigningKey {
    p256::ecdsa::SigningKey::from_slice(&[seed; 32]).expect("fixed test key")
}

fn es256_cose(key: &p256::ecdsa::SigningKey) -> Vec<u8> {
    let point = key.verifying_key().to_encoded_point(false);
    let mut cose = Vec::new();
    cbor_head(5, 5, &mut cose);
    cbor_int(1, &mut cose);
    cbor_int(2, &mut cose);
    cbor_int(3, &mut cose);
    cbor_int(-7, &mut cose);
    cbor_int(-1, &mut cose);
    cbor_int(1, &mut cose);
    cbor_int(-2, &mut cose);
    cbor_bytes(point.x().expect("x coordinate"), &mut cose);
    cbor_int(-3, &mut cose);
    cbor_bytes(point.y().expect("y coordinate"), &mut cose);
    cose
}

fn authenticator_data(
    rp_id: &str,
    flags: u8,
    sign_count: u32,
    credential: Option<(&[u8], &[u8])>,
) -> Vec<u8> {
    use sha2::Digest;
    let mut data = Vec::new();
    data.extend_from_slice(&sha2::Sha256::digest(rp_id.as_bytes()));
    data.push(flags);
    data.extend_from_slice(&sign_count.to_be_bytes());
    if let Some((cred_id, cose)) = credential {
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&(cred_id.len() as u16).to_be_bytes());
        data.extend_from_slice(cred_id);
        data.extend_from_slice(cose);
    }
    data
}

fn none_attestation(auth_data: &[u8]) -> Vec<u8> {
    let mut object = Vec::new();
    cbor_head(5, 3, &mut object);
    cbor_text("fmt", &mut object);
    cbor_text("none", &mut object);
    cbor_text("authData", &mut object);
    cbor_bytes(auth_data, &mut object);
    cbor_text("attStmt", &mut object);
    cbor_head(5, 0, &mut object);
    object
}

fn packed_attestation(auth_data: &[u8], alg: i64, signature: &[u8], with_x5c: bool) -> Vec<u8> {
    let mut stmt = Vec::new();
    cbor_head(5, if with_x5c { 3 } else { 2 }, &mut stmt);
    cbor_text("alg", &mut stmt);
    cbor_int(alg, &mut stmt);
    cbor_text("sig", &mut stmt);
    cbor_bytes(signature, &mut stmt);
    if with_x5c {
        cbor_text("x5c", &mut stmt);
        cbor_bytes(b"untrusted-synthetic-cert", &mut stmt);
    }
    let mut object = Vec::new();
    cbor_head(5, 3, &mut object);
    cbor_text("fmt", &mut object);
    cbor_text("packed", &mut object);
    cbor_text("authData", &mut object);
    cbor_bytes(auth_data, &mut object);
    cbor_text("attStmt", &mut object);
    object.extend_from_slice(&stmt);
    object
}

fn client_json(ceremony: &str, challenge: &str, origin: &str) -> Vec<u8> {
    format!(
        r#"{{"type":"{ceremony}","challenge":"{challenge}","origin":"{origin}","crossOrigin":false}}"#
    )
    .into_bytes()
}

fn es256_sign(key: &p256::ecdsa::SigningKey, auth_data: &[u8], client_data: &[u8]) -> Vec<u8> {
    use p256::ecdsa::signature::Signer;
    use sha2::Digest;
    let mut message = Vec::new();
    message.extend_from_slice(auth_data);
    message.extend_from_slice(&sha2::Sha256::digest(client_data));
    let signature: p256::ecdsa::Signature = key.sign(&message);
    signature.to_der().as_bytes().to_vec()
}

fn ceremony_expect(challenge: &str) -> webauthn::Expect<'_> {
    webauthn::Expect {
        rp_id: RP,
        origin: ORIGIN,
        challenge,
    }
}

// --- Full passkey flows through the public handler API ---------------------------------

/// Fresh signup through register/start + register/finish. Returns the session
/// cookie, the credential id, and the signing key seed for later assertions.
fn register_flow(
    harness: &Harness,
    email: &str,
    key_seed: u8,
    invite_code: Option<&str>,
) -> (String, String) {
    let response = harness.call(
        "POST",
        "/v1/auth/passkey/register/start",
        serde_json::json!({"email": email}),
    );
    assert_eq!(response.status, 200, "{email}: {}", response.body);
    let start: serde_json::Value =
        serde_json::from_str(&response.body).expect("start shape is JSON");
    let challenge = start["challenge"].as_str().expect("challenge").to_string();
    let magic = harness
        .mailer
        .sent
        .borrow()
        .iter()
        .rev()
        .find(|(address, _)| address == email)
        .expect("signup mails the inbox-proof token")
        .1
        .clone();

    let key = test_signing_key(key_seed);
    let cred_id = format!("synthetic-cred-{key_seed}-{email}").into_bytes();
    let auth_data = authenticator_data(RP, 0x45, 0, Some((&cred_id, &es256_cose(&key))));
    let client_data = client_json("webauthn.create", &challenge, ORIGIN);
    let response = harness.call(
        "POST",
        "/v1/auth/passkey/register/finish",
        serde_json::json!({
            "challenge": challenge,
            "credential_id": b64::encode(&cred_id),
            "attestation_object": b64::encode(&none_attestation(&auth_data)),
            "client_data_json": b64::encode(&client_data),
            "magic_token": magic,
            "invite_code": invite_code,
        }),
    );
    assert_eq!(response.status, 200, "{email}: {}", response.body);
    (cookie_of(&response), b64::encode(&cred_id))
}

fn login_flow(
    harness: &Harness,
    email: &str,
    key_seed: u8,
    cred_id_b64: &str,
    sign_count: u32,
) -> String {
    let response = harness.call(
        "POST",
        "/v1/auth/passkey/login/start",
        serde_json::json!({"email": email}),
    );
    assert_eq!(response.status, 200, "{email}: {}", response.body);
    let start: serde_json::Value =
        serde_json::from_str(&response.body).expect("start shape is JSON");
    let challenge = start["challenge"].as_str().expect("challenge").to_string();

    let key = test_signing_key(key_seed);
    let auth_data = authenticator_data(RP, 0x05, sign_count, None);
    let client_data = client_json("webauthn.get", &challenge, ORIGIN);
    let signature = es256_sign(&key, &auth_data, &client_data);
    let response = harness.call(
        "POST",
        "/v1/auth/passkey/login/finish",
        serde_json::json!({
            "challenge": challenge,
            "credential_id": cred_id_b64,
            "authenticator_data": b64::encode(&auth_data),
            "client_data_json": b64::encode(&client_data),
            "signature": b64::encode(&signature),
        }),
    );
    assert_eq!(response.status, 200, "{email}: {}", response.body);
    cookie_of(&response)
}

// --- Rotation window (finding: rotation-previous-secret-unwired) ------------------------

#[test]
fn config_rejects_short_previous_secret() {
    assert!(
        handlers::Config::new(
            RP,
            ORIGIN,
            PRIMARY_SECRET.to_vec(),
            [7u8; 32],
            1,
            3600,
            false,
            false,
            Some(vec![0x57u8; 16]),
        )
        .is_err(),
        "a short previous secret must fail closed, not narrow the window"
    );
    assert!(handlers::Config::new(
        RP,
        ORIGIN,
        PRIMARY_SECRET.to_vec(),
        [7u8; 32],
        1,
        3600,
        false,
        false,
        Some(PREVIOUS_SECRET.to_vec()),
    )
    .is_ok());
    assert!(handlers::Config::new(
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
    .is_ok());
}

#[test]
fn previous_secret_authenticates_and_refresh_migrates_to_primary() {
    // Rotation window: the primary mints, both verify. A pre-cutover token
    // (minted under the previous secret) must still authenticate, and a
    // refresh must re-sign it with the primary.
    let harness = Harness {
        config: test_config(false, Some(PREVIOUS_SECRET.to_vec())),
        ..harness()
    };
    let old_token =
        session::issue_session(PREVIOUS_SECRET, "rotation-user", NOW, 3600).expect("old token");
    let claims = session::verify_session(PREVIOUS_SECRET, &old_token, NOW).expect("old verifies");
    harness.store.seed_session_row(session::SessionRecord {
        id: claims.session_id.clone(),
        user_id: "rotation-user".into(),
        created_at: NOW,
        expires_at: NOW + 3600,
        rotation_counter: 0,
        revoked: false,
    });
    let cookie = format!("{}={}", session::COOKIE_NAME, old_token);

    let response = harness.call_full("GET", "/v1/vault", None, Some(cookie.clone()), None);
    assert_eq!(
        response.status, 200,
        "pre-cutover token must authenticate inside the rotation window: {}",
        response.body
    );

    let response = harness.call_full(
        "POST",
        "/v1/session/refresh",
        Some(serde_json::json!({}).to_string()),
        Some(cookie),
        Some(ORIGIN),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let migrated = cookie_of(&response);
    let migrated_token = migrated
        .split_once('=')
        .expect("cookie value")
        .1
        .to_string();
    assert!(
        session::verify_session(PRIMARY_SECRET, &migrated_token, NOW).is_ok(),
        "refreshed token must verify under the primary secret"
    );

    // The superseded pre-cutover token no longer matches the advanced row.
    let stale = harness.call_full(
        "GET",
        "/v1/vault",
        None,
        Some(format!("{}={}", session::COOKIE_NAME, old_token)),
        None,
    );
    assert_eq!(stale.status, 401, "{}", stale.body);
    assert_eq!(stale.body, GENERIC_AUTH_BODY);
}

#[test]
fn previous_secret_rejected_in_steady_state() {
    // No rotation window configured: only the primary verifies.
    let harness = harness();
    let old_token =
        session::issue_session(PREVIOUS_SECRET, "rotation-user", NOW, 3600).expect("old token");
    let claims = session::verify_session(PREVIOUS_SECRET, &old_token, NOW).expect("old verifies");
    harness.store.seed_session_row(session::SessionRecord {
        id: claims.session_id,
        user_id: "rotation-user".into(),
        created_at: NOW,
        expires_at: NOW + 3600,
        rotation_counter: 0,
        revoked: false,
    });
    let response = harness.call_full(
        "GET",
        "/v1/vault",
        None,
        Some(format!("{}={}", session::COOKIE_NAME, old_token)),
        None,
    );
    assert_eq!(response.status, 401, "{}", response.body);
    assert_eq!(response.body, GENERIC_AUTH_BODY);
}

// --- Shared codec contract (finding: secret-codec-flexible-vs-strict-b64) ---------------

#[test]
fn shared_codec_rejects_padded_and_std_alphabet_secrets() {
    // The single base64url codec every secret decode must go through:
    // unpadded alphabet only. A padded standard-base64 secret that a loose
    // decoder would accept must be rejected here.
    for rejected in [
        "Zm9v=", "Zm9v==", "Zm+9", "Zm/9", "Zm 9", "Zm\n9", "a", "abcde", "!!!",
    ] {
        assert!(
            b64::decode(rejected).is_err(),
            "loose input accepted: {rejected:?}"
        );
    }
    assert_eq!(b64::decode("Zm9v").expect("unpadded"), b"foo");
    assert_eq!(b64::decode("").expect("empty"), b"");
    let secret = vec![0x51u8; 40];
    assert_eq!(
        b64::decode(&b64::encode(&secret)).expect("round trip"),
        secret
    );
}

// --- Plaintext opt-in audit (finding: late-novel-plaintext-optin-audit-action) ----------

#[test]
fn plaintext_opt_in_save_audits_insecure_action() {
    let harness = harness();
    let cookie = magic_cookie(&harness, "plain-user", "plain@example.com");

    let response = put_vault(&harness, &cookie, plaintext_opt_in_provider(), None);
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(
        harness
            .store
            .audits_with(vault::INSECURE_PLAINTEXT_AUDIT_ACTION),
        1,
        "cleartext opt-in save must be repudiation-safe, not plain vault.update"
    );
    assert_eq!(harness.store.audits_with("vault.update"), 0);

    let cookie = magic_cookie(&harness, "tls-user", "tls@example.com");
    let response = put_vault(&harness, &cookie, public_provider(), None);
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(harness.store.audits_with("vault.update"), 1);
}

// --- Invite throttle (finding: late-novel-invite-redemption-unthrottled) -----------------

#[test]
fn invite_math_matches_documented_budget() {
    assert_eq!(auth::INVITE_REDEMPTION_MAX_ATTEMPTS, 10);
    assert_eq!(auth::INVITE_REDEMPTION_WINDOW_SECS, 300);
    let full: Vec<u64> = (0..10).map(|offset| NOW - offset).collect();
    assert!(
        !auth::invite_attempt_allowed(&full, NOW),
        "10 attempts in the window must trip the limit"
    );
    let nine: Vec<u64> = (0..9).map(|offset| NOW - offset).collect();
    assert!(
        auth::invite_attempt_allowed(&nine, NOW),
        "9 attempts in the window must pass"
    );
}

#[test]
fn invite_budget_denies_eleventh_attempt_with_generic_error() {
    let harness = Harness {
        config: test_config(true, None),
        ..harness()
    };
    let code = auth::generate_invite_code();
    harness.store.seed_invite(&code);

    // Positive control first: a fresh account redeems the same code shape.
    let (control_cookie, _) = register_flow(&harness, "control@example.com", 11, Some(&code));
    assert!(!control_cookie.is_empty());

    // Exhaust the per-client redemption budget for the target account via
    // the public atomic reservation (same scope/action the handler uses).
    let response = harness.call(
        "POST",
        "/v1/auth/passkey/register/start",
        serde_json::json!({"email": "throttled@example.com"}),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let start: serde_json::Value =
        serde_json::from_str(&response.body).expect("start shape is JSON");
    let user_id = start["user_id"].as_str().expect("user id").to_string();
    let attempt_key = format!("{SOURCE_IP}:{user_id}");
    let attempt_key = format!("invite:{attempt_key}");
    for _ in 0..u64::from(auth::INVITE_REDEMPTION_MAX_ATTEMPTS) {
        assert!(
            block_on(harness.store.reserve_rate_limit(
                &attempt_key,
                "auth.invite_redeem",
                u64::from(auth::INVITE_REDEMPTION_MAX_ATTEMPTS),
                NOW,
                auth::INVITE_REDEMPTION_WINDOW_SECS,
            ))
            .expect("reservation works"),
            "prefill must fill the budget"
        );
    }

    // One more attempt with a VALID code is still denied, generically.
    let fresh_code = auth::generate_invite_code();
    harness.store.seed_invite(&fresh_code);
    let magic = harness
        .mailer
        .sent
        .borrow()
        .iter()
        .rev()
        .find(|(address, _)| address == "throttled@example.com")
        .expect("signup mailed")
        .1
        .clone();
    let challenge = start["challenge"].as_str().expect("challenge").to_string();
    let key = test_signing_key(12);
    let cred_id = b"throttled-cred".to_vec();
    let auth_data = authenticator_data(RP, 0x45, 0, Some((&cred_id, &es256_cose(&key))));
    let client_data = client_json("webauthn.create", &challenge, ORIGIN);
    let response = harness.call(
        "POST",
        "/v1/auth/passkey/register/finish",
        serde_json::json!({
            "challenge": challenge,
            "credential_id": b64::encode(&cred_id),
            "attestation_object": b64::encode(&none_attestation(&auth_data)),
            "client_data_json": b64::encode(&client_data),
            "magic_token": magic,
            "invite_code": fresh_code,
        }),
    );
    assert_eq!(
        response.status, 401,
        "over-budget redemption with a valid code must be denied: {}",
        response.body
    );
    assert_eq!(response.body, GENERIC_AUTH_BODY);
    assert!(response.set_cookie.is_none(), "no session on denial");
    assert!(
        block_on(harness.store.credentials_for_user(&user_id))
            .expect("lookup works")
            .is_empty(),
        "no credential may be enrolled past the invite budget"
    );
}

// --- Test-connection budget (finding: late-novel-test-connection-limit-10-vs-5) ----------

#[test]
fn test_connection_enforces_shared_five_per_minute_budget() {
    assert_eq!(vault::TEST_CONNECTION_MAX_ATTEMPTS, 5);
    assert_eq!(vault::TEST_CONNECTION_WINDOW_SECS, 60);
    let harness = harness();
    let cookie = magic_cookie(&harness, "budget-user", "budget@example.com");
    let saved = put_vault(&harness, &cookie, public_provider(), None);
    assert_eq!(saved.status, 200, "{}", saved.body);

    for attempt in 1..=5u32 {
        let response = harness.call_full(
            "POST",
            "/v1/vault/test-connection",
            None,
            Some(cookie.clone()),
            Some(ORIGIN),
        );
        assert_eq!(response.status, 200, "attempt {attempt}: {}", response.body);
        assert!(
            response.body.contains(r#""ok":true"#),
            "attempt {attempt} must be admitted: {}",
            response.body
        );
    }
    assert_eq!(harness.store.audits_with("vault.test"), 5);
    assert_eq!(harness.dialer.seen.borrow().len(), 5);

    // Sixth attempt inside the window: denied before any dial, and the
    // denial itself is not a new audit row.
    let denied = harness.call_full(
        "POST",
        "/v1/vault/test-connection",
        None,
        Some(cookie),
        Some(ORIGIN),
    );
    assert_eq!(denied.status, 200, "{}", denied.body);
    assert!(
        denied.body.contains(r#""ok":false"#),
        "over-budget attempt must fail closed: {}",
        denied.body
    );
    assert_eq!(
        harness.store.audits_with("vault.test"),
        5,
        "denial must not mint an audit row"
    );
    assert_eq!(
        harness.dialer.seen.borrow().len(),
        5,
        "denial must not reach the dialer"
    );
}

#[test]
fn test_connection_lease_cap_blocks_third_concurrent_probe() {
    let harness = harness();
    let cookie = magic_cookie(&harness, "lease-user", "lease@example.com");
    let saved = put_vault(&harness, &cookie, public_provider(), None);
    assert_eq!(saved.status, 200, "{}", saved.body);

    // Saturate both concurrency leases out of band: the handler's own
    // acquisition must then fail and the probe must never run.
    for (slot, token) in ["seed-token-a", "seed-token-b"].iter().enumerate() {
        assert!(
            block_on(harness.store.acquire_test_connection_lease(
                "lease-user",
                token,
                20,
                vault::MAX_TEST_CONCURRENCY,
            ))
            .expect("lease seeds"),
            "lease slot {slot} must be seeded"
        );
    }
    let response = harness.call_full(
        "POST",
        "/v1/vault/test-connection",
        None,
        Some(cookie),
        Some(ORIGIN),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(
        response.body.contains(r#""ok":false"#),
        "lease exhaustion must fail closed: {}",
        response.body
    );
    assert!(
        harness.dialer.seen.borrow().is_empty(),
        "no dial without a lease"
    );
}

// --- Purpose confusion (round-2 regression) ------------------------------------------------

#[test]
fn register_finish_rejects_login_purpose_challenge() {
    // An attacker's key must not be enrollable on a victim account via a
    // login ceremony challenge, even with a valid inbox-proof token.
    let harness = harness();
    let login_start = harness.call(
        "POST",
        "/v1/auth/passkey/login/start",
        serde_json::json!({"email": "victim@example.com"}),
    );
    assert_eq!(login_start.status, 200, "{}", login_start.body);
    let login: serde_json::Value =
        serde_json::from_str(&login_start.body).expect("start shape is JSON");
    let login_challenge = login["challenge"].as_str().expect("challenge").to_string();

    let register_start = harness.call(
        "POST",
        "/v1/auth/passkey/register/start",
        serde_json::json!({"email": "victim@example.com"}),
    );
    assert_eq!(register_start.status, 200, "{}", register_start.body);
    let register: serde_json::Value =
        serde_json::from_str(&register_start.body).expect("start shape is JSON");
    let user_id = register["user_id"].as_str().expect("user id").to_string();
    let magic = harness
        .mailer
        .sent
        .borrow()
        .iter()
        .rev()
        .find(|(address, _)| address == "victim@example.com")
        .expect("signup mailed")
        .1
        .clone();

    let key = test_signing_key(21);
    let cred_id = b"attacker-cred".to_vec();
    let auth_data = authenticator_data(RP, 0x45, 0, Some((&cred_id, &es256_cose(&key))));
    let client_data = client_json("webauthn.create", &login_challenge, ORIGIN);
    let response = harness.call(
        "POST",
        "/v1/auth/passkey/register/finish",
        serde_json::json!({
            "challenge": login_challenge,
            "credential_id": b64::encode(&cred_id),
            "attestation_object": b64::encode(&none_attestation(&auth_data)),
            "client_data_json": b64::encode(&client_data),
            "magic_token": magic,
            "invite_code": null,
        }),
    );
    assert_eq!(
        response.status, 401,
        "login-purpose challenge at registration must be rejected: {}",
        response.body
    );
    assert_eq!(response.body, GENERIC_AUTH_BODY);
    assert!(response.set_cookie.is_none());
    assert!(
        block_on(harness.store.credentials_for_user(&user_id))
            .expect("lookup works")
            .is_empty(),
        "purpose confusion must enroll nothing"
    );
}

#[test]
fn login_finish_rejects_register_purpose_challenge() {
    // The mirror direction: a registration challenge presented at login
    // must not authenticate, even with an otherwise-valid assertion.
    let harness = harness();
    let (cookie, cred_id_b64) = register_flow(&harness, "owner@example.com", 22, None);
    assert!(!cookie.is_empty());
    let owner = block_on(harness.store.user_by_email("owner@example.com"))
        .expect("lookup works")
        .expect("owner exists");

    let rogue_challenge = auth::new_registration_challenge(RP, ORIGIN, NOW).expect("challenge");
    let rogue_id = rogue_challenge.id.clone();
    block_on(harness.store.insert_challenge(
        &rogue_challenge,
        Some(&owner.id),
        handlers::PURPOSE_REGISTER,
    ))
    .expect("rogue challenge stored");

    let key = test_signing_key(22);
    let auth_data = authenticator_data(RP, 0x05, 1, None);
    let client_data = client_json("webauthn.get", &rogue_id, ORIGIN);
    let signature = es256_sign(&key, &auth_data, &client_data);
    let response = harness.call(
        "POST",
        "/v1/auth/passkey/login/finish",
        serde_json::json!({
            "challenge": rogue_id,
            "credential_id": cred_id_b64,
            "authenticator_data": b64::encode(&auth_data),
            "client_data_json": b64::encode(&client_data),
            "signature": b64::encode(&signature),
        }),
    );
    assert_eq!(
        response.status, 401,
        "register-purpose challenge at login must be rejected: {}",
        response.body
    );
    assert_eq!(response.body, GENERIC_AUTH_BODY);
    assert!(response.set_cookie.is_none());

    // Control: the same credential completes a real login ceremony.
    let control = login_flow(&harness, "owner@example.com", 22, &cred_id_b64, 2);
    assert!(!control.is_empty());
}

// --- Inbox proof (round-2 regression) -------------------------------------------------------

#[test]
fn register_finish_without_inbox_proof_enrolls_nothing() {
    let harness = harness();
    let response = harness.call(
        "POST",
        "/v1/auth/passkey/register/start",
        serde_json::json!({"email": "fresh@example.com"}),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let start: serde_json::Value =
        serde_json::from_str(&response.body).expect("start shape is JSON");
    let challenge = start["challenge"].as_str().expect("challenge").to_string();
    let user_id = start["user_id"].as_str().expect("user id").to_string();

    let key = test_signing_key(23);
    let cred_id = b"fresh-cred".to_vec();
    let auth_data = authenticator_data(RP, 0x45, 0, Some((&cred_id, &es256_cose(&key))));
    let client_data = client_json("webauthn.create", &challenge, ORIGIN);
    let response = harness.call(
        "POST",
        "/v1/auth/passkey/register/finish",
        serde_json::json!({
            "challenge": challenge,
            "credential_id": b64::encode(&cred_id),
            "attestation_object": b64::encode(&none_attestation(&auth_data)),
            "client_data_json": b64::encode(&client_data),
            "magic_token": "not-the-emailed-token",
            "invite_code": null,
        }),
    );
    assert_eq!(response.status, 401, "{}", response.body);
    assert_eq!(response.body, GENERIC_AUTH_BODY);
    assert!(response.set_cookie.is_none());
    assert!(
        block_on(harness.store.credentials_for_user(&user_id))
            .expect("lookup works")
            .is_empty(),
        "no credential may be enrolled without inbox proof"
    );
}

// --- Enumeration shape (round-2 regression) --------------------------------------------------

#[test]
fn auth_starts_keep_fixed_shape_for_known_and_unknown() {
    let harness = harness();
    seed_user(&harness, "known-user", "known@example.com");
    block_on(harness.store.insert_credential(&auth::PasskeyCredential {
        cred_id_b64url: b64::encode(b"known-cred"),
        user_id: "known-user".to_string(),
        public_key: es256_cose(&test_signing_key(24)),
        sign_count: 0,
        registered_at: NOW,
    }))
    .expect("seed credential");

    // register/start: identical 200 shape for taken and fresh emails, mail
    // only for the fresh one, and a stable stand-in id for the taken one.
    let mut taken_ids = Vec::new();
    for email in [
        "known@example.com",
        "fresh-shape@example.com",
        "known@example.com",
    ] {
        let response = harness.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": email}),
        );
        assert_eq!(response.status, 200, "{email}: {}", response.body);
        let body: serde_json::Value =
            serde_json::from_str(&response.body).expect("start shape is JSON");
        assert!(body["challenge"].is_string(), "{email}");
        assert!(body["user_id"].is_string(), "{email}");
        assert_eq!(body["rp_id"], serde_json::json!(RP), "{email}");
        if email == "known@example.com" {
            taken_ids.push(body["user_id"].clone());
        }
    }
    assert_eq!(taken_ids[0], taken_ids[1], "stand-in id must be stable");
    assert_eq!(harness.mailer.sent.borrow().len(), 1);
    assert_eq!(harness.mailer.sent.borrow()[0].0, "fresh-shape@example.com");

    // login/start is usernameless: no credential ids for anyone.
    for email in ["ghost@example.com", "known@example.com"] {
        let response = harness.call(
            "POST",
            "/v1/auth/passkey/login/start",
            serde_json::json!({"email": email}),
        );
        assert_eq!(response.status, 200, "{email}: {}", response.body);
        let body: serde_json::Value =
            serde_json::from_str(&response.body).expect("start shape is JSON");
        assert_eq!(
            body["allow_credentials"],
            serde_json::json!([]),
            "{email}: allow list must stay empty"
        );
    }

    // magic/start: identical shape for unknown emails, nothing mailed.
    let response = harness.call(
        "POST",
        "/v1/auth/magic/start",
        serde_json::json!({"email": "nobody@example.com"}),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    assert_eq!(response.body, serde_json::json!({"ok": true}).to_string());
    assert!(
        harness
            .mailer
            .sent
            .borrow()
            .iter()
            .all(|(address, _)| address != "nobody@example.com"),
        "unknown email must receive no mail"
    );
}

// --- Single-use replay and clone detection (rounds 1-2 regressions) -------------------------

#[test]
fn magic_finish_rejects_replay() {
    let harness = harness();
    seed_user(&harness, "replay-user", "replay@example.com");
    let first = harness.call(
        "POST",
        "/v1/auth/magic/start",
        serde_json::json!({"email": "replay@example.com"}),
    );
    assert_eq!(first.status, 200, "{}", first.body);
    let token = harness
        .mailer
        .sent
        .borrow()
        .last()
        .expect("mailed")
        .1
        .clone();
    let login = harness.call(
        "POST",
        "/v1/auth/magic/finish",
        serde_json::json!({"token": token}),
    );
    assert_eq!(login.status, 200, "{}", login.body);
    let replay = harness.call(
        "POST",
        "/v1/auth/magic/finish",
        serde_json::json!({"token": token}),
    );
    assert_eq!(replay.status, 401, "{}", replay.body);
    assert_eq!(replay.body, GENERIC_AUTH_BODY);
    assert!(replay.set_cookie.is_none());
}

#[test]
fn register_finish_rejects_replayed_challenge() {
    let harness = harness();
    let start = harness.call(
        "POST",
        "/v1/auth/passkey/register/start",
        serde_json::json!({"email": "double@example.com"}),
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let body: serde_json::Value = serde_json::from_str(&start.body).expect("JSON");
    let challenge = body["challenge"].as_str().expect("challenge").to_string();
    let magic = harness
        .mailer
        .sent
        .borrow()
        .last()
        .expect("mailed")
        .1
        .clone();

    let key = test_signing_key(25);
    let cred_id = b"double-cred".to_vec();
    let auth_data = authenticator_data(RP, 0x45, 0, Some((&cred_id, &es256_cose(&key))));
    let client_data = client_json("webauthn.create", &challenge, ORIGIN);
    let finish = serde_json::json!({
        "challenge": challenge,
        "credential_id": b64::encode(&cred_id),
        "attestation_object": b64::encode(&none_attestation(&auth_data)),
        "client_data_json": b64::encode(&client_data),
        "magic_token": magic,
        "invite_code": null,
    });
    let first = harness.call("POST", "/v1/auth/passkey/register/finish", finish.clone());
    assert_eq!(first.status, 200, "{}", first.body);
    let second = harness.call("POST", "/v1/auth/passkey/register/finish", finish);
    assert_eq!(second.status, 401, "{}", second.body);
    assert_eq!(second.body, GENERIC_AUTH_BODY);
    assert!(second.set_cookie.is_none());
}

#[test]
fn login_finish_rejects_replayed_assertion_clone() {
    let harness = harness();
    let (_, cred_id_b64) = register_flow(&harness, "clone@example.com", 26, None);

    // First login advances the stored counter 0 -> 1.
    let cookie = login_flow(&harness, "clone@example.com", 26, &cred_id_b64, 1);
    assert!(!cookie.is_empty());

    // Replaying the identical assertion (same counter) is clone evidence.
    let start = harness.call(
        "POST",
        "/v1/auth/passkey/login/start",
        serde_json::json!({"email": "clone@example.com"}),
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let body: serde_json::Value = serde_json::from_str(&start.body).expect("JSON");
    let challenge = body["challenge"].as_str().expect("challenge").to_string();
    let key = test_signing_key(26);
    let auth_data = authenticator_data(RP, 0x05, 1, None);
    let client_data = client_json("webauthn.get", &challenge, ORIGIN);
    let signature = es256_sign(&key, &auth_data, &client_data);
    let replay = harness.call(
        "POST",
        "/v1/auth/passkey/login/finish",
        serde_json::json!({
            "challenge": challenge,
            "credential_id": cred_id_b64,
            "authenticator_data": b64::encode(&auth_data),
            "client_data_json": b64::encode(&client_data),
            "signature": b64::encode(&signature),
        }),
    );
    assert_eq!(replay.status, 401, "{}", replay.body);
    assert_eq!(replay.body, GENERIC_AUTH_BODY);
    assert!(replay.set_cookie.is_none(), "clone gets no session");
}

// --- Body cap (round-2 regression) ------------------------------------------------------------

#[test]
fn oversized_body_rejected_before_routing() {
    let harness = harness();
    let big = "9".repeat(33 * 1024);
    let response = harness.call_full("PUT", "/v1/vault", Some(big), None, Some(ORIGIN));
    assert_eq!(response.status, 400, "{}", response.body);
    assert_eq!(response.body, r#"{"error":"invalid request"}"#);
    assert!(
        harness.store.vaults.borrow().is_empty(),
        "oversized body must not reach storage"
    );
}

// --- Challenge-row growth (rounds 1-2 regressions) ----------------------------------------------

#[test]
fn login_start_budget_stores_no_challenge_past_cap() {
    let harness = harness();
    for _ in 0..10 {
        let response = harness.call(
            "POST",
            "/v1/auth/passkey/login/start",
            serde_json::json!({"email": "ghost-budget@example.com"}),
        );
        assert_eq!(response.status, 200, "{}", response.body);
    }
    let stored = harness.store.challenge_count();
    assert_eq!(stored, 10, "one row per admitted start");
    for _ in 0..2 {
        let response = harness.call(
            "POST",
            "/v1/auth/passkey/login/start",
            serde_json::json!({"email": "ghost-budget@example.com"}),
        );
        assert_eq!(response.status, 200, "{}", response.body);
        let body: serde_json::Value = serde_json::from_str(&response.body).expect("JSON");
        assert!(body["challenge"].is_string());
        assert_eq!(body["allow_credentials"], serde_json::json!([]));
    }
    assert_eq!(
        harness.store.challenge_count(),
        stored,
        "over-budget starts keep the shape but store nothing"
    );
}

#[test]
fn register_start_for_taken_account_stores_nothing() {
    let harness = harness();
    seed_user(&harness, "taken-user", "taken@example.com");
    block_on(harness.store.insert_credential(&auth::PasskeyCredential {
        cred_id_b64url: b64::encode(b"taken-cred"),
        user_id: "taken-user".to_string(),
        public_key: es256_cose(&test_signing_key(27)),
        sign_count: 0,
        registered_at: NOW,
    }))
    .expect("seed credential");

    let mut stand_ins = Vec::new();
    for _ in 0..3 {
        let response = harness.call(
            "POST",
            "/v1/auth/passkey/register/start",
            serde_json::json!({"email": "taken@example.com"}),
        );
        assert_eq!(response.status, 200, "{}", response.body);
        let body: serde_json::Value = serde_json::from_str(&response.body).expect("JSON");
        stand_ins.push(body["user_id"].clone());
    }
    assert_eq!(stand_ins[0], stand_ins[1]);
    assert_eq!(stand_ins[1], stand_ins[2]);
    assert_eq!(
        harness.store.challenge_count(),
        0,
        "taken-account starts store no challenge"
    );
    assert!(
        harness.mailer.sent.borrow().is_empty(),
        "taken-account starts send no mail"
    );
}

// --- Cookie parsing (round-1 regression) ---------------------------------------------------------

#[test]
fn nameless_cookie_does_not_break_session_auth() {
    let harness = harness();
    let cookie = magic_cookie(&harness, "cookie-user", "cookie@example.com");
    // Browsers send nameless cookies; a malformed part before the session
    // cookie must be skipped, not abort the whole header.
    let response = harness.call_full(
        "GET",
        "/v1/vault",
        None,
        Some(format!("nameless-part; {cookie}")),
        None,
    );
    assert_eq!(response.status, 200, "{}", response.body);
}

// --- Vault SSRF and validation wiring ----------------------------------------------------------------

#[test]
fn vault_put_rejects_private_dns_answers_at_save() {
    let harness = harness();
    let cookie = magic_cookie(&harness, "ssrf-user", "ssrf@example.com");

    let private_provider = serde_json::json!({
        "host": LOOPBACK_HOST,
        "port": 563,
        "tls": true,
        "username": "synthetic-user",
        "password": "synthetic-pass",
        "max_connections": 4
    });
    let response = put_vault(&harness, &cookie, private_provider, None);
    assert_eq!(response.status, 400, "{}", response.body);
    assert_eq!(response.body, GENERIC_VAULT_BODY);

    let literal_provider = serde_json::json!({
        "host": "127.0.0.1",
        "port": 563,
        "tls": true,
        "username": "synthetic-user",
        "password": "synthetic-pass",
        "max_connections": 4
    });
    let response = put_vault(&harness, &cookie, literal_provider, None);
    assert_eq!(response.status, 400, "{}", response.body);

    let saved = harness.call_full("GET", "/v1/vault", None, Some(cookie.clone()), None);
    assert_eq!(saved.status, 200, "{}", saved.body);
    let summary: serde_json::Value = serde_json::from_str(&saved.body).expect("JSON");
    assert_eq!(
        summary["provider_configured"],
        serde_json::json!(false),
        "rejected saves store nothing"
    );

    // Control: the public-host save succeeds through the same path.
    let good = put_vault(&harness, &cookie, public_provider(), None);
    assert_eq!(good.status, 200, "{}", good.body);
}

#[test]
fn vault_put_rejects_cleartext_indexer_url() {
    let harness = harness();
    let cookie = magic_cookie(&harness, "indexer-user", "indexer@example.com");
    let indexer = serde_json::json!({
        "url": format!("http://{INDEXER_HOST}/api"),
        "api_key": "synthetic-indexer-key"
    });
    let response = put_vault(&harness, &cookie, public_provider(), Some(indexer));
    assert_eq!(response.status, 400, "{}", response.body);
    assert_eq!(response.body, GENERIC_VAULT_BODY);
}

// --- WebAuthn ceremony edges (through the public webauthn API) ---------------------------------------

const FIXTURE_CHALLENGE: &str = "synthetic-fixture-challenge";

fn fixture_registration(key_seed: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let key = test_signing_key(key_seed);
    let cred_id = format!("fixture-cred-{key_seed}").into_bytes();
    let cose = es256_cose(&key);
    let auth_data = authenticator_data(RP, 0x45, 0, Some((&cred_id, &cose)));
    (cred_id, cose, auth_data)
}

#[test]
fn packed_self_attestation_verifies_with_matching_alg() {
    let (_, _, auth_data) = fixture_registration(31);
    let key = test_signing_key(31);
    let client_data = client_json("webauthn.create", FIXTURE_CHALLENGE, ORIGIN);
    let signature = es256_sign(&key, &auth_data, &client_data);
    let object = packed_attestation(&auth_data, -7, &signature, false);
    let credential =
        webauthn::verify_attestation(&object, &client_data, &ceremony_expect(FIXTURE_CHALLENGE))
            .expect("packed self-attestation verifies");
    assert_eq!(credential.sign_count, 0);
    assert!(!credential.cred_id.is_empty());
    assert!(!credential.public_key_cose.is_empty());
}

#[test]
fn attestation_rejects_x5c_chain_without_trust_store() {
    // No CA trust store exists yet: any attestation carrying an x5c chain
    // must be rejected, even with an otherwise-valid signature.
    let (_, _, auth_data) = fixture_registration(32);
    let key = test_signing_key(32);
    let client_data = client_json("webauthn.create", FIXTURE_CHALLENGE, ORIGIN);
    let signature = es256_sign(&key, &auth_data, &client_data);
    let object = packed_attestation(&auth_data, -7, &signature, true);
    assert_eq!(
        webauthn::verify_attestation(&object, &client_data, &ceremony_expect(FIXTURE_CHALLENGE)),
        Err(webauthn::WebAuthnError::Invalid),
        "x5c chain must be rejected without a trust store"
    );
}

#[test]
fn attestation_rejects_alg_mismatch() {
    let (_, _, auth_data) = fixture_registration(33);
    let key = test_signing_key(33);
    let client_data = client_json("webauthn.create", FIXTURE_CHALLENGE, ORIGIN);
    let signature = es256_sign(&key, &auth_data, &client_data);
    let object = packed_attestation(&auth_data, -8, &signature, false);
    assert_eq!(
        webauthn::verify_attestation(&object, &client_data, &ceremony_expect(FIXTURE_CHALLENGE)),
        Err(webauthn::WebAuthnError::Invalid),
        "attestation alg must match the credential key alg"
    );
}

#[test]
fn attestation_rejects_wrong_challenge_origin_type_and_rp() {
    let (_, _, auth_data) = fixture_registration(34);
    let object = none_attestation(&auth_data);
    let good_client = client_json("webauthn.create", FIXTURE_CHALLENGE, ORIGIN);
    // Positive control: the exact ceremony verifies.
    assert!(webauthn::verify_attestation(
        &object,
        &good_client,
        &ceremony_expect(FIXTURE_CHALLENGE)
    )
    .is_ok());
    for (label, client_data, expect) in [
        (
            "wrong challenge",
            client_json("webauthn.create", "another-challenge", ORIGIN),
            ceremony_expect(FIXTURE_CHALLENGE),
        ),
        (
            "wrong origin",
            client_json("webauthn.create", FIXTURE_CHALLENGE, "https://evil.example.com"),
            ceremony_expect(FIXTURE_CHALLENGE),
        ),
        (
            "wrong ceremony type",
            client_json("webauthn.get", FIXTURE_CHALLENGE, ORIGIN),
            ceremony_expect(FIXTURE_CHALLENGE),
        ),
        (
            "cross-origin flag",
            br#"{"type":"webauthn.create","challenge":"synthetic-fixture-challenge","origin":"https://passkey.example.com","crossOrigin":true}"#.to_vec(),
            ceremony_expect(FIXTURE_CHALLENGE),
        ),
    ] {
        assert_eq!(
            webauthn::verify_attestation(&object, &client_data, &expect),
            Err(webauthn::WebAuthnError::Invalid),
            "{label} must be rejected"
        );
    }
    // Wrong RP: the rpIdHash no longer matches.
    let other_rp = webauthn::Expect {
        rp_id: "other.example.com",
        origin: ORIGIN,
        challenge: FIXTURE_CHALLENGE,
    };
    assert_eq!(
        webauthn::verify_attestation(&object, &good_client, &other_rp),
        Err(webauthn::WebAuthnError::Invalid),
        "wrong rp id must be rejected"
    );
}

#[test]
fn attestation_rejects_none_with_attstmt_and_unknown_fmt() {
    let (_, _, auth_data) = fixture_registration(35);
    let client_data = client_json("webauthn.create", FIXTURE_CHALLENGE, ORIGIN);

    // `none` with a non-empty attStmt is malformed.
    let mut stmt = Vec::new();
    cbor_head(5, 1, &mut stmt);
    cbor_text("extra", &mut stmt);
    cbor_int(0, &mut stmt);
    let mut object = Vec::new();
    cbor_head(5, 3, &mut object);
    cbor_text("fmt", &mut object);
    cbor_text("none", &mut object);
    cbor_text("authData", &mut object);
    cbor_bytes(&auth_data, &mut object);
    cbor_text("attStmt", &mut object);
    object.extend_from_slice(&stmt);
    assert_eq!(
        webauthn::verify_attestation(&object, &client_data, &ceremony_expect(FIXTURE_CHALLENGE)),
        Err(webauthn::WebAuthnError::Invalid),
        "none with attStmt entries must be rejected"
    );

    // Unknown formats are rejected (only none + packed self-attestation).
    let mut unknown = Vec::new();
    cbor_head(5, 3, &mut unknown);
    cbor_text("fmt", &mut unknown);
    cbor_text("apple", &mut unknown);
    cbor_text("authData", &mut unknown);
    cbor_bytes(&auth_data, &mut unknown);
    cbor_text("attStmt", &mut unknown);
    cbor_head(5, 0, &mut unknown);
    assert_eq!(
        webauthn::verify_attestation(&unknown, &client_data, &ceremony_expect(FIXTURE_CHALLENGE)),
        Err(webauthn::WebAuthnError::Invalid),
        "unknown fmt must be rejected"
    );
}

#[test]
fn attestation_rejects_oversized_and_truncated_inputs() {
    let (_, _, auth_data) = fixture_registration(36);
    let client_data = client_json("webauthn.create", FIXTURE_CHALLENGE, ORIGIN);
    let expect = ceremony_expect(FIXTURE_CHALLENGE);

    let huge = vec![0u8; 17 * 1024];
    assert_eq!(
        webauthn::verify_attestation(&huge, &client_data, &expect),
        Err(webauthn::WebAuthnError::Invalid),
        "oversized attestation object must be rejected"
    );

    // Truncated authData: the credential bytes are cut short.
    let mut truncated = Vec::new();
    cbor_head(5, 3, &mut truncated);
    cbor_text("fmt", &mut truncated);
    cbor_text("none", &mut truncated);
    cbor_text("authData", &mut truncated);
    cbor_bytes(&auth_data[..40], &mut truncated);
    cbor_text("attStmt", &mut truncated);
    cbor_head(5, 0, &mut truncated);
    assert_eq!(
        webauthn::verify_attestation(&truncated, &client_data, &expect),
        Err(webauthn::WebAuthnError::Invalid),
        "truncated authData must be rejected"
    );

    // Empty credential id is rejected.
    let empty_cred =
        authenticator_data(RP, 0x45, 0, Some((&[], &es256_cose(&test_signing_key(36)))));
    let empty_object = none_attestation(&empty_cred);
    assert_eq!(
        webauthn::verify_attestation(&empty_object, &client_data, &expect),
        Err(webauthn::WebAuthnError::Invalid),
        "empty credential id must be rejected"
    );
}

#[test]
fn assertion_enforces_clone_detection_and_counterless() {
    let key = test_signing_key(37);
    let cose = es256_cose(&key);
    let expect = ceremony_expect(FIXTURE_CHALLENGE);

    let assert_with = |stored: u32, presented: u32| {
        let auth_data = authenticator_data(RP, 0x05, presented, None);
        let client_data = client_json("webauthn.get", FIXTURE_CHALLENGE, ORIGIN);
        let signature = es256_sign(&key, &auth_data, &client_data);
        webauthn::verify_assertion(&cose, stored, &auth_data, &client_data, &signature, &expect)
    };

    // Counterless authenticators always present 0: allowed.
    assert_eq!(assert_with(0, 0), Ok(0));
    // Strict increase once either side has counted.
    assert_eq!(assert_with(0, 1), Ok(1));
    assert_eq!(
        assert_with(1, 1),
        Err(webauthn::WebAuthnError::Invalid),
        "replayed counter is clone evidence"
    );
    assert_eq!(
        assert_with(5, 3),
        Err(webauthn::WebAuthnError::Invalid),
        "decreased counter is clone evidence"
    );

    // Wrong challenge never verifies, even with a valid signature shape.
    let auth_data = authenticator_data(RP, 0x05, 2, None);
    let bad_client = client_json("webauthn.get", "another-challenge", ORIGIN);
    let signature = es256_sign(&key, &auth_data, &bad_client);
    assert_eq!(
        webauthn::verify_assertion(&cose, 1, &auth_data, &bad_client, &signature, &expect),
        Err(webauthn::WebAuthnError::Invalid),
        "wrong challenge must be rejected"
    );
}

// --- PR #88 auth verification and challenge-purpose guard -------------------

#[test]
fn registration_requires_user_verification_and_keeps_failed_challenge_retryable() {
    let harness = harness();
    let email = "uv-register@example.com";
    let start = harness.call(
        "POST",
        "/v1/auth/passkey/register/start",
        serde_json::json!({"email": email}),
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let start: serde_json::Value = serde_json::from_str(&start.body).expect("JSON start");
    let challenge = start["challenge"].as_str().expect("challenge").to_string();
    let user_id = start["user_id"].as_str().expect("user id").to_string();
    let magic_token = harness
        .mailer
        .sent
        .borrow()
        .iter()
        .rev()
        .find(|(address, _)| address == email)
        .expect("signup proof was mailed")
        .1
        .clone();

    let key = test_signing_key(41);
    let cred_id = b"uv-register-credential";
    let cose = es256_cose(&key);
    let client_data = client_json("webauthn.create", &challenge, ORIGIN);
    let presence_only = authenticator_data(RP, 0x41, 0, Some((cred_id, &cose)));
    let rejected = harness.call(
        "POST",
        "/v1/auth/passkey/register/finish",
        serde_json::json!({
            "challenge": challenge,
            "credential_id": b64::encode(cred_id),
            "attestation_object": b64::encode(&none_attestation(&presence_only)),
            "client_data_json": b64::encode(&client_data),
            "magic_token": magic_token,
            "invite_code": null,
        }),
    );
    assert_eq!(rejected.status, 401, "presence-only enrollment must fail");
    assert_eq!(rejected.body, GENERIC_AUTH_BODY);
    assert!(rejected.set_cookie.is_none());
    assert!(
        block_on(harness.store.credentials_for_user(&user_id))
            .expect("credential lookup")
            .is_empty(),
        "a presence-only ceremony must enroll no credential"
    );

    // Verification failure occurs before either single-use claim, so the
    // same valid challenge and inbox proof can be retried with UV set.
    let user_verified = authenticator_data(RP, 0x45, 0, Some((cred_id, &cose)));
    let accepted = harness.call(
        "POST",
        "/v1/auth/passkey/register/finish",
        serde_json::json!({
            "challenge": challenge,
            "credential_id": b64::encode(cred_id),
            "attestation_object": b64::encode(&none_attestation(&user_verified)),
            "client_data_json": b64::encode(&client_data),
            "magic_token": magic_token,
            "invite_code": null,
        }),
    );
    assert_eq!(accepted.status, 200, "UV enrollment should work: {}", accepted.body);
}

#[test]
fn login_requires_user_verification_before_consuming_the_challenge() {
    let harness = harness();
    let email = "uv-login@example.com";
    let (_registration_cookie, credential_id) = register_flow(&harness, email, 42, None);
    let sessions_before = harness.store.sessions.borrow().len();
    let start = harness.call(
        "POST",
        "/v1/auth/passkey/login/start",
        serde_json::json!({"email": email}),
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let start: serde_json::Value = serde_json::from_str(&start.body).expect("JSON start");
    let challenge = start["challenge"].as_str().expect("challenge").to_string();
    let key = test_signing_key(42);

    let presence_only = authenticator_data(RP, 0x01, 1, None);
    let client_data = client_json("webauthn.get", &challenge, ORIGIN);
    let rejected = harness.call(
        "POST",
        "/v1/auth/passkey/login/finish",
        serde_json::json!({
            "challenge": challenge,
            "credential_id": credential_id,
            "authenticator_data": b64::encode(&presence_only),
            "client_data_json": b64::encode(&client_data),
            "signature": b64::encode(&es256_sign(&key, &presence_only, &client_data)),
        }),
    );
    assert_eq!(rejected.status, 401, "presence-only login must fail");
    assert_eq!(rejected.body, GENERIC_AUTH_BODY);
    assert!(rejected.set_cookie.is_none());
    assert_eq!(harness.store.sessions.borrow().len(), sessions_before);
    let row = block_on(harness.store.challenge_by_id(&challenge))
        .expect("challenge lookup")
        .expect("challenge remains available");
    assert!(!row.challenge.consumed, "failed UV verification must not burn the challenge");

    let user_verified = authenticator_data(RP, 0x05, 1, None);
    let accepted = harness.call(
        "POST",
        "/v1/auth/passkey/login/finish",
        serde_json::json!({
            "challenge": challenge,
            "credential_id": credential_id,
            "authenticator_data": b64::encode(&user_verified),
            "client_data_json": b64::encode(&client_data),
            "signature": b64::encode(&es256_sign(&key, &user_verified, &client_data)),
        }),
    );
    assert_eq!(accepted.status, 200, "UV login should work: {}", accepted.body);
    let credential = block_on(harness.store.credential_by_id(&credential_id))
        .expect("credential lookup")
        .expect("credential remains stored");
    assert_eq!(credential.sign_count, 1);
}

#[test]
fn enrollment_ignores_untrusted_attestation_counter_and_first_assertion_advances_it() {
    let harness = harness();
    let email = "counter-enrollment@example.com";
    let start = harness.call(
        "POST",
        "/v1/auth/passkey/register/start",
        serde_json::json!({"email": email}),
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let start: serde_json::Value = serde_json::from_str(&start.body).expect("JSON start");
    let challenge = start["challenge"].as_str().expect("challenge").to_string();
    let magic_token = harness
        .mailer
        .sent
        .borrow()
        .iter()
        .rev()
        .find(|(address, _)| address == email)
        .expect("signup proof was mailed")
        .1
        .clone();

    let key = test_signing_key(43);
    let cred_id = b"counter-enrollment-credential";
    let cose = es256_cose(&key);
    let auth_data = authenticator_data(RP, 0x45, u32::MAX, Some((cred_id, &cose)));
    let client_data = client_json("webauthn.create", &challenge, ORIGIN);
    let registered = harness.call(
        "POST",
        "/v1/auth/passkey/register/finish",
        serde_json::json!({
            "challenge": challenge,
            "credential_id": b64::encode(cred_id),
            "attestation_object": b64::encode(&none_attestation(&auth_data)),
            "client_data_json": b64::encode(&client_data),
            "magic_token": magic_token,
            "invite_code": null,
        }),
    );
    assert_eq!(registered.status, 200, "{}", registered.body);

    let credential_id = b64::encode(cred_id);
    let credential = block_on(harness.store.credential_by_id(&credential_id))
        .expect("credential lookup")
        .expect("credential stored");
    assert_eq!(credential.sign_count, 0, "attestation counter is unauthenticated");

    let start = harness.call(
        "POST",
        "/v1/auth/passkey/login/start",
        serde_json::json!({"email": email}),
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let start: serde_json::Value = serde_json::from_str(&start.body).expect("JSON start");
    let login_challenge = start["challenge"].as_str().expect("challenge").to_string();
    let assertion_data = authenticator_data(RP, 0x05, 1, None);
    let assertion_client = client_json("webauthn.get", &login_challenge, ORIGIN);
    let logged_in = harness.call(
        "POST",
        "/v1/auth/passkey/login/finish",
        serde_json::json!({
            "challenge": login_challenge,
            "credential_id": credential_id,
            "authenticator_data": b64::encode(&assertion_data),
            "client_data_json": b64::encode(&assertion_client),
            "signature": b64::encode(&es256_sign(&key, &assertion_data, &assertion_client)),
        }),
    );
    assert_eq!(logged_in.status, 200, "first signed assertion should establish count: {}", logged_in.body);
    let credential = block_on(harness.store.credential_by_id(&credential_id))
        .expect("credential lookup")
        .expect("credential stored");
    assert_eq!(credential.sign_count, 1);
}

#[test]
fn login_rejects_a_losing_sign_count_race_without_issuing_a_session() {
    let harness = harness();
    let email = "counter-race@example.com";
    let (_registration_cookie, credential_id) = register_flow(&harness, email, 44, None);
    let sessions_before = harness.store.sessions.borrow().len();
    let start = harness.call(
        "POST",
        "/v1/auth/passkey/login/start",
        serde_json::json!({"email": email}),
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let start: serde_json::Value = serde_json::from_str(&start.body).expect("JSON start");
    let challenge = start["challenge"].as_str().expect("challenge").to_string();
    let key = test_signing_key(44);
    let auth_data = authenticator_data(RP, 0x05, 1, None);
    let client_data = client_json("webauthn.get", &challenge, ORIGIN);
    harness
        .store
        .race_next_sign_count_advance
        .set(true);

    let response = harness.call(
        "POST",
        "/v1/auth/passkey/login/finish",
        serde_json::json!({
            "challenge": challenge,
            "credential_id": credential_id.clone(),
            "authenticator_data": b64::encode(&auth_data),
            "client_data_json": b64::encode(&client_data),
            "signature": b64::encode(&es256_sign(&key, &auth_data, &client_data)),
        }),
    );
    assert_eq!(response.status, 401, "lost counter CAS must fail generically");
    assert_eq!(response.body, GENERIC_AUTH_BODY);
    assert!(response.set_cookie.is_none());
    assert_eq!(harness.store.sessions.borrow().len(), sessions_before);
    assert_eq!(harness.store.audits_with("auth.login"), 0);
    let credential = block_on(harness.store.credential_by_id(&credential_id))
        .expect("credential lookup")
        .expect("credential stored");
    assert_eq!(credential.sign_count, 1, "the concurrent winner advanced it");
}

#[test]
fn audit_source_ip_is_a_canonical_single_ip_or_unknown() {
    let clean = vault::audit_event("user", "auth.login", true, SOURCE_IP, NOW);
    assert_eq!(clean.source_ip, SOURCE_IP);

    let hostile = format!("{SOURCE_IP}\nFORGED ok=true");
    let rejected = vault::audit_event("user", "auth.login", true, &hostile, NOW);
    assert_eq!(rejected.source_ip, "unknown");
}

#[test]
fn purpose_guard_migration_keeps_the_constraint_creation_strategy_pinned() {
    let migration_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../migrations/0006_auth_challenge_purpose_guard.sql");
    assert!(
        migration_path.is_file(),
        "purpose-guard migration must be present; this test does not apply it"
    );
    let migration = std::fs::read_to_string(&migration_path).expect("read purpose-guard migration");
    let normalized = migration
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let expected_constraint_guard = format!(
        concat!(
            "do $$ begin if not exists ( ",
            "select 1 from pg_constraint where ",
            "conrelid = 'auth_challenges'::regclass and ",
            "conname = 'auth_challenges_purpose_check' ) then ",
            "alter table auth_challenges add constraint ",
            "auth_challenges_purpose_check check (purpose in ('', '{}', '{}')); ",
            "end if; end $$;"
        ),
        handlers::PURPOSE_REGISTER,
        handlers::PURPOSE_LOGIN
    );
    assert!(normalized.contains(&expected_constraint_guard));
    assert!(normalized.contains(
        "if new.purpose is distinct from old.purpose then raise exception 'auth_challenges.purpose is immutable'"
    ));
    assert!(normalized.contains(
        "drop trigger if exists auth_challenges_purpose_immutable on auth_challenges"
    ));
    assert!(normalized.contains(
        "create trigger auth_challenges_purpose_immutable before update of purpose on auth_challenges"
    ));
    assert!(normalized.contains("execute function auth_challenges_purpose_immutable()"));
}
