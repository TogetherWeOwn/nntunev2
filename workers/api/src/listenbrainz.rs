//! ListenBrainz account-link source preparation (ADR 0012 section 4).
//!
//! Pure, provider-free building blocks: validated username/token types, a
//! per-user field envelope for the token (reusing [`crate::field_envelope`],
//! no custom crypto), the taste to upstream-feedback mapping and
//! conservative `X-RateLimit-*` / `Retry-After` parsing. Nothing here does
//! HTTP, reads a store or produces events.
//!
//! Integration contracts for the caller (session-authenticated API over a
//! durable `Store`; not wired here):
//! - `user_id` always comes from the authenticated session, never from the
//!   request body. Pass the same value to [`LinkInput::seal`] and
//!   [`StoredLink::open`].
//! - Persist exactly one row per user in the dedicated table
//!   [`LINK_TABLE`] (`listenbrainz_links`): `user_id`, `username`,
//!   `kek_version`, and the four [`FieldEnvelope`] byte columns. Relinking
//!   is delete-then-store; unlinking deletes the row in the same operation.
//! - Select the `kek_version` row column and fetch the KEK for exactly that
//!   version; an unknown version fails, it never falls back to another key.
//! - The token is opened only server-side immediately before an upstream
//!   call, never returned to a client and never logged. Clients only ever
//!   see [`LinkSummary`].
//! - Honour [`next_allowed_at`] per user/token before every upstream call.
//! - `hide` stays local: [`feedback_for`] yields no upstream feedback for it.

use serde::Serialize;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    field_envelope::{self, Context, FieldEnvelope},
    rotation::KekSource,
};

/// Dedicated table the envelope context binds to.
pub const LINK_TABLE: &str = "listenbrainz_links";
/// Field the envelope context binds to.
pub const TOKEN_FIELD: &str = "token";
const ENVELOPE_VERSION: u16 = 2;
const PAYLOAD_FORMAT: u16 = 1;

const MAX_USERNAME_CHARS: usize = 64;
const MAX_USER_ID_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    InvalidUsername,
    InvalidToken,
    InvalidUserId,
    InvalidMbid,
    UnknownKey,
    /// Rewrap target version is zero or not strictly newer than the row's.
    InvalidRewrapVersion,
    /// Malformed stored metadata or ciphertext shape.
    Corrupt,
    /// Seal failure, or authentication failure on open (wrong row, user,
    /// username, key or tampered bytes). Deliberately not more specific.
    Crypto,
}

/// Conservative ListenBrainz username: 1..=64 chars, alphanumeric or
/// `_ - . ` plus interior spaces, no leading/trailing space. Excludes
/// path/query/control characters so it is safe in upstream URL paths.
#[derive(Clone, PartialEq, Eq)]
pub struct Username(String);

impl Username {
    pub fn parse(raw: &str) -> Result<Self, LinkError> {
        let n = raw.chars().count();
        let ok = (1..=MAX_USERNAME_CHARS).contains(&n)
            && !raw.starts_with(' ')
            && !raw.ends_with(' ')
            && raw
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ' '));
        if ok {
            Ok(Self(raw.to_owned()))
        } else {
            Err(LinkError::InvalidUsername)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Username {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Username").field(&self.0).finish()
    }
}

