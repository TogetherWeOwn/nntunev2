//! Internal PostgreSQL stage/control/publication authority. No live dispatch or
//! public route uses this adapter yet. Ordered schema integration, upload
//! initialization and authenticated control/reconciliation routes remain required
//! integration. Null-only reconciliation below is an explicit internal primitive.
//! The caller supplies the authenticated original, captures the trusted queue
//! generation, and applies its existing database/service deadline. No KV fallback.

use crate::{
    admin_checkpoint::{Identity, Snapshot, MAX_CHECKPOINT_BYTES},
    admin_execution::{
        controlled_checkpoint, validate_publication, validate_stage_transition, Control,
    },
    admin_writers::{write_album, AlbumWrite, FileGeometry, SourceSealer, WriteError},
    jobs::{self, JobCheckpoint, JobState},
    matching::MbRecording,
};
use tokio_postgres::{Client, GenericClient};

/// Borrow exclusively from the API/keyring's authenticated original holder,
/// never a queue body or request. Like run_stage/AlbumWrite, this internal seam
/// is not an authorization or decryption boundary. No payload clone or Debug.
pub struct StageOriginal<'a> {
    pub job_id: &'a str,
    pub idempotency_key: &'a str,
    pub nzb_bytes: usize,
    pub parsed: &'a nntune_core::nzb::Document,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    Invalid,
    Stale,
    Database,
}

fn identity<'a>(original: &StageOriginal<'a>) -> Identity<'a> {
    Identity {
        job_id: original.job_id,
        user_id: "",
        idempotency_key: original.idempotency_key,
    }
}

// Bound the transferred JSON text before decoding. SQL NULL, JSON null and
// malformed/oversized checkpoints are all non-executable, not initializations.
// Original identity/length is rechecked after asynchronous provider reads.
const SELECT: &str = "SELECT j.status, j.admin_checkpoint_revision,
    j.admin_checkpoint IS NULL AS legacy_null,
    CASE WHEN octet_length(j.admin_checkpoint::text) <= 16384
         THEN j.admin_checkpoint::text ELSE NULL END AS checkpoint
    FROM ingest_jobs j JOIN ingest_originals o ON o.job_id = j.id
    WHERE j.id = $1 AND j.source = 'admin'
      AND j.idempotency_key = $2 AND j.content_sha256 = substring($2 from 7)
      AND j.nzb_bytes = $3 AND o.content_sha256 = j.content_sha256
      AND o.nzb_bytes = j.nzb_bytes
      AND o.nzb_ref_envelope_version = 2 AND o.nzb_ref_payload_format = 1
      AND octet_length(o.nzb_ref_encrypted) = $3 + 16
      AND octet_length(o.nzb_ref_dek_wrapped_nonce) = 12
      AND octet_length(o.nzb_ref_dek_wrapped) = 48
      AND octet_length(o.nzb_ref_payload_nonce) = 12 LIMIT 2";

async fn load_on(
    client: &impl GenericClient,
    original: &StageOriginal<'_>,
    generation: i64,
    lock: bool,
) -> Result<(Snapshot, String), StoreError> {
    if generation <= 0 {
        return Err(StoreError::Invalid);
    }
    let bytes = i64::try_from(original.nzb_bytes).map_err(|_| StoreError::Invalid)?;
    let sql = if lock {
        format!("{SELECT} FOR UPDATE OF j FOR SHARE OF o")
    } else {
        SELECT.into()
    };
    let rows = client
        .query(&sql, &[&original.job_id, &original.idempotency_key, &bytes])
        .await
        .map_err(|_| StoreError::Database)?;
    if rows.len() != 1 {
        return Err(StoreError::Stale);
    }
    let row = &rows[0];
    let encoded: Option<String> = row.try_get("checkpoint").map_err(|_| StoreError::Invalid)?;
    let encoded = encoded.ok_or(StoreError::Invalid)?;
    if encoded.len() > MAX_CHECKPOINT_BYTES {
        return Err(StoreError::Invalid);
    }
    let revision: i64 = row
        .try_get("admin_checkpoint_revision")
        .map_err(|_| StoreError::Invalid)?;
    let snapshot = Snapshot::decode(&encoded, revision, &identity(original))
        .map_err(|_| StoreError::Invalid)?;
    let status: String = row.try_get("status").map_err(|_| StoreError::Invalid)?;
    if status != snapshot.status() {
        return Err(StoreError::Invalid);
    }
    if snapshot.generation() != generation {
        return Err(StoreError::Stale);
    }
    Ok((snapshot, encoded))
}

/// Load only the generation captured by authenticated enqueue. Old deliveries
/// must not read the current generation and silently adopt a manual restart.
/// Terminal/parked snapshots may be inspected but cannot persist a stage.
pub async fn load(
    client: &Client,
    original: &StageOriginal<'_>,
    generation: i64,
) -> Result<Snapshot, StoreError> {
    load_on(client, original, generation, false)
        .await
        .map(|(snapshot, _)| snapshot)
}

