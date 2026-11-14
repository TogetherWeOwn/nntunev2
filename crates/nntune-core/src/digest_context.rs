//! Canonical digest-map context encoding, not an enrolled manifest or serving authority.
//!
//! The caller supplies trusted application identities and the exact origin header.
//! Provider data cannot establish either trust or publication eligibility here.

use crate::digest_geometry::DigestGeometry;
use sha2::{Digest, Sha256};

/// Encodes the root-version-independent scheme-2 context. This value alone
/// cannot enroll a map, verify a page, or authorize serving.
pub fn context_hash(
    identities: &[[u8; 16]; 5],
    geometry: DigestGeometry,
    header: &[u8; 48],
) -> Option<[u8; 32]> {
    if identities
        .iter()
        .any(|id| id[6] >> 4 != 4 || id[8] >> 6 != 2)
    {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"nntune/s3-media/digest-context/v1\0");
    for id in identities {
        hash.update(id);
    }
    hash.update(geometry.plaintext_len().to_be_bytes());
    hash.update(geometry.sealed_len().to_be_bytes());
    hash.update(header);
    Some(hash.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_identities_and_binds_all_context_fields() {
        let mut ids = [[0u8; 16]; 5];
        for (i, id) in ids.iter_mut().enumerate() {
            id[0] = i as u8 + 1;
            id[6] = 0x40;
            id[8] = 0x80;
        }
        let header = [7; 48];
        let geometry = DigestGeometry::new(65_537).unwrap();
        let digest = context_hash(&ids, geometry, &header).unwrap();
        assert_eq!(
            digest,
            [
                0xba, 0x31, 0xb4, 0x7b, 0xd1, 0x32, 0x5b, 0x4c, 0x0b, 0xb4, 0xf2, 0x18, 0x05, 0x2d,
                0xb3, 0xc9, 0x01, 0x46, 0x35, 0x04, 0xa6, 0x12, 0x5b, 0x38, 0xc3, 0xe5, 0xc6, 0xe4,
                0xc3, 0xc5, 0xe4, 0xde,
            ]
        );
        for i in 0..5 {
            let mut changed = ids;
            changed[i][15] ^= 1;
            assert_ne!(context_hash(&changed, geometry, &header), Some(digest));
            changed[i][6] = 0;
            assert!(context_hash(&changed, geometry, &header).is_none());
            changed[i][6] = 0x40;
            changed[i][8] = 0;
            assert!(context_hash(&changed, geometry, &header).is_none());
        }
        assert!(context_hash(&[[0; 16]; 5], geometry, &header).is_none());
        assert_ne!(context_hash(&ids, geometry, &[8; 48]), Some(digest));
        assert_ne!(
            context_hash(&ids, DigestGeometry::new(65_536).unwrap(), &header),
            Some(digest)
        );
    }
}
