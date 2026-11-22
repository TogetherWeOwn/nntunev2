//! S3 ciphertext range validation and bounded body reading. No transport wiring.

use nntune_core::{
    digest_geometry::RangeGeometry,
    digest_readback::ReadbackBody,
    s3_locator::{S3Binding, S3Locator},
};

use crate::range::{content_range_value, ByteRange};

/// Synthetic response fields; absent or ambiguous metadata must not be supplied as a match.
#[derive(Clone, Copy)]
pub struct S3RangeResponse<'a> {
    pub status: u16,
    pub content_range: Option<&'a str>,
    pub content_length: Option<u64>,
    pub binding: Option<&'a S3Binding>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum S3RangeResponseError {
    NoCiphertext,
    Status,
    ContentRange,
    ObjectIdentity,
    Length,
    Expired,
    Body,
}

/// Read one planned sealed chunk. For protected playback, the expected locator
/// must come from the authorized reference's confirmed lookup commit. The
/// request locator records what the transport actually used with the trusted
/// provider configuration. Supply the binding observed in the response.
/// `is_live` must require time strictly before the returned `usable_until`;
/// the transport must cancel pending I/O at that deadline. Verify digest/AEAD
/// before releasing plaintext.
pub async fn read_s3_range_chunk(
    plan: RangeGeometry,
    index: u32,
    expected_locator: &S3Locator,
    requested_locator: &S3Locator,
    response: S3RangeResponse<'_>,
    mut body: impl ReadbackBody,
    mut is_live: impl FnMut() -> bool,
) -> Result<Vec<u8>, S3RangeResponseError> {
    use S3RangeResponseError::*;

    if !is_live() {
        return Err(Expired);
    }
    if requested_locator != expected_locator {
        return Err(ObjectIdentity);
    }
    let chunk = plan.sealed_chunk(index).ok_or(NoCiphertext)?;
    let len =
        validate_s3_range_metadata(plan, chunk.ciphertext, expected_locator.binding(), response)?;
    let mut bytes = vec![0; len as usize];
    let mut remaining = bytes.as_mut_slice();
    while !remaining.is_empty() {
        let result = body.read(remaining).await;
        if !is_live() {
            return Err(Expired);
        }
        let count = result.map_err(|_| Body)?;
        if count == 0 {
            return Err(Length);
        }
        remaining = remaining.get_mut(count..).ok_or(Body)?;
    }
    let eof = body.read(&mut [0]).await;
    if !is_live() {
        return Err(Expired);
    }
    match eof {
        Ok(0) => Ok(bytes),
        Ok(1) => Err(Length),
        _ => Err(Body),
    }
}

/// Check one completed response against the trusted plan and pinned version or ETag.
/// `body_bytes` must be the measured byte count through EOF, not a header or a
/// truncated count. A future caller must enforce the planned bound while reading.
/// Success is not authentication: the existing digest range reader must still
/// verify each complete chunk against the trusted map before any plaintext release.
pub fn validate_s3_range_response(
    plan: RangeGeometry,
    expected_binding: &S3Binding,
    response: S3RangeResponse<'_>,
    body_bytes: u64,
) -> Result<(), S3RangeResponseError> {
    let span = plan
        .ciphertext_span()
        .ok_or(S3RangeResponseError::NoCiphertext)?;
    let len = validate_s3_range_metadata(plan, span, expected_binding, response)?;
    if body_bytes != len {
        return Err(S3RangeResponseError::Length);
    }
    Ok(())
}

