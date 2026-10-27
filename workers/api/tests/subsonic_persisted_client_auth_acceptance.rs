//! Independent acceptance bank for migration 0023 per-client API-key auth.
//!
//! Baseline compile-RED is intentional: there is no persisted Subsonic adapter,
//! and nntune-subsonic is currently a wasm-only dependency of this package.
//! Builder supplies a native-testable production module `subsonic_auth` and
//! makes the existing protocol dependency available to native tests. No test
//! fallback belongs in this file; do not merge this bank without its adapter.
//!
//! Expected narrow production seam (all names below are in `subsonic_auth`):
//! - `SubsonicClientRow: Clone + Debug` is a lookup projection with public
//!   `client_id: String`, `user_id: String`, `api_key_hmac: String`,
//!   `identity: nntune_subsonic::Identity`, `revoked_at: Option<u64>`, and
//!   `owner_disabled: bool`. Debug must redact the HMAC. This projection is
//!   not a replacement schema; ciphertext is unnecessary for API-key auth.
//! - `SubsonicClientLookup::client_by_hmac(&self, hmac: &str)` returns
//!   `impl Future<Output = Result<Option<SubsonicClientRow>, handlers::StoreError>>`.
//! - `prepare_subsonic_auth<L: SubsonicClientLookup>(lookup: Option<&L>,
//!   pepper: Option<&[u8]>, credentials: &nntune_subsonic::Credentials)` is async
//!   and returns `PreparedSubsonicAuth: nntune_subsonic::AuthStore + Debug`.
//!   It resolves this request only; absent configuration, lookup errors,
//!   revoked/disabled owners, and mismatching records are deny-only results.
//!   The Worker must use this same seam, not a test-only implementation.
//!
//! Sources: migrations/0023_subsonic_clients.sql and ADR 0001. Do not silently
//! replace the reviewed 40/43/44/10 contract with ADR 0006's future redesign,
//! invent a raw-key alphabet, or implement legacy sealed-password/token auth.
//! Synthetic lookup doubles replace external persistence, not authentication.
//! These tests do NOT prove real SQL, Worker binding wiring, live logs, staging,
//! provisioning, or native-client compatibility.

#[path = "../../../crates/nntune-subsonic/tests/support/client_auth_library.rs"]
mod client_auth_library;

use client_auth_library::GuardedLibrary;
use nntune_api::handlers::StoreError;
use nntune_api::subsonic_auth::{prepare_subsonic_auth, SubsonicClientLookup, SubsonicClientRow};
use nntune_subsonic::{AuthStore, Credentials, HttpResponse, Identity, Params, SubsonicApp};
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

// Public, deliberately synthetic fixtures; no display-format constraint is
// inferred from these strings. Literal digests were independently calculated
// with Python hmac.new(pepper, key.encode(), hashlib.sha256).hexdigest().
const PEPPER: &[u8] = b"synthetic-subsonic-index-pepper-v1!";
const ALPHA_KEY: &str = "synthetic-client-alpha-0123456789abcdef";
const BETA_KEY: &str = "synthetic-client-beta-fedcba9876543210";
const ALPHA_HMAC: &str = "524f316146ebc41db98bf8e3881dd7038659624f1061d24b30d8ab1a1d576a05";
const BETA_HMAC: &str = "8b3b21ca0a0fb0320bb2b717861d3d910a033a4e173d9dec0e2b5cde83b60b10";

fn alpha_identity() -> Identity {
    Identity {
        username: "owner-alpha".into(),
        email: "alpha@example.test".into(),
        is_admin: false,
    }
}

fn beta_identity() -> Identity {
    Identity {
        username: "owner-beta".into(),
        email: "beta@example.test".into(),
        is_admin: false,
    }
}

fn alpha_row() -> SubsonicClientRow {
    SubsonicClientRow {
        client_id: "synthetic-client-alpha".into(),
        user_id: "synthetic-user-alpha".into(),
        api_key_hmac: ALPHA_HMAC.into(),
        identity: alpha_identity(),
        revoked_at: None,
        owner_disabled: false,
    }
}

fn beta_row() -> SubsonicClientRow {
    SubsonicClientRow {
        client_id: "synthetic-client-beta".into(),
        user_id: "synthetic-user-beta".into(),
        api_key_hmac: BETA_HMAC.into(),
        identity: beta_identity(),
        revoked_at: None,
        owner_disabled: false,
    }
}

enum Reply {
    Rows(Vec<SubsonicClientRow>),
    WrongRecord(SubsonicClientRow),
    Unavailable,
}

