//! Inert S3 contract. Transport checksums do not authenticate media or authorize publication.

use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::media::{ciphertext_size, Header, HEADER_SIZE, PLAIN_CHUNK_SIZE, TAG_SIZE};
use nntune_core::media_envelope::EnvelopeContext;
use nntune_core::origin_sealer::{SealedChunk, MAX_NEW_PLAINTEXT};
use nntune_core::s3_locator::{S3Binding, S3Locator, S3MediaMetadata};
use sha2::{Digest, Sha256};
use xml::{reader::XmlEvent::*, Encoding, ParserConfig};

pub mod head_receipt;
pub mod readback_inventory;
pub mod resume_checkpoint;
pub mod session_completion;
pub mod session_outcome;
pub mod sidecar;

pub const PART_BYTES: usize = 5_244_160;
pub const MAX_COMPLETION_BODY: usize = 16 * 1024;
const S3_XML_NAMESPACE: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MultipartError {
    Geometry,
    PartReceipt,
    CompletionStatus,
    CompletionBody,
    EmbeddedError,
    Checksum,
    CompletionIdentity,
    CompletionVersion,
}

type Result<T> = std::result::Result<T, MultipartError>;

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
    pub fn new(target: UploadTarget<'a>, header: Header, plaintext_len: u64) -> Result<Self> {
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

    pub fn complete(self, parts: Vec<UploadedPart<'a>>) -> Result<Completion<'a>> {
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
        Ok(Completion {
            plan: self,
            parts,
            checksum: format!("{}-{count}", STANDARD.encode(composite.finalize())),
        })
    }
}

/// Partitions the wire header and ordered ciphertext-and-tag chunks, without map access.
pub struct Partition<'a> {
    plan: Plan<'a>,
    remaining: u64,
    number: u32,
    body: Vec<u8>,
}

impl<'a> Partition<'a> {
    pub fn push(&mut self, chunk: &SealedChunk) -> Result<Option<UploadPart<'a>>> {
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

    pub fn finish(self) -> Result<Option<UploadPart<'a>>> {
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

    pub fn verify_reply(&self, etag: String, checksum: &str) -> Result<UploadedPart<'a>> {
        if etag.trim().is_empty() || etag.len() > 1024 {
            return Err(MultipartError::PartReceipt);
        }
        (self.checksum == checksum)
            .then(|| UploadedPart {
                plan: self.plan,
                number: self.number,
                len: self.body.len(),
                etag,
                digest: self.digest,
                checksum: self.checksum.clone(),
            })
            .ok_or(MultipartError::Checksum)
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

/// One response to the exact completion request. The transport supplies the
/// configuration it used, not a value taken from a response header or the caller.
/// Pass raw `x-amz-version-id` occurrences from this response, not a HEAD or GET.
pub struct VersionedCompletionReply<'a> {
    pub configuration: [u8; 16],
    pub status: u16,
    pub body: &'a [u8],
    pub version_headers: &'a [&'a str],
}

/// Media portion of a completed inventory. Does not establish sidecar
/// completion, byte validity, provider qualification, or publication authority.
pub struct CompletedVersionedMedia(S3MediaMetadata);

impl CompletedVersionedMedia {
    pub fn metadata(&self) -> &S3MediaMetadata {
        &self.0
    }
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

    /// Verify the bounded receipt's composite checksum and any included bucket/key.
    pub fn verify_reply(&self, status: u16, body: &[u8]) -> Result<()> {
        self.verify_reply_target(status, body).map(|_| ())
    }

    /// Capture the exact version returned by a successful multipart completion.
    /// The caller must dispatch this plan against the authorized configuration
    /// and supply the unmodified response for that request with a trusted intent.
    pub fn verify_versioned_reply(
        &self,
        context: &EnvelopeContext,
        reply: VersionedCompletionReply<'_>,
    ) -> Result<CompletedVersionedMedia> {
        use MultipartError::{CompletionIdentity, CompletionVersion};
        if reply.configuration != context.identities[5]
            || self.plan.header != context.header
            || self.plan.plaintext_len != context.plaintext_len
            || !self.verify_reply_target(reply.status, reply.body)?
        {
            return Err(CompletionIdentity);
        }
        let [version] = reply.version_headers else {
            return Err(CompletionVersion);
        };
        // Bound the allocation and refuse coalesced field values. Do not trim,
        // decode or replace an opaque version (including with an ETag).
        if version.len() > 1024 || version.contains(',') {
            return Err(CompletionVersion);
        }
        let (target, ciphertext_len) = self.object();
        let locator = S3Locator::new(
            target.bucket.into(),
            target.key.into(),
            S3Binding::VersionId((*version).into()),
        )
        .map_err(|error| match error {
            nntune_core::s3_locator::S3LocatorError::Binding => CompletionVersion,
            _ => CompletionIdentity,
        })?;
        S3MediaMetadata::from_trusted_context(locator, context)
            .filter(|metadata| metadata.ciphertext_len() == ciphertext_len)
            .map(CompletedVersionedMedia)
            .ok_or(CompletionIdentity)
    }

