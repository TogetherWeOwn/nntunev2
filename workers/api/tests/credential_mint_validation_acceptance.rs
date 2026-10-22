//! Acceptance tests: credential-mint validation (paired Test Author card).
//!
//! Contract under test (`POST /v1/credentials`, `credentials_mint` in
//! `workers/api/src/handlers.rs`; scope vocabulary in
//! `workers/api/src/native_rest.rs`, ADR 0010):
//!
//! - `ttl_secs: 0` is a validation error — 422 with
//!   `"code":"validation_failed"` — not a credential that expires at
//!   issuance. The rejection inserts zero credential rows, writes no
//!   `native_credential_created` audit row, and carries no secret
//!   (`secret` key or `ntk_` material) in the response body.
//! - A raw scope list longer than the number of defined ADR scopes
//!   (`native_rest::ALL_SCOPES.len()`) is the same 422
//!   `validation_failed` with zero inserts and no mint audit row, even
//!   when every name in the list is an otherwise-valid scope (the
//!   over-long entries can only be duplicates, since only
//!   `ALL_SCOPES.len()` distinct scopes exist — rejecting on raw length
//!   keeps an arbitrarily long duplicate list from minting).
//! - Duplicate scope names in an otherwise valid request (raw length
//!   within the cap) are deduplicated, first-seen order preserved, in
//!   both the 201 response `scopes` and the stored row.
//! - The happy path is unchanged: valid scopes with a TTL inside the
//!   365-day cap (including the boundaries `ttl_secs: 1` and
//!   `ttl_secs: MAX_CREDENTIAL_TTL_SECS`) still mint a 201 with the
//!   one-time `ntk_` secret, store exactly one row, and audit the id.
//!
//! RED on the base SHA by assertion (not compile error): zero-TTL and
//! over-long requests currently mint 201s, and duplicates are stored
//! verbatim. The paired Builder card implements the validation in
//! production code and must NEVER edit this file. Intentionally RED:
//! this file must not merge alone — it lands with the paired build.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --manifest-path workers/api/Cargo.toml \
//!   --test credential_mint_validation_acceptance
//! ```
//!
//! Fixtures are synthetic only (public repo: never real secrets,
//! peppers, or credential material).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use nntune_api::handlers::Store;
use nntune_api::native_rest::{Scope, ALL_SCOPES, MAX_CREDENTIAL_TTL_SECS};
use nntune_api::{auth, handlers, session, vault};

const NOW: u64 = 1_700_000_000;
const RP: &str = "passkey.example.com";
const ORIGIN: &str = "https://passkey.example.com";
const SOURCE_IP: &str = "198.51.100.9";
const PRIMARY_SECRET: &[u8] = b"acceptance-primary-secret-fits-32-bytes!";
const USER_ID: &str = "u1";
const EMAIL: &str = "a@example.com";

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

