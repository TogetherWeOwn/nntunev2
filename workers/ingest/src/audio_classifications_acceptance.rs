//! Read the durable classification before minting fresh evidence.
//!
//! Unmounted until the publication consumer is ready. Only a present,
//! owner/job/digest-bound checkpoint permits skipping evidence retrieval; a
//! missing checkpoint, fenced original or read error refuses rather than
//! authorizing regeneration. The classification checkpoint is consumed only
//! when it was derived from the accepted manifest winner and frozen through
//! the store, and only the database winner leaves this seam. KV is never
//! acceptance authority, and classification grants no membership, catalogue
//! or playback authority.

use std::future::Future;

use crate::{
    audio_classifications::{
        classify_entries, ClassError, ClassificationCheckpoint, MirrorEvidence,
    },
    audio_classifications_store::StoreError,
    audio_manifest::{valid_identity, AcceptedAudioManifest},
};

#[allow(async_fn_in_trait)]
pub trait ClassificationAcceptanceStore {
    async fn load(
        &self,
        job_id: &str,
        user_id: &str,
        original_hash: &str,
    ) -> Result<Option<ClassificationCheckpoint>, StoreError>;

    async fn freeze(
        &self,
        candidate: &ClassificationCheckpoint,
        job_id: &str,
        user_id: &str,
        original_hash: &str,
    ) -> Result<ClassificationCheckpoint, StoreError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationAcceptanceError {
    Identity,
    Store(StoreError),
    Classify(ClassError),
    InvalidWinner,
}

fn bound(checkpoint: &ClassificationCheckpoint, job: &str, owner: &str, hash: &str) -> bool {
    checkpoint.is_for(job, owner) && checkpoint.original_content_sha256() == hash
}

/// A lazy evidence producer is invoked only for a confirmed miss, never for
/// read errors, a persisted checkpoint or redelivery of an accepted
/// classification. A concurrent first classification can beat our candidate:
/// return that winner, not the candidate. The accepted manifest itself is
/// identity-checked before any store access. Legacy rows stay unchanged;
/// position consumers must still refuse unfrozen manifest geometry.
pub async fn accept_classifications<S, F, Fut>(
    store: &S,
    manifest: &AcceptedAudioManifest,
    job_id: &str,
    user_id: &str,
    original_hash: &str,
    prepare: F,
) -> Result<ClassificationCheckpoint, ClassificationAcceptanceError>
where
    S: ClassificationAcceptanceStore,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<Option<MirrorEvidence>>, ClassError>>,
{
    if !valid_identity(job_id, user_id, original_hash) {
        return Err(ClassificationAcceptanceError::Identity);
    }
    if !manifest.is_for(job_id, user_id) || manifest.original_content_sha256() != original_hash {
        return Err(ClassificationAcceptanceError::Identity);
    }
    let winner = match store
        .load(job_id, user_id, original_hash)
        .await
        .map_err(ClassificationAcceptanceError::Store)?
    {
        Some(winner) => winner,
        None => {
            let evidence = prepare()
                .await
                .map_err(ClassificationAcceptanceError::Classify)?;
            let candidate = classify_entries(manifest, &evidence)
                .map_err(ClassificationAcceptanceError::Classify)?;
            store
                .freeze(&candidate, job_id, user_id, original_hash)
                .await
                .map_err(ClassificationAcceptanceError::Store)?
        }
    };
    if !bound(&winner, job_id, user_id, original_hash) {
        return Err(ClassificationAcceptanceError::InvalidWinner);
    }
    Ok(winner)
}

/// No Debug: the environment can hold secret-bearing bindings. Construction
/// opens no connection; each operation uses the existing bounded connector.
pub struct PgClassificationAcceptanceStore<'a> {
    pub env: &'a worker::Env,
    pub trace_id: &'a str,
}

impl ClassificationAcceptanceStore for PgClassificationAcceptanceStore<'_> {
    async fn load(
        &self,
        job_id: &str,
        user_id: &str,
        original_hash: &str,
    ) -> Result<Option<ClassificationCheckpoint>, StoreError> {
        let client = crate::library_db_client(self.env, self.trace_id)
            .await
            .map_err(|()| StoreError::Unavailable)?;
        crate::audio_classifications_store::load(client, job_id, user_id, original_hash).await
    }

    async fn freeze(
        &self,
        candidate: &ClassificationCheckpoint,
        job_id: &str,
        user_id: &str,
        original_hash: &str,
    ) -> Result<ClassificationCheckpoint, StoreError> {
        let client = crate::library_db_client(self.env, self.trace_id)
            .await
            .map_err(|()| StoreError::Unavailable)?;
        crate::audio_classifications_store::freeze(
            client,
            candidate,
            job_id,
            user_id,
            original_hash,
        )
        .await
    }
}
