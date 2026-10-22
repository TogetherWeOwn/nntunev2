//! Scoped native REST credentials: minting and enforcement primitives.
//!
//! Design source: ADR 0010 (`docs/adr/0010-native-rest-credentials.md`),
//! which resolves the ADR 0006 deferral. Scope names, the route map, the
//! `ntk_` display form, HMAC-SHA256 storage, the 365-day cap, and the
//! session-only action set all come from that ADR — this module implements
//! them, it does not redefine them.
//!
//! Pure Rust, native + wasm32, no Worker runtime imports — the same
//! constraint as `auth` / `session` / `vault` / `rate_limit`. The HTTP
//! layer (issuance endpoints, bearer gate wiring) calls these primitives:
//! [`issue_credential`] to mint, [`authorize`] + [`scope_for_operation`] +
//! [`extract_bearer_key`] to enforce, [`revoke_credential`] /
//! [`revoke_all_for_user`] to revoke.
//!
//! Security properties (see ADR 0010 threat model):
//!
//! - The display secret is shown once at creation and never stored: what
//!   persists is an HMAC-SHA256 over the secret plus a short non-secret
//!   prefix for listing. [`NativeRestRecord`] and [`IssuedCredential`]
//!   carry manual `Debug` impls that name routing facts (id, prefix,
//!   scopes) but never the secret or the HMAC.
//! - Every credential-validity failure maps to [`CredentialError::Unauthenticated`]
//!   (unknown/tampered secret, unknown pepper version, owner mismatch,
//!   revoked, expired) so no oracle distinguishes them. A valid credential
//!   lacking the required scope maps to [`CredentialError::Forbidden`].
//! - [`authorize`] takes no cookie/session input by design: the HTTP layer
//!   must not consult cookies on bearer routes (no session-cookie
//!   fallback). There is intentionally no query-string parser either:
//!   query strings land in logs, so the HTTP layer must never read
//!   credentials from the query.
//! - Error codes reuse the repo taxonomy (`docs/api-errors.md`):
//!   `unauthorized` / `forbidden`. No new codes.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Catalog reads: artists, releases, tracks, search, library, playlist
/// reads, cover art (ADR 0010).
pub const SCOPE_LIBRARY_READ: &str = "library:read";
/// Playlist create/edit/delete (ADR 0010).
pub const SCOPE_PLAYLISTS_WRITE: &str = "playlists:write";
/// Scrobble / play events (ADR 0010).
pub const SCOPE_PLAYS_WRITE: &str = "plays:write";
/// Playback via the signed-grant transport (ADR 0010).
pub const SCOPE_STREAM_READ: &str = "stream:read";
/// Ingest job submission (ADR 0010).
pub const SCOPE_INGEST_SUBMIT: &str = "ingest:submit";

/// Full scope vocabulary, in ADR order. Parsing is exact against these.
pub const ALL_SCOPES: [&str; 5] = [
    SCOPE_LIBRARY_READ,
    SCOPE_PLAYLISTS_WRITE,
    SCOPE_PLAYS_WRITE,
    SCOPE_STREAM_READ,
    SCOPE_INGEST_SUBMIT,
];

/// A single granted scope. Exact, case-sensitive, no trimming: anything
/// outside [`ALL_SCOPES`] (including `"subsonic"`, `""`, `"Library:Read"`)
/// is not a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    LibraryRead,
    PlaylistsWrite,
    PlaysWrite,
    StreamRead,
    IngestSubmit,
}

impl Scope {
    /// Render back to the canonical scope string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::LibraryRead => SCOPE_LIBRARY_READ,
            Scope::PlaylistsWrite => SCOPE_PLAYLISTS_WRITE,
            Scope::PlaysWrite => SCOPE_PLAYS_WRITE,
            Scope::StreamRead => SCOPE_STREAM_READ,
            Scope::IngestSubmit => SCOPE_INGEST_SUBMIT,
        }
    }

    /// Parse an exact scope string. Returns `None` for anything else —
    /// never trims, folds, or guesses.
    pub fn parse(s: &str) -> Option<Scope> {
        match s {
            SCOPE_LIBRARY_READ => Some(Scope::LibraryRead),
            SCOPE_PLAYLISTS_WRITE => Some(Scope::PlaylistsWrite),
            SCOPE_PLAYS_WRITE => Some(Scope::PlaysWrite),
            SCOPE_STREAM_READ => Some(Scope::StreamRead),
            SCOPE_INGEST_SUBMIT => Some(Scope::IngestSubmit),
            _ => None,
        }
    }
}

