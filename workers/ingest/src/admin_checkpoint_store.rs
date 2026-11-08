//! Internal PostgreSQL stage authority. No live dispatch or public route uses
//! this adapter yet. Ordered schema integration, upload initialization, manual
//! controls and atomic publication/Done remain separate required integration.
//! The caller supplies the authenticated original, captures the trusted queue
//! generation, and applies its existing database/service deadline. No KV fallback.

use crate::{
    admin_checkpoint::{Identity, Snapshot, MAX_CHECKPOINT_BYTES},
    admin_execution::validate_stage_transition,
    jobs::JobCheckpoint,
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
    let next_encoded = successor.encode().map_err(|_| StoreError::Invalid)?;
    let tx = client
        .transaction()
        .await
        .map_err(|_| StoreError::Database)?;
    let (current, current_encoded) = load_on(&tx, original, expected.generation(), true).await?;
    if current != *expected {
        return Err(StoreError::Stale);
    }
    // After strict whole-snapshot comparison, retain the selected representation
    // for SQL equality: optional null vs omitted fields need not round-trip to
    // the same JSON keys. Revision prevents ABA when checkpoint contents repeat.
    let count = tx
        .execute(
            "UPDATE ingest_jobs SET admin_checkpoint = $5::text::jsonb,
                admin_checkpoint_revision = $6, status = $7
             WHERE id = $1 AND source = 'admin' AND status = $2
               AND admin_checkpoint_revision = $3
               AND admin_checkpoint = $4::text::jsonb",
            &[
                &original.job_id,
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
    tx.commit().await.map_err(|_| StoreError::Database)?;
    Ok(successor)
}