    // Returns whether both target fields were present, in addition to the
    // existing checksum and identity checks applied to all replies.
    fn verify_reply_target(&self, status: u16, body: &[u8]) -> Result<bool> {
        if status != 200 {
            return Err(MultipartError::CompletionStatus);
        }
        use quick_xml::events::Event::{Decl, DocType, Empty, Eof, Start};
        use quick_xml::name::PrefixDeclaration::{Default, Named};
        use MultipartError::{Checksum, CompletionBody, EmbeddedError};
        const SUCCESS: &str = "CompleteMultipartUploadResult";
        const SHA256: &str = "ChecksumSHA256";
        if body.len() > MAX_COMPLETION_BODY || std::str::from_utf8(body).is_err() {
            return Err(CompletionBody);
        }
        // Allow only the element's own S3 namespace declaration, before resolution.
        let mut tokens = quick_xml::Reader::from_reader(body);
        loop {
            let tag = match tokens.read_event().map_err(|_| CompletionBody)? {
                Start(tag) | Empty(tag) => tag,
                Decl(_) | DocType(_) => return Err(CompletionBody),
                Eof => break,
                _ => continue,
            };
            let prefix = tag.name().prefix();
            let binding = prefix.map_or(Default, |p| Named(p.into_inner()));
            if matches!(binding, Named("" | "xml" | "xmlns"))
                || tag.attributes().any(|attr| match attr {
                    Ok(a) => {
                        a.value != S3_XML_NAMESPACE || a.key.as_namespace_binding() != Some(binding)
                    }
                    Err(_) => true,
                })
            {
                return Err(CompletionBody);
            }
        }
        let mut reader = ParserConfig::new()
            .override_encoding(Some(Encoding::Utf8))
            .allow_multiple_root_elements(false)
            .create_reader(body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body));
        let mut next = || reader.next().map_err(|_| CompletionBody);
        let (mut depth, mut root_ns, mut matched, mut typed) = (0, None, false, false);
        let (mut saw_bucket, mut saw_key) = (false, false);
        loop {
            match next()? {
                StartElement { name, .. } => {
                    match (depth, name.local_name.as_str(), name.namespace.as_deref()) {
                        (_, "Error", _) => return Err(EmbeddedError),
                        (0, SUCCESS, None | Some(S3_XML_NAMESPACE)) => root_ns = name.namespace,
                        (1, field @ (SHA256 | "ChecksumType" | "Bucket" | "Key"), n)
                            if n == root_ns.as_deref() =>
                        {
                            let (seen, want, error) = match field {
                                SHA256 => (&mut matched, self.checksum.as_str(), Checksum),
                                "Bucket" => {
                                    (&mut saw_bucket, self.plan.target.bucket, CompletionBody)
                                }
                                "Key" => (&mut saw_key, self.plan.target.key, CompletionBody),
                                _ => (&mut typed, "COMPOSITE", CompletionBody),
                            };
                            let valid = !*seen && matches!(next()?, Characters(s) if s == want);
                            if !valid || !matches!(next()?, EndElement { .. }) {
                                return Err(error);
                            }
                            *seen = true;
                            continue;
                        }
                        (1, "Location" | "ETag", _) => {}
                        _ => return Err(CompletionBody),
                    }
                    depth += 1;
                }
                Characters(_) if depth == 2 => {}
                StartDocument { .. } | Whitespace(_) => {}
                EndElement { .. } => depth -= 1,
                EndDocument => return matched.then_some(saw_bucket && saw_key).ok_or(Checksum),
                _ => return Err(CompletionBody),
            }
        }
    }
}
