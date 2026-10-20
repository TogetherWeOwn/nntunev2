//! Version-pinned KEK lookup and current-write routing acceptance tests.
//!
//! Run: `python3 scripts/cargo_cache.py run -- test --manifest-path workers/api/Cargo.toml --test vault_kek_lookup_acceptance`
//!
//! Pins `Config::with_prior_keks` and `Config::kek_by_version`. These tests
//! intentionally do not compile until those seams exist. No migration,
//! rewrap/CAS, environment parsing, network, database, or real clock is used.
//! All keys are generated in-test; assertion diagnostics never print keys,
//! ciphertext, or decrypted credentials.
//!
//! Stored reads use an exactly empty POST body; GET only reads metadata.
//! PUT replaces the envelope without decrypting the old row, so an unknown
//! old version must not prevent a new, valid current-version write.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use nntune_api::{auth, handlers, session, vault};
use rand::RngCore;

const NOW: u64 = 1_700_000_000;
const RP: &str = "passkey.example.com";
const ORIGIN: &str = "https://passkey.example.com";
const HOST: &str = "news.example.com";
const PRIOR: u32 = 3;
const OLDER: u32 = 1;
const CURRENT: u32 = 11;
const UNKNOWN: u32 = 97;
const AUTH_FAILURE: &str = r#"{"error":"check your email and try again"}"#;
const TEST_FAILURE: &str = r#"{"ok":false}"#;

fn random_key() -> [u8; 32] {
    let mut key = [0; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(std::task::Waker::noop());
    let mut future = Box::pin(future);
    for _ in 0..10_000 {
        if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
            return result;
        }
    }
    panic!("offline fake unexpectedly awaited external work");
}

fn payload() -> Vec<u8> {
    serde_json::json!({
        "provider": {
            "host": HOST, "port": 563, "tls": true,
            "username": "synthetic-user", "password": "synthetic-password",
            "max_connections": 4
        },
        "indexer": null
    })
    .to_string()
    .into_bytes()
}

fn encrypted_row(key: &[u8; 32], version: u32, user: &str) -> handlers::VaultRow {
    handlers::VaultRow {
        blob: vault::encrypt_vault(key, version, user, &payload())
            .expect("encrypt synthetic fixture"),
        indexer_configured: false,
        updated_at: NOW,
    }
}

fn assert_round_trip(key: &[u8; 32], user: &str, blob: &vault::VaultBlob) {
    let plaintext = vault::decrypt_vault(key, user, blob).expect("decrypt synthetic fixture");
    let actual: serde_json::Value =
        serde_json::from_slice(&plaintext).expect("synthetic plaintext is JSON");
    let expected: serde_json::Value = serde_json::from_slice(&payload()).expect("fixture JSON");
    // Boolean comparison deliberately avoids Debug-printing decrypted data.
    assert!(
        actual == expected,
        "decrypted synthetic payload must round-trip"
    );
}

fn assert_response(response: &handlers::Response, status: u16, body: &str) {
    assert_eq!(response.status, status);
    // A broken implementation could put a key or credentials in the body;
    // do not print the unexpected body even on this failing assertion.
    assert!(
        response.body == body,
        "response must match the exact generic body"
    );
    assert!(
        response.set_cookie.is_none(),
        "vault routes do not issue cookies"
    );
    assert!(response.retry_after.is_none());
}

#[derive(Default)]
struct FakeStore {
    sessions: RefCell<HashMap<String, session::SessionRecord>>,
    vaults: RefCell<HashMap<String, handlers::VaultRow>>,
    vault_reads: RefCell<Vec<String>>,
    writes: Cell<usize>,
    leases: Cell<usize>,
}

