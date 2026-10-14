//! Signed-cookie sessions: HMAC-SHA256, server-secret from env, rotation.
//!
//! Threat-model §4: cookies are `HttpOnly; Secure; SameSite=Lax`, bounded
//! `Max-Age`, refreshed with a row counter and a fixed authentication-age
//! cap. Privilege changes require reauthentication and a new session id.
//! The server-side session row is authoritative: request authentication uses
//! `verify_stored_session`, not signature-only verification. Secret comes
//! from the Worker env, never code.

use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Clock skew tolerated when checking `iat` (seconds).
pub const CLOCK_SKEW_SECS: u64 = 60;
/// Absolute lifetime from authentication, not refreshed by rotation (one day).
pub const MAX_SESSION_SECS: u64 = 86_400;
/// Cookie name for the web session.
///
/// The `__Host-` prefix locks out `Domain=` overrides by construction:
/// a `__Host-` cookie is only accepted with `Path=/`, `Secure` and no
/// `Domain` attribute — all of which [`set_cookie_header`] already emits.
/// Never add `Domain=` to this cookie: browsers would reject it outright.
pub const COOKIE_NAME: &str = "__Host-nntune-session";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionClaims {
    /// Random per-session id (`jti`): joins the token to its server-side
    /// `sessions` row so a single session can be revoked without killing
    /// every session the user holds.
    pub session_id: String,
    pub user_id: String,
    /// Immutable authentication time, preserved across rotations.
    pub created_at: u64,
    pub issued_at: u64,
    pub expires_at: u64,
    pub rotation_counter: u32,
}

fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    crate::b64::encode(&bytes)
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

// Unpadded-base64url lives in one place: [`crate::b64`] over the `base64`
// crate (F-HARD-01). Decode errors map to [`SessionError::Invalid`] at each
// call site; nothing secret or distinguishing is ever echoed.
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
        created_at: now,
        issued_at: now,
        expires_at: now.saturating_add(ttl_secs.min(MAX_SESSION_SECS)),
        rotation_counter: 0,
    };
    let payload = serde_json::to_vec(&claims).expect("claims serialize");
    let payload_b64 = crate::b64::encode(&payload);
    let sig = sign(secret, &payload_b64);
    format!("{}.{}", payload_b64, crate::b64::encode(&sig))
}

/// Verify signature and time bounds only; this does NOT authenticate a
/// request. Use it to locate the session row, then require
/// [`verify_stored_session`] against that current row before authorization.
pub fn verify_session(secret: &[u8], token: &str, now: u64) -> Result<SessionClaims, SessionError> {
    let (payload_b64, sig_b64) = token.split_once('.').ok_or(SessionError::Invalid)?;
    let sig = crate::b64::decode(sig_b64).map_err(|_| SessionError::Invalid)?;
    let expected = sign(secret, payload_b64);
    if sig.len() != expected.len() || sig.ct_eq(&expected).unwrap_u8() != 1 {
        return Err(SessionError::Invalid);
    }
    let payload = crate::b64::decode(payload_b64).map_err(|_| SessionError::Invalid)?;
    let claims: SessionClaims =
        serde_json::from_slice(&payload).map_err(|_| SessionError::Invalid)?;
    if claims.user_id.is_empty() || claims.session_id.is_empty() {
        return Err(SessionError::Invalid);
    }
    if now >= claims.expires_at
        || now >= claims.created_at.saturating_add(MAX_SESSION_SECS)
        || claims.expires_at > claims.created_at.saturating_add(MAX_SESSION_SECS)
        || claims.created_at > claims.issued_at
        || claims.expires_at <= claims.issued_at
    {
        return Err(SessionError::Invalid);
    }
    if claims.issued_at > now.saturating_add(CLOCK_SKEW_SECS) {
        return Err(SessionError::Invalid);
    }
    Ok(claims)
}

/// Rotation-window verification: accept tokens signed by the current
/// `primary` secret OR the superseded `previous` secret (see
/// `docs/security/session-secret-rotation.md`). Outside a rotation window
/// callers pass the same secret twice or use [`verify_session`] directly —
/// a standing second secret would needlessly broaden the signing scope.
/// Both failures map to the same generic [`SessionError::Invalid`]; there
/// is no oracle for which secret matched.
pub fn verify_session_dual(
    primary: &[u8],
    previous: &[u8],
    token: &str,
    now: u64,
) -> Result<SessionClaims, SessionError> {
    verify_session(primary, token, now).or_else(|_| verify_session(previous, token, now))
}

/// Current server-side row from `sessions`; absence of a row must reject.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub user_id: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub rotation_counter: u32,
    pub revoked: bool,
}

/// Request authentication: reject revocation, stale rotations, mismatched
/// ownership and row expiry in addition to the signed-token lifetime cap.
pub fn verify_stored_session(
    secret: &[u8],
    token: &str,
    now: u64,
    record: &SessionRecord,
) -> Result<SessionClaims, SessionError> {
    let claims = verify_session(secret, token, now)?;
    if record.revoked
        || claims.session_id != record.id
        || claims.user_id != record.user_id
        || claims.created_at != record.created_at
        || claims.expires_at != record.expires_at
        || claims.rotation_counter != record.rotation_counter
        || now >= record.expires_at
    {
        return Err(SessionError::Invalid);
    }
    Ok(claims)
}

/// Prepare a refresh with `rotation_counter + 1`, without extending the
/// immutable authentication lifetime. This is NOT a database rotation:
/// first require [`verify_stored_session`], then atomically UPDATE the row
/// WHERE id/user_id/counter match AND revoked = FALSE, persisting the next
/// counter and expiry. Require exactly one row and commit before Set-Cookie.
/// Once committed, the previous token fails [`verify_stored_session`].
/// For privilege changes, revoke the old row and use [`issue_session`] after
/// reauthentication to create a NEW session id; never use refresh for that.
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
        created_at: claims.created_at,
        issued_at: now,
        expires_at: now
            .saturating_add(ttl_secs)
            .min(claims.created_at.saturating_add(MAX_SESSION_SECS)),
        rotation_counter: claims
            .rotation_counter
            .checked_add(1)
            .ok_or(SessionError::Invalid)?,
    };
    let payload = serde_json::to_vec(&next).expect("claims serialize");
    let payload_b64 = crate::b64::encode(&payload);
    let sig = sign(secret, &payload_b64);
    Ok(format!("{}.{}", payload_b64, crate::b64::encode(&sig)))
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

    #[test]
    fn dual_secret_accepts_previous_only_in_rotation_window() {
        let primary = b"primary-secret-00000000000000000000";
        let previous = b"previous-secret-0000000000000000000";
        let now = 1_700_000_000u64;
        // Token minted under the superseded secret verifies while the
        // previous secret is still configured ...
        let old_token = issue_session(previous, "user-1", now, 3600);
        assert!(verify_session(primary, &old_token, now + 60).is_err());
        assert!(verify_session_dual(primary, previous, &old_token, now + 60).is_ok());
        // ... new tokens verify under the primary alone ...
        let new_token = issue_session(primary, "user-1", now, 3600);
        assert!(verify_session_dual(primary, previous, &new_token, now + 60).is_ok());
        // ... and dropping the previous secret ends the window: old tokens
        // fail under the primary with no oracle for which secret matched.
        assert!(verify_session(primary, &old_token, now + 60).is_err());
        assert_eq!(
            verify_session_dual(
                b"wrong-1-00000000000000000000000",
                b"wrong-2-00000000000000000000000",
                &new_token,
                now + 60
            ),
            Err(SessionError::Invalid)
        );
    }
}
