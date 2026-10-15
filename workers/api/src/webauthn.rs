//! WebAuthn attestation/assertion signature verification.
//!
//! Scope (fail-closed): COSE ES256 (-7, P-256) and EdDSA (-8) credential
//! public keys; attestation formats `none` and `packed` *self-attestation*
//! only. `x5c` certificate chains are rejected — no attestation-CA trust
//! store ships in this slice, so accepting one would be unverifiable.
//! Everything else (unknown fmt/alg/kty, malformed CBOR, extensions
//! tricks) maps to the single generic [`WebAuthnError::Invalid`].
//!
//! Enforced on every ceremony:
//! - `clientDataJSON.type` is exactly `webauthn.create`/`webauthn.get`,
//!   `challenge` constant-time-equals the server-issued challenge,
//!   `origin` constant-time-equals the configured canonical origin, and
//!   `crossOrigin` is not true.
//! - `authenticatorData.rpIdHash == SHA-256(rp_id)` and the user-present
//!   flag is set.
//! - Sign-count clone detection: a nonzero presented counter must be
//!   strictly greater than the stored counter.
//!
//! Stored credential public keys are the raw COSE_Key CBOR bytes exactly
//! as they appeared in the attested credential data (never re-encoded).
//! This module is pure: challenge single-use claims and sign-count row
//! updates belong to the caller (`auth`/`handlers`).

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Max attestation/authenticator payload accepted (bounds CBOR work).
const MAX_INPUT_BYTES: usize = 16 * 1024;
/// Max CBOR nesting depth accepted.
const MAX_CBOR_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebAuthnError {
    /// The one generic failure: never distinguish forgery from malformed
    /// input (no ceremony oracle).
    Invalid,
}

/// Server-side ceremony expectations, pinned from Worker config.
pub struct Expect<'a> {
    pub rp_id: &'a str,
    pub origin: &'a str,
    /// The base64url challenge string issued for this ceremony.
    pub challenge: &'a str,
}

/// A credential extracted from a verified attestation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredCredential {
    pub cred_id: Vec<u8>,
    /// Raw COSE_Key CBOR bytes; store verbatim in `webauthn_credentials`.
    pub public_key_cose: Vec<u8>,
    pub sign_count: u32,
}

// --- Minimal bounded CBOR -----------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Cbor {
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Simple(u8),
}

impl Cbor {
    fn as_int(&self) -> Option<i64> {
        match self {
            Cbor::Int(v) => Some(*v),
            _ => None,
        }
    }
    fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Cbor::Bytes(b) => Some(b),
            _ => None,
        }
    }
    fn as_text(&self) -> Option<&str> {
        match self {
            Cbor::Text(t) => Some(t),
            _ => None,
        }
    }
    fn as_map(&self) -> Option<&Vec<(Cbor, Cbor)>> {
        match self {
            Cbor::Map(m) => Some(m),
            _ => None,
        }
    }
}

fn cbor_read_arg(bytes: &[u8], pos: &mut usize, info: u8) -> Result<u64, WebAuthnError> {
    let take = |bytes: &[u8], pos: &mut usize, n: usize| -> Result<u64, WebAuthnError> {
        let end = pos.checked_add(n).ok_or(WebAuthnError::Invalid)?;
        if end > bytes.len() {
            return Err(WebAuthnError::Invalid);
        }
        let mut v = 0u64;
        for &b in &bytes[*pos..end] {
            v = (v << 8) | b as u64;
        }
        *pos = end;
        Ok(v)
    };
    match info {
        0..=23 => Ok(info as u64),
        24 => take(bytes, pos, 1),
        25 => take(bytes, pos, 2),
        26 => take(bytes, pos, 4),
        27 => take(bytes, pos, 8),
        // Indefinite lengths and reserved values: reject.
        _ => Err(WebAuthnError::Invalid),
    }
}

