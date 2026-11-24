use tokio_postgres::{Client, IsolationLevel, Transaction};

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

impl CompletionScope<'_> {
    async fn lock(&self, tx: &Transaction<'_>, lease: Option<&str>) -> Result<(), OutcomeError> {
        tx.query_opt(
            LOCK,
            &[
                &uuid(&self.environment),
                &uuid(&self.library),
                &self.principal,
                &self.session,
                &self.fingerprint.as_slice(),
                &uuid(&self.seal),
                &self.revision,
                &self.offset,
                &lease,
            ],
        )
        .await
        .map_err(|_| Indeterminate)?
        .map(|_| ())
        .ok_or(Refused)
    }
}

/// Stage as the first operation of a dedicated READ COMMITTED transaction.
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
            .query_opt(
                RECORD,
                &[
                    &scope.session,
                    &uuid(&intent.map_id),
                    &lease,
                    &retain_until,
                    &uuid(&object),
                    &custody.bucket,
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
    let tx = db
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .start()
        .await
        .map_err(|_| Indeterminate)?;
    tx.batch_execute(BOUND_TX)
        .await
        .map_err(|_| Indeterminate)?;
    scope.lock(&tx, None).await?;
    let until = authorize(&tx, scope).await?;
    // READ COMMITTED takes a fresh snapshot after waiting on the session.
    let row = tx
        .query_opt(READ, &[&scope.session, &until])
        .await
        .map_err(|_| Indeterminate)?
        .ok_or(Refused)?;
    let map: Option<String> = row.try_get(0).map_err(|_| Indeterminate)?;
    let result = map
        .map(|map| {
            Ok::<_, tokio_postgres::Error>(CommittedOutcome {
                map,
                object: row.try_get(1)?,
                generation: row.try_get(2)?,
                configuration: row.try_get(3)?,
                root_version: row.try_get(4)?,
                media_version: row.try_get(5)?,
                page_versions: row.try_get(6)?,
            })
        })
        .transpose()
        .map_err(|_| Indeterminate)?;
    tx.commit().await.map_err(|_| Indeterminate)?;
    Ok(result)
}
