//! Durable propagation/backoff state. This schedules bounded work, not network
//! exactly-once execution or conclusive negative health. No transaction spans a
//! STAT/repair pass. Callbacks use the original pass permit plus the refreshed
//! current handle; recovery never reconstructs a permit for an old callback.

use super::*;
use crate::two_spool::{recheck_delay_ms, MAX_RECHECKS, PROPAGATION_DELAY_MS};

/// Only scheduling scalars; the containing checkpoint still redacts its Debug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecheckSchedule {
    Waiting {
        completed_passes: u32,
        not_before: i64,
    },
    Exhausted,
    /// A pre-migration round has no durable pass history. Do not reset its budget.
    Untracked,
}

#[derive(Clone)]
pub struct RecheckPermit {
    handle: GenerationHandle,
    completed_passes: u32,
    pub(super) cursor: i32,
}

impl fmt::Debug for RecheckPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecheckPermit([REDACTED])")
    }
}

#[derive(Debug)]
pub enum RecheckPreparation {
    Ready(Box<RecheckPermit>),
    Waiting { not_before: i64 },
    Exhausted,
    Untracked,
    StaleOrDiverged,
}

pub(super) async fn start_round(
    tx: &Transaction<'_>,
    handle: &GenerationHandle,
) -> Result<(), StoreError> {
    let inserted = tx
        .execute(
            "INSERT INTO posted_file_recheck_schedule \
             (generation_id, round, state, not_before) \
             SELECT generation_id, round, 'waiting', \
             ceil(extract(epoch FROM clock_timestamp()))::bigint + $2 \
             FROM posted_file_health WHERE generation_id = $1 AND round > 0",
            &[&handle.generation, &((PROPAGATION_DELAY_MS / 1000) as i64)],
        )
        .await
        .map_err(database_error)?;
    if inserted != 1 {
        return Err(StoreError::Inconsistent);
    }
    Ok(())
}

pub(super) async fn load_schedule(
    tx: &Transaction<'_>,
    handle: &GenerationHandle,
) -> Result<Option<RecheckSchedule>, StoreError> {
    load_generation_schedule(tx, &handle.generation, handle.round).await
}

pub(super) async fn load_generation_schedule(
    tx: &Transaction<'_>,
    generation: &str,
    round: i64,
) -> Result<Option<RecheckSchedule>, StoreError> {
    if round == 0 {
        return Ok(None);
    }
    let row = tx
        .query_opt(
            "SELECT completed_passes, state, not_before FROM posted_file_recheck_schedule \
             WHERE generation_id = $1 AND round = $2",
            &[&generation, &round],
        )
        .await
        .map_err(database_error)?
        .ok_or(StoreError::Inconsistent)?;
    let completed: i32 = row.get("completed_passes");
    let completed_passes = u32::try_from(completed).map_err(|_| StoreError::Inconsistent)?;
    let not_before: Option<i64> = row.get("not_before");
    match (row.get::<_, &str>("state"), completed_passes, not_before) {
        ("waiting", n, Some(not_before)) if n < MAX_RECHECKS => {
            Ok(Some(RecheckSchedule::Waiting {
                completed_passes,
                not_before,
            }))
        }
        ("exhausted", MAX_RECHECKS, None) => Ok(Some(RecheckSchedule::Exhausted)),
        ("untracked", 0, None) => Ok(Some(RecheckSchedule::Untracked)),
        _ => Err(StoreError::Inconsistent),
    }
}

async fn db_now(tx: &Transaction<'_>) -> Result<i64, StoreError> {
    Ok(tx
        .query_one(
            "SELECT floor(extract(epoch FROM clock_timestamp()))::bigint",
            &[],
        )
        .await
        .map_err(database_error)?
        .get(0))
}

fn unfinished(current: &Row, handle: &GenerationHandle) -> bool {
    handle.round > 0
        && current.get::<_, &str>("state") == "verifying"
        && current.get::<_, Option<i64>>("verified_at").is_none()
}

/// Prepare a due missing-set pass from the durable schedule, after the normal
/// job/attempt/claim/round/revision and DB-clock lease checks. No counters change.
/// Parallel deliveries can obtain the same permit; only one completion commits.
/// This is not a physical-network exactly-once claim or replacement permission.
pub async fn prepare_recheck(
    client: &mut Client,
    handle: &GenerationHandle,
) -> Result<RecheckPreparation, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(RecheckPreparation::StaleOrDiverged);
    };
    if !unfinished(&current, handle) {
        return Ok(RecheckPreparation::StaleOrDiverged);
    }
    let cursor: i32 = tx
        .query_one(
            "SELECT work_cursor FROM posted_file_recheck_schedule \
             WHERE generation_id = $1 AND round = $2",
            &[&handle.generation, &handle.round],
        )
        .await
        .map_err(database_error)?
        .get(0);
    if !(0..=current.get::<_, i32>("article_count") * 2).contains(&cursor) {
        return Err(StoreError::Inconsistent);
    }
    let outcome = match load_schedule(&tx, handle).await? {
        Some(RecheckSchedule::Waiting {
            completed_passes,
            not_before,
        }) => {
            if db_now(&tx).await? < not_before {
                RecheckPreparation::Waiting { not_before }
            } else {
                RecheckPreparation::Ready(Box::new(RecheckPermit {
                    handle: handle.clone(),
                    completed_passes,
                    cursor,
                }))
            }
        }
        Some(RecheckSchedule::Exhausted) => RecheckPreparation::Exhausted,
        Some(RecheckSchedule::Untracked) => RecheckPreparation::Untracked,
        None => return Err(StoreError::Inconsistent),
    };
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(outcome)
}

