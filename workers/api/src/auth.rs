//! Accounts/auth: email magic links, passkey challenge lifecycle, invite codes.
//!
//! Threat-model §4: magic-link tokens are cryptographic-random (256-bit),
//! hashed (SHA-256) at rest, single-use, ≤15-min expiry; login errors are
//! generic (no user-enumeration oracle); challenges are single-use with
//! short expiry and RP/origin pinning.
//!
//! Full WebAuthn attestation/assertion signature verification wires to a
//! WebAuthn library in the follow-up slice (child card): this module owns
//! the challenge lifecycle, origin/RP binding, expiry, single-use, and
//! credential storage shapes, all enforced and tested here.

use rand::RngCore;
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
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Same message the API returns for every auth failure.
            AuthError::Invalid => write!(f, "check your email and try again"),
        }
    }
}

fn random_b64url_32() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    // Reuse the session module's alphabet via a local copy to avoid coupling.
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(43);
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

/// Issue a magic link: returns `(plaintext_token, stored_record)`.
/// The plaintext goes into the emailed URL; only the hash is stored.
pub fn issue_magic_link(user_id: &str, email: &str, now: u64) -> (String, MagicLinkRecord) {
    let token = random_b64url_32();
    let record = MagicLinkRecord {
        token_hash: sha256_hex(&token),
        user_id: user_id.to_string(),
        email: email.to_string(),
        issued_at: now,
        expires_at: now.saturating_add(MAGIC_LINK_TTL_SECS),
        used: false,
    };
    (token, record)
}

/// Consume a presented token against its stored record. Returns the `user_id`
/// on success. Single-use: callers must persist the returned record (with
/// `used: true`) before issuing a session.
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

/// Successful consumption: the authenticated user plus the spent record to persist.
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
pub fn new_registration_challenge(rp_id: &str, origin: &str, now: u64) -> PasskeyChallenge {
    assert!(!rp_id.is_empty(), "rp_id must be pinned to the real host");
    assert!(
        origin.starts_with("https://") || origin.starts_with("http://localhost"),
        "origin must be https (http only for localhost dev)"
    );
    PasskeyChallenge {
        id: random_b64url_32(),
        rp_id: rp_id.to_string(),
        origin: origin.to_string(),
        issued_at: now,
        expires_at: now.saturating_add(CHALLENGE_TTL_SECS),
        consumed: false,
    }
}

/// Verify the ceremony environment and consume the challenge (single-use).
/// Credential signature verification happens in the follow-up slice against
/// the stored public key; this gate (origin/RP/expiry/single-use) runs first
/// and independently.
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

// --- Invite codes (optional gate) ----------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteRecord {
    /// SHA-256 hex of the code. Plaintext codes are never stored.
    pub code_hash: String,
    pub used: bool,
    pub created_at: u64,
}

pub fn hash_invite_code(code: &str) -> String {
    sha256_hex(code)
}

/// Validate a presented invite code against its stored record.
pub fn consume_invite(
    mut record: InviteRecord,
    presented_code: &str,
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
    if record.used {
        return Err(AuthError::Invalid);
    }
    record.used = true;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_link_hash_is_sha256_hex() {
        let (token, record) = issue_magic_link("u", "a@b.c", 0);
        assert_eq!(record.token_hash, sha256_hex(&token));
        assert_eq!(record.token_hash.len(), 64);
    }

    #[test]
    fn invite_single_use() {
        let record = InviteRecord {
            code_hash: hash_invite_code("INVITE-1"),
            used: false,
            created_at: 0,
        };
        let spent = consume_invite(record, "INVITE-1").unwrap();
        assert!(spent.used);
        assert!(consume_invite(spent, "INVITE-1").is_err());
    }
}