/// Canonical 8-4-4-4-12 hex shape (case-insensitive).
fn uuid_shape(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// ListenBrainz user token (a UUID). Zeroized on drop; deliberately no
/// `Clone`, `Serialize` or `PartialEq`, and `Debug` is redacted.
pub struct Token(Zeroizing<String>);

impl Token {
    pub fn parse(raw: &str) -> Result<Self, LinkError> {
        if uuid_shape(raw) {
            Ok(Self(Zeroizing::new(raw.to_owned())))
        } else {
            Err(LinkError::InvalidToken)
        }
    }

    /// For the single server-side upstream authorization header only.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// Explicit username + token from the link request.
pub struct LinkInput {
    pub username: Username,
    pub token: Token,
}

impl LinkInput {
    pub fn new(username: &str, token: &str) -> Result<Self, LinkError> {
        Ok(Self {
            username: Username::parse(username)?,
            token: Token::parse(token)?,
        })
    }

    /// Seal under `kek_version`. The context binds table, field, user and
    /// username, so the row cannot be swapped to another user or username.
    pub fn seal(
        &self,
        keks: &impl KekSource,
        kek_version: u32,
        user_id: &str,
    ) -> Result<StoredLink, LinkError> {
        let scope = scope(user_id)?;
        let kek = keks.kek(kek_version).ok_or(LinkError::UnknownKey)?;
        let plaintext = Zeroizing::new(self.token.expose().as_bytes().to_vec());
        let envelope = field_envelope::seal(
            kek,
            kek_version,
            &context(&scope, self.username.as_str()),
            &plaintext,
        )
        .map_err(|_| LinkError::Crypto)?;
        Ok(StoredLink {
            user_id: user_id.to_owned(),
            username: self.username.clone(),
            kek_version,
            envelope,
        })
    }
}

impl std::fmt::Debug for LinkInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkInput")
            .field("username", &self.username)
            .field("token", &"<redacted>")
            .finish()
    }
}

fn scope(user_id: &str) -> Result<String, LinkError> {
    let ok = !user_id.is_empty()
        && user_id.len() <= MAX_USER_ID_BYTES
        && user_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    if ok {
        Ok(format!("user:{user_id}"))
    } else {
        Err(LinkError::InvalidUserId)
    }
}

fn context<'a>(scope: &'a str, username: &'a str) -> Context<'a> {
    Context {
        table: LINK_TABLE,
        field: TOKEN_FIELD,
        scope,
        row: username,
        version: ENVELOPE_VERSION,
        format: PAYLOAD_FORMAT,
    }
}

/// Durable wrap-only write result. A lost captured envelope must be
/// reloaded, never retried as an unconditional replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewrapOutcome {
    Rewrapped,
    ClaimLost,
    AccountDeleting,
    NoRow,
}

/// One persisted link row (what the Store reads/writes). Envelope bytes are
/// opaque; `Debug` never prints them.
#[derive(Clone)]
pub struct StoredLink {
    pub user_id: String,
    pub username: Username,
    pub kek_version: u32,
    pub envelope: FieldEnvelope,
}

impl StoredLink {
    /// Open for a server-side upstream call. `user_id` must be the
    /// authenticated session user; a row for another user is rejected both
    /// by the explicit check and by the authenticated context.
    pub fn open(&self, keks: &impl KekSource, user_id: &str) -> Result<Token, LinkError> {
        let scope = scope(user_id)?;
        if self.user_id != user_id {
            return Err(LinkError::Crypto);
        }
        let e = &self.envelope;
        if e.dek_wrapped.len() != 48 || e.ciphertext.len() != 36 + 16 {
            return Err(LinkError::Corrupt);
        }
        let kek = keks.kek(self.kek_version).ok_or(LinkError::UnknownKey)?;
        let mut bytes = Zeroizing::new(
            field_envelope::open(
                kek,
                self.kek_version,
                &context(&scope, self.username.as_str()),
                e,
            )
            .map_err(|_| LinkError::Crypto)?,
        );
        match String::from_utf8(std::mem::take(&mut *bytes)) {
            Ok(s) => {
                let s = Zeroizing::new(s);
                if uuid_shape(&s) {
                    Ok(Token(s))
                } else {
                    Err(LinkError::Corrupt)
                }
            }
            Err(e) => {
                e.into_bytes().zeroize();
                Err(LinkError::Corrupt)
            }
        }
    }

    /// Pure DEK-only rewrap from this row's captured KEK version to a
    /// strictly newer, positive `target_version`. The payload nonce and
    /// ciphertext are preserved byte-for-byte; the token is never decrypted.
    /// Persisting the result is the caller's job: compare-and-set against the
    /// full captured envelope and username, not just the version (a relink
    /// can replace the credential without changing its KEK version).
    pub fn rewrap(
        &self,
        keks: &impl KekSource,
        user_id: &str,
        target_version: u32,
    ) -> Result<StoredLink, LinkError> {
        let scope = scope(user_id)?;
        if self.user_id != user_id {
            return Err(LinkError::Crypto);
        }
        if target_version == 0 || target_version <= self.kek_version {
            return Err(LinkError::InvalidRewrapVersion);
        }
        let e = &self.envelope;
        if e.dek_wrapped.len() != 48 || e.ciphertext.len() != 36 + 16 {
            return Err(LinkError::Corrupt);
        }
        let old_kek = keks.kek(self.kek_version).ok_or(LinkError::UnknownKey)?;
        let new_kek = keks.kek(target_version).ok_or(LinkError::UnknownKey)?;
        let envelope = field_envelope::rewrap(
            old_kek,
            self.kek_version,
            new_kek,
            target_version,
            &context(&scope, self.username.as_str()),
            e,
        )
        .map_err(|_| LinkError::Crypto)?;
        Ok(StoredLink {
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            kek_version: target_version,
            envelope,
        })
    }