struct SyntheticLookup {
    reply: RefCell<Reply>,
    lookups: Rc<RefCell<Vec<String>>>,
}

impl SyntheticLookup {
    fn new(rows: Vec<SubsonicClientRow>) -> Self {
        Self {
            reply: RefCell::new(Reply::Rows(rows)),
            lookups: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl SubsonicClientLookup for SyntheticLookup {
    async fn client_by_hmac(&self, hmac: &str) -> Result<Option<SubsonicClientRow>, StoreError> {
        self.lookups.borrow_mut().push(hmac.into());
        match &*self.reply.borrow() {
            Reply::Rows(rows) => Ok(rows.iter().find(|row| row.api_key_hmac == hmac).cloned()),
            Reply::WrongRecord(row) => Ok(Some(row.clone())),
            Reply::Unavailable => Err(StoreError),
        }
    }
}

fn finish<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "synthetic lookup did not complete"
        );
        std::thread::yield_now();
    }
}

fn credentials(query: &str) -> Credentials {
    let params = Params::parse(query);
    Credentials::from_params(|key| params.get(key).map(str::to_owned))
}

fn request(
    lookup: Option<&SyntheticLookup>,
    pepper: Option<&[u8]>,
    query: &str,
) -> (HttpResponse, Vec<Identity>) {
    let auth = finish(prepare_subsonic_auth(lookup, pepper, &credentials(query)));
    let (library, calls) = GuardedLibrary::new(auth);
    let response = SubsonicApp::new(library).handle(
        "GET",
        "/rest/getPlaylists",
        &format!("f=json&{query}"),
        "",
    );
    let calls = calls.borrow().clone();
    (response, calls)
}

fn assert_denied(result: &(HttpResponse, Vec<Identity>), code: i64, message: &str) {
    let (response, library_calls) = result;
    assert_eq!(response.status, 200);
    assert_eq!(response.content_type, "application/json");
    let document: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(document["subsonic-response"]["status"], "failed");
    assert_eq!(document["subsonic-response"]["error"]["code"], code);
    assert_eq!(document["subsonic-response"]["error"]["message"], message);
    assert!(library_calls.is_empty(), "denied auth reached the Library");
}

fn assert_owner(result: &(HttpResponse, Vec<Identity>), expected: Identity) {
    let document: serde_json::Value = serde_json::from_slice(&result.0.body).unwrap();
    assert_eq!(result.0.status, 200);
    assert_eq!(document["subsonic-response"]["status"], "ok");
    assert_eq!(result.1, [expected]);
}

#[test]
fn persisted_keys_resolve_only_their_owning_users() {
    let lookup = SyntheticLookup::new(vec![alpha_row(), beta_row()]);
    let alpha = request(Some(&lookup), Some(PEPPER), &format!("apiKey={ALPHA_KEY}"));
    let beta = request(Some(&lookup), Some(PEPPER), &format!("apiKey={BETA_KEY}"));
    assert_owner(&alpha, alpha_identity());
    assert_owner(&beta, beta_identity());
}

#[test]
fn external_lookup_receives_the_peppered_hmac_not_the_display_key() {
    let lookup = SyntheticLookup::new(vec![alpha_row()]);
    let result = request(Some(&lookup), Some(PEPPER), &format!("apiKey={ALPHA_KEY}"));
    assert_owner(&result, alpha_identity());
    assert_eq!(*lookup.lookups.borrow(), [ALPHA_HMAC]);
}

#[test]
fn missing_empty_unknown_and_malformed_credentials_fail_before_library_access() {
    for (query, code, message) in [
        ("", 40, "Wrong username or password."),
        ("apiKey=", 40, "Wrong username or password."),
        ("apiKey=synthetic-unknown-key", 44, "Invalid API key."),
        ("apiKey=%00", 44, "Invalid API key."),
        ("apiKey=%ZZ", 44, "Invalid API key."),
        ("apiKey[]=synthetic", 40, "Wrong username or password."),
    ] {
        let lookup = SyntheticLookup::new(vec![alpha_row()]);
        assert_denied(&request(Some(&lookup), Some(PEPPER), query), code, message);
    }
}

#[test]
fn wrong_user_and_mixed_legacy_parameters_are_rejected_before_persisted_lookup() {
    for legacy in ["u=owner-beta", "p=synthetic", "t=synthetic", "s=synthetic"] {
        let lookup = SyntheticLookup::new(vec![alpha_row(), beta_row()]);
        let result = request(
            Some(&lookup),
            Some(PEPPER),
            &format!("apiKey={ALPHA_KEY}&{legacy}"),
        );
        assert_denied(&result, 43, "Conflicting authentication parameters.");
        assert!(lookup.lookups.borrow().is_empty());
    }
}

