//! ADR 0014 pool provider registry foundation: settings validation and
//! the `pool_provider_secrets` envelope (roundtrip, scope isolation from
//! legacy `providers` envelopes, version pin, rewrap).

use nntune_api::pool_providers::{
    decrypt_pool_secret, encrypt_pool_secret, rewrap_pool_secret, PoolProviderSecret,
    PoolProviderSettings,
};
use nntune_api::vault::VaultError;
use rand::RngCore;

const REF_A: &str = "11111111-1111-4111-8111-111111111111";
const REF_B: &str = "22222222-2222-4222-8222-222222222222";

fn key() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    k
}

fn secret(pw: &str) -> PoolProviderSecret {
    PoolProviderSecret {
        password: pw.to_string(),
    }
}

fn good() -> PoolProviderSettings {
    PoolProviderSettings {
        id: REF_A.into(),
        label: "Primary".into(),
        host: "news.example.com".into(),
        port: 563,
        tls: true,
        username: "svc-user".into(),
        can_read: true,
        can_post: false,
        max_connections: 20,
        stream_reserve: 5,
        ingest_limit: 15,
        backbone: "omicron".into(),
        tier: 1,
        enabled: true,
    }
}

fn rejects(f: impl FnOnce(&mut PoolProviderSettings)) {
    let mut s = good();
    f(&mut s);
    assert!(s.validate().is_err());
}

#[test]
fn good_settings_validate_and_roundtrip_serde() {
    assert!(good().validate().is_ok());
    let json = serde_json::to_string(&good()).unwrap();
    let back: PoolProviderSettings = serde_json::from_str(&json).unwrap();
    assert!(back == good());
    // Unknown fields are refused.
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v["extra"] = serde_json::json!(1);
    assert!(serde_json::from_value::<PoolProviderSettings>(v).is_err());
}

#[test]
fn validation_rejects_bad_shapes() {
    rejects(|s| s.id = "not-a-uuid".into());
    rejects(|s| s.id = "abcdef12-1234-4234-8234-123456789abc".to_uppercase());
    rejects(|s| s.label = "".into());
    rejects(|s| s.label = "a".repeat(65));
    rejects(|s| s.label = "bad\nlabel".into());
    rejects(|s| s.host = "https://news.example.com".into());
    rejects(|s| s.host = "news.example.com:563".into());
    rejects(|s| s.host = "a b".into());
    rejects(|s| s.host = "".into());
    rejects(|s| s.port = 8080);
    rejects(|s| s.port = 0);
    rejects(|s| s.username = "".into());
    rejects(|s| s.username = "u\0".into());
    rejects(|s| s.username = "u".repeat(256));
    rejects(|s| s.backbone = "".into());
    rejects(|s| s.backbone = "has space".into());
    rejects(|s| s.backbone = "b".repeat(65));
    rejects(|s| s.tier = 101);
}

#[test]
fn tls_is_required_and_port_119_still_needs_tls() {
    rejects(|s| s.tls = false);
    rejects(|s| {
        s.port = 119;
        s.tls = false;
    });
    let mut s = good();
    s.port = 119;
    assert!(s.validate().is_ok());
}

#[test]
fn connection_limits_are_enforced() {
    rejects(|s| s.max_connections = 0);
    rejects(|s| s.max_connections = 51);
    let mut s = good();
    s.max_connections = 50;
    s.stream_reserve = 10;
    s.ingest_limit = 40;
    assert!(s.validate().is_ok());
    rejects(|s| s.stream_reserve = 0);
    rejects(|s| s.stream_reserve = 21);
    rejects(|s| s.ingest_limit = 16);
}

#[test]
fn non_reading_accounts_cannot_ingest() {
    let mut s = good();
    s.can_read = false;
    s.can_post = true;
    s.stream_reserve = 0;
    s.ingest_limit = 0;
    assert!(s.validate().is_ok());
    s.ingest_limit = 1;
    assert!(s.validate().is_err());
}

