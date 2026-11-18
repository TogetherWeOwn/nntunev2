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
