//! Exact version deletion for the dedicated staging fixture bucket.

use super::{
    encode_query_value, Digest, S3CompletionSender, Sha256, SignedReply, SignedRequest, SystemTime,
};
use crate::multipart::session_outcome::{valid_compact_uuid_v4 as uuid, valid_version_pin};

#[cfg(feature = "fixture-provider-preflight")]
#[path = "s3_fixture_preflight.rs"]
mod preflight;

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

    /// Check the runner's selected bucket before it commits the SQL fence.
    pub fn matches_bucket(&self, bucket: &str) -> bool {
        bucket == BUCKET
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
        use FixtureRetirementError::Unconfirmed;
        let deleted = self.send("DELETE", bucket, key, version, until).await?;
        if deleted.status != 204 || !deleted.body.is_empty() {
            return Err(Unconfirmed);
        }
        check_version(&deleted, version, false)?;
        let head = self.send("HEAD", bucket, key, version, until).await?;
        if head.status != 404 || !head.body.is_empty() {
            return Err(Unconfirmed);
        }
        check_version(&head, version, false)
    }

    async fn send(
        &self,
        method: &'static str,
        bucket: &str,
        key: &str,
        version: &str,
        until: SystemTime,
    ) -> Result<SignedReply, FixtureRetirementError> {
        use FixtureRetirementError::{Refused, Unconfirmed};
        if !self.matches_bucket(bucket) || !fixture_key(key) || !valid_version_pin(version) {
            return Err(Refused);
        }
        self.0
            .signed_request(
                SignedRequest {
                    configuration: self.0.configuration,
                    bucket,
                    key,
                    method,
                    query: format!("versionId={}", encode_query_value(version)),
                    body: &[],
                    payload_hash: hex::encode(Sha256::digest([])),
                    operation_headers: &[],
                },
                until,
            )
            .await
            .map_err(|_| Unconfirmed)
    }
}

fn check_version(
    reply: &SignedReply,
    version: &str,
    required: bool,
) -> Result<(), FixtureRetirementError> {
    use FixtureRetirementError::Unconfirmed;
    let observed = reply
        .headers
        .get("x-amz-version-id")
        .map_err(|_| Unconfirmed)?;
    match observed.as_deref() {
        Some(value) if value == version => Ok(()),
        None if !required => Ok(()),
        _ => Err(Unconfirmed),
    }
}

fn fixture_key(key: &str) -> bool {
    if key.len() > 128 {
        return false;
    }
    match key.split('/').collect::<Vec<_>>().as_slice() {
        ["media", object, generation] => uuid(object) && uuid(generation),
        ["media-digests", object, generation, map, "00000000"] => {
            uuid(object) && uuid(generation) && uuid(map)
        }
        _ => false,
    }
}
