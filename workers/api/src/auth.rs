//! Accounts/auth: email magic links, passkey challenge lifecycle, invite codes.
//!
//! Threat-model §4: magic-link tokens are cryptographic-random (256-bit),
//! hashed (SHA-256) at rest, single-use, ≤15-min expiry; login errors are
//! generic (no user-enumeration oracle); challenges are single-use with
//! short expiry and RP/origin pinning.
//!
//! WebAuthn attestation/assertion signature verification lives in
//! `crate::webauthn`; this module owns the challenge lifecycle,
//! origin/RP binding, expiry, single-use claims, and credential
//! storage shapes, all enforced and tested here.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Magic-link lifetime: 15 minutes (seconds).
pub const MAGIC_LINK_TTL_SECS: u64 = 900;
/// Passkey challenge lifetime: 5 minutes (seconds).
pub const CHALLENGE_TTL_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    /// Generic: every login/consume failure maps here. Never distinguish
    /// "unknown user" from "bad token" (enumeration oracle).
    Invalid,
    /// Invalid server-side RP/origin configuration; this is not a bad credential.
    Misconfigured,
    /// Secure randomness unavailable while minting a token, challenge id, or
    /// invite code: HTTP 503 `service_unavailable`. Never a panic, fallback,
    /// or reused value.
    RandomnessUnavailable,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Keep credential failures indistinguishable.
            AuthError::Invalid => write!(f, "check your email and try again"),
            AuthError::Misconfigured => write!(f, "authentication service unavailable"),
            // Sealed public text: matches the `service_unavailable` envelope.
            AuthError::RandomnessUnavailable => write!(f, "service unavailable"),
        }
    }
}

impl From<crate::random::CryptoError> for AuthError {
    fn from(_: crate::random::CryptoError) -> Self {
        AuthError::RandomnessUnavailable
    }
}

fn random_b64url_32() -> Result<String, AuthError> {
    let bytes = crate::random::secure_random_bytes::<32>()?;
    // Shared unpadded-base64url (F-HARD-01): no per-module copies.
    Ok(crate::b64::encode(&bytes))
}

fn sha256_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

// --- Email magic link ---------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagicLinkRecord {
    /// SHA-256 hex of the plaintext token. The plaintext is emailed once
    /// and never stored.
    pub token_hash: String,
    pub user_id: String,
    pub email: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub used: bool,
}

/// SHA-256 hex of a presented magic-link token, for record lookup by hash.
/// The HTTP layer looks the record up with this, then still requires a
/// committed conditional claim (optionally with session insertion in the same
/// transaction) — a hash lookup is not authentication.
pub fn hash_magic_token(token: &str) -> String {
    sha256_hex(token)
}

/// Issue a magic link: returns `(plaintext_token, stored_record)`.
/// The plaintext goes into the emailed URL; only the hash is stored.
///
/// Fail-closed: secure-randomness failure returns
/// [`AuthError::RandomnessUnavailable`] (HTTP 503) so the caller sends no
/// mail and stores nothing. The success path is byte-identical: the same 32
/// entropy bytes through the same [`crate::b64::encode`].
pub fn issue_magic_link(
    user_id: &str,
    email: &str,
    now: u64,
) -> Result<(String, MagicLinkRecord), AuthError> {
    let token = random_b64url_32()?;
    let record = MagicLinkRecord {
        token_hash: sha256_hex(&token),
        user_id: user_id.to_string(),
        email: email.to_string(),
        issued_at: now,
        expires_at: now.saturating_add(MAGIC_LINK_TTL_SECS),
        used: false,
    };
    Ok((token, record))
}

