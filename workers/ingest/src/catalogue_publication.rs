//! Unmounted transaction for one verified occurrence in the shared catalogue.
//! The S3 source table and the locator schema are currently rehearsal-only.

use nntune_core::{
    digest_geometry::DigestGeometry,
    s3_locator::{S3Binding, S3Locator},
};
use tokio_postgres::{
    types::{ToSql, Type},
    Transaction,
};

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

impl PublishedObject<'_> {
    /// Validate receipt shape before SQL checks the durable pins and custody.
    pub fn version_pin(&self) -> Result<&str, PublicationError> {
        let S3Binding::VersionId(version) = self.locator.binding() else {
            return Err(PublicationError::Refused);
        };
        let length = self.intent.plaintext_len;
        let geometry = DigestGeometry::new(length).ok_or(PublicationError::Refused)?;
        if length == 0
            || self.page_versions.len() != geometry.pages() as usize
            || !valid_version_pin(version)
            || self.page_versions.iter().any(|pin| !valid_version_pin(pin))
        {
            return Err(PublicationError::Refused);
        }
        Ok(version)
    }
}

fn valid_version_pin(pin: &str) -> bool {
    pin != "null"
        && (1..=1024).contains(&pin.len())
        && pin
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b',')
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
/// Refuse a measured duration that does not fit the catalogue column.
pub(crate) async fn stage_with_duration(
    tx: &mut Transaction<'_>,
    matched: &VerifiedOccurrence<'_>,
    result: &PublishedObject<'_>,
    duration_ms: Option<u64>,
) -> Result<String, PublicationError> {
    use PublicationError::Refused;
    let duration_ms = duration_ms
        .map(i32::try_from)
        .transpose()
        .map_err(|_| Refused)?;
    let track = &matched.track;
    let version = result.version_pin()?;
    let [environment, library, object, generation, configuration] = result.intent.identities;
    let bytes = i64::try_from(result.intent.plaintext_len).map_err(|_| Refused)?;
    if object != matched.object_id
        || !validate_job_id(matched.job_id)
        || !(1..=128).contains(&matched.owner_id.len())
        || matched.owner_id.contains('\0')
        || track.title.len() > MAX_TRACK_TITLE_BYTES
        || validate_track_row(track).is_err()
    {
        return Err(Refused);
    }

    let identity: [(&(dyn ToSql + Sync), Type); 5] = [
        (&track.release_id, Type::TEXT),
        (&track.medium_no, Type::INT4),
        (&track.track_no, Type::INT4),
        (&track.mbid, Type::TEXT),
        (&duration_ms, Type::INT4),
    ];
    let track_params = [
        identity[0].clone(),
        identity[1].clone(),
        identity[2].clone(),
        identity[3].clone(),
        (&track.title, Type::TEXT),
        (&matched.job_id, Type::TEXT),
        (&matched.owner_id, Type::TEXT),
        identity[4].clone(),
    ];
    let savepoint = tx.transaction().await?;
    let staged = async {
        let job = savepoint
            .query_typed_opt(
                include_str!("catalogue_publication/track.sql"),
                &track_params,
            )
            .await?
            .ok_or(Refused)?;
        let seq: i64 = job.get(0);
        // A fresh snapshot sees the canonical row after a conflicting insert.
        let row = savepoint
            .query_typed_opt(
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
            .query_typed_opt(
                include_str!("catalogue_publication/source.sql"),
                &[
                    (&object, Type::TEXT),
                    (&hex::encode(environment), Type::TEXT),
                    (&hex::encode(library), Type::TEXT),
                    (&hex::encode(generation), Type::TEXT),
                    (&hex::encode(configuration), Type::TEXT),
                    (&bytes, Type::INT8),
                    (&result.plaintext_sha256.as_slice(), Type::BYTEA),
                    (&hex::encode(result.intent.map_id), Type::TEXT),
                    (&hex::encode(result.root_version_id), Type::TEXT),
                    (&result.locator.bucket(), Type::TEXT),
                    (&result.locator.key(), Type::TEXT),
                    (&version, Type::TEXT),
                    (&result.page_versions, Type::TEXT_ARRAY),
                    (&result.intent.header.as_slice(), Type::BYTEA),
                    (&track_id, Type::TEXT),
                    (&matched.job_id, Type::TEXT),
                    (&seq, Type::INT8),
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
