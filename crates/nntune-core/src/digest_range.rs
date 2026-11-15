//! Bounded range planning against sealer-origin evidence, not a serving API.
//!
//! Construction authenticates the manifest header, but does not verify pages
//! or open chunks. Each complete chunk must be authenticated before release.

use crate::digest_cursor::RangeCursor;
use crate::digest_enrollment::OriginManifest;
use crate::media::{FileKey, Header, KeyMode, Profile, VerifiedHeader};

#[cfg(test)]
#[path = "digest_range_tests.rs"]
mod tests;

#[derive(Debug, PartialEq, Eq)]
pub enum RangeError {
    InvalidRange,
    Header,
}

/// Sequential range plan; no page, ciphertext, or plaintext is retained.
pub struct DigestRange<'a> {
    // Retained for the authenticated chunk-opening successor.
    #[allow(dead_code)]
    manifest: &'a OriginManifest,
    #[allow(dead_code)]
    key: &'a FileKey,
    cursor: RangeCursor,
    #[allow(dead_code)]
    range: std::ops::Range<u64>,
}

impl<'a> DigestRange<'a> {
    /// Select a half-open range; an empty object selects its final tag.
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

    /// The initial chunk and page indices, or `None` for an empty selection.
    pub fn next_indices(&self) -> Option<(u32, u32)> {
        self.cursor
            .next_chunk()
            .map(|chunk| (chunk.index, chunk.page))
    }
}
