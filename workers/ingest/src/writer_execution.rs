//! Native-callable boundary for the ingest writers.
//!
//! This is the interface scaffold for the separately owned execution tests,
//! not a second persistence model. The executors are deliberately fail-closed
//! until those tests are delivered; no edge path calls them yet. The follow-up
//! implementation must use this same sequence from the admission/queue adapters
//! and inject only the database transport underneath it.

use std::collections::BTreeMap;

use crate::retry::{JobError, ERR_WRITE_FAILED};
use crate::track_writers::TrackRow;

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

/// Persist admission before enqueueing. The execution tests will pin replay
/// identity and refusal handling before the edge adapter is connected.
pub async fn admit_job(
    _db: &impl WriterDb,
    _job_id: &str,
    _user_id: &str,
    _idempotency_key: &str,
    _accepted_at: i64,
) -> Result<(), JobError> {
    Err(unavailable())
}

/// Persist terminal status before checkpoint persistence/ack, including terminal
/// checkpoint replays. Existing terminal convergence semantics remain unchanged.
pub async fn settle_job(
    _db: &impl WriterDb,
    _job_id: &str,
    _user_id: &str,
    _status: &str,
) -> Result<(), JobError> {
    Err(unavailable())
}

/// Materialize normalized tracks and reselect their database-owned IDs, keyed
/// by the original NZB file ordinal retained in each derived row.
pub async fn materialize_tracks(
    _db: &impl WriterDb,
    _user_id: &str,
    _rows: &[TrackRow<'_>],
) -> Result<BTreeMap<usize, String>, JobError> {
    Err(unavailable())
}

/// Execute media then grant only after verified bytes/R2/blob success. Each
/// zero-row write must use its declared PK reselect and existing classifier.
pub async fn publish_media(
    _db: &impl WriterDb,
    _publication: &MediaPublication,
) -> Result<(), JobError> {
    Err(unavailable())
}

fn unavailable() -> JobError {
    JobError::transient(ERR_WRITE_FAILED, "writer execution not implemented")
}