// --- Fake store ----------------------------------------------------------------

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
    native_credentials: RefCell<HashMap<String, handlers::NativeCredentialRow>>,
    rate_limit_slots: RefCell<HashMap<(String, String, u32), u64>>,
    rate_limit_calls: Cell<usize>,
    native_lookup_calls: Cell<usize>,
    native_use_calls: Cell<usize>,
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
        self.rate_limit_calls.set(self.rate_limit_calls.get() + 1);
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

    async fn insert_native_credential(
        &self,
        row: &handlers::NativeCredentialRow,
    ) -> Result<(), handlers::StoreError> {
        let mut rows = self.native_credentials.borrow_mut();
        // Mirror the migration 0019 composite UNIQUE: neither the id nor
        // the (pepper_version, key_hmac) identity may repeat.
        if rows.contains_key(&row.record.id)
            || rows.values().any(|existing| {
                existing.record.pepper_version == row.record.pepper_version
                    && existing.record.key_hmac == row.record.key_hmac
            })
        {
            return Err(handlers::StoreError);
        }
        rows.insert(row.record.id.clone(), row.clone());
        Ok(())
    }

    async fn native_credential_by_hmac(
        &self,
        pepper_version: u32,
        key_hmac: &str,
    ) -> Result<Option<handlers::NativeCredentialRow>, handlers::StoreError> {
        self.native_lookup_calls
            .set(self.native_lookup_calls.get() + 1);
        Ok(self
            .native_credentials
            .borrow()
            .values()
            .find(|row| {
                row.record.pepper_version == pepper_version && row.record.key_hmac == key_hmac
            })
            .cloned())
    }

    async fn record_native_credential_use(
        &self,
        id: &str,
        now: u64,
    ) -> Result<(), handlers::StoreError> {
        self.native_use_calls.set(self.native_use_calls.get() + 1);
        if let Some(row) = self.native_credentials.borrow_mut().get_mut(id) {
            row.last_used_at = Some(now);
        }
        Ok(())
    }

    async fn revoke_native_credential(
        &self,
        id: &str,
        user_id: &str,
        now: u64,
    ) -> Result<bool, handlers::StoreError> {
        let mut rows = self.native_credentials.borrow_mut();
        match rows.get_mut(id) {
            Some(row) if row.record.user_id == user_id => {
                row.record.revoked_at.get_or_insert(now);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn native_credentials_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<handlers::NativeCredentialRow>, handlers::StoreError> {
        let mut rows: Vec<handlers::NativeCredentialRow> = self
            .native_credentials
            .borrow()
            .values()
            .filter(|row| row.record.user_id == user_id)
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            (a.record.created_at, &a.record.id).cmp(&(b.record.created_at, &b.record.id))
        });
        Ok(rows)
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

// --- Fake mailer / dialer / DNS ------------------------------------------------

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
    async fn probe(&self, _target: &vault::ValidatedTarget, _tls: bool) -> bool {
        false
    }
}

#[derive(Default)]
struct FakeDns;

impl handlers::AsyncDns for FakeDns {
    fn resolve<'a>(&'a self, _host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>> {
        Box::pin(async { Vec::new() })
    }
}

// --- Native REST surface acceptance ---------------------------------------------

mod surface_acceptance {
    use super::*;

    fn request(
        h: &Harness,
        method: &str,
        path: &str,
        authorization: Option<&str>,
        cookie: Option<&str>,
    ) -> handlers::Response {
        block_on(handlers::handle(
            &h.ctx(),
            &handlers::Request {
                method: method.to_string(),
                path: path.to_string(),
                cookie: cookie.map(str::to_string),
                origin: Some(ORIGIN.to_string()),
                authorization: authorization.map(str::to_string),
                body: Vec::new(),
            },
        ))
    }

    fn bearer_harness() -> (Harness, String, String, String) {
        let (h, cookie) = logged_in_harness();
        let minted = h.mint(&cookie, serde_json::json!({
            "label": "synthetic surface acceptance",
            "scopes": ["library:read", "playlists:write", "plays:write", "stream:read", "ingest:submit"],
            "ttl_secs": 3600
        }));
        assert_eq!(minted.status, 201, "synthetic setup must mint");
        let body: serde_json::Value = serde_json::from_str(&minted.body).unwrap();
        let authorization = format!("Bearer {}", body["secret"].as_str().unwrap());
        let id = body["id"].as_str().unwrap().to_string();
        (h, cookie, authorization, id)
    }

    fn spend(h: &Harness) -> (usize, usize, usize) {
        (
            h.store.rate_limit_calls.get(),
            h.store.native_lookup_calls.get(),
            h.store.native_use_calls.get(),
        )
    }

    #[test]
    fn invalid_and_absent_bearers_have_the_same_plain_not_found_surface() {
        // Catch mapped-path disclosure by malformed, unknown, revoked or expired keys,
        // including an attempted rescue by an otherwise valid session cookie.
        let paths = [
            ("GET", "/v1/library"),
            ("GET", "/v1/artists"),
            ("GET", "/v1/releases"),
            ("GET", "/v1/tracks"),
            ("GET", "/v1/search"),
            ("GET", "/v1/playlists"),
            ("POST", "/v1/playlists"),
            ("PATCH", "/v1/playlists/synthetic"),
            ("POST", "/v1/plays"),
            ("GET", "/v1/stream"),
            ("POST", "/v1/ingest"),
            ("GET", "/v1/not-a-route"),
            ("DELETE", "/v1/tracks"),
        ];
        for (method, path) in paths {
            for state in ["malformed", "unknown", "revoked", "expired"] {
                let (h, cookie, bearer, id) = bearer_harness();
                if state == "revoked" {
                    h.store
                        .native_credentials
                        .borrow_mut()
                        .get_mut(&id)
                        .unwrap()
                        .record
                        .revoked_at = Some(NOW - 1);
                } else if state == "expired" {
                    h.store
                        .native_credentials
                        .borrow_mut()
                        .get_mut(&id)
                        .unwrap()
                        .record
                        .expires_at = NOW - 1;
                }
                let header = match state {
                    "malformed" => "Bearer ",
                    "unknown" => "Bearer ntk_syntheticUnknownKey0000000000000000000000000",
                    _ => &bearer,
                };
                for cookie in [None, Some(cookie.as_str())] {
                    let absent = request(&h, method, path, None, cookie);
                    let invalid = request(&h, method, path, Some(header), cookie);
                    assert_eq!(absent.status, 404, "{method} {path}: absent posture");
                    assert_eq!(
                        invalid.status, 404,
                        "{method} {path}: {state} must not disclose route existence"
                    );
                    assert_eq!(
                        invalid.body, absent.body,
                        "{method} {path}: generic body equality"
                    );
                    assert!(invalid.set_cookie.is_none(), "no session rescue/refresh");
                    assert_eq!(
                        h.store.native_use_calls.get(),
                        0,
                        "invalid keys cannot record successful use"
                    );
                    assert!(
                        !invalid.body.contains("ntk_"),
                        "no credential material in refusal"
                    );
                }
            }
        }
    }

    #[test]
    fn valid_bearer_and_session_and_anonymous_routing_remain_distinct() {
        let (h, cookie, bearer, id) = bearer_harness();
        let before = spend(&h);
        let valid = request(&h, "GET", "/v1/search", Some(&bearer), None);
        assert_eq!(
            valid.status, 404,
            "data dispatch remains outside this slice"
        );
        assert!(
            h.store.native_lookup_calls.get() > before.1,
            "real bearer lookup must run"
        );
        assert_eq!(h.store.native_use_calls.get(), before.2 + 1);
        assert_eq!(
            h.store.native_credentials.borrow()[&id].last_used_at,
            Some(NOW)
        );
        // A valid credential without the required scope stays forbidden, not hidden.
        h.store
            .native_credentials
            .borrow_mut()
            .get_mut(&id)
            .unwrap()
            .record
            .scopes = vec![Scope::LibraryRead];
        assert_eq!(
            request(&h, "POST", "/v1/plays", Some(&bearer), None).status,
            403
        );
        assert_eq!(
            request(&h, "GET", "/v1/credentials", None, Some(&cookie)).status,
            200
        );
        assert_eq!(
            request(&h, "GET", "/v1/credentials", None, None).status,
            401
        );
        assert_eq!(
            request(&h, "GET", "/v1/credentials", Some(&bearer), None).status,
            401
        );
        assert_eq!(request(&h, "GET", "/healthz", None, None).status, 200);
    }

    #[test]
    fn nested_playlist_writes_are_rejected_before_any_bearer_store_spend() {
        for method in ["PUT", "PATCH", "DELETE"] {
            let (h, _cookie, bearer, _id) = bearer_harness();
            for path in [
                "/v1/playlists/synthetic/tracks",
                "/v1/playlists/synthetic/tracks/child",
                "/v1/playlists/synthetic/",
                "/v1/playlists//",
            ] {
                let before = spend(&h);
                let response = request(&h, method, path, Some(&bearer), None);
                assert_eq!(response.status, 404, "{method} {path}: reject nested shape");
                assert_eq!(
                    spend(&h),
                    before,
                    "{method} {path}: no admission, lookup or last-used write"
                );
            }
            let before = spend(&h);
            let response = request(&h, method, "/v1/playlists/synthetic", Some(&bearer), None);
            assert_eq!(
                response.status, 404,
                "single-item data dispatch is unchanged"
            );
            assert!(
                h.store.rate_limit_calls.get() > before.0,
                "positive budget control"
            );
            assert!(
                h.store.native_lookup_calls.get() > before.1,
                "positive lookup control"
            );
            assert_eq!(
                h.store.native_use_calls.get(),
                before.2 + 1,
                "single-item route remains authorized"
            );
        }
    }

    #[test]
    fn options_preflight_answers_exactly_the_served_paths() {
        // Catch preflight-blocked lawful bearer calls (served native
        // paths 404 on OPTIONS, so HEAD revalidation and PATCH edits can
        // never clear a browser preflight) as well as a preflight that
        // maps unserved, nested or unknown shapes. A presented junk
        // bearer must not change the preflight posture either.
        let (h, _cookie) = logged_in_harness();
        for path in [
            // Dispatch-known controls: preflight already answers here.
            "/healthz",
            "/v1/auth/passkey/register/start",
            "/v1/auth/passkey/register/finish",
            "/v1/auth/passkey/login/start",
            "/v1/auth/passkey/login/finish",
            "/v1/session/reauthenticate/start",
            "/v1/session/reauthenticate/finish",
            "/v1/auth/magic/start",
            "/v1/auth/magic/finish",
            "/v1/session/refresh",
            "/v1/session/logout",
            "/v1/session/revoke-all",
            "/v1/vault",
            "/v1/vault/test",
            "/v1/vault/test-connection",
            "/v1/credentials",
            "/v1/credentials/synthetic-id",
            // Scope-map-served native paths.
            "/v1/library",
            "/v1/artists",
            "/v1/releases",
            "/v1/tracks",
            "/v1/search",
            "/v1/playlists",
            "/v1/plays",
            "/v1/stream",
            "/v1/ingest",
            "/v1/playlists/playlist-synthetic",
        ] {
            for authorization in [
                None,
                Some("Bearer ntk_syntheticJunkPreflight000000000000000"),
            ] {
                let response = request(&h, "OPTIONS", path, authorization, None);
                assert_eq!(
                    response.status, 204,
                    "OPTIONS {path}: served paths must preflight"
                );
                assert!(response.body.is_empty(), "OPTIONS {path}: empty 204");
                assert!(response.set_cookie.is_none(), "OPTIONS {path}: no cookie");
            }
        }
        for path in [
            "/v1/not-a-route",
            "/nope",
            "/v1/playlists/playlist-synthetic/tracks",
            "/v1/playlists/playlist-synthetic/tracks/track-synthetic",
            "/v1/playlists/playlist-synthetic/",
            "/v1/playlists//",
            "/v1/credentials/synthetic-id/extra",
            "/v1/credentials/",
        ] {
            let response = request(&h, "OPTIONS", path, None, None);
            assert_eq!(
                response.status, 404,
                "OPTIONS {path}: unserved shapes stay hidden"
            );
        }
    }

    #[test]
    fn credentials_golden_fixture_covers_served_routes_in_both_directions() {
        // Catch a served credentials operation missing from the fixture, as well
        // as a fixture row that names an unserved operation or invalid mint body.
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/api-web-contract.json")).unwrap();
        let routes = fixture["routes"].as_array().unwrap();
        let credentials: Vec<_> = routes
            .iter()
            .filter(|row| row["path"].as_str().unwrap().starts_with("/v1/credentials"))
            .collect();
        assert_eq!(
            credentials.len(),
            3,
            "mint, list and single revoke must all be represented"
        );
        for (method, path) in [
            ("POST", "/v1/credentials"),
            ("GET", "/v1/credentials"),
            ("DELETE", "/v1/credentials/{id}"),
        ] {
            let matching: Vec<_> = credentials
                .iter()
                .filter(|row| {
                    let actual = row["path"].as_str().unwrap();
                    let shape = if actual
                        .strip_prefix("/v1/credentials/")
                        .is_some_and(|tail| !tail.is_empty() && !tail.contains('/'))
                    {
                        "/v1/credentials/{id}"
                    } else {
                        actual
                    };
                    row["method"] == method && shape == path
                })
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "missing or duplicate fixture operation {method} {path}"
            );
        }
        let (h, cookie) = logged_in_harness();
        for row in credentials {
            let method = row["method"].as_str().unwrap();
            let path = row["path"].as_str().unwrap();
            assert!(
                row["web"].as_str().is_some_and(|name| !name.is_empty()),
                "every credentials route needs a web builder"
            );
            let body = row.get("wire");
            if method == "POST" {
                let wire = body.expect("mint wire body required");
                let mut fields: Vec<_> = wire
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                fields.sort_unstable();
                assert_eq!(fields, ["label", "scopes", "ttl_secs"], "mint wire shape");
                assert!(row.get("input").is_some(), "mint builder input required");
                let minted = h.call(method, path, body, Some(&cookie));
                assert_eq!(minted.status, 201, "fixture mint must be accepted");
                let value: serde_json::Value = serde_json::from_str(&minted.body).unwrap();
                let id = value["id"].as_str().unwrap();
                let listed = h.call("GET", "/v1/credentials", None, Some(&cookie));
                assert_eq!(listed.status, 200);
                assert!(
                    !listed.body.contains(value["secret"].as_str().unwrap()),
                    "one-time secret not listed"
                );
                let revoked = h.call(
                    "DELETE",
                    &format!("/v1/credentials/{id}"),
                    None,
                    Some(&cookie),
                );
                assert_eq!(revoked.status, 200);
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&revoked.body).unwrap(),
                    serde_json::json!({"ok": true})
                );
                let mut unknown = wire.clone();
                unknown["unexpected"] = serde_json::json!(true);
                let before = h.rows().len();
                assert_eq!(
                    h.call(method, path, Some(&unknown), Some(&cookie)).status,
                    422,
                    "mint denies unknown fields"
                );
                assert_eq!(h.rows().len(), before, "unknown field cannot mint");
            } else {
                let response = h.call(method, path, body, None);
                assert_eq!(
                    response.status, 401,
                    "fixture route must be served and session-only: {method} {path}"
                );
            }
        }
    }
}

