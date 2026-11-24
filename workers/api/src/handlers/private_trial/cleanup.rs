//! Retire only a completed trial fixture and return its immutable delete pins.

use nntune_core::s3_locator::{S3Binding, S3Locator};
use nntune_ingest::multipart::session_upload::begin;
use tokio_postgres::{types::Type, Client};

use super::{caller::FIXTURE_DIGEST, TrialError};
use crate::storage_digest_read::uuid_text;

pub struct Fixture<'a> {
    pub environment: [u8; 16],
    pub library: [u8; 16],
    pub object: [u8; 16],
    pub track: &'a str,
    pub session: &'a str,
    pub job: &'a str,
    pub issuer: &'a str,
    pub invite: i64,
    pub bucket: &'a str,
    /// The staged cohort's configured access cutoff in Unix seconds.
    pub cutoff: i64,
}

struct RetiredFixture {
    media: S3Locator,
    page_key: String,
    page_version: String,
}

/// Invoke from a protected staging cleanup job with its selected fixture. The
/// transport must delete the indicated version and confirm its absence. A retry
/// can resume after the first version disappears or a commit is unconfirmed.
pub async fn cleanup(
    db: &mut Client,
    fixture: &Fixture<'_>,
    mut delete_and_confirm: impl AsyncFnMut(&str, &str, &str) -> Result<(), TrialError>,
) -> Result<bool, TrialError> {
    let Some(pins) = retire(db, fixture).await? else {
        return Ok(false);
    };
    let S3Binding::VersionId(media_version) = pins.media.binding() else {
        return Err(TrialError::Unconfirmed);
    };
    delete_and_confirm(pins.media.bucket(), pins.media.key(), media_version).await?;
    delete_and_confirm(pins.media.bucket(), &pins.page_key, &pins.page_version).await?;
    let tx = begin(db).await.map_err(|_| TrialError::Unconfirmed)?;
    let removed = tx.execute_typed(
        "DELETE FROM public.catalogue_s3_sources s USING public.storage_validated_digest_winners w
         WHERE (s.track_id, s.object_id, s.source_job_id) = ($1::text::uuid, $2::text::uuid, $3)
         AND (w.object_id, w.environment_id, w.library_id) = (s.object_id, $4::text::uuid, $5::text::uuid)
         AND w.deleting_at IS NOT NULL",
        &[(&fixture.track, Type::TEXT), (&uuid_text(fixture.object), Type::TEXT), (&fixture.job, Type::TEXT),
          (&uuid_text(fixture.environment), Type::TEXT), (&uuid_text(fixture.library), Type::TEXT)],
    ).await.map_err(|_| TrialError::Unconfirmed)?;
    tx.commit().await.map_err(|_| TrialError::Unconfirmed)?;
    Ok(removed == 1)
}

