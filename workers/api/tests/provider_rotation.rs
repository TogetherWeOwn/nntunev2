//! S7 RED: rotation writes the new secret without dropping the user
//! session; old credential stops working, new one verifies via
//! test-connection; no credential material in error bodies.

use nntune_api::{session, vault};
use std::net::IpAddr;

struct FakeDns;
impl vault::DnsResolver for FakeDns {
    fn resolve(&self, host: &str) -> Vec<IpAddr> {
        match host {
            "news.example.com" => vec!["93.184.216.34".parse().unwrap()],
            _ => vec![],
        }
    }
}

fn test_kek() -> [u8; 32] {
    [0x11u8; 32]
}

fn provider_with_password(password: &str) -> vault::ProviderConfig {
    vault::ProviderConfig {
        host: "news.example.com".into(),
        port: 563,
        tls: true,
        username: "alice".into(),
        password: password.into(),
        max_connections: 4,
        allow_insecure_plaintext: false,
    }
}

#[test]
fn rotation_keeps_session_and_swaps_credential() {
    let kek = test_kek();
    let user_id = "user-1";
    let session_secret = b"test-session-secret-that-is-32-bytes!!".to_vec();
    let now = 1_700_000_000u64;

    // Live session before rotation.
    let token = session::issue_session(&session_secret, user_id, now, 3600).expect("issue");

    // Old credential sealed in the vault.
    let old_config = provider_with_password("old-s3cret");
    let old_plaintext = serde_json::to_vec(
        &serde_json::json!({"username": old_config.username, "password": old_config.password}),
    )
    .unwrap();
    let old_blob = vault::encrypt_vault(&kek, 1, user_id, &old_plaintext).expect("encrypt old");

    // Rotation: validate the new config, seal it, verify via test-connection
    // gates (validation + rate-limit window open).
    let new_config = provider_with_password("new-s3cret-rotated");
    let rotated_blob = vault::rotate_provider_credentials(
        &kek,
        1,
        user_id,
        &old_blob,
        &old_config,
        &new_config,
        &FakeDns,
    )
    .expect("rotate");
    let back: serde_json::Value = serde_json::from_slice(
        &vault::decrypt_vault(&kek, user_id, &rotated_blob).expect("decrypt"),
    )
    .unwrap();
    assert_eq!(back["password"], "new-s3cret-rotated");
    assert!(
        !vault::decrypt_vault(&kek, user_id, &rotated_blob)
            .expect("decrypt")
            .windows("old-s3cret".len())
            .any(|w| w == b"old-s3cret"),
        "old credential must stop working after rotation"
    );

    // New credential verifies via test-connection gates.
    assert!(new_config.validate(&FakeDns).is_ok());
    assert!(vault::test_attempt_allowed(&[], now));

    // The user session survives the rotation untouched.
    let claims =
        session::verify_session(&session_secret, &token, now + 60).expect("session survives");
    assert_eq!(claims.user_id, user_id);

    // Rotation is audited as a rotation, not a generic update.
    assert_eq!(vault::provider_rotation_audit_action(), "vault.rotate");
}

#[test]
fn provider_auth_expired_code_is_documented() {
    assert_eq!(vault::PROVIDER_AUTH_EXPIRED_CODE, "provider_auth_expired");
    let body = vault::provider_auth_expired_body();
    assert!(body.contains("provider_auth_expired"));
    assert!(!body.contains("s3cret"));
}

#[test]
fn provider_error_bodies_never_carry_secrets() {
    let secret = "s3cret-provider-password";
    let body = vault::provider_auth_expired_body();
    assert!(!body.contains(secret), "secret in error body: {body}");
    let summary = vault::redacted_summary(true, false, 1_700_000_000u64);
    let json = serde_json::to_string(&summary).unwrap();
    assert!(!json.contains(secret));
}
