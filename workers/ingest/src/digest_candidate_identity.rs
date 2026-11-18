//! Non-authorizing identity checks for inactive digest-map candidates.

use nntune_core::digest_enrollment::OriginManifest;

/// Identity order is environment, library, object, sealing generation, provider.
pub struct CandidateIntent {
    pub identities: [[u8; 16]; 5],
    pub plaintext_len: u64,
    pub map_id: [u8; 16],
    pub header: [u8; 48],
    pub reservation_generation: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CandidateError {
    Identity,
    MissingOrFenced,
    Permanent,
    /// The server may have committed. Neither retry nor publication is safe
    /// until an authoritative fenced reconciliation protocol is implemented.
    UnknownCommit,
}

/// Match sealer-origin evidence; this does not authorize persistence or reuse.
pub fn candidate_hashes<'a>(
    intent: &CandidateIntent,
    manifest: &'a OriginManifest,
) -> Result<&'a [[u8; 32]], CandidateError> {
    if intent.reservation_generation <= 0 {
        return Err(CandidateError::Identity);
    }
    manifest
        .candidate_page_hashes(
            &intent.identities,
            intent.plaintext_len,
            &intent.map_id,
            &intent.header,
        )
        .ok_or(CandidateError::Identity)
}

/// Stored page geometry and hash, read in page-index order.
#[allow(dead_code)]
pub struct CandidatePage {
    pub index: i32,
    pub chunk_count: i32,
    pub first_chunk: i32,
    pub entry_count: i32,
    pub hash: Vec<u8>,
}

/// Compare every persisted page and its geometry to sealer-origin hashes.
///
/// This comparison alone is not reconciliation: the caller must also establish
/// immutable identity, a fresh authoritative snapshot and reservation fencing.
#[allow(dead_code)]
pub fn candidate_pages_match(
    plaintext_len: u64,
    expected: &[[u8; 32]],
    persisted: &[CandidatePage],
) -> bool {
    if !(1..=8_589_934_592).contains(&plaintext_len) {
        return false;
    }
    let chunks = plaintext_len.div_ceil(65_536) as usize;
    let pages = chunks.div_ceil(256);
    expected.len() == pages
        && persisted.len() == pages
        && expected
            .iter()
            .zip(persisted)
            .enumerate()
            .all(|(index, (hash, page))| {
                let first = index * 256;
                usize::try_from(page.index) == Ok(index)
                    && usize::try_from(page.chunk_count) == Ok(chunks)
                    && usize::try_from(page.first_chunk) == Ok(first)
                    && usize::try_from(page.entry_count) == Ok((chunks - first).min(256))
                    && page.hash.as_slice() == hash
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nntune_core::digest_enrollment::DigestEnrollment;

    fn id(value: u8) -> [u8; 16] {
        let mut id = [value; 16];
        id[6] = 0x40;
        id[8] = 0x80;
        id
    }

    #[test]
    fn persisted_pages_must_match_every_ordered_hash_and_geometry() {
        let expected = [[1; 32], [2; 32]];
        let length = 256 * 65_536 + 1;
        let page = |index, first_chunk, entry_count, value| CandidatePage {
            index,
            chunk_count: 257,
            first_chunk,
            entry_count,
            hash: vec![value; 32],
        };
        let mut pages = vec![page(0, 0, 256, 1), page(1, 256, 1, 2)];
        assert!(candidate_pages_match(length, &expected, &pages));
        assert!(!candidate_pages_match(0, &[], &[]));
        assert!(!candidate_pages_match(u64::MAX, &expected, &pages));
        assert!(!candidate_pages_match(length, &[], &pages));
        assert!(!candidate_pages_match(length, &expected, &[]));
        assert!(!candidate_pages_match(length, &expected[..1], &pages));
        assert!(!candidate_pages_match(length, &expected, &pages[..1]));
        pages.push(page(2, 512, 1, 2));
        assert!(!candidate_pages_match(length, &expected, &pages));
        pages.pop();
        pages.swap(0, 1);
        assert!(!candidate_pages_match(length, &expected, &pages));
        pages.swap(0, 1);
        for index in [0, -1] {
            pages[1].index = index;
            assert!(!candidate_pages_match(length, &expected, &pages));
        }
        pages[1].index = 1;
        pages[1].chunk_count = 256;
        assert!(!candidate_pages_match(length, &expected, &pages));
        pages[1].chunk_count = 257;
        pages[1].first_chunk = 255;
        assert!(!candidate_pages_match(length, &expected, &pages));
        pages[1].first_chunk = 256;
        pages[1].entry_count = 2;
        assert!(!candidate_pages_match(length, &expected, &pages));
        pages[1].entry_count = 1;
        pages[1].hash[0] ^= 1;
        assert!(!candidate_pages_match(length, &expected, &pages));
        pages[1].hash[0] ^= 1;
        pages[1].hash.pop();
        assert!(!candidate_pages_match(length, &expected, &pages));
    }

    #[test]
    fn persisted_pages_cover_maximum_geometry_without_truncation() {
        let length = 8_589_934_592;
        let expected = vec![[7; 32]; 512];
        let mut pages: Vec<_> = (0..512)
            .map(|index| CandidatePage {
                index,
                chunk_count: 131_072,
                first_chunk: index * 256,
                entry_count: 256,
                hash: vec![7; 32],
            })
            .collect();
        assert!(candidate_pages_match(length, &expected, &pages));
        pages[511].entry_count = 255;
        assert!(!candidate_pages_match(length, &expected, &pages));
        pages[511].entry_count = 256;
        pages[511].hash[31] ^= 1;
        assert!(!candidate_pages_match(length, &expected, &pages));
        assert!(!candidate_pages_match(length + 1, &expected, &pages));
    }

    #[test]
    fn candidate_binds_complete_intent_and_generation() {
        let identities = [id(1), id(2), id(3), id(4), id(5)];
        let mut builder = DigestEnrollment::new(1, identities, id(6)).unwrap();
        let header = builder.header().to_bytes();
        builder.seal_chunk(&[42]).unwrap();
        let manifest = builder.finish().ok().unwrap();
        let mut intent = CandidateIntent {
            identities,
            plaintext_len: 1,
            map_id: id(6),
            header,
            reservation_generation: 1,
        };
        assert_eq!(candidate_hashes(&intent, &manifest).unwrap().len(), 1);
        for index in 0..5 {
            intent.identities[index][0] ^= 1;
            assert_eq!(
                candidate_hashes(&intent, &manifest),
                Err(CandidateError::Identity)
            );
            intent.identities[index][0] ^= 1;
        }
        intent.plaintext_len = 2;
        assert_eq!(
            candidate_hashes(&intent, &manifest),
            Err(CandidateError::Identity)
        );
        intent.plaintext_len = 1;
        intent.map_id = id(7);
        assert_eq!(
            candidate_hashes(&intent, &manifest),
            Err(CandidateError::Identity)
        );
        intent.map_id = id(6);
        intent.header[0] ^= 1;
        assert_eq!(
            candidate_hashes(&intent, &manifest),
            Err(CandidateError::Identity)
        );
        intent.header = header;
        for generation in [0, -1, i64::MIN] {
            intent.reservation_generation = generation;
            assert_eq!(
                candidate_hashes(&intent, &manifest),
                Err(CandidateError::Identity)
            );
        }
    }
}
