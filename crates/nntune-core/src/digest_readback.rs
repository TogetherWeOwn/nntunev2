//! Bounded content verification against an existing sealer-origin manifest.
//!
//! This checks a sequence of supplied frames, not the transport that framed
//! them. The caller must separately verify locator/version, actual EOF, the
//! complete sidecar inventory, and media validity. Success grants no custody,
//! publication, or reuse authority.

use crate::digest_enrollment::OriginManifest;
use crate::digest_range::{DigestRange, RangeError};
use crate::media::FileKey;
use sha2::{Digest, Sha256};

#[derive(Debug, PartialEq, Eq)]
pub enum ReadbackError {
    Frame(RangeError),
    Incomplete,
    Failed,
    PlaintextDigest,
}

/// Authenticates each frame before hashing plaintext and releases no plaintext.
/// Retains no chunk buffers between calls; the incremental hasher may retain
/// a partial plaintext block and does not guarantee erasure. Do not log state.
pub struct DigestReadback<'a> {
    range: DigestRange<'a>,
    expected: &'a [u8; 32],
    hash: Option<Sha256>,
}

impl<'a> DigestReadback<'a> {
    pub fn new(manifest: &'a OriginManifest, key: &'a FileKey) -> Result<Self, ReadbackError> {
        let range = DigestRange::new(manifest, key, 0, manifest.geometry().plaintext_len())
            .map_err(ReadbackError::Frame)?;
        Ok(Self {
            range,
            expected: manifest.plaintext_sha256(),
            hash: Some(Sha256::new()),
        })
    }

    /// Verify one complete sealed chunk against its enrolled page. Supply the
    /// exact wire header only for index zero. Any refusal poisons the readback.
    pub fn push(
        &mut self,
        index: u32,
        page: &[u8],
        sealed: Vec<u8>,
        wire_header: Option<&[u8]>,
    ) -> Result<(), ReadbackError> {
        let mut hash = self.hash.take().ok_or(ReadbackError::Failed)?;
        if (index == 0) != wire_header.is_some() {
            return Err(ReadbackError::Frame(RangeError::Header));
        }
        let part = self
            .range
            .open_next(index, page, sealed, wire_header)
            .map_err(ReadbackError::Frame)?;
        hash.update(part.as_slice());
        self.hash = Some(hash);
        Ok(())
    }

    /// Require every expected frame and the original whole-plaintext digest.
    /// This does not prove EOF; the transport must also reject surplus data.
    pub fn finish(self) -> Result<(), ReadbackError> {
        let hash = self.hash.ok_or(ReadbackError::Failed)?;
        if self.range.next_indices().is_some() {
            return Err(ReadbackError::Incomplete);
        }
        if <[u8; 32]>::from(hash.finalize()) != *self.expected {
            return Err(ReadbackError::PlaintextDigest);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest_test_fixture::{fixture, Fixture};

    fn first(readback: &mut DigestReadback<'_>, f: &Fixture) {
        readback
            .push(0, &f.pages[0], f.chunks[0].clone(), Some(&f.header))
            .unwrap();
    }

    #[test]
    fn complete_readback_requires_every_expected_frame_and_original_digest() {
        let mut plaintext = vec![5; 65_537];
        plaintext[65_536] = 8;
        let f = fixture(&plaintext);
        let mut partial = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut partial, &f);
        assert_eq!(partial.finish(), Err(ReadbackError::Incomplete));

        let verify = |expected: Option<&[u8; 32]>| {
            let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
            if let Some(expected) = expected {
                readback.expected = expected;
            }
            first(&mut readback, &f);
            readback
                .push(1, &f.pages[0], f.chunks[1].clone(), None)
                .unwrap();
            readback.finish()
        };
        assert_eq!(verify(None), Ok(()));
        let mut wrong_digest = *f.manifest.plaintext_sha256();
        wrong_digest[0] ^= 1;
        assert_eq!(
            verify(Some(&wrong_digest)),
            Err(ReadbackError::PlaintextDigest)
        );
    }

    #[test]
    fn header_on_later_frame_poisons_readback() {
        let f = fixture(&vec![5; 65_537]);
        let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut readback, &f);
        assert_eq!(
            readback.push(1, &f.pages[0], f.chunks[1].clone(), Some(&f.header)),
            Err(ReadbackError::Frame(RangeError::Header))
        );
        assert_eq!(
            readback.push(1, &f.pages[0], f.chunks[1].clone(), None),
            Err(ReadbackError::Failed)
        );
        assert_eq!(readback.finish(), Err(ReadbackError::Failed));
    }

    #[test]
    fn refused_or_surplus_frame_poisons_readback() {
        let f = fixture(&vec![5; 65_537]);
        let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut readback, &f);
        let mut changed = f.chunks[1].clone();
        *changed.last_mut().unwrap() ^= 1;
        assert_eq!(
            readback.push(1, &f.pages[0], changed, None),
            Err(ReadbackError::Frame(RangeError::DigestMismatch))
        );
        assert_eq!(
            readback.push(1, &f.pages[0], f.chunks[1].clone(), None),
            Err(ReadbackError::Failed)
        );
        assert_eq!(readback.finish(), Err(ReadbackError::Failed));

        let mut readback = DigestReadback::new(&f.manifest, &f.key).unwrap();
        first(&mut readback, &f);
        readback
            .push(1, &f.pages[0], f.chunks[1].clone(), None)
            .unwrap();
        assert_eq!(
            readback.push(2, &f.pages[0], f.chunks[1].clone(), None),
            Err(ReadbackError::Frame(RangeError::InvalidChunk))
        );
        assert_eq!(readback.finish(), Err(ReadbackError::Failed));
    }
}