// --- Harness -------------------------------------------------------------------

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
        .expect("test config is valid")
        .with_native_rest_pepper(vec![7u8; 32], 1),
        store: FakeStore::default(),
        mailer: FakeMailer::default(),
        dialer: FakeDialer,
        dns: FakeDns,
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

    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        cookie: Option<&str>,
    ) -> handlers::Response {
        let request = handlers::Request {
            method: method.to_string(),
            path: path.to_string(),
            cookie: cookie.map(str::to_string),
            origin: Some(ORIGIN.to_string()),
            authorization: None,
            body: body
                .map(|value| value.to_string().into_bytes())
                .unwrap_or_default(),
        };
        block_on(handlers::handle(&self.ctx(), &request))
    }

    fn mint(&self, cookie: &str, body: serde_json::Value) -> handlers::Response {
        self.call("POST", "/v1/credentials", Some(&body), Some(cookie))
    }

    fn rows(&self) -> Vec<handlers::NativeCredentialRow> {
        block_on(self.store.native_credentials_for_user(USER_ID)).expect("list stored rows")
    }

    fn mint_audits(&self) -> usize {
        self.store
            .audits
            .borrow()
            .iter()
            .filter(|event| event.action.starts_with("native_credential_created:"))
            .count()
    }
}

fn seed_user(harness: &Harness) {
    block_on(harness.store.insert_user(&handlers::UserRecord {
        id: USER_ID.to_string(),
        email: EMAIL.to_string(),
        created_at: NOW,
    }))
    .expect("seed user");
}