    /// Client-safe projection: never the token, envelope or key version.
    pub fn summary(&self) -> LinkSummary {
        LinkSummary {
            linked: true,
            username: Some(self.username.as_str().to_owned()),
        }
    }
}

impl std::fmt::Debug for StoredLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredLink")
            .field("user_id", &self.user_id)
            .field("username", &self.username)
            .field("kek_version", &self.kek_version)
            .field("envelope", &"<redacted>")
            .finish()
    }
}

/// What the API may return to the client after linking.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct LinkSummary {
    pub linked: bool,
    pub username: Option<String>,
}

// --- Taste mapping --------------------------------------------------------

/// Local taste state. `Hide` is a local-only preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taste {
    Love,
    Dislike,
    Neutral,
    /// Clearing a previous taste; upstream score 0.
    Clear,
    Hide,
}

/// Upstream feedback score: love 1, dislike -1, neutral/clear 0. `Hide`
/// returns `None`: it is never sent upstream (no automatic dislike).
pub fn feedback_score(taste: Taste) -> Option<i8> {
    match taste {
        Taste::Love => Some(1),
        Taste::Dislike => Some(-1),
        Taste::Neutral | Taste::Clear => Some(0),
        Taste::Hide => None,
    }
}

/// Valid, lowercase-normalised, non-nil recording MBID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RecordingMbid(String);

