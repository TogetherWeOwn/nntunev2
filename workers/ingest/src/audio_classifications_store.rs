//! Durable first acceptance for classification checkpoints (ADR 0002 §6).
//!
//! Unmounted preparation: the production consumer must load the returned
//! database winner, never its own reselection, before publishing positions or
//! mirror snapshots. Only checkpoints classified from an accepted manifest
//! qualify; legacy unfrozen manifests produce no checkpoint through any other
//! path. This seam grants no membership, catalogue or playback authority.

use crate::audio_classifications::ClassificationCheckpoint;
use crate::audio_manifest::{valid_identity, MAX_AUDIO_ENTRIES, MAX_MANIFEST_BYTES};

pub const FREEZE_SQL: &str = include_str!("audio_classifications_store.sql");

pub const LOAD_SQL: &str = "SELECT original.accepted_audio_classifications_v1
FROM ingest_originals AS original
JOIN ingest_jobs AS job ON job.id = original.job_id
JOIN users AS owner ON owner.id = job.user_id
WHERE original.job_id = $1 AND job.user_id = $2
  AND owner.deleting_at IS NULL
  AND original.content_sha256 = $3
  AND job.content_sha256 = original.content_sha256";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    Identity,
    MissingOrFenced,
    InvalidStored,
    Permanent,
    Unavailable,
}