/// Display-form class prefix: identifies the credential class at a glance
/// and in logs. The prefix alone is public; the entropy after it is secret.
pub const CREDENTIAL_PREFIX: &str = "ntk_";
/// Non-secret leading slice length (past [`CREDENTIAL_PREFIX`]) kept for
/// "which key is this" listing.
pub const KEY_PREFIX_LEN: usize = 8;
/// Mandatory expiry cap: 365 days. No never-expiring keys (ADR 0010).
pub const MAX_CREDENTIAL_TTL_SECS: u64 = 31_536_000;

/// Entropy bytes per display secret: 256-bit CSPRNG, server-generated only.
const SECRET_ENTROPY_BYTES: usize = 32;
/// Base62 digits for 32 bytes: 62^43 > 2^256, so 32 bytes always fit in
/// 43 digits; shorter encodings are left-padded with `'0'`.
const SECRET_ENTROPY_BASE62_LEN: usize = 43;
/// Random id bytes per credential row.
const ID_BYTES: usize = 16;

const BASE62_ALPHABET: &[u8; 62] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Encode bytes as fixed-width base62 (big-endian divmod, `'0'`-padded).
fn base62_encode(bytes: &[u8]) -> String {
    let mut num: Vec<u8> = bytes.to_vec();
    let mut out: Vec<u8> = Vec::new();
    let mut start = 0usize;
    while start < num.len() {
        let mut rem: u16 = 0;
        for byte in num.iter_mut().skip(start) {
            let cur = (rem << 8) | u16::from(*byte);
            *byte = (cur / 62) as u8;
            rem = cur % 62;
        }
        out.push(BASE62_ALPHABET[rem as usize]);
        while start < num.len() && num[start] == 0 {
            start += 1;
        }
    }
    out.reverse();
    let mut s = String::from_utf8(out).expect("base62 alphabet is ASCII");
    while s.len() < SECRET_ENTROPY_BASE62_LEN {
        s.insert(0, '0');
    }
    s
}

fn lowercase_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Versioned HMAC pepper from the Worker-secret keyring (separate from the
/// Subsonic pepper so either surface rotates without forcing the other).
/// The bytes are secret: `Debug` names the version only.
pub struct Pepper<'a> {
    pub version: u32,
    pub bytes: &'a [u8],
}

impl std::fmt::Debug for Pepper<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pepper")
            .field("version", &self.version)
            .finish()
    }
}

/// Stored credential row. `key_hmac` is the HMAC of the display secret —
/// the secret itself is never stored. Manual `Debug`: names the id, owner,
/// listing prefix, scopes and lifetime state, but never secret material.
#[derive(Clone)]
pub struct NativeRestRecord {
    pub id: String,
    pub user_id: String,
    pub key_prefix: String,
    pub key_hmac: String,
    pub pepper_version: u32,
    pub scopes: Vec<Scope>,
    pub created_at: u64,
    pub expires_at: u64,
    pub revoked_at: Option<u64>,
}

impl std::fmt::Debug for NativeRestRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeRestRecord")
            .field("id", &self.id)
            .field("user_id", &self.user_id)
            .field("key_prefix", &self.key_prefix)
            .field("scopes", &self.scopes)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("revoked_at", &self.revoked_at)
            .finish()
    }
}

/// Freshly minted credential: the display secret (shown once, then dropped
/// by the HTTP layer) plus the row to persist. Manual `Debug` reuses the
/// record's redacted form — the plaintext never appears in diagnostics.
pub struct IssuedCredential {
    pub display_once: String,
    pub record: NativeRestRecord,
}

impl std::fmt::Debug for IssuedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedCredential")
            .field("record", &self.record)
            .finish()
    }
}

/// Lowercase-hex HMAC-SHA256 over the display secret, deterministic per
/// (secret, pepper) and pepper-bound: the wrong pepper mismatches, which
/// is what makes rotation safe.
pub fn credential_hmac(secret: &str, pepper: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(pepper).expect("HMAC accepts any key length");
    mac.update(secret.as_bytes());
    lowercase_hex(&mac.finalize().into_bytes())
}