/// Explicit reconciliation of historical SQL-NULL authority only. The caller
/// must authorize the admin action, supply the authenticated original, capture
/// expected status/revision, and apply its deadline. This is not automatic
/// repair, upload dedupe, an audit sink or a live route. Populated (even invalid)
/// checkpoints and parked/terminal rows need their separate legal control path.
/// All evidence starts fresh at Queued; revision/generation never reset. Enqueue
/// only after commit; commit-result uncertainty requires authority reload.
/// New uploads instead initialize with their job/original admission transaction.
/// This function never sends work or reads KV.
pub async fn reconcile_null(
    client: &mut Client,
    original: &StageOriginal<'_>,
    expected_status: &str,
    expected_revision: i64,
) -> Result<Snapshot, StoreError> {
    if !matches!(expected_status, "queued" | "running") {
        return Err(StoreError::Invalid);
    }
    let successor = Snapshot::reconciled(&identity(original), expected_revision)
        .map_err(|_| StoreError::Invalid)?;
    let encoded = successor.encode().map_err(|_| StoreError::Invalid)?;
    let bytes = i64::try_from(original.nzb_bytes).map_err(|_| StoreError::Invalid)?;
    let tx = client
        .transaction()
        .await
        .map_err(|_| StoreError::Database)?;
    let rows = tx
        .query(
            &format!("{SELECT} FOR UPDATE OF j FOR SHARE OF o"),
            &[&original.job_id, &original.idempotency_key, &bytes],
        )
        .await
        .map_err(|_| StoreError::Database)?;
    if rows.len() != 1 {
        return Err(StoreError::Stale);
    }
    let row = &rows[0];
    let legacy_null: bool = row
        .try_get("legacy_null")
        .map_err(|_| StoreError::Invalid)?;
    let status: String = row.try_get("status").map_err(|_| StoreError::Invalid)?;
    let revision: i64 = row
        .try_get("admin_checkpoint_revision")
        .map_err(|_| StoreError::Invalid)?;
    if !legacy_null || status != expected_status || revision != expected_revision {
        return Err(StoreError::Stale);
    }
    let count = tx
        .execute(
            "UPDATE ingest_jobs SET admin_checkpoint = $4::text::jsonb,
                admin_checkpoint_revision = $5, status = 'queued'
             WHERE id = $1 AND source = 'admin' AND status = $2
               AND admin_checkpoint_revision = $3 AND admin_checkpoint IS NULL",
            &[
                &original.job_id,
                &expected_status,
                &expected_revision,
                &encoded,
                &successor.revision(),
            ],
        )
        .await
        .map_err(|_| StoreError::Database)?;
    if count != 1 {
        return Err(StoreError::Stale);
    }
    tx.commit().await.map_err(|_| StoreError::Database)?;
    Ok(successor)
}

/// Persist one ordinary stage and its status in a short job-locked transaction.
/// On stale/failed/cancelled writes the caller discards computed results. A
/// commit-result error is uncertain: reload DB, never blindly retry the write.
/// This function cannot initialize, restart, reject or publish a job.
pub async fn persist_stage(
    client: &mut Client,
    original: &StageOriginal<'_>,
    expected: &Snapshot,
    next: &JobCheckpoint,
) -> Result<Snapshot, StoreError> {
    let bound = identity(original);
    // Snapshot's public counter helper is not transition authority. Revalidate
    // both representations and derive the only permitted ordinary successor.
    let encoded = expected.encode().map_err(|_| StoreError::Invalid)?;
    Snapshot::decode(&encoded, expected.revision(), &bound).map_err(|_| StoreError::Invalid)?;
    validate_stage_transition(
        expected.checkpoint(),
        next,
        original.job_id,
        original.idempotency_key,
        original.parsed,
    )
    .map_err(|_| StoreError::Invalid)?;
    let successor = expected
        .successor(next.clone(), false, &bound)
        .map_err(|_| StoreError::Invalid)?;
    let tx = client
        .transaction()
        .await
        .map_err(|_| StoreError::Database)?;
    let (current, current_encoded) = load_on(&tx, original, expected.generation(), true).await?;
    if current != *expected {
        return Err(StoreError::Stale);
    }
    update_on(&tx, expected, &current_encoded, &successor).await?;
    tx.commit().await.map_err(|_| StoreError::Database)?;
    Ok(successor)
}