/// Pure record validation/preparation, NOT authentication or a database claim.
/// Independent snapshots may both pass. HTTP callers must also commit the
/// conditional database claim, using [`consume_magic_link_atomically`] or
/// [`crate::handlers::Store::redeem_magic_link`] to include session insertion
/// in the same transaction, before returning an authenticated result.
pub fn consume_magic_link(
    mut record: MagicLinkRecord,
    presented_token: &str,
    now: u64,
) -> Result<ConsumedMagicLink, AuthError> {
    let presented_hash = sha256_hex(presented_token);
    if presented_hash
        .as_bytes()
        .ct_eq(record.token_hash.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Err(AuthError::Invalid);
    }
    if record.used {
        return Err(AuthError::Invalid);
    }
    if now > record.expires_at {
        return Err(AuthError::Invalid);
    }
    record.used = true;
    Ok(ConsumedMagicLink {
        user_id: record.user_id.clone(),
        used_record: record,
    })
}

/// Validated candidate and prepared spent record. It becomes an authenticated
/// result ONLY after the conditional database claim commits (via
/// [`consume_magic_link_atomically`] or transactional session redemption);
/// the pure preparation helper grants no session.
#[derive(Debug, Clone)]
pub struct ConsumedMagicLink {
    pub user_id: String,
    pub used_record: MagicLinkRecord,
}

impl ConsumedMagicLink {
    /// Test helper: recover the spent record to prove replay rejection.
    pub fn into_record_for_replay_test(self) -> MagicLinkRecord {
        self.used_record
    }
}

// --- Passkey challenge lifecycle ----------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasskeyChallenge {
    /// Random challenge id (base64url, 256-bit).
    pub id: String,
    /// Relying-party id, pinned to the real origin's host.
    pub rp_id: String,
    /// Exact expected origin (scheme + host), e.g. `https://nntune.example.com`.
    pub origin: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub consumed: bool,
}

/// Start a registration (or authentication) ceremony. Validates the RP/origin
/// shape now so misconfiguration fails before any credential is created.
/// Returns [`AuthError::Misconfigured`] (never panics) when the server's
/// configured RP/origin is invalid; credential and challenge failures remain
/// [`AuthError::Invalid`].
pub fn new_registration_challenge(
    rp_id: &str,
    origin: &str,
    now: u64,
) -> Result<PasskeyChallenge, AuthError> {
    let parsed = url::Url::parse(origin).map_err(|_| AuthError::Misconfigured)?;
    let host = parsed.host_str().ok_or(AuthError::Misconfigured)?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let local = matches!(host, "localhost" | "127.0.0.1" | "::1");
    if !(parsed.scheme() == "https" || (parsed.scheme() == "http" && local))
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        // Require an actual serialized origin, not a URL that normalizes
        // away empty userinfo, a path, or other non-origin components.
        || origin != parsed.origin().ascii_serialization()
        // This application pins the RP to the exact host, not a parent
        // suffix. Cross-subdomain credentials are intentionally unsupported.
        || rp_id != host
        || (!local && !host.contains('.'))
    {
        return Err(AuthError::Misconfigured);
    }
    Ok(PasskeyChallenge {
        id: random_b64url_32()?,
        rp_id: rp_id.to_string(),
        origin: origin.to_string(),
        issued_at: now,
        expires_at: now.saturating_add(CHALLENGE_TTL_SECS),
        consumed: false,
    })
}

/// Pure ceremony validation/preparation, NOT a database single-use claim.
/// HTTP callers must use [`consume_challenge_atomically`] and complete
/// credential signature verification before issuing any session. The Worker
/// must not treat a passing lifecycle gate alone as WebAuthn authentication.
pub fn verify_and_consume_challenge(
    mut challenge: PasskeyChallenge,
    rp_id: &str,
    origin: &str,
    now: u64,
) -> Result<PasskeyChallenge, AuthError> {
    if challenge.consumed {
        return Err(AuthError::Invalid);
    }
    if now > challenge.expires_at {
        return Err(AuthError::Invalid);
    }
    if challenge
        .rp_id
        .as_bytes()
        .ct_eq(rp_id.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Err(AuthError::Invalid);
    }
    if challenge
        .origin
        .as_bytes()
        .ct_eq(origin.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Err(AuthError::Invalid);
    }
    challenge.consumed = true;
    Ok(challenge)
}

