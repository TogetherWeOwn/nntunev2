//! Acceptance pins for WebAuthn CBOR hardening (`workers/api/src/webauthn.rs`).
//!
//! The bounded CBOR parser rejects, all as generic [`webauthn::WebAuthnError::Invalid`]:
//! - nesting deeper than `MAX_CBOR_DEPTH` (8) (`cbor_parse_at` depth arm),
//! - indefinite-length items (`0x9f` etc., `cbor_read_arg` info-31 arm),
//! - arrays over 64 and maps over 32 entries (count arms).
//!
//! These tests drive every arm through the public `verify_attestation`
//! ceremony API with `fmt = "none"`: a valid registration whose COSE key
//! carries one hostile extra value (key `99`, ignored by `parse_cose_key`
//! when parsing succeeds). The 8-deep control proves the extra value is
//! ignored on the success path, so each rejection is caused by its
//! hardening arm, not by a malformed ceremony.
//!
//! Depth accounting (parser counts the top-level value at depth 0):
//! COSE map (depth 0) -> value arrays (depths 1..=N) -> leaf int
//! (depth N+1). `N = 8` puts the leaf at depth 9 (> 8, rejected);
//! `N = 7` puts it at depth 8 (accepted). Including the top map that is
//! 9 levels of nesting vs the 8-level control.

use nntune_api::webauthn;

const RP: &str = "passkey.example.com";
const ORIGIN: &str = "https://passkey.example.com";
const CHALLENGE: &str = "synthetic-cbor-hardening-challenge";
/// COSE map key unused by `parse_cose_key` (it only reads 1/3/-1/-2/-3).
const EXTRA_KEY: i64 = 99;

// --- tiny CBOR encoder (fixtures only) ---

fn cbor_head(major: u8, value: u64, out: &mut Vec<u8>) {
    let marker = major << 5;
    if value < 24 {
        out.push(marker | value as u8);
    } else if value <= u8::MAX as u64 {
        out.push(marker | 24);
        out.push(value as u8);
    } else if value <= u16::MAX as u64 {
        out.push(marker | 25);
        out.extend_from_slice(&(value as u16).to_be_bytes());
    } else {
        out.push(marker | 26);
        out.extend_from_slice(&(value as u32).to_be_bytes());
    }
}

fn cbor_int(value: i64, out: &mut Vec<u8>) {
    if value >= 0 {
        cbor_head(0, value as u64, out);
    } else {
        cbor_head(1, (-1 - value) as u64, out);
    }
}

