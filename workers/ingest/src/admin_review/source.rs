//! Unmounted selection of the current published source for a review job.

use nntune_core::s3_locator::{S3Binding, S3Locator};
use tokio_postgres::{types::Type, Client, IsolationLevel, Transaction};

use super::{
    load, not_found, versioned_object, Authorization, BoundaryError, ReviewJob, ReviewStatus,
    READ_SCOPE,
};
use crate::{
    api_error::ApiError, jobs::validate_job_id, multipart::session_outcome::uuid_text,
    queue::validate_mbid,
};

/// The job revision and canonical track selected by the operator. The serving
/// environment and library must come from trusted caller configuration.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Selection<'a> {
    pub job_id: &'a str,
    pub revision: u32,
    pub track_id: &'a str,
    pub environment: [u8; 16],
    pub library: [u8; 16],
}

/// An internal selection. Playback still requires its own authorization,
/// storage reference and protection for the exact version.
pub struct PublishedSource<'a> {
    pub selection: Selection<'a>,
    pub locator: S3Locator,
}

/// Use a bounded connection. The review reader must check current admin
/// authority and use the supplied read-only snapshot for the owner,
/// environment, library and job. Reloading the same selection checks the
/// current publication again before returning a locator.
pub async fn select<'a>(
    auth: &Authorization<'_>,
    client: &mut Client,
    selection: Selection<'a>,
    get: impl AsyncFn(
        &Transaction<'_>,
        &str,
        &Selection<'_>,
    ) -> Result<Option<ReviewJob>, BoundaryError>,
) -> Result<Option<PublishedSource<'a>>, ApiError> {
    let caller = auth.caller(READ_SCOPE)?;
    if !validate_job_id(selection.job_id)
        || selection.track_id.len() != 36
        || !validate_mbid(selection.track_id)
        || selection
            .track_id
            .bytes()
            .any(|byte| byte.is_ascii_uppercase())
    {
        return Err(not_found());
    }

    let tx = client
        .build_transaction()
        .read_only(true)
        .isolation_level(IsolationLevel::RepeatableRead)
        .start()
        .await
        .map_err(|_| BoundaryError::Unavailable)?;
    let read = async |actor: &str, _: &str| get(&tx, actor, &selection).await;
    let job = load(read, &caller, selection.job_id).await?;
    if job.view.status != ReviewStatus::Published || job.view.revision != selection.revision {
        return Ok(None);
    }
    let Ok(object) = versioned_object(&job) else {
        return Ok(None);
    };
    let locator = object.media.locator();
    let S3Binding::VersionId(version) = locator.binding() else {
        return Ok(None);
    };
    let bytes =
        i64::try_from(object.media.plaintext_len()).map_err(|_| BoundaryError::Unavailable)?;
    let current = tx
        .query_typed_opt(
            include_str!("source.sql"),
            &[
                (&selection.job_id, Type::TEXT),
                (&caller.user_id, Type::TEXT),
                (&selection.track_id, Type::TEXT),
                (&uuid_text(selection.environment), Type::TEXT),
                (&uuid_text(selection.library), Type::TEXT),
                (&locator.bucket(), Type::TEXT),
                (&locator.key(), Type::TEXT),
                (version, Type::TEXT),
                (&object.plaintext_sha256.as_slice(), Type::BYTEA),
                (&bytes, Type::INT8),
            ],
        )
        .await
        .map_err(|_| BoundaryError::Unavailable)?;
    tx.commit().await.map_err(|_| BoundaryError::Unavailable)?;
    Ok(current.map(|_| PublishedSource {
        selection,
        locator: locator.clone(),
    }))
}
