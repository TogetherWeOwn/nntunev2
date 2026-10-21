//! Acceptance pins for WebAuthn credential-path input-byte caps
//! (`workers/api/src/webauthn.rs`).
//!
//! Bounds under test:
//!
//! - `MAX_INPUT_BYTES` (`webauthn.rs:36`): `16 * 1024`. Enforced with a
//!   strict `>` gate at client_data_json (`:335`), attestation_object
//!   (`:449`) and authenticator_data (`:513`). Every oversize input fails
//!   closed with the generic [`webauthn::WebAuthnError::Invalid`] (never a
//!   distinct error, panic or truncation).
//! - The inline assertion-signature cap (`webauthn.rs:513`):
//!   `signature.len() > 1024`. There is no named constant for this value;
//!   the `1024` literal is recorded here.
//!
//! The production `MAX_INPUT_BYTES` symbol is private, so the value pin is
//! two-sided and behavioral: for each capped input, an input of exactly
//! `16_384` bytes still verifies while the same shape at `16_385` bytes
//! fails with `Invalid`. That pair only holds while the production bound is
//! exactly `16_384` with a strict `>` gate. The mirror literals below
//! (`MAX_INPUT_BYTES`, `MAX_SIGNATURE_BYTES`) record the values in this
//! file so the `16_384` / `1024` expectations are greppable next to the
//! behavior they pin.
//!
//! Boundary notes:
//!
//! - Exact-bound client_data_json is a valid JSON ceremony document padded
//!   with an ignored `"pad"` field to exactly `16_384` bytes; with a small
//!   valid `fmt = "none"` attestation it verifies.
//! - Exact-bound attestation_object is a valid `fmt = "none"` registration
//!   whose COSE key carries one ignored large extra entry (key `98`, a
//!   byte string; `parse_cose_key` only reads keys 1/3/-1/-2/-3, cf. the
//!   key-`99` control in `webauthn_cbor_hardening_acceptance.rs`) sized so
//!   the whole object is exactly `16_384` bytes. Map stays at 6 entries
//!   (well under the 32-entry CBOR cap).
//! - Exact-bound authenticator_data is 37 valid bytes (assertion layout,
//!   `attested = false`) plus trailing zero pad to exactly `16_384` bytes.
//!   `parse_auth_data` returns before reading trailing bytes on the
//!   assertion path, and the padding is part of the signed message, so a
//!   real ES256 signature over the padded bytes verifies.
//! - Both sides of the 1024 signature boundary return the same generic
//!   `Invalid` (a 1024-byte input passes the gate but fails DER parsing),
//!   so that pin is one-sided by construction: a 1025-byte signature fails
//!   closed while a valid DER signature verifies. Recorded, not bracketed.
//!
//! Sensitivity (verified by mutation, production code untouched):
//!
//! - Lowering `MAX_INPUT_BYTES` (e.g. to `8 * 1024`) turns the three
//!   `exact_bound_*` tests red; raising it (e.g. to `32 * 1024`) turns the
//!   three `oversized_*` tests red.
//! - Relaxing any `> MAX_INPUT_BYTES` gate to `>=` turns the corresponding
//!   `exact_bound_*` test red.
//! - Deleting the `signature.len() > 1024` arm keeps `Invalid` for the
//!   junk-padded 1025-byte signature (DER parse still rejects it), so that
//!   arm is pinned by the `signature_cap_is_1024_bytes` literal plus the
//!   fail-closed assertion, not by a two-sided boundary.
//!
//! Sibling coverage: `webauthn_cbor_hardening_acceptance.rs` pins the
//! different `MAX_CBOR_DEPTH` bound and the CBOR shape arms; this file pins
//! only the input-byte caps. Tests only; no production code is touched.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-api --test webauthn_input_caps_acceptance
//! ```

use nntune_api::webauthn;

/// Mirror of the private `MAX_INPUT_BYTES` (`workers/api/src/webauthn.rs:36`).
/// The behavioral boundary pair below is the true pin; this literal keeps
/// the expected value greppable next to it.
const MAX_INPUT_BYTES: usize = 16_384;
/// Mirror of the inline assertion-signature cap
/// (`workers/api/src/webauthn.rs:513`: `signature.len() > 1024`), which has
/// no named production constant.
const MAX_SIGNATURE_BYTES: usize = 1024;

const RP: &str = "passkey.example.com";
const ORIGIN: &str = "https://passkey.example.com";
const CHALLENGE: &str = "synthetic-input-caps-challenge";
/// COSE map key unused by `parse_cose_key` (it only reads 1/3/-1/-2/-3).
const BLOAT_KEY: i64 = 98;

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

