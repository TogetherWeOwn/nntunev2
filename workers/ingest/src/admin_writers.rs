//! Storage primitive for the authenticated admin path, not a queue handler.
//! The caller owns the outer transaction and its deadline, must compare the
//! expected checkpoint with durable state in that transaction BEFORE calling,
//! and must atomically persist Done before committing. This module does none
//! of those operations and cannot enable publication on its own. Live dispatch
//! remains closed pending the durable stage-store and schema integration.

use std::{collections::HashSet, future::Future};

use nntune_core::{
    nzb::Document,
    original_source::{OriginalSource, RangeHint, MAX_CIPHERTEXT_BYTES, MAX_FILE_BYTES},
};
use tokio_postgres::Transaction;

use crate::{
    admin_execution, jobs::JobCheckpoint, library, matching::MbRecording, track_writers as tracks,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteError {
    Invalid,
    Fence,
    Conflict,
    Seal,
    Store,
}

/// Only sealed material crosses back from the API/keyring. No Debug/Serialize,
/// and no KEK/DEK enters ingest. The trusted sealer must bind the exact source's
/// winning track identity using the existing shared-source envelope contract.
pub struct SourceEnvelope {
    pub ciphertext: Vec<u8>,
    pub kek_version: i32,
    pub dek_wrapped_nonce: [u8; 12],
    pub dek_wrapped: Vec<u8>,
    pub payload_nonce: [u8; 12],
}

pub trait SourceSealer {
    fn seal(
        &mut self,
        source: &OriginalSource,
    ) -> impl Future<Output = Result<SourceEnvelope, WriteError>>;
}

/// From the trusted original-file inspection, never estimated from encoded
/// segment byte counts (STAT does not establish decoded size). Optional range
/// geometry remains a hint that stream must corroborate against actual bytes.
pub struct FileGeometry {
    pub file_ordinal: usize,
    pub decoded_bytes: u64,
    pub range_hint: Option<RangeHint>,
}

/// Job/key/document are borrowed from the authenticated original reader, not
/// queue input. Recordings must be from the retained release in the CC0 mirror.
/// No upload fields or plaintext source values are stored in this structure.
pub struct AlbumWrite<'a> {
    pub checkpoint: &'a JobCheckpoint,
    pub original_job: &'a str,
    pub original_key: &'a str,
    pub parsed: &'a Document,
    pub recordings: &'a [MbRecording],
    pub geometry: &'a [FileGeometry],
}

fn validate<'a>(write: &AlbumWrite<'a>) -> Result<tracks::TrackDerivation<'a>, WriteError> {
    admin_execution::validate_publication(
        write.checkpoint,
        write.original_job,
        write.original_key,
        write.parsed,
    )
    .map_err(|_| WriteError::Invalid)?;
    if !library::valid_admin_key(write.original_key)
        || write.original_job.len() != 47
        || !write.original_job.starts_with("adm_")
    {
        return Err(WriteError::Invalid);
    }
    let derived = tracks::derive_track_rows(
        write.checkpoint.admin_release_mbid.as_deref(),
        &write.parsed.files,
        write.recordings,
    )
    .map_err(|_| WriteError::Invalid)?;
    // Never publish a partial album silently. A stale/incomplete mirror or
    // missing decoded geometry is a caller-visible refusal, not Done.
    if derived.rows.is_empty()
        || !derived.skips.is_empty()
        || write.geometry.len() != derived.rows.len()
    {
        return Err(WriteError::Invalid);
    }
    let mut ordinals = HashSet::new();
    if write
        .parsed
        .files
        .iter()
        .any(|file| file.ordinal >= nntune_core::nzb::MAX_FILES || !ordinals.insert(file.ordinal))
    {
        return Err(WriteError::Invalid);
    }
    let mut selected = HashSet::new();
    for geometry in write.geometry {
        if !selected.insert(geometry.file_ordinal)
            || !(1..=MAX_FILE_BYTES).contains(&geometry.decoded_bytes)
            || !derived
                .rows
                .iter()
                .any(|row| row.file_ordinal == geometry.file_ordinal)
        {
            return Err(WriteError::Invalid);
        }
    }
    if write
        .parsed
        .files
        .iter()
        .any(|file| file.role == "content" && file.container != "flac")
    {
        return Err(WriteError::Invalid);
    }
    Ok(derived)
}

