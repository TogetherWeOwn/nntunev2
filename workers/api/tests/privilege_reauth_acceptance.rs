//! Acceptance tests: privilege-change re-authentication with a fresh
//! session id.
//!
//! TDD RED: these tests FAIL (compile error) until the build implements
//! the contract below in `workers/api/src/session.rs`. The builder must
//! NEVER edit this file.
//!
//! Failing command (this card):
//!   `cargo test --manifest-path workers/api/Cargo.toml --test privilege_reauth_acceptance`
//!
//! Contract under test (implement exactly these items in `session.rs`;
//! pure Rust, native + wasm32, no Worker runtime imports — same
//! constraint as the rest of the crate):
//!
//! - `pub const PRIVILEGE_REAUTH_WINDOW_SECS: u64 = 600;` — privilege-gated
//!   operations require authentication no older than 10 minutes. The age
//!   is measured from `SessionClaims::created_at` (the immutable
//!   authentication time), NEVER from `issued_at`: a refresh rotation
//!   must not restore freshness.
//! - `pub const REAUTH_REQUIRED_CODE: &str = "reauth_required";` — the
//!   stable, user-facing error code the HTTP layer returns (in the
//!   structured error envelope) when a privilege-gated op needs fresh
//!   authentication. Reuse this code; do not invent a second one.
//! - `SessionError::ReauthRequired` — new variant, distinct from
//!   `SessionError::Invalid`, so the client can prompt for re-auth
//!   instead of treating the session as dead. Plus:
//!   `SessionError::code(&self) -> &'static str` (ReauthRequired maps to
//!   `REAUTH_REQUIRED_CODE`; Invalid maps to any single stable code that
//!   is NOT the re-auth code) and `SessionError::http_status(&self) ->
//!   u16` (401 for both — mirror `LibraryError::http_status`).
//!   `Display` stays generic: never leak why verification failed.
//! - `pub fn require_fresh_session(keys: &SessionKeys<'_>, token: &str,
//!   now: u64, record: &SessionRecord) -> Result<SessionClaims,
//!   SessionError>` — the privilege gate. It MUST first run the full
//!   `verify_stored_session` check (signature, row, counter, revocation,
//!   expiry) and propagate any failure as `Invalid` — an unauthenticated
//!   or revoked caller never receives the re-auth code (no oracle). Only
//!   a session that is otherwise valid but whose authentication age
//!   `now.saturating_sub(created_at)` exceeds
//!   `PRIVILEGE_REAUTH_WINDOW_SECS` returns `ReauthRequired`. The
//!   boundary is inclusive: age exactly equal to the window passes.
//!   Privilege-gated handlers call this instead of
//!   `verify_stored_session`; ordinary handlers are unchanged.
//! - `pub struct ReauthenticatedSession { pub token: String,
//!   pub new_record: SessionRecord, pub revoked_session_id: String }`
//!   (`Debug + Clone`) and
//!   `pub fn reauthenticate_session(keys: &SessionKeys<'_>,
//!   old_token: &str, now: u64, old_record: &SessionRecord,
//!   ttl_secs: u64) -> Result<ReauthenticatedSession, SessionError>`.
//!   Call ONLY after the caller has completed a fresh authentication
//!   proof (passkey/magic-link ceremony is out of scope here). It MUST
//!   `verify_stored_session` the old token first (any failure is
//!   `Invalid`, nothing is minted), then mint a NEW session id under
//!   `keys.primary` only (never the rotation-window previous secret),
//!   with `created_at == issued_at == now`, `rotation_counter == 0` and
//!   the expiry rules of `issue_session`. This is a pure prepare step,
//!   like `rotate_session`: the caller MUST atomically INSERT
//!   `new_record` and revoke exactly the one old row
//!   (`UPDATE sessions SET revoked = TRUE WHERE id =
//!   revoked_session_id`), commit, and only then send the new cookie.
//!   Single-session revoke — never a logout-everywhere.
//! - `workers/api/README.md` (Sessions bullet of the security contract)
//!   documents the freshness window value (600 seconds / 10 minutes)
//!   and the `reauth_required` code.
//!
//! Fixtures are synthetic only. No network, database, or secrets.

use base64::Engine as _;
use hmac::{Hmac, Mac};
use nntune_api::session::{
    self, SessionError, SessionKeys, SessionRecord, PRIVILEGE_REAUTH_WINDOW_SECS,
    REAUTH_REQUIRED_CODE,
};
use sha2::Sha256;

const NOW: u64 = 1_700_000_000;
const SECRET: &[u8] = b"synthetic-session-secret-at-least-32-bytes";
const USER: &str = "user-acceptance";

fn keys() -> SessionKeys<'static> {
    SessionKeys::single(SECRET)
}