/// Stored WebAuthn credential (public key only — never private material).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasskeyCredential {
    pub cred_id_b64url: String,
    pub user_id: String,
    /// Raw public key bytes (COSE-decoded); signature checks use these.
    pub public_key: Vec<u8>,
    /// Monotonic signature counter for clone detection.
    pub sign_count: u32,
    pub registered_at: u64,
}

// --- Invite codes (admin-issued, migration 0033) -------------------------------

/// Invite kinds (`invite_codes.kind` CHECK in migration 0033). Member
/// invites obey windows/quotas (post-M1); admin invites do not.
pub const INVITE_KIND_ADMIN: &str = "admin";
pub const INVITE_KIND_MEMBER: &str = "member";

/// Audit actions for the invite lifecycle. Audit rows reference the
/// invite by row id in `details`; code plaintext and hashes never
/// appear in audit rows or logs.
pub const INVITE_ISSUED_AUDIT_ACTION: &str = "invite.issued";
pub const INVITE_REDEEMED_AUDIT_ACTION: &str = "invite.redeemed";
pub const INVITE_REVOKED_AUDIT_ACTION: &str = "invite.revoked";

fn default_invite_kind() -> String {
    INVITE_KIND_ADMIN.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRecord {
    /// Surrogate row id (migration 0033); 0 before the row is stored.
    /// Audit rows reference invites by this id, never by code material.
    #[serde(default)]
    pub id: i64,
    /// SHA-256 hex of the code. Plaintext codes are never stored.
    pub code_hash: String,
    /// Legacy spend flag, kept in sync with `redeemed_at` by the claim.
    pub used: bool,
    pub created_at: u64,
    /// Issuer's user id; `None` only for pre-0029 operator-seeded rows.
    #[serde(default)]
    pub issued_by: Option<String>,
    #[serde(default = "default_invite_kind")]
    pub kind: String,
    /// When set, redemption requires signup with this exact normalised
    /// email.
    #[serde(default)]
    pub email: Option<String>,
    /// Required redemption deadline (epoch seconds).
    #[serde(default)]
    pub expires_at: u64,
    #[serde(default)]
    pub redeemed_by: Option<String>,
    #[serde(default)]
    pub redeemed_at: Option<u64>,
    #[serde(default)]
    pub revoked_at: Option<u64>,
    #[serde(default)]
    pub revoked_by: Option<String>,
    /// The invite window a member invite was issued under (post-M1);
    /// admin invites store `None`.
    #[serde(default)]
    pub window_id: Option<i64>,
    /// The issuer's current email, joined at lookup time (not a column)
    /// so the pure prepare can refuse self-invites; `None` when the
    /// issuer is unknown or their account is gone.
    #[serde(default, skip_serializing)]
    pub issuer_email: Option<String>,
}

impl InviteRecord {
    /// A freshly issued, unredeemed invite row (id assigned by the DB).
    pub fn new_issued(
        code_hash: String,
        issued_by: Option<String>,
        kind: &str,
        email: Option<String>,
        created_at: u64,
        expires_at: u64,
    ) -> Self {
        InviteRecord {
            id: 0,
            code_hash,
            used: false,
            created_at,
            issued_by,
            kind: kind.to_string(),
            email,
            expires_at,
            redeemed_by: None,
            redeemed_at: None,
            revoked_at: None,
            revoked_by: None,
            window_id: None,
            issuer_email: None,
        }
    }

    /// Derived lifecycle status for the admin list. Spend keys on
    /// `redeemed_at`, not `redeemed_by`: account deletion nulls the FK
    /// but must never free the code.
    pub fn status(&self, now: u64) -> &'static str {
        if self.redeemed_at.is_some() || self.used {
            "redeemed"
        } else if self.revoked_at.is_some() {
            "revoked"
        } else if self.expires_at <= now {
            "expired"
        } else {
            "active"
        }
    }
}

