//! Source-only opening against sealer-origin evidence; not serving authority.

use crate::digest_enrollment::OriginManifest;
use crate::digest_verifier::PageError;
use crate::media::{FileKey, Header, KeyMode, Profile, VerifiedHeader};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq)]
pub enum ChunkError {
    Header,
    Page(PageError),
    InvalidChunk,
    DigestMismatch,
    Authentication,
}

/// Verify the enrolled header and page, then authenticate one complete chunk.
/// Chunk zero additionally requires the exact wire header. No plaintext escapes
/// on failure; successful plaintext is zeroized when dropped.
pub fn open_chunk(
    manifest: &OriginManifest,
    key: &FileKey,
    index: u32,
    page: &[u8],
    sealed: Vec<u8>,
    wire_header: Option<&[u8]>,
) -> Result<Zeroizing<Vec<u8>>, ChunkError> {
    let geometry = manifest.geometry();
    let header = Header::from_bytes(manifest.header_bytes());
    let header = VerifiedHeader::verify(&Profile::nntune(), KeyMode::Random, key, &header)
        .map_err(|_| ChunkError::Header)?;
    let reader = header
        .reader(geometry.plaintext_len())
        .map_err(|_| ChunkError::Header)?;
    if index == 0 {
        reader
            .check_wire_header(wire_header.ok_or(ChunkError::Header)?)
            .map_err(|_| ChunkError::Header)?;
    }
    let expected = manifest
        .verify_page(index / 256, page)
        .map_err(ChunkError::Page)?
        .expectation(index)
        .ok_or(ChunkError::InvalidChunk)?;
    let (length, final_chunk) = geometry.chunk(index).ok_or(ChunkError::InvalidChunk)?;
    if expected.index() != index
        || expected.sealed_len() != length
        || expected.final_chunk() != final_chunk
        || sealed.len() != length as usize
    {
        return Err(ChunkError::InvalidChunk);
    }
    let mut mismatch = false;
    reader
        .open_chunk_with_precheck(u64::from(index), sealed, |bytes| {
            if <[u8; 32]>::from(Sha256::digest(bytes)) != *expected.digest() {
                mismatch = true;
                return Err(crate::Error::Malformed("digest mismatch".into()));
            }
            Ok(())
        })
        .map_err(|_| {
            if mismatch {
                ChunkError::DigestMismatch
            } else {
                ChunkError::Authentication
            }
        })
}

#[cfg(test)]
#[path = "digest_chunk_tests.rs"]
mod tests;
