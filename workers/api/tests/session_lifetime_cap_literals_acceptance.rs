//! Absolute session lifetime cap literal pins (tests-only).
//!
//! Contract under test (`workers/api/src/session.rs:20`, enforced at
//! `:239,267-268,365,470,506`):
//!
//! - `MAX_SESSION_SECS == 86_400` (one day). The name appears in
//!   `workers/api/tests/` only symbolically, so a silent value change would
//!   keep those suites green. This file pins the literal.
//! - A token minted at the cap validates through the session entry
//!   ([`session::verify_session`], plus the stored-row auth entry
//!   [`session::verify_stored_session`]), while a token carrying cap+1 fails
//!   closed with [`session::SessionError::Invalid`].
//! - Mint clamps: [`session::issue_session`] caps `ttl_secs` at the lifetime,
//!   so an over-cap request still mints a token that verifies with
//!   `expires_at == created_at + 86_400`.
//! - Cookie `Max-Age` clamps: [`session::set_cookie_header`] caps any
//!   caller-supplied value at the lifetime, so no cookie outlives the token.
//!
//! Sensitivity (verified by construction, production code untouched):
//!
//! - Lowering the const (e.g. to `3600`) turns the literal pin red, breaks
//!   the at-cap expiry assertion (mint clamps lower), and breaks the cookie
//!   clamp string.
//! - Raising the const (e.g. to `172_800`) turns the literal pin red, flips
//!   the cap+1 rejection green (the forged token now satisfies the wider
//!   bound), and breaks the cookie clamp string.
//! - Renaming or moving the const without updating this suite fails the
//!   source-text pin.
//! - Deleting the `expires_at > created_at + MAX` bound (or the mint/cookie
//!   `.min`) flips the cap+1 / clamp tests green-then-wrong: they fail.
//!
//! Out of scope: secret strength, future-iat skew, privilege freshness
//! windows, revocation — different consts, pinned elsewhere. Fixtures use
//! synthetic secrets only (public repo: never real secrets; no secret
//! material is logged or asserted on — only lengths and error variants).
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-api --test session_lifetime_cap_literals_acceptance
//! ```

use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use nntune_api::session::{self, SessionError, SessionKeys};
use sha2::Sha256;

const NOW: u64 = 1_700_000_000;
/// Pinned copy of the absolute lifetime: one day in seconds. Every boundary
/// below is spelled against this number, never against
/// `session::MAX_SESSION_SECS`, so the suite — not the symbol — owns the
/// value.
const PINNED_CAP_SECS: u64 = 86_400;

const SECRET: &[u8] = b"synthetic-lifetime-cap-pin-secret!";

fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Independently signed session token. Intentionally bypasses
/// `issue_session` — which clamps `ttl_secs` — so the fixture can carry an
/// `expires_at` past the cap that the issuer never mints.
fn signed_session(created_at: u64, issued_at: u64, expires_at: u64) -> String {
    let payload = serde_json::json!({
        "session_id": "lifetime-cap-fixture",
        "user_id": "user-lifetime-pin",
        "created_at": created_at,
        "issued_at": issued_at,
        "expires_at": expires_at,
        "rotation_counter": 0
    });
    let encoded = base64url(&serde_json::to_vec(&payload).unwrap());
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
    mac.update(encoded.as_bytes());
    format!("{encoded}.{}", base64url(&mac.finalize().into_bytes()))
}

#[test]
fn max_session_secs_is_exactly_86400() {
    assert_eq!(
        session::MAX_SESSION_SECS,
        PINNED_CAP_SECS,
        "absolute session lifetime must stay exactly 86_400s (one day)"
    );
    assert_eq!(
        session::MAX_SESSION_SECS,
        24 * 60 * 60,
        "the cap must stay expressible as one day"
    );
    assert!(
        include_str!("../src/session.rs").contains("pub const MAX_SESSION_SECS: u64 = 86_400;"),
        "session lifetime cap declaration must stay exactly \
         `pub const MAX_SESSION_SECS: u64 = 86_400;`"
    );
}

