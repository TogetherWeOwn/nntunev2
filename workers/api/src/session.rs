//! Signed-cookie sessions: HMAC-SHA256, server-secret from env, rotation.
//!
//! Threat-model §4: cookies are `HttpOnly; Secure; SameSite=Lax`, bounded
//! `Max-Age`, refreshed with a row counter and a fixed authentication-age
//! cap. Privilege changes require reauthentication and a new session id.
//! The server-side session row is authoritative: request authentication uses
//! `verify_stored_session`, not signature-only verification. Secret comes
//! from the Worker env, never code.

use hmac::{Hmac, KeyInit, Mac};
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

/// Pinned cookie scope. `__Host-` REQUIRES `Path=/` exactly: any narrower
/// path makes browsers reject the cookie, and any `Domain=` does the same.
/// Both header builders emit this constant and never a `Domain=`, so the
/// cookie is host-only (exact origin host, never subdomains) by
/// construction — scope pinning cannot drift per call site.
pub const COOKIE_PATH: &str = "/";

/// Route the Worker entry wires single-session logout to:
/// `POST /v1/session/logout` → authenticate with [`verify_stored_session`],
/// [`logout_session_atomically`], then send [`clear_cookie_header`].
/// POST only: a state-changing GET logout is a cross-site request-forgery
/// logout vector (`SameSite=Lax` still sends the cookie on top-level GET).
pub const LOGOUT_PATH: &str = "/v1/session/logout";
/// Route the Worker entry wires logout-everywhere to:
/// `POST /v1/session/revoke-all` → authenticate with
/// [`verify_stored_session`], [`logout_everywhere_atomically`], then send
/// [`clear_cookie_header`]. POST only, same reason as [`LOGOUT_PATH`].
pub const LOGOUT_EVERYWHERE_PATH: &str = "/v1/session/revoke-all";
/// Audit-log action for single-session logout (ids/results only, never
/// token material — see `vault::audit_event`).
pub const LOGOUT_AUDIT_ACTION: &str = "session.logout";
/// Audit-log action for logout-everywhere.
pub const LOGOUT_EVERYWHERE_AUDIT_ACTION: &str = "session.revoke_all";

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
    getrandom::fill(&mut bytes).expect("secure randomness unavailable");
    crate::b64::encode(&bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    Invalid,
    Misconfigured,
    /// The revoke did not commit (storage failure). The handler must NOT
    /// clear the session cookie: return a retryable 5xx so the caller keeps
    /// the only credential that can authenticate a retry. Carries the
    /// verified `user_id` so the failed revoke can be audited.
    Storage {
        user_id: String,
    },
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Generic: never leak why verification failed.
        match self {
            Self::Invalid => write!(f, "invalid session"),
            Self::Misconfigured => write!(f, "session service unavailable"),
            // Distinct from Invalid so the handler can return a retryable
            // 5xx without clearing the cookie; carries no secret material.
            Self::Storage { .. } => write!(f, "session store unavailable"),
        }
    }
}

// Unpadded-base64url lives in one place: [`crate::b64`] over the `base64`
// crate (F-HARD-01). Decode errors map to [`SessionError::Invalid`] at each
// call site; nothing secret or distinguishing is ever echoed.

/// Minimum HMAC secret length (32 bytes / 256 bits). Every signing and
/// verification entry point enforces it and fails closed: an unset env var
/// that decodes to an empty slice, or any truncated secret, is
/// [`SessionError::Misconfigured`] — never a working weak key.
pub const MIN_SECRET_LEN: usize = 32;

fn check_secret_len(secret: &[u8]) -> Result<(), SessionError> {
    if secret.len() < MIN_SECRET_LEN {
        return Err(SessionError::Misconfigured);
    }
    Ok(())
}

/// Secrets accepted for one verification call: exactly one primary plus an
/// optional previous, never a list or version chain. The previous secret is
/// verify-only and exists only inside a recorded rotation window; steady
/// state is [`SessionKeys::single`]. See
/// `docs/security/session-secret-rotation.md`.
#[derive(Clone, Copy)]
pub struct SessionKeys<'a> {
    pub primary: &'a [u8],
    pub previous: Option<&'a [u8]>,
}