fn cbor_parse_at(bytes: &[u8], pos: &mut usize, depth: usize) -> Result<Cbor, WebAuthnError> {
    if depth > MAX_CBOR_DEPTH || *pos >= bytes.len() {
        return Err(WebAuthnError::Invalid);
    }
    let head = bytes[*pos];
    *pos += 1;
    let major = head >> 5;
    let info = head & 0x1f;
    match major {
        0 => {
            let v = cbor_read_arg(bytes, pos, info)?;
            i64::try_from(v)
                .map(Cbor::Int)
                .map_err(|_| WebAuthnError::Invalid)
        }
        1 => {
            let v = cbor_read_arg(bytes, pos, info)?;
            let v = i64::try_from(v).map_err(|_| WebAuthnError::Invalid)?;
            Ok(Cbor::Int(-1 - v))
        }
        2 | 3 => {
            let len = usize::try_from(cbor_read_arg(bytes, pos, info)?)
                .map_err(|_| WebAuthnError::Invalid)?;
            let end = pos.checked_add(len).ok_or(WebAuthnError::Invalid)?;
            if end > bytes.len() {
                return Err(WebAuthnError::Invalid);
            }
            let raw = bytes[*pos..end].to_vec();
            *pos = end;
            if major == 2 {
                Ok(Cbor::Bytes(raw))
            } else {
                String::from_utf8(raw)
                    .map(Cbor::Text)
                    .map_err(|_| WebAuthnError::Invalid)
            }
        }
        4 => {
            let count = usize::try_from(cbor_read_arg(bytes, pos, info)?)
                .map_err(|_| WebAuthnError::Invalid)?;
            if count > 64 {
                return Err(WebAuthnError::Invalid);
            }
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(cbor_parse_at(bytes, pos, depth + 1)?);
            }
            Ok(Cbor::Array(items))
        }
        5 => {
            let count = usize::try_from(cbor_read_arg(bytes, pos, info)?)
                .map_err(|_| WebAuthnError::Invalid)?;
            if count > 32 {
                return Err(WebAuthnError::Invalid);
            }
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                let k = cbor_parse_at(bytes, pos, depth + 1)?;
                // Duplicate keys make map semantics ambiguous: reject.
                if entries.iter().any(|(existing, _)| *existing == k) {
                    return Err(WebAuthnError::Invalid);
                }
                let v = cbor_parse_at(bytes, pos, depth + 1)?;
                entries.push((k, v));
            }
            Ok(Cbor::Map(entries))
        }
        7 => {
            // Only simple values are tolerated (floats rejected): nothing
            // in WebAuthn attestation/assertion needs them.
            if info >= 25 {
                return Err(WebAuthnError::Invalid);
            }
            let v = cbor_read_arg(bytes, pos, info)?;
            u8::try_from(v)
                .map(Cbor::Simple)
                .map_err(|_| WebAuthnError::Invalid)
        }
        _ => Err(WebAuthnError::Invalid),
    }
}

/// Parse a complete CBOR document; trailing bytes are an error.
fn cbor_parse(bytes: &[u8]) -> Result<Cbor, WebAuthnError> {
    let mut pos = 0usize;
    let value = cbor_parse_at(bytes, &mut pos, 0)?;
    if pos != bytes.len() {
        return Err(WebAuthnError::Invalid);
    }
    Ok(value)
}

fn map_get_text<'a>(map: &'a [(Cbor, Cbor)], key: &str) -> Option<&'a Cbor> {
    map.iter()
        .find(|(k, _)| k.as_text() == Some(key))
        .map(|(_, v)| v)
}

fn map_get_int(map: &[(Cbor, Cbor)], key: i64) -> Option<&Cbor> {
    map.iter()
        .find(|(k, _)| k.as_int() == Some(key))
        .map(|(_, v)| v)
}

// --- COSE keys ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoseKey {
    Es256 { x: [u8; 32], y: [u8; 32] },
    EdDsa { x: [u8; 32] },
}

impl CoseKey {
    fn alg(&self) -> i64 {
        match self {
            CoseKey::Es256 { .. } => -7,
            CoseKey::EdDsa { .. } => -8,
        }
    }
}

