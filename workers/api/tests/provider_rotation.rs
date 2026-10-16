//! S7 RED: rotation writes the new secret without dropping the user
//! session; the old credential stops working; the full stored row
//! (provider fields + indexer secret) survives rotation with only the
//! password replaced; identity is bound to the decrypted stored row;
//! no credential material in error bodies.
//!
//! `rotate_provider_credentials` performs shape + SSRF validation only —
//! it does not dial. Verifying the new secret with a real test-connection
//! (rate-limit + budget gates) is the API entry's job before it calls rotate.

use nntune_api::{session, vault};
use std::net::IpAddr;

struct FakeDns;
impl vault::DnsResolver for FakeDns {
    fn resolve(&self, host: &str) -> Vec<IpAddr> {
        match host {
            // Both hosts resolve publicly so a host-mismatch rejection can
            // only come from the identity guard, never from DNS validation.
            "news.example.com" | "other.example.com" => {
                vec!["93.184.216.34".parse().unwrap()]
            }
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

/// Full stored row: provider object + indexer secret, as `encrypt_vault`
/// documents ("serialized provider+indexer secrets").
fn full_row_plaintext(password: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "provider": {
            "host": "news.example.com",
            "port": 563,
            "tls": true,
            "username": "alice",
            "password": password,
            "max_connections": 4,
        },
        "indexer": {
            "url": "https://indexer.example.com/api",
            "api_key": "indexer-s3cret-key",
        },
    }))
    .unwrap()
}

fn seal(plaintext: &[u8]) -> vault::VaultBlob {
    vault::encrypt_vault(&test_kek(), 1, "user-1", plaintext).expect("encrypt old")
}

#[test]
fn rotation_keeps_session_swaps_credential_and_preserves_full_row() {
    let kek = test_kek();
    let user_id = "user-1";
    let session_secret = b"test-session-secret-that-is-32-bytes!!".to_vec();
    let now = 1_700_000_000u64;

    // Live session before rotation.
    let token = session::issue_session(&session_secret, user_id, now, 3600).expect("issue");

    // Old credential sealed in the vault as a full provider+indexer row.
    let old_blob = seal(&full_row_plaintext("old-s3cret"));

    let new_config = provider_with_password("new-s3cret-rotated");
    let rotated_blob =
        vault::rotate_provider_credentials(&kek, 1, user_id, &old_blob, &new_config, &FakeDns)
            .expect("rotate");
    let back: serde_json::Value = serde_json::from_slice(
        &vault::decrypt_vault(&kek, user_id, &rotated_blob).expect("decrypt"),
    )
    .unwrap();
    // Only the password changed: provider identity/fields and the indexer
    // secret are preserved verbatim.
    assert_eq!(back["provider"]["password"], "new-s3cret-rotated");
    assert_eq!(back["provider"]["host"], "news.example.com");
    assert_eq!(back["provider"]["username"], "alice");
    assert_eq!(back["provider"]["port"], 563);
    assert_eq!(back["provider"]["max_connections"], 4);
    assert_eq!(
        back["indexer"]["api_key"], "indexer-s3cret-key",
        "rotation must not drop the indexer secret"
    );
    assert_eq!(back["indexer"]["url"], "https://indexer.example.com/api");
    assert!(
        !vault::decrypt_vault(&kek, user_id, &rotated_blob)
            .expect("decrypt")
            .windows("old-s3cret".len())
            .any(|w| w == b"old-s3cret"),
        "old credential must stop working after rotation"
    );

    // Shape + SSRF validation passes for the new config (dial verification
    // is the caller's test-connection step, not rotate's).
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
fn rotation_rejects_when_old_row_cannot_be_decrypted() {
    // Deleting the decrypt (ownership proof) must fail this test: a blob
    // sealed for another user cannot be rotated by user-1.
    let foreign_blob =
        vault::encrypt_vault(&test_kek(), 1, "user-2", &full_row_plaintext("old-s3cret"))
            .expect("encrypt foreign");
    let new_config = provider_with_password("new-s3cret-rotated");
    assert!(
        vault::rotate_provider_credentials(
            &test_kek(),
            1,
            "user-1",
            &foreign_blob,
            &new_config,
            &FakeDns,
        )
        .is_err(),
        "rotation must fail closed when the stored row cannot be decrypted"
    );
}

#[test]
fn rotation_rejects_host_mismatch_against_stored_row() {
    // Identity comes from the decrypted row (news.example.com); the new
    // config names a different, still-resolvable host.
    let old_blob = seal(&full_row_plaintext("old-s3cret"));
    let mut new_config = provider_with_password("new-s3cret-rotated");
    new_config.host = "other.example.com".into();
    assert!(
        new_config.validate(&FakeDns).is_ok(),
        "mismatched host is itself valid — only the identity guard may reject"
    );
    assert!(
        vault::rotate_provider_credentials(
            &test_kek(),
            1,
            "user-1",
            &old_blob,
            &new_config,
            &FakeDns
        )
        .is_err(),
        "rotation must reject a host change (that is a fresh save)"
    );
}

#[test]
fn rotation_rejects_username_mismatch_against_stored_row() {
    let old_blob = seal(&full_row_plaintext("old-s3cret"));
    let mut new_config = provider_with_password("new-s3cret-rotated");
    new_config.username = "mallory".into();
    assert!(
        vault::rotate_provider_credentials(
            &test_kek(),
            1,
            "user-1",
            &old_blob,
            &new_config,
            &FakeDns
        )
        .is_err(),
        "rotation must reject a username change (that is a fresh save)"
    );
}

#[test]
fn rotation_rejects_stored_row_without_bindable_identity() {
    // A payload with no provider host/username cannot be identity-bound:
    // fail closed instead of sealing a caller-asserted identity.
    let old_blob = seal(br#"{"password":"old-s3cret"}"#);
    let new_config = provider_with_password("new-s3cret-rotated");
    assert!(vault::rotate_provider_credentials(
        &test_kek(),
        1,
        "user-1",
        &old_blob,
        &new_config,
        &FakeDns
    )
    .is_err());
    // A non-JSON payload likewise fails closed rather than being replaced
    // by a partial {"username","password"} row.
    let opaque_blob = seal(b"not-json");
    assert!(vault::rotate_provider_credentials(
        &test_kek(),
        1,
        "user-1",
        &opaque_blob,
        &new_config,
        &FakeDns,
    )
    .is_err());
}

#[test]
fn rotation_preserves_flat_row_extra_fields() {
    // Legacy flat rows: provider fields at top level alongside other keys.
    let flat = serde_json::to_vec(&serde_json::json!({
        "host": "news.example.com",
        "username": "alice",
        "password": "old-s3cret",
        "indexer_api_key": "flat-indexer-s3cret",
    }))
    .unwrap();
    let old_blob = seal(&flat);
    let new_config = provider_with_password("new-s3cret-rotated");
    let rotated = vault::rotate_provider_credentials(
        &test_kek(),
        1,
        "user-1",
        &old_blob,
        &new_config,
        &FakeDns,
    )
    .expect("rotate flat row");
    let back: serde_json::Value =
        serde_json::from_slice(&vault::decrypt_vault(&test_kek(), "user-1", &rotated).unwrap())
            .unwrap();
    assert_eq!(back["password"], "new-s3cret-rotated");
    assert_eq!(
        back["indexer_api_key"], "flat-indexer-s3cret",
        "flat-row extra fields must survive rotation"
    );
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