fn new_secret() -> Result<String, CredentialError> {
    let entropy = crate::random::secure_random_bytes::<SECRET_ENTROPY_BYTES>()?;
    Ok(format!("{CREDENTIAL_PREFIX}{}", base62_encode(&entropy)))
}

fn new_id() -> Result<String, CredentialError> {
    let bytes = crate::random::secure_random_bytes::<ID_BYTES>()?;
    Ok(crate::b64::encode(&bytes))
}

/// Mint a credential for `user_id` with exactly `scopes` (including the
/// empty set, which authorizes nothing). The display secret is unique per
/// issue; `expires_at` clamps fail-safe to the 365-day cap so even
/// `u64::MAX` TTLs can never mint a forever key.
///
/// Fail-closed: secure-randomness failure returns
/// [`CredentialError::RandomnessUnavailable`] (HTTP 503). The success path
/// is byte-identical: the same 32 secret bytes (base62) and 16 id bytes
/// (base64url) through the same encodings.
pub fn issue_credential(
    user_id: &str,
    scopes: &[Scope],
    ttl_secs: u64,
    now: u64,
    pepper: &Pepper<'_>,
) -> Result<IssuedCredential, CredentialError> {
    let display_once = new_secret()?;
    let record = NativeRestRecord {
        id: new_id()?,
        user_id: user_id.to_string(),
        key_prefix: display_once[..CREDENTIAL_PREFIX.len() + KEY_PREFIX_LEN].to_string(),
        key_hmac: credential_hmac(&display_once, pepper.bytes),
        pepper_version: pepper.version,
        scopes: scopes.to_vec(),
        created_at: now,
        expires_at: now.saturating_add(ttl_secs.min(MAX_CREDENTIAL_TTL_SECS)),
        revoked_at: None,
    };
    Ok(IssuedCredential {
        display_once,
        record,
    })
}

/// Credential failure taxonomy. Reuses the repo registry
/// (`docs/api-errors.md`) — no new codes: randomness failures map to the
/// existing `service_unavailable` entry. `Display` is generic on purpose:
/// clients match the outcome class, attackers learn nothing beyond it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialError {
    Unauthenticated,
    Forbidden,
    /// Secure randomness unavailable while minting a credential: HTTP 503
    /// `service_unavailable`. Never a panic, fallback, or reused secret/id.
    RandomnessUnavailable,
}

impl CredentialError {
    /// Stable registry code for the structured error envelope.
    pub fn code(&self) -> &'static str {
        match self {
            CredentialError::Unauthenticated => "unauthorized",
            CredentialError::Forbidden => "forbidden",
            CredentialError::RandomnessUnavailable => "service_unavailable",
        }
    }

    /// HTTP status for the envelope.
    pub fn http_status(&self) -> u16 {
        match self {
            CredentialError::Unauthenticated => 401,
            CredentialError::Forbidden => 403,
            CredentialError::RandomnessUnavailable => 503,
        }
    }
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Generic: never says which check failed, never any secret material.
            CredentialError::Unauthenticated => write!(f, "invalid credential"),
            CredentialError::Forbidden => write!(f, "insufficient scope"),
            // Sealed public text: matches the `service_unavailable` envelope.
            CredentialError::RandomnessUnavailable => write!(f, "service unavailable"),
        }
    }
}

impl From<crate::random::CryptoError> for CredentialError {
    fn from(_: crate::random::CryptoError) -> Self {
        CredentialError::RandomnessUnavailable
    }
}

