//! API-owned v2 field envelope. Callers pin the table/field/scope/row;
//! payload AAD excludes KEK version so rotation can remain wrap-only.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::vault::VaultError;

/// Stored bytes only; never a plaintext DEK. Deliberately no Debug.
#[derive(Clone)]
pub struct FieldEnvelope {
    pub dek_wrapped_nonce: [u8; 12],
    pub dek_wrapped: Vec<u8>,
    pub payload_nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct Dek([u8; 32]);

/// Expected identity comes from the selected row, not ciphertext claims.
pub(crate) struct Context<'a> {
    pub table: &'a str,
    pub field: &'a str,
    pub scope: &'a str,
    pub row: &'a str,
    pub version: u16,
    pub format: u16,
}

impl Context<'_> {
    pub(crate) fn aad(&self, purpose: &str, kek_version: Option<u32>) -> Vec<u8> {
        let mut out = Vec::new();
        for s in [
            "nntune-envelope-v2",
            purpose,
            self.table,
            self.field,
            self.scope,
            self.row,
        ] {
            out.extend_from_slice(&(s.len() as u32).to_be_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&self.format.to_be_bytes());
        if let Some(v) = kek_version {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }
}

pub(crate) fn seal(
    kek: &[u8; 32],
    kek_version: u32,
    context: &Context<'_>,
    plaintext: &[u8],
) -> Result<FieldEnvelope, VaultError> {
    let mut bytes = crate::random::secure_random_bytes::<32>()?;
    let dek = Dek(bytes);
    bytes.zeroize();
    let wrap_nonce = crate::random::secure_random_bytes::<12>()?;
    let payload_nonce = crate::random::secure_random_bytes::<12>()?;
    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| VaultError::Crypto)?;
    let dek_wrapped = kek_cipher
        .encrypt(
            &Nonce::from(wrap_nonce),
            Payload {
                msg: &dek.0,
                aad: &context.aad("dek", Some(kek_version)),
            },
        )
        .map_err(|_| VaultError::Crypto)?;
    let dek_cipher = Aes256Gcm::new_from_slice(&dek.0).map_err(|_| VaultError::Crypto)?;
    let ciphertext = dek_cipher
        .encrypt(
            &Nonce::from(payload_nonce),
            Payload {
                msg: plaintext,
                aad: &context.aad("payload", None),
            },
        )
        .map_err(|_| VaultError::Crypto)?;
    Ok(FieldEnvelope {
        dek_wrapped_nonce: wrap_nonce,
        dek_wrapped,
        payload_nonce,
        ciphertext,
    })
}

pub(crate) fn open(
    kek: &[u8; 32],
    kek_version: u32,
    context: &Context<'_>,
    envelope: &FieldEnvelope,
) -> Result<Vec<u8>, VaultError> {
    if envelope.dek_wrapped.len() != 48 {
        return Err(VaultError::CorruptStoredData);
    }
    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| VaultError::Crypto)?;
    let bytes = Zeroizing::new(
        kek_cipher
            .decrypt(
                &Nonce::from(envelope.dek_wrapped_nonce),
                Payload {
                    msg: &envelope.dek_wrapped,
                    aad: &context.aad("dek", Some(kek_version)),
                },
            )
            .map_err(|_| VaultError::Crypto)?,
    );
    if bytes.len() != 32 {
        return Err(VaultError::Crypto);
    }
    let dek_cipher = Aes256Gcm::new_from_slice(&bytes).map_err(|_| VaultError::Crypto)?;
    dek_cipher
        .decrypt(
            &Nonce::from(envelope.payload_nonce),
            Payload {
                msg: &envelope.ciphertext,
                aad: &context.aad("payload", None),
            },
        )
        .map_err(|_| VaultError::Crypto)
}
