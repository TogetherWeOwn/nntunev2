//! In-memory comparison for the diagnostics-disabled trusted-map profile.

use nntune_core::{media_envelope::EnvelopeContext, s3_locator::S3MediaMetadata};

use super::{Plan, UploadedPart};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ResumeBinding<'a> {
    /// Binds the upload target, exact wire header and object lengths.
    pub plan: Plan<'a>,
    pub configuration: [u8; 16],
    pub environment: &'a str,
    pub library: &'a str,
    pub session: &'a str,
    pub object: &'a str,
    pub generation: &'a str,
    pub envelope_scheme: u16,
}

impl ResumeBinding<'_> {
    pub(super) fn matches_context(&self, context: &EnvelopeContext) -> bool {
        let [_, environment, library, object, generation, configuration] = context.identities;
        let target = self.plan.target;
        self.envelope_scheme == 2
            && self.environment == hex::encode(environment)
            && self.library == hex::encode(library)
            && self.object == hex::encode(object)
            && self.generation == hex::encode(generation)
            && self.configuration == configuration
            && self.plan.header == context.header
            && self.plan.plaintext_len == context.plaintext_len
            && S3MediaMetadata::matches_target(target.bucket, target.key, context)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CheckpointPart<'a> {
    pub number: u32,
    pub len: usize,
    pub etag: &'a str,
    /// SHA-256 of the exact ciphertext part bytes, not the ETag or a plaintext digest.
    pub digest: [u8; 32],
}

pub struct ResumeCheckpoint<'a> {
    pub binding: ResumeBinding<'a>,
    pub parts: &'a [CheckpointPart<'a>],
}

impl ResumeCheckpoint<'_> {
    /// Compare the entire observed sequence with the saved prefix. Pass parts
    /// in their original order, including repeated entries across pages.
    /// An observation never supplies verified receipts or authorizes resume.
    pub(crate) fn matches_parts<'a>(
        &'a self,
        observed: impl IntoIterator<Item = CheckpointPart<'a>>,
    ) -> bool {
        self.parts.iter().copied().eq(observed)
    }
}

/// Match the caller's local binding and verified receipt prefix against a checkpoint.
/// Neither input may be reconstructed from provider state. A match only makes
/// resume worth considering; it does not validate sealer state or authorize any action.
pub fn matches_checkpoint(
    expected: ResumeBinding<'_>,
    receipts: &[UploadedPart<'_>],
    checkpoint: &ResumeCheckpoint<'_>,
) -> bool {
    expected.envelope_scheme == 2
        && expected == checkpoint.binding
        && receipts.len() == checkpoint.parts.len()
        && receipts
            .iter()
            .enumerate()
            .all(|(i, receipt)| receipt.plan == expected.plan && receipt.number as usize == i + 1)
        && checkpoint.matches_parts(receipts.iter().map(|receipt| CheckpointPart {
            number: receipt.number,
            len: receipt.len,
            etag: &receipt.etag,
            digest: receipt.digest,
        }))
}

#[cfg(test)]
#[path = "resume_checkpoint_tests.rs"]
mod tests;
