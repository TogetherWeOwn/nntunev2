use std::time::SystemTime;

use tokio_postgres::{types::Type, Client, IsolationLevel, Row, Transaction};

use super::{stage_completed_candidate, uuid, CandidateIntent, InventoryAppendError};
use crate::multipart::{
    readback_inventory::{ReadbackScope, VerifiedCompletedReadback},
    session_outcome::{CommittedOutcome, CompletionScope},
};
use OutcomeError::{Indeterminate, Refused};

const BOUND_TX: &str = "SET LOCAL transaction_timeout = '10s'; SET LOCAL synchronous_commit = on";
const LOCK: &str = include_str!("outcome_lock.sql");
const RECORD: &str = include_str!("outcome_record.sql");
const READ: &str = include_str!("outcome_read.sql");

#[derive(Debug, PartialEq, Eq)]
pub enum OutcomeError {
    Refused,
    Publication(InventoryAppendError),
    Indeterminate,
}

/// Exact committed pins with a bounded fence against winner deletion.
pub struct ProtectedReuse {
    outcome: CommittedOutcome,
    usable_until: SystemTime,
}

impl ProtectedReuse {
    pub fn outcome(&self) -> &CommittedOutcome {
        &self.outcome
    }

    pub fn usable_until(&self) -> SystemTime {
        self.usable_until
    }
}

impl CompletionScope<'_> {
    async fn lock(&self, tx: &Transaction<'_>, lease: Option<&str>) -> Result<(), OutcomeError> {
        tx.query_typed_opt(
            LOCK,
            &[
                (&uuid(&self.environment), Type::TEXT),
                (&uuid(&self.library), Type::TEXT),
                (&self.principal, Type::TEXT),
                (&self.session, Type::TEXT),
                (&self.fingerprint.as_slice(), Type::BYTEA),
                (&uuid(&self.seal), Type::TEXT),
                (&self.revision, Type::INT8),
                (&self.offset, Type::INT8),
                (&lease, Type::TEXT),
            ],
        )
        .await
        .map_err(|_| Indeterminate)?
        .map(|_| ())
        .ok_or(Refused)
    }
}

/// Use a dedicated READ COMMITTED transaction. Prior writes may advance the
/// same scoped checkpoint and obtain its final lease. Lock the session before
/// touching the digest reservation.
/// Supply authorized scope, a live lease, custody and retention. Report success after outer commit.
/// Roll back the outer transaction on error or cancellation. Reconcile an ambiguous commit
/// through the scoped reader.
pub async fn stage_completion(
    tx: &mut Transaction<'_>,
    scope: &CompletionScope<'_>,
    lease: &str,
    intent: &CandidateIntent,
    custody: ReadbackScope<'_>,
    verified: &VerifiedCompletedReadback<'_, '_>,
    retain_until: i64,
) -> Result<(), OutcomeError> {
    let [environment, library, object, seal, _] = intent.identities;
    if (environment, library, seal) != (scope.environment, scope.library, scope.seal)
        || u64::try_from(scope.offset).ok() != Some(intent.plaintext_len)
    {
        return Err(Refused);
    }
    tx.batch_execute(BOUND_TX)
        .await
        .map_err(|_| Indeterminate)?;
    let mut staged = tx.transaction().await.map_err(|_| Indeterminate)?;
    let result = async {
        scope.lock(&staged, Some(lease)).await?;
        stage_completed_candidate(&mut staged, intent, custody, verified, retain_until)
            .await
            .map_err(OutcomeError::Publication)?;
        staged
            .query_typed_opt(
                RECORD,
                &[
                    (&scope.session, Type::TEXT),
                    (&uuid(&intent.map_id), Type::TEXT),
                    (&lease, Type::TEXT),
                    (&retain_until, Type::INT8),
                    (&uuid(&object), Type::TEXT),
                    (&custody.bucket, Type::TEXT),
                ],
            )
            .await
            .map_err(|_| Indeterminate)?
            .ok_or(Refused)?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => staged.commit().await.map_err(|_| Indeterminate),
        Err(error) => {
            staged.rollback().await.map_err(|_| Indeterminate)?;
            Err(error)
        }
    }
}

