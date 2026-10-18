//! Tests-only pin: session future-issued-at skew rejection.
//!
//! Implementation stays untouched. These tests pin the current behavior of
//! `session::verify_session` against the `CLOCK_SKEW_SECS` (60s) future
//! `issued_at` tolerance:
//!
//! 1. `issued_at = NOW + 61` is rejected with `Err(SessionError::Invalid)`.
//! 2. A far-future `issued_at` is rejected with `Err(Invalid)` and no panic
//!    (all clock arithmetic is saturating).
//! 3. Boundary control: `issued_at = NOW + 60` verifies.
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
    // Far-future issuance must fail closed with the generic error, never
    // panic on clock arithmetic (all bounds use saturating_add).
    for issued_at in [NOW + 1_000_000, u64::MAX] {
        let token = signed_session(NOW, issued_at, NOW + 3600);
        assert_eq!(
            session::verify_session(SECRET, &token, NOW),
            Err(session::SessionError::Invalid),
            "far-future issued_at {issued_at} must be rejected without panic"
        );
    }
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