/// A savepoint makes an error/cancellation roll back this entire album even
/// if the caller subsequently commits its outer transaction. Success only
/// releases the savepoint: no independent commit or Done transition exists.
pub async fn write_album(
    transaction: &mut Transaction<'_>,
    sealer: &mut impl SourceSealer,
    write: &AlbumWrite<'_>,
) -> Result<usize, WriteError> {
    let derived = validate(write)?;
    let recordings: Vec<String> = derived.rows.iter().map(|row| row.mbid.to_owned()).collect();
    let library_write = library::SharedUpsertParams {
        job_id: write.original_job,
        idempotency_key: write.original_key,
        release_mbid: write.checkpoint.admin_release_mbid.as_deref(),
        recording_mbids: &recordings,
    };
    library::validate_shared_library_write(&library_write).map_err(|_| WriteError::Invalid)?;
    let savepoint = transaction
        .transaction()
        .await
        .map_err(|_| WriteError::Store)?;
    let result = write_inner(&savepoint, sealer, write, &derived, &library_write).await;
    match result {
        Ok(count) => {
            savepoint.commit().await.map_err(|_| WriteError::Store)?;
            Ok(count)
        }
        Err(error) => {
            savepoint.rollback().await.map_err(|_| WriteError::Store)?;
            Err(error)
        }
    }
}

