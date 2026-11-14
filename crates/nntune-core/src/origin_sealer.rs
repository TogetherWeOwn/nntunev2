//! Bounded new-upload sealer. Digests are trusted only when retained from this
//! origin; this module does not publish, persist, or authenticate a digest map.

use crate::media::{self, FileKey, Header, Profile, Sealer, PLAIN_CHUNK_SIZE};
use crate::rng::{FillRandom, SystemRng};
use sha2::{Digest, Sha256};

/// Maximum plaintext length for the new digest-map format (not a legacy read limit).
pub const MAX_NEW_PLAINTEXT: u64 = 1 << 33;

/// One exact ciphertext-and-tag chunk and its origin digest (header excluded).
pub struct SealedChunk {
    pub bytes: Vec<u8>,
    pub digest: [u8; 32],
    pub final_chunk: bool,
}

/// A single-use sequential lifecycle. Drop and start fresh after any error or
/// lost state; never reconstruct an occupied stream position using the same key.
pub struct OriginSealer {
    key: FileKey,
    sealer: Sealer,
    remaining: u64,
    failed: bool,
}

impl OriginSealer {
    /// Reject empty or oversized new inputs before drawing any key material.
    pub fn new(plaintext_len: u64) -> crate::Result<Self> {
        Self::with_rng(plaintext_len, &mut SystemRng)
    }

    fn with_rng<R: FillRandom>(plaintext_len: u64, rng: &mut R) -> crate::Result<Self> {
        if !(1..=MAX_NEW_PLAINTEXT).contains(&plaintext_len)
            || media::ciphertext_size(plaintext_len).is_none()
        {
            return Err(crate::Error::Malformed("invalid new media length".into()));
        }
        let key = FileKey::generate(rng)?;
        let sealer = Sealer::random(&Profile::nntune(), &key, rng)?;
        Ok(Self {
            key,
            sealer,
            remaining: plaintext_len,
            failed: false,
        })
    }

    /// For trusted wrapping only; key bytes must never be logged or persisted raw.
    pub fn key(&self) -> &FileKey {
        &self.key
    }

    pub fn header(&self) -> Header {
        self.sealer.header()
    }

    /// Accept exactly the next full chunk or the exact remaining final chunk.
    /// Any invalid call permanently fences this lifecycle.
    pub fn seal_chunk(&mut self, plaintext: &[u8]) -> crate::Result<SealedChunk> {
        let expected = self.remaining.min(PLAIN_CHUNK_SIZE);
        if self.failed || expected == 0 || u64::try_from(plaintext.len()).ok() != Some(expected) {
            self.failed = true;
            return Err(crate::Error::Malformed(
                "invalid origin chunk length or state".into(),
            ));
        }
        let final_chunk = self.remaining == expected;
        let bytes = match self.sealer.seal_chunk(plaintext, final_chunk) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.failed = true;
                return Err(error);
            }
        };
        self.remaining -= expected;
        let digest = Sha256::digest(&bytes).into();
        Ok(SealedChunk {
            bytes,
            digest,
            final_chunk,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(u8);
    impl FillRandom for Fixed {
        fn fill_bytes(&mut self, dest: &mut [u8]) -> crate::Result<()> {
            dest.fill(self.0);
            self.0 += 1;
            Ok(())
        }
    }

    #[test]
    fn origin_matches_pinned_sealer_and_hashes_emitted_chunks() {
        for len in [1, 65535, 65536, 65537, 131072] {
            let input = vec![0x42; len];
            let mut origin = OriginSealer::with_rng(len as u64, &mut Fixed(3)).unwrap();
            let key = FileKey::generate(&mut Fixed(3)).unwrap();
            let mut rng = Fixed(4);
            let mut reference = Sealer::random(&Profile::nntune(), &key, &mut rng).unwrap();
            assert_eq!(origin.header(), reference.header());
            let count = input.chunks(65536).len();
            for (i, chunk) in input.chunks(65536).enumerate() {
                let sealed = origin.seal_chunk(chunk).unwrap();
                assert_eq!(sealed.final_chunk, i + 1 == count);
                assert_eq!(
                    sealed.bytes,
                    reference.seal_chunk(chunk, sealed.final_chunk).unwrap()
                );
                assert_eq!(
                    sealed.digest,
                    <[u8; 32]>::from(Sha256::digest(&sealed.bytes))
                );
            }
            assert!(origin.seal_chunk(&[]).is_err());
        }
    }

    #[test]
    fn approved_wire_vectors_match() {
        // Same synthetic key, nonce and plaintext as media_approved_vectors.
        struct VectorRng;
        impl FillRandom for VectorRng {
            fn fill_bytes(&mut self, dest: &mut [u8]) -> crate::Result<()> {
                let start = match dest.len() {
                    32 => 0,
                    16 => 0xa0,
                    _ => panic!("unexpected entropy length"),
                };
                for (i, byte) in dest.iter_mut().enumerate() {
                    *byte = start + i as u8;
                }
                Ok(())
            }
        }
        for (size, expected) in [
            (
                1,
                "2b09c284bfecae4a7215a15956e3b27ceaf06baa660700875f055e6d35c5ae15",
            ),
            (
                65537,
                "41077c1a9caee69eb720a4dce47d9c8acb288b51deb2eb876cec5b760a1ccba9",
            ),
        ] {
            let plain: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let mut origin = OriginSealer::with_rng(size as u64, &mut VectorRng).unwrap();
            let mut wire = origin.header().to_bytes().to_vec();
            for chunk in plain.chunks(65536) {
                wire.extend(origin.seal_chunk(chunk).unwrap().bytes);
            }
            let actual: String = media::ContentIdentity::of(&wire)
                .digest()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn entropy_failure_aborts_without_a_sealer() {
        struct FailOn(usize);
        impl FillRandom for FailOn {
            fn fill_bytes(&mut self, dest: &mut [u8]) -> crate::Result<()> {
                if self.0 == 0 {
                    return Err(crate::Error::Malformed("entropy failure".into()));
                }
                self.0 -= 1;
                dest.fill(7);
                Ok(())
            }
        }
        for draw in 0..2 {
            assert!(OriginSealer::with_rng(1, &mut FailOn(draw)).is_err());
        }
    }

    #[test]
    fn rejects_invalid_lengths_and_fences_invalid_chunk() {
        assert!(OriginSealer::with_rng(0, &mut Fixed(0)).is_err());
        assert!(OriginSealer::with_rng(MAX_NEW_PLAINTEXT + 1, &mut Fixed(0)).is_err());
        assert!(OriginSealer::with_rng(MAX_NEW_PLAINTEXT, &mut Fixed(0)).is_ok());
        let mut origin = OriginSealer::with_rng(65537, &mut Fixed(0)).unwrap();
        assert!(origin.seal_chunk(&[0]).is_err());
        assert!(origin.seal_chunk(&vec![0; 65536]).is_err());
    }
}
