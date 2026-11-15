//! Bounded, source-only range opening against sealer-origin evidence.
//!
//! This does not authorize serving or load provider data. The caller supplies
//! each complete page and chunk in order; a failed chunk releases no plaintext.

use crate::digest_enrollment::OriginManifest;
use crate::digest_verifier::PageError;
use crate::media::{FileKey, Header, KeyMode, Profile, VerifiedHeader, PLAIN_CHUNK_SIZE};
use sha2::{Digest, Sha256};
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
    header: VerifiedHeader,
    start: u64,
    end: u64,
    next: u32,
    last: Option<u32>,
    failed: bool,
}

impl<'a> DigestRange<'a> {
    /// Select a half-open range; the empty object authenticates its final tag.
    pub fn new(
        manifest: &'a OriginManifest,
        key: &FileKey,
        start: u64,
        end: u64,
    ) -> Result<Self, RangeError> {
        let geometry = manifest.geometry();
        if start > end || end > geometry.plaintext_len() {
            return Err(RangeError::InvalidRange);
        }
        let header = Header::from_bytes(manifest.header());
        let header = VerifiedHeader::verify(&Profile::nntune(), KeyMode::Random, key, &header)
            .map_err(|_| RangeError::Header)?;
        let (next, last) = if geometry.plaintext_len() == 0 {
            (0, Some(0))
        } else if start == end {
            (0, None)
        } else {
            (
                (start / PLAIN_CHUNK_SIZE) as u32,
                Some(((end - 1) / PLAIN_CHUNK_SIZE) as u32),
            )
        };
        Ok(Self {
            manifest,
            header,
            start,
            end,
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
            Some((self.next, self.next / 256))
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
        let geometry = self.manifest.geometry();
        let reader = self
            .header
            .reader(geometry.plaintext_len())
            .map_err(|_| RangeError::Header)?;
        if index == 0 {
            reader
                .check_wire_header(wire_header.ok_or(RangeError::Header)?)
                .map_err(|_| RangeError::Header)?;
        }
        let verified = self
            .manifest
            .verify_page(index / 256, page)
            .map_err(RangeError::Page)?;
        let expected = verified
            .expectation(index)
            .ok_or(RangeError::InvalidChunk)?;
        let (length, final_chunk) = geometry.chunk(index).ok_or(RangeError::InvalidChunk)?;
        if expected.index() != index
            || expected.sealed_len() != length
            || expected.final_chunk() != final_chunk
            || sealed.len() != length as usize
        {
            return Err(RangeError::InvalidChunk);
        }
        let mut mismatch = false;
        let opened = reader.open_chunk_with_precheck(u64::from(index), sealed, |bytes| {
            if <[u8; 32]>::from(Sha256::digest(bytes)) != *expected.digest() {
                mismatch = true;
                return Err(crate::Error::Malformed("digest mismatch".into()));
            }
            Ok(())
        });
        let bytes = opened.map_err(|_| {
            if mismatch {
                RangeError::DigestMismatch
            } else {
                RangeError::Authentication
            }
        })?;
        let chunk_start = u64::from(index) * PLAIN_CHUNK_SIZE;
        let start = self
            .start
            .saturating_sub(chunk_start)
            .min(bytes.len() as u64) as usize;
        let end = self.end.saturating_sub(chunk_start).min(bytes.len() as u64) as usize;
        if index == last {
            self.last = None;
        } else {
            self.next += 1;
        }
        Ok(OpenPart { bytes, start, end })
    }
}
