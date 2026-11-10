//! Verified-scratch to manifest-candidate composition, not durable acceptance.
//!
//! Unmounted until classification/publication consumers are ready. A returned
//! candidate is not a database winner, mirror evidence or library membership.

use std::future::Future;

use crate::{
    audio_grouping::{prepare_manifest, GroupingError},
    audio_manifest::{valid_identity, AcceptedAudioManifest},
    audio_scratch::{ScratchAudioReader, ScratchPrefixStore},
    audio_selection::{extract_hints, AudioSelectionPlan, SelectionError},
    verify::VerifyProgress,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProducerError {
    Identity,
    Selection(SelectionError),
    Grouping(GroupingError),
}

/// No Debug: original bytes and private verification records are not status data.
/// These bindings come from the accepted job and completed verification round,
/// never a caller-supplied source URL or a zipped recording-id array.
pub struct VerifiedCandidateInput<'a> {
    pub job_id: &'a str,
    pub user_id: &'a str,
    pub original: &'a str,
    pub expected_hash: &'a str,
    pub progress: &'a VerifyProgress,
}

/// Recover the persisted selection before touching scratch, or propose and
/// atomically accept a complete verified candidate. The database winner alone
/// is returned for a later checkpoint/classification consumer. This does not
/// mount Matching or grant any publication/membership authority.
pub async fn accept_verified_manifest<S, D, A>(
    input: VerifiedCandidateInput<'_>,
    store: S,
    deadline: D,
    acceptance: &A,
) -> Result<AcceptedAudioManifest, crate::audio_acceptance::AcceptanceError>
where
    S: ScratchPrefixStore,
    D: Future<Output = ()> + Unpin,
    A: crate::audio_acceptance::ManifestAcceptanceStore,
{
    crate::audio_acceptance::accept_manifest(
        acceptance,
        input.job_id,
        input.user_id,
        input.expected_hash,
        || prepare_verified_candidate(input, store, deadline),
    )
    .await
}

/// Compose the bounded direct-FLAC path without exposing intermediate hints.
/// One caller-owned deadline covers all scratch I/O. Selection, transport or
/// grouping refusal returns no partial manifest. Malformed tag blocks retain
/// their selected file with empty hints, as required by the extraction contract.
///
/// The caller must freeze through `audio_manifest_store::freeze` and use only
/// its returned winner before checkpoint publication or mirror lookup. This
/// function deliberately does neither, and does not compute post-match positions.
pub async fn prepare_verified_candidate<S: ScratchPrefixStore, D: Future<Output = ()> + Unpin>(
    input: VerifiedCandidateInput<'_>,
    store: S,
    deadline: D,
) -> Result<AcceptedAudioManifest, ProducerError> {
    if !valid_identity(input.job_id, input.user_id, input.expected_hash) {
        return Err(ProducerError::Identity);
    }
    let plan = AudioSelectionPlan::from_original(input.original, input.expected_hash)
        .map_err(ProducerError::Selection)?;
    let mut reader = ScratchAudioReader::bind(
        input.job_id,
        input.original,
        input.expected_hash,
        input.progress,
        store,
        deadline,
    )
    .map_err(ProducerError::Selection)?;
    let extracted = extract_hints(&plan, &mut reader)
        .await
        .map_err(ProducerError::Selection)?;
    prepare_manifest(input.job_id, input.user_id, &plan, extracted).map_err(ProducerError::Grouping)
}