fn parse_cose_key(bytes: &[u8]) -> Result<CoseKey, WebAuthnError> {
    let value = cbor_parse(bytes)?;
    let map = value.as_map().ok_or(WebAuthnError::Invalid)?;
    let kty = map_get_int(map, 1)
        .and_then(Cbor::as_int)
        .ok_or(WebAuthnError::Invalid)?;
    let alg = map_get_int(map, 3)
        .and_then(Cbor::as_int)
        .ok_or(WebAuthnError::Invalid)?;
    let crv = map_get_int(map, -1)
        .and_then(Cbor::as_int)
        .ok_or(WebAuthnError::Invalid)?;
    let x = map_get_int(map, -2)
        .and_then(Cbor::as_bytes)
        .ok_or(WebAuthnError::Invalid)?;
    match (kty, alg, crv) {
        (2, -7, 1) => {
            let y = map_get_int(map, -3)
                .and_then(Cbor::as_bytes)
                .ok_or(WebAuthnError::Invalid)?;
            let x: [u8; 32] = x.try_into().map_err(|_| WebAuthnError::Invalid)?;
            let y: [u8; 32] = y.try_into().map_err(|_| WebAuthnError::Invalid)?;
            // Reject points not on P-256 (also rejects the identity).
            let point = p256::EncodedPoint::from_affine_coordinates(
                p256::FieldBytes::from_slice(&x),
                p256::FieldBytes::from_slice(&y),
                false,
            );
            p256::ecdsa::VerifyingKey::from_encoded_point(&point)
                .map_err(|_| WebAuthnError::Invalid)?;
            Ok(CoseKey::Es256 { x, y })
        }
        (1, -8, 6) => {
            let x: [u8; 32] = x.try_into().map_err(|_| WebAuthnError::Invalid)?;
            ed25519_dalek::VerifyingKey::from_bytes(&x).map_err(|_| WebAuthnError::Invalid)?;
            Ok(CoseKey::EdDsa { x })
        }
        _ => Err(WebAuthnError::Invalid),
    }
}

fn verify_signature(key: &CoseKey, message: &[u8], signature: &[u8]) -> Result<(), WebAuthnError> {
    match key {
        CoseKey::Es256 { x, y } => {
            use p256::ecdsa::signature::Verifier;
            let point = p256::EncodedPoint::from_affine_coordinates(
                p256::FieldBytes::from_slice(x),
                p256::FieldBytes::from_slice(y),
                false,
            );
            let verifying = p256::ecdsa::VerifyingKey::from_encoded_point(&point)
                .map_err(|_| WebAuthnError::Invalid)?;
            // WebAuthn ES256 signatures are ASN.1 DER.
            let sig =
                p256::ecdsa::Signature::from_der(signature).map_err(|_| WebAuthnError::Invalid)?;
            verifying
                .verify(message, &sig)
                .map_err(|_| WebAuthnError::Invalid)
        }
        CoseKey::EdDsa { x } => {
            use ed25519_dalek::Verifier;
            let verifying =
                ed25519_dalek::VerifyingKey::from_bytes(x).map_err(|_| WebAuthnError::Invalid)?;
            let sig_bytes: [u8; 64] = signature.try_into().map_err(|_| WebAuthnError::Invalid)?;
            let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
            verifying
                .verify(message, &sig)
                .map_err(|_| WebAuthnError::Invalid)
        }
    }
}

// --- clientDataJSON / authenticatorData ---------------------------------------

fn ct_eq_str(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).unwrap_u8() == 1
}

fn verify_client_data(
    client_data_json: &[u8],
    expected_type: &str,
    expect: &Expect<'_>,
) -> Result<(), WebAuthnError> {
    if client_data_json.len() > MAX_INPUT_BYTES {
        return Err(WebAuthnError::Invalid);
    }
    let value: serde_json::Value =
        serde_json::from_slice(client_data_json).map_err(|_| WebAuthnError::Invalid)?;
    let obj = value.as_object().ok_or(WebAuthnError::Invalid)?;
    let type_ = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or(WebAuthnError::Invalid)?;
    let challenge = obj
        .get("challenge")
        .and_then(|v| v.as_str())
        .ok_or(WebAuthnError::Invalid)?;
    let origin = obj
        .get("origin")
        .and_then(|v| v.as_str())
        .ok_or(WebAuthnError::Invalid)?;
    if obj.get("crossOrigin").and_then(|v| v.as_bool()) == Some(true) {
        return Err(WebAuthnError::Invalid);
    }
    if !ct_eq_str(type_, expected_type)
        || !ct_eq_str(challenge, expect.challenge)
        || !ct_eq_str(origin, expect.origin)
    {
        return Err(WebAuthnError::Invalid);
    }
    Ok(())
}

struct AuthData {
    sign_count: u32,
    attested: Option<RegisteredCredential>,
}

