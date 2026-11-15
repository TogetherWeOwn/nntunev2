//! Bounded, source-only range opening against sealer-origin evidence.
//!
//! This does not authorize serving or load provider data. The caller supplies
//! each complete page and chunk in order; a failed chunk releases no plaintext.

pub use crate::digest_chunk::OpenPart;
use crate::digest_chunk::{open_chunk_part, ChunkError};
use crate::digest_cursor::{CursorError, RangeCursor};
use crate::digest_enrollment::OriginManifest;
use crate::digest_verifier::PageError;
use crate::media::{FileKey, Header, KeyMode, Profile, VerifiedHeader};

#[cfg(test)]
#[path = "digest_range_tests.rs"]
mod tests;

#[derive(Debug, PartialEq, Eq)]
pub enum RangeError {
    InvalidRange,
    InvalidChunk,
    Header,
    Page(PageError),
    DigestMismatch,
    Authentication,
}

/// Sequential range cursor, retaining no page or plaintext between calls.
pub struct DigestRange<'a> {
    manifest: &'a OriginManifest,
    key: &'a FileKey,
    cursor: RangeCursor,
    range: std::ops::Range<u64>,
}

impl<'a> DigestRange<'a> {
    /// Select a half-open range; the empty object authenticates its final tag.
    pub fn new(
        manifest: &'a OriginManifest,
        key: &'a FileKey,
        start: u64,
        end: u64,
    ) -> Result<Self, RangeError> {
        let plan = manifest
            .geometry()
            .range(start, end)
            .ok_or(RangeError::InvalidRange)?;
        let header = Header::from_bytes(manifest.header_bytes());
        VerifiedHeader::verify(&Profile::nntune(), KeyMode::Random, key, &header)
            .map_err(|_| RangeError::Header)?;
        Ok(Self {
            manifest,
            key,
            cursor: RangeCursor::new(plan),
            range: start..end,
        })
    }

    /// The next chunk and page indices, or `None` after completion/failure.
    pub fn next_indices(&self) -> Option<(u32, u32)> {
        self.cursor
            .next_chunk()
            .map(|chunk| (chunk.index, chunk.page))
    }

    /// Verify and open exactly the next chunk. Chunk zero requires its wire header.
    /// An error fences this cursor; already returned authenticated parts remain valid.
    pub fn open_next(
        &mut self,
        index: u32,
        page: &[u8],
        sealed: Vec<u8>,
        wire_header: Option<&[u8]>,
    ) -> Result<OpenPart, RangeError> {
        self.cursor
            .advance(index, || {
                open_chunk_part(
                    self.manifest,
                    self.key,
                    self.range.clone(),
                    index,
                    page,
                    sealed,
                    wire_header,
                )
            })
            .map_err(|error| match error {
                CursorError::InvalidChunk | CursorError::Operation(ChunkError::InvalidChunk) => {
                    RangeError::InvalidChunk
                }
                CursorError::Operation(ChunkError::Header) => RangeError::Header,
                CursorError::Operation(ChunkError::Page(error)) => RangeError::Page(error),
                CursorError::Operation(ChunkError::DigestMismatch) => RangeError::DigestMismatch,
                CursorError::Operation(ChunkError::Authentication) => RangeError::Authentication,
            })
    }
}