/// Use the writer connection to lock the session and recheck live authorization under that lock.
/// Replay expires with authorization, the session or retention. A scoped absence stops completion.
pub async fn read_completion(
    db: &mut Client,
    scope: &CompletionScope<'_>,
    authorize: impl AsyncFnOnce(&Transaction<'_>, &CompletionScope<'_>) -> Result<i64, OutcomeError>,
) -> Result<Option<CommittedOutcome>, OutcomeError> {
    let tx = begin(db).await?;
    scope.lock(&tx, None).await?;
    let until = authorize(&tx, scope).await?;
    // READ COMMITTED takes a fresh snapshot after waiting on the session.
    let row = tx
        .query_typed_opt(READ, &[(&scope.session, Type::TEXT), (&until, Type::INT8)])
        .await
        .map_err(|_| Indeterminate)?
        .ok_or(Refused)?;
    let result = decode_outcome(&row).map_err(|_| Indeterminate)?;
    tx.commit().await.map_err(|_| Indeterminate)?;
    Ok(result)
}

async fn begin(db: &mut Client) -> Result<Transaction<'_>, OutcomeError> {
    let tx = db
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .start()
        .await
        .map_err(|_| Indeterminate)?;
    tx.batch_execute(BOUND_TX)
        .await
        .map_err(|_| Indeterminate)?;
    Ok(tx)
}

/// Protect the exact winner of an authorized completed session. Authenticate
/// the caller before entering, then use `authorize` for a current reuse grant
/// and its expiry after both locks. The callback can also reject incompatible
/// pins before the fence is written. Pins confer no provider access or authority
/// to bind another reference. Consumers must stop at the returned deadline.
pub async fn protect_completion_reuse(
    db: &mut Client,
    scope: &CompletionScope<'_>,
    authorize: impl AsyncFnOnce(&CompletionScope<'_>, &CommittedOutcome) -> Result<i64, OutcomeError>,
    now: impl Fn() -> SystemTime,
) -> Result<ProtectedReuse, OutcomeError> {
    let tx = begin(db).await?;
    scope.lock(&tx, None).await?;
    let row = tx
        .query_typed_opt(
            include_str!("reuse_lock.sql"),
            &[(&scope.session, Type::TEXT)],
        )
        .await
        .map_err(|_| Indeterminate)?
        .ok_or(Refused)?;
    let outcome = decode_outcome(&row)
        .map_err(|_| Indeterminate)?
        .ok_or(Refused)?;
    let until = authorize(scope, &outcome).await?;
    let usable_until = tx
        .query_typed_opt(
            include_str!("reuse_protect.sql"),
            &[
                (&scope.session, Type::TEXT),
                (&outcome.object, Type::TEXT),
                (&until, Type::INT8),
            ],
        )
        .await
        .map_err(|_| Indeterminate)?
        .ok_or(Refused)?
        .try_get(0)
        .map_err(|_| Indeterminate)?;
    tx.commit().await.map_err(|_| Indeterminate)?;
    if now() >= usable_until {
        return Err(Refused);
    }
    Ok(ProtectedReuse {
        outcome,
        usable_until,
    })
}

fn decode_outcome(row: &Row) -> Result<Option<CommittedOutcome>, tokio_postgres::Error> {
    let Some(map) = row.try_get(0)? else {
        return Ok(None);
    };
    Ok(Some(CommittedOutcome {
        map,
        object: row.try_get(1)?,
        generation: row.try_get(2)?,
        configuration: row.try_get(3)?,
        root_version: row.try_get(4)?,
        media_version: row.try_get(5)?,
        page_versions: row.try_get(6)?,
    }))
}
