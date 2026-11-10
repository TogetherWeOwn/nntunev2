//! Durable ListenBrainz KEK rotation runs (`listenbrainz_rotation_runs`).
//!
//! Source only: no route, scheduler or runtime store calls this yet. A run is
//! a separate typed ledger from the vault `rotation_runs` table. The caller
//! supplies a stable run id and a connected `Client`; this module never
//! connects, reconnects or retries.
//!
//! One [`batch`] is ONE transaction that: sets finite local statement, lock
//! and idle-in-transaction timeouts; locks the run row (`FOR UPDATE NOWAIT`);
//! fences env, target, expected checkpoint sequence and unfinished state;
//! scans at most [`MAX_SCAN_LIMIT`](crate::listenbrainz_rotation::MAX_SCAN_LIMIT)
//! owners; and per owner reloads the link, locks the owner row, applies the
//! full-envelope compare-and-set and writes its scalar audit row; then
//! advances the checkpoint (cursor, sequence, counters) and commits. Nothing
//! is committed separately. Any statement error aborts the whole transaction
//! (including every rewrap in it): a latch on the transaction adapter makes a
//! swallowed per-row SQL failure fatal. A locally rejected row (corrupt or
//! unauthenticated data, unknown key) is counted `failed` and the batch goes
//! on, because it did not abort the transaction.
//!
//! The whole operation, commit included, runs under one deadline. If it
//! expires or the commit result is unknown the caller gets `Err` and the
//! in-memory report is discarded; it is never replayed. Call [`reconcile`]
//! with the same id: it locks the authoritative row and returns what was
//! actually committed. The target key is checked before any transaction.
//!
//! `finished` means only that the keyset scan was exhausted; failed and
//! deferred counts stay visible. It is not evidence that a key may be retired.

use std::cell::Cell;

use tokio_postgres::{types::Type, Client, GenericClient, Row};

use super::{cas_rewrap_in, read_in, rotation_candidates_in, StepError};
use crate::{
    handlers::StoreError,
    listenbrainz::{RewrapOutcome, StoredLink},
    listenbrainz_rotation::{
        check_scan_args,
        lifecycle::{valid_env, valid_run_id, valid_target, RotationRun},
        rotate_batch, LinkRewrapStore, OwnerCursor,
    },
    rotation::KekSource,
    worker_entry::with_deadline,
};

/// Per-statement ceiling inside a run transaction (ms).
pub(crate) const STATEMENT_TIMEOUT_MS: u64 = 5_000;
/// Longest wait for any lock inside a run transaction (ms).
pub(crate) const LOCK_TIMEOUT_MS: u64 = 2_000;
/// Idle-in-transaction ceiling, so a stalled caller cannot pin locks (ms).
pub(crate) const IDLE_IN_TRANSACTION_TIMEOUT_MS: u64 = 10_000;
/// Deadline for one whole open/reconcile/batch call, commit included (ms).
pub(crate) const OPERATION_DEADLINE_MS: u64 = 30_000;

const RUN_COLUMNS: &str = "id, env, to_version, owner_cursor, checkpoint_seq, scanned, \
     rewrapped, no_link, already_current, deferred, failed, \
     finished_at IS NOT NULL AS finished";

fn check_args(id: &str, env: &str, target: u32) -> Result<(), StoreError> {
    if valid_run_id(id) && valid_env(env) && valid_target(target) {
        Ok(())
    } else {
        Err(StoreError)
    }
}

/// `SET LOCAL` lands on the one backend this transaction pins.
async fn apply_timeouts(tx: &impl GenericClient) -> Result<(), StoreError> {
    tx.batch_execute(&format!(
        "SET LOCAL statement_timeout = {STATEMENT_TIMEOUT_MS}; \
         SET LOCAL lock_timeout = {LOCK_TIMEOUT_MS}; \
         SET LOCAL idle_in_transaction_session_timeout = {IDLE_IN_TRANSACTION_TIMEOUT_MS}"
    ))
    .await
    .map_err(|_| StoreError)
}

fn decode_run(row: &Row) -> Result<RotationRun, StoreError> {
    let get = |name: &str| row.try_get::<_, i64>(name).map_err(|_| StoreError);
    let to_version: i32 = row.try_get("to_version").map_err(|_| StoreError)?;
    let cursor: Option<String> = row.try_get("owner_cursor").map_err(|_| StoreError)?;
    let run = RotationRun {
        id: row.try_get("id").map_err(|_| StoreError)?,
        env: row.try_get("env").map_err(|_| StoreError)?,
        to_version: u32::try_from(to_version).map_err(|_| StoreError)?,
        // Exact stored bytes: a bad cursor fails closed, never normalized.
        owner_cursor: cursor
            .map(|c| OwnerCursor::parse(&c).ok_or(StoreError))
            .transpose()?,
        checkpoint_seq: get("checkpoint_seq")?,
        scanned: get("scanned")?,
        rewrapped: get("rewrapped")?,
        no_link: get("no_link")?,
        already_current: get("already_current")?,
        deferred: get("deferred")?,
        failed: get("failed")?,
        finished: row.try_get("finished").map_err(|_| StoreError)?,
    };
    run.validate()?;
    Ok(run)
}