impl RecordingMbid {
    pub fn parse(raw: &str) -> Result<Self, LinkError> {
        let s = raw.to_ascii_lowercase();
        if uuid_shape(&s) && s.bytes().any(|b| b != b'0' && b != b'-') {
            Ok(Self(s))
        } else {
            Err(LinkError::InvalidMbid)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Body for the upstream recording-feedback call (shape only; no call).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Feedback {
    recording_mbid: RecordingMbid,
    score: i8,
}

/// `Ok(None)` means keep local (hide). The MBID is validated even then, so
/// callers get a consistent error for bad input.
pub fn feedback_for(taste: Taste, recording_mbid: &str) -> Result<Option<Feedback>, LinkError> {
    let recording_mbid = RecordingMbid::parse(recording_mbid)?;
    Ok(feedback_score(taste).map(|score| Feedback {
        recording_mbid,
        score,
    }))
}

// --- Rate limiting --------------------------------------------------------

/// Wait used for 429 without usable headers.
pub const DEFAULT_429_WAIT_S: u64 = 60;
/// Wait used when `Remaining: 0` arrives without a usable reset.
pub const DEFAULT_EXHAUSTED_WAIT_S: u64 = 10;
/// The public API guidance caps clients at one request per second even
/// when the response advertises remaining allowance.
pub const MIN_REQUEST_INTERVAL_S: u64 = 1;

fn header<'a>(headers: &'a [(&str, &str)], name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    headers
        .iter()
        .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
}

/// Digits only (no sign, whitespace or fraction); across duplicate headers
/// the largest valid value wins (conservative).
fn max_u64(headers: &[(&str, &str)], name: &str) -> Option<u64> {
    header(headers, name)
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|v| v.parse::<u64>().ok())
        .max()
}

/// Earliest unix second (`now_s` based) at which the next upstream call to
/// this token is allowed. Pure and conservative:
/// - HTTP 429: the longest of `Retry-After` (delta seconds only),
///   `X-RateLimit-Reset-In` and `X-RateLimit-Reset` (epoch
///   seconds), at least 1s; `DEFAULT_429_WAIT_S` if none parses.
/// - Any other status with `X-RateLimit-Remaining: 0`: wait for the reset,
///   `DEFAULT_EXHAUSTED_WAIT_S` if no usable reset.
/// - Otherwise: one second, including missing/malformed remaining headers.
///
/// A valid numeric `Retry-After` is a lower bound for every status and
/// remaining-budget combination. Never shorten a provider cooldown.
/// The transport must reject unsupported
/// `Retry-After` HTTP-date values before retrying; this helper handles delta
/// seconds and the documented ListenBrainz numeric reset headers only.
/// The adapter must persist and atomically reserve a shared request budget;
/// this pure function alone is not concurrency or fleet-wide enforcement.
pub fn next_allowed_at(now_s: u64, status: u16, headers: &[(&str, &str)]) -> u64 {
    // Do not guess an earlier retry for HTTP-date, overflow or malformed
    // Retry-After. A transport supporting HTTP-date can resolve it first.
    if header(headers, "retry-after").any(|v| {
        v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) || v.parse::<u64>().is_err()
    }) {
        return u64::MAX;
    }
    let reset_wait = [
        max_u64(headers, "x-ratelimit-reset-in"),
        max_u64(headers, "x-ratelimit-reset").map(|at| at.saturating_sub(now_s)),
    ]
    .into_iter()
    .flatten()
    .max();
    let wait = if status == 429 {
        let w = [max_u64(headers, "retry-after"), reset_wait]
            .into_iter()
            .flatten()
            .max();
        w.unwrap_or(DEFAULT_429_WAIT_S).max(1)
    } else if header(headers, "x-ratelimit-remaining").any(|v| v.parse::<u64>() == Ok(0)) {
        reset_wait.unwrap_or(DEFAULT_EXHAUSTED_WAIT_S).max(1)
    } else {
        max_u64(headers, "retry-after")
            .unwrap_or(MIN_REQUEST_INTERVAL_S)
            .max(1)
    };
    let wait = wait.max(max_u64(headers, "retry-after").unwrap_or(0));
    now_s.saturating_add(wait)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Keys;
    impl KekSource for Keys {
        fn kek(&self, version: u32) -> Option<&[u8; 32]> {
            match version {
                1 => Some(&[7; 32]),
                2 => Some(&[9; 32]),
                3 => Some(&[11; 32]),
                _ => None,
            }
        }
    }

    const TOKEN: &str = "0b9e2f4a-1c3d-4e5f-8a6b-7c8d9e0f1a2b";
    const MBID: &str = "8f3471b5-7e6a-48da-86a9-c1c07a0f47ae";

    fn link() -> LinkInput {
        LinkInput::new("alice_lb", TOKEN).unwrap()
    }

    #[test]
    fn validates_username_and_token() {
        for bad in [
            "",
            " a",
            "a ",
            "a/b",
            "a?b",
            "a\nb",
            "a%20",
            &"x".repeat(65),
        ] {
            assert_eq!(
                Username::parse(bad),
                Err(LinkError::InvalidUsername),
                "{bad:?}"
            );
        }
        assert!(Username::parse("Rob Dobbs-1.x_").is_ok());
        for bad in [
            "",
            "not-a-token",
            &TOKEN[1..],
            "0b9e2f4a1c3d4e5f8a6b7c8d9e0f1a2bxxxx",
            "zb9e2f4a-1c3d-4e5f-8a6b-7c8d9e0f1a2b",
        ] {
            assert!(
                matches!(Token::parse(bad), Err(LinkError::InvalidToken)),
                "{bad:?}"
            );
        }
        assert!(Token::parse(&TOKEN.to_uppercase()).is_ok());
    }