pub fn hash_invite_code(code: &str) -> String {
    sha256_hex(code)
}

/// Who is redeeming, for [`prepare_invite_redemption`]. `email` is the
/// signup account's normalised email; `issuer_email` comes from the
/// record's join at lookup and is passed explicitly so the pure check
/// and the SQL claim refuse the same self-invite.
#[derive(Debug, Clone, Copy)]
pub struct InviteRedemption<'a> {
    pub user_id: &'a str,
    pub email: &'a str,
    pub issuer_email: Option<&'a str>,
    pub now: u64,
}

/// Pure invite validation/preparation, NOT a database single-use claim.
/// Every refusal is the same [`AuthError::Invalid`] — unknown, expired,
/// revoked, spent, email-mismatched and self-invite codes are one
/// indistinguishable class, so the check is never an oracle for which
/// predicate fired. Signup callers must use
/// [`consume_invite_atomically`] inside their signup transaction; two
/// in-memory snapshots are not a uniqueness boundary. The prepared
/// record carries the spend stamps the conditional claim binds.
pub fn prepare_invite_redemption(
    mut record: InviteRecord,
    presented_code: &str,
    redemption: &InviteRedemption<'_>,
) -> Result<InviteRecord, AuthError> {
    let presented_hash = sha256_hex(presented_code);
    if presented_hash
        .as_bytes()
        .ct_eq(record.code_hash.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Err(AuthError::Invalid);
    }
    if record.used || record.redeemed_at.is_some() {
        return Err(AuthError::Invalid);
    }
    if record.revoked_at.is_some() {
        return Err(AuthError::Invalid);
    }
    if record.expires_at <= redemption.now {
        return Err(AuthError::Invalid);
    }
    if let Some(bound_email) = record.email.as_deref() {
        if !bound_email.eq_ignore_ascii_case(redemption.email) {
            return Err(AuthError::Invalid);
        }
    }
    // Self-invite resistance: the issuer may not redeem their own
    // invite under their own email (or their own pre-existing row).
    if record.issued_by.as_deref() == Some(redemption.user_id) {
        return Err(AuthError::Invalid);
    }
    if let Some(issuer_email) = redemption.issuer_email {
        if issuer_email.eq_ignore_ascii_case(redemption.email) {
            return Err(AuthError::Invalid);
        }
    }
    record.used = true;
    record.redeemed_by = Some(redemption.user_id.to_string());
    record.redeemed_at = Some(redemption.now);
    Ok(record)
}

// --- Invite issuance + redemption throttle (F-HARD-01 item 4) -------------------

/// Invite-code entropy: 128 bits, crypto-random, unpadded-base64url
/// (22 chars). Returns the plaintext code; callers store only
/// [`hash_invite_code`] of it and deliver the plaintext to the inviter
/// once. Human-guessable or sequential codes are banned: at 128 bits
/// enumeration is infeasible and the throttle below is defense in depth.
///
/// Fail-closed: secure-randomness failure returns
/// [`AuthError::RandomnessUnavailable`] (HTTP 503). The success path is
/// byte-identical: the same 16 entropy bytes through the same
/// [`crate::b64::encode`].
pub fn generate_invite_code() -> Result<String, AuthError> {
    let bytes = crate::random::secure_random_bytes::<16>()?;
    Ok(crate::b64::encode(&bytes))
}

/// Sliding window for the invite-redemption rate limit (seconds).
pub const INVITE_REDEMPTION_WINDOW_SECS: u64 = 300;
/// Max redemption attempts per key per [`INVITE_REDEMPTION_WINDOW_SECS`].
/// Generous for humans retrying a pasted code, tight for scripts: 10
/// attempts per 5 minutes per IP (or per account once authenticated).
pub const INVITE_REDEMPTION_MAX_ATTEMPTS: u32 = 10;