#[test]
fn secret_roundtrips() {
    let (k, r) = (key(), REF_A);
    let env = encrypt_pool_secret(&k, 3, r, &secret("hunter2")).unwrap();
    let out = decrypt_pool_secret(&k, 3, r, &env).unwrap();
    assert_eq!(out.password, "hunter2");
    // Fresh DEK/nonces per call.
    let env2 = encrypt_pool_secret(&k, 3, r, &secret("hunter2")).unwrap();
    assert_ne!(env.ciphertext, env2.ciphertext);
    assert_ne!(env.payload_nonce, env2.payload_nonce);
}

#[test]
fn invalid_inputs_are_rejected_before_crypto() {
    let k = key();
    assert!(encrypt_pool_secret(&k, 1, "bad", &secret("x")).is_err());
    assert!(encrypt_pool_secret(&k, 1, REF_A, &secret("")).is_err());
    assert!(encrypt_pool_secret(&k, 1, REF_A, &secret(&"p".repeat(1025))).is_err());
    for bad in ["a\rb", "a\nb", "a\0b"] {
        assert!(encrypt_pool_secret(&k, 1, REF_A, &secret(bad)).is_err());
    }
}

#[test]
fn tampering_and_transplant_fail() {
    let k = key();
    let env = encrypt_pool_secret(&k, 1, REF_A, &secret("pw")).unwrap();

    // Different row key.
    assert!(decrypt_pool_secret(&k, 1, REF_B, &env).is_err());
    // Wrong KEK, wrong pinned version.
    assert!(decrypt_pool_secret(&key(), 1, REF_A, &env).is_err());
    assert_eq!(
        decrypt_pool_secret(&k, 2, REF_A, &env).err(),
        Some(VaultError::Crypto)
    );

    let mut e = encrypt_pool_secret(&k, 1, REF_A, &secret("pw")).unwrap();
    e.ciphertext[0] ^= 1;
    assert!(decrypt_pool_secret(&k, 1, REF_A, &e).is_err());
    let mut e = encrypt_pool_secret(&k, 1, REF_A, &secret("pw")).unwrap();
    e.dek_wrapped[0] ^= 1;
    assert!(decrypt_pool_secret(&k, 1, REF_A, &e).is_err());
    let mut e = encrypt_pool_secret(&k, 1, REF_A, &secret("pw")).unwrap();
    e.dek_wrapped.pop();
    assert_eq!(
        decrypt_pool_secret(&k, 1, REF_A, &e).err(),
        Some(VaultError::CorruptStoredData)
    );
}

#[test]
fn rewrap_changes_only_the_wrap() {
    let (old, new) = (key(), key());
    let env = encrypt_pool_secret(&old, 1, REF_A, &secret("pw")).unwrap();
    let re = rewrap_pool_secret(&old, 1, &new, 2, REF_A, &env).unwrap();

    assert_eq!(re.ciphertext, env.ciphertext);
    assert_eq!(re.payload_nonce, env.payload_nonce);
    assert_ne!(re.dek_wrapped, env.dek_wrapped);
    assert_ne!(re.dek_wrapped_nonce, env.dek_wrapped_nonce);
    assert_eq!(re.dek_wrapped.len(), 48);

    assert_eq!(
        decrypt_pool_secret(&new, 2, REF_A, &re).unwrap().password,
        "pw"
    );
    // Version pin: the new wrap does not open under the old pair or a
    // mismatched version label.
    assert!(decrypt_pool_secret(&old, 1, REF_A, &re).is_err());
    assert!(decrypt_pool_secret(&new, 1, REF_A, &re).is_err());
    assert!(decrypt_pool_secret(&new, 3, REF_A, &re).is_err());
    // Wrong row, wrong old KEK/version fail to rewrap.
    assert!(rewrap_pool_secret(&old, 1, &new, 2, REF_B, &env).is_err());
    assert!(rewrap_pool_secret(&new, 1, &new, 2, REF_A, &env).is_err());
    assert!(rewrap_pool_secret(&old, 9, &new, 10, REF_A, &env).is_err());
}