    #[test]
    fn roundtrip_and_versioned_kek() {
        let stored = link().seal(&Keys, 1, "u-1").unwrap();
        assert_eq!(stored.kek_version, 1);
        assert_eq!(stored.open(&Keys, "u-1").unwrap().expose(), TOKEN);
        // Same plaintext seals differently each time.
        let again = link().seal(&Keys, 1, "u-1").unwrap();
        assert_ne!(stored.envelope.ciphertext, again.envelope.ciphertext);
        // Wrong KEK version for the stored bytes fails; unknown fails closed.
        let mut other = stored.clone();
        other.kek_version = 2;
        assert_eq!(other.open(&Keys, "u-1").unwrap_err(), LinkError::Crypto);
        other.kek_version = 9;
        assert_eq!(other.open(&Keys, "u-1").unwrap_err(), LinkError::UnknownKey);
        assert_eq!(
            link().seal(&Keys, 5, "u-1").unwrap_err(),
            LinkError::UnknownKey
        );
        assert_eq!(
            link().seal(&Keys, 1, "").unwrap_err(),
            LinkError::InvalidUserId
        );
        assert_eq!(
            link().seal(&Keys, 1, "a:b").unwrap_err(),
            LinkError::InvalidUserId
        );
    }

    #[test]
    fn user_and_username_swaps_rejected() {
        let stored = link().seal(&Keys, 1, "u-1").unwrap();
        assert!(stored.open(&Keys, "u-2").is_err());
        // Row's user column rewritten to another user: AAD still rejects.
        let mut moved = stored.clone();
        moved.user_id = "u-2".into();
        assert_eq!(moved.open(&Keys, "u-2").unwrap_err(), LinkError::Crypto);
        let mut renamed = stored.clone();
        renamed.username = Username::parse("mallory").unwrap();
        assert_eq!(renamed.open(&Keys, "u-1").unwrap_err(), LinkError::Crypto);
    }

    #[test]
    fn corruption_rejected() {
        let stored = link().seal(&Keys, 1, "u-1").unwrap();
        let mut c = stored.clone();
        c.envelope.ciphertext[0] ^= 1;
        assert_eq!(c.open(&Keys, "u-1").unwrap_err(), LinkError::Crypto);
        let mut c = stored.clone();
        c.envelope.dek_wrapped[0] ^= 1;
        assert_eq!(c.open(&Keys, "u-1").unwrap_err(), LinkError::Crypto);
        let mut c = stored.clone();
        c.envelope.payload_nonce[0] ^= 1;
        assert!(c.open(&Keys, "u-1").is_err());
        let mut c = stored.clone();
        c.envelope.ciphertext.pop();
        assert_eq!(c.open(&Keys, "u-1").unwrap_err(), LinkError::Corrupt);
        let mut c = stored;
        c.envelope.dek_wrapped.pop();
        assert_eq!(c.open(&Keys, "u-1").unwrap_err(), LinkError::Corrupt);
    }