// Manual Debug impl: the HMAC secrets must never appear in `{:#?}`
// output, `dbg!`, tracing fields, or assert messages (threat-model §3:
// no log line may contain credential material; CI logs are
// attacker-readable). Only secret lengths and rotation presence print.
impl<'a> std::fmt::Debug for SessionKeys<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionKeys")
            .field("primary_len", &self.primary.len())
            .field("has_previous", &self.previous.is_some())
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl<'a> SessionKeys<'a> {
    /// Steady state: one secret mints and verifies.
    pub fn single(primary: &'a [u8]) -> Self {
        SessionKeys {
            primary,
            previous: None,
        }
    }

    /// Rotation window: the primary mints; both verify. The caller passes
    /// `None` (via [`SessionKeys::single`]) when no rotation is recorded —
    /// an empty or short previous secret fails closed instead of silently
    /// widening or narrowing the window.
    pub fn rotation(primary: &'a [u8], previous: &'a [u8]) -> Self {
        SessionKeys {
            primary,
            previous: Some(previous),
        }
    }
}

fn sign(secret: &[u8], payload_b64: &str) -> Result<Vec<u8>, SessionError> {
    check_secret_len(secret)?;
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| SessionError::Invalid)?;
    mac.update(payload_b64.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

/// Issue a new session token for `user_id` valid for `ttl_secs` from `now`.
/// Fails closed on a short secret — callers surface the error, never a
/// token minted under a weak key.
pub fn issue_session(
    secret: &[u8],
    user_id: &str,
    now: u64,
    ttl_secs: u64,
) -> Result<String, SessionError> {
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
    let sig = sign(secret, &payload_b64)?;
    Ok(format!("{}.{}", payload_b64, crate::b64::encode(&sig)))
}

/// Verify signature and time bounds only; this does NOT authenticate a
/// request. Use it to locate the session row, then require
/// [`verify_stored_session`] against that current row before authorization.
/// Short secrets fail closed (see [`MIN_SECRET_LEN`]).
pub fn verify_session(secret: &[u8], token: &str, now: u64) -> Result<SessionClaims, SessionError> {
    check_secret_len(secret)?;
    let (payload_b64, sig_b64) = token.split_once('.').ok_or(SessionError::Invalid)?;
    let sig = crate::b64::decode(sig_b64).map_err(|_| SessionError::Invalid)?;
    let expected = sign(secret, payload_b64)?;
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
/// callers pass [`SessionKeys::single`] — a standing second secret would
/// needlessly broaden the signing scope. Both secrets are length-checked
/// up front, so a misconfigured short `previous` fails the whole call
/// closed instead of silently narrowing the window. A bad token maps to
/// [`SessionError::Invalid`]; a short configured secret maps to
/// [`SessionError::Misconfigured`] before token verification.
pub fn verify_session_dual(
    keys: &SessionKeys<'_>,
    token: &str,
    now: u64,
) -> Result<SessionClaims, SessionError> {
    check_secret_len(keys.primary)?;
    if let Some(previous) = keys.previous {
        check_secret_len(previous)?;
    }
    verify_session(keys.primary, token, now).or_else(|_| match keys.previous {
        Some(previous) => verify_session(previous, token, now),
        None => Err(SessionError::Invalid),
    })
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
    keys: &SessionKeys<'_>,
    token: &str,
    now: u64,
    record: &SessionRecord,
) -> Result<SessionClaims, SessionError> {
    let claims = verify_session_dual(keys, token, now)?;
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
    keys: &SessionKeys<'_>,
    token: &str,
    now: u64,
    ttl_secs: u64,
) -> Result<String, SessionError> {
    // Dual path: a pre-cutover token verifies here and is re-signed below
    // with the primary, so rotation migrates sessions onto the new secret.
    let claims = verify_session_dual(keys, token, now)?;
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
    // Always re-sign with the primary: rotation migrates, never mints
    // under the previous secret.
    let sig = sign(keys.primary, &payload_b64)?;
    Ok(format!("{}.{}", payload_b64, crate::b64::encode(&sig)))
}

/// `Set-Cookie` header value. Always HttpOnly + Secure + SameSite=Lax,
/// pinned to [`COOKIE_PATH`] with no `Domain=` (the `__Host-` contract).
/// `Max-Age` is clamped to [`MAX_SESSION_SECS`]: no caller-supplied value
/// may mint a cookie that outlives the absolute token lifetime.
pub fn set_cookie_header(token: &str, max_age_secs: u64) -> String {
    format!(
        "{}={}; Path={}; HttpOnly; Secure; SameSite=Lax; Max-Age={}",
        COOKIE_NAME,
        token,
        COOKIE_PATH,
        max_age_secs.min(MAX_SESSION_SECS)
    )
}

/// Expired-cookie header for logout / revoke. Same pinned name, path and
/// flags as [`set_cookie_header`] — a mismatched `Path` would leave the
/// live cookie in the browser.
pub fn clear_cookie_header() -> String {
    format!(
        "{}=; Path={}; HttpOnly; Secure; SameSite=Lax; Max-Age=0",
        COOKIE_NAME, COOKIE_PATH
    )
}

// --- Revocation: single-session logout + logout-everywhere -------------------

/// Pure snapshot preparation for single-session logout, NOT a database
/// revocation. Mutating an in-memory row revokes nothing server-side:
/// HTTP callers must use [`logout_session_atomically`], which requires the
/// committed conditional UPDATE. This helper exists for fixtures and for
/// preparing the post-revoke row shape. Idempotent: revoking an already
/// revoked row is a no-op, never an un-revoke.
pub fn revoke_session(record: &mut SessionRecord) {
    record.revoked = true;
}

/// Pure snapshot preparation for logout-everywhere, NOT a database
/// revocation (see [`revoke_session`]). Revokes every row in `records`
/// owned by `user_id`, returning how many rows transitioned from active
/// to revoked. Fail closed: an empty `user_id` revokes nothing, so a bug
/// upstream of the route can never turn into a mass revocation. Already
/// revoked rows are left revoked and not counted again.
pub fn revoke_all_sessions(records: &mut [SessionRecord], user_id: &str) -> usize {
    if user_id.is_empty() {
        return 0;
    }
    let mut transitioned = 0;
    for record in records.iter_mut() {
        if record.user_id == user_id && !record.revoked {
            record.revoked = true;
            transitioned += 1;
        }
    }
    transitioned
}

/// A validated candidate for one atomic, parameterized Postgres revocation.
/// Bind the listed parameters; never interpolate a record into SQL.
pub enum SessionRevokeClaim<'a> {
    /// Parameters: id (the verified token's `session_id`), user_id.
    /// `user_id` is bound as well as `id` so a row can only ever be revoked
    /// under its owner, never by presenting another user's session id.
    Single {
        session_id: &'a str,
        user_id: &'a str,
    },
    /// Parameter: user_id (from the verified token, never the request).
    AllForUser { user_id: &'a str },
}

impl SessionRevokeClaim<'_> {
    /// Execute this statement once and count its returned rows. The
    /// `revoked = FALSE` guard makes the returned count exactly the rows
    /// this call transitioned, so concurrent logouts cannot double-count.
    pub fn statement(&self) -> &'static str {
        match self {
            Self::Single { .. } => "UPDATE sessions SET revoked = TRUE WHERE id = $1 AND user_id = $2 AND revoked = FALSE RETURNING id",
            Self::AllForUser { .. } => "UPDATE sessions SET revoked = TRUE WHERE user_id = $1 AND revoked = FALSE RETURNING id",
        }
    }

    /// Values to bind, in `$1, $2` order. The Worker adapter binds from this
    /// method so the statement text and the bound values cannot drift apart;
    /// tests assert these values with a `session_id` that differs from the
    /// `user_id` so a swapped bind fails.
    pub fn params(&self) -> Vec<&str> {
        match self {
            Self::Single {
                session_id,
                user_id,
            } => vec![*session_id, *user_id],
            Self::AllForUser { user_id } => vec![*user_id],
        }
    }
}