fn magic_login(harness: &Harness) -> String {
    let start = harness.call(
        "POST",
        "/v1/auth/magic/start",
        Some(&serde_json::json!({"email": EMAIL})),
        None,
    );
    assert_eq!(start.status, 200, "{}", start.body);
    let token = harness
        .mailer
        .sent
        .borrow()
        .last()
        .expect("magic mail sent")
        .1
        .clone();
    let finish = harness.call(
        "POST",
        "/v1/auth/magic/finish",
        Some(&serde_json::json!({"token": token})),
        None,
    );
    assert_eq!(finish.status, 200, "{}", finish.body);
    finish
        .set_cookie
        .expect("login sets a session cookie")
        .split(';')
        .next()
        .expect("cookie pair precedes attributes")
        .to_string()
}

fn logged_in_harness() -> (Harness, String) {
    let harness = harness();
    seed_user(&harness);
    let cookie = magic_login(&harness);
    // Control: login itself mints no credential and writes no mint audit,
    // so the counts below measure only the mint under test.
    assert!(harness.rows().is_empty(), "login must store no credential");
    assert_eq!(harness.mint_audits(), 0, "login must write no mint audit");
    (harness, cookie)
}

fn assert_validation_rejection(harness: &Harness, resp: &handlers::Response, case: &str) {
    assert_eq!(
        resp.status, 422,
        "{case}: expected 422 validation_failed, got {}: {}",
        resp.status, resp.body
    );
    assert!(
        resp.body.contains("\"code\":\"validation_failed\""),
        "{case}: body must carry the validation_failed code: {}",
        resp.body
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&resp.body).expect("rejection body is JSON");
    assert!(
        parsed.get("secret").is_none(),
        "{case}: rejection must carry no secret key: {}",
        resp.body
    );
    assert!(
        !resp.body.contains("ntk_"),
        "{case}: rejection must carry no credential material: {}",
        resp.body
    );
    assert!(
        resp.set_cookie.is_none(),
        "{case}: rejection must not set a cookie"
    );
    assert!(
        harness.rows().is_empty(),
        "{case}: rejected mint must insert zero credential rows"
    );
    assert_eq!(
        harness.mint_audits(),
        0,
        "{case}: rejected mint must write no native_credential_created audit row"
    );
}

