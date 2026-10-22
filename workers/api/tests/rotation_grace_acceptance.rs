//! Rotation grace-window drills at the core-function level.
//!
//! Synthetic walk of the rotation ceremony through the functions that run
//! it (`vault::rewrap_dek` for the re-wrap pass, `session::verify_session_dual`
//! on the request path): KEK reads stay green across the write flip, the
//! re-wrap migrates the DEK only, and session tokens minted under the
//! previous secret authenticate inside the window and die generically once
//! it ends. All keys are synthetic fills, never real secrets.
//!
//! Owned elsewhere (recur here only as drill controls):
//! - Re-wrap SQL contract, claim races and runbook text
//!   (`kek_rotation_acceptance.rs`).
//! - In-window session auth, refresh migration onto the primary and the
//!   HTTP envelopes (401 + generic bodies) — the HTTP acceptance fixtures
//!   (`pr30_findings_acceptance.rs`).
//! - Secret length floors and steady-state single-key paths
//!   (`session_secret_floor_acceptance.rs`, session unit tests).
//!
//! What this file does NOT prove: previous-secret removal safety (no
//! selector assertion — post-window death here is ordinary expiry), or
//! any HTTP/UI behavior.

use nntune_api::{session, vault};

const NOW: u64 = 1_700_000_000;
const DRILL_USER: &str = "rotation-drill-user";
const DRILL_PLAINTEXT: &[u8] = b"synthetic-credential-payload";

/// Synthetic KEK fills: distinct per version, obviously not real key material.
const KEK_V1: [u8; 32] = [0xA1; 32];
const KEK_V2: [u8; 32] = [0xB2; 32];
const KEK_WRONG: [u8; 32] = [0xC3; 32];

const PRIMARY_SECRET: &[u8] = b"synthetic-primary-secret-32-bytes!!";
const PREVIOUS_SECRET: &[u8] = b"synthetic-previous-secret-32-bytes!";

#[test]
fn kek_grace_window_old_reads_succeed_until_retire() {
    // Phase 0 (steady state v1): a row wrapped under KEK v1 decrypts with v1.
    let v1_blob =
        vault::encrypt_vault(&KEK_V1, 1, DRILL_USER, DRILL_PLAINTEXT).expect("encrypt under v1");
    assert_eq!(v1_blob.kek_version, 1);
    let recovered = vault::decrypt_vault(&KEK_V1, DRILL_USER, &v1_blob).expect("v1 reads v1");
    assert_eq!(recovered, DRILL_PLAINTEXT);

    // Phases 1-2 (pre-stage + flip writes to v2): the old row still decrypts
    // with v1 while new rows wrap under v2 — the downtime-free grace.
    let v2_blob =
        vault::encrypt_vault(&KEK_V2, 2, DRILL_USER, DRILL_PLAINTEXT).expect("encrypt under v2");
    assert_eq!(v2_blob.kek_version, 2);
    let post_flip_read =
        vault::decrypt_vault(&KEK_V2, DRILL_USER, &v2_blob).expect("v2 reads post-flip writes");
    assert_eq!(post_flip_read, DRILL_PLAINTEXT);
    let grace_read =
        vault::decrypt_vault(&KEK_V1, DRILL_USER, &v1_blob).expect("old KEK reads during grace");
    assert_eq!(grace_read, DRILL_PLAINTEXT);

    // Phase 3 (re-wrap pass): DEK-only migration. The payload ciphertext and
    // its nonce are untouched; only the wrap moves to v2.
    let rewrapped =
        vault::rewrap_dek(&KEK_V1, &KEK_V2, 2, DRILL_USER, &v1_blob).expect("re-wrap v1 -> v2");
    assert_eq!(rewrapped.kek_version, 2);
    assert_eq!(
        rewrapped.payload_b64url, v1_blob.payload_b64url,
        "re-wrap must not touch the payload ciphertext"
    );
    assert_eq!(
        rewrapped.payload_nonce_b64url, v1_blob.payload_nonce_b64url,
        "re-wrap must not touch the payload nonce"
    );
    let migrated =
        vault::decrypt_vault(&KEK_V2, DRILL_USER, &rewrapped).expect("v2 reads rewrapped row");
    assert_eq!(migrated, DRILL_PLAINTEXT);

    // Keep the KEK unchanged to isolate the stored version's AAD binding.
    let mut skewed = v1_blob.clone();
    skewed.kek_version = 2;

    // Version-pinned decrypt is strict both ways, and every failure carries
    // the identical generic message — the same text the HTTP envelope
    // carries — so a wrong version (or a wholly wrong key) is no oracle for
    // an attacker and no distinguishing detail for operators either.
    for err in [
        vault::decrypt_vault(&KEK_V1, DRILL_USER, &skewed)
            .expect_err("same-key version-only tampering must fail"),
        vault::decrypt_vault(&KEK_V2, DRILL_USER, &v1_blob).unwrap_err(),
        vault::decrypt_vault(&KEK_V1, DRILL_USER, &rewrapped).unwrap_err(),
        vault::decrypt_vault(&KEK_WRONG, DRILL_USER, &v1_blob).unwrap_err(),
        vault::decrypt_vault(&KEK_WRONG, DRILL_USER, &rewrapped).unwrap_err(),
    ] {
        assert_eq!(
            err.to_string(),
            "vault unavailable",
            "wrong-version reads must fail generically"
        );
    }
}

