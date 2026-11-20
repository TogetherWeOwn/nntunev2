//! Offline comparison only. No provider or publication authority.

use nntune_core::{
    media::{ciphertext_size, HEADER_SIZE, TAG_SIZE},
    media_envelope::EnvelopeContext,
    origin_sealer::MAX_NEW_PLAINTEXT,
    s3_locator::{S3Locator, S3MediaMetadata},
};

use super::{Completion, MultipartError};

/// Synthetic HEAD observations, not values copied from the expected object.
pub struct HeadReceipt<'a> {
    pub configuration: Option<[u8; 16]>,
    pub locator: Option<&'a S3Locator>,
    pub content_length: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HeadReceiptError {
    Completion(MultipartError),
    ExpectedObject,
    MissingHead,
    MissingMetadata,
    WrongObject,
    InvalidSize,
    SizeMismatch,
}

/// Check the completion response and HEAD against independently supplied expectations.
/// A locator binds the canonical object/generation key and version or opaque ETag.
/// A match says nothing about stored bytes, map eligibility or permission to reuse.
pub fn compare_head_receipt(
    completion: &Completion<'_>,
    status: u16,
    body: &[u8],
    context: &EnvelopeContext,
    locator: &S3Locator,
    head: Option<HeadReceipt<'_>>,
) -> Result<(), HeadReceiptError> {
    use HeadReceiptError::*;

    completion.verify_reply(status, body).map_err(Completion)?;
    let expected =
        S3MediaMetadata::from_trusted_context(locator.clone(), context).ok_or(ExpectedObject)?;
    let (target, length) = completion.object();
    if target.bucket != locator.bucket()
        || target.key != locator.key()
        || completion.plan.header != context.header
        || completion.plan.plaintext_len != expected.plaintext_len()
        || length != expected.ciphertext_len()
    {
        return Err(ExpectedObject);
    }
    let head = head.ok_or(MissingHead)?;
    let (Some(configuration), Some(observed), Some(size)) =
        (head.configuration, head.locator, head.content_length)
    else {
        return Err(MissingMetadata);
    };
    if configuration != context.identities[5] || observed != locator {
        return Err(WrongObject);
    }
    let max = ciphertext_size(MAX_NEW_PLAINTEXT).ok_or(InvalidSize)?;
    if size <= HEADER_SIZE + TAG_SIZE || size > max {
        return Err(InvalidSize);
    }
    (size == length).then_some(()).ok_or(SizeMismatch)
}