#[test]
fn empty_legacy_values_do_not_conflict_with_a_valid_api_key() {
    let lookup = SyntheticLookup::new(vec![alpha_row()]);
    assert_owner(
        &request(
            Some(&lookup),
            Some(PEPPER),
            &format!("apiKey={ALPHA_KEY}&u=&p=&t=&s="),
        ),
        alpha_identity(),
    );
}

#[test]
fn opaque_keys_are_not_trimmed_or_case_folded_before_lookup() {
    for query in [
        format!("apiKey=%20{ALPHA_KEY}"),
        format!("apiKey={ALPHA_KEY}%20"),
        format!("apiKey={}", ALPHA_KEY.to_ascii_uppercase()),
    ] {
        let lookup = SyntheticLookup::new(vec![alpha_row()]);
        assert_denied(
            &request(Some(&lookup), Some(PEPPER), &query),
            44,
            "Invalid API key.",
        );
    }
}

#[test]
fn revoked_and_disabled_owner_rows_are_indistinguishable_from_unknown_keys() {
    let unknown = request(
        Some(&SyntheticLookup::new(Vec::new())),
        Some(PEPPER),
        &format!("apiKey={ALPHA_KEY}"),
    );
    assert_denied(&unknown, 44, "Invalid API key.");
    let mut revoked = alpha_row();
    revoked.revoked_at = Some(1_800_000_000);
    let mut disabled = alpha_row();
    disabled.owner_disabled = true;
    for row in [revoked, disabled] {
        let lookup = SyntheticLookup::new(vec![row]);
        let result = request(Some(&lookup), Some(PEPPER), &format!("apiKey={ALPHA_KEY}"));
        assert_denied(&result, 44, "Invalid API key.");
        assert_eq!(result.0, unknown.0);
    }
}

#[test]
fn revocation_after_success_does_not_reuse_identity_or_revoke_another_client() {
    let mut sibling = beta_row();
    sibling.user_id = "synthetic-user-alpha".into();
    sibling.identity = alpha_identity();
    let lookup = SyntheticLookup::new(vec![alpha_row(), sibling.clone()]);
    assert_owner(
        &request(Some(&lookup), Some(PEPPER), &format!("apiKey={ALPHA_KEY}")),
        alpha_identity(),
    );
    let mut revoked = alpha_row();
    revoked.revoked_at = Some(1_800_000_000);
    *lookup.reply.borrow_mut() = Reply::Rows(vec![revoked, sibling]);
    assert_denied(
        &request(Some(&lookup), Some(PEPPER), &format!("apiKey={ALPHA_KEY}")),
        44,
        "Invalid API key.",
    );
    assert_owner(
        &request(Some(&lookup), Some(PEPPER), &format!("apiKey={BETA_KEY}")),
        alpha_identity(),
    );
}

#[test]
fn store_error_after_success_denies_without_cached_identity_or_library_access() {
    let lookup = SyntheticLookup::new(vec![alpha_row()]);
    assert_owner(
        &request(Some(&lookup), Some(PEPPER), &format!("apiKey={ALPHA_KEY}")),
        alpha_identity(),
    );
    *lookup.reply.borrow_mut() = Reply::Unavailable;
    assert_denied(
        &request(Some(&lookup), Some(PEPPER), &format!("apiKey={ALPHA_KEY}")),
        44,
        "Invalid API key.",
    );
}

#[test]
fn missing_store_or_missing_empty_or_wrong_pepper_fails_closed() {
    let lookup = SyntheticLookup::new(vec![alpha_row()]);
    let query = format!("apiKey={ALPHA_KEY}");
    for key in [ALPHA_KEY, "test-key-1", "test-key-2"] {
        assert_denied(
            &request(None, Some(PEPPER), &format!("apiKey={key}")),
            44,
            "Invalid API key.",
        );
    }
    for pepper in [None, Some(&b""[..])] {
        assert_denied(
            &request(Some(&lookup), pepper, &query),
            44,
            "Invalid API key.",
        );
    }
    assert!(
        lookup.lookups.borrow().is_empty(),
        "missing config must not query storage"
    );
    assert_denied(
        &request(Some(&lookup), Some(b"synthetic-wrong-pepper"), &query),
        44,
        "Invalid API key.",
    );
}