fn record_for(claims: &session::SessionClaims) -> SessionRecord {
    SessionRecord {
        id: claims.session_id.clone(),
        user_id: claims.user_id.clone(),
        created_at: claims.created_at,
        expires_at: claims.expires_at,
        rotation_counter: claims.rotation_counter,
        revoked: false,
    }
}

/// A valid session issued at `NOW` with a one-hour TTL, plus its row.
fn active_session() -> (String, SessionRecord) {
    let token = session::issue_session(SECRET, USER, NOW, 3600).expect("issue");
    let claims = session::verify_session(SECRET, &token, NOW).expect("verify");
    let row = record_for(&claims);
    (token, row)
}

#[test]
fn freshness_window_and_reauth_code_are_the_documented_constants() {
    assert_eq!(PRIVILEGE_REAUTH_WINDOW_SECS, 600, "10-minute window");
    assert_eq!(REAUTH_REQUIRED_CODE, "reauth_required");
    assert_eq!(SessionError::ReauthRequired.code(), REAUTH_REQUIRED_CODE);
    assert_eq!(SessionError::ReauthRequired.http_status(), 401);
    assert_eq!(SessionError::Invalid.http_status(), 401);
    assert_ne!(
        SessionError::Invalid.code(),
        REAUTH_REQUIRED_CODE,
        "a dead session must not be reported as merely stale"
    );
    assert_ne!(SessionError::ReauthRequired, SessionError::Invalid);
}

#[test]
fn fresh_session_passes_privilege_gate_through_inclusive_boundary() {
    let (token, row) = active_session();
    for at in [NOW, NOW + 1, NOW + PRIVILEGE_REAUTH_WINDOW_SECS] {
        let claims = session::require_fresh_session(&keys(), &token, at, &row)
            .unwrap_or_else(|e| panic!("age {}s must pass the gate: {e}", at - NOW));
        assert_eq!(claims.user_id, USER);
        assert_eq!(claims.session_id, row.id);
        assert_eq!(claims.created_at, NOW);
    }
}

#[test]
fn stale_session_rejected_with_reauth_code_while_ordinary_auth_still_passes() {
    let (token, row) = active_session();
    let stale_at = NOW + PRIVILEGE_REAUTH_WINDOW_SECS + 1;
    // The session itself is still valid for ordinary operations ...
    assert!(
        session::verify_stored_session(&keys(), &token, stale_at, &row).is_ok(),
        "freshness is a privilege gate, not a second session expiry"
    );
    // ... but a privilege-gated op demands re-authentication, with the
    // documented code — not the generic invalid-session failure.
    let err = session::require_fresh_session(&keys(), &token, stale_at, &row)
        .expect_err("stale authentication must not authorize a privilege change");
    assert_eq!(err, SessionError::ReauthRequired);
    assert_eq!(err.code(), "reauth_required");
    assert_eq!(err.http_status(), 401);
}

#[test]
fn refresh_rotation_does_not_restore_freshness() {
    // Freshness is measured from the immutable authentication time
    // (`created_at`), preserved across rotations — refreshing a stale
    // session must never launder it back into a fresh one.
    let (token, _row) = active_session();
    let rotated_at = NOW + PRIVILEGE_REAUTH_WINDOW_SECS + 300;
    let rotated =
        session::rotate_session(&keys(), &token, rotated_at, 3600).expect("rotate stale session");
    let claims = session::verify_session(SECRET, &rotated, rotated_at).expect("rotated verifies");
    assert_eq!(claims.created_at, NOW, "rotation preserves auth time");
    assert_eq!(claims.issued_at, rotated_at);
    let row = record_for(&claims);
    assert_eq!(
        session::require_fresh_session(&keys(), &rotated, rotated_at + 1, &row),
        Err(SessionError::ReauthRequired),
        "issued 1s ago but authenticated outside the window: must re-auth"
    );
}

// Independent HMAC fixture signer (does not use the issuer), so the gate
// is proven to read `created_at` from the claims themselves.
fn signed_session(session_id: &str, created_at: u64, issued_at: u64, expires_at: u64) -> String {
    let payload = serde_json::json!({
        "session_id": session_id,
        "user_id": USER,
        "created_at": created_at,
        "issued_at": issued_at,
        "expires_at": expires_at,
        "rotation_counter": 3
    });
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).unwrap());
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
    mac.update(encoded.as_bytes());
    format!(
        "{encoded}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    )
}

