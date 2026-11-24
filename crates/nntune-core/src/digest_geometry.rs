//! Checked geometry for the digest-page format, not an authority or verifier.
//!
//! These calculations alone do not enroll origin bytes, validate a page, or
//! authorize serving. Empty input is supported for codec/restore fixtures only;
//! admission of new media remains the responsibility of the origin sealer.

use crate::media::{chunk_offset, PLAIN_CHUNK_SIZE as CHUNK};
use std::ops::{Range, RangeInclusive};

const R2_PART_BYTES: u64 = 5_244_160;
const MAX_PLAINTEXT: u64 = 1 << 33;
const PAGE_ENTRIES: u32 = 256;
const PAGE_HEADER: u32 = 92;
pub const MAX_PAGE_ENCODED_LEN: u32 = PAGE_HEADER + PAGE_ENTRIES * 32;

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

/// Range selection only; callers must authenticate each complete chunk before use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeGeometry {
    geometry: DigestGeometry,
    start: u64,
    end: u64,
    chunks: Option<(u32, u32)>,
}

/// Complete chunk and page indices and the half-open slice within its plaintext.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeChunk {
    pub index: u32,
    pub page: u32,
    pub start: usize,
    pub end: usize,
}

/// The complete input to a chunk authentication and the slice to use on success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedRangeChunk {
    /// Half-open object offsets including the tag, excluding the media header.
    pub ciphertext: Range<u64>,
    pub final_chunk: bool,
    pub selection: RangeChunk,
}

impl RangeGeometry {
    /// Total ciphertext object size, including the separate media header.
    pub fn sealed_len(self) -> u64 {
        self.geometry.sealed_len()
    }

    /// Complete sealed chunks covering the selection, excluding the header.
    /// This describes coverage, not a recommendation to fetch it all at once.
    pub fn ciphertext_span(self) -> Option<Range<u64>> {
        let (first, last) = self.chunks?;
        Some(self.sealed_chunk(first)?.ciphertext.start..self.sealed_chunk(last)?.ciphertext.end)
    }

    /// Inclusive one-based R2 upload parts intersecting the ciphertext span.
    /// Parts partition the whole object (starting with the header) and may
    /// split a chunk or tag. They are not authentication or GET boundaries.
    pub fn r2_parts(self) -> Option<RangeInclusive<u64>> {
        let span = self.ciphertext_span()?;
        Some((1 + span.start / R2_PART_BYTES)..=(1 + span.end.checked_sub(1)? / R2_PART_BYTES))
    }

    /// Plan one complete chunk; authenticate before using `selection` to trim it.
    pub fn sealed_chunk(self, index: u32) -> Option<SealedRangeChunk> {
        let selection = self.chunk(index)?;
        let (len, final_chunk) = self.geometry.chunk(index)?;
        let start = chunk_offset(u64::from(index))?;
        Some(SealedRangeChunk {
            ciphertext: start..start.checked_add(u64::from(len))?,
            final_chunk,
            selection,
        })
    }

    /// Inclusive first and last chunk indices; `None` for empty nonempty-object ranges.
    pub fn chunks(self) -> Option<(u32, u32)> {
        self.chunks
    }

    /// Returns a selection only for a chunk belonging to this range.
    pub fn chunk(self, index: u32) -> Option<RangeChunk> {
        let (first, last) = self.chunks?;
        if index < first || index > last {
            return None;
        }
        let (sealed_len, _) = self.geometry.chunk(index)?;
        let length = u64::from(sealed_len.checked_sub(16)?);
        let offset = u64::from(index).checked_mul(CHUNK)?;
        let page = index.checked_div(PAGE_ENTRIES)?;
        self.geometry.page(page)?;
        Some(RangeChunk {
            index,
            page,
            start: usize::try_from(self.start.saturating_sub(offset).min(length)).ok()?,
            end: usize::try_from(self.end.saturating_sub(offset).min(length)).ok()?,
        })
    }
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

