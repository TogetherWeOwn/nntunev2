//! Sealer-owned origin enrollment, not published or authorized serving.
//!
//! Publication requires an authorized, consistent trusted record, complete
//! immutable rows, lifecycle fencing and pinned sidecar identities. This
//! in-memory result neither supplies those controls nor verifies fetched pages.

use crate::digest_context::context_hash;
use crate::digest_geometry::DigestGeometry;
use crate::digest_page::{encode_page, page_hash};
use crate::media::{FileKey, Header};
use crate::origin_sealer::OriginSealer;

fn invalid() -> crate::Error {
    crate::Error::Malformed("invalid digest enrollment state".into())
}

/// Complete sealer-origin evidence. Its private fields cannot be hydrated from
/// provider bytes or caller-supplied hashes, and do not authorize serving.
///
/// Provider rows cannot be substituted into an existing manifest:
/// ```compile_fail
/// use nntune_core::digest_enrollment::OriginManifest;
/// fn replace(manifest: &mut OriginManifest, provider_hash: [u8; 32]) {
///     manifest.rows[0] = provider_hash;
/// }
/// ```
/// Nor can arbitrary provider hashes be appended to an enrollment:
/// ```compile_fail
/// use nntune_core::digest_enrollment::DigestEnrollment;
/// fn enroll(builder: &mut DigestEnrollment, provider_hash: [u8; 32]) {
///     builder.rows.push(provider_hash);
/// }
/// ```
#[allow(dead_code)] // Consumed by the independent verifier in the next slice.
pub struct OriginManifest {
    geometry: DigestGeometry,
    map_id: [u8; 16],
    context: [u8; 32],
    header: [u8; 48],
    rows: Vec<[u8; 32]>,
}

impl OriginManifest {
    /// Match trusted application identities and the exact origin header before
    /// exposing sealer-derived hashes for inactive candidate storage. This is
    /// not a reservation check, trusted enrollment, or authorization to reuse.
    pub fn candidate_page_hashes(
        &self,
        identities: &[[u8; 16]; 5],
        plaintext_len: u64,
        map_id: &[u8; 16],
        header: &[u8; 48],
    ) -> Option<&[[u8; 32]]> {
        if self.geometry.plaintext_len() != plaintext_len
            || &self.map_id != map_id
            || &self.header != header
            || context_hash(identities, self.geometry, header)? != self.context
            || self.rows.len() != self.geometry.pages() as usize
        {
            return None;
        }
        Some(&self.rows)
    }

    pub(crate) fn geometry(&self) -> DigestGeometry {
        self.geometry
    }
    pub(crate) fn header_bytes(&self) -> &[u8; 48] {
        &self.header
    }
    pub(crate) fn map_id(&self) -> &[u8; 16] {
        &self.map_id
    }
    pub(crate) fn context(&self) -> &[u8; 32] {
        &self.context
    }
    pub(crate) fn row(&self, index: usize) -> Option<&[u8; 32]> {
        self.rows.get(index)
    }