/// Commit one explicit, already-authorized admin control. The caller enforces
/// admin authorization (and archive capability for resume), supplies the original
/// holder and expected snapshot, and applies its bounded database deadline. This
/// internal adapter is not an auth boundary, audit sink or live manual endpoint.
/// Rotate generation even on rejection so all old queue/service results lose.
/// Enqueue only the committed generation after success; commit errors require
/// authority reload, not blind replay. This function never sends queue work.
pub async fn apply_control(
    client: &mut Client,
    original: &StageOriginal<'_>,
    expected: &Snapshot,
    control: Control<'_>,
) -> Result<Snapshot, StoreError> {
    let bound = identity(original);
    let encoded = expected.encode().map_err(|_| StoreError::Invalid)?;
    Snapshot::decode(&encoded, expected.revision(), &bound).map_err(|_| StoreError::Invalid)?;
    let next = controlled_checkpoint(
        expected.checkpoint(),
        control,
        original.job_id,
        original.idempotency_key,
        original.parsed,
    )
    .map_err(|_| StoreError::Invalid)?;
    let successor = expected
        .successor(next, true, &bound)
        .map_err(|_| StoreError::Invalid)?;
    let tx = client
        .transaction()
        .await
        .map_err(|_| StoreError::Database)?;
    let (current, current_encoded) = load_on(&tx, original, expected.generation(), true).await?;
    if current != *expected {
        return Err(StoreError::Stale);
    }
    update_on(&tx, expected, &current_encoded, &successor).await?;
    tx.commit().await.map_err(|_| StoreError::Database)?;
    Ok(successor)
}

async fn update_on(
    tx: &impl GenericClient,
    expected: &Snapshot,
    current_encoded: &str,
    successor: &Snapshot,
) -> Result<(), StoreError> {
    // After strict whole-snapshot comparison, retain the selected representation
    // for SQL equality: optional null vs omitted fields need not round-trip to
    // the same JSON keys. Revision prevents ABA when checkpoint contents repeat.
    let next_encoded = successor.encode().map_err(|_| StoreError::Invalid)?;
    let count = tx
        .execute(
            "UPDATE ingest_jobs SET admin_checkpoint = $5::text::jsonb,
                admin_checkpoint_revision = $6, status = $7
             WHERE id = $1 AND source = 'admin' AND status = $2
               AND admin_checkpoint_revision = $3
               AND admin_checkpoint = $4::text::jsonb",
            &[
                &expected.checkpoint().job_id,
                &expected.status(),
                &expected.revision(),
                &current_encoded,
                &next_encoded,
                &successor.revision(),
                &successor.status(),
            ],
        )
        .await
        .map_err(|_| StoreError::Database)?;
    if count != 1 {
        return Err(StoreError::Stale);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationError {
    Checkpoint(StoreError),
    Album(WriteError),
}

impl From<StoreError> for PublicationError {
    fn from(error: StoreError) -> Self {
        Self::Checkpoint(error)
    }
}

/// Commit album rows and Done in one outer transaction, never merely release
/// the album savepoint. The caller has already authenticated the original and
/// obtained CC0 mirror recordings and trusted decoded-file geometry; no network
/// work belongs under this lock. The sealer is local API/keyring work. The caller
/// applies its bounded deadline to the whole future, including sealing/commit.
/// Any error/drop before commit rolls back all rows. A commit-result error is
/// uncertain: reload authority before deciding whether publication succeeded.
pub async fn publish(
    client: &mut Client,
    original: &StageOriginal<'_>,
    expected: &Snapshot,
    recordings: &[MbRecording],
    geometry: &[FileGeometry],
    sealer: &mut impl SourceSealer,
) -> Result<Snapshot, PublicationError> {
    let bound = identity(original);
    let encoded = expected.encode().map_err(|_| StoreError::Invalid)?;
    Snapshot::decode(&encoded, expected.revision(), &bound).map_err(|_| StoreError::Invalid)?;
    validate_publication(
        expected.checkpoint(),
        original.job_id,
        original.idempotency_key,
        original.parsed,
    )
    .map_err(|_| StoreError::Invalid)?;
    let mut done = expected.checkpoint().clone();
    if jobs::advance_checkpoint(&mut done, false) != JobState::Done {
        return Err(StoreError::Invalid.into());
    }
    let successor = expected
        .successor(done, false, &bound)
        .map_err(|_| StoreError::Invalid)?;
    let mut tx = client
        .transaction()
        .await
        .map_err(|_| StoreError::Database)?;
    let (current, current_encoded) = load_on(&tx, original, expected.generation(), true).await?;
    if current != *expected {
        return Err(StoreError::Stale.into());
    }
    write_album(
        &mut tx,
        sealer,
        &AlbumWrite {
            checkpoint: current.checkpoint(),
            original_job: original.job_id,
            original_key: original.idempotency_key,
            parsed: original.parsed,
            recordings,
            geometry,
        },
    )
    .await
    .map_err(PublicationError::Album)?;
    update_on(&tx, expected, &current_encoded, &successor).await?;
    tx.commit().await.map_err(|_| StoreError::Database)?;
    Ok(successor)
}
