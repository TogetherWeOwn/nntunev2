//! Native-callable ingest writers. Edge adapters inject only the database
//! transport; validation, ordered writes, replay probes and error policy stay
//! here so native acceptance tests exercise the production sequence.

use std::collections::{BTreeMap, BTreeSet};

use crate::ingest_jobs::{self, TransitionOutcome, MAX_ACCEPTED_AT_SECS};
use crate::retry::{JobError, ERR_ACCOUNT_DELETING, ERR_WRITE_FAILED};
use crate::track_writers::{self, TrackRow, TrackWriteOutcome};
use crate::transcode::{self, PublishOutcome};

/// Ordered SQL parameters and selected columns. UUIDs cross this boundary as
/// canonical text; the edge transport owns the PostgreSQL wire representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    I64(i64),
    I32(i32),
}

/// Only a SQLSTATE may leave the driver boundary, never a driver error message
/// or connection detail. `None` includes connect, transport and deadline errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbFailure {
    pub sqlstate: Option<String>,
}

/// Transport below the real writer sequence. Tests can record the exact SQL,
/// ordered binds, affected rows and reselects without a Workers `Env`.
#[allow(async_fn_in_trait)]
pub trait WriterDb {
    async fn execute(&self, sql: &'static str, params: &[Value]) -> Result<u64, DbFailure>;
    async fn query(
        &self,
        sql: &'static str,
        params: &[Value],
    ) -> Result<Vec<Vec<Value>>, DbFailure>;
}

/// Database-backed identity selected before transcode/artifact side effects.
/// The provider comes from the current checkpoint, never the stale message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationContext {
    pub track_id: String,
    pub provider_id: String,
}

/// Per-user publication identity, joined to a verified artifact through its
/// original file ordinal and the selected database track ID, never a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPublication {
    pub user_id: String,
    pub track_id: String,
    pub source_job_id: String,
    pub content_hash: String,
    pub provider_id: String,
    pub verified_at: i64,
}

/// Persist admission before enqueueing. An ignored insert can be a replay or
/// a closed fence; only the exact stored job identity makes it a replay.
pub async fn admit_job(
    db: &impl WriterDb,
    job_id: &str,
    user_id: &str,
    idempotency_key: &str,
    accepted_at: i64,
) -> Result<(), JobError> {
    ingest_jobs::validate_admit_params(job_id, user_id, idempotency_key, accepted_at)
        .map_err(|_| invalid())?;
    validate_owner(user_id)?;
    let affected = db
        .execute(
            ingest_jobs::upsert_ingest_job_sql(),
            &[
                text(job_id),
                text(user_id),
                text(idempotency_key),
                Value::I64(accepted_at),
            ],
        )
        .await
        .map_err(db_error)?;
    if written(affected)? {
        return Ok(());
    }
    let row = one_row(
        db.query(
            ingest_jobs::select_ingest_job_identity_sql(),
            &[text(job_id)],
        )
        .await
        .map_err(db_error)?,
    )?
    .ok_or_else(account_deleting)?;
    if row == [text(user_id), text(idempotency_key)] {
        Ok(())
    } else {
        Err(invalid())
    }
}

/// Persist terminal status before checkpoint persistence/ack, including terminal
/// checkpoint replays. Either stored terminal status is a converged replay;
/// this does not rewrite a first terminal status to the requested opposite one.
pub async fn settle_job(
    db: &impl WriterDb,
    job_id: &str,
    user_id: &str,
    status: &str,
) -> Result<(), JobError> {
    validate_owner(user_id)?;
    if !crate::jobs::validate_job_id(job_id) || !ingest_jobs::validate_transition_status(status) {
        return Err(invalid());
    }
    let affected = db
        .execute(
            ingest_jobs::transition_ingest_job_sql(),
            &[text(job_id), text(user_id), text(status)],
        )
        .await
        .map_err(db_error)?;
    if written(affected)? {
        return Ok(());
    }
    let row = one_row(
        db.query(
            ingest_jobs::select_ingest_job_status_sql(),
            &[text(job_id), text(user_id)],
        )
        .await
        .map_err(db_error)?,
    )?;
    let stored = match row.as_deref() {
        Some([Value::Text(status)]) => Some(status.as_str()),
        None => None,
        _ => return Err(invalid()),
    };
    match ingest_jobs::classify_transition_zero_rows(stored) {
        TransitionOutcome::Converged => Ok(()),
        TransitionOutcome::MissingOrFenced => Err(account_deleting()),
    }
}

