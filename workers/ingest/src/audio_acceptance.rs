//! Read the durable selection before considering fresh scratch/tag extraction.
//!
//! Unmounted until the classification and publication consumers are ready. Only
//! the database winner may leave this seam; KV is never acceptance authority.

use std::future::Future;

use crate::{
    audio_manifest::{valid_identity, AcceptedAudioManifest},
    audio_manifest_store::FreezeError,
    audio_producer::ProducerError,
};

#[allow(async_fn_in_trait)]
pub trait ManifestAcceptanceStore {
    async fn load(
        &self,
        job_id: &str,
        user_id: &str,
        original_hash: &str,
    ) -> Result<Option<AcceptedAudioManifest>, FreezeError>;

    async fn freeze(
        &self,
        candidate: &AcceptedAudioManifest,
        job_id: &str,
        user_id: &str,
        original_hash: &str,
    ) -> Result<AcceptedAudioManifest, FreezeError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptanceError {
    Identity,
    Store(FreezeError),
    Producer(ProducerError),
    InvalidWinner,
}

fn bound(manifest: &AcceptedAudioManifest, job: &str, owner: &str, hash: &str) -> bool {
    manifest.is_for(job, owner) && manifest.original_content_sha256() == hash
}

/// A lazy producer is invoked only for a confirmed unfrozen original, never for
/// read errors, missing/fenced originals or redelivery of an accepted selection.
/// A concurrent first acceptance can beat our candidate: return that winner,
/// not the candidate. Legacy winners remain unchanged; position consumers must
/// still refuse their unfrozen effective-disc vectors.
pub async fn accept_manifest<S, F, Fut>(
    store: &S,
    job_id: &str,
    user_id: &str,
    original_hash: &str,
    prepare: F,
) -> Result<AcceptedAudioManifest, AcceptanceError>
where
    S: ManifestAcceptanceStore,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<AcceptedAudioManifest, ProducerError>>,
{
    if !valid_identity(job_id, user_id, original_hash) {
        return Err(AcceptanceError::Identity);
    }
    let winner = match store
        .load(job_id, user_id, original_hash)
        .await
        .map_err(AcceptanceError::Store)?
    {
        Some(winner) => winner,
        None => {
            let candidate = prepare().await.map_err(AcceptanceError::Producer)?;
            if !bound(&candidate, job_id, user_id, original_hash) {
                return Err(AcceptanceError::Identity);
            }
            store
                .freeze(&candidate, job_id, user_id, original_hash)
                .await
                .map_err(AcceptanceError::Store)?
        }
    };
    if !bound(&winner, job_id, user_id, original_hash) {
        return Err(AcceptanceError::InvalidWinner);
    }
    Ok(winner)
}

/// No Debug: the environment can hold secret-bearing bindings. Construction
/// opens no connection; each operation uses the existing bounded connector.
pub struct PgManifestAcceptanceStore<'a> {
    pub env: &'a worker::Env,
    pub trace_id: &'a str,
}

impl ManifestAcceptanceStore for PgManifestAcceptanceStore<'_> {
    async fn load(
        &self,
        job_id: &str,
        user_id: &str,
        original_hash: &str,
    ) -> Result<Option<AcceptedAudioManifest>, FreezeError> {
        let client = crate::library_db_client(self.env, self.trace_id)
            .await
            .map_err(|()| FreezeError::Unavailable)?;
        crate::audio_manifest_store::load(client, job_id, user_id, original_hash).await
    }

    async fn freeze(
        &self,
        candidate: &AcceptedAudioManifest,
        job_id: &str,
        user_id: &str,
        original_hash: &str,
    ) -> Result<AcceptedAudioManifest, FreezeError> {
        let client = crate::library_db_client(self.env, self.trace_id)
            .await
            .map_err(|()| FreezeError::Unavailable)?;
        crate::audio_manifest_store::freeze(client, candidate, job_id, user_id, original_hash).await
    }
}