#[test]
fn freshness_reads_authentication_time_not_issuance_time() {
    // Authenticated at NOW, most recently issued (rotated) seconds ago,
    // checked one second past the window from authentication.
    let at = NOW + PRIVILEGE_REAUTH_WINDOW_SECS + 1;
    let token = signed_session("fixture-session", NOW, at - 5, NOW + 3600);
    let row = SessionRecord {
        id: "fixture-session".into(),
        user_id: USER.into(),
        created_at: NOW,
        expires_at: NOW + 3600,
        rotation_counter: 3,
        revoked: false,
    };
    assert_eq!(
        session::require_fresh_session(&keys(), &token, at, &row),
        Err(SessionError::ReauthRequired)
    );
    // A fixture authenticated and issued at NOW passes inside the window.
    let fresh = signed_session("fixture-session", NOW, NOW, NOW + 3600);
    assert!(session::require_fresh_session(&keys(), &fresh, NOW + 60, &row).is_ok());
}

#[test]
fn invalid_sessions_never_receive_the_reauth_code() {
    let (token, row) = active_session();
    let stale_at = NOW + PRIVILEGE_REAUTH_WINDOW_SECS + 1;
    // Tampered token.
    let mut tampered = token.clone();
    tampered.push('A');
    assert_eq!(
        session::require_fresh_session(&keys(), &tampered, stale_at, &row),
        Err(SessionError::Invalid)
    );
    // Revoked row.
    let revoked = SessionRecord {
        revoked: true,
        ..row.clone()
    };
    assert_eq!(
        session::require_fresh_session(&keys(), &token, stale_at, &revoked),
        Err(SessionError::Invalid)
    );
    // Row for a different session.
    let foreign = SessionRecord {
        id: "other-session".into(),
        ..row.clone()
    };
    assert_eq!(
        session::require_fresh_session(&keys(), &token, stale_at, &foreign),
        Err(SessionError::Invalid)
    );
    // Expired session (past its own TTL): dead, not stale.
    assert_eq!(
        session::require_fresh_session(&keys(), &token, NOW + 3600, &row),
        Err(SessionError::Invalid)
    );
    // Short/weak secret fails closed as Invalid, like every other entry.
    assert_eq!(
        session::require_fresh_session(&SessionKeys::single(b"short"), &token, NOW, &row),
        Err(SessionError::Invalid)
    );
}

#[test]
fn reauth_mints_new_session_id_and_resets_freshness() {
    let (old_token, old_row) = active_session();
    let reauth_at = NOW + 1200; // stale for privilege ops, session still valid
    assert_eq!(
        session::require_fresh_session(&keys(), &old_token, reauth_at, &old_row),
        Err(SessionError::ReauthRequired),
        "precondition: the old session is stale"
    );
    let reauthed = session::reauthenticate_session(&keys(), &old_token, reauth_at, &old_row, 3600)
        .expect("re-authentication of a valid session mints a replacement");

    // Exactly the old session — and only it — is slated for revocation.
    assert_eq!(reauthed.revoked_session_id, old_row.id);

    let claims =
        session::verify_session(SECRET, &reauthed.token, reauth_at).expect("new token verifies");
    assert_ne!(claims.session_id, old_row.id, "a NEW session id is minted");
    assert_eq!(claims.user_id, USER);
    assert_eq!(claims.created_at, reauth_at, "freshness resets at re-auth");
    assert_eq!(claims.issued_at, reauth_at);
    assert_eq!(claims.rotation_counter, 0);
    assert_eq!(claims.expires_at, reauth_at + 3600);

    // The row to insert matches the new token exactly and starts live.
    assert_eq!(reauthed.new_record.id, claims.session_id);
    assert_eq!(reauthed.new_record.user_id, USER);
    assert_eq!(reauthed.new_record.created_at, reauth_at);
    assert_eq!(reauthed.new_record.expires_at, claims.expires_at);
    assert_eq!(reauthed.new_record.rotation_counter, 0);
    assert!(!reauthed.new_record.revoked);

    // The replacement passes the privilege gate now and at the boundary,
    // and goes stale on the same schedule as any other session.
    assert!(session::require_fresh_session(
        &keys(),
        &reauthed.token,
        reauth_at,
        &reauthed.new_record
    )
    .is_ok());
    assert!(session::require_fresh_session(
        &keys(),
        &reauthed.token,
        reauth_at + PRIVILEGE_REAUTH_WINDOW_SECS,
        &reauthed.new_record
    )
    .is_ok());
    assert_eq!(
        session::require_fresh_session(
            &keys(),
            &reauthed.token,
            reauth_at + PRIVILEGE_REAUTH_WINDOW_SECS + 1,
            &reauthed.new_record
        ),
        Err(SessionError::ReauthRequired)
    );
}