fn validate_candidate(
    checkpoint: &ClassificationCheckpoint,
    job_id: &str,
    user_id: &str,
    original_hash: &str,
) -> Result<(), StoreError> {
    if !valid_identity(job_id, user_id, original_hash)
        || !checkpoint.is_for(job_id, user_id)
        || checkpoint.original_content_sha256() != original_hash
    {
        return Err(StoreError::Identity);
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct CheckpointVersion {
    version: u8,
}

fn supported_encoding(raw: &str) -> bool {
    // The checkpoint column has the same byte cap as the manifest column.
    raw.len() <= MAX_MANIFEST_BYTES
        && serde_json::from_str::<CheckpointVersion>(raw).is_ok_and(|root| root.version == 1)
}

fn valid_entries(checkpoint: &ClassificationCheckpoint) -> bool {
    let entries = checkpoint.entries();
    let mut survivors = std::collections::HashSet::new();
    !entries.is_empty()
        && entries.len() <= MAX_AUDIO_ENTRIES
        && entries.iter().enumerate().all(|(index, entry)| {
            let ordinal = (index + 1) as u32;
            entry.audio_ordinal == ordinal
                && entry.medium_no > 0
                && entry.track_no > 0
                && (1..=ordinal).contains(&entry.release_group_ref)
                && (entry.duplicate_of.is_some()
                    || survivors.insert((entry.release_group_ref, entry.medium_no, entry.track_no)))
                && {
                    let anchor = &entries[(entry.release_group_ref - 1) as usize];
                    anchor.release_group_ref == entry.release_group_ref && anchor.kind == entry.kind
                }
                && entry.duplicate_of.is_none_or(|winner| {
                    (1..ordinal).contains(&winner) && {
                        let winner = &entries[(winner - 1) as usize];
                        winner.duplicate_of.is_none()
                            && winner.release_group_ref == entry.release_group_ref
                            && winner.medium_no == entry.medium_no
                            && winner.track_no == entry.track_no
                    }
                })
        })
}

fn accept_stored(
    raw: &str,
    job_id: &str,
    user_id: &str,
    original_hash: &str,
) -> Result<ClassificationCheckpoint, StoreError> {
    if !supported_encoding(raw) {
        return Err(StoreError::InvalidStored);
    }
    let stored = ClassificationCheckpoint::parse(raw).map_err(|_| StoreError::InvalidStored)?;
    validate_candidate(&stored, job_id, user_id, original_hash)
        .map_err(|_| StoreError::InvalidStored)?;
    if !valid_entries(&stored) {
        return Err(StoreError::InvalidStored);
    }
    Ok(stored)
}

fn encode_candidate(checkpoint: &ClassificationCheckpoint) -> Result<String, StoreError> {
    if !valid_entries(checkpoint) {
        return Err(StoreError::Permanent);
    }
    let raw = serde_json::to_string(checkpoint).map_err(|_| StoreError::Permanent)?;
    supported_encoding(&raw)
        .then_some(raw)
        .ok_or(StoreError::Permanent)
}

/// Only a present, owner/digest-bound original whose checkpoint is SQL NULL
/// returns None. Missing originals, deletion fences and invalid records refuse
/// rather than authorizing regeneration. The returned checkpoint is immutable;
/// this read grants neither membership nor permission to publish after deletion.
pub async fn load(
    client: tokio_postgres::Client,
    job_id: &str,
    user_id: &str,
    original_hash: &str,
) -> Result<Option<ClassificationCheckpoint>, StoreError> {
    if !valid_identity(job_id, user_id, original_hash) {
        return Err(StoreError::Identity);
    }
    crate::library::with_db_deadline(
        async move {
            let row = client
                .query_opt(LOAD_SQL, &[&job_id, &user_id, &original_hash])
                .await
                .map_err(|error| {
                    if crate::library::is_deterministic_sqlstate(
                        error.code().map(|code| code.code()),
                    ) {
                        StoreError::Permanent
                    } else {
                        StoreError::Unavailable
                    }
                })?
                .ok_or(StoreError::MissingOrFenced)?;
            let raw: Option<String> = row.try_get(0).map_err(|_| StoreError::InvalidStored)?;
            raw.as_deref()
                .map(|raw| accept_stored(raw, job_id, user_id, original_hash))
                .transpose()
        },
        worker::Delay::from(std::time::Duration::from_millis(
            crate::library::LIBRARY_DB_DEADLINE_MS,
        )),
        || StoreError::Unavailable,
    )
    .await
}

/// One autocommit statement serializes contenders on the retained original row.
/// PostgreSQL re-evaluates COALESCE after a concurrent updater commits, returning
/// that first winner rather than an earlier statement-snapshot read. No fallback
/// to the candidate is allowed on a missing row, malformed winner or DB failure.
/// The owned client is dropped on deadline; an uncertain commit is safe to retry
/// through this same seam. No driver detail or checkpoint reaches errors/logs.
/// The caller opens the client through `library_db_client`'s bounded connect.
pub async fn freeze(
    client: tokio_postgres::Client,
    checkpoint: &ClassificationCheckpoint,
    job_id: &str,
    user_id: &str,
    original_hash: &str,
) -> Result<ClassificationCheckpoint, StoreError> {
    validate_candidate(checkpoint, job_id, user_id, original_hash)?;
    let raw = encode_candidate(checkpoint)?;
    crate::library::with_db_deadline(
        async move {
            let row = client
                .query_opt(FREEZE_SQL, &[&job_id, &user_id, &raw, &original_hash])
                .await
                .map_err(|error| {
                    if crate::library::is_deterministic_sqlstate(
                        error.code().map(|code| code.code()),
                    ) {
                        StoreError::Permanent
                    } else {
                        StoreError::Unavailable
                    }
                })?
                .ok_or(StoreError::MissingOrFenced)?;
            let stored: String = row.try_get(0).map_err(|_| StoreError::InvalidStored)?;
            accept_stored(&stored, job_id, user_id, original_hash)
        },
        worker::Delay::from(std::time::Duration::from_millis(
            crate::library::LIBRARY_DB_DEADLINE_MS,
        )),
        || StoreError::Unavailable,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_classifications::{classify_entries, ClassError, ClassKind};
    use crate::audio_grouping::prepare_manifest;
    use crate::audio_manifest::AudioHints;
    use crate::audio_selection::{AudioSelectionPlan, ExtractedAudio};
    use crate::dispatch::content_sha256_hex;

    fn test_checkpoint() -> ClassificationCheckpoint {
        let mut xml = String::from("<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">");
        xml.push_str("<file poster=\"test\" date=\"0\" subject=\"&quot;0.flac&quot; yEnc (1/1)\"><groups><group>alt.binaries.test</group></groups><segments><segment bytes=\"100\" number=\"1\">part0@example.invalid</segment></segments></file>");
        xml.push_str("</nzb>");
        let plan =
            AudioSelectionPlan::from_original(&xml, &content_sha256_hex(xml.as_bytes())).unwrap();
        let hints = AudioHints {
            album: Some("Album".into()),
            track_artist: Some("Artist".into()),
            title: Some("A".into()),
            track_no: Some(1),
            ..AudioHints::default()
        };
        let extracted = plan
            .selections()
            .iter()
            .map(|s| ExtractedAudio {
                audio_ordinal: s.audio_ordinal(),
                selector: s.selector().clone(),
                hints: hints.clone(),
            })
            .collect();
        let manifest =
            prepare_manifest("class_store_job", "class_store_owner", &plan, extracted).unwrap();
        classify_entries(&manifest, &[None]).unwrap()
    }

    #[test]
    fn candidate_requires_exact_job_owner_and_original_digest() {
        let checkpoint = test_checkpoint();
        let hash = checkpoint.original_content_sha256().to_string();
        assert_eq!(
            validate_candidate(&checkpoint, "class_store_job", "class_store_owner", &hash),
            Ok(())
        );
        for (job, owner, digest) in [
            ("other_job", "class_store_owner", hash.clone()),
            ("class_store_job", "other_owner", hash.clone()),
            ("class_store_job", "class_store_owner", "b".repeat(64)),
        ] {
            assert_eq!(
                validate_candidate(&checkpoint, job, owner, &digest),
                Err(StoreError::Identity)
            );
        }
    }

    #[test]
    fn persisted_winner_is_used_instead_of_a_reread() {
        let checkpoint = test_checkpoint();
        let hash = checkpoint.original_content_sha256().to_string();
        let raw = serde_json::to_string(&checkpoint).unwrap();
        let stored = accept_stored(&raw, "class_store_job", "class_store_owner", &hash).unwrap();
        assert_eq!(stored, checkpoint);
        assert_eq!(stored.entries(), checkpoint.entries());
        assert!(matches!(stored.entries()[0].kind, ClassKind::AllLocal));
    }

    #[test]
    fn persisted_winner_must_parse_and_match_every_binding() {
        let checkpoint = test_checkpoint();
        let raw = encode_candidate(&checkpoint).unwrap();
        let hash = checkpoint.original_content_sha256().to_string();
        let accept = |raw: &str| accept_stored(raw, "class_store_job", "class_store_owner", &hash);
        let at_limit = format!("{raw}{}", " ".repeat(MAX_MANIFEST_BYTES - raw.len()));
        assert_eq!(accept(&at_limit), Ok(checkpoint));
        let oversized = format!("{at_limit} ");
        for raw in ["null", "{}", "not json", &oversized] {
            assert_eq!(accept(raw), Err(StoreError::InvalidStored));
        }
        for (job, owner, digest) in [
            ("other_job", "class_store_owner", hash.clone()),
            ("class_store_job", "other_owner", hash.clone()),
            ("class_store_job", "class_store_owner", "b".repeat(64)),
        ] {
            assert_eq!(
                accept_stored(&raw, job, owner, &digest),
                Err(StoreError::InvalidStored)
            );
        }
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let first = &value["entries"][0];
        let mut second = first.clone();
        second["audio_ordinal"] = 2.into();
        for (pointer, replacement, rejected_by_parser) in [
            ("/version", 2.into(), true),
            ("/entries", serde_json::json!([]), true),
            ("/entries/0/track_no", 0.into(), false),
            ("/entries/0/audio_ordinal", 2.into(), false),
            ("/entries/0/release_group_ref", 0.into(), false),
            ("/entries/0/duplicate_of", 1.into(), false),
            ("/entries", serde_json::json!([first, second]), false),
        ] {
            let mut bad = value.clone();
            *bad.pointer_mut(pointer).unwrap() = replacement;
            let raw = bad.to_string();
            assert_eq!(accept(&raw), Err(StoreError::InvalidStored), "{pointer}");
            let parsed = ClassificationCheckpoint::parse(&raw);
            if rejected_by_parser {
                assert_eq!(parsed, Err(ClassError::Evidence), "{pointer}");
            } else {
                assert_eq!(
                    encode_candidate(&parsed.unwrap()),
                    Err(StoreError::Permanent),
                    "{pointer}"
                );
            }
        }
    }

    #[test]
    fn freeze_statement_is_single_row_atomic_and_owner_fenced() {
        assert!(FREEZE_SQL.contains("coalesce("));
        assert!(FREEZE_SQL.contains("accepted_audio_classifications_v1"));
        assert!(FREEZE_SQL.contains("deleting_at IS NULL FOR SHARE"));
        assert!(FREEZE_SQL.contains("job.user_id = $2"));
        assert!(FREEZE_SQL.contains("original.content_sha256 = $4"));
        assert!(FREEZE_SQL.contains("job.content_sha256 = original.content_sha256"));
        assert!(FREEZE_SQL.contains("RETURNING original.accepted_audio_classifications_v1"));
        assert!(!FREEZE_SQL.contains("DO NOTHING"));
        assert!(LOAD_SQL.contains("accepted_audio_classifications_v1"));
    }
}