/// Lock the authoritative run row without waiting behind another holder.
async fn lock_run(tx: &impl GenericClient, id: &str) -> Result<RotationRun, StoreError> {
    let sql = format!(
        "SELECT {RUN_COLUMNS} FROM listenbrainz_rotation_runs WHERE id = $1 FOR UPDATE NOWAIT"
    );
    let rows = tx
        .query_typed(&sql, &[(&id, Type::TEXT)])
        .await
        .map_err(|_| StoreError)?;
    match rows.as_slice() {
        [row] => {
            let run = decode_run(row)?;
            if run.id != id {
                return Err(StoreError);
            }
            Ok(run)
        }
        _ => Err(StoreError),
    }
}

/// Open (or idempotently re-open) the run `id` for `env` at `target`.
///
/// A replay of the same id, env and target returns the stored run, progress
/// included. The same id with a different env or target is an error. A
/// different open run for the same env, whatever its target, violates the
/// partial unique index and fails the whole call.
pub(crate) async fn open(
    client: &mut Client,
    id: &str,
    env: &str,
    target: u32,
    keks: &impl KekSource,
) -> Result<RotationRun, StoreError> {
    check_args(id, env, target)?;
    if keks.kek(target).is_none() {
        return Err(StoreError);
    }
    let fresh = RotationRun::new(id, env, target)?;
    with_deadline(open_tx(client, &fresh), OPERATION_DEADLINE_MS, StoreError).await
}

async fn open_tx(client: &mut Client, fresh: &RotationRun) -> Result<RotationRun, StoreError> {
    let tx = client.transaction().await.map_err(|_| StoreError)?;
    match open_in(&tx, fresh).await {
        Ok(run) => {
            tx.commit().await.map_err(|_| StoreError)?;
            Ok(run)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

async fn open_in(tx: &impl GenericClient, fresh: &RotationRun) -> Result<RotationRun, StoreError> {
    apply_timeouts(tx).await?;
    let to_version = i32::try_from(fresh.to_version).map_err(|_| StoreError)?;
    let no_cursor: Option<&str> = None;
    tx.query_typed(
        "INSERT INTO listenbrainz_rotation_runs (id, env, to_version, owner_cursor, \
         checkpoint_seq, scanned, rewrapped, no_link, already_current, deferred, failed) \
         VALUES ($1, $2, $3, $4, 0, 0, 0, 0, 0, 0, 0) ON CONFLICT (id) DO NOTHING",
        &[
            (&fresh.id.as_str(), Type::TEXT),
            (&fresh.env.as_str(), Type::TEXT),
            (&to_version, Type::INT4),
            (&no_cursor, Type::TEXT),
        ],
    )
    .await
    .map_err(|_| StoreError)?;
    let run = lock_run(tx, &fresh.id).await?;
    if run.env != fresh.env || run.to_version != fresh.to_version {
        return Err(StoreError);
    }
    Ok(run)
}

/// Read what is durably committed for run `id`, under its row lock, after an
/// error whose commit result is unknown. Returns the stored state only (never
/// a replayed report). `env` and `target` must match the stored run.
pub(crate) async fn reconcile(
    client: &mut Client,
    id: &str,
    env: &str,
    target: u32,
) -> Result<RotationRun, StoreError> {
    check_args(id, env, target)?;
    with_deadline(
        reconcile_tx(client, id, env, target),
        OPERATION_DEADLINE_MS,
        StoreError,
    )
    .await
}

async fn reconcile_tx(
    client: &mut Client,
    id: &str,
    env: &str,
    target: u32,
) -> Result<RotationRun, StoreError> {
    let tx = client.transaction().await.map_err(|_| StoreError)?;
    let result = async {
        apply_timeouts(&tx).await?;
        let run = lock_run(&tx, id).await?;
        if run.env != env || run.to_version != target {
            return Err(StoreError);
        }
        Ok(run)
    }
    .await;
    // Read-only: nothing to commit either way.
    let _ = tx.rollback().await;
    result
}

/// Transaction-scoped store for [`rotate_batch`]. Reuses the ordinary scan,
/// read and CAS bodies on the one open transaction. A statement error
/// aborts a PostgreSQL transaction, so the first one is latched; the
/// orchestrator would otherwise count it as a failed row and carry on.
struct TxLinks<'a, T: GenericClient> {
    tx: &'a T,
    sql_failed: Cell<bool>,
}

impl<T: GenericClient> TxLinks<'_, T> {
    fn live(&self) -> Result<(), StoreError> {
        if self.sql_failed.get() {
            Err(StoreError)
        } else {
            Ok(())
        }
    }

    fn step<R>(&self, result: Result<R, StepError>) -> Result<R, StoreError> {
        result.map_err(|e| {
            if e == StepError::Sql {
                self.sql_failed.set(true);
            }
            StoreError
        })
    }
}

impl<T: GenericClient> LinkRewrapStore for TxLinks<'_, T> {
    async fn list_candidates(
        &self,
        below_version: u32,
        after: Option<&OwnerCursor>,
        limit: u32,
    ) -> Result<Vec<OwnerCursor>, StoreError> {
        self.live()?;
        self.step(rotation_candidates_in(self.tx, below_version, after, limit).await)
    }

    async fn read_link(&self, user_id: &str) -> Result<Option<StoredLink>, StoreError> {
        self.live()?;
        self.step(read_in(self.tx, user_id).await)
    }

    async fn cas_rewrap_link(
        &self,
        user_id: &str,
        captured: &StoredLink,
        replacement: &StoredLink,
        now: u64,
    ) -> Result<RewrapOutcome, StoreError> {
        self.live()?;
        self.step(cas_rewrap_in(self.tx, user_id, captured, replacement, now).await)
    }
}