fn parse_auth_data(
    bytes: &[u8],
    expect: &Expect<'_>,
    attested: bool,
) -> Result<AuthData, WebAuthnError> {
    // rpIdHash(32) || flags(1) || signCount(4 BE) || [attested credential data]
    if bytes.len() < 37 {
        return Err(WebAuthnError::Invalid);
    }
    let rp_hash = Sha256::digest(expect.rp_id.as_bytes());
    if bytes[..32].ct_eq(rp_hash.as_slice()).unwrap_u8() != 1 {
        return Err(WebAuthnError::Invalid);
    }
    let flags = bytes[32];
    let user_present = flags & 0x01 != 0;
    let has_attested = flags & 0x40 != 0;
    if !user_present || has_attested != attested {
        return Err(WebAuthnError::Invalid);
    }
    let sign_count = u32::from_be_bytes([bytes[33], bytes[34], bytes[35], bytes[36]]);
    if !attested {
        return Ok(AuthData {
            sign_count,
            attested: None,
        });
    }
    // AAGUID(16) || credIdLen(2 BE) || credId || COSE public key (CBOR)
    if bytes.len() < 37 + 18 {
        return Err(WebAuthnError::Invalid);
    }
    let cred_id_len = u16::from_be_bytes([bytes[53], bytes[54]]) as usize;
    if cred_id_len == 0 || cred_id_len > 1024 {
        return Err(WebAuthnError::Invalid);
    }
    let cred_id_start: usize = 55;
    let cred_id_end = cred_id_start
        .checked_add(cred_id_len)
        .ok_or(WebAuthnError::Invalid)?;
    if cred_id_end >= bytes.len() {
        return Err(WebAuthnError::Invalid);
    }
    let cred_id = bytes[cred_id_start..cred_id_end].to_vec();
    let mut pos = cred_id_end;
    let cose = cbor_parse_at(bytes, &mut pos, 0)?;
    // The stored bytes are the exact CBOR slice (never re-encoded).
    let public_key_cose = bytes[cred_id_end..pos].to_vec();
    drop(cose);
    // Reject keys we cannot verify with, right at registration.
    parse_cose_key(&public_key_cose)?;
    Ok(AuthData {
        sign_count,
        attested: Some(RegisteredCredential {
            cred_id,
            public_key_cose,
            sign_count,
        }),
    })
}

fn signed_message(auth_data: &[u8], client_data_json: &[u8]) -> Vec<u8> {
    let client_hash = Sha256::digest(client_data_json);
    let mut msg = Vec::with_capacity(auth_data.len() + client_hash.len());
    msg.extend_from_slice(auth_data);
    msg.extend_from_slice(&client_hash);
    msg
}

// --- Ceremonies -----------------------------------------------------------------

/// Verify a registration attestation (`fmt=none` or packed self-attestation)
/// and extract the credential to store. The caller must still claim the
/// challenge via `auth::consume_challenge_atomically` — this function proves
/// the authenticator's statements, not single-use.
pub fn verify_attestation(
    attestation_object: &[u8],
    client_data_json: &[u8],
    expect: &Expect<'_>,
) -> Result<RegisteredCredential, WebAuthnError> {
    if attestation_object.len() > MAX_INPUT_BYTES {
        return Err(WebAuthnError::Invalid);
    }
    verify_client_data(client_data_json, "webauthn.create", expect)?;
    let value = cbor_parse(attestation_object)?;
    let map = value.as_map().ok_or(WebAuthnError::Invalid)?;
    let fmt = map_get_text(map, "fmt")
        .and_then(Cbor::as_text)
        .ok_or(WebAuthnError::Invalid)?;
    let auth_data_bytes = map_get_text(map, "authData")
        .and_then(Cbor::as_bytes)
        .ok_or(WebAuthnError::Invalid)?;
    let att_stmt = map_get_text(map, "attStmt")
        .and_then(Cbor::as_map)
        .ok_or(WebAuthnError::Invalid)?;
    // `authData` is borrowed from the parsed CBOR; copy so the owned
    // credential does not depend on the parse tree.
    let auth_data_bytes = auth_data_bytes.to_vec();
    let parsed = parse_auth_data(&auth_data_bytes, expect, true)?;
    let credential = parsed.attested.ok_or(WebAuthnError::Invalid)?;
    match fmt {
        "none" => {
            if !att_stmt.is_empty() {
                return Err(WebAuthnError::Invalid);
            }
        }
        "packed" => {
            // Self-attestation only: an x5c chain cannot be validated here.
            if map_get_text(att_stmt, "x5c").is_some() {
                return Err(WebAuthnError::Invalid);
            }
            let alg = map_get_text(att_stmt, "alg")
                .and_then(Cbor::as_int)
                .ok_or(WebAuthnError::Invalid)?;
            let sig = map_get_text(att_stmt, "sig")
                .and_then(Cbor::as_bytes)
                .ok_or(WebAuthnError::Invalid)?;
            let key = parse_cose_key(&credential.public_key_cose)?;
            if alg != key.alg() {
                return Err(WebAuthnError::Invalid);
            }
            verify_signature(
                &key,
                &signed_message(&auth_data_bytes, client_data_json),
                sig,
            )?;
        }
        _ => return Err(WebAuthnError::Invalid),
    }
    Ok(credential)
}

