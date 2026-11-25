//! Exact version deletion for the dedicated staging fixture bucket.

use super::{encode_query_value, Digest, S3CompletionSender, Sha256, SignedRequest, SystemTime};

const BUCKET: &str = "nntunev2-private-fixtures-staging";

#[derive(Debug, PartialEq, Eq)]
pub enum FixtureRetirementError {
    Refused,
    Unconfirmed,
}

/// Construct only from the dedicated staging cleanup credential. The caller
/// establishes its committed SQL retirement fence before passing version pins.
pub struct FixtureVersionRemover(S3CompletionSender);

impl FixtureVersionRemover {
    pub fn new(
        configuration: [u8; 16],
        endpoint: &str,
        region: String,
        access_key: String,
        secret_key: String,
    ) -> Result<Self, FixtureRetirementError> {
        S3CompletionSender::new(
            configuration,
            BUCKET.into(),
            endpoint,
            region,
            access_key,
            secret_key,
        )
        .map(Self)
        .map_err(|_| FixtureRetirementError::Refused)
    }

    /// Delete and confirm absence of the same version. Repeating an exact
    /// delete is safe after an interruption.
    pub async fn delete_and_confirm(
        &self,
        bucket: &str,
        key: &str,
        version: &str,
        until: SystemTime,
    ) -> Result<(), FixtureRetirementError> {
        use FixtureRetirementError::{Refused, Unconfirmed};
        if bucket != BUCKET || !fixture_key(key) || !super::readback::valid_version(version) {
            return Err(Refused);
        }
        let query = format!("versionId={}", encode_query_value(version));
        let hash = hex::encode(Sha256::digest([]));
        let send = async |method| {
            self.0
                .signed_request(
                    SignedRequest {
                        configuration: self.0.configuration,
                        bucket,
                        key,
                        method,
                        query: query.clone(),
                        body: &[],
                        payload_hash: hash.clone(),
                        operation_headers: &[],
                    },
                    until,
                )
                .await
                .map_err(|_| Unconfirmed)
        };
        let deleted = send("DELETE").await?;
        let observed = deleted
            .headers
            .get("x-amz-version-id")
            .map_err(|_| Unconfirmed)?;
        if deleted.status != 204
            || !deleted.body.is_empty()
            || observed.as_deref().is_some_and(|value| value != version)
        {
            return Err(Unconfirmed);
        }
        let head = send("HEAD").await?;
        if head.status != 404 || !head.body.is_empty() {
            return Err(Unconfirmed);
        }
        Ok(())
    }
}

fn fixture_key(key: &str) -> bool {
    if key.len() > 128 {
        return false;
    }
    let uuid = |id: &str| {
        let b = id.as_bytes();
        b.len() == 32
            && b[12] == b'4'
            && matches!(b[16], b'8'..=b'b')
            && b.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    match key.split('/').collect::<Vec<_>>().as_slice() {
        ["media", object, generation] => uuid(object) && uuid(generation),
        ["media-digests", object, generation, map, "00000000"] => {
            uuid(object) && uuid(generation) && uuid(map)
        }
        _ => false,
    }
}
