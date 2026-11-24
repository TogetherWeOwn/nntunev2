//! Unmounted composition for an authorized admin FLAC release.

use tokio_postgres::Transaction;

use crate::{
    admin_review::ObjectRef,
    catalogue_publication::{self, PublicationError, PublishedObject, VerifiedOccurrence},
    digest_candidate_identity::candidate_hashes,
    matching::admin_publish_match::{
        decide_publish_match, select_release, CompleteReleaseCandidates, FileNumbering, NeedsReview,
    },
    multipart::readback_inventory::VerifiedCompletedReadback,
    track_writers::track_row_for,
};

pub struct ExpectedFile<'a> {
    pub numbering: FileNumbering<'a>,
    pub object: &'a ObjectRef,
}

/// The complete, ordered file manifest and scope from the authorized job.
pub struct AuthorizedRelease<'a> {
    pub job_id: &'a str,
    pub owner_id: &'a str,
    pub environment: [u8; 16],
    pub library: [u8; 16],
    pub files: &'a [ExpectedFile<'a>],
}

/// Injected by the encrypted completion caller after authoritative commit or
/// fenced reconciliation. Readback proves the exact bytes are valid FLAC.
pub struct CommittedFlac<'a> {
    pub verified: &'a VerifiedCompletedReadback<'a, 'a>,
    pub receipt: PublishedObject<'a>,
}

impl CommittedFlac<'_> {
    fn matches(&self, job: &AuthorizedRelease<'_>, file: &ExpectedFile<'_>) -> bool {
        let Self { verified, receipt } = self;
        let [environment, library, ..] = receipt.intent.identities;
        (environment, library) == (job.environment, job.library)
            && verified.matches_media(&file.object.media, &file.object.plaintext_sha256)
            && receipt.locator == file.object.media.locator()
            && receipt.plaintext_sha256 == &file.object.plaintext_sha256
            && candidate_hashes(receipt.intent, verified.origin()).is_ok()
            && &receipt.root_version_id == verified.authenticated_envelope().0
            && verified
                .inventory()
                .pages()
                .iter()
                .map(|(_, pin)| pin.version)
                .eq(receipt.page_versions.iter().copied().map(Some))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StageOutcome {
    NeedsReview(NeedsReview),
    /// Track IDs in file order. These are usable only after outer commit.
    Staged(Vec<String>),
}

/// Stage every occurrence in pairing order in a bounded READ COMMITTED
/// transaction using trusted exact-title CC0 mirror evidence. Replay with the
/// same job, mapping and committed receipts.
///
/// The future encrypted caller must supply the authoritative committed result.
/// The future app proxy must establish current admin authority and bind the
/// manifest to the authorized job and revision independently of those results.
/// The caller owns lifecycle fences, retention, deadlines, review state, audit
/// and commit. Roll back the outer transaction on cancellation or uncertainty.
pub async fn stage(
    tx: &mut Transaction<'_>,
    job: &AuthorizedRelease<'_>,
    committed: &[CommittedFlac<'_>],
    query: &str,
    candidates: Option<CompleteReleaseCandidates<'_>>,
    recordings: (u16, &str),
) -> Result<StageOutcome, PublicationError> {
    if committed.is_empty()
        || job.files.len() != committed.len()
        || !committed
            .iter()
            .zip(job.files)
            .all(|(flac, file)| flac.matches(job, file))
    {
        return Err(PublicationError::Refused);
    }

    let matched = select_release(query, candidates).and_then(|release| {
        let numbers: Vec<_> = job.files.iter().map(|file| file.numbering).collect();
        let (status, body) = recordings;
        decide_publish_match(release, committed.len(), &numbers, status, body)
    });
    let matched = match matched {
        Ok(matched) => matched,
        Err(reason) => return Ok(StageOutcome::NeedsReview(reason)),
    };
    let tracks = matched
        .recordings()
        .iter()
        .enumerate()
        .map(|(ordinal, recording)| {
            track_row_for(matched.release_mbid(), recording, ordinal)
                .ok()
                .filter(|row| row.title.len() <= catalogue_publication::MAX_TRACK_TITLE_BYTES)
        });
    let Some(tracks) = tracks.collect::<Option<Vec<_>>>() else {
        return Ok(StageOutcome::NeedsReview(NeedsReview::RecordingEvidence));
    };

    let mut batch = tx.transaction().await?;
    let mut track_ids = Vec::with_capacity(tracks.len());
    for (track, flac) in tracks.into_iter().zip(committed) {
        let occurrence = VerifiedOccurrence {
            job_id: job.job_id,
            owner_id: job.owner_id,
            object_id: flac.receipt.intent.identities[2],
            track,
        };
        match catalogue_publication::stage(&mut batch, &occurrence, &flac.receipt).await {
            Ok(id) => track_ids.push(id),
            Err(error) => {
                batch.rollback().await?;
                return Err(error);
            }
        }
    }
    batch.commit().await?;
    Ok(StageOutcome::Staged(track_ids))
}