/// Second redemption budget, keyed by the signup email: rotating IPs
/// and pre-created account rows must not multiply guesses against one
/// invited address. Same generic denial shape as the per-client key —
/// the response never reveals which limit fired.
pub const INVITE_EMAIL_WINDOW_SECS: u64 = 3_600;
/// Max redemption attempts per email per [`INVITE_EMAIL_WINDOW_SECS`].
pub const INVITE_EMAIL_MAX_ATTEMPTS: u32 = 5;

/// Cutoff for the sliding redemption window: attempts at or before this
/// timestamp no longer count. The HTTP slice prunes stored attempt rows at
/// or below it.
pub fn invite_attempt_retention_cutoff(now_secs: u64) -> u64 {
    now_secs.saturating_sub(INVITE_REDEMPTION_WINDOW_SECS)
}

/// Sliding-window gate for invite redemption.
///
/// `recent_attempt_secs` MUST come from a shared store (Postgres/KV) keyed
/// by client IP (anonymous signup) or account — never an in-memory list:
/// serverless isolates have no shared memory and a restart would wipe the
/// counter. The HTTP slice MUST:
/// - load the key's attempt timestamps,
/// - deny with the generic [`AuthError::Invalid`] when this returns false
///   (indistinguishable from a bad code — no throttle oracle),
/// - otherwise persist the new attempt timestamp atomically BEFORE running
///   the conditional invite claim (a check-then-insert race fails open
///   under concurrency; use a conditional write / transaction).
///
/// Fail-closed on clock skew: timestamps in the future count against the
/// caller.
pub fn invite_attempt_allowed(recent_attempt_secs: &[u64], now_secs: u64) -> bool {
    let in_window = recent_attempt_secs
        .iter()
        .filter(|ts| now_secs.saturating_sub(**ts) < INVITE_REDEMPTION_WINDOW_SECS)
        .count();
    in_window < INVITE_REDEMPTION_MAX_ATTEMPTS as usize
}

/// A validated candidate for one atomic, parameterized Postgres claim.
/// Bind the listed parameters; never interpolate a record into SQL.
pub enum SingleUseClaim<'a> {
    /// Parameters: token_hash, user_id, email, expires_at, now.
    MagicLink {
        record: &'a MagicLinkRecord,
        now: u64,
    },
    /// Parameters: id, rp_id, origin, expires_at, now.
    Challenge {
        record: &'a PasskeyChallenge,
        now: u64,
    },
    /// Parameters: code_hash, redeemed_by, redeemed_at — the spend
    /// stamps [`prepare_invite_redemption`] wrote onto the record.
    Invite { record: &'a InviteRecord },
}

impl SingleUseClaim<'_> {
    /// Execute this statement once and count its returned rows. The primary
    /// key + conditional flag transition makes concurrent claims exclusive.
    pub fn statement(&self) -> &'static str {
        match self {
            Self::MagicLink { .. } => "UPDATE magic_link_tokens SET used = TRUE WHERE token_hash = $1 AND user_id = $2 AND email = $3 AND expires_at = $4 AND expires_at > $5 AND used = FALSE RETURNING token_hash",
            Self::Challenge { .. } => "UPDATE auth_challenges SET consumed = TRUE WHERE id = $1 AND rp_id = $2 AND origin = $3 AND expires_at = $4 AND expires_at > $5 AND consumed = FALSE RETURNING id",
            // One conditional UPDATE carries every spend predicate, so
            // revocation, expiry and a competing redemption all win or
            // lose atomically against this claim. The email predicates
            // read the redeemer's `users` row ($2) in-database — the
            // same values `prepare_invite_redemption` checked on the
            // snapshot, re-proven at claim time.
            Self::Invite { .. } => "UPDATE invite_codes SET used = TRUE, redeemed_by = $2, redeemed_at = $3 WHERE code_hash = $1 AND used = FALSE AND redeemed_at IS NULL AND revoked_at IS NULL AND expires_at > $3 AND (email IS NULL OR EXISTS (SELECT 1 FROM users WHERE id = $2 AND lower(email) = lower(invite_codes.email))) AND NOT EXISTS (SELECT 1 FROM users issuer JOIN users redeemer ON lower(redeemer.email) = lower(issuer.email) WHERE issuer.id = invite_codes.issued_by AND redeemer.id = $2) RETURNING code_hash",
        }
    }
}