/// Check headers before reading any of the body; return the exact read bound.
fn validate_s3_range_metadata(
    plan: RangeGeometry,
    span: std::ops::Range<u64>,
    expected_binding: &S3Binding,
    response: S3RangeResponse<'_>,
) -> Result<u64, S3RangeResponseError> {
    use S3RangeResponseError::*;

    if response.status != 206 {
        return Err(Status);
    }
    let range = ByteRange {
        start: span.start,
        end: span.end - 1,
    };
    let expected_range = content_range_value(range, plan.sealed_len());
    if response.content_range != Some(expected_range.as_str()) {
        return Err(ContentRange);
    }
    if response.binding != Some(expected_binding) {
        return Err(ObjectIdentity);
    }
    let expected_len = range.len();
    if response.content_length != Some(expected_len) {
        return Err(Length);
    }
    Ok(expected_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::plan_encrypted_range;
    use nntune_core::{
        digest_enrollment::DigestEnrollment,
        digest_geometry::DigestGeometry,
        digest_range::DigestRange,
        media_envelope::{wrap_scheme2, EnvelopeContext},
        s3_locator::S3MediaMetadata,
    };
    use std::cell::Cell;

    struct SliceBody<'a>(&'a [u8], &'a Cell<usize>);

    impl ReadbackBody for SliceBody<'_> {
        async fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
            self.1.set(self.1.get() + 1);
            std::io::Read::read(&mut self.0, dst)
        }
    }

    #[tokio::test]
    async fn reads_only_a_complete_chunk_before_expiry() {
        use S3RangeResponseError::*;

        let id = |n| {
            let mut bytes = [n; 16];
            bytes[6] = 0x40;
            bytes[8] = 0x80;
            bytes
        };
        let mut enrollment =
            DigestEnrollment::new(65_537, [id(1), id(2), id(3), id(4), id(5)], id(6)).unwrap();
        let context = EnvelopeContext {
            identities: [id(7), id(1), id(2), id(3), id(4), id(5)],
            plaintext_len: 65_537,
            header: enrollment.header().to_bytes(),
        };
        let root = [0xa5; 32];
        let record = wrap_scheme2(&root, &context, enrollment.key().expose_secret()).unwrap();
        enrollment.seal_chunk(&vec![0; 65_536]).unwrap();
        let final_chunk = enrollment.seal_chunk(b"x").unwrap();
        let manifest = enrollment.finish().ok().unwrap();

        let key = "media/03030303030340038003030303030303/04040404040440048004040404040404";
        let locate = |bucket: &str, key: &str, version: &str| {
            S3Locator::new(
                bucket.into(),
                key.into(),
                S3Binding::VersionId(version.into()),
            )
            .unwrap()
        };
        let media = S3MediaMetadata::from_trusted_context(
            locate("fixture-media", key, "media-pin"),
            &context,
        )
        .unwrap();
        let expected = media.locator();
        let plan = DigestGeometry::new(media.plaintext_len())
            .unwrap()
            .range(65_536, 65_537)
            .unwrap();
        let response = S3RangeResponse {
            status: 206,
            content_range: Some("bytes 65600-65616/65617"),
            content_length: Some(17),
            binding: Some(expected.binding()),
        };
        let reads = Cell::new(0);
        let data = &final_chunk.bytes;
        let mut trailing = data.clone();
        trailing.push(0);
        for incomplete_body in [&data[..16], &trailing] {
            assert_eq!(
                read_s3_range_chunk(
                    plan,
                    1,
                    expected,
                    expected,
                    response,
                    SliceBody(incomplete_body, &reads),
                    || true,
                )
                .await,
                Err(Length)
            );
        }

        let other_object = key.replacen("030303", "050505", 1);
        let wrong_version = S3Binding::VersionId("other-version".into());
        for (requested, observed) in [
            (locate("other-bucket", key, "media-pin"), response),
            (
                locate("fixture-media", &other_object, "media-pin"),
                response,
            ),
            (locate("fixture-media", key, "other-version"), response),
            (
                expected.clone(),
                S3RangeResponse {
                    binding: Some(&wrong_version),
                    ..response
                },
            ),
        ] {
            reads.set(0);
            assert_eq!(
                read_s3_range_chunk(
                    plan,
                    1,
                    expected,
                    &requested,
                    observed,
                    SliceBody(data, &reads),
                    || true
                )
                .await,
                Err(ObjectIdentity)
            );
            assert_eq!(reads.get(), 0);
        }

        // Expire before the first read, after the body, and at completion of the EOF probe.
        for deadline in 0..=2 {
            reads.set(0);
            assert_eq!(
                read_s3_range_chunk(
                    plan,
                    1,
                    expected,
                    expected,
                    response,
                    SliceBody(data, &reads),
                    || reads.get() < deadline
                )
                .await,
                Err(Expired)
            );
            assert_eq!(reads.get(), deadline);
        }
        let sealed = read_s3_range_chunk(
            plan,
            1,
            expected,
            expected,
            response,
            SliceBody(data, &reads),
            || true,
        )
        .await
        .unwrap();
        let mut cursor = DigestRange::from_enrolled(
            &manifest,
            &root,
            &context,
            &record,
            &record,
            65_536..65_537,
        )
        .unwrap();
        let opened = cursor
            .open_next(1, &final_chunk.page.unwrap(), sealed, None)
            .unwrap();
        assert_eq!(opened.as_slice(), b"x");
        assert_eq!(cursor.next_indices(), None);
    }

    #[test]
    fn accepts_complete_boundary_and_short_final_spans() {
        let geometry = DigestGeometry::new(65_537).unwrap();
        for (request, content_range, len, binding) in [
            (
                "bytes=65535-65536",
                "bytes 48-65616/65617",
                65_569,
                S3Binding::VersionId("v1".into()),
            ),
            (
                "bytes=65536-65536",
                "bytes 65600-65616/65617",
                17,
                S3Binding::Etag("\"tag\"".into()),
            ),
        ] {
            let plan = plan_encrypted_range(Some(request), geometry).1.unwrap();
            let body = vec![0xa5_u8; len];
            let response = S3RangeResponse {
                status: 206,
                content_range: Some(content_range),
                content_length: Some(len as u64),
                binding: Some(&binding),
            };
            let result = validate_s3_range_response(plan, &binding, response, body.len() as u64);
            assert_eq!(result, Ok(()), "{request}");
        }
    }

    #[test]
    fn rejects_ignored_range_wrong_identity_and_inconsistent_lengths() {
        use S3RangeResponseError::*;

        let geometry = DigestGeometry::new(65_537).unwrap();
        let plan = plan_encrypted_range(Some("bytes=-1"), geometry).1.unwrap();
        let binding = S3Binding::VersionId("v1".into());
        let other_version = S3Binding::VersionId("v2".into());
        let wrong_kind = S3Binding::Etag("v1".into());
        let valid = S3RangeResponse {
            status: 206,
            content_range: Some("bytes 65600-65616/65617"),
            content_length: Some(17),
            binding: Some(&binding),
        };
        let validate = |response, len| validate_s3_range_response(plan, &binding, response, len);
        for (status, len) in [(200, 65_617), (416, 17)] {
            let response = S3RangeResponse { status, ..valid };
            assert_eq!(validate(response, len), Err(Status));
        }
        for content_range in [
            None,
            Some("bytes 65599-65615/65617"),
            Some("bytes 65600-65617/65617"),
            Some("bytes 65600-65616/65618"),
            Some("bytes 65600-65616/*"),
        ] {
            let response = S3RangeResponse {
                content_range,
                ..valid
            };
            assert_eq!(validate(response, 17), Err(ContentRange));
        }
        for binding in [None, Some(&other_version), Some(&wrong_kind)] {
            let response = S3RangeResponse { binding, ..valid };
            assert_eq!(validate(response, 17), Err(ObjectIdentity));
        }
        for content_length in [None, Some(16), Some(18)] {
            let response = S3RangeResponse {
                content_length,
                ..valid
            };
            assert_eq!(validate(response, 17), Err(Length));
        }
        let body = [0xa5_u8; 18];
        for body in [&body[..16], &body[..]] {
            assert_eq!(validate(valid, body.len() as u64), Err(Length));
        }
        let empty = geometry.range(1, 1).unwrap();
        let result = validate_s3_range_response(empty, &binding, valid, 17);
        assert_eq!(result, Err(NoCiphertext));
    }
}
