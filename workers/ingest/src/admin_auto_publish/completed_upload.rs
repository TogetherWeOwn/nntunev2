//! Unmounted caller from a scoped upload outcome to catalogue publication.

use nntune_core::s3_locator::S3Binding;
use tokio_postgres::Transaction;

use super::{stage, AuthorizedRelease, CommittedFlac, StageOutcome};
use crate::{
    catalogue_publication::PublicationError,
    matching::admin_publish_match::{CompleteReleaseCandidates, RECORDINGS_LIMIT},
    multipart::session_outcome::{uuid_text, CommittedOutcome, CompletionScope},
};

/// Trusted final checkpoint and authenticated readback in job file order.
#[derive(Clone, Copy)]
pub struct VerifiedUpload<'a> {
    pub scope: CompletionScope<'a>,
    pub flac: CommittedFlac<'a>,
}

/// Stage a release from immutable `completed` session outcomes. The reader
/// must recheck live authorization and resolve each outcome under the session
/// fence after the atomic winner and terminal-session commit. Abort, expiry and
/// their cleanup must preserve completed inventory. Missing results and errors stop writes.
///
/// Use the same path on retry. It reads outcomes and stages catalogue rows
/// without dispatching storage operations or restaging the guarded winner.
/// The caller supplies a bounded READ COMMITTED transaction, retains the job's
/// lifecycle fence, and acknowledges publication only after outer commit.
pub async fn stage_completed_release(
    tx: &mut Transaction<'_>,
    job: &AuthorizedRelease<'_>,
    uploads: &[VerifiedUpload<'_>],
    query: &str,
    candidates: Option<CompleteReleaseCandidates<'_>>,
    recordings: (u16, &str),
    mut read_committed: impl AsyncFnMut(
        &CompletionScope<'_>,
    ) -> Result<Option<CommittedOutcome>, PublicationError>,
) -> Result<StageOutcome, PublicationError> {
    use PublicationError::Refused;
    if uploads.is_empty() || uploads.len() != job.files.len() || uploads.len() >= RECORDINGS_LIMIT {
        return Err(Refused);
    }

    let mut committed = Vec::with_capacity(uploads.len());
    for upload in uploads {
        let scope = &upload.scope;
        let receipt = &upload.flac.receipt;
        let [environment, library, object, generation, configuration] = receipt.intent.identities;
        if (scope.environment, scope.library, scope.principal)
            != (job.environment, job.library, job.owner_id)
            || (scope.environment, scope.library, scope.seal) != (environment, library, generation)
            || u64::try_from(scope.offset) != Ok(receipt.intent.plaintext_len)
        {
            return Err(Refused);
        }
        let outcome = read_committed(scope).await?.ok_or(Refused)?;
        let map = receipt.intent.map_id;
        let root = receipt.root_version_id;
        if [map, object, generation, configuration, root].map(uuid_text)
            != [
                &outcome.map,
                &outcome.object,
                &outcome.generation,
                &outcome.configuration,
                &outcome.root_version,
            ]
            .map(String::as_str)
            || !matches!(receipt.locator.binding(), S3Binding::VersionId(pin) if pin == &outcome.media_version)
            || receipt
                .page_versions
                .iter()
                .copied()
                .ne(outcome.page_versions.iter().map(String::as_str))
        {
            return Err(Refused);
        }
        committed.push(upload.flac);
    }

    stage(tx, job, &committed, query, candidates, recordings).await
}