impl handlers::Store for FakeStore {
    async fn user_by_email(
        &self,
        _: &str,
    ) -> Result<Option<handlers::UserRecord>, handlers::StoreError> {
        panic!("unexpected user lookup");
    }
    async fn insert_user(&self, _: &handlers::UserRecord) -> Result<(), handlers::StoreError> {
        panic!("unexpected user insert");
    }
    async fn insert_challenge(
        &self,
        _: &auth::PasskeyChallenge,
        _: Option<&str>,
        _: &str,
    ) -> Result<(), handlers::StoreError> {
        panic!("unexpected challenge insert");
    }
    async fn challenge_by_id(
        &self,
        _: &str,
    ) -> Result<Option<handlers::ChallengeRow>, handlers::StoreError> {
        panic!("unexpected challenge lookup");
    }
    async fn insert_credential(
        &self,
        _: &auth::PasskeyCredential,
    ) -> Result<(), handlers::StoreError> {
        panic!("unexpected credential insert");
    }
    async fn credential_by_id(
        &self,
        _: &str,
    ) -> Result<Option<auth::PasskeyCredential>, handlers::StoreError> {
        panic!("unexpected credential lookup");
    }
    async fn credentials_for_user(
        &self,
        _: &str,
    ) -> Result<Vec<auth::PasskeyCredential>, handlers::StoreError> {
        panic!("unexpected credential list");
    }
    async fn advance_sign_count(
        &self,
        _: &str,
        _: u32,
        _: u32,
    ) -> Result<bool, handlers::StoreError> {
        panic!("unexpected sign count update");
    }
    async fn insert_magic(&self, _: &auth::MagicLinkRecord) -> Result<(), handlers::StoreError> {
        panic!("unexpected magic link insert");
    }
    async fn magic_by_hash(
        &self,
        _: &str,
    ) -> Result<Option<auth::MagicLinkRecord>, handlers::StoreError> {
        panic!("unexpected magic link lookup");
    }
    async fn reserve_rate_limit(
        &self,
        _: &str,
        _: &str,
        _: u64,
        _: u64,
        _: u64,
    ) -> Result<bool, handlers::StoreError> {
        Ok(true)
    }
    async fn prune_expired_challenges(&self, _: u64) -> Result<(), handlers::StoreError> {
        panic!("unexpected challenge prune");
    }
    async fn acquire_test_connection_lease(
        &self,
        _: &str,
        _: &str,
        _: u64,
        _: u32,
    ) -> Result<bool, handlers::StoreError> {
        self.leases.set(self.leases.get() + 1);
        Ok(true)
    }
    async fn release_test_connection_lease(
        &self,
        _: &str,
        _: &str,
    ) -> Result<(), handlers::StoreError> {
        Ok(())
    }
    async fn invite_by_hash(
        &self,
        _: &str,
    ) -> Result<Option<auth::InviteRecord>, handlers::StoreError> {
        panic!("unexpected invite lookup");
    }
    async fn insert_session(&self, _: &session::SessionRecord) -> Result<(), handlers::StoreError> {
        panic!("unexpected session insert");
    }
    async fn session_by_id(
        &self,
        id: &str,
    ) -> Result<Option<session::SessionRecord>, handlers::StoreError> {
        Ok(self.sessions.borrow().get(id).cloned())
    }
    async fn rotate_session_row(
        &self,
        _: &str,
        _: u32,
        _: u32,
        _: u64,
    ) -> Result<bool, handlers::StoreError> {
        panic!("unexpected session rotation");
    }
    async fn revoke_session(&self, _: &str) -> Result<(), handlers::StoreError> {
        panic!("unexpected session revocation");
    }
    async fn revoke_all_sessions(&self, _: &str) -> Result<(), handlers::StoreError> {
        panic!("unexpected session revocation");
    }
    async fn vault_by_user(
        &self,
        user: &str,
    ) -> Result<Option<handlers::VaultRow>, handlers::StoreError> {
        self.vault_reads.borrow_mut().push(user.to_string());
        Ok(self.vaults.borrow().get(user).cloned())
    }
    async fn upsert_vault(
        &self,
        user: &str,
        row: &handlers::VaultRow,
    ) -> Result<(), handlers::StoreError> {
        self.writes.set(self.writes.get() + 1);
        self.vaults
            .borrow_mut()
            .insert(user.to_string(), row.clone());
        Ok(())
    }
    async fn delete_vault(&self, _: &str) -> Result<(), handlers::StoreError> {
        panic!("unexpected vault deletion");
    }
    async fn insert_audit(&self, _: &vault::AuditEvent) -> Result<(), handlers::StoreError> {
        Ok(())
    }
}

