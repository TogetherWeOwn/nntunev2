//! Inert S3 request contract for the SHA-256 COMPOSITE profile. Transport
//! checksums do not authenticate media or authorize publication.

use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::media::{ciphertext_size, Header, HEADER_SIZE, PLAIN_CHUNK_SIZE, TAG_SIZE};
use nntune_core::origin_sealer::{SealedChunk, MAX_NEW_PLAINTEXT};
use quick_xml::{events::Event, Reader};
use sha2::{Digest, Sha256};

pub const PART_BYTES: usize = 5_244_160;
pub const CHECKSUM_ALGORITHM: &str = "SHA256";
pub const CHECKSUM_TYPE: &str = "COMPOSITE";
pub const MAX_COMPLETION_BODY: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MultipartError {
    Geometry,
    PartReceipt,
    CompletionStatus,
    CompletionBody,
    EmbeddedError,
    Checksum,
}

/// Opaque provider identity supplied by the caller; not an authorization check.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct UploadTarget<'a> {
    pub bucket: &'a str,
    pub key: &'a str,
    pub upload_id: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Plan<'a> {
    target: UploadTarget<'a>,
    header: [u8; HEADER_SIZE as usize],
    plaintext_len: u64,
    ciphertext_len: u64,
}

impl<'a> Plan<'a> {
    pub fn new(
        target: UploadTarget<'a>,
        header: Header,
        plaintext_len: u64,
    ) -> Result<Self, MultipartError> {
        if !(1..=MAX_NEW_PLAINTEXT).contains(&plaintext_len) {
            return Err(MultipartError::Geometry);
        }
        Ok(Self {
            target,
            header: header.to_bytes(),
            plaintext_len,
            ciphertext_len: ciphertext_size(plaintext_len).ok_or(MultipartError::Geometry)?,
        })
    }

    pub fn partition(self) -> Partition<'a> {
        let mut body = Vec::with_capacity(Self::part_len(self.ciphertext_len));
        body.extend_from_slice(&self.header);
        Partition {
            plan: self,
            remaining: self.plaintext_len,
            number: 1,
            body,
        }
    }

    fn part_len(remaining: u64) -> usize {
        remaining.min(PART_BYTES as u64) as usize
    }

    pub fn complete(self, parts: Vec<UploadedPart<'a>>) -> Result<Completion<'a>, MultipartError> {
        let count = self.ciphertext_len.div_ceil(PART_BYTES as u64) as usize;
        if parts.len() != count {
            return Err(MultipartError::Geometry);
        }
        let mut composite = Sha256::new();
        for (index, part) in parts.iter().enumerate() {
            let remaining = self.ciphertext_len - index as u64 * PART_BYTES as u64;
            if part.plan != self
                || part.number != index as u32 + 1
                || part.len != Self::part_len(remaining)
            {
                return Err(MultipartError::PartReceipt);
            }
            composite.update(part.digest);
        }
        let checksum = format!("{}-{count}", STANDARD.encode(composite.finalize()));
        Ok(Completion {
            plan: self,
            parts,
            checksum,
        })
    }
}

/// Byte partitioner over H || ciphertext-and-tag chunks. The caller supplies
/// ordered origin chunks; no chunk digest or map entry is read by this layer.
pub struct Partition<'a> {
    plan: Plan<'a>,
    remaining: u64,
    number: u32,
    body: Vec<u8>,
}

impl<'a> Partition<'a> {
    pub fn push(&mut self, chunk: &SealedChunk) -> Result<Option<UploadPart<'a>>, MultipartError> {
        let plain = self.remaining.min(PLAIN_CHUNK_SIZE);
        if plain == 0
            || chunk.final_chunk != (plain == self.remaining)
            || chunk.bytes.len() as u64 != plain + TAG_SIZE
        {
            return Err(MultipartError::Geometry);
        }
        self.remaining -= plain;
        let take = chunk.bytes.len().min(PART_BYTES - self.body.len());
        self.body.extend_from_slice(&chunk.bytes[..take]);
        if self.body.len() < PART_BYTES {
            return Ok(None);
        }
        let part = UploadPart::new(self.plan, self.number, std::mem::take(&mut self.body));
        self.number += 1;
        let consumed = (u64::from(self.number) - 1) * PART_BYTES as u64;
        self.body = Vec::with_capacity(Plan::part_len(self.plan.ciphertext_len - consumed));
        self.body.extend_from_slice(&chunk.bytes[take..]);
        Ok(Some(part))
    }

    pub fn finish(self) -> Result<Option<UploadPart<'a>>, MultipartError> {
        if self.remaining != 0 {
            return Err(MultipartError::Geometry);
        }
        Ok((!self.body.is_empty()).then(|| UploadPart::new(self.plan, self.number, self.body)))
    }
}

