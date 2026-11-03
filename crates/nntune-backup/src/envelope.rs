//! Drill-local v2 field envelope for per-file media keys.
//!
//! Byte-compatible with the ADR 0002 rule-1 construction implemented by
//! `workers/api/src/providers.rs` (`aad_v2`): the same `u32` big-endian
//! length-prefixed framing over
//! `(domain, purpose, table, field, owner_scope, row_key,
//! envelope_version, payload_format)` plus `kek_version` (u32
//! big-endian) for the DEK wrap only. The pipeline card that implements
//! the production `track_sources.nzb_ref` seal MUST produce
//! byte-compatible envelopes; the `aad_matches_production_construction`
//! test pins the framing.
//!
//! Drill-only difference: this module seals synthetic drill keys under
//! ephemeral drill KEKs. It never reads production key material, and the
//! [`Envelope`] struct carries no `Debug` derive so key-adjacent bytes
//! cannot land in a log line.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Value bound into the drill `nzb_ref_envelope_version` column: the
/// ADR 0002 rule-1 v2 field envelope (schema `CHECK (... = 2)`).
pub const ENVELOPE_VERSION_V2: u16 = 2;
/// Drill payload format: opaque media-key payload v1.
pub const PAYLOAD_FORMAT_MEDIA_KEY_V1: u16 = 1;
/// GCM nonce length (schema `CHECK (octet_length(..) = 12)`).
pub const NONCE_LEN: usize = 12;
/// Wrapped-DEK length: 32-byte DEK + 16-byte GCM tag (schema `CHECK
/// (octet_length(nzb_ref_dek_wrapped) = 48)`).
pub const WRAPPED_DEK_LEN: usize = 48;
/// Per-file media (DEK) key length.
pub const DEK_LEN: usize = 32;
/// KEK length.
pub const KEK_LEN: usize = 32;

/// AAD domain pinning the v2 envelope family (ADR 0002 rule 1).
const AAD_DOMAIN: &str = "nntune-envelope-v2";
/// AAD purpose pins.
const PURPOSE_DEK: &str = "dek";
const PURPOSE_PAYLOAD: &str = "payload";
/// AAD table/field pins for the posted-file NZB reference envelope.
const AAD_TABLE: &str = "track_sources";
const AAD_FIELD: &str = "nzb_ref";

/// Envelope failure modes. Authentication failures and unknown versions
/// collapse to [`EnvelopeError::Crypto`]: the caller learns nothing
/// about which step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeError {
    /// Authentication failure, bad key length, or unknown version.
    Crypto,
    /// Stored-shape failure (short/long wrapped DEK, negative versions).
    CorruptStoredData,
    /// OS randomness unavailable; fail closed, never fall back.
    RandomnessUnavailable,
}

impl core::fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EnvelopeError::Crypto => write!(f, "envelope unavailable"),
            EnvelopeError::CorruptStoredData => write!(f, "corrupt stored envelope"),
            EnvelopeError::RandomnessUnavailable => write!(f, "randomness unavailable"),
        }
    }
}

/// Per-row data-encrypting key, cleared on drop. No `Debug`: key
/// material must never be printable.
#[derive(Zeroize, ZeroizeOnDrop)]
struct Dek([u8; DEK_LEN]);

/// Sealed v2 envelope: raw bytes for the `BYTEA` columns. No `Debug`
/// derive: this struct carries key material adjacent to ciphertext.
pub struct Envelope {
    pub dek_wrapped_nonce: [u8; NONCE_LEN],
    pub dek_wrapped: Vec<u8>,
    pub payload_nonce: [u8; NONCE_LEN],
    /// Ciphertext of the media-key payload (the wrapped per-file key
    /// plus its layout metadata, opaque bytes).
    pub ciphertext: Vec<u8>,
}

fn fill_random(buf: &mut [u8]) -> Result<(), EnvelopeError> {
    getrandom::fill(buf).map_err(|_| EnvelopeError::RandomnessUnavailable)
}

