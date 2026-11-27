use ed25519_dalek::{Signer, SigningKey};
use nntune_api::{auth, session, vault, webauthn};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::net::IpAddr;

const RP_ID: &str = "music.example.com";
const ORIGIN: &str = "https://music.example.com";
const NOW: u64 = 1_700_000_000;

#[test]
fn magic_link_is_only_valid_within_its_issued_window() {
    let (token, record) = auth::issue_magic_link("member", "member@example.com", NOW).unwrap();
    for now in [NOW - 1, record.expires_at] {
        assert!(matches!(
            auth::consume_magic_link(record.clone(), &token, now),
            Err(auth::AuthError::Invalid)
        ));
    }
    assert!(auth::consume_magic_link(record.clone(), &token, record.expires_at - 1).is_ok());
    let mut oversized = record;
    oversized.expires_at += 1;
    assert!(auth::consume_magic_link(oversized, &token, NOW).is_err());
}

#[test]
fn passkey_challenge_is_only_valid_within_its_issued_window() {
    let challenge = auth::new_registration_challenge(RP_ID, ORIGIN, NOW).unwrap();
    for now in [NOW - 1, challenge.expires_at] {
        assert!(matches!(
            auth::verify_and_consume_challenge(challenge.clone(), RP_ID, ORIGIN, now),
            Err(auth::AuthError::Invalid)
        ));
    }
    assert!(auth::verify_and_consume_challenge(
        challenge.clone(),
        RP_ID,
        ORIGIN,
        challenge.expires_at - 1
    )
    .is_ok());
    let mut oversized = challenge;
    oversized.expires_at += 1;
    assert!(auth::verify_and_consume_challenge(oversized, RP_ID, ORIGIN, NOW).is_err());
}

#[test]
fn session_issuance_and_verification_bound_token_size() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use hmac::{Hmac, KeyInit, Mac};

    let key = [21; 32];
    let token = session::issue_session(&key, "member", NOW, 300).unwrap();
    let mut claims = session::verify_session(&key, &token, NOW).unwrap();
    claims.user_id = "m".repeat(4096);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
    mac.update(payload.as_bytes());
    let oversized = format!(
        "{payload}.{}",
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    );
    assert_eq!(
        session::verify_session(&key, &oversized, NOW),
        Err(session::SessionError::Invalid)
    );
    assert_eq!(
        session::issue_session(&key, &claims.user_id, NOW, 300),
        Err(session::SessionError::Invalid)
    );
}

#[test]
fn session_rotation_refuses_a_clock_before_creation() {
    let key = [22; 32];
    let token = session::issue_session(&key, "member", NOW, 300).unwrap();
    let keys = session::SessionKeys::single(&key);
    assert!(session::verify_session(&key, &token, NOW - 1).is_ok());
    assert_eq!(
        session::rotate_session(&keys, &token, NOW - 1, 300),
        Err(session::SessionError::Invalid)
    );
    let rotated = session::rotate_session(&keys, &token, NOW, 300).unwrap();
    assert_eq!(
        session::verify_session(&key, &rotated, NOW)
            .unwrap()
            .rotation_counter,
        1
    );
}

#[test]
fn provider_dial_rejects_ambiguous_hosts_before_dns() {
    #[derive(Default)]
    struct PublicDns(RefCell<Vec<String>>);
    impl vault::DnsResolver for PublicDns {
        fn resolve(&self, host: &str) -> Vec<IpAddr> {
            self.0.borrow_mut().push(host.to_string());
            vec![IpAddr::from([93, 184, 216, 34])]
        }
    }

    let dns = PublicDns::default();
    for host in ["127.1", "0x7f000001", "2130706433", "bad-.example.com"] {
        assert!(matches!(
            vault::authorize_pool_dial(host, 563, &dns),
            Err(vault::VaultError::Invalid)
        ));
    }
    assert!(dns.0.borrow().is_empty());
    assert!(vault::authorize_pool_dial("93.184.216.34", 563, &dns).is_ok());
    assert!(vault::authorize_pool_dial("news.example.com", 563, &dns).is_ok());
    assert_eq!(*dns.0.borrow(), ["news.example.com"]);
}

fn assertion(
    client_fields: &str,
    flags: u8,
    extensions: &[u8],
) -> Result<u32, webauthn::WebAuthnError> {
    let key = SigningKey::from_bytes(&[41; 32]);
    // COSE OKP, EdDSA, Ed25519, public x coordinate.
    let mut cose = vec![0xa4, 1, 1, 3, 0x27, 0x20, 6, 0x21, 0x58, 32];
    cose.extend_from_slice(key.verifying_key().as_bytes());
    let client_data = format!(
        r#"{{"type":"webauthn.get","challenge":"offline-challenge","origin":"{ORIGIN}"{client_fields}}}"#
    );
    let mut auth_data = Sha256::digest(RP_ID).to_vec();
    auth_data.extend_from_slice(&[flags, 0, 0, 0, 1]);
    auth_data.extend_from_slice(extensions);
    let mut message = auth_data.clone();
    message.extend(Sha256::digest(client_data.as_bytes()));
    let signature = key.sign(&message).to_bytes();
    webauthn::verify_assertion(
        &cose,
        0,
        &auth_data,
        client_data.as_bytes(),
        &signature,
        &webauthn::Expect {
            rp_id: RP_ID,
            origin: ORIGIN,
            challenge: "offline-challenge",
        },
    )
}

#[test]
fn webauthn_refuses_ambiguous_client_security_fields() {
    assert_eq!(assertion("", 0x05, &[]), Ok(1));
    assert_eq!(assertion(r#", "crossOrigin": false"#, 0x05, &[]), Ok(1));
    for fields in [
        r#", "crossOrigin": null"#,
        r#", "crossOrigin": "false"#,
        r#", "crossOrigin": true, "crossOrigin": false"#,
        r#", "origin": "https://music.example.com"#,
    ] {
        assert_eq!(
            assertion(fields, 0x05, &[]),
            Err(webauthn::WebAuthnError::Invalid)
        );
    }
}

#[test]
fn webauthn_refuses_inconsistent_flags_and_extension_bytes() {
    assert_eq!(assertion("", 0x9d, &[0xa0]), Ok(1));
    for (flags, extensions) in [
        (0x15, &[][..]),
        (0x05, &[0xa0][..]),
        (0x85, &[][..]),
        (0x85, &[0x80][..]),
        (0x85, &[0xa0, 0][..]),
    ] {
        assert_eq!(
            assertion("", flags, extensions),
            Err(webauthn::WebAuthnError::Invalid)
        );
    }
}
