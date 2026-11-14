//! Scheme-2 envelope structure and authenticated wrapping primitives.
//!
//! The caller must obtain the expected fields from independently trusted state
//! and separately enforce key purpose, lifecycle and immutable locator policy.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::media::ciphertext_size;

/// Public context from trusted state, not from the envelope being checked.
pub struct EnvelopeContext {
    /// Root version, environment, library, object, generation and provider version.
    pub identities: [[u8; 16]; 6],
    pub plaintext_len: u64,
    pub header: [u8; 48],
}

/// Check the exact scheme-2 encoding and trusted prefix, without decrypting.
/// This does not check the wrapped key, tag, header commitment or ciphertext.
pub fn matches_scheme2(record: &[u8], expected: &EnvelopeContext) -> bool {
    if record.len() != 230 || &record[..10] != b"NNTMED01\0\x02" {
        return false;
    }
    for (i, id) in expected.identities.iter().enumerate() {
        if id[6] >> 4 != 4 || id[8] >> 6 != 2 || record[10 + 16 * i..26 + 16 * i] != id[..] {
            return false;
        }
    }
    let Some(encoded_len) = ciphertext_size(expected.plaintext_len) else {
        return false;
    };
    record[106..114] == expected.plaintext_len.to_be_bytes()
        && record[114..122] == encoded_len.to_be_bytes()
        && record[122..170] == expected.header
        && record[170..182] == [0; 12]
}

/// Internal failure class; callers must not expose it as a public oracle.
#[derive(Debug, PartialEq, Eq)]
pub enum EnvelopeError {
    /// Rejected before AEAD verification; not an authentication failure.
    InvalidContext,
    /// The AEAD primitive rejected the ciphertext or tag.
    Authentication,
    /// An internal primitive operation failed.
    Crypto,
}

fn prefix(context: &EnvelopeContext) -> Result<[u8; 170], EnvelopeError> {
    let encoded_len =
        ciphertext_size(context.plaintext_len).ok_or(EnvelopeError::InvalidContext)?;
    let mut bytes = [0; 170];
    bytes[..10].copy_from_slice(b"NNTMED01\0\x02");
    for (i, id) in context.identities.iter().enumerate() {
        if id[6] >> 4 != 4 || id[8] >> 6 != 2 {
            return Err(EnvelopeError::InvalidContext);
        }
        bytes[10 + i * 16..26 + i * 16].copy_from_slice(id);
    }
    bytes[106..114].copy_from_slice(&context.plaintext_len.to_be_bytes());
    bytes[114..122].copy_from_slice(&encoded_len.to_be_bytes());
    bytes[122..].copy_from_slice(&context.header);
    Ok(bytes)
}

fn cipher(root: &[u8; 32], prefix: &[u8; 170]) -> Result<Aes256Gcm, EnvelopeError> {
    let hkdf = Hkdf::<Sha256>::new(Some(b"nntune/s3-media/wrap-kdf/v2\0"), root);
    let mut info = [0; 198];
    info[..28].copy_from_slice(b"nntune/s3-media/wrap-key/v2\0");
    info[28..].copy_from_slice(prefix);
    let mut key = Zeroizing::new([0; 32]);
    hkdf.expand(&info, &mut *key)
        .map_err(|_| EnvelopeError::Crypto)?;
    Aes256Gcm::new_from_slice(&*key).map_err(|_| EnvelopeError::Crypto)
}

fn aad(prefix: &[u8; 170]) -> [u8; 194] {
    let mut aad = [0; 194];
    aad[..24].copy_from_slice(b"nntune/s3-media/wrap/v2\0");
    aad[24..].copy_from_slice(prefix);
    aad
}

/// Wrap a new immutable lifecycle's key using a caller-selected trusted media root.
///
/// The caller MUST durably serialize the winning lifecycle and key before any
/// provider write, guarantee one distinct key per actual root and context, and
/// return the stored bytes on retry or same-root rewrap. Uncertain intent must
/// be fenced and replaced with a fresh lifecycle, never encrypted again here.
/// This primitive does not implement persistence, purpose/version authorization,
/// immutable locator validation or concurrency control; it is not an integration.
pub fn wrap_scheme2(
    root: &[u8; 32],
    context: &EnvelopeContext,
    key: &[u8; 32],
) -> Result<[u8; 230], EnvelopeError> {
    let prefix = prefix(context)?;
    let cipher = cipher(root, &prefix)?;
    let ciphertext = cipher
        .encrypt(
            &Nonce::from([0; 12]),
            Payload {
                msg: key,
                aad: &aad(&prefix),
            },
        )
        .map_err(|_| EnvelopeError::Crypto)?;
    let mut record = [0; 230];
    record[..170].copy_from_slice(&prefix);
    record[182..].copy_from_slice(&ciphertext);
    Ok(record)
}

/// Authenticate and unwrap only scheme 2 using independently trusted context.
/// The caller selects and authorizes the root purpose/version before this call;
/// no alternative family or root is attempted. Lifecycle, locator and rollback
/// protection remain external obligations.
pub fn unwrap_scheme2(
    root: &[u8; 32],
    context: &EnvelopeContext,
    record: &[u8],
) -> Result<Zeroizing<[u8; 32]>, EnvelopeError> {
    if !matches_scheme2(record, context) {
        return Err(EnvelopeError::InvalidContext);
    }
    let prefix = prefix(context)?;
    let cipher = cipher(root, &prefix)?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                &Nonce::from([0; 12]),
                Payload {
                    msg: &record[182..],
                    aad: &aad(&prefix),
                },
            )
            .map_err(|_| EnvelopeError::Authentication)?,
    );
    let mut key = Zeroizing::new([0; 32]);
    key.copy_from_slice(&plaintext);
    Ok(key)
}

#[cfg(test)]
#[path = "media_envelope_tests.rs"]
mod tests;
