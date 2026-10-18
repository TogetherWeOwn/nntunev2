//! Tests-only pin: session future-issued-at skew rejection.
//!
//! Implementation stays untouched. These tests pin the current behavior of
//! `session::verify_session` against the `CLOCK_SKEW_SECS` (60s) future
//! `issued_at` tolerance:
//!
//! 1. `issued_at = NOW + 61` is rejected with `Err(SessionError::Invalid)`.
//! 2. A far-future `issued_at` is rejected with `Err(Invalid)` and no panic:
//!    the skew-isolating case satisfies every earlier bound so only the skew
//!    check can reject it, while the extreme `u64::MAX` case fails closed via
//!    the independent `expires_at` bound.
//! 3. Boundary control: `issued_at = NOW + 60` verifies.
//! 4. Near-`u64::MAX` verification succeeds, proving clock arithmetic is
//!    saturating (a plain `+` would panic on overflow).
//!
//! Fixtures are signed by an independent HMAC-SHA256 signer defined here —
//! never by `issue_session`, which cannot mint future-issued tokens — so the
//! suite does not tautologically re-exercise the issuer. Fixtures use
//! synthetic credentials only (public repo: never real secrets).

use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use nntune_api::session;
use sha2::Sha256;

const NOW: u64 = 1_700_000_000;
const SECRET: &[u8] = b"synthetic-future-skew-pin-secret-32b!!";

fn base64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Independently signed session token. Intentionally bypasses `issue_session`
/// so the fixture can carry a future `issued_at` the issuer never mints.
fn signed_session(created_at: u64, issued_at: u64, expires_at: u64) -> String {
    let payload = serde_json::json!({
        "session_id": "future-skew-fixture",
        "user_id": "user-skew-pin",
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
fn future_issued_beyond_skew_is_rejected() {
    // One second past the 60s tolerance: every other bound is satisfied, so
    // only the future-issued-at skew check can reject this token.
    let token = signed_session(NOW, NOW + 61, NOW + 3600);
    assert_eq!(
        session::verify_session(SECRET, &token, NOW),
        Err(session::SessionError::Invalid),
        "issued_at NOW+61 must be rejected past the 60s skew window"
    );
}

#[test]
fn far_future_issued_is_rejected_without_panic() {
    // Isolating case: created_at == issued_at == far with an expires_at inside
    // the session lifetime satisfies every earlier bound, so only the
    // future-issued-at skew check can reject this token. Deleting the skew
    // check turns this assertion into Ok.
    let far = NOW + 1_000_000;
    let token = signed_session(far, far, far + 3600);
    assert_eq!(
        session::verify_session(SECRET, &token, NOW),
        Err(session::SessionError::Invalid),
        "far-future issued_at {far} must be rejected by the skew check"
    );

    // Extreme value: u64::MAX cannot satisfy `expires_at > issued_at`, so it
    // is rejected by that independent bound — still fail-closed with the
    // generic error and no panic.
    let token = signed_session(NOW, u64::MAX, NOW + 3600);
    assert_eq!(
        session::verify_session(SECRET, &token, NOW),
        Err(session::SessionError::Invalid),
        "u64::MAX issued_at must be rejected without panic"
    );
}

#[test]
fn near_max_clock_arithmetic_does_not_panic() {
    // With `now` near u64::MAX, `now + CLOCK_SKEW_SECS` would overflow a plain
    // `+` (panic in debug). The implementation uses saturating_add, so this
    // in-window token verifies instead of panicking.
    let now = u64::MAX - 10;
    let token = signed_session(u64::MAX - 100, now, u64::MAX - 5);
    let claims = session::verify_session(SECRET, &token, now)
        .expect("near-max in-window token must verify without overflow panic");
    assert_eq!(claims.issued_at, now);
}

#[test]
fn skew_boundary_control_verifies() {
    // Exactly at the tolerance edge: still accepted, proving the rejection
    // above is the 60s boundary and not a blanket future-issued rejection.
    let token = signed_session(NOW, NOW + 60, NOW + 3600);
    let claims = session::verify_session(SECRET, &token, NOW)
        .expect("issued_at NOW+60 is within the skew window");
    assert_eq!(claims.issued_at, NOW + 60);
    assert_eq!(claims.created_at, NOW);
    assert_eq!(claims.user_id, "user-skew-pin");
}