/// Frame one variable-length AAD segment: `u32` big-endian length +
/// bytes. Identical to the framing in `workers/api/src/providers.rs`.
fn frame(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Canonical v2 AAD for (`owner_scope` = user id, `row_key` =
/// `<user_id>:<track_id>` for a posted-file row). Built from the row
/// being written/read, never from a stored unauthenticated claim.
/// Payload AAD intentionally excludes `kek_version`: an authorized
/// rewrap changes only the wrap columns.
fn aad_v2(
    purpose: &str,
    user_id: &str,
    row_key: &str,
    envelope_version: u16,
    payload_format: u16,
    kek_version: Option<u32>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(128 + user_id.len() + row_key.len());
    frame(&mut out, AAD_DOMAIN);
    frame(&mut out, purpose);
    frame(&mut out, AAD_TABLE);
    frame(&mut out, AAD_FIELD);
    frame(&mut out, user_id);
    frame(&mut out, row_key);
    out.extend_from_slice(&envelope_version.to_be_bytes());
    out.extend_from_slice(&payload_format.to_be_bytes());
    if let Some(v) = kek_version {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

/// Seal a per-file media key under `kek` at `kek_version`, bound to
/// (`user_id`, `row_key`). Fresh DEK and fresh nonces per call.
pub fn seal_media_key(
    kek: &[u8; KEK_LEN],
    kek_version: u32,
    user_id: &str,
    row_key: &str,
    media_key_payload: &[u8],
) -> Result<Envelope, EnvelopeError> {
    let mut dek_bytes = [0u8; DEK_LEN];
    fill_random(&mut dek_bytes)?;
    let dek = Dek(dek_bytes);
    dek_bytes.zeroize();

    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| EnvelopeError::Crypto)?;
    let mut wrap_nonce = [0u8; NONCE_LEN];
    fill_random(&mut wrap_nonce)?;
    let dek_wrapped = kek_cipher
        .encrypt(
            &Nonce::from(wrap_nonce),
            Payload {
                msg: &dek.0,
                aad: &aad_v2(
                    PURPOSE_DEK,
                    user_id,
                    row_key,
                    ENVELOPE_VERSION_V2,
                    PAYLOAD_FORMAT_MEDIA_KEY_V1,
                    Some(kek_version),
                ),
            },
        )
        .map_err(|_| EnvelopeError::Crypto)?;

    let dek_cipher = Aes256Gcm::new_from_slice(&dek.0).map_err(|_| EnvelopeError::Crypto)?;
    let mut payload_nonce = [0u8; NONCE_LEN];
    fill_random(&mut payload_nonce)?;
    let ciphertext = dek_cipher
        .encrypt(
            &Nonce::from(payload_nonce),
            Payload {
                msg: media_key_payload,
                aad: &aad_v2(
                    PURPOSE_PAYLOAD,
                    user_id,
                    row_key,
                    ENVELOPE_VERSION_V2,
                    PAYLOAD_FORMAT_MEDIA_KEY_V1,
                    None,
                ),
            },
        )
        .map_err(|_| EnvelopeError::Crypto)?;

    Ok(Envelope {
        dek_wrapped_nonce: wrap_nonce,
        dek_wrapped,
        payload_nonce,
        ciphertext,
    })
}

/// Open an envelope with the version-pinned KEK. Unknown versions,
/// missing keys and bad tags all return [`EnvelopeError::Crypto`].
pub fn open_media_key(
    kek: &[u8; KEK_LEN],
    user_id: &str,
    row_key: &str,
    envelope_version: u16,
    payload_format: u16,
    kek_version: u32,
    envelope: &Envelope,
) -> Result<Vec<u8>, EnvelopeError> {
    if envelope.dek_wrapped.len() != WRAPPED_DEK_LEN {
        return Err(EnvelopeError::CorruptStoredData);
    }
    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| EnvelopeError::Crypto)?;
    let mut dek_bytes = kek_cipher
        .decrypt(
            &Nonce::from(envelope.dek_wrapped_nonce),
            Payload {
                msg: &envelope.dek_wrapped,
                aad: &aad_v2(
                    PURPOSE_DEK,
                    user_id,
                    row_key,
                    envelope_version,
                    payload_format,
                    Some(kek_version),
                ),
            },
        )
        .map_err(|_| EnvelopeError::Crypto)?;
    if dek_bytes.len() != DEK_LEN {
        dek_bytes.zeroize();
        return Err(EnvelopeError::Crypto);
    }
    let mut arr = [0u8; DEK_LEN];
    arr.copy_from_slice(&dek_bytes);
    dek_bytes.zeroize();
    let dek = Dek(arr);

    let dek_cipher = Aes256Gcm::new_from_slice(&dek.0).map_err(|_| EnvelopeError::Crypto)?;
    dek_cipher
        .decrypt(
            &Nonce::from(envelope.payload_nonce),
            Payload {
                msg: &envelope.ciphertext,
                aad: &aad_v2(
                    PURPOSE_PAYLOAD,
                    user_id,
                    row_key,
                    envelope_version,
                    payload_format,
                    None,
                ),
            },
        )
        .map_err(|_| EnvelopeError::Crypto)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_kek(fill: u8) -> [u8; KEK_LEN] {
        [fill; KEK_LEN]
    }

    #[test]
    fn round_trip() {
        let env = seal_media_key(
            &test_kek(0x11),
            7,
            "drill-user",
            "drill-user:track-1",
            b"media-key-payload",
        )
        .expect("seal");
        assert_eq!(env.dek_wrapped.len(), WRAPPED_DEK_LEN);
        let pt = open_media_key(
            &test_kek(0x11),
            "drill-user",
            "drill-user:track-1",
            ENVELOPE_VERSION_V2,
            PAYLOAD_FORMAT_MEDIA_KEY_V1,
            7,
            &env,
        )
        .expect("open");
        assert_eq!(pt, b"media-key-payload");
    }

    #[test]
    fn wrong_kek_version_fails_closed() {
        let env = seal_media_key(&test_kek(0x11), 7, "drill-user", "drill-user:track-1", b"x")
            .expect("seal");
        assert_eq!(
            open_media_key(
                &test_kek(0x22),
                "drill-user",
                "drill-user:track-1",
                ENVELOPE_VERSION_V2,
                PAYLOAD_FORMAT_MEDIA_KEY_V1,
                7,
                &env
            ),
            Err(EnvelopeError::Crypto)
        );
    }

    #[test]
    fn wrong_row_fails_closed() {
        let env = seal_media_key(&test_kek(0x11), 7, "drill-user", "drill-user:track-1", b"x")
            .expect("seal");
        assert_eq!(
            open_media_key(
                &test_kek(0x11),
                "drill-user",
                "drill-user:track-2",
                ENVELOPE_VERSION_V2,
                PAYLOAD_FORMAT_MEDIA_KEY_V1,
                7,
                &env
            ),
            Err(EnvelopeError::Crypto)
        );
    }

    #[test]
    fn bad_wrapped_len_is_corrupt_not_crypto() {
        let mut env = seal_media_key(&test_kek(0x11), 7, "u", "u:t", b"x").expect("seal");
        env.dek_wrapped.pop();
        assert_eq!(
            open_media_key(
                &test_kek(0x11),
                "u",
                "u:t",
                ENVELOPE_VERSION_V2,
                PAYLOAD_FORMAT_MEDIA_KEY_V1,
                7,
                &env
            ),
            Err(EnvelopeError::CorruptStoredData)
        );
    }

    /// Pins the framing against the documented production
    /// construction: domain + purpose + table + field + owner + row +
    /// u16 versions (+ u32 kek version for the wrap). Any framing
    /// change breaks byte-compatibility with the production seal path
    /// and must be a deliberate, reviewed migration — never a silent
    /// edit.
    #[test]
    fn aad_matches_production_construction() {
        let aad = aad_v2(PURPOSE_DEK, "u", "u:t", 2, 1, Some(7));
        let mut want = Vec::new();
        for s in [
            "nntune-envelope-v2",
            "dek",
            "track_sources",
            "nzb_ref",
            "u",
            "u:t",
        ] {
            want.extend_from_slice(&(s.len() as u32).to_be_bytes());
            want.extend_from_slice(s.as_bytes());
        }
        want.extend_from_slice(&2u16.to_be_bytes());
        want.extend_from_slice(&1u16.to_be_bytes());
        want.extend_from_slice(&7u32.to_be_bytes());
        assert_eq!(aad, want);

        // The payload AAD re-frames with the payload purpose and omits
        // the KEK version (rewrap changes the wrap columns only).
        let mut want_payload = Vec::new();
        for s in [
            "nntune-envelope-v2",
            "payload",
            "track_sources",
            "nzb_ref",
            "u",
            "u:t",
        ] {
            want_payload.extend_from_slice(&(s.len() as u32).to_be_bytes());
            want_payload.extend_from_slice(s.as_bytes());
        }
        want_payload.extend_from_slice(&2u16.to_be_bytes());
        want_payload.extend_from_slice(&1u16.to_be_bytes());
        let payload = aad_v2(PURPOSE_PAYLOAD, "u", "u:t", 2, 1, None);
        assert_eq!(payload, want_payload);
    }
}