impl auth::SingleUseStore for FakeStore {
    async fn claim(&self, _: auth::SingleUseClaim<'_>) -> Result<u64, auth::AuthError> {
        panic!("unexpected single-use claim");
    }
}

struct FakeMailer;
impl handlers::Mailer for FakeMailer {
    async fn send_magic_link(&self, _: &str, _: &str) -> Result<(), handlers::StoreError> {
        panic!("vault routes must not send mail");
    }
}

#[derive(Default)]
struct FakeDns(Cell<usize>);
impl handlers::AsyncDns for FakeDns {
    fn resolve<'a>(&'a self, host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>> {
        self.0.set(self.0.get() + 1);
        assert!(host == HOST, "only the synthetic provider may be resolved");
        Box::pin(async { vec!["93.184.216.34".parse().expect("fixture IP")] })
    }
}

#[derive(Default)]
struct FakeDialer(Cell<usize>);
impl handlers::Dialer for FakeDialer {
    async fn probe(&self, _: &vault::ValidatedTarget, _: bool) -> bool {
        self.0.set(self.0.get() + 1);
        true
    }
}

struct Harness {
    secret: [u8; 32],
    now: Cell<u64>,
    store: FakeStore,
    mailer: FakeMailer,
    dns: FakeDns,
    dialer: FakeDialer,
}

impl Harness {
    fn new() -> Self {
        Self {
            secret: random_key(),
            now: Cell::new(NOW),
            store: FakeStore::default(),
            mailer: FakeMailer,
            dns: FakeDns::default(),
            dialer: FakeDialer::default(),
        }
    }

    fn config(&self, key: [u8; 32], version: u32) -> handlers::Config {
        handlers::Config::new(
            RP,
            ORIGIN,
            self.secret.to_vec(),
            key,
            version,
            3600,
            false,
            false,
            None,
        )
        .expect("valid synthetic config")
    }

    fn cookie(&self, user: &str) -> String {
        let now = self.now.get();
        let token =
            session::issue_session(&self.secret, user, now, 3600).expect("issue fixture session");
        let claims =
            session::verify_session(&self.secret, &token, now).expect("verify fixture session");
        self.store.sessions.borrow_mut().insert(
            claims.session_id.clone(),
            session::SessionRecord {
                id: claims.session_id,
                user_id: user.to_string(),
                created_at: now,
                expires_at: now + 3600,
                rotation_counter: 0,
                revoked: false,
            },
        );
        format!("{}={token}", session::COOKIE_NAME)
    }

    fn request(
        &self,
        config: &handlers::Config,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Vec<u8>,
    ) -> handlers::Response {
        let ctx = handlers::Ctx {
            cfg: config,
            store: &self.store,
            mailer: &self.mailer,
            dialer: &self.dialer,
            dns: &self.dns,
            now: self.now.get(),
            source_ip: "198.51.100.9".to_string(),
        };
        let request = handlers::Request {
            method: method.to_string(),
            path: path.to_string(),
            cookie: cookie.map(str::to_string),
            origin: Some(ORIGIN.to_string()),
            authorization: None,
            body,
        };
        block_on(handlers::handle(&ctx, &request))
    }

    fn put(&self, config: &handlers::Config, cookie: &str) {
        let response = self.request(config, "PUT", "/v1/vault", Some(cookie), payload());
        let expected = serde_json::json!({
            "provider_configured": true, "indexer_configured": false,
            "updated_at": self.now.get()
        });
        assert_eq!(response.status, 200);
        let actual: serde_json::Value = serde_json::from_str(&response.body).expect("summary JSON");
        assert!(
            actual == expected,
            "PUT returns only the existing metadata summary"
        );
        assert!(response.set_cookie.is_none());
        assert!(response.retry_after.is_none());
    }