#[test]
fn token_minted_at_cap_validates_through_session_entry() {
    let keys = SessionKeys::single(SECRET);
    let token = session::issue_session(SECRET, "user-1", NOW, 86_400).expect("cap ttl issues");
    let claims = session::verify_session(SECRET, &token, NOW).expect("at-cap token verifies");
    assert_eq!(
        claims.expires_at,
        NOW + 86_400,
        "a token minted at the cap must expire exactly NOW + 86_400"
    );
    assert_eq!(claims.created_at, NOW);
    // The stored-row auth entry — the path request authentication actually
    // uses — accepts the at-cap token against its matching row.
    let record = session::SessionRecord {
        id: claims.session_id.clone(),
        user_id: claims.user_id.clone(),
        created_at: claims.created_at,
        expires_at: claims.expires_at,
        rotation_counter: claims.rotation_counter,
        revoked: false,
    };
    let stored = session::verify_stored_session(&keys, &token, NOW, &record)
        .expect("at-cap token passes the stored-row check");
    assert_eq!(stored, claims);
}

#[test]
fn token_past_cap_fails_closed_through_session_entry() {
    let keys = SessionKeys::single(SECRET);
    // Boundary control first: an independently signed at-cap token verifies,
    // proving the rejection below is the cap boundary and not a blanket
    // fixture rejection.
    let at_cap = signed_session(NOW, NOW, NOW + 86_400);
    let at_cap_record = session::SessionRecord {
        id: "lifetime-cap-fixture".to_string(),
        user_id: "user-lifetime-pin".to_string(),
        created_at: NOW,
        expires_at: NOW + 86_400,
        rotation_counter: 0,
        revoked: false,
    };
    assert!(
        session::verify_session(SECRET, &at_cap, NOW).is_ok(),
        "independently signed at-cap token must verify"
    );
    assert!(
        session::verify_stored_session(&keys, &at_cap, NOW, &at_cap_record).is_ok(),
        "at-cap token must pass the stored-row check"
    );

    // One second past the cap: every other bound is satisfied, so only the
    // `expires_at > created_at + MAX_SESSION_SECS` bound can reject it.
    // Deleting that bound flips this assertion to Ok.
    let past_cap = signed_session(NOW, NOW, NOW + 86_401);
    assert_eq!(
        session::verify_session(SECRET, &past_cap, NOW),
        Err(SessionError::Invalid),
        "a token living 86_401s past authentication must fail closed"
    );
    let past_cap_record = session::SessionRecord {
        id: "lifetime-cap-fixture".to_string(),
        user_id: "user-lifetime-pin".to_string(),
        created_at: NOW,
        expires_at: NOW + 86_401,
        rotation_counter: 0,
        revoked: false,
    };
    assert_eq!(
        session::verify_stored_session(&keys, &past_cap, NOW, &past_cap_record),
        Err(SessionError::Invalid),
        "a cap+1 token must fail the stored-row check too"
    );
}

#[test]
fn mint_clamps_over_cap_ttl_to_the_lifetime() {
    // `issue_session` caps `ttl_secs` at the lifetime: an over-cap request
    // mints a token that still verifies, expiring exactly at the cap.
    let token = session::issue_session(SECRET, "user-1", NOW, 86_400 * 3)
        .expect("over-cap ttl still issues");
    let claims = session::verify_session(SECRET, &token, NOW).expect("clamped token verifies");
    assert_eq!(
        claims.expires_at,
        NOW + 86_400,
        "over-cap mint must clamp expiry to NOW + 86_400"
    );
}

#[test]
fn cookie_max_age_clamps_to_the_cap() {
    let clamped = session::set_cookie_header("tok", u64::MAX);
    assert_eq!(
        clamped, "__Host-nntune-session=tok; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=86400",
        "over-cap Max-Age must clamp to exactly 86400"
    );
    assert_eq!(
        session::set_cookie_header("tok", 86_400 * 3),
        clamped,
        "3x-cap Max-Age must clamp to the same pinned header"
    );
    assert!(
        session::set_cookie_header("tok", 60).ends_with("Max-Age=60"),
        "under-cap Max-Age must pass through unchanged"
    );
}
