//! Bounded, source-only range opening against sealer-origin evidence.
//!
//! This does not authorize serving or load provider data. The caller supplies
//! each complete page and chunk in order; a failed chunk releases no plaintext.

use crate::digest_chunk::{open_chunk, ChunkError};
use crate::digest_enrollment::OriginManifest;
use crate::digest_geometry::RangeGeometry;
use crate::digest_verifier::PageError;
use crate::media::{FileKey, Header, KeyMode, Profile, VerifiedHeader};
use zeroize::Zeroizing;

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

/// A single authenticated portion. The entire opened chunk is zeroized on drop.
pub struct OpenPart {
    bytes: Zeroizing<Vec<u8>>,
    start: usize,
    end: usize,
}

impl OpenPart {
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[self.start..self.end]
    }
}

/// Sequential range cursor, retaining no page or plaintext between calls.
pub struct DigestRange<'a> {
    manifest: &'a OriginManifest,
    key: &'a FileKey,
    plan: RangeGeometry,
    next: u32,
    last: Option<u32>,
    failed: bool,
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
        let (next, last) = plan
            .chunks()
            .map_or((0, None), |(first, last)| (first, Some(last)));
        Ok(Self {
            manifest,
            key,
            plan,
            next,
            last,
            failed: false,
        })
    }

    /// The next chunk and page indices, or `None` after completion/failure.
    pub fn next_indices(&self) -> Option<(u32, u32)> {
        if self.failed || self.last.is_none() {
            None
        } else {
            self.plan
                .chunk(self.next)
                .map(|chunk| (chunk.index, chunk.page))
        }
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
        let result = self.open_one(index, page, sealed, wire_header);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn open_one(
        &mut self,
        index: u32,
        page: &[u8],
        sealed: Vec<u8>,
        wire_header: Option<&[u8]>,
    ) -> Result<OpenPart, RangeError> {
        let last = self.last.ok_or(RangeError::InvalidChunk)?;
        if self.failed || index != self.next || index > last {
            return Err(RangeError::InvalidChunk);
        }
        let selection = self.plan.chunk(index).ok_or(RangeError::InvalidChunk)?;
        let bytes = open_chunk(self.manifest, self.key, index, page, sealed, wire_header).map_err(
            |error| match error {
                ChunkError::Header => RangeError::Header,
                ChunkError::Page(error) => RangeError::Page(error),
                ChunkError::InvalidChunk => RangeError::InvalidChunk,
                ChunkError::DigestMismatch => RangeError::DigestMismatch,
                ChunkError::Authentication => RangeError::Authentication,
            },
        )?;
        if selection.end > bytes.len() {
            return Err(RangeError::InvalidChunk);
        }
        if index == last {
            self.last = None;
        } else {
            self.next += 1;
        }
        Ok(OpenPart {
            bytes,
            start: selection.start,
            end: selection.end,
        })
    }
}
