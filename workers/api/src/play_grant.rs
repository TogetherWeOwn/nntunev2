//! Play grants for the `stream` Worker: this Worker mints them at the
//! grant-issuance gate, the stream Worker verifies one per play before
//! serving bytes.
//!
//! Token shape (byte-exact mirror of `workers/stream/src/grant.rs`,
//! which is the format definition): `v2.<base64url(payload)>.
//! <base64url(hmac-sha256)>` where the HMAC covers the base64url payload
//! segment exactly as spelled, and the payload is
//! `user_id|track_id|play_id|lease_epoch|exp_unix`. Keyed with the
//! [`PLAY_GRANT_SECRET_VAR`] Worker secret, provisioned with the same
//! value on both Workers. The stream side reads that secret as a string
//! and keys the HMAC with its raw bytes, so this side must too — the
//! value is never base64-decoded here. Lifetimes clamp to
//! [`MAX_GRANT_TTL`], mirroring the verifier's cap. Both crates pin the
//! shared vectors in `fixtures/play-grant/vectors.json` (generated with
//! Python's hmac, independent of either implementation).
//!
//! Minting fails closed: a short or absent secret, or an identity the
//! stream verifier would reject as unmeterable, yields an error instead
//! of a token. Identity validity is decided by
//! [`crate::ledger::derive_play_idempotency_key`] itself — the same
//! field rule (`[A-Za-z0-9_-]`, user ≤ 128 bytes, track/play ≤ 512
//! bytes) the verifier enforces — so a grant minted here can always be
//! metered by construction, with no second copy of the rule to drift.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Worker secret shared with the `stream` Worker
/// (`wrangler secret put PLAY_GRANT_SECRET` on both; never a `[vars]`
/// entry). Used as raw string bytes on both sides.
pub const PLAY_GRANT_SECRET_VAR: &str = "PLAY_GRANT_SECRET";

/// Minimum secret length in bytes; below it no grant is minted.
pub const MIN_SECRET_BYTES: usize = 32;

/// Maximum grant lifetime: 15 minutes, matching the stream verifier's
/// cap (`grant::MAX_GRANT_TTL`); longer requests clamp to it.
pub const MAX_GRANT_TTL: u64 = 900;

/// Why no grant was minted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MintError {
    /// The secret is absent or shorter than [`MIN_SECRET_BYTES`].
    NotConfigured,
    /// An identity field fails the ledger field rule, so the stream
    /// verifier would refuse the minted token as malformed.
    InvalidIdentity,
}

/// A minted grant: the token plus its expiry (the handler reports both;
/// the client refreshes ahead of `exp_unix` for a continuing play).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedGrant {
    pub token: String,
    pub exp_unix: u64,
}

/// Mint a `v2` play grant for (`user_id`, `track_id`, `play_id`) at
/// `lease_epoch`, valid for `min(ttl_secs, MAX_GRANT_TTL)` seconds from
/// `now_unix` (saturating). Byte-exact with the stream Worker's
/// `issue_v2_at`; pinned by `fixtures/play-grant/vectors.json` on both
/// sides.
pub fn issue_v2_at(
    user_id: &str,
    track_id: &str,
    play_id: &str,
    lease_epoch: u64,
    now_unix: u64,
    ttl_secs: u64,
    secret: Option<&[u8]>,
) -> Result<IssuedGrant, MintError> {
    let secret = match secret {
        Some(s) if s.len() >= MIN_SECRET_BYTES => s,
        _ => return Err(MintError::NotConfigured),
    };
    if crate::ledger::derive_play_idempotency_key(user_id, track_id, play_id).is_err() {
        return Err(MintError::InvalidIdentity);
    }
    let exp_unix = now_unix.saturating_add(ttl_secs.min(MAX_GRANT_TTL));
    let payload = format!("{user_id}|{track_id}|{play_id}|{lease_epoch}|{exp_unix}");
    let payload_b64 = crate::b64::encode(payload.as_bytes());
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| MintError::NotConfigured)?;
    mac.update(payload_b64.as_bytes());
    let sig = mac.finalize().into_bytes();
    Ok(IssuedGrant {
        token: format!("v2.{payload_b64}.{}", crate::b64::encode(&sig)),
        exp_unix,
    })
}