#[test]
fn session_rotation_window_end_old_tokens_already_dead() {
    // Cutover at NOW: pre-cutover tokens verify through the dual path
    // (grace), exactly as the ceremony requires.
    let old_token =
        session::issue_session(PREVIOUS_SECRET, DRILL_USER, NOW, 3600).expect("issue pre-cutover");
    let window = session::SessionKeys::rotation(PRIMARY_SECRET, PREVIOUS_SECRET);
    let claims = session::verify_session_dual(&window, &old_token, NOW + 60)
        .expect("pre-cutover cookie authenticates during the window");
    let row = session::SessionRecord {
        id: claims.session_id.clone(),
        user_id: claims.user_id.clone(),
        created_at: claims.created_at,
        expires_at: claims.expires_at,
        rotation_counter: claims.rotation_counter,
        revoked: false,
    };
    session::verify_stored_session(&window, &old_token, NOW + 60, &row)
        .expect("row check passes during the window");
    assert_eq!(claims.rotation_counter, 0);

    // Previous-key dependence, not expiry: nothing is expired at NOW + 60,
    // yet the pre-cutover token is rejected without the previous secret ...
    let single = session::SessionKeys::single(PRIMARY_SECRET);
    let rejection = session::verify_session_dual(&single, &old_token, NOW + 60).unwrap_err();
    assert_eq!(
        rejection.to_string(),
        "invalid session",
        "pre-cutover token must depend on the previous key"
    );

    // ... while a token minted under the primary verifies through the same
    // single-primary path at the same instant, so the rejection above is key
    // dependence, not a broken verifier.
    let new_token =
        session::issue_session(PRIMARY_SECRET, DRILL_USER, NOW, 3600).expect("issue under primary");
    session::verify_session_dual(&single, &new_token, NOW + 60)
        .expect("primary-signed token verifies while the window is open");

    // After window_end (created_at + MAX_SESSION_SECS + skew) the old token
    // is dead under BOTH secrets, and the same-age primary-signed token
    // dies with it. The TTL (3600s) expires first, so this death is ordinary
    // expiry, not previous-key removal. Every failure is the generic
    // `invalid session`; the HTTP layer maps that to a 401 with the generic
    // auth envelope — never a 500, never a reason oracle. HTTP behavior
    // itself is pinned by the HTTP fixtures, not here.
    let after_window = NOW + session::MAX_SESSION_SECS + session::CLOCK_SKEW_SECS + 1;
    for err in [
        session::verify_session_dual(&window, &old_token, after_window).unwrap_err(),
        session::verify_stored_session(&window, &old_token, after_window, &row).unwrap_err(),
        session::verify_stored_session(&single, &old_token, after_window, &row).unwrap_err(),
        session::verify_session_dual(&single, &new_token, after_window).unwrap_err(),
    ] {
        assert_eq!(
            err.to_string(),
            "invalid session",
            "post-window failures must stay generic"
        );
    }
}