fn cbor_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    cbor_head(2, bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn cbor_text(text: &str, out: &mut Vec<u8>) {
    cbor_head(3, text.len() as u64, out);
    out.extend_from_slice(text.as_bytes());
}

fn test_signing_key(seed: u8) -> p256::ecdsa::SigningKey {
    p256::ecdsa::SigningKey::from_slice(&[seed; 32]).expect("fixed test key")
}

/// Valid ES256 COSE_Key plus one extra entry whose raw CBOR value is
/// supplied by the caller. A parseable extra value is ignored, so the
/// ceremony still verifies (see the 8-deep control).
fn es256_cose_with_extra(key: &p256::ecdsa::SigningKey, extra_value: &[u8]) -> Vec<u8> {
    let point = key.verifying_key().to_sec1_point(false);
    let mut cose = Vec::new();
    cbor_head(5, 6, &mut cose);
    cbor_int(1, &mut cose);
    cbor_int(2, &mut cose);
    cbor_int(3, &mut cose);
    cbor_int(-7, &mut cose);
    cbor_int(-1, &mut cose);
    cbor_int(1, &mut cose);
    cbor_int(-2, &mut cose);
    cbor_bytes(point.x().expect("x coordinate"), &mut cose);
    cbor_int(-3, &mut cose);
    cbor_bytes(point.y().expect("y coordinate"), &mut cose);
    cbor_int(EXTRA_KEY, &mut cose);
    cose.extend_from_slice(extra_value);
    cose
}

/// `levels` nested single-element arrays around int 0.
fn nested_arrays(levels: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..levels {
        cbor_head(4, 1, &mut out);
    }
    cbor_int(0, &mut out);
    out
}

fn array_65() -> Vec<u8> {
    let mut out = Vec::new();
    cbor_head(4, 65, &mut out);
    for _ in 0..65 {
        cbor_int(0, &mut out);
    }
    out
}

fn map_33() -> Vec<u8> {
    let mut out = Vec::new();
    cbor_head(5, 33, &mut out);
    for i in 0..33 {
        cbor_int(1000 + i, &mut out);
        cbor_int(i, &mut out);
    }
    out
}

fn authenticator_data(cred_id: &[u8], cose: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    let mut data = Vec::new();
    data.extend_from_slice(&sha2::Sha256::digest(RP.as_bytes()));
    // AT | UP | UV: attested credential data with user verification.
    data.push(0x45);
    data.extend_from_slice(&0u32.to_be_bytes());
    data.extend_from_slice(&[0u8; 16]); // AAGUID
    data.extend_from_slice(&(cred_id.len() as u16).to_be_bytes());
    data.extend_from_slice(cred_id);
    data.extend_from_slice(cose);
    data
}

fn none_attestation(auth_data: &[u8]) -> Vec<u8> {
    let mut object = Vec::new();
    cbor_head(5, 3, &mut object);
    cbor_text("fmt", &mut object);
    cbor_text("none", &mut object);
    cbor_text("authData", &mut object);
    cbor_bytes(auth_data, &mut object);
    cbor_text("attStmt", &mut object);
    cbor_head(5, 0, &mut object);
    object
}

fn client_json() -> Vec<u8> {
    format!(
        r#"{{"type":"webauthn.create","challenge":"{CHALLENGE}","origin":"{ORIGIN}","crossOrigin":false}}"#
    )
    .into_bytes()
}

fn expect() -> webauthn::Expect<'static> {
    webauthn::Expect {
        rp_id: RP,
        origin: ORIGIN,
        challenge: CHALLENGE,
    }
}

fn attestation_for_extra(extra_value: &[u8], seed: u8) -> (Vec<u8>, Vec<u8>) {
    let key = test_signing_key(seed);
    let cose = es256_cose_with_extra(&key, extra_value);
    let auth_data = authenticator_data(b"hardening-cred", &cose);
    (none_attestation(&auth_data), client_json())
}

#[test]
fn nine_deep_nesting_rejected() {
    // COSE map + 8 nested arrays = 9 levels; the leaf int lands at
    // parser depth 9 > MAX_CBOR_DEPTH (8).
    let (object, client_data) = attestation_for_extra(&nested_arrays(8), 41);
    assert_eq!(
        webauthn::verify_attestation(&object, &client_data, &expect()),
        Err(webauthn::WebAuthnError::Invalid),
        "9-deep CBOR nesting must be rejected"
    );
}

#[test]
fn eight_deep_nesting_verifies_control() {
    // COSE map + 7 nested arrays = 8 levels; the leaf int lands at
    // parser depth 8, exactly at the bound. The extra value parses and
    // is ignored, so the ceremony verifies.
    let (object, client_data) = attestation_for_extra(&nested_arrays(7), 42);
    let credential = webauthn::verify_attestation(&object, &client_data, &expect())
        .expect("8-deep nesting must verify");
    assert_eq!(credential.cred_id, b"hardening-cred");
}

#[test]
fn indefinite_length_item_rejected() {
    // 0x9f: major type 4 (array), additional info 31 (indefinite length).
    let (object, client_data) = attestation_for_extra(&[0x9f], 43);
    assert_eq!(
        webauthn::verify_attestation(&object, &client_data, &expect()),
        Err(webauthn::WebAuthnError::Invalid),
        "indefinite-length CBOR item must be rejected"
    );
}

#[test]
fn array_over_64_rejected() {
    let (object, client_data) = attestation_for_extra(&array_65(), 44);
    assert_eq!(
        webauthn::verify_attestation(&object, &client_data, &expect()),
        Err(webauthn::WebAuthnError::Invalid),
        "65-element array must be rejected"
    );
}

#[test]
fn map_over_32_rejected() {
    let (object, client_data) = attestation_for_extra(&map_33(), 45);
    assert_eq!(
        webauthn::verify_attestation(&object, &client_data, &expect()),
        Err(webauthn::WebAuthnError::Invalid),
        "33-entry map must be rejected"
    );
}
