//! Signed-cookie sessions: HMAC-SHA256, server-secret from env, rotation.
//!
//! Threat-model §4: cookies are `HttpOnly; Secure; SameSite=Lax`, bounded
//! `Max-Age`, rotated on privilege change. The server-side session row
//! (see migrations) is the revocation source; this module is the token
//! layer. Secret comes from the Worker env, never code.

use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Clock skew tolerated when checking `iat` (seconds).
pub const CLOCK_SKEW_SECS: u64 = 60;
/// Cookie name for the web session.
pub const COOKIE_NAME: &str = "nntune_session";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionClaims {
    /// Random per-session id (`jti`): joins the token to its server-side
    /// `sessions` row so a single session can be revoked without killing
    /// every session the user holds.
    pub session_id: String,
    pub user_id: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub rotation_counter: u32,
}

fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    b64url_encode(&bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    Invalid,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Generic: never leak why verification failed.
        write!(f, "invalid session")
    }
}

fn b64url_encode(bytes: &[u8]) -> String {
    base64_url_encode(bytes)
}

fn b64url_decode(s: &str) -> Result<Vec<u8>, SessionError> {
    base64_url_decode(s).map_err(|_| SessionError::Invalid)
}

// Minimal base64url without a new dependency (core-compatible).
fn base64_url_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        }
    }
    out
}

fn base64_url_decode(s: &str) -> Result<Vec<u8>, ()> {
    fn val(c: u8) -> Result<u32, ()> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'-' => Ok(62),
            b'_' => Ok(63),
            _ => Err(()),
        }
    }
    let bytes = s.as_bytes();
    if bytes.len() % 4 == 1 {
        return Err(());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        let mut present = 0usize;
        for &c in chunk {
            n = (n << 6) | val(c)?;
            present += 1;
        }
        // Pad the final quantum with zero bits.
        n <<= 6 * (4 - present);
        out.push(((n >> 16) & 0xFF) as u8);
        if present > 2 {
            out.push(((n >> 8) & 0xFF) as u8);
        }
        if present > 3 {
            out.push((n & 0xFF) as u8);
        }
    }
    Ok(out)
}

fn sign(secret: &[u8], payload_b64: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(payload_b64.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Issue a new session token for `user_id` valid for `ttl_secs` from `now`.
pub fn issue_session(secret: &[u8], user_id: &str, now: u64, ttl_secs: u64) -> String {
    let claims = SessionClaims {
        session_id: new_session_id(),
        user_id: user_id.to_string(),
        issued_at: now,
        expires_at: now.saturating_add(ttl_secs),
        rotation_counter: 0,
    };
    let payload = serde_json::to_vec(&claims).expect("claims serialize");
    let payload_b64 = b64url_encode(&payload);
    let sig = sign(secret, &payload_b64);
    format!("{}.{}", payload_b64, b64url_encode(&sig))
}

/// Verify a token at time `now`. Generic error: no reason is exposed.
pub fn verify_session(secret: &[u8], token: &str, now: u64) -> Result<SessionClaims, SessionError> {
    let (payload_b64, sig_b64) = token.split_once('.').ok_or(SessionError::Invalid)?;
    let sig = b64url_decode(sig_b64)?;
    let expected = sign(secret, payload_b64);
    if sig.len() != expected.len() || sig.ct_eq(&expected).unwrap_u8() != 1 {
        return Err(SessionError::Invalid);
    }
    let payload = b64url_decode(payload_b64)?;
    let claims: SessionClaims =
        serde_json::from_slice(&payload).map_err(|_| SessionError::Invalid)?;
    if claims.user_id.is_empty() || claims.session_id.is_empty() {
        return Err(SessionError::Invalid);
    }
    if now > claims.expires_at {
        return Err(SessionError::Invalid);
    }
    if claims.issued_at > now.saturating_add(CLOCK_SKEW_SECS) {
        return Err(SessionError::Invalid);
    }
    Ok(claims)
}

/// Rotate: verify the old token, re-issue with `rotation_counter + 1`.
pub fn rotate_session(
    secret: &[u8],
    token: &str,
    now: u64,
    ttl_secs: u64,
) -> Result<String, SessionError> {
    let claims = verify_session(secret, token, now)?;
    let next = SessionClaims {
        session_id: claims.session_id,
        user_id: claims.user_id,
        issued_at: now,
        expires_at: now.saturating_add(ttl_secs),
        rotation_counter: claims.rotation_counter.saturating_add(1),
    };
    let payload = serde_json::to_vec(&next).expect("claims serialize");
    let payload_b64 = b64url_encode(&payload);
    let sig = sign(secret, &payload_b64);
    Ok(format!("{}.{}", payload_b64, b64url_encode(&sig)))
}

/// `Set-Cookie` header value. Always HttpOnly + Secure + SameSite=Lax.
pub fn set_cookie_header(token: &str, max_age_secs: u64) -> String {
    format!(
        "{}={}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={}",
        COOKIE_NAME, token, max_age_secs
    )
}

/// Expired-cookie header for logout / revoke.
pub fn clear_cookie_header() -> String {
    format!(
        "{}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0",
        COOKIE_NAME
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_tokens_rejected() {
        let secret = b"0123456789abcdef0123456789abcdef";
        assert!(verify_session(secret, "no-dot", 0).is_err());
        assert!(verify_session(secret, "!!!.@@@", 0).is_err());
        assert!(verify_session(secret, "", 0).is_err());
    }
}