/// The `Authorization` header value for a play fetch.
pub fn bearer(grant: &str) -> String {
    format!("Bearer {grant}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"play-grant-test-secret-0123456789ab";
    const NOW: u64 = 1_800_000_000;

    #[test]
    fn shared_vectors_mint_byte_exactly() {
        // The same file pins the stream side (issue + verify), so a
        // format drift in either crate fails one of the two suites.
        let doc: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/play-grant/vectors.json"))
                .expect("vectors parse");
        let vectors = doc["vectors"].as_array().expect("vectors array");
        assert!(!vectors.is_empty(), "vector file must not be empty");
        for v in vectors {
            let name = v["name"].as_str().expect("name");
            let issued = issue_v2_at(
                v["user_id"].as_str().expect("user"),
                v["track_id"].as_str().expect("track"),
                v["play_id"].as_str().expect("play"),
                v["lease_epoch"].as_u64().expect("epoch"),
                v["now_unix"].as_u64().expect("now"),
                v["ttl_secs"].as_u64().expect("ttl"),
                Some(v["secret"].as_str().expect("secret").as_bytes()),
            )
            .unwrap_or_else(|e| panic!("{name}: mint failed: {e:?}"));
            assert_eq!(
                issued.token,
                v["token"].as_str().expect("token"),
                "{name}: token must match the shared vector byte-exactly"
            );
            assert_eq!(
                issued.exp_unix,
                v["exp_unix"].as_u64().expect("exp"),
                "{name}: expiry (ttl clamped to the cap)"
            );
        }
    }

    #[test]
    fn short_or_absent_secret_fails_closed() {
        for secret in [None, Some(&b""[..]), Some(&b"short"[..])] {
            assert_eq!(
                issue_v2_at("user_01", "track-9", "play-7", 1, NOW, 600, secret),
                Err(MintError::NotConfigured),
            );
        }
        // Exactly at the minimum mints.
        assert!(issue_v2_at(
            "user_01",
            "track-9",
            "play-7",
            1,
            NOW,
            600,
            Some(&[7u8; 32][..])
        )
        .is_ok());
    }

    #[test]
    fn unmeterable_identities_are_refused() {
        // Each of these would make the stream verifier reject the token
        // as malformed (field rule / byte caps), so minting refuses too.
        for (user, track, play) in [
            ("", "track-9", "play-7"),
            ("user|01", "track-9", "play-7"),
            ("user 01", "track-9", "play-7"),
            ("user_01", "track/9", "play-7"),
            ("user_01", "track-9", "play.7"),
            ("user_01", "track-9", ""),
        ] {
            assert_eq!(
                issue_v2_at(user, track, play, 1, NOW, 600, Some(SECRET)),
                Err(MintError::InvalidIdentity),
                "({user:?}, {track:?}, {play:?})"
            );
        }
        let long_user = "u".repeat(129);
        let long_track = "t".repeat(513);
        assert_eq!(
            issue_v2_at(&long_user, "track-9", "play-7", 1, NOW, 600, Some(SECRET)),
            Err(MintError::InvalidIdentity)
        );
        assert_eq!(
            issue_v2_at("user_01", &long_track, "play-7", 1, NOW, 600, Some(SECRET)),
            Err(MintError::InvalidIdentity)
        );
    }

    #[test]
    fn ttl_clamps_and_saturates() {
        let issued = issue_v2_at(
            "user_01",
            "track-9",
            "play-7",
            1,
            NOW,
            u64::MAX,
            Some(SECRET),
        )
        .expect("mint");
        assert_eq!(issued.exp_unix, NOW + MAX_GRANT_TTL);
        let issued = issue_v2_at(
            "user_01",
            "track-9",
            "play-7",
            1,
            u64::MAX - 10,
            600,
            Some(SECRET),
        )
        .expect("mint");
        assert_eq!(issued.exp_unix, u64::MAX, "saturating near u64::MAX");
    }

    #[test]
    fn bearer_wraps_the_token() {
        assert_eq!(bearer("v2.x.y"), "Bearer v2.x.y");
    }
}