#[test]
fn a_successfully_prepared_identity_is_bound_to_the_exact_presented_key() {
    let lookup = SyntheticLookup::new(vec![alpha_row(), beta_row()]);
    let auth = finish(prepare_subsonic_auth(
        Some(&lookup),
        Some(PEPPER),
        &credentials(&format!("apiKey={ALPHA_KEY}")),
    ));
    assert_eq!(auth.verify_api_key(ALPHA_KEY), Some(alpha_identity()));
    assert_eq!(auth.verify_api_key(BETA_KEY), None);
    assert_eq!(auth.verify_api_key(""), None);
    let (library, calls) = GuardedLibrary::new(auth);
    let response = SubsonicApp::new(library).handle(
        "POST",
        "/rest/getPlaylists.view",
        "f=json",
        &format!("apiKey={BETA_KEY}"),
    );
    assert_denied(&(response, calls.borrow().clone()), 44, "Invalid API key.");
}

#[test]
fn a_repository_returning_another_keys_row_cannot_cross_authenticate() {
    let lookup = SyntheticLookup::new(Vec::new());
    *lookup.reply.borrow_mut() = Reply::WrongRecord(beta_row());
    assert_denied(
        &request(Some(&lookup), Some(PEPPER), &format!("apiKey={ALPHA_KEY}")),
        44,
        "Invalid API key.",
    );
}

#[test]
fn persisted_api_key_support_does_not_enable_legacy_password_or_token_auth() {
    let lookup = SyntheticLookup::new(vec![alpha_row()]);
    for (query, code, message) in [
        (
            format!("u=owner-alpha&p={ALPHA_KEY}"),
            40,
            "Wrong username or password.",
        ),
        (
            "u=owner-alpha&p=enc:73796e746865746963".into(),
            40,
            "Wrong username or password.",
        ),
        (
            "u=owner-alpha&t=10a0a7011b920dfd0118c43ff19f3515&s=synthetic-salt".into(),
            40,
            "Wrong username or password.",
        ),
        (
            "u=owner-alpha&t=10a0a7011b920dfd0118c43ff19f3515".into(),
            10,
            "Required parameter is missing.",
        ),
    ] {
        assert_denied(&request(Some(&lookup), Some(PEPPER), &query), code, message);
    }
    assert!(
        lookup.lookups.borrow().is_empty(),
        "legacy auth must not read API-key rows"
    );
    let auth = finish(prepare_subsonic_auth(
        Some(&lookup),
        Some(PEPPER),
        &credentials(&format!("apiKey={ALPHA_KEY}")),
    ));
    assert_eq!(auth.verify_password("owner-alpha", ALPHA_KEY), None);
    assert_eq!(
        auth.verify_token(
            "owner-alpha",
            "10a0a7011b920dfd0118c43ff19f3515",
            "synthetic-salt"
        ),
        None
    );
}

#[test]
fn a_valid_web_session_is_not_an_api_key_or_a_password_fallback() {
    use nntune_api::session::{
        issue_session, verify_session, verify_stored_session, SessionKeys, SessionRecord,
    };
    let session_secret = b"synthetic-session-signing-secret-32-bytes";
    let now = 1_800_000_000;
    let token = issue_session(session_secret, "synthetic-user-alpha", now, 300).unwrap();
    let claims = verify_session(session_secret, &token, now).unwrap();
    let record = SessionRecord {
        id: claims.session_id.clone(),
        user_id: claims.user_id.clone(),
        created_at: claims.created_at,
        expires_at: claims.expires_at,
        rotation_counter: claims.rotation_counter,
        revoked: false,
    };
    assert!(
        verify_stored_session(&SessionKeys::single(session_secret), &token, now, &record).is_ok()
    );
    let lookup = SyntheticLookup::new(vec![alpha_row()]);
    for (query, code, message) in [
        (format!("apiKey={token}"), 44, "Invalid API key."),
        (
            format!("u=owner-alpha&p={token}"),
            40,
            "Wrong username or password.",
        ),
    ] {
        assert_denied(&request(Some(&lookup), Some(PEPPER), &query), code, message);
    }
}

#[test]
fn row_and_request_auth_debug_never_disclose_key_hmac_or_pepper() {
    let lookup = SyntheticLookup::new(vec![alpha_row()]);
    let auth = finish(prepare_subsonic_auth(
        Some(&lookup),
        Some(PEPPER),
        &credentials(&format!("apiKey={ALPHA_KEY}")),
    ));
    let row = alpha_row();
    for debug in [
        format!("{auth:?}"),
        format!("{auth:#?}"),
        format!("{row:?}"),
        format!("{row:#?}"),
    ] {
        for canary in [ALPHA_KEY, ALPHA_HMAC, std::str::from_utf8(PEPPER).unwrap()] {
            assert!(
                !debug.contains(canary),
                "credential material disclosed through Debug"
            );
        }
    }
}
