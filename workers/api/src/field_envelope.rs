//! API-owned v2 field envelope. Callers pin the table/field/scope/row;
//! payload AAD excludes KEK version so rotation can remain wrap-only.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce, A_MAX, P_MAX,
};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::vault::VaultError;

const DEK_LEN: usize = 32;
const TAG_LEN: usize = 16;

/// Stored bytes only; never a plaintext DEK. Deliberately no Debug.
#[derive(Clone)]
pub struct FieldEnvelope {
    pub dek_wrapped_nonce: [u8; 12],
    pub dek_wrapped: Vec<u8>,
    pub payload_nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct Dek([u8; DEK_LEN]);

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
    fn aad_parts<'a>(&'a self, purpose: &'a str) -> [&'a str; 6] {
        [
            "nntune-envelope-v2",
            purpose,
            self.table,
            self.field,
            self.scope,
            self.row,
        ]
    }

    fn aad_capacity(&self, purpose: &str, versioned: bool) -> Option<usize> {
        let trailer = if versioned { 8usize } else { 4 };
        self.aad_parts(purpose)
            .into_iter()
            .try_fold(trailer, |size, part| {
                u32::try_from(part.len()).ok()?;
                size.checked_add(4)?.checked_add(part.len())
            })
            .filter(|size| *size <= isize::MAX as usize && *size as u64 <= A_MAX)
    }

    fn checked_aad(&self, purpose: &str, kek_version: Option<u32>) -> Result<Vec<u8>, VaultError> {
        let capacity = self
            .aad_capacity(purpose, kek_version.is_some())
            .ok_or(VaultError::Invalid)?;
        let mut out = Vec::with_capacity(capacity);
        for s in self.aad_parts(purpose) {
            out.extend_from_slice(&(s.len() as u32).to_be_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&self.format.to_be_bytes());
        if let Some(v) = kek_version {
            out.extend_from_slice(&v.to_be_bytes());
        }
        Ok(out)
    }

    #[cfg(test)]
    pub(crate) fn aad(&self, purpose: &str, kek_version: Option<u32>) -> Vec<u8> {
        self.checked_aad(purpose, kek_version).unwrap()
    }
}

