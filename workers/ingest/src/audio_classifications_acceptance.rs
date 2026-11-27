//! Read the durable classification before minting fresh evidence.
//!
//! Unmounted until the publication consumer is ready. A confirmed miss can
//! trigger evidence retrieval. A fenced original or read error refuses it.
//! Only a database winner consistent with the accepted manifest leaves this
//! seam. KV is never acceptance authority, and classification grants no
//! membership, catalogue or playback authority.

use std::future::Future;

use crate::{
    audio_classifications::{
        classify_entries, ClassError, ClassificationCheckpoint, MirrorEvidence,
    },
    audio_classifications_store::StoreError,
    audio_manifest::{valid_identity, AcceptedAudioManifest},
    audio_positions::PositionError,
    audio_publication::prepare_publication,
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
    if manifest.effective_discs().is_none() {
        return Err(ClassificationAcceptanceError::Classify(
            ClassError::Positions(PositionError::UnfrozenDiscs),
        ));
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
    prepare_publication(manifest, &winner)
        .map_err(|_| ClassificationAcceptanceError::InvalidWinner)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_manifest::{AudioEntry, AudioSelector};

    struct Store {
        replay: bool,
        winner: ClassificationCheckpoint,
    }

    impl ClassificationAcceptanceStore for Store {
        async fn load(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Option<ClassificationCheckpoint>, StoreError> {
            Ok(self.replay.then(|| self.winner.clone()))
        }

        async fn freeze(
            &self,
            _: &ClassificationCheckpoint,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<ClassificationCheckpoint, StoreError> {
            assert!(!self.replay);
            Ok(self.winner.clone())
        }
    }

    #[tokio::test]
    async fn refuses_a_winner_with_the_right_identity_and_wrong_geometry() {
        let sha = "a".repeat(64);
        let manifest = AcceptedAudioManifest::from_entries(
            "job_a",
            "owner",
            &sha,
            vec![AudioEntry {
                audio_ordinal: 1,
                source_selector: AudioSelector {
                    nzb_file_ordinal: 0,
                    archive_member: None,
                    audio_selection_index: 0,
                },
                artist_group_ref: 1,
                release_group_ref: 1,
                hints: Default::default(),
                embedded_art_selector: None,
            }],
        )
        .unwrap();
        let winner = classify_entries(&manifest, &[None]).unwrap();
        let mut invalid = serde_json::to_value(&winner).unwrap();
        invalid["entries"][0]["release_group_ref"] = 2.into();
        let invalid = ClassificationCheckpoint::parse(&invalid.to_string()).unwrap();
        for replay in [true, false] {
            for (winner, valid) in [(winner.clone(), true), (invalid.clone(), false)] {
                let store = Store { replay, winner };
                let got =
                    accept_classifications(&store, &manifest, "job_a", "owner", &sha, || async {
                        assert!(!replay, "replays must skip fresh evidence");
                        Ok(vec![None])
                    })
                    .await;
                assert_eq!(
                    got,
                    if valid {
                        Ok(store.winner)
                    } else {
                        Err(ClassificationAcceptanceError::InvalidWinner)
                    }
                );
            }
        }
    }
}