// --- Acceptance ----------------------------------------------------------------

#[test]
fn zero_ttl_mint_is_validation_failed_and_mints_nothing() {
    let (harness, cookie) = logged_in_harness();

    let resp = harness.mint(
        &cookie,
        serde_json::json!({"label": "cli", "scopes": ["library:read"], "ttl_secs": 0}),
    );

    // A zero TTL would otherwise mint a credential already expired at
    // issuance (expires_at == created_at): a dead key the client only
    // discovers on first use. It must reject as a validation error.
    assert_validation_rejection(&harness, &resp, "ttl_secs: 0");
}

#[test]
fn over_long_scope_list_is_validation_failed_and_mints_nothing() {
    let (harness, cookie) = logged_in_harness();

    // One entry past the defined vocabulary: every name is a valid ADR
    // scope, so only the raw-length rule can reject this list.
    let mut scopes: Vec<&str> = ALL_SCOPES.to_vec();
    scopes.push(ALL_SCOPES[0]);
    assert_eq!(
        scopes.len(),
        ALL_SCOPES.len() + 1,
        "fixture must be exactly one over the defined scope count"
    );

    let resp = harness.mint(
        &cookie,
        serde_json::json!({"label": "cli", "scopes": scopes, "ttl_secs": 3600}),
    );
    assert_validation_rejection(&harness, &resp, "scope list longer than ALL_SCOPES");

    // A far-over list of valid names rejects the same way (the cap is a
    // length rule, not a one-past special case) and still mints nothing.
    let repeated = vec![ALL_SCOPES[0]; ALL_SCOPES.len() * 4];
    let resp = harness.mint(
        &cookie,
        serde_json::json!({"label": "cli", "scopes": repeated, "ttl_secs": 3600}),
    );
    assert_validation_rejection(&harness, &resp, "scope list far longer than ALL_SCOPES");
}