pub(crate) fn seal(
    kek: &[u8; 32],
    kek_version: u32,
    context: &Context<'_>,
    plaintext: &[u8],
) -> Result<FieldEnvelope, VaultError> {
    if plaintext.len() as u64 > P_MAX {
        return Err(VaultError::Invalid);
    }
    let mut bytes = crate::random::secure_random_bytes::<DEK_LEN>()?;
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
                aad: &context.checked_aad("dek", Some(kek_version))?,
            },
        )
        .map_err(|_| VaultError::Crypto)?;
    let dek_cipher = Aes256Gcm::new_from_slice(&dek.0).map_err(|_| VaultError::Crypto)?;
    let ciphertext = dek_cipher
        .encrypt(
            &Nonce::from(payload_nonce),
            Payload {
                msg: plaintext,
                aad: &context.checked_aad("payload", None)?,
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

/// Capacities of the two AAD buffers and two decrypt inputs. The locked
/// `aead` 0.6.1 decrypt path copies each full ciphertext, including the tag,
/// then truncates in place without allocating a separate plaintext buffer.
pub(crate) fn open_capacities(
    context: &Context<'_>,
    envelope: &FieldEnvelope,
) -> Option<[usize; 4]> {
    Some([
        context.aad_capacity("dek", true)?,
        envelope.dek_wrapped.len(),
        context.aad_capacity("payload", false)?,
        envelope.ciphertext.len(),
    ])
}

pub(crate) fn open(
    kek: &[u8; 32],
    kek_version: u32,
    context: &Context<'_>,
    envelope: &FieldEnvelope,
) -> Result<Vec<u8>, VaultError> {
    if envelope.dek_wrapped.len() != DEK_LEN + TAG_LEN
        || !(TAG_LEN as u64..=P_MAX + TAG_LEN as u64).contains(&(envelope.ciphertext.len() as u64))
    {
        return Err(VaultError::CorruptStoredData);
    }
    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| VaultError::Crypto)?;
    let bytes = Zeroizing::new(
        kek_cipher
            .decrypt(
                &Nonce::from(envelope.dek_wrapped_nonce),
                Payload {
                    msg: &envelope.dek_wrapped,
                    aad: &context.checked_aad("dek", Some(kek_version))?,
                },
            )
            .map_err(|_| VaultError::Crypto)?,
    );
    if bytes.len() != DEK_LEN {
        return Err(VaultError::Crypto);
    }
    let dek_cipher = Aes256Gcm::new_from_slice(&bytes).map_err(|_| VaultError::Crypto)?;
    dek_cipher
        .decrypt(
            &Nonce::from(envelope.payload_nonce),
            Payload {
                msg: &envelope.ciphertext,
                aad: &context.checked_aad("payload", None)?,
            },
        )
        .map_err(|_| VaultError::Crypto)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [19; 32];
    const KEK_VERSION: u32 = 7;
    const CONTEXT: Context<'static> = Context {
        table: "table",
        field: "field",
        scope: "owner",
        row: "row",
        version: 2,
        format: 1,
    };

    #[test]
    fn envelope_rejects_wrong_identity_and_key() {
        let envelope = seal(&KEY, KEK_VERSION, &CONTEXT, b"fixture").unwrap();
        assert_eq!(
            open(&KEY, KEK_VERSION, &CONTEXT, &envelope).unwrap(),
            b"fixture"
        );
        for other in [
            Context {
                table: "elsewhere",
                ..CONTEXT
            },
            Context {
                field: "other",
                ..CONTEXT
            },
            Context {
                scope: "different owner",
                ..CONTEXT
            },
            Context {
                row: "different row",
                ..CONTEXT
            },
            Context {
                version: 3,
                ..CONTEXT
            },
            Context {
                format: 2,
                ..CONTEXT
            },
        ] {
            assert!(matches!(
                open(&KEY, KEK_VERSION, &other, &envelope),
                Err(VaultError::Crypto)
            ));
        }
        assert!(matches!(
            open(&[9; 32], KEK_VERSION, &CONTEXT, &envelope),
            Err(VaultError::Crypto)
        ));
        assert!(matches!(
            open(&KEY, KEK_VERSION + 1, &CONTEXT, &envelope),
            Err(VaultError::Crypto)
        ));
    }

    #[test]
    fn envelope_rejects_nonce_tag_and_ciphertext_changes() {
        let envelope = seal(&KEY, KEK_VERSION, &CONTEXT, b"fixture").unwrap();
        let mutations: [fn(&mut FieldEnvelope); 6] = [
            |value| value.dek_wrapped_nonce[0] ^= 1,
            |value| value.payload_nonce[0] ^= 1,
            |value| value.dek_wrapped[0] ^= 1,
            |value| value.dek_wrapped[DEK_LEN] ^= 1,
            |value| value.ciphertext[0] ^= 1,
            |value| {
                let index = value.ciphertext.len() - 1;
                value.ciphertext[index] ^= 1;
            },
        ];
        for mutate in mutations {
            let mut tampered = envelope.clone();
            mutate(&mut tampered);
            assert!(matches!(
                open(&KEY, KEK_VERSION, &CONTEXT, &tampered),
                Err(VaultError::Crypto)
            ));
        }
    }

    #[test]
    fn envelope_requires_full_tags_and_accepts_authenticated_empty_payload() {
        let envelope = seal(&KEY, KEK_VERSION, &CONTEXT, b"").unwrap();
        assert_eq!(envelope.ciphertext.len(), TAG_LEN);
        assert!(open(&KEY, KEK_VERSION, &CONTEXT, &envelope)
            .unwrap()
            .is_empty());
        let mut truncated = envelope.clone();
        truncated.dek_wrapped.pop();
        assert!(matches!(
            open(&KEY, KEK_VERSION, &CONTEXT, &truncated),
            Err(VaultError::CorruptStoredData)
        ));
        let mut truncated = envelope;
        truncated.ciphertext.pop();
        assert!(matches!(
            open(&KEY, KEK_VERSION, &CONTEXT, &truncated),
            Err(VaultError::CorruptStoredData)
        ));
    }
}
