//! Independent validation of bounded pages against sealer-origin evidence.
//!
//! This is not published serving authority. Future serving must load a complete,
//! immutable manifest from an authorized consistent record or authenticated
//! restore, check lifecycle eligibility and fencing, and pin sidecar identities.

use crate::digest_enrollment::OriginManifest;
use crate::digest_geometry::DigestGeometry;
use crate::digest_page::page_hash;

/// Pre-AEAD consistency or integrity failures, not authentication failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageError {
    InvalidIndex,
    InvalidLength,
    HashMismatch,
    InvalidGrammar,
}

/// A page whose complete bytes and canonical header match origin evidence.
pub struct VerifiedPage<'a> {
    bytes: &'a [u8],
    geometry: DigestGeometry,
    first: u32,
    count: u32,
}

/// Origin-enrolled expectation for one complete sealed chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkExpectation {
    index: u32,
    sealed_len: u32,
    final_chunk: bool,
    digest: [u8; 32],
}

impl ChunkExpectation {
    pub fn index(&self) -> u32 {
        self.index
    }
    pub fn sealed_len(&self) -> u32 {
        self.sealed_len
    }
    pub fn final_chunk(&self) -> bool {
        self.final_chunk
    }
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

impl OriginManifest {
    /// Hash the complete bounded untrusted bytes before parsing any fields.
    pub fn verify_page<'a>(
        &self,
        index: u32,
        bytes: &'a [u8],
    ) -> Result<VerifiedPage<'a>, PageError> {
        let geometry = self.geometry();
        let page = geometry.page(index).ok_or(PageError::InvalidIndex)?;
        let expected_hash = self.row(index as usize).ok_or(PageError::InvalidIndex)?;
        let actual_hash = page_hash(bytes).ok_or(PageError::InvalidLength)?;
        if &actual_hash != expected_hash {
            return Err(PageError::HashMismatch);
        }
        if bytes.len() != page.encoded_len as usize {
            return Err(PageError::InvalidLength);
        }
        if &bytes[0..8] != b"NNTDGP01"
            || bytes[8..10] != 1_u16.to_be_bytes()
            || bytes[10..12] != u16::from(page.final_page).to_be_bytes()
            || &bytes[12..28] != self.map_id()
            || &bytes[28..60] != self.context()
            || bytes[60..68] != geometry.plaintext_len().to_be_bytes()
            || bytes[68..76] != geometry.sealed_len().to_be_bytes()
            || bytes[76..80] != geometry.chunks().to_be_bytes()
            || bytes[80..84] != index.to_be_bytes()
            || bytes[84..88] != page.first.to_be_bytes()
            || bytes[88..90] != (page.count as u16).to_be_bytes()
            || bytes[90..92] != [0, 0]
        {
            return Err(PageError::InvalidGrammar);
        }
        Ok(VerifiedPage {
            bytes,
            geometry,
            first: page.first,
            count: page.count,
        })
    }
}