    #[test]
    fn debug_and_summary_never_leak() {
        let input = link();
        let stored = input.seal(&Keys, 1, "u-1").unwrap();
        let token = stored.open(&Keys, "u-1").unwrap();
        for s in [
            format!("{input:?}"),
            format!("{stored:?}"),
            format!("{token:?}"),
        ] {
            assert!(!s.contains(TOKEN), "{s}");
            assert!(s.contains("redacted"), "{s}");
        }
        let summary = serde_json::to_string(&stored.summary()).unwrap();
        assert_eq!(summary, r#"{"linked":true,"username":"alice_lb"}"#);
        assert!(!summary.contains(TOKEN));
    }

    struct OnlyKey(u32, [u8; 32]);
    impl KekSource for OnlyKey {
        fn kek(&self, version: u32) -> Option<&[u8; 32]> {
            (version == self.0).then_some(&self.1)
        }
    }

    #[test]
    fn rewrap_moves_only_the_wrap_and_version() {
        let stored = link().seal(&Keys, 1, "u-1").unwrap();
        let r = stored.rewrap(&Keys, "u-1", 2).unwrap();
        assert_eq!(r.kek_version, 2);
        assert_eq!(r.user_id, stored.user_id);
        assert_eq!(r.username, stored.username);
        assert_eq!(r.open(&Keys, "u-1").unwrap().expose(), TOKEN);
        assert_eq!(r.envelope.payload_nonce, stored.envelope.payload_nonce);
        assert_eq!(r.envelope.ciphertext, stored.envelope.ciphertext);
        assert_ne!(r.envelope.dek_wrapped, stored.envelope.dek_wrapped);
        assert_ne!(
            r.envelope.dek_wrapped_nonce,
            stored.envelope.dek_wrapped_nonce
        );
        // Skipping versions is allowed (strictly newer).
        let r3 = stored.rewrap(&Keys, "u-1", 3).unwrap();
        assert_eq!(r3.open(&Keys, "u-1").unwrap().expose(), TOKEN);
        // Old key alone cannot open the rewrapped row, new key alone can.
        assert!(r.open(&OnlyKey(1, [7; 32]), "u-1").is_err());
        assert_eq!(r.open(&OnlyKey(2, [9; 32]), "u-1").unwrap().expose(), TOKEN);
        // Original row is unchanged and still opens under the old key.
        assert_eq!(stored.open(&Keys, "u-1").unwrap().expose(), TOKEN);
        let s = format!("{r:?}");
        assert!(s.contains("redacted") && !s.contains(TOKEN));
    }

    #[test]
    fn rewrap_rejects_bad_versions() {
        let stored = link().seal(&Keys, 2, "u-1").unwrap();
        for v in [0, 1, 2] {
            assert_eq!(
                stored.rewrap(&Keys, "u-1", v).unwrap_err(),
                LinkError::InvalidRewrapVersion,
                "{v}"
            );
        }
        // Unknown target and unknown source fail closed.
        assert_eq!(
            stored.rewrap(&Keys, "u-1", 4).unwrap_err(),
            LinkError::UnknownKey
        );
        assert_eq!(
            stored.rewrap(&OnlyKey(3, [11; 32]), "u-1", 3).unwrap_err(),
            LinkError::UnknownKey
        );
    }

    #[test]
    fn rewrap_fails_closed_on_wrong_context_or_tamper() {
        let stored = link().seal(&Keys, 1, "u-1").unwrap();
        assert!(stored.rewrap(&Keys, "u-2", 2).is_err());
        let mut moved = stored.clone();
        moved.user_id = "u-2".into();
        assert_eq!(
            moved.rewrap(&Keys, "u-2", 2).unwrap_err(),
            LinkError::Crypto
        );
        let mut renamed = stored.clone();
        renamed.username = Username::parse("mallory").unwrap();
        assert_eq!(
            renamed.rewrap(&Keys, "u-1", 2).unwrap_err(),
            LinkError::Crypto
        );
        let mut c = stored.clone();
        c.envelope.dek_wrapped[0] ^= 1;
        assert_eq!(c.rewrap(&Keys, "u-1", 2).unwrap_err(), LinkError::Crypto);
        let mut c = stored.clone();
        c.envelope.dek_wrapped_nonce[0] ^= 1;
        assert_eq!(c.rewrap(&Keys, "u-1", 2).unwrap_err(), LinkError::Crypto);
        let mut c = stored.clone();
        c.envelope.dek_wrapped.pop();
        assert_eq!(c.rewrap(&Keys, "u-1", 2).unwrap_err(), LinkError::Corrupt);
        let mut c = stored;
        c.envelope.ciphertext.pop();
        assert_eq!(c.rewrap(&Keys, "u-1", 2).unwrap_err(), LinkError::Corrupt);
    }

    #[test]
    fn taste_mapping_keeps_hide_local() {
        assert_eq!(feedback_score(Taste::Love), Some(1));
        assert_eq!(feedback_score(Taste::Dislike), Some(-1));
        assert_eq!(feedback_score(Taste::Neutral), Some(0));
        assert_eq!(feedback_score(Taste::Clear), Some(0));
        assert_eq!(feedback_score(Taste::Hide), None);
        let f = feedback_for(Taste::Love, &MBID.to_uppercase())
            .unwrap()
            .unwrap();
        assert_eq!(f.recording_mbid.as_str(), MBID);
        assert_eq!(
            serde_json::to_string(&f).unwrap(),
            format!(r#"{{"recording_mbid":"{MBID}","score":1}}"#)
        );
        assert_eq!(feedback_for(Taste::Hide, MBID).unwrap(), None);
        for bad in [
            "",
            "nope",
            "00000000-0000-0000-0000-000000000000",
            &MBID[1..],
        ] {
            assert_eq!(
                feedback_for(Taste::Love, bad).unwrap_err(),
                LinkError::InvalidMbid
            );
            assert_eq!(
                feedback_for(Taste::Hide, bad).unwrap_err(),
                LinkError::InvalidMbid
            );
        }
    }

    #[test]
    fn rate_limit_429() {
        let now = 1_000;
        // Longest of the sources wins; header names are case-insensitive.
        let h = [
            ("Retry-After", "5"),
            ("X-RateLimit-Reset-In", "20"),
            ("x-ratelimit-reset", "1010"),
        ];
        assert_eq!(next_allowed_at(now, 429, &h), 1_020);
        assert_eq!(next_allowed_at(now, 429, &[("retry-after", "30")]), 1_030);
        assert_eq!(
            next_allowed_at(now, 429, &[("x-ratelimit-reset", "1007")]),
            1_007
        );
        // Unsupported HTTP-date, malformed and overflowing Retry-After
        // fail closed rather than inventing an earlier retry time.
        for v in [
            "",
            "abc",
            "-5",
            "1.5",
            "Wed, 21 Oct 2026 07:28:00 GMT",
            "99999999999999999999999",
        ] {
            assert_eq!(
                next_allowed_at(now, 429, &[("retry-after", v)]),
                u64::MAX,
                "{v:?}"
            );
        }
        assert_eq!(next_allowed_at(now, 429, &[]), now + DEFAULT_429_WAIT_S);
        // Zero / past reset still waits at least a second; long provider
        // cooldowns must never be shortened.
        assert_eq!(next_allowed_at(now, 429, &[("retry-after", "0")]), now + 1);
        assert_eq!(
            next_allowed_at(now, 429, &[("x-ratelimit-reset", "5")]),
            now + 1
        );
        assert_eq!(
            next_allowed_at(now, 429, &[("retry-after", "86400")]),
            now + 86400
        );
        assert_eq!(
            next_allowed_at(u64::MAX, 429, &[("retry-after", "5")]),
            u64::MAX
        );
    }

    #[test]
    fn retry_after_is_a_floor_even_when_budget_is_exhausted() {
        for status in [200, 429, 503] {
            let headers = [
                ("Retry-After", "120"),
                ("X-RateLimit-Remaining", "0"),
                ("X-RateLimit-Reset-In", "5"),
            ];
            assert_eq!(next_allowed_at(1000, status, &headers), 1120);
        }
    }

    #[test]
    fn zero_padded_numeric_headers_keep_their_delay() {
        assert_eq!(
            next_allowed_at(1000, 429, &[("Retry-After", "000000000000000000003600")]),
            4600
        );
        assert_eq!(
            next_allowed_at(
                1000,
                200,
                &[
                    ("X-RateLimit-Remaining", "0"),
                    ("X-RateLimit-Reset-In", "000000000000000000003600")
                ]
            ),
            4600
        );
    }

    #[test]
    fn rate_limit_success_headers() {
        let now = 1_000;
        // Exhausted success is respected.
        let h = [
            ("X-RateLimit-Remaining", "0"),
            ("X-RateLimit-Reset-In", "7"),
        ];
        assert_eq!(next_allowed_at(now, 200, &h), 1_007);
        let h = [
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1012"),
        ];
        assert_eq!(next_allowed_at(now, 200, &h), 1_012);
        // Exhausted without a usable reset: conservative fallback.
        for h in [
            vec![("x-ratelimit-remaining", "0")],
            vec![
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset-in", "soon"),
            ],
        ] {
            assert_eq!(
                next_allowed_at(now, 200, &h),
                now + DEFAULT_EXHAUSTED_WAIT_S
            );
        }
        // Every request retains the documented one-request/second floor.
        assert_eq!(
            next_allowed_at(
                now,
                200,
                &[
                    ("x-ratelimit-remaining", "3"),
                    ("x-ratelimit-reset-in", "9")
                ]
            ),
            now + 1
        );
        assert_eq!(
            next_allowed_at(now, 200, &[("x-ratelimit-remaining", "x")]),
            now + 1
        );
        assert_eq!(next_allowed_at(now, 200, &[]), now + 1);
        let duplicates = [
            ("x-ratelimit-remaining", "3"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset-in", "7"),
        ];
        assert_eq!(next_allowed_at(now, 200, &duplicates), now + 7);
        assert_eq!(
            serde_json::to_string(&LinkSummary::default()).unwrap(),
            r#"{"linked":false,"username":null}"#
        );
    }
}