/// Materialize normalized tracks and always reselect their database-owned IDs,
/// keyed by the original NZB file ordinal. Validate the entire batch first so
/// a bad tail cannot commit a valid prefix under an ambiguous file identity.
pub async fn materialize_tracks(
    db: &impl WriterDb,
    user_id: &str,
    rows: &[TrackRow<'_>],
) -> Result<BTreeMap<usize, String>, JobError> {
    validate_owner(user_id)?;
    let mut ordinals = BTreeSet::new();
    for row in rows {
        track_writers::validate_track_row(row).map_err(|_| invalid())?;
        if !ordinals.insert(row.file_ordinal) {
            return Err(invalid());
        }
    }
    let mut ids = BTreeMap::new();
    for row in rows {
        let affected = db
            .execute(
                track_writers::upsert_track_sql(),
                &[
                    text(row.mbid),
                    text(row.release_id),
                    text(row.title),
                    Value::I32(row.medium_no),
                    Value::I32(row.track_no),
                    text(user_id),
                ],
            )
            .await
            .map_err(db_error)?;
        let inserted = written(affected)?;
        let selected = one_row(
            db.query(
                track_writers::select_track_id_by_position_sql(),
                &[
                    text(row.release_id),
                    Value::I32(row.medium_no),
                    Value::I32(row.track_no),
                ],
            )
            .await
            .map_err(db_error)?,
        )?;
        if !inserted
            && track_writers::classify_track_zero_rows(selected.is_some())
                == TrackWriteOutcome::AccountDeleting
        {
            return Err(account_deleting());
        }
        let id = match selected.as_deref() {
            Some([Value::Text(id)]) if crate::queue::validate_mbid(id) => id.clone(),
            _ => return Err(invalid()),
        };
        ids.insert(row.file_ordinal, id);
    }
    Ok(ids)
}

/// Fail-closed native test boundary for the pre-artifact owner/provider/track
/// validation. It is not implemented or called by the edge sequencer yet;
/// independent acceptance must cover the real writer before that wiring lands.
pub async fn publication_context(
    _db: &impl WriterDb,
    _msg: &crate::queue::QueueMsg,
    _cp: &crate::jobs::JobCheckpoint,
    _input: &crate::transcode::TranscodeInput,
) -> Result<PublicationContext, JobError> {
    Err(JobError::transient(
        ERR_WRITE_FAILED,
        "publication context unavailable",
    ))
}

/// Execute media then grant only after verified bytes/R2/blob success. Each
/// zero-row write uses its declared PK reselect and existing classifier. Probe
/// success only disambiguates a replay; it never authorizes a new write.
pub async fn publish_media(
    db: &impl WriterDb,
    publication: &MediaPublication,
) -> Result<(), JobError> {
    let p = publication;
    validate_owner(&p.user_id)?;
    if !crate::queue::validate_mbid(&p.track_id)
        || !crate::queue::validate_provider_id(&p.provider_id)
        || !crate::jobs::validate_job_id(&p.source_job_id)
        || p.content_hash.len() != 64
        || !p
            .content_hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !(0..=MAX_ACCEPTED_AT_SECS).contains(&p.verified_at)
    {
        return Err(invalid());
    }
    let media_key = [
        text(&p.user_id),
        text(&p.track_id),
        text(&p.source_job_id),
        text(&p.content_hash),
    ];
    let mut media_params = media_key.to_vec();
    media_params.push(Value::I64(p.verified_at));
    let affected = db
        .execute(transcode::upsert_track_media_sql(), &media_params)
        .await
        .map_err(db_error)?;
    if !written(affected)? {
        let row = publication_row(
            db.query(transcode::select_track_media_by_pk_sql(), &media_key)
                .await
                .map_err(db_error)?,
        )?;
        // UUID wire decoding canonicalizes case. Compare the track's parsed
        // identity, but keep every other identity/profile field exact.
        let track_id = uuid::Uuid::parse_str(&p.track_id).map_err(|_| invalid())?;
        match row.as_slice() {
            [user, Value::Text(track), job, hash, Value::Text(format), Value::I32(64)]
                if user == &media_key[0]
                    && uuid::Uuid::parse_str(track).ok() == Some(track_id)
                    && job == &media_key[2]
                    && hash == &media_key[3]
                    && format == "opus" => {}
            _ => return Err(invalid()),
        }
    }
    let grant_key = [text(&p.user_id), text(&p.content_hash)];
    let affected = db
        .execute(
            transcode::upsert_cache_grant_sql(),
            &[
                text(&p.user_id),
                text(&p.content_hash),
                text(&p.provider_id),
                Value::I64(p.verified_at),
            ],
        )
        .await
        .map_err(db_error)?;
    if !written(affected)? {
        let row = publication_row(
            db.query(transcode::select_cache_grant_by_pk_sql(), &grant_key)
                .await
                .map_err(db_error)?,
        )?;
        // The grant's key is owner/hash, not provider. A different valid
        // first-writer provider is still the existing proof, never rewritten.
        match row.as_slice() {
            [user, hash, Value::Text(provider)]
                if user == &grant_key[0]
                    && hash == &grant_key[1]
                    && crate::queue::validate_provider_id(provider) => {}
            _ => return Err(invalid()),
        }
    }
    Ok(())
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn validate_owner(user_id: &str) -> Result<(), JobError> {
    if crate::dispatch::validate_user_id(user_id) {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn written(affected: u64) -> Result<bool, JobError> {
    match affected {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid()),
    }
}

fn one_row(mut rows: Vec<Vec<Value>>) -> Result<Option<Vec<Value>>, JobError> {
    match rows.len() {
        0 => Ok(None),
        1 => Ok(rows.pop()),
        _ => Err(invalid()),
    }
}

fn publication_row(rows: Vec<Vec<Value>>) -> Result<Vec<Value>, JobError> {
    let row = one_row(rows)?;
    match transcode::classify_publish_zero_rows(row.is_some()) {
        PublishOutcome::Duplicate => row.ok_or_else(invalid),
        _ => Err(account_deleting()),
    }
}

fn invalid() -> JobError {
    JobError::permanent(ERR_WRITE_FAILED, "writer identity or result invalid")
}

fn account_deleting() -> JobError {
    JobError::permanent(
        ERR_ACCOUNT_DELETING,
        "account is being deleted or writer row missing",
    )
}

fn db_error(failure: DbFailure) -> JobError {
    if crate::library::is_deterministic_sqlstate(failure.sqlstate.as_deref()) {
        JobError::permanent(ERR_WRITE_FAILED, "writer database schema mismatch")
    } else {
        JobError::transient(ERR_WRITE_FAILED, "writer database unavailable")
    }
}