impl VerifiedPage<'_> {
    /// Returns expectations only for chunks covered by this verified page.
    pub fn expectation(&self, index: u32) -> Option<ChunkExpectation> {
        let offset = index.checked_sub(self.first)?;
        if offset >= self.count {
            return None;
        }
        let start = 92 + offset as usize * 32;
        let digest = self.bytes[start..start + 32].try_into().ok()?;
        let (sealed_len, final_chunk) = self.geometry.chunk(index)?;
        Some(ChunkExpectation {
            index,
            sealed_len,
            final_chunk,
            digest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest_enrollment::DigestEnrollment;
    use sha2::{Digest, Sha256};

    fn id(n: u8) -> [u8; 16] {
        let mut id = [n; 16];
        id[6] = 0x40;
        id[8] = 0x80;
        id
    }

    #[test]
    fn sealer_page_expectation_chain() {
        for length in [
            1_u64,
            65_536,
            65_537,
            255 * 65_536,
            256 * 65_536,
            257 * 65_536,
        ] {
            let geometry = DigestGeometry::new(length).unwrap();
            let mut builder = DigestEnrollment::new(length, [id(1); 5], id(2)).unwrap();
            let mut pages = Vec::new();
            let mut digests = Vec::new();
            for index in 0..geometry.chunks() {
                let (size, _) = geometry.chunk(index).unwrap();
                let output = builder.seal_chunk(&vec![7; size as usize - 16]).unwrap();
                digests.push(<[u8; 32]>::from(Sha256::digest(&output.bytes)));
                if let Some(page) = output.page {
                    pages.push(page);
                }
            }
            let manifest = builder.finish().ok().unwrap();
            for (j, bytes) in pages.iter().enumerate() {
                let page = manifest.verify_page(j as u32, bytes).ok().unwrap();
                let part = geometry.page(j as u32).unwrap();
                for index in part.first..part.first + part.count {
                    let expected = page.expectation(index).unwrap();
                    assert_eq!(expected.index(), index);
                    assert_eq!(
                        (expected.sealed_len(), expected.final_chunk()),
                        geometry.chunk(index).unwrap()
                    );
                    assert_eq!(expected.digest(), &digests[index as usize]);
                }
                assert!(page.expectation(part.first + part.count).is_none());
            }
        }
    }

    #[test]
    fn matching_hash_does_not_accept_malformed_headers_or_lengths() {
        let mut builder = DigestEnrollment::new(1, [id(1); 5], id(2)).unwrap();
        let page = builder.seal_chunk(&[9]).unwrap().page.unwrap();
        let mut manifest = builder.finish().ok().unwrap();
        for offset in [0, 8, 10, 12, 18, 20, 28, 60, 68, 76, 80, 84, 88, 90] {
            let mut bad = page.clone();
            bad[offset] ^= 0x80;
            manifest.replace_test_row(0, page_hash(&bad).unwrap());
            assert!(
                matches!(
                    manifest.verify_page(0, &bad),
                    Err(PageError::InvalidGrammar)
                ),
                "offset {offset}"
            );
        }
        let mut trailing = page.clone();
        trailing.push(0);
        manifest.replace_test_row(0, page_hash(&trailing).unwrap());
        assert!(matches!(
            manifest.verify_page(0, &trailing),
            Err(PageError::InvalidLength)
        ));
        manifest.replace_test_row(0, page_hash(&page).unwrap());
        let mut changed = page.clone();
        changed[92] ^= 1;
        assert!(matches!(
            manifest.verify_page(0, &changed),
            Err(PageError::HashMismatch)
        ));
        assert!(matches!(
            manifest.verify_page(0, &page[..123]),
            Err(PageError::InvalidLength)
        ));
        assert!(matches!(
            manifest.verify_page(1, &page),
            Err(PageError::InvalidIndex)
        ));
        assert!(matches!(
            manifest.verify_page(0, &vec![0; 8_285]),
            Err(PageError::InvalidLength)
        ));
    }

    #[test]
    fn rejects_cross_map_and_matching_hash_truncated_page() {
        let mut first = DigestEnrollment::new(65_537, [id(1); 5], id(2)).unwrap();
        first.seal_chunk(&vec![1; 65_536]).unwrap();
        let page = first.seal_chunk(&[2]).unwrap().page.unwrap();
        let mut manifest = first.finish().ok().unwrap();
        let mut other = DigestEnrollment::new(65_537, [id(1); 5], id(3)).unwrap();
        other.seal_chunk(&vec![1; 65_536]).unwrap();
        let other_page = other.seal_chunk(&[2]).unwrap().page.unwrap();
        assert!(matches!(
            manifest.verify_page(0, &other_page),
            Err(PageError::HashMismatch)
        ));
        manifest.replace_test_row(0, page_hash(&other_page).unwrap());
        assert!(matches!(
            manifest.verify_page(0, &other_page),
            Err(PageError::InvalidGrammar)
        ));
        let truncated = &page[..124];
        manifest.replace_test_row(0, page_hash(truncated).unwrap());
        assert!(matches!(
            manifest.verify_page(0, truncated),
            Err(PageError::InvalidLength)
        ));
    }
}
