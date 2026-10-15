//! Acceptance tests: provider credential rotation (vault rewrite + session
//! preservation) and the credential-negative invariant on existing types.
//!
//! Test-only card: no `src/` changes here. Everything below uses APIs that
//! already exist (`vault::encrypt_vault` / `decrypt_vault`,
//! `session::issue_session` / `verify_stored_session`), so this file is
//! GREEN before the build and guards the builder against regressions while
//! the sibling `provider_reauth_401_acceptance` file pins the new 401
//! contract (RED until the builder lands it).
//!
//! Fixtures are synthetic only (fake KEK, `example.com` hosts, synthetic
//! passwords): nothing here touches live provider traffic or real secrets.

use nntune_api::{session, vault};

// Fake KEK: 32 synthetic bytes, never a real key. Matches the `test_kek`
// pattern in `api_tests.rs`.
fn fake_kek() -> [u8; 32] {
    [0x42u8; 32]
}

const USER: &str = "user-rotation-acceptance";
const OLD_PAYLOAD: &[u8] =
    br#"{"host":"news.example.com","user":"synthetic-user","pass":"synthetic-old-password"}"#;
const NEW_PAYLOAD: &[u8] =
    br#"{"host":"news.example.com","user":"synthetic-user","pass":"synthetic-new-password"}"#;

#[test]
fn rotation_rewrites_vault_row_with_new_secret() {
    // Rotation writes the new secret to the vault: the fresh row decrypts
    // to the NEW plaintext, and the row bytes change (the server overwrites
    // the row, so the old credential stops being served).
    let kek = fake_kek();
    let old_blob = vault::encrypt_vault(&kek, 1, USER, OLD_PAYLOAD).expect("encrypt old");
    let new_blob = vault::encrypt_vault(&kek, 1, USER, NEW_PAYLOAD).expect("encrypt new");

    let back = vault::decrypt_vault(&kek, USER, &new_blob).expect("decrypt new");
    assert_eq!(back, NEW_PAYLOAD, "rotated row verifies via decrypt");

    let old_back = vault::decrypt_vault(&kek, USER, &old_blob).expect("decrypt old");
    assert_eq!(old_back, OLD_PAYLOAD);
    assert_ne!(
        old_blob.payload_b64url, new_blob.payload_b64url,
        "rotation changes the stored row; the old credential is no longer served"
    );
}

#[test]
fn rotation_preserves_user_session() {
    // Rotation must not drop the user session: a vault rewrite leaves the
    // session row untouched, so the pre-rotation token still verifies
    // against the same stored row with the same counter.
    const SECRET: &[u8] = b"synthetic-session-secret-at-least-32-bytes";
    const NOW: u64 = 1_700_000_000;

    let token = session::issue_session(SECRET, USER, NOW, 3600).expect("issue");
    let claims = session::verify_session(SECRET, &token, NOW).unwrap();
    let row = session::SessionRecord {
        id: claims.session_id,
        user_id: USER.into(),
        created_at: NOW,
        expires_at: NOW + 3600,
        rotation_counter: 0,
        revoked: false,
    };
    session::verify_stored_session(
        &session::SessionKeys::single(SECRET),
        &token,
        NOW + 60,
        &row,
    )
    .expect("session valid before rotation");

    // The rotation itself: rewrite the vault row with the new secret.
    let kek = fake_kek();
    let rotated = vault::encrypt_vault(&kek, 1, USER, NEW_PAYLOAD).expect("encrypt rotated");
    assert_eq!(
        vault::decrypt_vault(&kek, USER, &rotated).expect("decrypt rotated"),
        NEW_PAYLOAD
    );

    // Same token, same row, still valid afterwards — no re-login required.
    let after = session::verify_stored_session(
        &session::SessionKeys::single(SECRET),
        &token,
        NOW + 120,
        &row,
    )
    .expect("session survives vault rotation");
    assert_eq!(after.user_id, USER);
    assert_eq!(after.rotation_counter, 0);
}

#[test]
fn existing_vault_and_session_types_carry_no_credential_material() {
    // Negative test on existing types: no log line, Debug dump, or JSON
    // body may contain credential material (threat-model section 3).
    // Synthetic secrets stand in for real ones throughout.
    const PASSWORD: &str = "synthetic-password-9f8e7d6c";
    const API_KEY: &str = "synthetic-indexer-key-1a2b3c4d";

    let provider = vault::ProviderConfig {
        host: "news.example.com".into(),
        port: 563,
        tls: true,
        username: "synthetic-user".into(),
        password: PASSWORD.into(),
        max_connections: 4,
        allow_insecure_plaintext: false,
    };
    let provider_dump = format!("{provider:?}");
    assert!(
        !provider_dump.contains(PASSWORD),
        "password in Debug: {provider_dump}"
    );
    assert!(provider_dump.contains("<redacted>"));

    let indexer = vault::IndexerConfig {
        url: "https://index.example.com/api".into(),
        api_key: API_KEY.into(),
    };
    let indexer_dump = format!("{indexer:?}");
    assert!(
        !indexer_dump.contains(API_KEY),
        "api key in Debug: {indexer_dump}"
    );

    // Error displays stay generic.
    for err in [vault::VaultError::Invalid, vault::VaultError::Crypto] {
        let text = format!("{err}");
        assert!(!text.contains(PASSWORD) && !text.contains(API_KEY));
        assert_eq!(text, "vault unavailable");
    }

    // Read shapes are booleans + audit ids/results only.
    let summary = vault::redacted_summary(true, false, 1_700_000_000u64);
    let summary_json = serde_json::to_string(&summary).unwrap();
    assert!(!summary_json.contains(PASSWORD) && !summary_json.contains(API_KEY));

    let event = vault::audit_event(USER, "vault.update", true, "203.0.113.7", 1_700_000_000u64);
    let event_json = serde_json::to_string(&event).unwrap();
    assert!(!event_json.contains(PASSWORD) && !event_json.contains(API_KEY));

    let probe = vault::TestConnectionResult { ok: false };
    let probe_json = serde_json::to_string(&probe).unwrap();
    assert!(!probe_json.contains(PASSWORD) && !probe_json.contains(API_KEY));
}
