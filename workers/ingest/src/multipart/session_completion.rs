//! Inactive composition of the local checkpoint and transport receipt checks.

use nntune_core::media_envelope::EnvelopeContext;

use super::{
    resume_checkpoint::{matches_checkpoint, ResumeBinding, ResumeCheckpoint},
    CompletedVersionedMedia, Completion, MultipartError, UploadedPart, VersionedCompletionReply,
};

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

impl<'a> SessionCompletion<'a> {
    /// Use the binding from the authorized session and locally verified parts.
    /// A matching prefix alone is insufficient: completion needs every part.
    /// Scope and lifecycle IDs in the binding use compact lowercase UUIDs.
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
        if expected.environment != hex::encode(environment)
            || expected.library != hex::encode(library)
            || expected.object != hex::encode(object)
            || expected.generation != hex::encode(generation)
            || expected.plan.header != context.header
            || expected.plan.plaintext_len != context.plaintext_len
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

    /// Capture the media version from the same completion response as the
    /// checksum. Use the trusted session's context and issue HEAD against this pin.
    pub fn verify_completion(
        &self,
        reply: VersionedCompletionReply<'_>,
    ) -> Result<CompletedVersionedMedia, MultipartError> {
        self.completion.verify_versioned_reply(self.context, reply)
    }
}
