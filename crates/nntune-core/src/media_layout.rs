//! Keyless article bookkeeping for `velmora.media.v1`, not a crypto implementation.
//! The committing header is carried separately from the chunk spans. Stored
//! plans must identify this format; legacy offsets cannot be interpreted as v1.

use crate::media::{chunk_count, chunk_offset, ciphertext_size, PLAIN_CHUNK_SIZE};

/// An article's contiguous plaintext range and sealed chunk range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArticleSpan {
    pub plaintext_start: u64,
    pub plaintext_end: u64,
    pub ciphertext_offset: u64,
    pub ciphertext_len: u64,
}

/// Complete ciphertext chunks covering an inclusive plaintext range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeSpan {
    pub first_chunk: u64,
    pub last_chunk: u64,
    pub ciphertext_start: u64,
    pub ciphertext_end: u64,
    pub object_len: u64,
}

/// Plan a range from the trusted plaintext length, not a provider response.
/// `ciphertext_end` is exclusive; callers must verify and trim complete chunks.
pub fn plan_range(plaintext_len: u64, start: u64, end: u64) -> Option<RangeSpan> {
    if start > end || end >= plaintext_len {
        return None;
    }
    let object_len = ciphertext_size(plaintext_len)?;
    let first_chunk = start / PLAIN_CHUNK_SIZE;
    let last_chunk = end / PLAIN_CHUNK_SIZE;
    let ciphertext_start = chunk_offset(first_chunk)?;
    let ciphertext_end = if last_chunk.checked_add(1)? == chunk_count(plaintext_len) {
        object_len
    } else {
        chunk_offset(last_chunk.checked_add(1)?)?
    };
    Some(RangeSpan {
        first_chunk,
        last_chunk,
        ciphertext_start,
        ciphertext_end,
        object_len,
    })
}

/// Bound allocating bookkeeping even for adversarial sizes or article policies.
/// Empty media has one authenticated empty final chunk, as required by v1.
pub const MAX_ARTICLE_SPANS: u64 = 32_768;

pub fn plan_articles(plaintext_len: u64, chunks_per_article: u64) -> Option<Vec<ArticleSpan>> {
    if chunks_per_article == 0 {
        return None;
    }
    let chunks = chunk_count(plaintext_len);
    let count = chunks.div_ceil(chunks_per_article);
    if count > MAX_ARTICLE_SPANS {
        return None;
    }
    let total = ciphertext_size(plaintext_len)?;
    let mut spans = Vec::with_capacity(count as usize);
    let mut first = 0;
    while first < chunks {
        let end = first.saturating_add(chunks_per_article).min(chunks);
        let plaintext_start = first.checked_mul(PLAIN_CHUNK_SIZE)?.min(plaintext_len);
        let plaintext_end = if end == chunks {
            plaintext_len
        } else {
            end.checked_mul(PLAIN_CHUNK_SIZE)?
        };
        let ciphertext_offset = chunk_offset(first)?;
        let ciphertext_end = if end == chunks {
            total
        } else {
            chunk_offset(end)?
        };
        spans.push(ArticleSpan {
            plaintext_start,
            plaintext_end,
            ciphertext_offset,
            ciphertext_len: ciphertext_end.checked_sub(ciphertext_offset)?,
        });
        first = end;
    }
    Some(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{CIPHER_CHUNK_SIZE, HEADER_SIZE, TAG_SIZE};

    #[test]
    fn successor_geometry_includes_empty_final_authentication_and_short_tails() {
        for size in [
            0,
            1,
            PLAIN_CHUNK_SIZE - 1,
            PLAIN_CHUNK_SIZE,
            PLAIN_CHUNK_SIZE + 1,
            11 * PLAIN_CHUNK_SIZE,
            11 * PLAIN_CHUNK_SIZE + 1,
        ] {
            for policy in [1, 11, u64::MAX] {
                let spans = plan_articles(size, policy).unwrap();
                assert_eq!(spans[0].ciphertext_offset, 48);
                assert_eq!(spans[0].plaintext_start, 0);
                for pair in spans.windows(2) {
                    assert_eq!(pair[0].plaintext_end, pair[1].plaintext_start);
                    assert_eq!(
                        pair[0].ciphertext_offset + pair[0].ciphertext_len,
                        pair[1].ciphertext_offset
                    );
                }
                let last = spans.last().unwrap();
                assert_eq!(last.plaintext_end, size);
                assert_eq!(
                    last.ciphertext_offset + last.ciphertext_len,
                    ciphertext_size(size).unwrap()
                );
            }
        }
        assert_eq!(plan_articles(0, 1).unwrap()[0].ciphertext_len, TAG_SIZE);
        let spans = plan_articles(PLAIN_CHUNK_SIZE + 1, 1).unwrap();
        assert_eq!(spans[1].ciphertext_offset, HEADER_SIZE + CIPHER_CHUNK_SIZE);
        assert_eq!(spans[1].ciphertext_len, 17);
    }

    #[test]
    fn inclusive_ranges_cover_complete_chunks_and_reject_invalid_bounds() {
        let span = plan_range(65_537, 65_535, 65_536).unwrap();
        assert_eq!(span.first_chunk, 0);
        assert_eq!(span.last_chunk, 1);
        assert_eq!((span.ciphertext_start, span.ciphertext_end), (48, 65_617));
        assert_eq!(span.object_len, 65_617);
        let last = plan_range(65_537, 65_536, 65_536).unwrap();
        assert_eq!(
            (last.ciphertext_start, last.ciphertext_end),
            (65_600, 65_617)
        );
        let full = plan_range(65_536, 0, 65_535).unwrap();
        assert_eq!((full.ciphertext_start, full.ciphertext_end), (48, 65_600));
        assert!(plan_range(0, 0, 0).is_none());
        assert!(plan_range(10, 2, 1).is_none());
        assert!(plan_range(10, 0, 10).is_none());
        assert!(plan_range(u64::MAX, 0, 0).is_none());
    }

    #[test]
    fn invalid_or_unbounded_geometry_refuses() {
        assert!(plan_articles(1, 0).is_none());
        assert!(plan_articles(u64::MAX, 1).is_none());
        assert!(plan_articles(u64::MAX, u64::MAX).is_none());
        assert!(plan_articles((MAX_ARTICLE_SPANS + 1) * PLAIN_CHUNK_SIZE, 1).is_none());
    }
}