/// Worker/Hyperdrive boundary. Implementations must execute the conditional
/// UPDATE from [`SessionRevokeClaim::statement`], binding
/// [`SessionRevokeClaim::params`] in order, and return the number of rows it
/// returned, NOT perform SELECT + unconditional UPDATE. Return
/// [`SessionError::Storage`] (with the claim's `user_id`) on storage
/// failure. Revocation is effective only after the transaction commits; the
/// response (including [`clear_cookie_header`]) may be sent only after
/// commit. On storage failure the handler must NOT clear the cookie — the
/// other sessions are still live and the caller needs its cookie to retry —
/// so it returns a retryable 5xx instead. An in-memory flag is not an
/// implementation.
pub trait SessionRevokeStore {
    fn revoke(
        &self,
        claim: SessionRevokeClaim<'_>,
    ) -> impl std::future::Future<Output = Result<u64, SessionError>>;
}

/// Single-session logout ([`LOGOUT_PATH`]): authenticate the presented
/// cookie against its current row, then revoke exactly that row.
/// Zero transitioned rows (a concurrent revoke won the race) or multiple
/// rows fail closed with [`SessionError::Invalid`]; the cookie is already
/// dead server-side (or the token was invalid), so the handler still sends
/// [`clear_cookie_header`]. A store failure returns
/// [`SessionError::Storage`] with the verified `user_id` instead: the revoke
/// did not commit, so the handler must NOT clear the cookie and must return
/// a retryable 5xx. Returns the verified claims so the handler can
/// audit-log [`LOGOUT_AUDIT_ACTION`] under the token's `user_id` — never a
/// request-supplied id.
pub async fn logout_session_atomically(
    keys: &SessionKeys<'_>,
    token: &str,
    now: u64,
    record: &SessionRecord,
    store: &impl SessionRevokeStore,
) -> Result<SessionClaims, SessionError> {
    let claims = verify_stored_session(keys, token, now, record)?;
    let transitioned = store
        .revoke(SessionRevokeClaim::Single {
            session_id: &claims.session_id,
            user_id: &claims.user_id,
        })
        .await
        .map_err(|_| SessionError::Storage {
            user_id: claims.user_id.clone(),
        })?;
    if transitioned != 1 {
        return Err(SessionError::Invalid);
    }
    Ok(claims)
}