/// Complete one bounded pass with its ORIGINAL permit and the current refreshed
/// handle after per-slot confirmations/accounting. Only the health revision may
/// have advanced within the pass. Takeover/new round/claim changes retire it.
/// The persisted pass ordinal also prevents duplicate callback replay using a
/// recovered newer handle. An unknown commit must recover/load, never remint.
///
/// Transport-only passes can reach the scheduling cap, but exhaustion writes NO
/// negative health, presence, content, retention or serving/selection authority.
/// The verifier owns provenance and keep-original/retry policy, not this timer.
pub async fn complete_recheck(
    client: &mut Client,
    handle: &GenerationHandle,
    permit: &RecheckPermit,
) -> Result<MutationOutcome, StoreError> {
    let mut expected = permit.handle.clone();
    expected.health_revision = handle.health_revision;
    if &expected != handle {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(MutationOutcome::StaleOrDiverged);
    };
    if !unfinished(&current, handle) {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    let Some(RecheckSchedule::Waiting {
        completed_passes,
        not_before,
    }) = load_schedule(&tx, handle).await?
    else {
        return Ok(MutationOutcome::StaleOrDiverged);
    };
    if completed_passes != permit.completed_passes || db_now(&tx).await? < not_before {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    let next = completed_passes + 1;
    let exhausted = next == MAX_RECHECKS;
    let delay = (recheck_delay_ms(completed_passes) / 1000) as i64;
    let changed = tx
        .execute(
            "UPDATE posted_file_recheck_schedule SET completed_passes = $3, work_cursor = 0, \
             state = CASE WHEN $4 THEN 'exhausted' ELSE 'waiting' END, \
             not_before = CASE WHEN $4 THEN NULL ELSE \
                 ceil(extract(epoch FROM clock_timestamp()))::bigint + $5 END \
             WHERE generation_id = $1 AND round = $2 AND completed_passes = $6 AND state = 'waiting'",
            &[
                &handle.generation,
                &handle.round,
                &(next as i32),
                &exhausted,
                &delay,
                &(completed_passes as i32),
            ],
        )
        .await
        .map_err(database_error)?;
    if changed != 1 {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    tx.execute(
        "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
        &[&handle.generation],
    )
    .await
    .map_err(database_error)?;
    commit_mutation(tx, handle).await
}

/// Advance exactly one slot/side after its callbacks. A crash before this commit
/// repeats only same-ID work; a crash after it resumes at the next cursor. The
/// revision bump retires even idempotent network permits from the prior step.
/// Pass completion and cursor reset share the existing completion transaction.
pub(super) async fn advance_cursor(
    client: &mut Client,
    handle: &GenerationHandle,
    permit: &RecheckPermit,
    cursor: i32,
) -> Result<MutationOutcome, StoreError> {
    let mut expected = permit.handle.clone();
    expected.health_revision = handle.health_revision;
    if &expected != handle {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(MutationOutcome::StaleOrDiverged);
    };
    if !unfinished(&current, handle)
        || !(0..current.get::<_, i32>("article_count") * 2).contains(&cursor)
    {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    let changed = tx
        .execute(
            "UPDATE posted_file_recheck_schedule SET work_cursor = work_cursor + 1 \
             WHERE generation_id = $1 AND round = $2 AND completed_passes = $3 \
             AND state = 'waiting' AND work_cursor = $4 \
             AND not_before <= floor(extract(epoch FROM clock_timestamp()))::bigint",
            &[
                &handle.generation,
                &handle.round,
                &(permit.completed_passes as i32),
                &cursor,
            ],
        )
        .await
        .map_err(database_error)?;
    if changed != 1 {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    tx.execute(
        "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
        &[&handle.generation],
    )
    .await
    .map_err(database_error)?;
    commit_mutation(tx, handle).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recheck_permit_debug_redacts_all_fences() {
        let permit = RecheckPermit {
            handle: GenerationHandle {
                attempt: AttemptHandle {
                    job_id: "private-job".into(),
                    owner_id: "private-owner".into(),
                    attempt: 3,
                    token: "private-attempt-token".into(),
                },
                operation: "private-operation".into(),
                generation: "private-generation".into(),
                claim_token: "private-claim-token".into(),
                registry_revision: 2,
                predecessor: None,
                predecessor_health_revision: None,
                health_revision: 1,
                round: 7,
                round_token: "private-round-token".into(),
            },
            completed_passes: 1,
            cursor: 2,
        };
        assert_eq!(format!("{permit:?}"), "RecheckPermit([REDACTED])");
        assert_eq!(
            format!("{:?}", RecheckPreparation::Ready(Box::new(permit))),
            "Ready(RecheckPermit([REDACTED]))"
        );
    }

    #[test]
    fn scheduler_schema_and_second_precision_match_existing_policy() {
        assert_eq!(MAX_RECHECKS, 5);
        assert_eq!(PROPAGATION_DELAY_MS % 1000, 0);
        for pass in 0..MAX_RECHECKS {
            assert_eq!(recheck_delay_ms(pass) % 1000, 0);
        }
    }
}