#[test]
fn duplicate_scopes_are_deduplicated_in_response_and_stored_row() {
    let (harness, cookie) = logged_in_harness();

    // Raw length stays within the cap (5 == ALL_SCOPES.len()), so the
    // request is otherwise valid; only its duplicates are in question.
    // First-seen order here is stream:read before library:read, which
    // differs from both canonical ALL_SCOPES order and alphabetical
    // order, so a canonical- or alphabetical-order dedupe fails this.
    let scopes = [
        "stream:read",
        "library:read",
        "stream:read",
        "library:read",
        "stream:read",
    ];
    assert_eq!(scopes.len(), ALL_SCOPES.len());

    let resp = harness.mint(
        &cookie,
        serde_json::json!({"label": "cli", "scopes": scopes, "ttl_secs": 3600}),
    );
    assert_eq!(resp.status, 201, "{}", resp.body);
    let minted: serde_json::Value = serde_json::from_str(&resp.body).expect("mint body is JSON");
    assert_eq!(
        minted["scopes"],
        serde_json::json!(["stream:read", "library:read"]),
        "response scopes must be deduplicated, first-seen order preserved"
    );
    assert_eq!(minted["expires_at"], NOW + 3600);

    let rows = harness.rows();
    assert_eq!(rows.len(), 1, "valid duplicate-bearing mint stores one row");
    assert_eq!(
        rows[0].record.scopes,
        vec![Scope::StreamRead, Scope::LibraryRead],
        "stored row must carry the deduplicated scope set"
    );
    assert_eq!(
        harness.mint_audits(),
        1,
        "the deduplicated mint audits exactly once"
    );
}