/// Logout-everywhere ([`LOGOUT_EVERYWHERE_PATH`]): authenticate the
/// presented cookie against its current row, then revoke EVERY session row
/// for that token's `user_id`, including the caller's own. Afterwards all
/// pre-existing cookies for the user fail [`verify_stored_session`]
/// against their (now revoked) rows; a fresh login still works — this is
/// revocation, not a ban. Returns how many rows transitioned, for the
/// [`LOGOUT_EVERYWHERE_AUDIT_ACTION`] audit event. Zero transitioned rows
/// fails closed with [`SessionError::Invalid`]: the caller's own
/// just-verified row must transition, so zero means a concurrent revoke
/// already did the job — every cookie for the user is dead server-side, and
/// the handler still sends [`clear_cookie_header`]. A store failure returns
/// [`SessionError::Storage`] with the verified `user_id` instead: nothing
/// was revoked, so the handler must NOT clear the caller's cookie and must
/// return a retryable 5xx (the caller needs its cookie to retry, and the
/// failure is audited under the returned `user_id`). The `user_id` revoked
/// is always the verified token's; [`verify_session`] rejects empty user
/// ids, so this path can never mass-revoke on a missing id.
pub async fn logout_everywhere_atomically(
    keys: &SessionKeys<'_>,
    token: &str,
    now: u64,
    record: &SessionRecord,
    store: &impl SessionRevokeStore,
) -> Result<u64, SessionError> {
    let claims = verify_stored_session(keys, token, now, record)?;
    let transitioned = store
        .revoke(SessionRevokeClaim::AllForUser {
            user_id: &claims.user_id,
        })
        .await
        .map_err(|_| SessionError::Storage {
            user_id: claims.user_id.clone(),
        })?;
    if transitioned == 0 {
        return Err(SessionError::Invalid);
    }
    Ok(transitioned)
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
        let window = SessionKeys::rotation(primary, previous);
        // Token minted under the superseded secret verifies while the
        // previous secret is still configured ...
        let old_token = issue_session(previous, "user-1", now, 3600).expect("issue");
        assert!(verify_session(primary, &old_token, now + 60).is_err());
        assert!(verify_session_dual(&window, &old_token, now + 60).is_ok());
        // ... new tokens verify under the primary alone ...
        let new_token = issue_session(primary, "user-1", now, 3600).expect("issue");
        assert!(verify_session_dual(&window, &new_token, now + 60).is_ok());
        // ... and dropping the previous secret ends the window: old tokens
        // fail under the primary with no oracle for which secret matched.
        assert!(verify_session(primary, &old_token, now + 60).is_err());
        let wrong = SessionKeys::rotation(
            b"wrong-1-000000000000000000000000",
            b"wrong-2-000000000000000000000000",
        );
        assert_eq!(
            verify_session_dual(&wrong, &new_token, now + 60),
            Err(SessionError::Invalid)
        );
    }

    fn record_for(claims: &SessionClaims) -> SessionRecord {
        SessionRecord {
            id: claims.session_id.clone(),
            user_id: claims.user_id.clone(),
            created_at: claims.created_at,
            expires_at: claims.expires_at,
            rotation_counter: claims.rotation_counter,
            revoked: false,
        }
    }

    #[test]
    fn rotation_window_end_to_end_through_row_check() {
        // The documented rotation flow: a pre-cutover token authenticates
        // through verify_stored_session, refreshes onto the primary via
        // rotate_session, and dies when the previous secret is dropped.
        let primary = b"primary-secret-00000000000000000000";
        let previous = b"previous-secret-0000000000000000000";
        let now = 1_700_000_000u64;
        let window = SessionKeys::rotation(primary, previous);

        let old_token = issue_session(previous, "user-1", now, 3600).expect("issue");
        let record =
            record_for(&verify_session_dual(&window, &old_token, now + 60).expect("dual verifies"));
        // Step 3 of the runbook: the pre-cutover cookie authenticates.
        let claims =
            verify_stored_session(&window, &old_token, now + 60, &record).expect("row check");
        assert_eq!(claims.rotation_counter, 0);
        // Refresh migrates the session onto the primary signature ...
        let rotated = rotate_session(&window, &old_token, now + 120, 3600).expect("rotate");
        let rotated_claims =
            verify_session(primary, &rotated, now + 180).expect("primary alone verifies");
        assert_eq!(rotated_claims.rotation_counter, 1);
        assert_eq!(rotated_claims.session_id, claims.session_id);
        // ... and dropping the previous secret ends the window: the old
        // token fails the row check under the primary alone.
        let steady = SessionKeys::single(primary);
        assert!(verify_stored_session(&steady, &old_token, now + 180, &record).is_err());
    }

    #[test]
    fn short_or_empty_secrets_fail_closed() {
        let primary = b"primary-secret-00000000000000000000";
        let now = 1_700_000_000u64;
        // Minting under a weak key is refused, never issued.
        assert!(issue_session(b"", "victim", now, 3600).is_err());
        assert!(issue_session(b"short", "victim", now, 3600).is_err());
        assert!(verify_session(b"", "x.y", now).is_err());
        // An unset previous secret (empty slice) fails the whole call:
        // a token minted under the empty key never verifies.
        let empty_prev = SessionKeys::rotation(primary, b"");
        assert!(verify_session_dual(&empty_prev, "x.y", now).is_err());
        // The primary itself is length-checked even when a previous exists.
        let short_primary = SessionKeys::rotation(b"short", b"");
        assert!(verify_session_dual(&short_primary, "x.y", now).is_err());
    }

    #[test]
    fn cookie_headers_pin_path_and_clamp_max_age() {
        assert_eq!(COOKIE_PATH, "/", "__Host- requires Path=/ exactly");
        let set = set_cookie_header("tok", u64::MAX);
        assert_eq!(
            set,
            format!(
                "{COOKIE_NAME}=tok; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={MAX_SESSION_SECS}"
            ),
            "over-cap Max-Age clamps to the absolute lifetime"
        );
        assert!(set_cookie_header("tok", 60).ends_with("Max-Age=60"));
        let cleared = clear_cookie_header();
        assert_eq!(
            cleared,
            format!("{COOKIE_NAME}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0"),
            "clearing header must pin the same name/path/flags"
        );
        for header in [&set, &cleared] {
            assert!(!header.contains("Domain="), "never Domain=: {header}");
            assert!(!header.contains("SameSite=None"), "never None: {header}");
        }
    }

    #[test]
    fn revoke_all_sessions_empty_user_revokes_nothing_including_empty_owned_rows() {
        // Pins the fail-closed empty-user guard: the fixture includes a row
        // whose `user_id` IS empty, the only case the guard observably
        // changes. Deleting the guard revokes that row and fails this test.
        let mut records = vec![
            SessionRecord {
                id: "sess-a".to_string(),
                user_id: "user-1".to_string(),
                created_at: 1_700_000_000,
                expires_at: 1_700_003_600,
                rotation_counter: 0,
                revoked: false,
            },
            SessionRecord {
                id: "sess-empty-owner".to_string(),
                user_id: String::new(),
                created_at: 1_700_000_000,
                expires_at: 1_700_003_600,
                rotation_counter: 0,
                revoked: false,
            },
        ];
        assert_eq!(revoke_all_sessions(&mut records, ""), 0);
        assert!(
            records.iter().all(|record| !record.revoked),
            "an empty user_id revokes nothing, not even empty-owned rows"
        );
        // Sanity: a real user id still revokes only its own rows.
        assert_eq!(revoke_all_sessions(&mut records, "user-1"), 1);
        assert!(records[0].revoked);
        assert!(!records[1].revoked);
    }

    // The only awaited operation is our immediately-ready in-memory store.
    fn ready<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(result) => result,
            std::task::Poll::Pending => {
                panic!("deterministic storage fake unexpectedly awaited external work")
            }
        }
    }

    struct RecordingRevokeStore {
        outcome: Result<u64, SessionError>,
        calls: std::cell::RefCell<Vec<(&'static str, Vec<String>)>>,
    }

    impl SessionRevokeStore for RecordingRevokeStore {
        async fn revoke(&self, claim: SessionRevokeClaim<'_>) -> Result<u64, SessionError> {
            self.calls.borrow_mut().push((
                claim.statement(),
                claim.params().into_iter().map(str::to_owned).collect(),
            ));
            self.outcome.clone()
        }
    }

    #[test]
    fn atomic_logout_requires_verified_row_and_one_transition() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let keys = SessionKeys::single(secret);
        let now = 1_700_000_000u64;
        let token = issue_session(secret, "user-1", now, 3600).expect("issue");
        let record = record_for(&verify_session(secret, &token, now).expect("verifies"));

        // Exactly one transitioned row: the single-logout claim binds both
        // id and owner, and the verified claims come back for audit.
        let store = RecordingRevokeStore {
            outcome: Ok(1),
            calls: std::cell::RefCell::new(Vec::new()),
        };
        let claims = ready(logout_session_atomically(
            &keys,
            &token,
            now + 60,
            &record,
            &store,
        ))
        .expect("logout");
        assert_eq!(claims.user_id, "user-1");
        assert_ne!(
            claims.session_id, claims.user_id,
            "fixture ids must differ so a swapped bind is visible"
        );
        let calls = store.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0]
                .0
                .contains("id = $1 AND user_id = $2 AND revoked = FALSE"),
            "single revoke is owner-bound and transition-only: {}",
            calls[0].0
        );
        assert_eq!(
            calls[0].1,
            vec![claims.session_id.clone(), claims.user_id.clone()],
            "single revoke must bind the verified session_id then user_id"
        );
        drop(calls);

        // Zero / multiple rows fail closed with the generic error ...
        for outcome in [Ok(0), Ok(2)] {
            let store = RecordingRevokeStore {
                outcome: outcome.clone(),
                calls: std::cell::RefCell::new(Vec::new()),
            };
            assert_eq!(
                ready(logout_session_atomically(
                    &keys,
                    &token,
                    now + 60,
                    &record,
                    &store
                )),
                Err(SessionError::Invalid),
                "outcome {outcome:?} must fail closed"
            );
        }
        // ... while a store failure keeps the verified user for audit and
        // must not be mistaken for an invalid session. Both arms below pin
        // the `map_err` normalization: the fake returns something OTHER than
        // the expected value, so deleting the conversion fails the test.
        for outcome in [
            Err(SessionError::Invalid),
            Err(SessionError::Storage {
                user_id: "intruder-wrong-user".to_string(),
            }),
        ] {
            let store = RecordingRevokeStore {
                outcome,
                calls: std::cell::RefCell::new(Vec::new()),
            };
            assert_eq!(
                ready(logout_session_atomically(
                    &keys,
                    &token,
                    now + 60,
                    &record,
                    &store
                )),
                Err(SessionError::Storage {
                    user_id: "user-1".to_string()
                }),
                "store failure must normalize to Storage with the verified user"
            );
        }

        // An already-revoked row never reaches the store: authentication
        // rejects it first, so a replayed logout grants nothing.
        let revoked = SessionRecord {
            revoked: true,
            ..record.clone()
        };
        let store = RecordingRevokeStore {
            outcome: Ok(1),
            calls: std::cell::RefCell::new(Vec::new()),
        };
        assert!(ready(logout_session_atomically(
            &keys,
            &token,
            now + 60,
            &revoked,
            &store
        ))
        .is_err());
        assert!(
            store.calls.borrow().is_empty(),
            "unauthenticated logout must not touch the store"
        );
    }

    #[test]
    fn atomic_logout_everywhere_revokes_by_verified_user_only() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let keys = SessionKeys::single(secret);
        let now = 1_700_000_000u64;
        let token = issue_session(secret, "user-1", now, 3600).expect("issue");
        let record = record_for(&verify_session(secret, &token, now).expect("verifies"));

        // Any positive transition count succeeds (the user's other devices
        // are included) and is returned for the audit event.
        let store = RecordingRevokeStore {
            outcome: Ok(3),
            calls: std::cell::RefCell::new(Vec::new()),
        };
        assert_eq!(
            ready(logout_everywhere_atomically(
                &keys,
                &token,
                now + 60,
                &record,
                &store
            )),
            Ok(3)
        );
        let calls = store.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].0.contains("user_id = $1 AND revoked = FALSE")
                && !calls[0].0.contains("WHERE id ="),
            "revoke-all is scoped to the verified owner, not one row: {}",
            calls[0].0
        );
        assert_eq!(
            calls[0].1,
            vec![record.user_id.clone()],
            "revoke-all must bind the verified token's user_id"
        );
        drop(calls);

        // Zero transitions fail closed: the caller's own just-verified row
        // must be among the revoked.
        {
            let store = RecordingRevokeStore {
                outcome: Ok(0),
                calls: std::cell::RefCell::new(Vec::new()),
            };
            assert_eq!(
                ready(logout_everywhere_atomically(
                    &keys,
                    &token,
                    now + 60,
                    &record,
                    &store
                )),
                Err(SessionError::Invalid),
                "zero transitions must fail closed"
            );
        }
        // A store failure keeps the verified user for audit (retryable 5xx
        // without clearing the cookie), never the generic error. Both arms
        // pin the `map_err` normalization: the fake returns something OTHER
        // than the expected value, so deleting the conversion fails the test.
        for outcome in [
            Err(SessionError::Invalid),
            Err(SessionError::Storage {
                user_id: "intruder-wrong-user".to_string(),
            }),
        ] {
            let store = RecordingRevokeStore {
                outcome,
                calls: std::cell::RefCell::new(Vec::new()),
            };
            assert_eq!(
                ready(logout_everywhere_atomically(
                    &keys,
                    &token,
                    now + 60,
                    &record,
                    &store
                )),
                Err(SessionError::Storage {
                    user_id: "user-1".to_string()
                }),
                "store failure must normalize to Storage with the verified user"
            );
        }

        // A token for one user presented with another user's row never
        // reaches the store (verify_stored_session binds the pair), so
        // nobody can revoke another user's sessions.
        let foreign = SessionRecord {
            user_id: "user-2".to_string(),
            ..record.clone()
        };
        let store = RecordingRevokeStore {
            outcome: Ok(9),
            calls: std::cell::RefCell::new(Vec::new()),
        };
        assert!(ready(logout_everywhere_atomically(
            &keys,
            &token,
            now + 60,
            &foreign,
            &store
        ))
        .is_err());
        assert!(store.calls.borrow().is_empty());
    }

    #[test]
    fn debug_redacts_session_secrets() {
        // Threat-model §3: no log line may contain credential material.
        // SessionKeys holds the raw session HMAC secrets; a `{:#?}`/
        // `dbg!`/assert dump must not carry the secret bytes.
        let primary = b"PRIMARY-SECRET-0123456789abcdef00";
        let previous = b"PREVIOUS-SECRET-0123456789abcdef0";
        for keys in [
            SessionKeys::single(primary),
            SessionKeys::rotation(primary, previous),
        ] {
            for dumped in [format!("{keys:?}"), format!("{keys:#?}")] {
                assert!(
                    !dumped.contains("PRIMARY-SECRET"),
                    "primary secret leaked: {dumped}"
                );
                assert!(
                    !dumped.contains("PREVIOUS-SECRET"),
                    "previous secret leaked: {dumped}"
                );
                assert!(
                    dumped.contains("<redacted>"),
                    "no redaction marker: {dumped}"
                );
            }
        }
    }
}
