//! Resolve authenticated playback custody from an exact protected read.

use std::time::SystemTime;

use nntune_core::{
    digest_enrollment::OriginManifest,
    media_envelope::{verify_enrolled_scheme2, EnvelopeContext},
    s3_locator::{S3Binding, S3Locator, S3MediaMetadata},
};
use tokio_postgres::types::Type;

use super::{uuid_text, CommittedDigestRead};
use crate::{
    handlers::StoreError,
    postgres::{PgConnect, PgQuery},
    worker_entry::with_deadline,
};

/// The independently configured active playback root, selected by purpose
/// and version before invocation. The secret stays with the caller.
pub struct PlaybackRoot<'a> {
    pub version: [u8; 16],
    pub secret: &'a [u8; 32],
}

pub struct ResolvedPlayback {
    pub read: CommittedDigestRead,
    pub media: S3MediaMetadata,
    pub manifest: OriginManifest,
    pub context: EnvelopeContext,
    pub record: [u8; 230],
}

/// The caller must obtain the locator from the same committed publication
/// and recheck its current authority before passing this bundle to Stream.
pub async fn resolve_custody(
    db: &mut impl PgConnect,
    read: CommittedDigestRead,
    locator: &S3Locator,
    root: &PlaybackRoot<'_>,
    now: impl Fn() -> SystemTime,
) -> Result<Option<ResolvedPlayback>, StoreError> {
    with_deadline(
        read_custody(db, read, locator, root, now),
        4_000,
        StoreError,
    )
    .await
}

async fn read_custody(
    db: &mut impl PgConnect,
    read: CommittedDigestRead,
    locator: &S3Locator,
    root: &PlaybackRoot<'_>,
    now: impl Fn() -> SystemTime,
) -> Result<Option<ResolvedPlayback>, StoreError> {
    let pins = read.playback_snapshot()?;
    if now() >= pins.usable_until
        || root.version != pins.root_version_id
        || !matches!(locator.binding(), S3Binding::VersionId(version) if version == pins.media_version)
    {
        return Ok(None);
    }
    let tx = db.read_only_snapshot().await.map_err(|_| StoreError)?;
    tx.set_local_statement_timeout(4_000)
        .await
        .map_err(|_| StoreError)?;
    let Some(row) = tx
        .query_typed_opt(
            include_str!("custody.sql"),
            &[
                (&uuid_text(pins.object_id), Type::TEXT),
                (&uuid_text(pins.root_version_id), Type::TEXT),
                (&uuid_text(pins.map_id), Type::TEXT),
                (&locator.bucket(), Type::TEXT),
                (&locator.key(), Type::TEXT),
                (&pins.media_version, Type::TEXT),
            ],
        )
        .await
        .map_err(|_| StoreError)?
    else {
        return Ok(None);
    };
    let bytes = |key| row.try_get::<_, &[u8]>(key).map_err(|_| StoreError);
    let context = EnvelopeContext {
        identities: [
            root.version,
            pins.environment_id,
            pins.library_id,
            pins.object_id,
            pins.generation_id,
            pins.provider_configuration,
        ],
        plaintext_len: pins.plaintext_len,
        header: bytes("header_tag")?.try_into().map_err(|_| StoreError)?,
    };
    let record = bytes("wrapped_record")?
        .try_into()
        .map_err(|_| StoreError)?;
    let hashes = row
        .try_get::<_, Vec<&[u8]>>("hashes")
        .map_err(|_| StoreError)?
        .into_iter()
        .map(|hash| hash.try_into().map_err(|_| StoreError))
        .collect::<Result<Vec<_>, _>>()?;
    let Some(manifest) = OriginManifest::from_committed_storage(&pins, context.header, hashes)
    else {
        return Ok(None);
    };
    let Some(media) = S3MediaMetadata::from_trusted_context(locator.clone(), &context) else {
        return Ok(None);
    };
    // There is no provider-supplied envelope. The retained record is selected
    // directly from protected storage and authenticated against the live root.
    if verify_enrolled_scheme2(root.secret, &context, &record, &record, &manifest).is_err() {
        return Ok(None);
    }
    let until = pins.usable_until;
    tx.commit().await.map_err(|_| StoreError)?;
    Ok((now() < until).then_some(ResolvedPlayback {
        read,
        media,
        manifest,
        context,
        record,
    }))
}