#[test]
fn happy_path_mint_is_unchanged_within_the_365_day_cap() {
    let (harness, cookie) = logged_in_harness();

    let resp = harness.mint(
        &cookie,
        serde_json::json!({
            "label": "cli",
            "scopes": ["library:read", "plays:write"],
            "ttl_secs": 3600,
        }),
    );
    assert_eq!(resp.status, 201, "{}", resp.body);
    let minted: serde_json::Value = serde_json::from_str(&resp.body).expect("mint body is JSON");
    let secret = minted["secret"]
        .as_str()
        .expect("happy path still returns the one-time secret");
    assert!(
        secret.starts_with("ntk_"),
        "display form carries the class prefix"
    );
    assert_eq!(
        minted["scopes"],
        serde_json::json!(["library:read", "plays:write"])
    );
    assert_eq!(minted["expires_at"], NOW + 3600);
    assert_eq!(harness.rows().len(), 1);
    assert_eq!(harness.mint_audits(), 1);

    // Boundaries adjacent to the new rejections stay valid: the smallest
    // positive TTL and a TTL exactly at the 365-day cap both mint.
    for (ttl, expires_at) in [
        (1, NOW + 1),
        (MAX_CREDENTIAL_TTL_SECS, NOW + MAX_CREDENTIAL_TTL_SECS),
    ] {
        let resp = harness.mint(
            &cookie,
            serde_json::json!({"scopes": ["library:read"], "ttl_secs": ttl}),
        );
        assert_eq!(resp.status, 201, "ttl_secs={ttl}: {}", resp.body);
        let minted: serde_json::Value =
            serde_json::from_str(&resp.body).expect("mint body is JSON");
        assert_eq!(minted["expires_at"], expires_at, "ttl_secs={ttl}");
    }
    assert_eq!(harness.rows().len(), 3);
    assert_eq!(harness.mint_audits(), 3);
}