/// Run one batch of at most `limit` owners for run `id` and checkpoint it.
///
/// `expected_seq` is the `checkpoint_seq` the caller last saw; a stale or
/// replayed value is rejected before any scan. On `Err` the call may or may
/// not have committed (only a commit-time or deadline error is ambiguous):
/// call [`reconcile`] rather than retrying with the old sequence.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn batch(
    client: &mut Client,
    id: &str,
    env: &str,
    target: u32,
    expected_seq: i64,
    limit: u32,
    keks: &impl KekSource,
    now: u64,
) -> Result<RotationRun, StoreError> {
    check_args(id, env, target)?;
    check_scan_args(target, limit).map_err(|_| StoreError)?;
    if expected_seq < 0 || expected_seq == i64::MAX || i64::try_from(now).is_err() {
        return Err(StoreError);
    }
    // Checked before any transaction so a missing key never opens one.
    if keks.kek(target).is_none() {
        return Err(StoreError);
    }
    with_deadline(
        batch_tx(client, id, env, target, expected_seq, limit, keks, now),
        OPERATION_DEADLINE_MS,
        StoreError,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn batch_tx(
    client: &mut Client,
    id: &str,
    env: &str,
    target: u32,
    expected_seq: i64,
    limit: u32,
    keks: &impl KekSource,
    now: u64,
) -> Result<RotationRun, StoreError> {
    let tx = client.transaction().await.map_err(|_| StoreError)?;
    match batch_in(&tx, id, env, target, expected_seq, limit, keks, now).await {
        Ok(run) => {
            // A failure here has an unknown outcome; the report is dropped.
            tx.commit().await.map_err(|_| StoreError)?;
            Ok(run)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn batch_in(
    tx: &impl GenericClient,
    id: &str,
    env: &str,
    target: u32,
    expected_seq: i64,
    limit: u32,
    keks: &impl KekSource,
    now: u64,
) -> Result<RotationRun, StoreError> {
    apply_timeouts(tx).await?;
    let current = lock_run(tx, id).await?;
    current.fence(env, target, expected_seq)?;
    let links = TxLinks {
        tx,
        sql_failed: Cell::new(false),
    };
    let report = rotate_batch(
        &links,
        keks,
        target,
        current.owner_cursor.as_ref(),
        limit,
        now,
    )
    .await;
    // A latched SQL failure aborts everything, whatever the report says.
    if links.sql_failed.get() {
        return Err(StoreError);
    }
    let report = report.map_err(|_| StoreError)?;
    let next = current.checkpoint(expected_seq, &report)?;
    persist(tx, &current, &next).await?;
    Ok(next)
}

/// Write the checkpoint, fenced again in SQL on env, target, the previous
/// sequence and unfinished state, and require that the stored result is
/// exactly the computed one.
async fn persist(
    tx: &impl GenericClient,
    previous: &RotationRun,
    next: &RotationRun,
) -> Result<(), StoreError> {
    let to_version = i32::try_from(next.to_version).map_err(|_| StoreError)?;
    let cursor: Option<&str> = next.owner_cursor.as_ref().map(OwnerCursor::as_str);
    let sql = format!(
        "UPDATE listenbrainz_rotation_runs SET owner_cursor = $1, checkpoint_seq = $2, \
         scanned = $3, rewrapped = $4, no_link = $5, already_current = $6, deferred = $7, \
         failed = $8, updated_at = clock_timestamp(), \
         finished_at = CASE WHEN $9::boolean THEN clock_timestamp() ELSE NULL END \
         WHERE id = $10 AND env = $11 AND to_version = $12 AND checkpoint_seq = $13 \
         AND finished_at IS NULL RETURNING {RUN_COLUMNS}"
    );
    let rows = tx
        .query_typed(
            &sql,
            &[
                (&cursor, Type::TEXT),
                (&next.checkpoint_seq, Type::INT8),
                (&next.scanned, Type::INT8),
                (&next.rewrapped, Type::INT8),
                (&next.no_link, Type::INT8),
                (&next.already_current, Type::INT8),
                (&next.deferred, Type::INT8),
                (&next.failed, Type::INT8),
                (&next.finished, Type::BOOL),
                (&next.id.as_str(), Type::TEXT),
                (&next.env.as_str(), Type::TEXT),
                (&to_version, Type::INT4),
                (&previous.checkpoint_seq, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    match rows.as_slice() {
        [row] if decode_run(row)? == *next => Ok(()),
        _ => Err(StoreError),
    }
}