#[test]
fn reauth_revokes_only_the_old_session_not_the_users_others() {
    let (old_token, old_row) = active_session();
    // A second, independent session for the same user (another device).
    let (sibling_token, sibling_row) = active_session();
    assert_ne!(sibling_row.id, old_row.id);

    let reauth_at = NOW + 1200;
    let reauthed = session::reauthenticate_session(&keys(), &old_token, reauth_at, &old_row, 3600)
        .expect("re-auth");
    assert_ne!(
        reauthed.revoked_session_id, sibling_row.id,
        "single-session revoke: the sibling is never slated"
    );

    // Caller applies the contract: revoke the old row, insert the new.
    let revoked_old = SessionRecord {
        revoked: true,
        ..old_row.clone()
    };
    assert_eq!(
        session::verify_stored_session(&keys(), &old_token, reauth_at, &revoked_old),
        Err(SessionError::Invalid),
        "the old id is dead after re-auth"
    );
    assert_eq!(
        session::require_fresh_session(&keys(), &old_token, reauth_at, &revoked_old),
        Err(SessionError::Invalid),
        "a revoked old id is Invalid at the gate, not re-auth-able"
    );
    // The sibling session is untouched (not a logout-everywhere).
    assert!(
        session::verify_stored_session(&keys(), &sibling_token, reauth_at, &sibling_row).is_ok(),
        "the user's other session must survive a privilege re-auth"
    );
    // And the replacement session is live.
    assert!(session::verify_stored_session(
        &keys(),
        &reauthed.token,
        reauth_at,
        &reauthed.new_record
    )
    .is_ok());
}

#[test]
fn reauth_rejects_invalid_revoked_or_expired_old_sessions() {
    let (token, row) = active_session();
    // Revoked old row: nothing may be minted from a dead session.
    let revoked = SessionRecord {
        revoked: true,
        ..row.clone()
    };
    assert_eq!(
        session::reauthenticate_session(&keys(), &token, NOW + 60, &revoked, 3600).unwrap_err(),
        SessionError::Invalid
    );
    // Tampered old token.
    let mut tampered = token.clone();
    tampered.push('A');
    assert_eq!(
        session::reauthenticate_session(&keys(), &tampered, NOW + 60, &row, 3600).unwrap_err(),
        SessionError::Invalid
    );
    // Row belonging to another user (fixation / row-swap attempt).
    let foreign = SessionRecord {
        user_id: "other-user".into(),
        ..row.clone()
    };
    assert_eq!(
        session::reauthenticate_session(&keys(), &token, NOW + 60, &foreign, 3600).unwrap_err(),
        SessionError::Invalid
    );
    // Expired old session: the user must log in again, not re-auth in place.
    assert_eq!(
        session::reauthenticate_session(&keys(), &token, NOW + 3600, &row, 3600).unwrap_err(),
        SessionError::Invalid
    );
    // Short/weak secret fails closed.
    assert_eq!(
        session::reauthenticate_session(
            &SessionKeys::single(b"short"),
            &token,
            NOW + 60,
            &row,
            3600
        )
        .unwrap_err(),
        SessionError::Invalid
    );
}

#[test]
fn reauth_mints_under_primary_secret_in_rotation_window() {
    // Mirror the refresh rule: a pre-cutover token may re-authenticate
    // during a secret-rotation window, but the replacement is always
    // signed with the primary secret.
    let primary: &[u8] = b"primary-secret-00000000000000000000";
    let previous: &[u8] = b"previous-secret-0000000000000000000";
    let window = SessionKeys::rotation(primary, previous);
    let old_token = session::issue_session(previous, USER, NOW, 3600).expect("issue");
    let old_claims = session::verify_session_dual(&window, &old_token, NOW).expect("dual verifies");
    let old_row = record_for(&old_claims);

    let reauthed = session::reauthenticate_session(&window, &old_token, NOW + 1200, &old_row, 3600)
        .expect("re-auth in rotation window");
    assert!(
        session::verify_session(primary, &reauthed.token, NOW + 1200).is_ok(),
        "replacement must verify under the primary alone"
    );
    assert!(
        session::verify_session(previous, &reauthed.token, NOW + 1200).is_err(),
        "replacement must never be minted under the previous secret"
    );
    assert_ne!(reauthed.new_record.id, old_row.id);
    assert_eq!(reauthed.revoked_session_id, old_row.id);
}

#[test]
fn readme_documents_freshness_window_and_reauth_code() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("workers/api/README.md must exist");
    let lower = readme.to_ascii_lowercase();
    assert!(
        lower.contains("freshness"),
        "README must describe the privilege freshness window"
    );
    assert!(
        readme.contains(&PRIVILEGE_REAUTH_WINDOW_SECS.to_string()),
        "README must state the window value ({PRIVILEGE_REAUTH_WINDOW_SECS} seconds)"
    );
    assert!(
        readme.contains(REAUTH_REQUIRED_CODE),
        "README must document the {REAUTH_REQUIRED_CODE} error code"
    );
}
