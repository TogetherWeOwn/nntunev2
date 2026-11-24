use std::time::SystemTime;

use nntune_core::{
    digest_enrollment::StoredManifest,
    media_envelope::EnvelopeContext,
    s3_locator::{S3Binding, S3Locator, S3MediaMetadata},
};
use tokio_postgres::types::Type;

use super::{uuid_text, CommittedDigestRead};
use crate::{
    handlers::StoreError,
    postgres::{PgConnect, PgQuery},
    worker_entry::with_deadline,
};

/// The active playback root, supplied by independently configured custody.
pub struct PlaybackRoot<'a> {
    pub version: [u8; 16],
    pub secret: &'a [u8; 32],
}

pub struct ResolvedPlayback {
    pub read: CommittedDigestRead,
    pub media: S3MediaMetadata,
    pub manifest: StoredManifest,
    pub context: EnvelopeContext,
    pub record: [u8; 230],
    pub ordered_page_hashes: Vec<[u8; 32]>,
}

const CUSTODY_TIMEOUT_MS: u64 = 4_000;

/// The caller must obtain the locator from the same committed publication
/// and recheck its current authority before passing this bundle to Stream.
pub async fn resolve_custody(
    db: &mut impl PgConnect,
    read: CommittedDigestRead,
    locator: &S3Locator,
    root: &PlaybackRoot<'_>,
    now: impl Fn() -> SystemTime,
) -> Result<Option<ResolvedPlayback>, StoreError> {
    let operation = async {
        let pins = read.playback_snapshot()?;
        if now() >= pins.usable_until
            || root.version != pins.root_version_id
            || !matches!(locator.binding(), S3Binding::VersionId(version) if version == pins.media_version)
        {
            return Ok(None);
        }
        let tx = db.read_only_snapshot().await.map_err(|_| StoreError)?;
        tx.set_local_statement_timeout(CUSTODY_TIMEOUT_MS)
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
        let Some(manifest) =
            StoredManifest::from_committed_storage(&pins, context.header, hashes.clone())
        else {
            return Ok(None);
        };
        let Some(media) = S3MediaMetadata::from_trusted_context(locator.clone(), &context) else {
            return Ok(None);
        };
        if !manifest.authenticate(root.secret, &context, &record) {
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
            ordered_page_hashes: hashes,
        }))
    };
    with_deadline(operation, CUSTODY_TIMEOUT_MS, StoreError).await
}
