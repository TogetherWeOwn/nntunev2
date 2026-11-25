//! Unmounted transaction for one verified occurrence in the shared catalogue.
//! The S3 source table and the locator schema are currently rehearsal-only.

use nntune_core::s3_locator::{S3Binding, S3Locator};
use tokio_postgres::{types::ToSql, Transaction};

use crate::{
    digest_candidate_store::CandidateIntent,
    jobs::validate_job_id,
    track_writers::{validate_track_row, TrackRow},
};

pub(crate) const MAX_TRACK_TITLE_BYTES: usize = 4096;

/// Trusted handoff binding an already authorized, verified job to the
/// matched occurrence and the exact object that backed the match.
pub struct VerifiedOccurrence<'a> {
    pub job_id: &'a str,
    pub owner_id: &'a str,
    pub object_id: [u8; 16],
    pub track: TrackRow<'a>,
}

/// Exact committed publisher result. The store cross-checks its immutable
/// identities against the winner, locator and ordered receipt pins.
#[derive(Clone, Copy)]
pub struct PublishedObject<'a> {
    pub intent: &'a CandidateIntent,
    pub locator: &'a S3Locator,
    pub plaintext_sha256: &'a [u8; 32],
    pub root_version_id: [u8; 16],
    pub page_versions: &'a [&'a str],
}

#[derive(Debug, PartialEq, Eq)]
pub enum PublicationError {
    Refused,
    Indeterminate,
}

/// Stage a canonical track and its current S3 binding, returning the local
/// track UUID on first publication, replacement or exact replay. Use trusted
/// evidence in a bounded READ COMMITTED transaction for one occurrence.
/// Commit before using the ID and roll back on cancellation or uncertainty.
/// Live use also requires an authorized storage reference for retention.
pub async fn stage(
    tx: &mut Transaction<'_>,
    matched: &VerifiedOccurrence<'_>,
    result: &PublishedObject<'_>,
) -> Result<String, PublicationError> {
    stage_with_duration(tx, matched, result, None).await
}

/// When authenticated FLAC readback measured a duration, a fresh canonical row
/// stores it and an existing row must have the same measured value. A legacy
/// row with no duration requires separate reconciliation of the shared track.
pub(crate) async fn stage_with_duration(
    tx: &mut Transaction<'_>,
    matched: &VerifiedOccurrence<'_>,
    result: &PublishedObject<'_>,
    duration_ms: Option<i32>,
) -> Result<String, PublicationError> {
    use PublicationError::Refused;
    let track = &matched.track;
    let S3Binding::VersionId(version) = result.locator.binding() else {
        return Err(Refused);
    };
    let [environment, library, object, generation, configuration] = result.intent.identities;
    let bytes = i64::try_from(result.intent.plaintext_len).map_err(|_| Refused)?;
    if object != matched.object_id
        || !validate_job_id(matched.job_id)
        || !(1..=128).contains(&matched.owner_id.len())
        || matched.owner_id.contains('\0')
        || track.title.len() > MAX_TRACK_TITLE_BYTES
        || validate_track_row(track).is_err()
        || !(1..=512).contains(&result.page_versions.len())
        || result
            .page_versions
            .iter()
            .any(|pin| pin.len() > 1024 || pin.contains('\0'))
    {
        return Err(Refused);
    }

    let identity: [&(dyn ToSql + Sync); 5] = [
        &track.release_id,
        &track.medium_no,
        &track.track_no,
        &track.mbid,
        &duration_ms,
    ];
    let track_params: [&(dyn ToSql + Sync); 8] = [
        identity[0],
        identity[1],
        identity[2],
        identity[3],
        &track.title,
        &matched.job_id,
        &matched.owner_id,
        &duration_ms,
    ];
    let savepoint = tx.transaction().await?;
    let staged = async {
        let job = savepoint
            .query_opt(
                include_str!("catalogue_publication/track.sql"),
                &track_params,
            )
            .await?
            .ok_or(Refused)?;
        let seq: i64 = job.get(0);
        // A fresh snapshot sees the canonical row after a conflicting insert.
        let row = savepoint
            .query_opt(
                "SELECT id::text FROM public.tracks
             WHERE release_id = $1::text::uuid AND medium_no = $2 AND track_no = $3
               AND mbid = $4::text::uuid AND ($5::integer IS NULL OR duration_ms = $5)
             FOR NO KEY UPDATE",
                &identity,
            )
            .await?
            .ok_or(Refused)?;
        let track_id: String = row.get(0);
        let object = hex::encode(object);
        savepoint
            .query_opt(
                include_str!("catalogue_publication/source.sql"),
                &[
                    &object,
                    &hex::encode(environment),
                    &hex::encode(library),
                    &hex::encode(generation),
                    &hex::encode(configuration),
                    &bytes,
                    &result.plaintext_sha256.as_slice(),
                    &hex::encode(result.intent.map_id),
                    &hex::encode(result.root_version_id),
                    &result.locator.bucket(),
                    &result.locator.key(),
                    version,
                    &result.page_versions,
                    &result.intent.header.as_slice(),
                    &track_id,
                    &matched.job_id,
                    &seq,
                ],
            )
            .await?
            .ok_or(Refused)?;

        Ok(track_id)
    }
    .await;
    if staged.is_ok() {
        savepoint.commit().await?;
    } else {
        savepoint.rollback().await?;
    }
    staged
}

impl From<tokio_postgres::Error> for PublicationError {
    fn from(_: tokio_postgres::Error) -> Self {
        Self::Indeterminate
    }
}
