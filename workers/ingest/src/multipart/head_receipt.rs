//! Offline comparison only. No provider or publication authority.

use nntune_core::{
    media::{ciphertext_size, HEADER_SIZE, TAG_SIZE},
    media_envelope::EnvelopeContext,
    origin_sealer::MAX_NEW_PLAINTEXT,
    s3_locator::{S3Binding, S3Locator, S3MediaMetadata},
};

use super::{
    session_outcome::valid_version_pin, CompletedVersionedMedia, Completion, MultipartError,
};

/// Metadata observed for the exact HEAD request. Supply the configuration
/// used and the returned locator and length independently of the expected pin.
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
    compare_observation(&expected, context.identities[5], head)
}

impl CompletedVersionedMedia {
    /// Compare HEAD against the version captured by completion and its trusted
    /// context. This comparison only verifies transport metadata.
    pub fn verify_head(
        &self,
        context: &EnvelopeContext,
        head: Option<HeadReceipt<'_>>,
    ) -> Result<(), HeadReceiptError> {
        let expected = self.metadata();
        if !expected.matches_context(context) {
            return Err(HeadReceiptError::ExpectedObject);
        }
        compare_observation(expected, context.identities[5], head)
    }
}

fn compare_observation(
    expected: &S3MediaMetadata,
    configuration: [u8; 16],
    head: Option<HeadReceipt<'_>>,
) -> Result<(), HeadReceiptError> {
    use HeadReceiptError::*;

    if matches!(expected.locator().binding(), S3Binding::VersionId(pin) if !valid_version_pin(pin))
    {
        return Err(ExpectedObject);
    }
    let head = head.ok_or(MissingHead)?;
    let (Some(observed_configuration), Some(observed), Some(size)) =
        (head.configuration, head.locator, head.content_length)
    else {
        return Err(MissingMetadata);
    };
    if observed_configuration != configuration || observed != expected.locator() {
        return Err(WrongObject);
    }
    let max = ciphertext_size(MAX_NEW_PLAINTEXT).ok_or(InvalidSize)?;
    if size <= HEADER_SIZE + TAG_SIZE || size > max {
        return Err(InvalidSize);
    }
    (size == expected.ciphertext_len())
        .then_some(())
        .ok_or(SizeMismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_coalesced_version_even_when_head_agrees() {
        let id = 0x00000000_0000_4000_8000_000000000000_u128.to_be_bytes();
        let key = format!("media/{0}/{0}", hex::encode(id));
        let context = EnvelopeContext {
            identities: [id; 6],
            plaintext_len: 1,
            header: [0; 48],
        };
        let binding = S3Binding::VersionId("one,two".into());
        let locator = S3Locator::new("fixture-bucket".into(), key, binding).unwrap();
        let metadata = S3MediaMetadata::from_trusted_context(locator.clone(), &context).unwrap();
        let head = HeadReceipt {
            configuration: Some(id),
            locator: Some(&locator),
            content_length: Some(metadata.ciphertext_len()),
        };
        assert_eq!(
            compare_observation(&metadata, id, Some(head)),
            Err(HeadReceiptError::ExpectedObject)
        );
    }
}
