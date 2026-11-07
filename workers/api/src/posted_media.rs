//! API-owned posted-file key custody. Only the existing keyring and v2 field
//! envelope hold the random media key; ingest never receives a KEK. The returned
//! sealer and stored header come from the SAME engine writer. Opening authenticates
//! the row-bound envelope and the committing header before releasing a reader.
//!
//! This is the crypto boundary, not a DB upsert or posting/reuse decision. Callers
//! must preserve exact surviving custody on replay. Legacy rows require their
//! historical reader; they are never inferred or relabelled as media v1 here.

use crate::{
    field_envelope::{self, Context, FieldEnvelope},
    rotation::KekSource,
};
use nntune_core::{
    media::{FileKey, Header, KeyMode, Profile, Sealer, VerifiedHeader, HEADER_SIZE},
    rng::SystemRng,
};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKeyError {
    Invalid,
    UnknownKey,
    Crypto,
}

/// Maps directly to the file_key_* columns. No plaintext key or Debug output.
#[derive(Clone)]
pub struct SealedMediaKey {
    pub envelope_version: u16,
    pub payload_format: u16,
    pub kek_version: u32,
    pub envelope: FieldEnvelope,
}

/// The caller streams ciphertext through this writer, never constructs a second
/// writer for the same record. No Debug/serialization of the writer or envelopes.
pub struct MintedMedia {
    pub sealer: Sealer,
    pub header: [u8; HEADER_SIZE as usize],
    pub sealed_key: SealedMediaKey,
}

fn context(hash: &str) -> Result<Context<'_>, MediaKeyError> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(MediaKeyError::Invalid);
    }
    Ok(Context {
        table: "posted_files",
        field: "file_key",
        scope: "global:posted_files",
        row: hash,
        version: 2,
        format: 1,
    })
}

/// Fresh random per-file media key and header, wrapped under an exact existing
/// keyring version. This operation alone grants no admission or posting authority.
pub fn mint(
    keks: &impl KekSource,
    kek_version: u32,
    plaintext_sha256: &str,
) -> Result<MintedMedia, MediaKeyError> {
    let context = context(plaintext_sha256)?;
    if kek_version == 0 || kek_version > i32::MAX as u32 {
        return Err(MediaKeyError::Invalid);
    }
    let kek = keks.kek(kek_version).ok_or(MediaKeyError::UnknownKey)?;
    let key = FileKey::generate(&mut SystemRng).map_err(|_| MediaKeyError::Crypto)?;
    let sealer = Sealer::random(&Profile::nntune(), &key, &mut SystemRng)
        .map_err(|_| MediaKeyError::Crypto)?;
    let envelope = field_envelope::seal(kek, kek_version, &context, key.expose_secret())
        .map_err(|_| MediaKeyError::Crypto)?;
    Ok(MintedMedia {
        header: sealer.header().to_bytes(),
        sealer,
        sealed_key: SealedMediaKey {
            envelope_version: 2,
            payload_format: 1,
            kek_version,
            envelope,
        },
    })
}