/// Keep this request for retries: target, part number, body and checksum are immutable.
pub struct UploadPart<'a> {
    plan: Plan<'a>,
    number: u32,
    body: Vec<u8>,
    digest: [u8; 32],
    checksum: String,
}

impl<'a> UploadPart<'a> {
    fn new(plan: Plan<'a>, number: u32, body: Vec<u8>) -> Self {
        let digest: [u8; 32] = Sha256::digest(&body).into();
        Self {
            plan,
            number,
            body,
            digest,
            checksum: STANDARD.encode(digest),
        }
    }

    /// Target, one-based part number, body, and `x-amz-checksum-sha256`.
    pub fn request(&self) -> (UploadTarget<'a>, u32, &[u8], &str) {
        (self.plan.target, self.number, &self.body, &self.checksum)
    }

    pub fn verify_reply(
        &self,
        etag: String,
        checksum: &str,
    ) -> Result<UploadedPart<'a>, MultipartError> {
        if etag.trim().is_empty() || etag.len() > 1024 {
            return Err(MultipartError::PartReceipt);
        }
        if self.checksum != checksum {
            return Err(MultipartError::Checksum);
        }
        Ok(UploadedPart {
            plan: self.plan,
            number: self.number,
            len: self.body.len(),
            etag,
            digest: self.digest,
            checksum: self.checksum.clone(),
        })
    }
}

pub struct UploadedPart<'a> {
    plan: Plan<'a>,
    number: u32,
    len: usize,
    etag: String,
    digest: [u8; 32],
    checksum: String,
}

pub struct Completion<'a> {
    plan: Plan<'a>,
    parts: Vec<UploadedPart<'a>>,
    checksum: String,
}

impl<'a> Completion<'a> {
    /// Target and exact total ciphertext length for the completion request.
    pub fn object(&self) -> (UploadTarget<'a>, u64) {
        (self.plan.target, self.plan.ciphertext_len)
    }

    /// Ordered completion manifest: part number, opaque ETag, part checksum.
    pub fn manifest(&self) -> impl Iterator<Item = (u32, &str, &str)> + use<'_, 'a> {
        self.parts
            .iter()
            .map(|p| (p.number, p.etag.as_str(), p.checksum.as_str()))
    }

    /// Require a complete XML success response and its exact composite checksum,
    /// with the part-count suffix. Even HTTP 200 can carry an S3 Error body.
    pub fn verify_reply(&self, status: u16, body: &[u8]) -> Result<(), MultipartError> {
        if status != 200 {
            return Err(MultipartError::CompletionStatus);
        }
        if body.len() > MAX_COMPLETION_BODY || std::str::from_utf8(body).is_err() {
            return Err(MultipartError::CompletionBody);
        }
        let mut reader = Reader::from_reader(body);
        reader.config_mut().expand_empty_elements = true;
        let (mut root, mut depth, mut matched) = (false, 0, false);
        let malformed = |_| MultipartError::CompletionBody;
        loop {
            match reader.read_event().map_err(malformed)? {
                Event::Start(tag) => {
                    if tag.local_name().into_inner() == "Error" {
                        return Err(MultipartError::EmbeddedError);
                    }
                    if tag.attributes().any(|a| a.is_err()) {
                        return Err(MultipartError::CompletionBody);
                    }
                    match (depth, tag.name().into_inner()) {
                        (0, "CompleteMultipartUploadResult") if !root => {
                            root = true;
                            depth = 1;
                        }
                        (1, "ChecksumSHA256") if !matched => {
                            let checksum = reader.read_text(tag.name()).map_err(malformed)?;
                            if checksum.as_ref() != self.checksum.as_str() {
                                return Err(MultipartError::Checksum);
                            }
                            matched = true;
                        }
                        (1, name) if name != "ChecksumSHA256" => depth = 2,
                        _ => return Err(MultipartError::CompletionBody),
                    }
                }
                Event::Text(_) | Event::GeneralRef(_) if depth == 2 => {}
                Event::Text(text) if text.as_bytes().iter().all(u8::is_ascii_whitespace) => {}
                Event::End(_) if depth > 0 => depth -= 1,
                Event::Decl(_) if !root => {}
                Event::Comment(_) => {}
                Event::Eof if root && depth == 0 => break,
                _ => return Err(MultipartError::CompletionBody),
            }
        }
        matched.then_some(()).ok_or(MultipartError::Checksum)
    }
}