async fn write_inner(
    transaction: &Transaction<'_>,
    sealer: &mut impl SourceSealer,
    write: &AlbumWrite<'_>,
    derived: &tracks::TrackDerivation<'_>,
    library_write: &library::SharedUpsertParams<'_>,
) -> Result<usize, WriteError> {
    // Serialize against deletion, rejection and other publishers on the JOB,
    // never the uploader's account. Recheck the authenticated content identity.
    let job = transaction
        .query_opt(
            "SELECT j.id FROM ingest_jobs j WHERE j.id = $1 AND j.source = 'admin' AND j.status = 'running' AND j.idempotency_key = $2 AND j.idempotency_key = 'admin:' || j.content_sha256 AND EXISTS (SELECT 1 FROM ingest_originals o WHERE o.job_id = j.id AND o.content_sha256 = j.content_sha256 AND o.nzb_bytes = j.nzb_bytes) FOR UPDATE OF j",
            &[&write.original_job, &write.original_key],
        )
        .await
        .map_err(|_| WriteError::Store)?;
    if job.is_none() {
        return Err(WriteError::Fence);
    }
    for row in &derived.rows {
        transaction
            .execute(
                tracks::upsert_track_sql(),
                &[
                    &row.mbid,
                    &row.release_id,
                    &row.title,
                    &row.medium_no,
                    &row.track_no,
                    &write.original_job,
                ],
            )
            .await
            .map_err(|_| WriteError::Store)?;
        let selected = transaction
            .query_opt(
                &format!("{} FOR SHARE", tracks::select_track_for_source_sql()),
                &[&row.release_id, &row.medium_no, &row.track_no],
            )
            .await
            .map_err(|_| WriteError::Store)?
            .ok_or(WriteError::Fence)?;
        let id: String = selected.try_get("id").map_err(|_| WriteError::Store)?;
        let release: String = selected
            .try_get("release_id")
            .map_err(|_| WriteError::Store)?;
        let recording: String = selected.try_get("mbid").map_err(|_| WriteError::Conflict)?;
        let selected_track = tracks::SourceTrack {
            id: &id,
            release_id: &release,
            medium_no: selected
                .try_get("medium_no")
                .map_err(|_| WriteError::Store)?,
            track_no: selected
                .try_get("track_no")
                .map_err(|_| WriteError::Store)?,
            recording_id: &recording,
        };
        let geometry = write
            .geometry
            .iter()
            .find(|g| g.file_ordinal == row.file_ordinal)
            .ok_or(WriteError::Invalid)?;
        let source = tracks::original_source_for_track(
            write.original_job,
            row.release_id,
            &write.parsed.files,
            write.recordings,
            &selected_track,
            geometry.decoded_bytes,
            geometry.range_hint,
        )
        .map_err(|_| WriteError::Conflict)?;
        // Preserve the entire first provenance value, not merely its job id.
        // No ciphertext is loaded on replay and no redundant seal is requested.
        if source_exists(transaction, &id).await? {
            continue;
        }
        let sealed = sealer.seal(&source).await?;
        if !(17..=MAX_CIPHERTEXT_BYTES).contains(&sealed.ciphertext.len()) {
            return Err(WriteError::Seal);
        }
        let params = tracks::TrackSourceParams {
            track_id: &id,
            source_job_id: write.original_job,
            indexer_id: None,
            nzb_ref_encrypted: &sealed.ciphertext,
            nzb_ref_kek_version: sealed.kek_version,
            nzb_ref_dek_wrapped_nonce: &sealed.dek_wrapped_nonce,
            nzb_ref_dek_wrapped: &sealed.dek_wrapped,
            nzb_ref_payload_nonce: &sealed.payload_nonce,
            nzb_ref_payload_format: 1,
        };
        tracks::validate_track_source_params(&params).map_err(|_| WriteError::Seal)?;
        transaction
            .execute(
                tracks::upsert_track_source_sql(),
                &[
                    &params.track_id,
                    &params.source_job_id,
                    &params.indexer_id,
                    &params.nzb_ref_encrypted,
                    &params.nzb_ref_kek_version,
                    &params.nzb_ref_dek_wrapped_nonce,
                    &params.nzb_ref_dek_wrapped,
                    &params.nzb_ref_payload_nonce,
                    &params.nzb_ref_payload_format,
                ],
            )
            .await
            .map_err(|_| WriteError::Store)?;
        // A different job may have won the source slot while sealing. Check
        // that winner as well; never equate a zero-row insert with readiness.
        if !source_exists(transaction, &id).await? {
            return Err(WriteError::Fence);
        }
    }
    let affected = transaction
        .execute(
            library::upsert_shared_library_sql(),
            &[
                &library_write.job_id,
                &library_write.idempotency_key,
                &library_write.release_mbid,
                &library_write.recording_mbids,
            ],
        )
        .await
        .map_err(|_| WriteError::Store)?;
    if affected != 1 {
        return Err(WriteError::Conflict);
    }
    Ok(derived.rows.len())
}

async fn source_exists(transaction: &Transaction<'_>, id: &str) -> Result<bool, WriteError> {
    let existing = transaction.query_opt(
        "SELECT nzb_ref_envelope_version = 2 AND nzb_ref_payload_format = 1 AND nzb_ref_kek_version >= 0 AND octet_length(nzb_ref_encrypted) BETWEEN 17 AND 8388624 AND octet_length(nzb_ref_dek_wrapped_nonce) = 12 AND octet_length(nzb_ref_dek_wrapped) = 48 AND octet_length(nzb_ref_payload_nonce) = 12 AS valid FROM track_sources WHERE track_id = $1::text::uuid FOR SHARE",
        &[&id],
    ).await.map_err(|_| WriteError::Store)?;
    match existing {
        None => Ok(false),
        Some(row) if row.try_get::<_, bool>("valid").ok() == Some(true) => Ok(true),
        Some(_) => Err(WriteError::Conflict),
    }
}