/// All expected identity/format fields come from the selected immutable DB row,
/// never an untrusted wire header. A VerifiedHeader authenticates the header only:
/// callers still authenticate ciphertext chunks and check the complete digest.
pub fn open(
    keks: &impl KekSource,
    plaintext_sha256: &str,
    media_format: &str,
    media_key_mode: &str,
    header: &[u8],
    stored: &SealedMediaKey,
) -> Result<VerifiedHeader, MediaKeyError> {
    let context = context(plaintext_sha256)?;
    if media_format != nntune_core::media::FORMAT_LABEL
        || media_key_mode != "random"
        || header.len() != HEADER_SIZE as usize
        || stored.envelope_version != 2
        || stored.payload_format != 1
        || stored.kek_version == 0
        || stored.kek_version > i32::MAX as u32
        || stored.envelope.ciphertext.len() != 48
        || stored.envelope.dek_wrapped.len() != 48
    {
        return Err(MediaKeyError::Invalid);
    }
    let kek = keks
        .kek(stored.kek_version)
        .ok_or(MediaKeyError::UnknownKey)?;
    let bytes = Zeroizing::new(
        field_envelope::open(kek, stored.kek_version, &context, &stored.envelope)
            .map_err(|_| MediaKeyError::Crypto)?,
    );
    if bytes.len() != 32 {
        return Err(MediaKeyError::Invalid);
    }
    // from_bytes takes a copy; keep the caller's import buffer wiping too.
    let mut raw = Zeroizing::new([0u8; 32]);
    raw.copy_from_slice(&bytes);
    let key = FileKey::from_bytes(*raw);
    let header = Header::from_slice(header).map_err(|_| MediaKeyError::Crypto)?;
    VerifiedHeader::verify(&Profile::nntune(), KeyMode::Random, &key, &header)
        .map_err(|_| MediaKeyError::Crypto)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nntune_ingest::posted_files;
    use sha2::{Digest, Sha256};

    struct Keys(Zeroizing<[u8; 32]>);
    impl Keys {
        fn new() -> Self {
            Self(Zeroizing::new(
                crate::random::secure_random_bytes().unwrap(),
            ))
        }
    }
    impl KekSource for Keys {
        fn kek(&self, version: u32) -> Option<&[u8; 32]> {
            (version == 7).then_some(&self.0)
        }
    }

    fn hash(byte: char) -> String {
        byte.to_string().repeat(64)
    }

    fn open_minted(keys: &Keys, row: &str, minted: &MintedMedia) -> VerifiedHeader {
        open(
            keys,
            row,
            posted_files::MEDIA_V1_FORMAT,
            posted_files::MEDIA_V1_KEY_MODE,
            &minted.header,
            &minted.sealed_key,
        )
        .unwrap()
    }

    #[test]
    fn envelope_context_matches_the_persisted_writer_contract() {
        let row = hash('a');
        let ctx = context(&row).unwrap();
        assert_eq!(ctx.table, posted_files::ENVELOPE_AAD_TABLE);
        assert_eq!(ctx.field, posted_files::FILE_KEY_AAD_FIELD);
        assert_eq!(ctx.scope, posted_files::ENVELOPE_AAD_OWNER_SCOPE);
        assert_eq!(ctx.row, row);
        assert_eq!(ctx.version, 2);
        assert_eq!(ctx.format as i16, posted_files::FILE_KEY_PAYLOAD_FORMAT);
    }

    #[test]
    fn keyring_roundtrip_authenticates_actual_engine_ciphertext_at_chunk_boundaries() {
        let keys = Keys::new();
        let row = hash('a');
        for size in [1, 65_535, 65_536, 65_537, 131_072] {
            let minted = mint(&keys, 7, &row).unwrap();
            let verified = open_minted(&keys, &row, &minted);
            let plaintext = vec![13; size];
            let wire = minted.sealer.seal_to_vec(&plaintext).unwrap();
            let reader = verified.reader(size as u64).unwrap();
            let expected: [u8; 32] = Sha256::digest(&plaintext).into();
            assert_eq!(reader.verify_all(&wire).unwrap(), expected);
        }
    }

    #[test]
    fn every_header_byte_and_wrong_key_are_rejected() {
        let keys = Keys::new();
        let row = hash('a');
        let minted = mint(&keys, 7, &row).unwrap();
        for i in 0..minted.header.len() {
            let mut header = minted.header;
            header[i] ^= 1;
            assert_eq!(
                open(
                    &keys,
                    &row,
                    posted_files::MEDIA_V1_FORMAT,
                    "random",
                    &header,
                    &minted.sealed_key
                )
                .unwrap_err(),
                MediaKeyError::Crypto
            );
        }
        assert_eq!(
            open(
                &Keys::new(),
                &row,
                posted_files::MEDIA_V1_FORMAT,
                "random",
                &minted.header,
                &minted.sealed_key
            )
            .unwrap_err(),
            MediaKeyError::Crypto
        );
    }

    #[test]
    fn cross_row_cross_field_and_same_row_fresh_key_substitutions_fail() {
        let keys = Keys::new();
        let row = hash('a');
        let mut minted = mint(&keys, 7, &row).unwrap();
        assert_eq!(
            open(
                &keys,
                &hash('b'),
                posted_files::MEDIA_V1_FORMAT,
                "random",
                &minted.header,
                &minted.sealed_key
            )
            .unwrap_err(),
            MediaKeyError::Crypto
        );
        let other = mint(&keys, 7, &row).unwrap();
        assert_eq!(
            open(
                &keys,
                &row,
                posted_files::MEDIA_V1_FORMAT,
                "random",
                &other.header,
                &minted.sealed_key
            )
            .unwrap_err(),
            MediaKeyError::Crypto
        );
        let raw = Zeroizing::new(crate::random::secure_random_bytes::<32>().unwrap());
        let ctx = Context {
            field: posted_files::MANIFEST_AAD_FIELD,
            ..context(&row).unwrap()
        };
        minted.sealed_key.envelope =
            field_envelope::seal(keys.kek(7).unwrap(), 7, &ctx, raw.as_slice()).unwrap();
        assert_eq!(
            open(
                &keys,
                &row,
                posted_files::MEDIA_V1_FORMAT,
                "random",
                &minted.header,
                &minted.sealed_key
            )
            .unwrap_err(),
            MediaKeyError::Crypto
        );
    }

    #[test]
    fn record_shape_modes_versions_and_unknown_keks_fail_closed() {
        let keys = Keys::new();
        let row = hash('a');
        let minted = mint(&keys, 7, &row).unwrap();
        for (format, mode) in [
            ("legacy", "random"),
            (posted_files::MEDIA_V1_FORMAT, "convergent"),
            (posted_files::MEDIA_V1_FORMAT, ""),
        ] {
            assert_eq!(
                open(
                    &keys,
                    &row,
                    format,
                    mode,
                    &minted.header,
                    &minted.sealed_key
                )
                .unwrap_err(),
                MediaKeyError::Invalid
            );
        }
        for len in [0, 24, 47] {
            assert_eq!(
                open(
                    &keys,
                    &row,
                    posted_files::MEDIA_V1_FORMAT,
                    "random",
                    &minted.header[..len],
                    &minted.sealed_key
                )
                .unwrap_err(),
                MediaKeyError::Invalid
            );
        }
        let mut stored = minted.sealed_key.clone();
        stored.envelope_version = 1;
        assert_eq!(
            open(
                &keys,
                &row,
                posted_files::MEDIA_V1_FORMAT,
                "random",
                &minted.header,
                &stored
            )
            .unwrap_err(),
            MediaKeyError::Invalid
        );
        stored = minted.sealed_key.clone();
        stored.payload_format = 2;
        assert_eq!(
            open(
                &keys,
                &row,
                posted_files::MEDIA_V1_FORMAT,
                "random",
                &minted.header,
                &stored
            )
            .unwrap_err(),
            MediaKeyError::Invalid
        );
        stored = minted.sealed_key.clone();
        stored.kek_version = 8;
        assert_eq!(
            open(
                &keys,
                &row,
                posted_files::MEDIA_V1_FORMAT,
                "random",
                &minted.header,
                &stored
            )
            .unwrap_err(),
            MediaKeyError::UnknownKey
        );
        assert!(matches!(mint(&keys, 0, &row), Err(MediaKeyError::Invalid)));
        assert!(matches!(
            mint(&keys, 8, &row),
            Err(MediaKeyError::UnknownKey)
        ));
        assert!(matches!(
            mint(&keys, 7, &hash('A')),
            Err(MediaKeyError::Invalid)
        ));
    }
}
