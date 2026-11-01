//! Caller grants for the `ingest` Worker (TWO-331).
//!
//! Every `/v1/ingest/*` route requires `Authorization: Bearer <grant>`
//! (`workers/ingest/src/caller_auth.rs`). This Worker authenticates the
//! session and mints the grant for the session user only; ingest acts on
//! the grant's signed `user_id`, never a body field.
//!
//! Token shape (byte-exact with ingest's verifier):
//! `v1.<hex(payload)>.<hex(hmac-sha256(payload_hex))>` where the payload is
//! `user_id|ingest:write|exp_unix`, keyed with the [`CALLER_SECRET_VAR`]
//! Worker secret, which is provisioned with the same value on both
//! Workers. Lifetimes clamp to [`MAX_GRANT_TTL`], mirroring the stream
//! play grant's `issue_at`. Both crates pin the shared vectors in
//! `fixtures/ingest-caller-grant/vectors.json`.
//!
//! Minting fails closed: a short or absent secret, or a user id ingest
//! would reject, yields an error instead of a token.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Worker secret shared with the `ingest` Worker
/// (`wrangler secret put INGEST_CALLER_SECRET` on both; never a `[vars]`
/// entry).
pub const CALLER_SECRET_VAR: &str = "INGEST_CALLER_SECRET";

/// Minimum secret length in bytes; ingest refuses every caller below it.
pub const MIN_SECRET_BYTES: usize = 32;

/// The only scope the ingest routes accept.
pub const INGEST_WRITE_SCOPE: &str = "ingest:write";

/// Maximum grant lifetime: 15 minutes, matching ingest's verifier cap.
pub const MAX_GRANT_TTL: u64 = 900;

/// Why no grant was minted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MintError {
    /// The secret is absent or shorter than [`MIN_SECRET_BYTES`].
    NotConfigured,
    /// The user id is empty, over 128 bytes, or contains `:`, `|` or NUL.
    InvalidUser,
}

/// Mint an `ingest:write` grant for `user_id`, valid for
/// `min(ttl_secs, MAX_GRANT_TTL)` seconds from `now_unix` (saturating).
pub fn issue_at(
    user_id: &str,
    now_unix: u64,
    ttl_secs: u64,
    secret: Option<&str>,
) -> Result<String, MintError> {
    let secret = match secret {
        Some(s) if s.len() >= MIN_SECRET_BYTES => s.as_bytes(),
        _ => return Err(MintError::NotConfigured),
    };
    if !valid_user_id(user_id) {
        return Err(MintError::InvalidUser);
    }
    let exp_unix = now_unix.saturating_add(ttl_secs.min(MAX_GRANT_TTL));
    let payload_hex = hex_encode(format!("{user_id}|{INGEST_WRITE_SCOPE}|{exp_unix}").as_bytes());
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| MintError::NotConfigured)?;
    mac.update(payload_hex.as_bytes());
    let sig = mac.finalize().into_bytes();
    Ok(format!("v1.{payload_hex}.{}", hex_encode(&sig)))
}

/// The `Authorization` header value for an ingest call.
pub fn bearer(grant: &str) -> String {
    format!("Bearer {grant}")
}

/// Ingest's `validate_user_id`, plus `|` (the payload separator, which
/// ingest's parser rejects as malformed).
fn valid_user_id(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 128
        && !raw.contains(':')
        && !raw.contains('|')
        && !raw.contains('\0')
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0x0f)] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret-test-secret-test-secret!";
    const NOW: u64 = 1_800_000_000;

    fn payload(token: &str) -> String {
        let hex = token
            .strip_prefix("v1.")
            .and_then(|rest| rest.split_once('.'))
            .map(|(p, _)| p)
            .expect("token shape");
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect();
        String::from_utf8(bytes).expect("utf8")
    }

    #[test]
    fn grant_carries_user_scope_and_expiry() {
        let token = issue_at("user_01", NOW, 600, Some(SECRET)).unwrap();
        assert_eq!(
            payload(&token),
            format!("user_01|ingest:write|{}", NOW + 600)
        );
    }

    #[test]
    fn lifetime_clamps_to_the_cap_and_saturates() {
        let token = issue_at("user_01", NOW, 86_400, Some(SECRET)).unwrap();
        assert_eq!(
            payload(&token),
            format!("user_01|ingest:write|{}", NOW + MAX_GRANT_TTL)
        );
        let token = issue_at("user_01", u64::MAX - 5, 600, Some(SECRET)).unwrap();
        assert_eq!(
            payload(&token),
            format!("user_01|ingest:write|{}", u64::MAX)
        );
    }

    #[test]
    fn missing_or_short_secret_fails_closed() {
        assert_eq!(
            issue_at("user_01", NOW, 600, None),
            Err(MintError::NotConfigured)
        );
        assert_eq!(
            issue_at("user_01", NOW, 600, Some(&SECRET[..MIN_SECRET_BYTES - 1])),
            Err(MintError::NotConfigured)
        );
    }

    #[test]
    fn user_ids_ingest_would_reject_are_never_minted() {
        let long = "u".repeat(129);
        for bad in ["", "user:01", "user|01", "user\0", long.as_str()] {
            assert_eq!(
                issue_at(bad, NOW, 600, Some(SECRET)),
                Err(MintError::InvalidUser),
                "{bad:?}"
            );
        }
        assert!(issue_at(&"u".repeat(128), NOW, 600, Some(SECRET)).is_ok());
    }

    #[test]
    fn bearer_header_shape() {
        assert_eq!(bearer("v1.ab.cd"), "Bearer v1.ab.cd");
    }
}