/// Worker/Hyperdrive boundary. Implementations must execute the conditional
/// UPDATE from [`SingleUseClaim::statement`] and return the number of rows
/// it returned, NOT perform SELECT + unconditional UPDATE. Return an error
/// on storage failure. Success is usable only after the transaction commits;
/// signup/session insertion may share that transaction, but nothing may be
/// sent to the client before commit. An in-memory flag is not an implementation.
pub trait SingleUseStore {
    fn claim(
        &self,
        candidate: SingleUseClaim<'_>,
    ) -> impl std::future::Future<Output = Result<u64, AuthError>>;
}

/// HTTP magic-link entry: validation plus an exclusive database claim.
/// Zero rows (replay/expiry), multiple rows, or a store error all fail closed.
pub async fn consume_magic_link_atomically(
    record: MagicLinkRecord,
    presented_token: &str,
    now: u64,
    store: &impl SingleUseStore,
) -> Result<ConsumedMagicLink, AuthError> {
    let prepared = consume_magic_link(record, presented_token, now)?;
    if store
        .claim(SingleUseClaim::MagicLink {
            record: &prepared.used_record,
            now,
        })
        .await?
        != 1
    {
        return Err(AuthError::Invalid);
    }
    Ok(prepared)
}

/// HTTP ceremony gate: exclusive challenge claim; still requires WebAuthn
/// signature verification before authentication (not provided in this slice).
pub async fn consume_challenge_atomically(
    challenge: PasskeyChallenge,
    rp_id: &str,
    origin: &str,
    now: u64,
    store: &impl SingleUseStore,
) -> Result<PasskeyChallenge, AuthError> {
    let prepared = verify_and_consume_challenge(challenge, rp_id, origin, now)?;
    if store
        .claim(SingleUseClaim::Challenge {
            record: &prepared,
            now,
        })
        .await?
        != 1
    {
        return Err(AuthError::Invalid);
    }
    Ok(prepared)
}