    /// Plan a validated half-open plaintext range without opening any bytes.
    pub fn range(self, start: u64, end: u64) -> Option<RangeGeometry> {
        if start > end || end > self.plaintext_len {
            return None;
        }
        let chunks = if self.plaintext_len == 0 {
            Some((0, 0)) // The empty object still has a final authentication tag.
        } else if start == end {
            None
        } else {
            Some((
                u32::try_from(start / CHUNK).ok()?,
                u32::try_from((end - 1) / CHUNK).ok()?,
            ))
        };
        Some(RangeGeometry {
            geometry: self,
            start,
            end,
            chunks,
        })
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
    fn range_validation_and_empty_objects() {
        let empty = DigestGeometry::new(0).unwrap();
        let plan = empty.range(0, 0).unwrap();
        assert_eq!(plan.chunks(), Some((0, 0)));
        assert_eq!(
            plan.chunk(0),
            Some(RangeChunk {
                index: 0,
                page: 0,
                start: 0,
                end: 0
            })
        );
        assert_eq!(plan.chunk(1), None);
        assert_eq!(empty.range(0, 1), None);
        let g = DigestGeometry::new(CHUNK + 1).unwrap();
        for point in [0, 1, CHUNK, CHUNK + 1] {
            let plan = g.range(point, point).unwrap();
            assert_eq!(plan.chunks(), None);
            assert_eq!(plan.chunk(0), None);
        }
        assert_eq!(g.range(2, 1), None);
        assert_eq!(g.range(0, CHUNK + 2), None);
        assert_eq!(g.range(u64::MAX, u64::MAX), None);
    }

    #[test]
    fn range_clips_at_chunk_and_page_boundaries() {
        let boundary = 256 * CHUNK;
        let g = DigestGeometry::new(boundary + 1).unwrap();
        let plan = g.range(boundary - 1, boundary + 1).unwrap();
        assert_eq!(plan.chunks(), Some((255, 256)));
        assert_eq!(
            plan.chunk(255),
            Some(RangeChunk {
                index: 255,
                page: 0,
                start: 65_535,
                end: 65_536
            })
        );
        assert_eq!(
            plan.chunk(256),
            Some(RangeChunk {
                index: 256,
                page: 1,
                start: 0,
                end: 1
            })
        );
        assert_eq!(plan.chunk(254), None);
        assert_eq!(plan.chunk(257), None);
        assert_eq!(plan.chunk(u32::MAX), None);
        let plan = g.range(CHUNK, 2 * CHUNK).unwrap();
        assert_eq!(plan.chunks(), Some((1, 1)));
        assert_eq!(
            plan.chunk(1),
            Some(RangeChunk {
                index: 1,
                page: 0,
                start: 0,
                end: 65_536
            })
        );
        let g = DigestGeometry::new(1 << 33).unwrap();
        let plan = g.range((1 << 33) - 1, 1 << 33).unwrap();
        assert_eq!(plan.chunks(), Some((131_071, 131_071)));
        assert_eq!(plan.chunk(131_071).unwrap().page, 511);
    }

    #[test]
    fn sealed_coverage_for_every_final_chunk_size() {
        // The wire chunk size is fixed; exhaust every possible final length.
        for tail in 1..=65_536_u64 {
            let last = [1, 79, 256, 131_071][(tail % 4) as usize];
            let offset = u64::from(last) * 65_536;
            let end = offset + tail;
            let plan = DigestGeometry::new(end)
                .unwrap()
                .range(offset - 1, end)
                .unwrap();
            let left = plan.sealed_chunk(last - 1).unwrap();
            let right = plan.sealed_chunk(last).unwrap();
            let split = 48 + offset + 16 * u64::from(last);
            assert_eq!(left.ciphertext, split - 65_552..split);
            assert_eq!(right.ciphertext, split..split + tail + 16);
            assert_eq!(plan.sealed_len(), right.ciphertext.end);
            assert_eq!(
                plan.ciphertext_span(),
                Some(left.ciphertext.start..right.ciphertext.end)
            );
            assert!(!left.final_chunk && right.final_chunk);
            assert_eq!((left.selection.start, left.selection.end), (65_535, 65_536));
            assert_eq!(
                (right.selection.start, right.selection.end),
                (0, tail as usize)
            );
            assert_eq!(right.selection.page, last / 256);
            assert_eq!(plan.sealed_chunk(last + 1), None);
        }
    }

    #[test]
    fn r2_parts_include_both_sides_of_a_split_chunk_but_not_an_empty_part() {
        let len = 80 * 65_536;
        let first_of_final = 79 * 65_536;
        let plan = DigestGeometry::new(len)
            .unwrap()
            .range(first_of_final, first_of_final + 1)
            .unwrap();
        assert_eq!(plan.ciphertext_span(), Some(5_178_656..5_244_208));
        assert_eq!(plan.r2_parts(), Some(1..=2));
        let len = len - 48;
        let exact = DigestGeometry::new(len)
            .unwrap()
            .range(len - 1, len)
            .unwrap();
        assert_eq!(exact.sealed_len(), 5_244_160);
        assert_eq!(exact.r2_parts(), Some(1..=1));
    }

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
