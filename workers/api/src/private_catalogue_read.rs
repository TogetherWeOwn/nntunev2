//! Unmounted PostgreSQL lookup for private music playback.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nntune_core::s3_locator::{valid_bucket, S3Binding, S3Locator};
use tokio_postgres::{types::Type, Row};

use crate::{
    handlers::{browser_playback_relay::BrowserReceipt, StoreError},
    postgres::{PgConnect, PgQuery},
    private_library::canonical_uuid,
    storage_digest_read::{uuid_text, DigestReadTarget, ReadUuid},
    worker_entry::query_playback_lease_row,
};

/// Independent pins for this exact committed receipt, supplied by the
/// trusted receipt store and provider configuration. The publication's
/// scoped storage reference uses the canonical track UUID as its ID.
pub struct PublicationPins<'a> {
    pub environment: [u8; 16],
    pub library: [u8; 16],
    pub accepted_seq: i64,
    pub provider_configuration: [u8; 16],
    pub bucket: &'a str,
    pub version: &'a str,
}

#[derive(PartialEq, Eq)]
pub struct ResolvedMusic {
    pub target: DigestReadTarget,
    pub locator: S3Locator,
    pub grant_deadline: SystemTime,
}

/// Call with a receipt and its pins from the authenticated private listening
/// flow and a bounded database connection. The lookup reads one committed
/// snapshot. Before any provider access, the caller still needs the committed
/// protected digest read for this target, matching custody and a live lease.
pub async fn resolve(
    client: &mut impl PgConnect,
    receipt: &BrowserReceipt,
    pins: &PublicationPins<'_>,
    grant_secret: &[u8],
    now: impl Fn() -> SystemTime,
) -> Result<Option<ResolvedMusic>, StoreError> {
    let expected_lease = &receipt.lease;
    let track = &expected_lease.track_id;
    let exp = receipt.grant.exp_unix;
    let now_secs = || now().duration_since(UNIX_EPOCH).map(|time| time.as_secs());
    if !now_secs().is_ok_and(|time| receipt.valid_private_grant(grant_secret, time)) {
        return Ok(None);
    }

    let tx = client.read_only_snapshot().await.map_err(|_| StoreError)?;
    let Some(lease) = query_playback_lease_row(&tx, &receipt.user_id).await? else {
        return Ok(None);
    };
    let lease_authorizes = || {
        lease.epoch == expected_lease.epoch
            && exp <= lease.expires_at.min(lease.continuation_deadline)
            && now_secs().is_ok_and(|time| {
                time < exp
                    && lease.is_continuation(
                        &expected_lease.session_id,
                        &expected_lease.play_id,
                        track,
                        time,
                    )
            })
    };
    if !lease_authorizes() {
        return Ok(None);
    }
    let Some((mut resolved, _)) = lookup(&tx, track, pins, Some(&receipt.artifact_sha256)).await?
    else {
        return Ok(None);
    };
    resolved.grant_deadline = UNIX_EPOCH
        .checked_add(Duration::from_secs(exp))
        .ok_or(StoreError)?;
    tx.commit().await.map_err(|_| StoreError)?;
    Ok(lease_authorizes().then_some(resolved))
}

#[derive(Clone, Copy)]
pub(crate) struct PublicationScope<'a> {
    pub environment: [u8; 16],
    pub library: [u8; 16],
    pub provider_configuration: [u8; 16],
    pub bucket: &'a str,
}

impl<'a> From<&PublicationPins<'a>> for PublicationScope<'a> {
    fn from(pins: &PublicationPins<'a>) -> Self {
        Self {
            environment: pins.environment,
            library: pins.library,
            provider_configuration: pins.provider_configuration,
            bucket: pins.bucket,
        }
    }
}

pub(crate) async fn lookup(
    db: &impl PgQuery,
    track: &str,
    pins: &PublicationPins<'_>,
    digest: Option<&str>,
) -> Result<Option<(ResolvedMusic, Row)>, StoreError> {
    lookup_scoped(
        db,
        track,
        pins.into(),
        Some((pins.accepted_seq, pins.version)),
        digest,
    )
    .await
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version != "null"
        && version.len() <= 1024
        && version.bytes().all(|b| b.is_ascii_graphic() && b != b',')
}

fn valid_lookup(track: &str, bucket: &str, pin: Option<(i64, &str)>, digest: Option<&str>) -> bool {
    canonical_uuid(track)
        && valid_bucket(bucket)
        && pin.is_none_or(|(seq, version)| seq > 0 && valid_version(version))
        && digest.is_none_or(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub(crate) async fn lookup_scoped(
    db: &impl PgQuery,
    track: &str,
    scope: PublicationScope<'_>,
    pin: Option<(i64, &str)>,
    digest: Option<&str>,
) -> Result<Option<(ResolvedMusic, Row)>, StoreError> {
    if !valid_lookup(track, scope.bucket, pin, digest) {
        return Ok(None);
    }
    let (seq, version) = pin.unzip();
    let row = db
        .query_typed_opt(
            include_str!("private_catalogue_read.sql"),
            &[
                (&track, Type::TEXT),
                (&uuid_text(scope.environment), Type::TEXT),
                (&uuid_text(scope.library), Type::TEXT),
                (&seq, Type::INT8),
                (&uuid_text(scope.provider_configuration), Type::TEXT),
                (&scope.bucket, Type::TEXT),
                (&version, Type::TEXT),
                (&digest, Type::TEXT),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id = |column| {
        row.try_get::<_, ReadUuid>(column)
            .map(|uuid| uuid.0)
            .map_err(|_| StoreError)
    };
    let version: &str = row.try_get("s3_version_id").map_err(|_| StoreError)?;
    if !valid_version(version) {
        return Ok(None);
    }
    let locator = S3Locator::new(
        scope.bucket.into(),
        row.try_get("s3_key").map_err(|_| StoreError)?,
        S3Binding::VersionId(version.into()),
    )
    .map_err(|_| StoreError)?;
    let digest: &[u8] = row.try_get("plaintext_sha256").map_err(|_| StoreError)?;
    let target = DigestReadTarget {
        environment: scope.environment,
        library: scope.library,
        reference: id("track_id")?,
        object: id("object_id")?,
        generation: id("generation_id")?,
        provider_configuration: scope.provider_configuration,
        root_version: id("root_version_id")?,
        plaintext_sha256: digest.try_into().map_err(|_| StoreError)?,
        plaintext_bytes: row.try_get("plaintext_bytes").map_err(|_| StoreError)?,
    };
    Ok(Some((
        ResolvedMusic {
            target,
            locator,
            grant_deadline: UNIX_EPOCH,
        },
        row,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_lookup_rejects_malformed_query_parameters() {
        let track = "a0000000-0000-4000-8000-000000000001";
        let bucket = "test-bucket";
        let digest = "ab".repeat(32);
        let pin = Some((1, "v+/="));
        assert!(valid_lookup(track, bucket, pin, Some(&digest)));
        assert!(!valid_lookup(track, "invalid/bucket", pin, Some(&digest)));
        for pin in [(0, "v1"), (-1, "v1"), (1, "null"), (1, "v1,v2")] {
            assert!(!valid_lookup(track, bucket, Some(pin), None));
        }
        for digest in [&digest[..63], &"x".repeat(64), "00\0"] {
            assert!(!valid_lookup(track, bucket, None, Some(digest)));
        }
    }
}

#[cfg(all(test, feature = "postgres-tests", not(target_arch = "wasm32")))]
#[path = "private_catalogue_read_postgres.rs"]
mod postgres_tests;
