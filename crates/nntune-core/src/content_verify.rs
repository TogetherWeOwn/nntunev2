//! Complete-read integrity check for NNTune's encrypt-on-write stream.
//!
//! Feed decoded ciphertext in stored-plan order, from ONE backbone per verifier.
//! This authenticates every data block through the existing ranged reader and
//! checks the exact header, length and full plaintext SHA-256. It neither opens
//! envelopes nor resolves articles/accounts. Recovery-article completeness,
//! canonical independent-backbone provenance and generation/round fences remain
//! the caller's responsibilities. Success alone grants no reuse or R2 deletion.

use crate::decrypt::rclone::{self, CIPHER_OVERHEAD, HEADER_SIZE, PLAIN_BLOCK_SIZE};
use crate::decrypt::ReadAt;
use crate::encrypt::{ciphertext_len, header};
use sha2::{Digest, Sha256};
use std::{fmt, io};
use zeroize::Zeroizing;

/// Static error classes: never carry keys, plaintext or provider diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    InvalidInput,
    Incomplete,
    AuthenticationFailed,
    IntegrityFailed,
}

/// A complete authenticated data-stream result, NOT a generation health permit.
/// No partial digest or plaintext is released by the verifier.
pub struct VerifiedContent {
    sha256: [u8; 32],
    plaintext_size: u64,
}

impl fmt::Debug for VerifiedContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VerifiedContent([REDACTED])")
    }
}

impl VerifiedContent {
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    pub fn plaintext_size(&self) -> u64 {
        self.plaintext_size
    }
}

/// Bounded-memory, ordered ciphertext consumer. Key copies, plaintext working
/// buffers and buffered hash state zeroize on drop, including error paths.
/// A failed push poisons the entire
/// read: subsequent input cannot restore it, and finish cannot return success.
/// Inputs must come from the opened immutable custody, not a queue's claimed hash.
/// Empty plaintext is refused: its header-only layout has no authenticated block.
pub struct ContentVerifier {
    derived: Zeroizing<[u8; 80]>,
    expected_header: [u8; HEADER_SIZE as usize],
    expected_sha256: [u8; 32],
    plaintext_size: u64,
    ciphertext_size: u64,
    received: u64,
    plaintext_read: u64,
    cipher_offset: u64,
    pending: Zeroizing<Vec<u8>>,
    header_verified: bool,
    digest: Sha256,
    failed: Option<VerifyError>,
}

impl fmt::Debug for ContentVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContentVerifier([REDACTED])")
    }
}

impl ContentVerifier {
    pub fn new(
        file_key: &[u8; 32],
        header_nonce: &[u8; 24],
        plaintext_size: u64,
        expected_sha256: &[u8; 32],
    ) -> Result<Self, VerifyError> {
        let ciphertext_size = ciphertext_len(plaintext_size).ok_or(VerifyError::InvalidInput)?;
        if plaintext_size == 0 {
            return Err(VerifyError::InvalidInput);
        }
        let mut derived = Zeroizing::new([0u8; 80]);
        derived[..32].copy_from_slice(file_key);
        Ok(Self {
            derived,
            expected_header: header(header_nonce),
            expected_sha256: *expected_sha256,
            plaintext_size,
            ciphertext_size,
            received: 0,
            plaintext_read: 0,
            cipher_offset: HEADER_SIZE,
            pending: Zeroizing::new(Vec::with_capacity(rclone::CIPHER_BLOCK_SIZE as usize)),
            header_verified: false,
            digest: Sha256::new(),
            failed: None,
        })
    }

    /// Accept any chunk size, buffering at most one sealed block. There are no
    /// source reads or allocations proportional to the entire file/chunk size.
    /// Call finish only after the exact stored data-article stream has ended;
    /// never concatenate bytes from another backbone to fill a failed read.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), VerifyError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = self.consume(chunk);
        if let Err(error) = result {
            self.failed = Some(error);
        }
        result
    }

    fn consume(&mut self, mut chunk: &[u8]) -> Result<(), VerifyError> {
        self.received = self
            .received
            .checked_add(chunk.len() as u64)
            .filter(|&n| n <= self.ciphertext_size)
            .ok_or(VerifyError::IntegrityFailed)?;
        while !chunk.is_empty() {
            let plain_len = (self.plaintext_size - self.plaintext_read).min(PLAIN_BLOCK_SIZE);
            let want = if self.header_verified {
                plain_len as usize + CIPHER_OVERHEAD as usize
            } else {
                HEADER_SIZE as usize
            };
            let take = (want - self.pending.len()).min(chunk.len());
            self.pending.extend_from_slice(&chunk[..take]);
            chunk = &chunk[take..];
            if self.pending.len() != want {
                continue;
            }
            if !self.header_verified {
                if self.pending.as_slice() != self.expected_header {
                    return Err(VerifyError::IntegrityFailed);
                }
                self.header_verified = true;
            } else {
                let window = CipherWindow {
                    header: &self.expected_header,
                    offset: self.cipher_offset,
                    block: &self.pending,
                };
                let mut plaintext = Zeroizing::new(vec![0u8; plain_len as usize]);
                let n = rclone::read_at(
                    &window,
                    self.derived.as_ref(),
                    self.plaintext_size,
                    &mut plaintext,
                    self.plaintext_read,
                )
                .map_err(|_| VerifyError::AuthenticationFailed)?;
                if n != plaintext.len() {
                    return Err(VerifyError::IntegrityFailed);
                }
                self.digest.update(plaintext.as_slice());
                self.plaintext_read += plain_len;
                self.cipher_offset += want as u64;
            }
            self.pending.clear();
        }
        Ok(())
    }

    /// Only the full, authenticated, exact-size AND exact-digest stream succeeds.
    /// The result contains no plaintext and has no standalone health authority.
    pub fn finish(self) -> Result<VerifiedContent, VerifyError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if !self.header_verified
            || self.received != self.ciphertext_size
            || self.plaintext_read != self.plaintext_size
            || !self.pending.is_empty()
        {
            return Err(VerifyError::Incomplete);
        }
        let sha256: [u8; 32] = self.digest.finalize().into();
        if sha256 != self.expected_sha256 {
            return Err(VerifyError::IntegrityFailed);
        }
        Ok(VerifiedContent {
            sha256,
            plaintext_size: self.plaintext_read,
        })
    }
}

// Present one block at its original absolute coordinates to the existing reader.
// This reuses its header/nonce/counter/authentication and short-tail handling,
// rather than maintaining a second secretbox implementation.
struct CipherWindow<'a> {
    header: &'a [u8; HEADER_SIZE as usize],
    offset: u64,
    block: &'a [u8],
}

impl ReadAt for CipherWindow<'_> {
    fn read_at(&self, buf: &mut [u8], off: u64) -> io::Result<usize> {
        if off < HEADER_SIZE {
            self.header.as_slice().read_at(buf, off)
        } else if off >= self.offset {
            self.block.read_at(buf, off - self.offset)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid block window",
            ))
        }
    }
}
