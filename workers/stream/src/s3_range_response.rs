//! Offline metadata check for a planned S3 ciphertext range. No transport or reader wiring.

use nntune_core::{digest_geometry::RangeGeometry, s3_locator::S3Binding};

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
    use S3RangeResponseError::*;

    let span = plan.ciphertext_span().ok_or(NoCiphertext)?;
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
    if response.content_length != Some(expected_len) || body_bytes != expected_len {
        return Err(Length);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::plan_encrypted_range;
    use nntune_core::digest_geometry::DigestGeometry;

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
