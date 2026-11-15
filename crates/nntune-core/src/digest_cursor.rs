//! Sequential ordering for a bounded range, not an authentication or serving API.
//!
//! The caller must verify and authenticate complete chunks inside `advance`.
//! A rejected index or operation error permanently fences this cursor.

use crate::digest_geometry::{RangeChunk, RangeGeometry};

#[derive(Debug, PartialEq, Eq)]
pub enum CursorError<E> {
    InvalidChunk,
    Operation(E),
}

/// Orders range chunks without retaining pages, ciphertext, or plaintext.
pub struct RangeCursor {
    plan: RangeGeometry,
    next: Option<u32>,
    last: Option<u32>,
}

impl RangeCursor {
    pub fn new(plan: RangeGeometry) -> Self {
        let (next, last) = plan
            .chunks()
            .map_or((None, None), |(first, last)| (Some(first), Some(last)));
        Self { plan, next, last }
    }

    /// The next selection, or `None` after completion or failure.
    pub fn next_chunk(&self) -> Option<RangeChunk> {
        self.plan.chunk(self.next?)
    }

    /// Advance only after the operation succeeds; any error fences the cursor.
    pub fn advance<T, E>(
        &mut self,
        index: u32,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, CursorError<E>> {
        if self.next != Some(index) || self.last.is_none() {
            self.next = None;
            return Err(CursorError::InvalidChunk);
        }
        match operation() {
            Ok(value) => {
                self.next = if self.last == Some(index) {
                    None
                } else {
                    index.checked_add(1)
                };
                Ok(value)
            }
            Err(error) => {
                self.next = None;
                Err(CursorError::Operation(error))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest_geometry::DigestGeometry;

    #[test]
    fn orders_chunks_across_pages_and_completes() {
        let boundary = 256 * 65_536;
        let plan = DigestGeometry::new(boundary + 1)
            .unwrap()
            .range(boundary - 1, boundary + 1)
            .unwrap();
        let mut cursor = RangeCursor::new(plan);
        let first = cursor.next_chunk().unwrap();
        assert_eq!(
            (first.index, first.page, first.start, first.end),
            (255, 0, 65_535, 65_536)
        );
        assert_eq!(cursor.advance(255, || Ok::<_, ()>(7)), Ok(7));
        let second = cursor.next_chunk().unwrap();
        assert_eq!(
            (second.index, second.page, second.start, second.end),
            (256, 1, 0, 1)
        );
        assert_eq!(cursor.advance(256, || Ok::<_, ()>(8)), Ok(8));
        assert_eq!(cursor.next_chunk(), None);
        assert_eq!(
            cursor.advance(256, || Ok::<_, ()>(9)),
            Err(CursorError::InvalidChunk)
        );
    }

    #[test]
    fn rejects_wrong_order_without_running_operation_and_fences() {
        let plan = DigestGeometry::new(65_537)
            .unwrap()
            .range(0, 65_537)
            .unwrap();
        let mut cursor = RangeCursor::new(plan);
        assert_eq!(
            cursor.advance(1, || -> Result<(), ()> { panic!("must not run") }),
            Err(CursorError::InvalidChunk)
        );
        assert_eq!(cursor.next_chunk(), None);
        assert_eq!(
            cursor.advance(0, || Ok::<_, ()>(())),
            Err(CursorError::InvalidChunk)
        );
    }

    #[test]
    fn operation_error_fences_after_prior_success() {
        let plan = DigestGeometry::new(65_537)
            .unwrap()
            .range(0, 65_537)
            .unwrap();
        let mut cursor = RangeCursor::new(plan);
        assert_eq!(cursor.advance(0, || Ok::<_, &str>(1)), Ok(1));
        assert_eq!(
            cursor.advance(1, || Err::<(), _>("bad chunk")),
            Err(CursorError::Operation("bad chunk"))
        );
        assert_eq!(cursor.next_chunk(), None);
        assert_eq!(
            cursor.advance(1, || Ok::<_, &str>(())),
            Err(CursorError::InvalidChunk)
        );
    }

    #[test]
    fn empty_object_still_selects_final_chunk_but_empty_range_does_not() {
        let mut empty = RangeCursor::new(DigestGeometry::new(0).unwrap().range(0, 0).unwrap());
        assert_eq!(empty.next_chunk().unwrap().index, 0);
        assert_eq!(empty.advance(0, || Ok::<_, ()>(())), Ok(()));
        assert_eq!(empty.next_chunk(), None);
        let mut no_selection =
            RangeCursor::new(DigestGeometry::new(1).unwrap().range(1, 1).unwrap());
        assert_eq!(no_selection.next_chunk(), None);
        assert_eq!(
            no_selection.advance(0, || Ok::<_, ()>(())),
            Err(CursorError::InvalidChunk)
        );
    }
}
