//! S3 ciphertext range validation and bounded body reading. No transport wiring.

use nntune_core::{
    digest_geometry::RangeGeometry, digest_readback::ReadbackBody, s3_locator::S3Binding,
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

/// Read one planned sealed chunk, never the full covered range. `is_live`
/// checks the committed read deadline; the transport must also cancel any
/// pending read at that deadline. Success still requires digest/AEAD verification.
pub async fn read_s3_range_chunk(
    plan: RangeGeometry,
    index: u32,
    expected_binding: &S3Binding,
    response: S3RangeResponse<'_>,
    mut body: impl ReadbackBody,
    mut is_live: impl FnMut() -> bool,
) -> Result<Vec<u8>, S3RangeResponseError> {
    use S3RangeResponseError::*;

    let chunk = plan.sealed_chunk(index).ok_or(NoCiphertext)?;
    let len = validate_s3_range_metadata(plan, chunk.ciphertext, expected_binding, response)?;
    let mut bytes = vec![0; len as usize];
    let mut remaining = bytes.as_mut_slice();
    loop {
        if !is_live() {
            return Err(Expired);
        }
        if remaining.is_empty() {
            if body.read(&mut [0]).await.map_err(|_| Body)? != 0 {
                return Err(Length);
            }
            return is_live().then_some(bytes).ok_or(Expired);
        }
        let count = body.read(remaining).await.map_err(|_| Body)?;
        if count == 0 {
            return Err(Length);
        }
        remaining = remaining.get_mut(count..).ok_or(Body)?;
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
    use nntune_core::digest_geometry::DigestGeometry;

    struct SliceBody<'a>(&'a [u8]);

    impl ReadbackBody for SliceBody<'_> {
        async fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
            std::io::Read::read(&mut self.0, dst)
        }
    }

    #[tokio::test]
    async fn reads_only_a_complete_chunk_before_expiry() {
        use S3RangeResponseError::*;

        let plan = DigestGeometry::new(65_537)
            .unwrap()
            .range(0, 65_537)
            .unwrap();
        let binding = S3Binding::VersionId("v1".into());
        let response = S3RangeResponse {
            status: 206,
            content_range: Some("bytes 65600-65616/65617"),
            content_length: Some(17),
            binding: Some(&binding),
        };
        let data = [0xa5; 18];
        for (len, expected) in [
            (16, Err(Length)),
            (17, Ok(data[..17].to_vec())),
            (18, Err(Length)),
        ] {
            let actual =
                read_s3_range_chunk(plan, 1, &binding, response, SliceBody(&data[..len]), || {
                    true
                })
                .await;
            assert_eq!(actual, expected);
        }
        let mut checks = 0;
        let expired =
            read_s3_range_chunk(plan, 1, &binding, response, SliceBody(&data[..17]), || {
                checks += 1;
                checks < 3 // Expire just as the EOF probe completes.
            })
            .await;
        assert_eq!(expired, Err(Expired));
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
