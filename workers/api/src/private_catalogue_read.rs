//! Unmounted PostgreSQL lookup for private music playback.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nntune_core::s3_locator::{S3Binding, S3Locator};
use tokio_postgres::{types::Type, Row};

use crate::{
    handlers::{browser_playback_relay::BrowserReceipt, StoreError},
    meter_sink::is_uuid_shaped,
    postgres::{PgConnect, PgQuery},
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

pub(crate) async fn lookup(
    db: &impl PgQuery,
    track: &str,
    pins: &PublicationPins<'_>,
    digest: Option<&str>,
) -> Result<Option<(ResolvedMusic, Row)>, StoreError> {
    let version = pins.version.as_bytes();
    if !is_uuid_shaped(track)
        || track.bytes().any(|b| b.is_ascii_uppercase())
        || pins.bucket.len() > 63
        || version.len() > 1024
        || version.contains(&b',')
        || version.contains(&0)
        || pins.bucket.contains('\0')
        || pins.accepted_seq <= 0
    {
        return Ok(None);
    }
    let row = db
        .query_typed_opt(
            include_str!("private_catalogue_read.sql"),
            &[
                (&track, Type::TEXT),
                (&uuid_text(pins.environment), Type::TEXT),
                (&uuid_text(pins.library), Type::TEXT),
                (&pins.accepted_seq, Type::INT8),
                (&uuid_text(pins.provider_configuration), Type::TEXT),
                (&pins.bucket, Type::TEXT),
                (&pins.version, Type::TEXT),
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
    let locator = S3Locator::new(
        pins.bucket.into(),
        row.try_get("s3_key").map_err(|_| StoreError)?,
        S3Binding::VersionId(pins.version.into()),
    )
    .map_err(|_| StoreError)?;
    let digest: &[u8] = row.try_get("plaintext_sha256").map_err(|_| StoreError)?;
    let target = DigestReadTarget {
        environment: pins.environment,
        library: pins.library,
        reference: id("track_id")?,
        object: id("object_id")?,
        generation: id("generation_id")?,
        provider_configuration: pins.provider_configuration,
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

#[cfg(all(test, feature = "postgres-tests", not(target_arch = "wasm32")))]
#[path = "private_catalogue_read_postgres.rs"]
mod postgres_tests;
