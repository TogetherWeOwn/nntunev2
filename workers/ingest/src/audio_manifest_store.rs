//! Durable first acceptance for private audio manifests (ADR 0002 §6).
//!
//! Unmounted preparation: Matching must use the returned database winner, not
//! its proposed selection, before copying it to KV or performing mirror lookup.
//! Only jobs with a retained, digest-bound original qualify. Legacy flat ledger
//! rows cannot be promoted into selections or membership through this seam.

use crate::audio_manifest::AcceptedAudioManifest;

pub const FREEZE_SQL: &str = include_str!("audio_manifest_store.sql");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeError {
    Identity,
    MissingOrFenced,
    InvalidStored,
    Permanent,
    Unavailable,
}

fn validate_candidate(
    manifest: &AcceptedAudioManifest,
    job_id: &str,
    user_id: &str,
    original_hash: &str,
) -> Result<(), FreezeError> {
    if !manifest.is_for(job_id, user_id) || manifest.original_content_sha256() != original_hash {
        return Err(FreezeError::Identity);
    }
    Ok(())
}

fn accept_stored(
    raw: &str,
    job_id: &str,
    user_id: &str,
    original_hash: &str,
) -> Result<AcceptedAudioManifest, FreezeError> {
    let stored = AcceptedAudioManifest::parse(raw).map_err(|_| FreezeError::InvalidStored)?;
    validate_candidate(&stored, job_id, user_id, original_hash)
        .map_err(|_| FreezeError::InvalidStored)?;
    Ok(stored)
}

/// One autocommit statement serializes contenders on the retained original row.
/// PostgreSQL re-evaluates COALESCE after a concurrent updater commits, returning
/// that first winner rather than an earlier statement-snapshot read. No fallback
/// to the candidate is allowed on a missing row, malformed winner or DB failure.
/// The owned client is dropped on deadline; an uncertain commit is safe to retry
/// through this same seam. No driver detail or manifest reaches errors/logs.
/// The caller opens the client through `library_db_client`'s bounded connect.
pub async fn freeze(
    client: tokio_postgres::Client,
    manifest: &AcceptedAudioManifest,
    job_id: &str,
    user_id: &str,
    original_hash: &str,
) -> Result<AcceptedAudioManifest, FreezeError> {
    validate_candidate(manifest, job_id, user_id, original_hash)?;
    let raw = serde_json::to_string(manifest).map_err(|_| FreezeError::Permanent)?;
    crate::library::with_db_deadline(
        async move {
            let row = client
                .query_opt(FREEZE_SQL, &[&job_id, &user_id, &raw, &original_hash])
                .await
                .map_err(|error| {
                    if crate::library::is_deterministic_sqlstate(
                        error.code().map(|code| code.code()),
                    ) {
                        FreezeError::Permanent
                    } else {
                        FreezeError::Unavailable
                    }
                })?
                .ok_or(FreezeError::MissingOrFenced)?;
            let stored: String = row.try_get(0).map_err(|_| FreezeError::InvalidStored)?;
            accept_stored(&stored, job_id, user_id, original_hash)
        },
        worker::Delay::from(std::time::Duration::from_millis(
            crate::library::LIBRARY_DB_DEADLINE_MS,
        )),
        || FreezeError::Unavailable,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn candidate(title: &str) -> AcceptedAudioManifest {
        AcceptedAudioManifest::parse(
            &json!({
                "version": 1, "job_id": "manifest_job", "user_id": "manifest_owner",
                "original_content_sha256": "a".repeat(64),
                "entries": [{"audio_ordinal": 1, "source_selector": {
                    "nzb_file_ordinal": 0, "archive_member": null, "audio_selection_index": 0
                }, "artist_group_ref": 1, "release_group_ref": 1, "hints": {"title": title}}],
                "art_blobs": []
            })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn candidate_requires_exact_job_owner_and_original_digest() {
        let manifest = candidate("Proposed");
        let hash = "a".repeat(64);
        assert_eq!(
            validate_candidate(&manifest, "manifest_job", "manifest_owner", &hash),
            Ok(())
        );
        for (job, owner, digest) in [
            ("other_job", "manifest_owner", hash.clone()),
            ("manifest_job", "other_owner", hash),
            ("manifest_job", "manifest_owner", "b".repeat(64)),
        ] {
            assert_eq!(
                validate_candidate(&manifest, job, owner, &digest),
                Err(FreezeError::Identity)
            );
        }
    }

    #[test]
    fn persisted_winner_is_used_instead_of_changed_tags() {
        let winner = candidate("First accepted");
        let changed = candidate("Later tags");
        let stored = accept_stored(
            &serde_json::to_string(&winner).unwrap(),
            "manifest_job",
            "manifest_owner",
            &"a".repeat(64),
        )
        .unwrap();
        assert_eq!(stored, winner);
        assert_ne!(stored, changed);
    }

    #[test]
    fn persisted_winner_must_parse_and_match_every_binding() {
        let raw = serde_json::to_string(&candidate("Private title")).unwrap();
        for (job, owner, digest) in [
            ("other_job", "manifest_owner", "a".repeat(64)),
            ("manifest_job", "other_owner", "a".repeat(64)),
            ("manifest_job", "manifest_owner", "b".repeat(64)),
        ] {
            assert_eq!(
                accept_stored(&raw, job, owner, &digest),
                Err(FreezeError::InvalidStored)
            );
        }
        for raw in ["null", "{}", "not json"] {
            assert_eq!(
                accept_stored(raw, "manifest_job", "manifest_owner", &"a".repeat(64)),
                Err(FreezeError::InvalidStored)
            );
        }
    }

    #[test]
    fn freeze_statement_is_single_row_atomic_and_owner_fenced() {
        assert!(FREEZE_SQL.contains("coalesce(original.accepted_audio_manifest_v1, $3)"));
        assert!(FREEZE_SQL.contains("deleting_at IS NULL FOR SHARE"));
        assert!(FREEZE_SQL.contains("job.user_id = $2"));
        assert!(FREEZE_SQL.contains("original.content_sha256 = $4"));
        assert!(FREEZE_SQL.contains("job.content_sha256 = original.content_sha256"));
        assert!(FREEZE_SQL.contains("RETURNING original.accepted_audio_manifest_v1"));
        assert!(!FREEZE_SQL.contains("DO NOTHING"));
    }
}