/// Authorize `presented_secret` against `record` for `required`.
///
/// Every credential-validity failure maps to `Unauthenticated`
/// (unknown/tampered secret, unknown pepper version, owner mismatch,
/// revoked, expired — no oracle distinguishes them); a valid credential
/// lacking `required` maps to `Forbidden`. Expiry boundary: `now >
/// expires_at` is expired, `now == expires_at` still passes. Owner
/// mismatch is `Unauthenticated`, never `Forbidden`, so a non-owner probe
/// cannot learn the key was otherwise valid.
///
/// Dual-pepper transition: a record whose version matches ANY entry in
/// `peppers` authorizes; an empty slice or an unlisted version fails
/// closed. Takes no cookie/session input by design — the HTTP layer must
/// not consult cookies on bearer routes.
pub fn authorize(
    record: &NativeRestRecord,
    presented_secret: &str,
    required: Scope,
    now: u64,
    caller_user_id: &str,
    peppers: &[Pepper<'_>],
) -> Result<(), CredentialError> {
    use subtle::ConstantTimeEq as _;
    if record.user_id != caller_user_id {
        return Err(CredentialError::Unauthenticated);
    }
    if record.revoked_at.is_some() {
        return Err(CredentialError::Unauthenticated);
    }
    if now > record.expires_at {
        return Err(CredentialError::Unauthenticated);
    }
    let Some(pepper) = peppers.iter().find(|p| p.version == record.pepper_version) else {
        return Err(CredentialError::Unauthenticated);
    };
    let presented_hmac = credential_hmac(presented_secret, pepper.bytes);
    if presented_hmac
        .as_bytes()
        .ct_eq(record.key_hmac.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Err(CredentialError::Unauthenticated);
    }
    if !record.scopes.contains(&required) {
        return Err(CredentialError::Forbidden);
    }
    Ok(())
}

/// Revoke one credential now. Idempotent and sticky: a second revoke never
/// un-revokes (the original timestamp stands).
pub fn revoke_credential(record: &mut NativeRestRecord, now: u64) {
    if record.revoked_at.is_none() {
        record.revoked_at = Some(now);
    }
}

/// Recovery/revoke-all path (email change/revert, passkey add/remove,
/// session-compromise response): revokes every unrevoked row for
/// `user_id`, returns how many transitioned. An empty user id revokes
/// nothing — an upstream bug must never become a mass revocation.
///
/// Clock-free by design (pure state transition): the stamp marks the
/// transition with the record's own creation time, the earliest provable
/// point. The store layer owns the wall-clock audit time.
pub fn revoke_all_for_user(records: &mut [NativeRestRecord], user_id: &str) -> usize {
    if user_id.is_empty() {
        return 0;
    }
    let mut revoked = 0;
    for record in records.iter_mut() {
        if record.user_id == user_id && record.revoked_at.is_none() {
            record.revoked_at = Some(record.created_at);
            revoked += 1;
        }
    }
    revoked
}

/// Header-only transport. Accepts `Bearer <opaque-token>` (scheme
/// case-insensitive per RFC 7235, exactly one space, non-empty token with
/// no whitespace inside) and returns the token slice; everything else is
/// `Unauthenticated`. There is intentionally no query-string parser: the
/// HTTP layer must never read credentials from the query.
pub fn extract_bearer_key(authorization: Option<&str>) -> Result<&str, CredentialError> {
    let header = authorization.ok_or(CredentialError::Unauthenticated)?;
    let (scheme, token) = header
        .split_once(' ')
        .ok_or(CredentialError::Unauthenticated)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(CredentialError::Unauthenticated);
    }
    if token.is_empty() || token.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err(CredentialError::Unauthenticated);
    }
    Ok(token)
}

/// Display-form shape gate: `ntk_` plus exactly 43 base62 characters
/// (`SECRET_ENTROPY_BASE62_LEN`). The HTTP bearer gate calls this BEFORE
/// any HMAC or database work, so random `Authorization` values cost no
/// Postgres connection or query (ADR 0010 DoS: unauthenticated bearer
/// routes were plain 404s with no DB work before this credential class).
/// A well-formed random token still reaches the HMAC lookup and fails
/// closed with the same plain 404 as a header-absent caller (no
/// route-existence oracle); shape alone never authenticates.
pub fn is_plausible_credential_shape(token: &str) -> bool {
    let Some(rest) = token.strip_prefix(CREDENTIAL_PREFIX) else {
        return false;
    };
    if rest.len() != SECRET_ENTROPY_BASE62_LEN {
        return false;
    }
    rest.bytes().all(|b| {
        matches!(b,
        b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z')
    })
}

/// What a (method, path) needs. `Bearer` routes accept the named scope;
/// `SessionOnly` routes are never reachable by bearer (enforced by the
/// auth layer, not by scope strings); `Deny` is everything else —
/// cross-surface use, unlisted methods, unknown paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDecision {
    Bearer(Scope),
    SessionOnly,
    Deny,
}