    fn stored_test(&self, config: &handlers::Config, cookie: &str) -> handlers::Response {
        self.request(config, "POST", "/v1/vault/test", Some(cookie), Vec::new())
    }

    fn row(&self, user: &str) -> handlers::VaultRow {
        self.store
            .vaults
            .borrow()
            .get(user)
            .expect("fixture row exists")
            .clone()
    }

    fn clear_effects(&self) {
        self.store.vault_reads.borrow_mut().clear();
        self.store.writes.set(0);
        self.store.leases.set(0);
        self.dns.0.set(0);
        self.dialer.0.set(0);
    }

    fn assert_no_external_work(&self) {
        assert_eq!(self.dns.0.get(), 0, "failure must not reach DNS");
        assert_eq!(self.dialer.0.get(), 0, "failure must not reach the dialer");
        assert_eq!(
            self.store.leases.get(),
            0,
            "failure must not acquire a lease"
        );
        assert_eq!(self.store.writes.get(), 0, "failure must not save a vault");
    }
}

#[test]
fn lookup_is_version_pinned() {
    let h = Harness::new();
    let (older, prior, current) = (random_key(), random_key(), random_key());
    let config = h
        .config(current, CURRENT)
        .with_prior_keks(vec![(PRIOR, prior), (OLDER, older)]);
    for (version, key) in [(OLDER, &older), (PRIOR, &prior), (CURRENT, &current)] {
        let row = encrypted_row(key, version, "selector-user");
        let selected = config
            .kek_by_version(version)
            .expect("provisioned version must resolve");
        assert_round_trip(selected, "selector-user", &row.blob);
    }
    assert!(
        config.kek_by_version(UNKNOWN).is_none(),
        "unprovisioned version must not fall back"
    );
}

#[test]
fn current_key_wins() {
    let h = Harness::new();
    let (current, decoy) = (random_key(), random_key());
    // A retained entry cannot shadow the selected current write key.
    let config = h
        .config(current, CURRENT)
        .with_prior_keks(vec![(CURRENT, decoy)]);
    let row = encrypted_row(&current, CURRENT, "selector-user");
    assert_round_trip(
        config.kek_by_version(CURRENT).expect("current key"),
        "selector-user",
        &row.blob,
    );
    let cookie = h.cookie("selector-user");
    h.put(&config, &cookie);
    let saved = h.row("selector-user");
    assert_eq!(saved.blob.kek_version, CURRENT);
    assert_round_trip(&current, "selector-user", &saved.blob);
    assert!(vault::decrypt_vault(&decoy, "selector-user", &saved.blob).is_err());
}

#[test]
fn put_uses_current_key() {
    let h = Harness::new();
    let (prior, current) = (random_key(), random_key());
    let config = h
        .config(current, CURRENT)
        .with_prior_keks(vec![(PRIOR, prior)]);
    let user = "write-user";
    let cookie = h.cookie(user);
    // PUT replaces, not decrypts, an unreadable previous envelope.
    h.store
        .vaults
        .borrow_mut()
        .insert(user.to_string(), encrypted_row(&prior, UNKNOWN, user));
    h.now.set(NOW + 1);
    h.put(&config, &cookie);
    let saved = h.row(user);
    assert_eq!(saved.blob.kek_version, CURRENT);
    assert_eq!(saved.updated_at, NOW + 1);
    assert_round_trip(&current, user, &saved.blob);
    assert!(vault::decrypt_vault(&prior, user, &saved.blob).is_err());
    assert_eq!(h.store.writes.get(), 1);
    assert!(
        h.store.vault_reads.borrow().is_empty(),
        "PUT must not decrypt the old row"
    );
}