/// Verify an authentication assertion against the stored COSE public key.
/// Returns the presented sign count (the caller persists it with a
/// conditional row update). Clone detection: when either counter is
/// nonzero the presented counter must strictly increase.
pub fn verify_assertion(
    public_key_cose: &[u8],
    stored_sign_count: u32,
    authenticator_data: &[u8],
    client_data_json: &[u8],
    signature: &[u8],
    expect: &Expect<'_>,
) -> Result<u32, WebAuthnError> {
    if authenticator_data.len() > MAX_INPUT_BYTES || signature.len() > 1024 {
        return Err(WebAuthnError::Invalid);
    }
    verify_client_data(client_data_json, "webauthn.get", expect)?;
    let parsed = parse_auth_data(authenticator_data, expect, false)?;
    let key = parse_cose_key(public_key_cose)?;
    verify_signature(
        &key,
        &signed_message(authenticator_data, client_data_json),
        signature,
    )?;
    // Counterless authenticators always present 0; only a replayed/cloned
    // counter is evidence of cloning.
    if (parsed.sign_count != 0 || stored_sign_count != 0) && parsed.sign_count <= stored_sign_count
    {
        return Err(WebAuthnError::Invalid);
    }
    Ok(parsed.sign_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{signature::Signer, Signature as EsSignature, SigningKey as EsSigningKey};

    const RP: &str = "example.com";
    const ORIGIN: &str = "https://example.com";
    const CHALLENGE: &str = "test-challenge-value";

    fn expect() -> Expect<'static> {
        Expect {
            rp_id: RP,
            origin: ORIGIN,
            challenge: CHALLENGE,
        }
    }

    // --- tiny CBOR encoder (fixtures only) ---
    fn enc_head(major: u8, v: u64, out: &mut Vec<u8>) {
        let m = major << 5;
        if v < 24 {
            out.push(m | v as u8);
        } else if v <= u8::MAX as u64 {
            out.push(m | 24);
            out.push(v as u8);
        } else if v <= u16::MAX as u64 {
            out.push(m | 25);
            out.extend_from_slice(&(v as u16).to_be_bytes());
        } else {
            out.push(m | 26);
            out.extend_from_slice(&(v as u32).to_be_bytes());
        }
    }
    fn enc_int(v: i64, out: &mut Vec<u8>) {
        if v >= 0 {
            enc_head(0, v as u64, out);
        } else {
            enc_head(1, (-1 - v) as u64, out);
        }
    }
    fn enc_bytes(b: &[u8], out: &mut Vec<u8>) {
        enc_head(2, b.len() as u64, out);
        out.extend_from_slice(b);
    }
    fn enc_text(t: &str, out: &mut Vec<u8>) {
        enc_head(3, t.len() as u64, out);
        out.extend_from_slice(t.as_bytes());
    }
    fn enc_map_header(n: u64, out: &mut Vec<u8>) {
        enc_head(5, n, out);
    }

    fn es256_keypair() -> (EsSigningKey, Vec<u8>) {
        let signing = EsSigningKey::from_slice(&[7u8; 32]).unwrap();
        let verifying = signing.verifying_key();
        let point = verifying.to_encoded_point(false);
        let x = point.x().unwrap();
        let y = point.y().unwrap();
        let mut cose = Vec::new();
        enc_map_header(5, &mut cose);
        enc_int(1, &mut cose);
        enc_int(2, &mut cose);
        enc_int(3, &mut cose);
        enc_int(-7, &mut cose);
        enc_int(-1, &mut cose);
        enc_int(1, &mut cose);
        enc_int(-2, &mut cose);
        enc_bytes(x, &mut cose);
        enc_int(-3, &mut cose);
        enc_bytes(y, &mut cose);
        (signing, cose)
    }

    fn client_data(type_: &str, challenge: &str, origin: &str) -> Vec<u8> {
        format!(
            r#"{{"type":"{}","challenge":"{}","origin":"{}","crossOrigin":false}}"#,
            type_, challenge, origin
        )
        .into_bytes()
    }

    fn auth_data(flags: u8, sign_count: u32, attested: Option<(&[u8], &[u8])>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&Sha256::digest(RP.as_bytes()));
        out.push(flags);
        out.extend_from_slice(&sign_count.to_be_bytes());
        if let Some((cred_id, cose)) = attested {
            out.extend_from_slice(&[0u8; 16]); // AAGUID
            out.extend_from_slice(&(cred_id.len() as u16).to_be_bytes());
            out.extend_from_slice(cred_id);
            out.extend_from_slice(cose);
        }
        out
    }

    fn attestation(fmt: &str, auth: &[u8], stmt: Vec<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        enc_map_header(3, &mut out);
        enc_text("fmt", &mut out);
        enc_text(fmt, &mut out);
        enc_text("authData", &mut out);
        enc_bytes(auth, &mut out);
        enc_text("attStmt", &mut out);
        out.extend_from_slice(&stmt);
        out
    }

    fn empty_map() -> Vec<u8> {
        let mut v = Vec::new();
        enc_map_header(0, &mut v);
        v
    }

    #[test]
    fn attestation_none_registers_es256_credential() {
        let (_sk, cose) = es256_keypair();
        let auth = auth_data(0x41, 3, Some((b"cred-1", &cose)));
        let obj = attestation("none", &auth, empty_map());
        let cred = verify_attestation(
            &obj,
            &client_data("webauthn.create", CHALLENGE, ORIGIN),
            &expect(),
        )
        .unwrap();
        assert_eq!(cred.cred_id, b"cred-1");
        assert_eq!(cred.public_key_cose, cose);
        assert_eq!(cred.sign_count, 3);
    }

    #[test]
    fn attestation_packed_self_verifies_signature() {
        let (sk, cose) = es256_keypair();
        let auth = auth_data(0x41, 0, Some((b"cred-2", &cose)));
        let cd = client_data("webauthn.create", CHALLENGE, ORIGIN);
        let sig: EsSignature = sk.sign(&signed_message(&auth, &cd));
        let mut stmt = Vec::new();
        enc_map_header(2, &mut stmt);
        enc_text("alg", &mut stmt);
        enc_int(-7, &mut stmt);
        enc_text("sig", &mut stmt);
        enc_bytes(sig.to_der().as_bytes(), &mut stmt);
        let obj = attestation("packed", &auth, stmt);
        assert!(verify_attestation(&obj, &cd, &expect()).is_ok());
    }

    #[test]
    fn attestation_packed_with_x5c_is_rejected() {
        let (sk, cose) = es256_keypair();
        let auth = auth_data(0x41, 0, Some((b"cred-3", &cose)));
        let cd = client_data("webauthn.create", CHALLENGE, ORIGIN);
        let sig: EsSignature = sk.sign(&signed_message(&auth, &cd));
        let mut stmt = Vec::new();
        enc_map_header(3, &mut stmt);
        enc_text("alg", &mut stmt);
        enc_int(-7, &mut stmt);
        enc_text("sig", &mut stmt);
        enc_bytes(sig.to_der().as_bytes(), &mut stmt);
        enc_text("x5c", &mut stmt);
        enc_head(4, 1, &mut stmt);
        enc_bytes(b"not-a-cert", &mut stmt);
        let obj = attestation("packed", &auth, stmt);
        assert!(verify_attestation(&obj, &cd, &expect()).is_err());
    }

    #[test]
    fn attestation_rejects_wrong_challenge_origin_and_type() {
        let (_sk, cose) = es256_keypair();
        let auth = auth_data(0x41, 0, Some((b"cred-4", &cose)));
        let obj = attestation("none", &auth, empty_map());
        assert!(verify_attestation(
            &obj,
            &client_data("webauthn.create", "other", ORIGIN),
            &expect()
        )
        .is_err());
        assert!(verify_attestation(
            &obj,
            &client_data("webauthn.create", CHALLENGE, "https://evil.example.com"),
            &expect()
        )
        .is_err());
        assert!(verify_attestation(
            &obj,
            &client_data("webauthn.get", CHALLENGE, ORIGIN),
            &expect()
        )
        .is_err());
    }

    #[test]
    fn assertion_verifies_and_advances_count() {
        let (sk, cose) = es256_keypair();
        let auth = auth_data(0x01, 41, None);
        let cd = client_data("webauthn.get", CHALLENGE, ORIGIN);
        let sig: EsSignature = sk.sign(&signed_message(&auth, &cd));
        let new_count =
            verify_assertion(&cose, 40, &auth, &cd, sig.to_der().as_bytes(), &expect()).unwrap();
        assert_eq!(new_count, 41);
    }

    #[test]
    fn assertion_rejects_replayed_counter_and_bad_signature() {
        let (sk, cose) = es256_keypair();
        let auth = auth_data(0x01, 41, None);
        let cd = client_data("webauthn.get", CHALLENGE, ORIGIN);
        let sig: EsSignature = sk.sign(&signed_message(&auth, &cd));
        let der = sig.to_der();
        // Stored counter already at the presented value: clone/replay.
        assert!(verify_assertion(&cose, 41, &auth, &cd, der.as_bytes(), &expect()).is_err());
        // Tampered signature.
        let mut bad = der.as_bytes().to_vec();
        let last = bad.len() - 1;
        bad[last] ^= 0x01;
        assert!(verify_assertion(&cose, 40, &auth, &cd, &bad, &expect()).is_err());
        // Counterless authenticator (0/0) is allowed.
        let auth0 = auth_data(0x01, 0, None);
        let sig0: EsSignature = sk.sign(&signed_message(&auth0, &cd));
        assert_eq!(
            verify_assertion(&cose, 0, &auth0, &cd, sig0.to_der().as_bytes(), &expect()).unwrap(),
            0
        );
    }

    #[test]
    fn assertion_rejects_wrong_rp_and_missing_user_presence() {
        let (sk, cose) = es256_keypair();
        let cd = client_data("webauthn.get", CHALLENGE, ORIGIN);
        // UP flag cleared.
        let auth = auth_data(0x00, 5, None);
        let sig: EsSignature = sk.sign(&signed_message(&auth, &cd));
        assert!(
            verify_assertion(&cose, 0, &auth, &cd, sig.to_der().as_bytes(), &expect()).is_err()
        );
        // Signed for a different RP.
        let mut auth = Vec::new();
        auth.extend_from_slice(&Sha256::digest(b"other.example.com"));
        auth.push(0x01);
        auth.extend_from_slice(&5u32.to_be_bytes());
        let sig: EsSignature = sk.sign(&signed_message(&auth, &cd));
        assert!(
            verify_assertion(&cose, 0, &auth, &cd, sig.to_der().as_bytes(), &expect()).is_err()
        );
    }

    #[test]
    fn eddsa_assertion_verifies() {
        use ed25519_dalek::Signer;
        let signing = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let vk = signing.verifying_key();
        let mut cose = Vec::new();
        enc_map_header(4, &mut cose);
        enc_int(1, &mut cose);
        enc_int(1, &mut cose);
        enc_int(3, &mut cose);
        enc_int(-8, &mut cose);
        enc_int(-1, &mut cose);
        enc_int(6, &mut cose);
        enc_int(-2, &mut cose);
        enc_bytes(vk.as_bytes(), &mut cose);
        let auth = auth_data(0x01, 1, None);
        let cd = client_data("webauthn.get", CHALLENGE, ORIGIN);
        let sig = signing.sign(&signed_message(&auth, &cd));
        assert_eq!(
            verify_assertion(&cose, 0, &auth, &cd, &sig.to_bytes(), &expect()).unwrap(),
            1
        );
    }

    #[test]
    fn malformed_inputs_reject_without_panic() {
        let cd = client_data("webauthn.get", CHALLENGE, ORIGIN);
        for junk in [&b""[..], b"\xff\xff\xff", b"\xbf", b"\xa1\x61a"] {
            assert!(verify_attestation(junk, &cd, &expect()).is_err());
            assert!(verify_assertion(junk, 0, junk, &cd, junk, &expect()).is_err());
        }
        assert!(verify_assertion(b"\xa0", 0, &[0u8; 37], b"not json", b"sig", &expect()).is_err());
    }
}
