//! Checked geometry for the digest-page format, not an authority or verifier.
//!
//! These calculations alone do not enroll origin bytes, validate a page, or
//! authorize serving. Empty input is supported for codec/restore fixtures only;
//! admission of new media remains the responsibility of the origin sealer.

const CHUNK: u64 = 65_536;
const MAX_PLAINTEXT: u64 = 1 << 33;
const PAGE_ENTRIES: u32 = 256;
const PAGE_HEADER: u32 = 92;

/// Bounded geometry computed from a trusted application plaintext length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigestGeometry {
    plaintext_len: u64,
    sealed_len: u64,
    chunks: u32,
    pages: u32,
}

/// The one canonical partition for a page in a digest map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageGeometry {
    pub index: u32,
    pub first: u32,
    pub count: u32,
    pub encoded_len: u32,
    pub final_page: bool,
}

impl DigestGeometry {
    /// Returns `None` outside the selected 8 GiB format bound.
    pub fn new(plaintext_len: u64) -> Option<Self> {
        if plaintext_len > MAX_PLAINTEXT {
            return None;
        }
        let chunks = plaintext_len
            .checked_add(CHUNK - 1)?
            .checked_div(CHUNK)?
            .max(1);
        let sealed_len = 48_u64
            .checked_add(plaintext_len)?
            .checked_add(chunks.checked_mul(16)?)?;
        let chunks = u32::try_from(chunks).ok()?;
        let pages = chunks
            .checked_add(PAGE_ENTRIES - 1)?
            .checked_div(PAGE_ENTRIES)?;
        Some(Self {
            plaintext_len,
            sealed_len,
            chunks,
            pages,
        })
    }

    pub fn plaintext_len(self) -> u64 {
        self.plaintext_len
    }
    pub fn sealed_len(self) -> u64 {
        self.sealed_len
    }
    pub fn chunks(self) -> u32 {
        self.chunks
    }
    pub fn pages(self) -> u32 {
        self.pages
    }

    /// The exact page partition, or `None` for an out-of-range index.
    pub fn page(self, index: u32) -> Option<PageGeometry> {
        if index >= self.pages {
            return None;
        }
        let first = index.checked_mul(PAGE_ENTRIES)?;
        let count = self.chunks.checked_sub(first)?.min(PAGE_ENTRIES);
        let encoded_len = PAGE_HEADER.checked_add(count.checked_mul(32)?)?;
        Some(PageGeometry {
            index,
            first,
            count,
            encoded_len,
            final_page: index == self.pages - 1,
        })
    }

    /// Complete sealed chunk length, including its tag, and finality.
    pub fn chunk(self, index: u32) -> Option<(u32, bool)> {
        if index >= self.chunks {
            return None;
        }
        let final_chunk = index == self.chunks - 1;
        let start = u64::from(index).checked_mul(CHUNK)?;
        let plaintext = self.plaintext_len.checked_sub(start)?.min(CHUNK);
        Some((u32::try_from(plaintext.checked_add(16)?).ok()?, final_chunk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_geometry_at_chunk_and_page_boundaries() {
        for (p, chunks, pages, last_len) in [
            (0, 1, 1, 16),
            (1, 1, 1, 17),
            (65_536, 1, 1, 65_552),
            (65_537, 2, 1, 17),
            (255 * CHUNK, 255, 1, 65_552),
            (256 * CHUNK, 256, 1, 65_552),
            (257 * CHUNK, 257, 2, 65_552),
            (1 << 33, 131_072, 512, 65_552),
        ] {
            let g = DigestGeometry::new(p).unwrap();
            assert_eq!((g.chunks(), g.pages()), (chunks, pages));
            assert_eq!(g.sealed_len(), 48 + p + 16 * u64::from(chunks));
            assert_eq!(g.chunk(chunks - 1), Some((last_len, true)));
            assert_eq!(g.chunk(chunks), None);
            let page = g.page(pages - 1).unwrap();
            assert_eq!(page.first, 256 * (pages - 1));
            assert_eq!(page.count, chunks - page.first);
            assert_eq!(page.encoded_len, 92 + 32 * page.count);
            assert!(page.final_page);
            assert_eq!(g.page(pages), None);
            if pages > 1 {
                assert_eq!(g.page(0).unwrap().count, 256);
                assert!(!g.page(0).unwrap().final_page);
                assert_eq!(g.chunk(0), Some((65_552, false)));
            }
        }
        assert!(DigestGeometry::new((1 << 33) + 1).is_none());
        assert!(DigestGeometry::new(u64::MAX).is_none());
    }
}
