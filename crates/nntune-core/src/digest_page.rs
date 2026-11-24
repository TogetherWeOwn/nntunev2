//! Canonical page encoding only: neither these bytes nor their hash confer
//! origin enrollment, publication eligibility, or authority to serve media.
//!
//! The eventual origin builder must derive entries exclusively from its owned
//! sealer. A verifier must compare a page against an independently trusted row
//! before parsing it; hashing provider bytes here cannot create that row.

use crate::digest_geometry::DigestGeometry;
use sha2::{Digest, Sha256};

/// Encode the unique page partition for trusted geometry. Inputs to this
/// utility are not authenticated, and its output is not an enrolled manifest.
pub fn encode_page(
    geometry: DigestGeometry,
    map_id: &[u8; 16],
    context: &[u8; 32],
    index: u32,
    entries: &[[u8; 32]],
) -> Option<Vec<u8>> {
    let page = geometry.page(index)?;
    if map_id[6] >> 4 != 4 || map_id[8] >> 6 != 2 || entries.len() != page.count as usize {
        return None;
    }
    let mut bytes = Vec::with_capacity(page.encoded_len as usize);
    bytes.extend_from_slice(b"NNTDGP01");
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&u16::from(page.final_page).to_be_bytes());
    bytes.extend_from_slice(map_id);
    bytes.extend_from_slice(context);
    bytes.extend_from_slice(&geometry.plaintext_len().to_be_bytes());
    bytes.extend_from_slice(&geometry.sealed_len().to_be_bytes());
    bytes.extend_from_slice(&geometry.chunks().to_be_bytes());
    bytes.extend_from_slice(&page.index.to_be_bytes());
    bytes.extend_from_slice(&page.first.to_be_bytes());
    bytes.extend_from_slice(&(page.count as u16).to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    for entry in entries {
        bytes.extend_from_slice(entry);
    }
    Some(bytes)
}

/// Calculate the domain-separated page hash for bounded bytes. This is a
/// computation, not validation or a source of trusted expected hashes.
pub fn page_hash(bytes: &[u8]) -> Option<[u8; 32]> {
    if !(124..=8_284).contains(&bytes.len()) {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"nntune/s3-media/digest-page/v1\0");
    hash.update((bytes.len() as u32).to_be_bytes());
    hash.update(bytes);
    Some(hash.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_canonical_page_boundaries() {
        let mut map = [0; 16];
        map[6] = 0x40;
        map[8] = 0x80;
        for chunks in [1_u32, 255, 256, 257] {
            let geometry = DigestGeometry::new(u64::from(chunks) * 65_536).unwrap();
            for index in 0..geometry.pages() {
                let page = geometry.page(index).unwrap();
                let entries = vec![[7; 32]; page.count as usize];
                let encoded = encode_page(geometry, &map, &[3; 32], index, &entries).unwrap();
                assert_eq!(encoded.len(), page.encoded_len as usize);
                assert_eq!(&encoded[..8], b"NNTDGP01");
                assert_eq!(&encoded[8..12], &[0, 1, 0, u8::from(page.final_page)]);
                assert_eq!(&encoded[12..28], &map);
                assert_eq!(&encoded[28..60], &[3; 32]);
                assert_eq!(&encoded[60..68], &geometry.plaintext_len().to_be_bytes());
                assert_eq!(&encoded[68..76], &geometry.sealed_len().to_be_bytes());
                assert_eq!(&encoded[76..80], &chunks.to_be_bytes());
                assert_eq!(&encoded[80..84], &index.to_be_bytes());
                assert_eq!(&encoded[84..88], &page.first.to_be_bytes());
                assert_eq!(&encoded[88..90], &(page.count as u16).to_be_bytes());
                assert_eq!(&encoded[90..92], &[0, 0]);
                assert_eq!(&encoded[92..], entries.concat());
                let digest = page_hash(&encoded).unwrap();
                if chunks == 1 {
                    assert_eq!(
                        digest,
                        [
                            0x43, 0x90, 0x99, 0xa9, 0xd2, 0x7c, 0x0a, 0x6f, 0x96, 0xbe, 0x3b, 0xd8,
                            0xcd, 0xad, 0x41, 0x95, 0x59, 0xd0, 0x54, 0xee, 0xc1, 0x89, 0xd4, 0xe6,
                            0x95, 0x56, 0x77, 0xfd, 0x17, 0x08, 0x62, 0x9d,
                        ]
                    );
                }
                assert!(encode_page(geometry, &map, &[3; 32], index, &[]).is_none());
            }
            assert!(encode_page(geometry, &[0; 16], &[3; 32], 0, &[[0; 32]]).is_none());
            assert!(encode_page(geometry, &map, &[3; 32], geometry.pages(), &[]).is_none());
        }
        assert!(page_hash(&[0; 123]).is_none());
        assert!(page_hash(&[0; 8_285]).is_none());
    }
}
