//! Checkpoint-bound S3 multipart completion and its receipt validation.

use std::fmt::Write as _;

use nntune_core::{media_envelope::EnvelopeContext, s3_locator::S3MediaMetadata};
use quick_xml::escape::escape;

use super::{
    resume_checkpoint::{matches_checkpoint, ResumeBinding, ResumeCheckpoint},
    CompletedVersionedMedia, Completion, MultipartError, UploadTarget, UploadedPart,
    VersionedCompletionReply, S3_XML_NAMESPACE,
};

const MAX_UPLOAD_ID_BYTES: usize = 4096;

pub struct SessionCompletion<'a> {
    completion: Completion<'a>,
    context: &'a EnvelopeContext,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PrepareError {
    CheckpointMismatch,
    EnvelopeContextMismatch,
    Parts(MultipartError),
}

/// Exact body for a signed CompleteMultipartUpload POST to this target.
pub struct CompletionRequest<'a> {
    pub configuration: [u8; 16],
    pub target: UploadTarget<'a>,
    pub body: Vec<u8>,
}

/// Attributes from the transport and the same bounded response to the POST.
pub struct CompletionResponse {
    pub configuration: [u8; 16],
    pub status: u16,
    pub body: Vec<u8>,
    /// Pass the response's version header, including any comma-joined repeats.
    pub version: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DispatchError {
    InvalidRequest,
    /// The caller must reconcile the locked session before further S3 actions.
    Unconfirmed(Option<MultipartError>),
}

impl<'a> SessionCompletion<'a> {
    /// Use the binding from the authorized session, its trusted checkpoint and
    /// locally verified parts. The caller enforces reservation ownership,
    /// request fingerprint, lease and expiry before any provider operation.
    /// Environment, library, object and generation use compact lowercase UUIDs.
    /// The session ID is the exact identity from the trusted reservation.
    pub fn prepare(
        expected: ResumeBinding<'a>,
        context: &'a EnvelopeContext,
        receipts: Vec<UploadedPart<'a>>,
        checkpoint: &ResumeCheckpoint<'_>,
    ) -> Result<Self, PrepareError> {
        if !matches_checkpoint(expected, &receipts, checkpoint) {
            return Err(PrepareError::CheckpointMismatch);
        }
        let [_, environment, library, object, generation, _] = context.identities;
        let target = expected.plan.target;
        if expected.environment != hex::encode(environment)
            || expected.library != hex::encode(library)
            || expected.object != hex::encode(object)
            || expected.generation != hex::encode(generation)
            || expected.plan.header != context.header
            || expected.plan.plaintext_len != context.plaintext_len
            || !S3MediaMetadata::matches_target(target.bucket, target.key, context)
        {
            return Err(PrepareError::EnvelopeContextMismatch);
        }
        let completion = expected
            .plan
            .complete(receipts)
            .map_err(PrepareError::Parts)?;
        Ok(Self {
            completion,
            context,
        })
    }

    pub fn manifest(&self) -> impl Iterator<Item = (u32, &str, &str)> + use<'_, 'a> {
        self.completion.manifest()
    }

    /// Dispatch the authorized checkpoint once. The transport must sign the
    /// exact body for its independently configured bucket and upload ID, cap
    /// its response read at MAX_COMPLETION_BODY, and disable retries and redirects.
    pub async fn dispatch(
        self,
        send: impl AsyncFnOnce(CompletionRequest<'a>) -> Result<CompletionResponse, ()>,
    ) -> Result<CompletedVersionedMedia, DispatchError> {
        let (target, _) = self.completion.object();
        if !(1..=MAX_UPLOAD_ID_BYTES).contains(&target.upload_id.len())
            || !target.upload_id.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(DispatchError::InvalidRequest);
        }
        let mut body = format!("<CompleteMultipartUpload xmlns=\"{S3_XML_NAMESPACE}\">");
        for (number, etag, checksum) in self.manifest() {
            if etag
                .chars()
                .any(|c| c.is_control() || matches!(c, '\u{fffe}' | '\u{ffff}'))
            {
                return Err(DispatchError::InvalidRequest);
            }
            write!(body, "<Part><PartNumber>{number}</PartNumber><ETag>{}</ETag><ChecksumSHA256>{checksum}</ChecksumSHA256></Part>", escape(etag))
                .map_err(|_| DispatchError::InvalidRequest)?;
        }
        body.push_str("</CompleteMultipartUpload>");
        let response = send(CompletionRequest {
            configuration: self.context.identities[5],
            target,
            body: body.into_bytes(),
        })
        .await
        .map_err(|()| DispatchError::Unconfirmed(None))?;
        self.verify_completion(VersionedCompletionReply {
            configuration: response.configuration,
            status: response.status,
            body: &response.body,
            version_headers: response.version.as_deref().as_slice(),
        })
        .map_err(|error| DispatchError::Unconfirmed(Some(error)))
    }

    /// Capture the media version from the same completion response as the
    /// checksum, using the trusted session's context. Use this exact pin for
    /// subsequent HEAD and authenticated readback. The receipt conveys no
    /// publication authority. An ambiguous post-dispatch outcome requires
    /// fenced reconciliation before further upload actions.
    pub fn verify_completion(
        &self,
        reply: VersionedCompletionReply<'_>,
    ) -> Result<CompletedVersionedMedia, MultipartError> {
        self.completion.verify_versioned_reply(self.context, reply)
    }
}