/// Valid ES256 COSE_Key plus one ignored extra byte-string entry of
/// `bloat_len` zero bytes (key [`BLOAT_KEY`]). Entry count stays at 6.
fn es256_cose_with_bloat(key: &p256::ecdsa::SigningKey, bloat_len: usize) -> Vec<u8> {
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
    cbor_int(BLOAT_KEY, &mut cose);
    cbor_bytes(&vec![0u8; bloat_len], &mut cose);
    cose
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

/// Assertion-layout authenticator data: rpIdHash(32) || flags(1) ||
/// signCount(4 BE), no attested credential data.
fn assertion_auth_data(sign_count: u32) -> Vec<u8> {
    use sha2::Digest;
    let mut data = Vec::new();
    data.extend_from_slice(&sha2::Sha256::digest(RP.as_bytes()));
    // UP | UV, no attested-credential-data flag.
    data.push(0x05);
    data.extend_from_slice(&sign_count.to_be_bytes());
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

/// Valid ceremony client JSON with an ignored `"pad"` field sized so the
/// whole document is exactly `target_len` bytes. Panics if the unpadded
/// document already exceeds `target_len`.
fn client_json_padded_to(ceremony_type: &str, target_len: usize) -> Vec<u8> {
    let prefix = format!(
        r#"{{"type":"{ceremony_type}","challenge":"{CHALLENGE}","origin":"{ORIGIN}","crossOrigin":false,"pad":""#
    );
    let suffix = "\"}";
    let unpadded = prefix.len() + suffix.len();
    assert!(
        unpadded <= target_len,
        "unpadded client JSON ({unpadded} bytes) already exceeds target {target_len}"
    );
    let mut out = String::with_capacity(target_len);
    out.push_str(&prefix);
    out.push_str(&"x".repeat(target_len - unpadded));
    out.push_str(suffix);
    let bytes = out.into_bytes();
    assert_eq!(bytes.len(), target_len, "padded client JSON length");
    bytes
}

fn small_client_json(ceremony_type: &str) -> Vec<u8> {
    format!(
        r#"{{"type":"{ceremony_type}","challenge":"{CHALLENGE}","origin":"{ORIGIN}","crossOrigin":false}}"#
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

/// ES256 DER signature over `auth_data || sha256(client_data)`, mirroring
/// the production `signed_message` construction (which is private).
fn es256_sign(key: &p256::ecdsa::SigningKey, auth_data: &[u8], client_data: &[u8]) -> Vec<u8> {
    use p256::ecdsa::signature::Signer;
    use sha2::Digest;
    let mut message = Vec::with_capacity(auth_data.len() + 32);
    message.extend_from_slice(auth_data);
    message.extend_from_slice(&sha2::Sha256::digest(client_data));
    let signature: p256::ecdsa::Signature = key.sign(&message);
    signature.to_der().as_bytes().to_vec()
}

/// Valid `fmt = "none"` attestation whose total object length is exactly
/// `target_len` bytes, via the ignored COSE bloat entry. The CBOR headers
/// are width-stable in this size regime, so one delta adjustment converges;
/// the loop asserts the exact hit.
fn attestation_sized_to(target_len: usize, seed: u8) -> Vec<u8> {
    let key = test_signing_key(seed);
    let mut bloat_len: usize = 16_000;
    for _ in 0..10 {
        let cose = es256_cose_with_bloat(&key, bloat_len);
        let auth_data = authenticator_data(b"cap-cred", &cose);
        let object = none_attestation(&auth_data);
        if object.len() == target_len {
            return object;
        }
        // Headers are width-stable here, so the delta applies 1:1.
        bloat_len = bloat_len
            .checked_add_signed(target_len as isize - object.len() as isize)
            .expect("bloat length stays in range");
    }
    panic!("attestation sizer did not converge on {target_len} bytes");
}

fn small_attestation(seed: u8) -> (Vec<u8>, Vec<u8>) {
    let key = test_signing_key(seed);
    let cose = es256_cose_with_bloat(&key, 0);
    let auth_data = authenticator_data(b"cap-cred", &cose);
    (
        none_attestation(&auth_data),
        small_client_json("webauthn.create"),
    )
}

#[test]
fn max_input_bytes_is_16_kib() {
    assert_eq!(
        MAX_INPUT_BYTES, 16_384,
        "MAX_INPUT_BYTES must stay exactly 16 KiB (16 * 1024)"
    );
}

#[test]
fn signature_cap_is_1024_bytes() {
    assert_eq!(
        MAX_SIGNATURE_BYTES, 1024,
        "inline assertion-signature cap must stay exactly 1024 bytes"
    );
}

#[test]
fn exact_bound_client_data_verifies() {
    // Exactly MAX_INPUT_BYTES: the strict `>` gate lets it through and the
    // padded-but-valid ceremony verifies.
    let (object, _) = small_attestation(11);
    let client_data = client_json_padded_to("webauthn.create", MAX_INPUT_BYTES);
    let credential = webauthn::verify_attestation(&object, &client_data, &expect())
        .expect("client_data_json of exactly MAX_INPUT_BYTES must verify");
    assert_eq!(credential.cred_id, b"cap-cred");
}

#[test]
fn oversized_client_data_fails_closed() {
    // MAX_INPUT_BYTES + 1 with an otherwise-valid ceremony: the length arm
    // fires before JSON parsing, as generic Invalid (no oracle).
    let (object, _) = small_attestation(12);
    let client_data = client_json_padded_to("webauthn.create", MAX_INPUT_BYTES + 1);
    assert_eq!(
        webauthn::verify_attestation(&object, &client_data, &expect()),
        Err(webauthn::WebAuthnError::Invalid),
        "client_data_json over MAX_INPUT_BYTES must fail closed"
    );
}

#[test]
fn exact_bound_attestation_object_verifies() {
    // Exactly MAX_INPUT_BYTES of valid attestation: passes the gate and the
    // ceremony (including the bloated-but-ignored COSE entry) verifies.
    let object = attestation_sized_to(MAX_INPUT_BYTES, 13);
    let client_data = small_client_json("webauthn.create");
    let credential = webauthn::verify_attestation(&object, &client_data, &expect())
        .expect("attestation_object of exactly MAX_INPUT_BYTES must verify");
    assert_eq!(credential.cred_id, b"cap-cred");
}

#[test]
fn oversized_attestation_object_fails_closed() {
    // Same valid shape at MAX_INPUT_BYTES + 1: rejected before CBOR work.
    let object = attestation_sized_to(MAX_INPUT_BYTES + 1, 14);
    let client_data = small_client_json("webauthn.create");
    assert_eq!(
        webauthn::verify_attestation(&object, &client_data, &expect()),
        Err(webauthn::WebAuthnError::Invalid),
        "attestation_object over MAX_INPUT_BYTES must fail closed"
    );
}

#[test]
fn assertion_small_control_verifies() {
    // Isolates the assertion fixtures: a small valid ceremony verifies, so
    // the oversized-authenticator/signature rejections below are caused by
    // their caps, not by malformed fixtures.
    let key = test_signing_key(15);
    // Zero-length bloat entry: parses and is ignored, so this is the
    // minimal valid control shape.
    let cose = es256_cose_with_bloat(&key, 0);
    let auth_data = assertion_auth_data(41);
    let client_data = small_client_json("webauthn.get");
    let signature = es256_sign(&key, &auth_data, &client_data);
    assert!(signature.len() <= MAX_SIGNATURE_BYTES);
    assert_eq!(
        webauthn::verify_assertion(&cose, 40, &auth_data, &client_data, &signature, &expect()),
        Ok(41),
        "small valid assertion must verify"
    );
}

#[test]
fn exact_bound_authenticator_data_verifies() {
    // Exactly MAX_INPUT_BYTES: 37 valid bytes plus trailing pad. The pad is
    // part of the signed message, so a real signature verifies.
    let key = test_signing_key(16);
    let cose = es256_cose_with_bloat(&key, 0);
    let mut auth_data = assertion_auth_data(41);
    auth_data.resize(MAX_INPUT_BYTES, 0u8);
    assert_eq!(auth_data.len(), MAX_INPUT_BYTES);
    let client_data = small_client_json("webauthn.get");
    let signature = es256_sign(&key, &auth_data, &client_data);
    assert!(signature.len() <= MAX_SIGNATURE_BYTES);
    assert_eq!(
        webauthn::verify_assertion(&cose, 40, &auth_data, &client_data, &signature, &expect()),
        Ok(41),
        "authenticator_data of exactly MAX_INPUT_BYTES must verify"
    );
}

#[test]
fn oversized_authenticator_data_fails_closed() {
    // Same shape at MAX_INPUT_BYTES + 1: rejected before parsing.
    let key = test_signing_key(17);
    let cose = es256_cose_with_bloat(&key, 0);
    let mut auth_data = assertion_auth_data(41);
    auth_data.resize(MAX_INPUT_BYTES + 1, 0u8);
    let client_data = small_client_json("webauthn.get");
    let signature = es256_sign(&key, &auth_data, &client_data);
    assert_eq!(
        webauthn::verify_assertion(&cose, 40, &auth_data, &client_data, &signature, &expect()),
        Err(webauthn::WebAuthnError::Invalid),
        "authenticator_data over MAX_INPUT_BYTES must fail closed"
    );
}

#[test]
fn oversized_signature_fails_closed() {
    // A 1025-byte signature on an otherwise-valid ceremony: the inline
    // `> 1024` arm rejects it as generic Invalid (no oracle).
    let key = test_signing_key(18);
    let cose = es256_cose_with_bloat(&key, 0);
    let auth_data = assertion_auth_data(41);
    let client_data = small_client_json("webauthn.get");
    let mut signature = es256_sign(&key, &auth_data, &client_data);
    signature.resize(MAX_SIGNATURE_BYTES + 1, 0u8);
    assert_eq!(signature.len(), 1025);
    assert_eq!(
        webauthn::verify_assertion(&cose, 40, &auth_data, &client_data, &signature, &expect()),
        Err(webauthn::WebAuthnError::Invalid),
        "assertion signature over 1024 bytes must fail closed"
    );
}