    #[cfg(test)]
    pub(crate) fn synthetic_test_manifest(
        geometry: DigestGeometry,
        map_id: [u8; 16],
        context: [u8; 32],
        rows: Vec<[u8; 32]>,
    ) -> Self {
        Self {
            geometry,
            map_id,
            context,
            header: [0; 48],
            rows,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_test_header(&mut self, header: [u8; 48]) {
        self.header = header;
    }

    #[cfg(test)]
    pub(crate) fn replace_test_row(&mut self, index: usize, hash: [u8; 32]) {
        self.rows[index] = hash;
    }
}

/// Owns the sequential sealer and buffers at most one page of chunk digests.
pub struct DigestEnrollment {
    sealer: OriginSealer,
    geometry: DigestGeometry,
    map_id: [u8; 16],
    context: [u8; 32],
    entries: Vec<[u8; 32]>,
    rows: Vec<[u8; 32]>,
    next_chunk: u32,
    failed: bool,
}

/// The exact sealed bytes and, at a page boundary, its canonical sidecar.
pub struct EnrollmentOutput {
    pub bytes: Vec<u8>,
    pub page: Option<Vec<u8>>,
}

impl DigestEnrollment {
    /// Identities are trusted application inputs in scheme-2 context order;
    /// the map UUID is independent and the header comes from the owned sealer.
    pub fn new(
        plaintext_len: u64,
        identities: [[u8; 16]; 5],
        map_id: [u8; 16],
    ) -> crate::Result<Self> {
        let geometry = DigestGeometry::new(plaintext_len).ok_or_else(invalid)?;
        if plaintext_len == 0 || map_id[6] >> 4 != 4 || map_id[8] >> 6 != 2 {
            return Err(invalid());
        }
        // Reject bad identities before drawing secret key material.
        context_hash(&identities, geometry, &[0; 48]).ok_or_else(invalid)?;
        let sealer = OriginSealer::new(plaintext_len)?;
        let context =
            context_hash(&identities, geometry, &sealer.header().to_bytes()).ok_or_else(invalid)?;
        Ok(Self {
            sealer,
            geometry,
            map_id,
            context,
            entries: Vec::with_capacity(256),
            rows: Vec::with_capacity(geometry.pages() as usize),
            next_chunk: 0,
            failed: false,
        })
    }

    /// For trusted wrapping only; never persist or log raw key bytes.
    pub fn key(&self) -> &FileKey {
        self.sealer.key()
    }

    pub fn header(&self) -> Header {
        self.sealer.header()
    }

    /// A failed call permanently fences the enrollment; discard any output.
    pub fn seal_chunk(&mut self, plaintext: &[u8]) -> crate::Result<EnrollmentOutput> {
        if self.failed {
            return Err(invalid());
        }
        let result = self.seal_next(plaintext);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn seal_next(&mut self, plaintext: &[u8]) -> crate::Result<EnrollmentOutput> {
        let (length, final_chunk) = self.geometry.chunk(self.next_chunk).ok_or_else(invalid)?;
        let sealed = self.sealer.seal_chunk(plaintext)?;
        if sealed.bytes.len() != length as usize || sealed.final_chunk != final_chunk {
            return Err(invalid());
        }
        self.entries.push(sealed.digest);
        self.next_chunk += 1;
        let index = self.rows.len() as u32;
        let partition = self.geometry.page(index).ok_or_else(invalid)?;
        let page = if self.entries.len() == partition.count as usize {
            let bytes = encode_page(
                self.geometry,
                &self.map_id,
                &self.context,
                index,
                &self.entries,
            )
            .ok_or_else(invalid)?;
            self.rows.push(page_hash(&bytes).ok_or_else(invalid)?);
            self.entries.clear();
            Some(bytes)
        } else {
            None
        };
        Ok(EnrollmentOutput {
            bytes: sealed.bytes,
            page,
        })
    }

    /// Consume the builder; incomplete or fenced state cannot mint a manifest.
    pub fn finish(self) -> crate::Result<OriginManifest> {
        if self.failed
            || self.next_chunk != self.geometry.chunks()
            || !self.entries.is_empty()
            || self.rows.len() != self.geometry.pages() as usize
            || self.rows.len() > 512
        {
            return Err(invalid());
        }
        Ok(OriginManifest {
            geometry: self.geometry,
            map_id: self.map_id,
            context: self.context,
            header: self.sealer.header().to_bytes(),
            rows: self.rows,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn id(byte: u8) -> [u8; 16] {
        let mut id = [byte; 16];
        id[6] = 0x40;
        id[8] = 0x80;
        id
    }

    #[test]
    fn enrolls_only_owned_sealer_output_at_boundaries() {
        for length in [
            1_u64,
            65_536,
            65_537,
            255 * 65_536,
            256 * 65_536,
            257 * 65_536,
        ] {
            let ids = [id(1), id(2), id(3), id(4), id(5)];
            let mut builder = DigestEnrollment::new(length, ids, id(6)).unwrap();
            let header = builder.header().to_bytes();
            let geometry = DigestGeometry::new(length).unwrap();
            let mut entries = Vec::new();
            let mut pages = Vec::new();
            for index in 0..geometry.chunks() {
                let (sealed_len, _) = geometry.chunk(index).unwrap();
                let output = builder
                    .seal_chunk(&vec![42; sealed_len as usize - 16])
                    .unwrap();
                entries.push(<[u8; 32]>::from(Sha256::digest(&output.bytes)));
                if let Some(page) = output.page {
                    assert_eq!(&page[92..], entries.concat());
                    pages.push(page);
                    entries.clear();
                }
            }
            let manifest = builder.finish().ok().unwrap();
            assert_eq!(manifest.geometry, geometry);
            assert_eq!(manifest.map_id, id(6));
            assert_eq!(manifest.header, header);
            assert_eq!(
                manifest.context,
                context_hash(&ids, geometry, &header).unwrap()
            );
            let hashes = manifest
                .candidate_page_hashes(&ids, length, &id(6), &header)
                .unwrap();
            assert_eq!(hashes.len(), geometry.pages() as usize);
            for (row, page) in hashes.iter().zip(&pages) {
                assert_eq!(*row, page_hash(page).unwrap());
            }
            for index in 0..ids.len() {
                let mut changed = ids;
                changed[index][0] ^= 1;
                assert!(manifest
                    .candidate_page_hashes(&changed, length, &id(6), &header)
                    .is_none());
            }
            assert!(manifest
                .candidate_page_hashes(&ids, length + 1, &id(6), &header)
                .is_none());
            assert!(manifest
                .candidate_page_hashes(&ids, length, &id(7), &header)
                .is_none());
            let mut changed_header = header;
            changed_header[0] ^= 1;
            assert!(manifest
                .candidate_page_hashes(&ids, length, &id(6), &changed_header)
                .is_none());
        }
    }

    #[test]
    fn candidate_view_rejects_incomplete_or_extra_rows() {
        let ids = [id(1), id(2), id(3), id(4), id(5)];
        let mut builder = DigestEnrollment::new(1, ids, id(6)).unwrap();
        let header = builder.header().to_bytes();
        builder.seal_chunk(&[42]).unwrap();
        let mut manifest = builder.finish().ok().unwrap();
        let hash = manifest.rows[0];
        assert_eq!(
            manifest.candidate_page_hashes(&ids, 1, &id(6), &header),
            Some(&[hash][..])
        );
        manifest.rows.clear();
        assert!(manifest
            .candidate_page_hashes(&ids, 1, &id(6), &header)
            .is_none());
        manifest.rows.extend([hash, hash]);
        assert!(manifest
            .candidate_page_hashes(&ids, 1, &id(6), &header)
            .is_none());
    }

    #[test]
    fn finish_rejects_missing_and_extra_rows() {
        for extra in [false, true] {
            let mut builder = DigestEnrollment::new(1, [id(1); 5], id(2)).unwrap();
            builder.seal_chunk(&[1]).unwrap();
            if extra {
                builder.rows.push(builder.rows[0]);
            } else {
                builder.rows.clear();
            }
            assert!(builder.finish().is_err());
        }
    }

    #[test]
    fn rejects_invalid_and_incomplete_lifecycles() {
        let ids = [id(1); 5];
        assert!(DigestEnrollment::new(0, ids, id(2)).is_err());
        assert!(DigestEnrollment::new((1 << 33) + 1, ids, id(2)).is_err());
        assert!(DigestEnrollment::new(1, [[0; 16]; 5], id(2)).is_err());
        assert!(DigestEnrollment::new(1, ids, [0; 16]).is_err());
        assert!(DigestEnrollment::new(1, ids, id(2))
            .unwrap()
            .finish()
            .is_err());
        let mut builder = DigestEnrollment::new(65_537, ids, id(2)).unwrap();
        assert!(builder.seal_chunk(&[1]).is_err());
        assert!(builder.seal_chunk(&vec![1; 65_536]).is_err());
        assert!(builder.finish().is_err());
        let mut builder = DigestEnrollment::new(1, ids, id(2)).unwrap();
        builder.seal_chunk(&[1]).unwrap();
        assert!(builder.seal_chunk(&[]).is_err());
        assert!(builder.finish().is_err());
        let max = DigestGeometry::new(1 << 33).unwrap();
        assert_eq!((max.pages(), max.chunks()), (512, 131_072));
        assert_eq!(max.page(511).unwrap().count, 256);
    }
}
