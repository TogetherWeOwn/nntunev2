//! Unmounted review operations for owner-scoped, app-proxied FLAC jobs.

use nntune_core::s3_locator::{S3Binding, S3MediaMetadata};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::{
    api_error::ApiError,
    caller_auth::{authenticate_scoped, owns_job, Caller},
    jobs::validate_job_id,
    matching::admin_publish_match::NeedsReview,
    multipart::session_outcome::valid_version_pin,
};

pub const READ_SCOPE: &str = "ingest:review:read";
pub const RETRY_SCOPE: &str = "ingest:review:retry";
pub const RETRY_BODY_LIMIT: usize = 128;

pub mod completed;
pub mod source;

/// Synthetic grants use the existing caller-grant format. A future app proxy
/// must check current admin status and retry step-up before issuing grants.
pub struct Authorization<'a> {
    pub bearer: Option<&'a str>,
    pub secret: Option<&'a str>,
    pub now: u64,
}

impl Authorization<'_> {
    fn caller(&self, scope: &str) -> Result<Caller, ApiError> {
        let bearer = self.bearer.filter(|value| value.len() <= 512);
        authenticate_scoped(bearer, self.secret, self.now, scope).map_err(|_| not_found())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Accepted,
    Failed,
    NeedsReview,
    Published,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueRequest {
    pub status: Option<ReviewStatus>,
    pub limit: Option<u16>,
    pub offset: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryRequest {
    pub revision: u32,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct JobView {
    pub job_id: String,
    pub revision: u32,
    pub status: ReviewStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<NeedsReview>,
}

#[derive(Debug, Serialize)]
pub struct QueueView {
    pub jobs: Vec<JobView>,
    pub limit: u16,
    pub offset: u32,
}

/// Private reference binding the versioned locator and immutable envelope context.
#[derive(Clone, PartialEq, Eq)]
pub struct ObjectRef {
    pub media: S3MediaMetadata,
    pub plaintext_sha256: [u8; 32],
}

pub struct ReviewJob {
    /// Advance the revision on every state or object change. Never reuse it.
    pub view: JobView,
    pub owner_id: String,
    pub object: Option<ObjectRef>,
    /// Input revision of the last committed retry, if it produced this row.
    pub retried_revision: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BoundaryError {
    MissingOrFenced,
    Conflict,
    Unavailable,
}

/// `list` scopes to the owner, serving environment and library. Apply the
/// status filter before paging newest first with a stable tie break.
pub async fn queue(
    auth: &Authorization<'_>,
    list: impl AsyncFn(&str, Option<ReviewStatus>, u16, u32) -> Result<Vec<ReviewJob>, BoundaryError>,
    request: QueueRequest,
) -> Result<QueueView, ApiError> {
    let caller = auth.caller(READ_SCOPE)?;
    let limit = request.limit.unwrap_or(50).min(100);
    let limit = if limit == 0 { 50 } else { limit };
    let offset = request.offset.unwrap_or(0);
    let jobs = list(&caller.user_id, request.status, limit, offset).await?;
    let mut seen = HashSet::with_capacity(usize::from(limit));
    if jobs.len() > usize::from(limit)
        || jobs.iter().any(|job| {
            !owns_job(&caller, &job.owner_id)
                || !validate_job_id(&job.view.job_id)
                || request
                    .status
                    .is_some_and(|status| status != job.view.status)
                || !seen.insert(job.view.job_id.as_str())
        })
    {
        return Err(BoundaryError::Unavailable.into());
    }
    Ok(QueueView {
        jobs: jobs.into_iter().map(|job| job.view).collect(),
        limit,
        offset,
    })
}

/// `get` loads a FLAC review job in the owner, environment and library scope.
pub async fn detail(
    auth: &Authorization<'_>,
    get: impl AsyncFn(&str, &str) -> Result<Option<ReviewJob>, BoundaryError>,
    job_id: &str,
) -> Result<JobView, ApiError> {
    let caller = auth.caller(READ_SCOPE)?;
    Ok(load(get, &caller, job_id).await?.view)
}

/// `get` and `verify` use the scope specified by [`detail`]. Verify the
/// exact version and return its readback reference, or Conflict if gone.
///
/// `publish` matches pinned input and rechecks admin authority, account fence,
/// owner, revision, state and object at commit. Commit publication, audit and
/// outcome at revision + 1 atomically. Return the committed row for concurrent
/// repeats, Conflict for stale input, MissingOrFenced for lost authority, or
/// Unavailable on uncertainty so the next retry can reconcile.
pub async fn retry(
    auth: &Authorization<'_>,
    get: impl AsyncFn(&str, &str) -> Result<Option<ReviewJob>, BoundaryError>,
    verify: impl AsyncFn(&str, &ObjectRef) -> Result<ObjectRef, BoundaryError>,
    publish: impl AsyncFn(&ReviewJob) -> Result<ReviewJob, BoundaryError>,
    job_id: &str,
    body: &[u8],
) -> Result<JobView, ApiError> {
    let (caller, request) = retry_input(auth, body)?;
    let job = load(get, &caller, job_id).await?;
    let object = versioned_object(&job)?;
    if is_retry_result(&job, request.revision) {
        return Ok(job.view);
    }
    next_revision(&job, request.revision)?;
    if verify(&caller.user_id, object).await? != *object {
        return Err(BoundaryError::Conflict.into());
    }
    let committed = publish(&job).await?;
    if !owns_job(&caller, &committed.owner_id) || committed.view.job_id != job_id {
        return Err(not_found());
    }
    if !is_retry_result(&committed, request.revision) || committed.object.as_ref() != Some(object) {
        return Err(BoundaryError::Unavailable.into());
    }
    Ok(committed.view)
}

fn retry_input(auth: &Authorization<'_>, body: &[u8]) -> Result<(Caller, RetryRequest), ApiError> {
    let caller = auth.caller(RETRY_SCOPE)?;
    if body.len() > RETRY_BODY_LIMIT {
        return Err(ApiError::by_code("payload_too_large", "body too large"));
    }
    if body.trim_ascii_start().first() != Some(&b'{') {
        return Err(ApiError::invalid_request("invalid retry request"));
    }
    let request: RetryRequest = serde_json::from_slice(body)
        .map_err(|_| ApiError::invalid_request("invalid retry request"))?;
    Ok((caller, request))
}

fn versioned_object(job: &ReviewJob) -> Result<&ObjectRef, BoundaryError> {
    job.object
        .as_ref()
        .filter(|object| {
            matches!(object.media.locator().binding(), S3Binding::VersionId(pin) if valid_version_pin(pin))
        })
        .ok_or(BoundaryError::Conflict)
}

fn next_revision(job: &ReviewJob, revision: u32) -> Result<u32, BoundaryError> {
    revision
        .checked_add(1)
        .filter(|_| job.view.revision == revision && job.view.status == ReviewStatus::NeedsReview)
        .ok_or(BoundaryError::Conflict)
}

fn is_retry_result(job: &ReviewJob, revision: u32) -> bool {
    use ReviewStatus::{NeedsReview, Published};
    job.retried_revision == Some(revision)
        && revision.checked_add(1) == Some(job.view.revision)
        && matches!(job.view.status, NeedsReview | Published)
}

async fn load(
    get: impl AsyncFn(&str, &str) -> Result<Option<ReviewJob>, BoundaryError>,
    caller: &Caller,
    job_id: &str,
) -> Result<ReviewJob, ApiError> {
    if !validate_job_id(job_id) {
        return Err(not_found());
    }
    get(&caller.user_id, job_id)
        .await?
        .filter(|job| job.view.job_id == job_id && owns_job(caller, &job.owner_id))
        .ok_or_else(not_found)
}

fn not_found() -> ApiError {
    ApiError::not_found("unknown ingest job")
}

impl From<BoundaryError> for ApiError {
    fn from(error: BoundaryError) -> Self {
        match error {
            BoundaryError::MissingOrFenced => not_found(),
            BoundaryError::Conflict => ApiError::by_code("conflict", "ingest state changed"),
            BoundaryError::Unavailable => ApiError::by_code("service_unavailable", ""),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queue_refuses_wrong_status_and_duplicate_rows() {
        let secret = "synthetic-admin-review-queue-fixture";
        let grant = crate::caller_auth::issue("owner", READ_SCOPE, 10, secret.as_bytes());
        let bearer = format!("Bearer {grant}");
        let auth = Authorization {
            bearer: Some(&bearer),
            secret: Some(secret),
            now: 1,
        };
        for (status, count) in [(ReviewStatus::Failed, 1), (ReviewStatus::Accepted, 2)] {
            let list = async |_: &str, _, _, _| {
                Ok((0..count)
                    .map(|_| ReviewJob {
                        view: JobView {
                            job_id: "upload".into(),
                            revision: 1,
                            status,
                            reason: None,
                        },
                        owner_id: "owner".into(),
                        object: None,
                        retried_revision: None,
                    })
                    .collect())
            };
            let request = QueueRequest {
                status: Some(ReviewStatus::Accepted),
                ..Default::default()
            };
            assert_eq!(queue(&auth, list, request).await.unwrap_err().status, 503);
        }
    }
}
