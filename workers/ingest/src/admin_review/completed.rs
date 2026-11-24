//! Unmounted retry receiver for the one-object FLAC review contract.

use tokio_postgres::{Client, IsolationLevel, Transaction};

use super::*;
use crate::{
    admin_auto_publish::{
        completed_upload::{stage_completed_release, VerifiedUpload},
        AuthorizedRelease, ExpectedFile, StageOutcome,
    },
    catalogue_publication::PublicationError,
    matching::admin_publish_match::{CompleteReleaseCandidates, FileNumbering},
    multipart::session_outcome::{CommittedOutcome, CompletionScope},
};

pub struct ReviewTarget<'a> {
    pub job_id: &'a str,
    pub body: &'a [u8],
    pub environment: [u8; 16],
    pub library: [u8; 16],
}

/// Resolved by the server for the locked job and revision. The upload carries
/// the sealed authenticated readback, scoped final checkpoint and exact pins.
/// Use the pinned job and authorized CC0 mirror. Return Unavailable for outages.
pub struct ResolvedCompletion<'a> {
    pub job_id: &'a str,
    pub revision: u32,
    pub upload: VerifiedUpload<'a>,
    pub numbering: FileNumbering<'a>,
    pub query: &'a str,
    pub candidates: Option<CompleteReleaseCandidates<'a>>,
    pub recordings: (u16, &'a str),
}

/// Use a bounded connection and the existing fenced completed-outcome reader.
/// `lock` fences live admin role, account deletion, owner, serving scope and
/// the job's revision and retention through commit, including on replay.
/// `record` persists the outcome, input revision and audit in that transaction.
pub async fn retry_completed<'a>(
    auth: &Authorization<'_>,
    client: &mut Client,
    target: ReviewTarget<'_>,
    lock: impl AsyncFn(&Transaction<'_>, &str, &str) -> Result<Option<ReviewJob>, BoundaryError>,
    record: impl AsyncFnOnce(&Transaction<'_>, &ReviewJob, &JobView) -> Result<(), BoundaryError>,
    resolve: impl AsyncFnOnce(&ReviewJob) -> Result<ResolvedCompletion<'a>, BoundaryError>,
    read_committed: impl AsyncFnMut(
        &CompletionScope<'_>,
    ) -> Result<Option<CommittedOutcome>, PublicationError>,
) -> Result<JobView, ApiError> {
    let (caller, request) = retry_input(auth, target.body)?;
    let revision = request.revision;
    let mut tx = client
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .start()
        .await
        .map_err(|_| BoundaryError::Unavailable)?;
    let job = load(
        async |actor, id| lock(&tx, actor, id).await,
        &caller,
        target.job_id,
    )
    .await?;
    let object = versioned_object(&job)?;
    if is_retry_result(&job, revision) {
        return Ok(job.view);
    }
    let next = next_revision(&job, revision)?;
    let resolved = resolve(&job).await?;
    if (resolved.job_id, resolved.revision) != (target.job_id, revision) {
        return Err(BoundaryError::Conflict.into());
    }
    let release = AuthorizedRelease {
        job_id: target.job_id,
        owner_id: &caller.user_id,
        environment: target.environment,
        library: target.library,
        files: &[ExpectedFile {
            numbering: resolved.numbering,
            object,
        }],
    };
    let (status, reason) = match stage_completed_release(
        &mut tx,
        &release,
        &[resolved.upload],
        resolved.query,
        resolved.candidates,
        resolved.recordings,
        read_committed,
    )
    .await
    .map_err(|error| match error {
        PublicationError::Refused => BoundaryError::Conflict,
        PublicationError::Indeterminate => BoundaryError::Unavailable,
    })? {
        StageOutcome::Staged(_) => (ReviewStatus::Published, None),
        StageOutcome::NeedsReview(reason) => (ReviewStatus::NeedsReview, Some(reason)),
    };
    let view = JobView {
        job_id: job.view.job_id.clone(),
        revision: next,
        status,
        reason,
    };
    record(&tx, &job, &view).await?;
    tx.commit().await.map_err(|_| BoundaryError::Unavailable)?;
    Ok(view)
}