/// The exhaustive default-deny map (ADR 0010). Methods match exact
/// uppercase (`"get"` fails closed); paths match the listed collection
/// roots plus subpaths. The HTTP layer passes the path without the query
/// string; path normalization (no `..`, no double slashes) is the caller's
/// job — an unnormalized path that misses every shape below denies.
///
/// An endpoint added after ADR 0010 stays `Deny` until this map is
/// explicitly updated: no implementation may infer a scope from a
/// resource name or a similar operation.
pub fn scope_for_operation(method: &str, path: &str) -> RouteDecision {
    use RouteDecision::{Bearer, Deny, SessionOnly};
    /// `/prefix/{single-segment}` (no deeper nesting). Callers pass a
    /// slash-terminated prefix, so the remainder never starts with `/`.
    fn one_segment_after(prefix: &str, path: &str) -> bool {
        path.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
    }
    match method {
        "GET" => {
            // Catalog reads → library:read (roots and subpaths).
            if path == "/v1/library"
                || path == "/v1/artists"
                || path == "/v1/releases"
                || path.starts_with("/v1/releases/")
                || path == "/v1/tracks"
                || path == "/v1/search"
                || path == "/v1/playlists"
                || path.starts_with("/v1/playlists/")
            {
                return Bearer(Scope::LibraryRead);
            }
            if path == "/v1/stream" || path.starts_with("/v1/stream/") {
                return Bearer(Scope::StreamRead);
            }
            // Session-only reads (never reachable by bearer).
            if path == "/v1/me" || path == "/v1/indexers" || path == "/v1/vault" {
                return SessionOnly;
            }
            if path == "/v1/credentials" {
                return SessionOnly;
            }
            if one_segment_after("/v1/operations/", path) {
                return SessionOnly;
            }
            Deny
        }
        // HEAD revalidates exactly the GET-readable catalog roots: a
        // lawful bearer HEAD must clear the same gate as the GET it
        // revalidates, and every other HEAD shape stays Deny until the
        // map is explicitly updated.
        "HEAD" => {
            if path == "/v1/library"
                || path == "/v1/artists"
                || path == "/v1/releases"
                || path == "/v1/tracks"
                || path == "/v1/search"
                || path == "/v1/playlists"
            {
                return Bearer(Scope::LibraryRead);
            }
            Deny
        }
        "POST" => {
            if path == "/v1/playlists" {
                return Bearer(Scope::PlaylistsWrite);
            }
            if path == "/v1/plays" {
                return Bearer(Scope::PlaysWrite);
            }
            if path == "/v1/ingest" || path.starts_with("/v1/ingest/") {
                return Bearer(Scope::IngestSubmit);
            }
            // Session-only writes (never reachable by bearer).
            if path == "/v1/indexers"
                || path == "/v1/vault/test"
                || path == "/v1/vault/test-connection"
                || path == "/v1/credentials"
                || path == "/v1/auth/passkey/login/start"
                || path == "/v1/session/refresh"
                || path == "/v1/session/revoke-all"
                || path == "/v1/billing/checkout"
            {
                return SessionOnly;
            }
            if let Some(rest) = path.strip_prefix("/v1/libraries/") {
                if let Some((id, tail)) = rest.split_once('/') {
                    if !id.is_empty() && tail == "members" {
                        return SessionOnly;
                    }
                }
            }
            Deny
        }
        "PUT" => {
            // Single-segment item shapes only: nested, trailing-slash or
            // empty-segment shapes deny here, before any bearer lookup or
            // rate-limit spend downstream.
            if one_segment_after("/v1/playlists/", path) {
                return Bearer(Scope::PlaylistsWrite);
            }
            if path == "/v1/me" || path == "/v1/vault" {
                return SessionOnly;
            }
            Deny
        }
        "PATCH" => {
            // Single-segment item shapes only (see PUT).
            if one_segment_after("/v1/playlists/", path) {
                return Bearer(Scope::PlaylistsWrite);
            }
            Deny
        }
        "DELETE" => {
            // Single-segment item shapes only (see PUT).
            if one_segment_after("/v1/playlists/", path) {
                return Bearer(Scope::PlaylistsWrite);
            }
            if one_segment_after("/v1/libraries/", path) {
                return SessionOnly;
            }
            if path == "/v1/account" {
                return SessionOnly;
            }
            if path.starts_with("/v1/credentials/") && path.len() > "/v1/credentials/".len() {
                return SessionOnly;
            }
            Deny
        }
        _ => Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEPPER_BYTES: &[u8] = b"synthetic-unit-pepper-32-bytes!!!!!";

    fn pepper() -> Pepper<'static> {
        Pepper {
            version: 1,
            bytes: PEPPER_BYTES,
        }
    }

    #[test]
    fn randomness_unavailable_is_503_service_unavailable() {
        let err = CredentialError::RandomnessUnavailable;
        assert_eq!(err.code(), "service_unavailable");
        assert_eq!(err.http_status(), 503);
        assert_eq!(err.to_string(), "service unavailable");
        assert_eq!(
            CredentialError::from(crate::random::CryptoError::RandomnessUnavailable),
            CredentialError::RandomnessUnavailable
        );
    }

    #[test]
    fn display_secret_shape_holds_class_prefix_and_fixed_entropy() {
        let cred =
            issue_credential(USER, &[Scope::LibraryRead], 60, NOW, &pepper()).expect("issue");
        assert!(cred.display_once.starts_with(CREDENTIAL_PREFIX));
        assert_eq!(
            cred.display_once.len(),
            CREDENTIAL_PREFIX.len() + SECRET_ENTROPY_BASE62_LEN
        );
        assert_eq!(
            cred.record.key_prefix.len(),
            CREDENTIAL_PREFIX.len() + KEY_PREFIX_LEN
        );
    }

    const USER: &str = "user-unit";
    const NOW: u64 = 1_700_000_000;

    #[test]
    fn issued_wrapper_debug_names_record_without_secret() {
        let cred =
            issue_credential(USER, &[Scope::LibraryRead], 60, NOW, &pepper()).expect("issue");
        let debug = format!("{:?}", cred);
        assert!(debug.contains(&cred.record.id));
        assert!(!debug.contains(&cred.display_once));
        assert!(!debug.contains(&cred.record.key_hmac));
    }

    #[test]
    fn plausible_shape_gate_accepts_only_minted_form() {
        // The HTTP bearer gate runs this BEFORE any HMAC or DB work, so a
        // regression here reopens the unauthenticated DB-amplification the
        // gate exists to close. Pin the exact minted form and each
        // rejection class: removing the guard must fail here.
        let minted =
            issue_credential(USER, &[Scope::LibraryRead], 60, NOW, &pepper()).expect("issue");
        assert!(
            is_plausible_credential_shape(&minted.display_once),
            "a freshly minted secret must pass the shape gate"
        );
        // Wrong prefix (including the empty token) never passes.
        for bad in ["", "bearer abc", "Bearer ntk_abc", "atk_"] {
            assert!(
                !is_plausible_credential_shape(bad),
                "{bad:?} must fail the shape gate"
            );
        }
        let good_tail = &minted.display_once[CREDENTIAL_PREFIX.len()..];
        assert_eq!(good_tail.len(), SECRET_ENTROPY_BASE62_LEN);
        // 42 and 44 chars: exactly 43 base62 chars are required.
        assert!(!is_plausible_credential_shape(&format!(
            "{CREDENTIAL_PREFIX}{}",
            &good_tail[..SECRET_ENTROPY_BASE62_LEN - 1]
        )));
        assert!(!is_plausible_credential_shape(&format!(
            "{CREDENTIAL_PREFIX}{}0",
            good_tail
        )));
        // Non-base62 char (dash, underscore, space) fails even at 43 length.
        let mut dashed = good_tail.to_string();
        dashed.replace_range(0..1, "-");
        assert!(!is_plausible_credential_shape(&format!(
            "{CREDENTIAL_PREFIX}{dashed}"
        )));
        // Well-formed random entropy passes the gate (it still fails closed
        // after the HMAC lookup with the plain 404; shape alone never
        // authenticates).
        assert!(is_plausible_credential_shape(&format!(
            "{CREDENTIAL_PREFIX}{}",
            "0".repeat(SECRET_ENTROPY_BASE62_LEN)
        )));
    }

    #[test]
    fn head_revalidates_every_catalog_root_and_nothing_else() {
        // HEAD clears the same gate as the GET it revalidates, on every
        // catalog root; non-catalog and unknown HEAD shapes stay Deny.
        for path in [
            "/v1/library",
            "/v1/artists",
            "/v1/releases",
            "/v1/tracks",
            "/v1/search",
            "/v1/playlists",
        ] {
            assert_eq!(
                scope_for_operation("HEAD", path),
                RouteDecision::Bearer(Scope::LibraryRead),
                "HEAD {path}"
            );
        }
        for path in ["/v1/plays", "/v1/stream", "/v1/ingest", "/v1/nope"] {
            assert_eq!(
                scope_for_operation("HEAD", path),
                RouteDecision::Deny,
                "HEAD {path}"
            );
        }
    }
}