#[test]
fn mixed_rows_read() {
    let h = Harness::new();
    let (older, prior, current) = (random_key(), random_key(), random_key());
    let config = h
        .config(current, CURRENT)
        .with_prior_keks(vec![(PRIOR, prior), (OLDER, older)]);
    let mut cookies = Vec::new();
    // Seed all envelopes through real PUT handlers under their then-current
    // write configs; read all of them under the same retained-key config.
    for (user, key, version) in [
        ("older-user", older, OLDER),
        ("prior-user", prior, PRIOR),
        ("current-user", current, CURRENT),
    ] {
        let cookie = h.cookie(user);
        h.put(&h.config(key, version), &cookie);
        assert_eq!(h.row(user).blob.kek_version, version);
        cookies.push((user, cookie, version));
    }
    h.now.set(NOW + 1);
    h.clear_effects();
    for (index, (user, cookie, version)) in cookies.iter().enumerate() {
        assert_response(&h.stored_test(&config, cookie), 200, r#"{"ok":true}"#);
        assert_eq!(
            h.dialer.0.get(),
            index + 1,
            "each stored read must reach the fake probe"
        );
        assert_eq!(
            h.row(user).blob.kek_version,
            *version,
            "reads must not rewrap rows"
        );
    }
    assert_eq!(
        h.store.vault_reads.borrow().as_slice(),
        ["older-user", "prior-user", "current-user"]
    );
    assert_eq!(
        h.store.writes.get(),
        0,
        "stored reads do not save or rewrap"
    );
}

#[test]
fn failures_are_indistinguishable() {
    let h = Harness::new();
    let (prior, current, wrong) = (random_key(), random_key(), random_key());
    let config = h
        .config(current, CURRENT)
        .with_prior_keks(vec![(PRIOR, prior)]);
    let user = "failure-user";
    let cookie = h.cookie(user);
    // Positive control: no network failure or broken session can satisfy
    // the negative cases accidentally.
    h.put(&config, &cookie);
    assert_response(&h.stored_test(&config, &cookie), 200, r#"{"ok":true}"#);

    // These envelopes genuinely decrypt with an available key, but that
    // key is NOT provisioned for UNKNOWN. Trying every key is forbidden.
    let unknown_current = encrypted_row(&current, UNKNOWN, user);
    let unknown_prior = encrypted_row(&prior, UNKNOWN, user);
    assert_round_trip(&current, user, &unknown_current.blob);
    assert_round_trip(&prior, user, &unknown_prior.blob);
    // Version PRIOR is provisioned, but this row uses the CURRENT key:
    // a fallback-to-current after a selected-key failure must also fail.
    let wrong_version_key = encrypted_row(&current, PRIOR, user);
    let wrong_key = encrypted_row(&wrong, CURRENT, user);
    let mut bad_payload = encrypted_row(&current, CURRENT, user);
    let mut bytes =
        nntune_api::b64::decode(&bad_payload.blob.payload_b64url).expect("decode fixture");
    *bytes.last_mut().expect("GCM tag exists") ^= 1;
    bad_payload.blob.payload_b64url = nntune_api::b64::encode(&bytes);
    let mut bad_wrap = encrypted_row(&prior, PRIOR, user);
    let mut bytes =
        nntune_api::b64::decode(&bad_wrap.blob.dek_wrapped_b64url).expect("decode fixture");
    *bytes.last_mut().expect("GCM tag exists") ^= 1;
    bad_wrap.blob.dek_wrapped_b64url = nntune_api::b64::encode(&bytes);
    assert!(vault::decrypt_vault(&current, user, &bad_payload.blob).is_err());
    assert!(vault::decrypt_vault(&prior, user, &bad_wrap.blob).is_err());

    for row in [
        None,
        Some(unknown_current),
        Some(unknown_prior),
        Some(wrong_version_key),
        Some(wrong_key),
        Some(bad_payload),
        Some(bad_wrap),
    ] {
        match row {
            Some(row) => {
                h.store.vaults.borrow_mut().insert(user.to_string(), row);
            }
            None => {
                h.store.vaults.borrow_mut().remove(user);
            }
        }
        h.clear_effects();
        assert_response(&h.stored_test(&config, &cookie), 200, TEST_FAILURE);
        assert_eq!(h.store.vault_reads.borrow().as_slice(), [user]);
        h.assert_no_external_work();
    }
}

#[test]
fn auth_and_owner_checks_precede_reads() {
    let h = Harness::new();
    let (prior, current) = (random_key(), random_key());
    let config = h
        .config(current, CURRENT)
        .with_prior_keks(vec![(PRIOR, prior)]);
    let owner = "owner-user";
    let owner_cookie = h.cookie(owner);
    let other_cookie = h.cookie("other-user");
    let mut corrupt = encrypted_row(&prior, PRIOR, owner);
    corrupt.blob.payload_b64url = "invalid!".to_string();
    for row in [
        None,
        Some(encrypted_row(&prior, PRIOR, owner)),
        Some(encrypted_row(&current, UNKNOWN, owner)),
        Some(corrupt),
    ] {
        match row {
            Some(row) => {
                h.store.vaults.borrow_mut().insert(owner.to_string(), row);
            }
            None => {
                h.store.vaults.borrow_mut().remove(owner);
            }
        }
        for cookie in [None, Some("__Host-nntune-session=invalid")] {
            for (method, path, body) in [
                ("PUT", "/v1/vault", payload()),
                ("POST", "/v1/vault/test", Vec::new()),
            ] {
                h.clear_effects();
                assert_response(
                    &h.request(&config, method, path, cookie, body),
                    401,
                    AUTH_FAILURE,
                );
                assert!(
                    h.store.vault_reads.borrow().is_empty(),
                    "unauthenticated requests must not load rows"
                );
                h.assert_no_external_work();
            }
        }
        // There is no request user-id selector: the verified session is the
        // only lookup identity. Another user's row is indistinguishable
        // from an absent row to this session, regardless of its version.
        h.clear_effects();
        assert_response(&h.stored_test(&config, &other_cookie), 200, TEST_FAILURE);
        assert_eq!(h.store.vault_reads.borrow().as_slice(), ["other-user"]);
        h.assert_no_external_work();
    }
    // Positive owner control, followed by a misassociated-row AAD check.
    h.store
        .vaults
        .borrow_mut()
        .insert(owner.to_string(), encrypted_row(&prior, PRIOR, owner));
    assert_response(
        &h.stored_test(&config, &owner_cookie),
        200,
        r#"{"ok":true}"#,
    );
    let owner_row = h.row(owner);
    h.store
        .vaults
        .borrow_mut()
        .insert("other-user".to_string(), owner_row);
    h.clear_effects();
    assert_response(&h.stored_test(&config, &other_cookie), 200, TEST_FAILURE);
    h.assert_no_external_work();
}

#[test]
fn legacy_single_key_is_preserved() {
    let h = Harness::new();
    let key = random_key();
    let legacy_version = 41;
    let config = h.config(key, legacy_version);
    let user = "legacy-user";
    let cookie = h.cookie(user);
    h.put(&config, &cookie);
    let saved = h.row(user);
    assert_eq!(saved.blob.kek_version, legacy_version);
    assert_round_trip(
        config.kek_by_version(legacy_version).expect("legacy key"),
        user,
        &saved.blob,
    );
    assert_response(&h.stored_test(&config, &cookie), 200, r#"{"ok":true}"#);
    assert!(config.kek_by_version(UNKNOWN).is_none());
    // Use the SAME key, not a different key: legacy must reject a missing
    // version because of lookup, not just because AEAD happens to fail.
    let unknown = encrypted_row(&key, UNKNOWN, user);
    assert_round_trip(&key, user, &unknown.blob);
    h.store
        .vaults
        .borrow_mut()
        .insert(user.to_string(), unknown);
    h.clear_effects();
    assert_response(&h.stored_test(&config, &cookie), 200, TEST_FAILURE);
    h.assert_no_external_work();
}