/// Signup invite gate: validation plus an exclusive database claim.
pub async fn consume_invite_atomically(
    record: InviteRecord,
    presented_code: &str,
    redemption: &InviteRedemption<'_>,
    store: &impl SingleUseStore,
) -> Result<InviteRecord, AuthError> {
    let prepared = prepare_invite_redemption(record, presented_code, redemption)?;
    if store
        .claim(SingleUseClaim::Invite { record: &prepared })
        .await?
        != 1
    {
        return Err(AuthError::Invalid);
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_link_hash_is_sha256_hex() {
        let (token, record) = issue_magic_link("u", "a@b.c", 0).expect("issue");
        assert_eq!(record.token_hash, sha256_hex(&token));
        assert_eq!(record.token_hash.len(), 64);
    }

    #[test]
    fn magic_link_tokens_and_challenge_ids_are_random() {
        // Pin the secure-randomness fill behind `random_b64url_32`: a
        // dropped fill would make the magic-link token a constant login
        // credential (43 'A's) and every passkey challenge id identical.
        let (token_a, _) = issue_magic_link("u", "a@b.c", 0).expect("issue");
        let (token_b, _) = issue_magic_link("u", "a@b.c", 0).expect("issue");
        assert_ne!(token_a, token_b, "tokens must be unique per issue");
        let zero_token = crate::b64::encode(&[0u8; 32]);
        assert_ne!(token_a, zero_token);
        assert_ne!(token_b, zero_token);
        assert_eq!(
            crate::b64::decode(&token_a).expect("token decodes").len(),
            32,
            "exactly 256 bits of entropy"
        );

        let challenge_a =
            new_registration_challenge("nntune.example.com", "https://nntune.example.com", 0)
                .expect("challenge");
        let challenge_b =
            new_registration_challenge("nntune.example.com", "https://nntune.example.com", 0)
                .expect("challenge");
        assert_ne!(
            challenge_a.id, challenge_b.id,
            "challenge ids must be unique per ceremony"
        );
        assert_ne!(challenge_a.id, zero_token);
        assert_ne!(challenge_b.id, zero_token);
    }

    #[test]
    fn malformed_origin_is_misconfigured() {
        assert!(matches!(
            new_registration_challenge("nntune.example.com", "https://", 0),
            Err(AuthError::Misconfigured)
        ));
    }

    #[test]
    fn hostless_origin_is_misconfigured() {
        assert!(matches!(
            new_registration_challenge("nntune.example.com", "file:///", 0),
            Err(AuthError::Misconfigured)
        ));
    }

    #[test]
    fn invite_single_use() {
        let redemption = InviteRedemption {
            user_id: "user-new",
            email: "new@example.com",
            issuer_email: Some("issuer@example.com"),
            now: 1_700_000_000,
        };
        let record = InviteRecord::new_issued(
            hash_invite_code("INVITE-1"),
            Some("user-issuer".to_string()),
            INVITE_KIND_ADMIN,
            None,
            1_699_999_000,
            1_700_100_000,
        );
        let spent = prepare_invite_redemption(record, "INVITE-1", &redemption).unwrap();
        assert!(spent.used);
        assert_eq!(spent.redeemed_by.as_deref(), Some("user-new"));
        assert_eq!(spent.redeemed_at, Some(redemption.now));
        assert!(
            prepare_invite_redemption(spent, "INVITE-1", &redemption).is_err(),
            "a prepared-spent record never prepares twice"
        );
    }

    #[test]
    fn invite_prepare_refuses_every_invalid_class_with_one_error() {
        let now = 1_700_000_000u64;
        let redemption = InviteRedemption {
            user_id: "user-new",
            email: "new@example.com",
            issuer_email: Some("issuer@example.com"),
            now,
        };
        let base = || {
            InviteRecord::new_issued(
                hash_invite_code("INVITE-2"),
                Some("user-issuer".to_string()),
                INVITE_KIND_ADMIN,
                None,
                now - 100,
                now + 100,
            )
        };
        // Wrong code.
        assert_eq!(
            prepare_invite_redemption(base(), "WRONG", &redemption),
            Err(AuthError::Invalid)
        );
        // Expired (deadline at or before now).
        let mut expired = base();
        expired.expires_at = now;
        assert_eq!(
            prepare_invite_redemption(expired, "INVITE-2", &redemption),
            Err(AuthError::Invalid)
        );
        // Revoked.
        let mut revoked = base();
        revoked.revoked_at = Some(now - 1);
        assert_eq!(
            prepare_invite_redemption(revoked, "INVITE-2", &redemption),
            Err(AuthError::Invalid)
        );
        // Spent via the redeemed stamp alone (used flag out of sync).
        let mut spent = base();
        spent.redeemed_at = Some(now - 1);
        assert_eq!(
            prepare_invite_redemption(spent, "INVITE-2", &redemption),
            Err(AuthError::Invalid)
        );
        // Email-bound to a different address.
        let mut bound = base();
        bound.email = Some("other@example.com".to_string());
        assert_eq!(
            prepare_invite_redemption(bound, "INVITE-2", &redemption),
            Err(AuthError::Invalid)
        );
        // Email-bound to the redeemer (case-insensitive) passes.
        let mut bound_ok = base();
        bound_ok.email = Some("NEW@example.com".to_string());
        assert!(prepare_invite_redemption(bound_ok, "INVITE-2", &redemption).is_ok());
        // Self-invite: redeemer's email is the issuer's.
        let self_redemption = InviteRedemption {
            email: "Issuer@Example.com",
            ..redemption
        };
        assert_eq!(
            prepare_invite_redemption(base(), "INVITE-2", &self_redemption),
            Err(AuthError::Invalid)
        );
        // Self-invite: redeemer's row is the issuer's row.
        let same_row = InviteRedemption {
            user_id: "user-issuer",
            email: "different@example.com",
            issuer_email: None,
            now,
        };
        assert_eq!(
            prepare_invite_redemption(base(), "INVITE-2", &same_row),
            Err(AuthError::Invalid)
        );
    }

    #[test]
    fn invite_status_derives_from_spend_stamps_not_links() {
        let now = 1_700_000_000u64;
        let mut record = InviteRecord::new_issued(
            hash_invite_code("INVITE-3"),
            Some("user-issuer".to_string()),
            INVITE_KIND_ADMIN,
            None,
            now - 100,
            now + 100,
        );
        assert_eq!(record.status(now), "active");
        record.expires_at = now;
        assert_eq!(record.status(now), "expired");
        record.revoked_at = Some(now - 1);
        assert_eq!(
            record.status(now),
            "revoked",
            "revoked wins over expired in display"
        );
        // Deleted redeemer: FK nulled, stamp kept — still redeemed.
        record.redeemed_at = Some(now - 2);
        record.redeemed_by = None;
        assert_eq!(record.status(now), "redeemed");
    }

    #[test]
    fn invite_codes_are_128_bit_random() {
        let a = generate_invite_code().expect("issue");
        let b = generate_invite_code().expect("issue");
        for code in [&a, &b] {
            assert_eq!(code.len(), 22, "16 bytes encode to 22 unpadded chars");
            assert!(
                code.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
                "url-safe alphabet only: {code:?}"
            );
            assert_eq!(
                crate::b64::decode(code)
                    .expect("generated code decodes")
                    .len(),
                16,
                "exactly 128 bits of entropy"
            );
            // Generated codes round-trip through the at-rest hash.
            assert_eq!(hash_invite_code(code).len(), 64);
        }
        assert_ne!(a, b, "codes must be unique per generation");
    }

    #[test]
    fn invite_redemption_throttle_trips() {
        let now = 1_700_000_000u64;
        let full: Vec<u64> = (0..10).map(|i| now - i).collect();
        assert!(
            !invite_attempt_allowed(&full, now),
            "10 attempts in the window must trip the limit"
        );
        let nine: Vec<u64> = (0..9).map(|i| now - i).collect();
        assert!(
            invite_attempt_allowed(&nine, now),
            "9 attempts in the window must pass"
        );
        assert!(
            invite_attempt_allowed(&[now - 300, now - 3_600], now),
            "attempts at/outside the window edge must age out"
        );
        assert!(
            !invite_attempt_allowed(&[now - 299; 10], now),
            "attempts just inside the window must count"
        );
        assert!(invite_attempt_allowed(&[], now), "first attempt must pass");
        assert!(
            !invite_attempt_allowed(&[now + 1_000; 10], now),
            "future timestamps must fail closed, not bypass"
        );
        assert_eq!(invite_attempt_retention_cutoff(now), now - 300);
        assert_eq!(invite_attempt_retention_cutoff(30), 0);
    }

    #[test]
    fn randomness_unavailable_is_503_service_unavailable() {
        let err = AuthError::RandomnessUnavailable;
        assert_eq!(err.to_string(), "service unavailable");
        let api = crate::api_error::ApiError::from(err);
        assert_eq!(
            (api.code, api.status, api.retryable),
            ("service_unavailable", 503, true)
        );
        assert_eq!(api.message, "service unavailable");
        // The shared helper converts losslessly into this variant.
        assert_eq!(
            AuthError::from(crate::random::CryptoError::RandomnessUnavailable),
            AuthError::RandomnessUnavailable
        );
    }
}