async fn retire(
    db: &mut Client,
    fixture: &Fixture<'_>,
) -> Result<Option<RetiredFixture>, TrialError> {
    let env = uuid_text(fixture.environment);
    let lib = uuid_text(fixture.library);
    let obj = uuid_text(fixture.object);
    let tx = begin(db).await.map_err(|_| TrialError::Unconfirmed)?;
    let Some(row) = tx
        .query_typed_opt(
            include_str!("cleanup.sql"),
            &[
                (&env, Type::TEXT),
                (&lib, Type::TEXT),
                (&obj, Type::TEXT),
                (&fixture.track, Type::TEXT),
                (&fixture.session, Type::TEXT),
                (&fixture.job, Type::TEXT),
                (&fixture.issuer, Type::TEXT),
                (&fixture.invite, Type::INT8),
                (&fixture.bucket, Type::TEXT),
                (&fixture.cutoff, Type::INT8),
                (&FIXTURE_DIGEST.as_slice(), Type::BYTEA),
            ],
        )
        .await
        .map_err(|_| TrialError::Unconfirmed)?
    else {
        return Ok(None);
    };
    let field = |column| {
        row.try_get::<_, String>(column)
            .map_err(|_| TrialError::Unconfirmed)
    };
    let generation = field("generation")?;
    let map = field("map")?;
    let root = field("root")?;
    let media = S3Locator::new(
        fixture.bucket.into(),
        format!("media/{obj}/{generation}"),
        S3Binding::VersionId(field("media_version")?),
    )
    .map_err(|_| TrialError::Unconfirmed)?;
    let page_version = field("page_version")?;
    // Apply the same opaque token validation as for a media version.
    S3Locator::new(
        fixture.bucket.into(),
        media.key().into(),
        S3Binding::VersionId(page_version.clone()),
    )
    .map_err(|_| TrialError::Unconfirmed)?;
    let retired = row
        .try_get::<_, bool>("retired")
        .map_err(|_| TrialError::Unconfirmed)?;
    // Take a fresh snapshot after the source lock. The admission writer can
    // finish while this cleanup waits for it, including at the cutoff.
    let no_other_links = tx.query_typed_one(
        "SELECT NOT EXISTS (
             SELECT FROM public.catalogue_s3_sources s WHERE s.object_id = $2::text::uuid AND s.track_id <> $1::text::uuid
         ) AND NOT EXISTS (
             SELECT FROM public.private_library_admissions a
             WHERE (a.track_id = $1::text::uuid OR a.object_id = $2::text::uuid)
               AND ($4 OR (a.track_id, a.object_id, a.invite_id) <> ($1::text::uuid, $2::text::uuid, $3))
         )",
        &[(&fixture.track, Type::TEXT), (&obj, Type::TEXT), (&fixture.invite, Type::INT8), (&retired, Type::BOOL)],
    ).await.map_err(|_| TrialError::Unconfirmed)?.try_get::<_, bool>(0)
        .map_err(|_| TrialError::Unconfirmed)?;
    if !no_other_links {
        return Ok(None);
    }
    if !retired {
        let track = fixture.track;
        let deleted = tx
            .query_typed_one(
                "WITH admission AS (
                DELETE FROM public.private_library_admissions
                WHERE invite_id = $1 AND track_id = $2::text::uuid AND object_id = $3::text::uuid
                RETURNING 1
             ), reference AS (
                DELETE FROM public.storage_validated_digest_references
                WHERE (environment_id, library_id, reference_id, object_id)
                    = ($4::text::uuid, $5::text::uuid, $2::text::uuid, $3::text::uuid)
                RETURNING 1
             )
             SELECT (SELECT count(*) FROM admission) = 1
                AND (SELECT count(*) FROM reference) = 1",
                &[
                    (&fixture.invite, Type::INT8),
                    (&track, Type::TEXT),
                    (&obj, Type::TEXT),
                    (&env, Type::TEXT),
                    (&lib, Type::TEXT),
                ],
            )
            .await
            .map_err(|_| TrialError::Unconfirmed)?
            .try_get::<_, bool>(0)
            .map_err(|_| TrialError::Unconfirmed)?;
        let length = super::FIXTURE_BYTES as i64;
        if !deleted || !tx.query_typed_one(
            "SELECT public.admit_storage_digest_delete($1::text::uuid, $2::text::uuid, $3, $4,
                    $5::text::uuid, $6::text::uuid, $7::text::uuid)",
            &[(&env, Type::TEXT), (&lib, Type::TEXT), (&FIXTURE_DIGEST.as_slice(), Type::BYTEA),
              (&length, Type::INT8), (&obj, Type::TEXT), (&generation, Type::TEXT), (&root, Type::TEXT)],
        ).await.map_err(|_| TrialError::Unconfirmed)?.try_get::<_, bool>(0)
            .map_err(|_| TrialError::Unconfirmed)? {
            return Ok(None);
        }
    }
    tx.commit().await.map_err(|_| TrialError::Unconfirmed)?;
    Ok(Some(RetiredFixture {
        media,
        page_key: format!("media-digests/{obj}/{generation}/{map}/00000000"),
        page_version,
    }))
}
