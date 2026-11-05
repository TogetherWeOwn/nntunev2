//! Exact attempt acquisition with a receipt retained before the commit boundary.
//!
//! A receipt is not attempt authority. Only a committed acquisition or an exact
//! live database recovery returns `AttemptHandle`. Keep the same receipt after
//! an ambiguous commit; never replace it with a newly prepared request on retry.
//! The original preparation is in-memory. Durable preparation retains the exact
//! proposal for a trusted coordinator's stable request identity across restarts.
//! Neither path wires a Queue consumer. Queue/KV hints must not supply trusted
//! owner, request or expected-attempt identities or select the current token.

use super::*;

/// Opaque pre-commit acquisition identity, not a persisted attempt handle.
/// No token accessor or Queue deserializer is provided. Trusted callers retain
/// this value across a possibly ambiguous acquisition result; Debug is redacted.
#[derive(Clone)]
pub struct AttemptAcquisition {
    proposed: AttemptHandle,
}

impl fmt::Debug for AttemptAcquisition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AttemptAcquisition([REDACTED])")
    }
}

/// Exact dispatch coordinates retained by the trusted coordinator, not attempt
/// authority. Construct only from its durable authenticated dispatch record,
/// never from Queue/KV fields. Validation checks shape, NOT authentication.
/// There is deliberately no deserializer or current-attempt/token accessor.
#[derive(Clone)]
pub struct DurableAttemptRequest {
    job_id: String,
    owner_id: String,
    admission_key: String,
    request_id: String,
    next_attempt: i64,
}

impl DurableAttemptRequest {
    pub fn new(
        job_id: &str,
        owner_id: &str,
        admission_key: &str,
        request_id: &str,
        expected_attempt: i64,
    ) -> Result<Self, StoreError> {
        if !identity(job_id)
            || !identity(owner_id)
            || !identity(request_id)
            || expected_attempt < 0
            || crate::ingest_jobs::validate_admit_params(job_id, owner_id, admission_key, 0)
                .is_err()
        {
            return Err(StoreError::InvalidInput);
        }
        let next_attempt = expected_attempt
            .checked_add(1)
            .ok_or(StoreError::InvalidInput)?;
        Ok(Self {
            job_id: job_id.to_owned(),
            owner_id: owner_id.to_owned(),
            admission_key: admission_key.to_owned(),
            request_id: request_id.to_owned(),
            next_attempt,
        })
    }
}

impl fmt::Debug for DurableAttemptRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DurableAttemptRequest([REDACTED])")
    }
}

/// Recover an already acquired attempt after losing all in-memory receipts.
/// The trusted coordinator supplies the SAME original dispatch coordinates.
/// Missing receipts, queued/stopped/foreign jobs, expiry or supersession refuse:
/// this never prepares, starts, acquires, takes over or renews anything. In
/// particular, a rolled-back startup/acquisition must not become a new attempt
/// just because a delayed delivery arrived. Startup/acquisition remain explicit.
/// The owner and exact admitted job are locked before loading the immutable
/// receipt; `recover_locked` rechecks the live attempt and DB clock after waits.
/// This API does not authenticate a dispatch or wire the production Queue.
pub async fn recover_durable_attempt_acquisition(
    client: &mut Client,
    request: &DurableAttemptRequest,
) -> Result<AttemptOutcome, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_owner(&tx, &request.owner_id).await? {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    if tx
        .query_opt(
            "SELECT id FROM ingest_jobs WHERE id = $1 AND user_id = $2 \
             AND idempotency_key = $3 AND status = 'running' FOR UPDATE",
            &[&request.job_id, &request.owner_id, &request.admission_key],
        )
        .await
        .map_err(database_error)?
        .is_none()
    {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    let Some(row) = tx
        .query_opt(
            "SELECT token FROM posted_file_attempt_receipts \
             WHERE job_id = $1 AND request_id = $2 AND owner_id = $3 AND attempt = $4",
            &[
                &request.job_id,
                &request.request_id,
                &request.owner_id,
                &request.next_attempt,
            ],
        )
        .await
        .map_err(database_error)?
    else {
        return Ok(AttemptOutcome::StaleOrMissing);
    };
    let proposed = AttemptHandle {
        job_id: request.job_id.clone(),
        owner_id: request.owner_id.clone(),
        attempt: request.next_attempt,
        token: row.get(0),
    };
    recover_locked(tx, &proposed).await
}

/// Mint the request identity before attempting a transaction that could commit.
/// This writes nothing and establishes no job ownership, lease or authority.
/// The token uses the same database CSPRNG as the original acquisition path.
pub async fn prepare_attempt_acquisition(
    client: &Client,
    job_id: &str,
    owner_id: &str,
    expected_attempt: i64,
) -> Result<AttemptAcquisition, StoreError> {
    if !identity(job_id) || !identity(owner_id) || expected_attempt < 0 {
        return Err(StoreError::InvalidInput);
    }
    let next = expected_attempt
        .checked_add(1)
        .ok_or(StoreError::InvalidInput)?;
    let token: String = client
        .query_one("SELECT gen_random_uuid()::text", &[])
        .await
        .map_err(database_error)?
        .get(0);
    Ok(AttemptAcquisition {
        proposed: AttemptHandle {
            job_id: job_id.to_owned(),
            owner_id: owner_id.to_owned(),
            attempt: next,
            token,
        },
    })
}

/// Persist or reload an exact proposal for a trusted coordinator request.
///
/// The coordinator must retain `request_id` and `expected_attempt` in its own
/// durable dispatch contract and bind `job_id`/`owner_id` to authenticated job
/// ownership, never Queue/KV hints. This is not a consumer authentication API.
/// A restart repeats ALL four identities; there is no job-only/current-token
/// recovery. Changing the request on an ambiguous result is forbidden.
///
/// An existing receipt must match owner and next attempt exactly. It is never
/// updated, even after acquisition, expiry or supersession. A new request is
/// persisted only for the exact absent/expired predecessor. Preparation grants
/// no lease or handle authority; acquire/recover still check the live attempt.
/// `None` refuses missing, foreign, stopped, busy or mismatched work. Ambiguous
/// preparation commit is recovered by repeating this exact request, not minting.
/// Migration 0046 is required; no production coordinator/Queue is wired here.
pub async fn prepare_durable_attempt_acquisition(
    client: &mut Client,
    job_id: &str,
    owner_id: &str,
    request_id: &str,
    expected_attempt: i64,
) -> Result<Option<AttemptAcquisition>, StoreError> {
    if !identity(job_id) || !identity(owner_id) || !identity(request_id) || expected_attempt < 0 {
        return Err(StoreError::InvalidInput);
    }
    let next = expected_attempt
        .checked_add(1)
        .ok_or(StoreError::InvalidInput)?;
    let tx = client.transaction().await.map_err(database_error)?;
    // The owner deletion fence precedes every job/attempt lock.
    if !lock_owner(&tx, owner_id).await? {
        return Ok(None);
    }
    if tx
        .query_opt(
            "SELECT id FROM ingest_jobs WHERE id = $1 AND user_id = $2 AND status = 'running' FOR UPDATE",
            &[&job_id, &owner_id],
        )
        .await
        .map_err(database_error)?
        .is_none()
    {
        return Ok(None);
    }
    let receipt = prepare_durable_locked(&tx, job_id, owner_id, request_id, next).await?;
    if receipt.is_some() {
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    }
    Ok(receipt)
}

/// Start an admitted job and persist its exact proposal in one transaction.
///
/// Only a trusted coordinator may supply the authenticated owner, original
/// admission key and durable request identity. Queue/KV fields are not that
/// contract. The admitted row must match all three job/owner/admission values,
/// and its owner must pass the deletion writer fence. No row is admitted here.
///
/// A queued job starts only at attempt zero with no attempt or receipt history;
/// resetting an old job to queued cannot revive it. Running jobs reuse the same
/// exact preparation rules, including immutable receipt replay and predecessor
/// checks. A refused/erroring preparation rolls back the running transition.
/// Ambiguous commit recovery repeats this exact call. No lease/handle, candidate
/// or network authority is returned, and no production coordinator is wired.
/// The existing running-only preparation API remains unchanged.
pub async fn start_durable_attempt_acquisition(
    client: &mut Client,
    job_id: &str,
    owner_id: &str,
    admission_key: &str,
    request_id: &str,
    expected_attempt: i64,
) -> Result<Option<AttemptAcquisition>, StoreError> {
    if !identity(job_id)
        || !identity(owner_id)
        || !identity(request_id)
        || expected_attempt < 0
        || crate::ingest_jobs::validate_admit_params(job_id, owner_id, admission_key, 0).is_err()
    {
        return Err(StoreError::InvalidInput);
    }
    let next = expected_attempt
        .checked_add(1)
        .ok_or(StoreError::InvalidInput)?;
    let tx = client.transaction().await.map_err(database_error)?;
    // Match admission/terminal writers: user deletion fence before the job lock.
    if !lock_owner(&tx, owner_id).await? {
        return Ok(None);
    }
    let Some(job) = tx
        .query_opt(
            "SELECT status FROM ingest_jobs WHERE id = $1 AND user_id = $2 \
         AND idempotency_key = $3 FOR UPDATE",
            &[&job_id, &owner_id, &admission_key],
        )
        .await
        .map_err(database_error)?
    else {
        return Ok(None);
    };
    match job.get::<_, &str>("status") {
        "running" => {}
        "queued" => {
            let has_history: bool = tx
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM posted_file_job_attempts WHERE job_id = $1) \
                 OR EXISTS (SELECT 1 FROM posted_file_attempt_receipts WHERE job_id = $1)",
                    &[&job_id],
                )
                .await
                .map_err(database_error)?
                .get(0);
            if expected_attempt != 0 || has_history {
                return Ok(None);
            }
            tx.execute(
                "UPDATE ingest_jobs SET status = 'running' WHERE id = $1",
                &[&job_id],
            )
            .await
            .map_err(database_error)?;
        }
        _ => return Ok(None),
    }
    let receipt = prepare_durable_locked(&tx, job_id, owner_id, request_id, next).await?;
    if receipt.is_some() {
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    }
    Ok(receipt)
}

// Caller holds the owning job lock. This helper never commits independently of
// the caller's startup/fence transaction and never reads a current token.
async fn prepare_durable_locked(
    tx: &Transaction<'_>,
    job_id: &str,
    owner_id: &str,
    request_id: &str,
    next: i64,
) -> Result<Option<AttemptAcquisition>, StoreError> {
    let expected_attempt = next - 1;
    let existing = tx
        .query_opt(
            "SELECT owner_id, attempt, token FROM posted_file_attempt_receipts \
             WHERE job_id = $1 AND request_id = $2",
            &[&job_id, &request_id],
        )
        .await
        .map_err(database_error)?;
    let token = if let Some(row) = existing {
        if row.get::<_, String>("owner_id") != owner_id || row.get::<_, i64>("attempt") != next {
            return Ok(None);
        }
        row.get::<_, String>("token")
    } else {
        let old = tx
            .query_opt(
                "SELECT attempt FROM posted_file_job_attempts WHERE job_id = $1 FOR UPDATE",
                &[&job_id],
            )
            .await
            .map_err(database_error)?;
        if let Some(row) = old {
            if row.get::<_, i64>("attempt") != expected_attempt {
                return Ok(None);
            }
            let active: bool = tx
                .query_one(
                    "SELECT lease_until > extract(epoch FROM clock_timestamp())::bigint \
                     FROM posted_file_job_attempts WHERE job_id = $1",
                    &[&job_id],
                )
                .await
                .map_err(database_error)?
                .get(0);
            if active {
                return Ok(None);
            }
        } else if expected_attempt != 0 {
            return Ok(None);
        }
        tx.query_one(
            "INSERT INTO posted_file_attempt_receipts (job_id, request_id, owner_id, attempt, token) \
             VALUES ($1, $2, $3, $4, gen_random_uuid()::text) RETURNING token",
            &[&job_id, &request_id, &owner_id, &next],
        )
        .await
        .map_err(database_error)?
        .get(0)
    };
    Ok(Some(AttemptAcquisition {
        proposed: AttemptHandle {
            job_id: job_id.to_owned(),
            owner_id: owner_id.to_owned(),
            attempt: next,
            token,
        },
    }))
}

/// Acquire only the receipt's exact next attempt, or recover its live committed
/// result without renewal. A matching but expired result is stale, not a request
/// to acquire again. A different receipt never borrows an existing token.
/// No candidate claim, round, custody, health, schedule or budget is changed.
pub async fn acquire_prepared_attempt(
    client: &mut Client,
    receipt: &AttemptAcquisition,
) -> Result<AttemptOutcome, StoreError> {
    let a = &receipt.proposed;
    let expected = a.attempt - 1;
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_owner(&tx, &a.owner_id).await? {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    if tx
        .query_opt(
            "SELECT id FROM ingest_jobs WHERE id = $1 AND user_id = $2 AND status = 'running' FOR UPDATE",
            &[&a.job_id, &a.owner_id],
        )
        .await
        .map_err(database_error)?
        .is_none()
    {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    let old = tx
        .query_opt(
            "SELECT attempt, token FROM posted_file_job_attempts WHERE job_id = $1 FOR UPDATE",
            &[&a.job_id],
        )
        .await
        .map_err(database_error)?;
    if let Some(row) = old {
        if row.get::<_, i64>("attempt") == a.attempt && row.get::<_, String>("token") == a.token {
            return recover_locked(tx, a).await;
        }
        if row.get::<_, i64>("attempt") != expected {
            return Ok(AttemptOutcome::StaleOrMissing);
        }
        // Sample the clock after the attempt lock, not in a SELECT target that
        // could have been evaluated before waiting on an unchanged tuple.
        let active: bool = tx
            .query_one(
                "SELECT lease_until > extract(epoch FROM clock_timestamp())::bigint \
                 FROM posted_file_job_attempts WHERE job_id = $1",
                &[&a.job_id],
            )
            .await
            .map_err(database_error)?
            .get(0);
        if active {
            return Ok(AttemptOutcome::Busy);
        }
    } else if expected != 0 {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    tx.execute(
        "INSERT INTO posted_file_job_attempts (job_id, attempt, token, lease_until) \
         VALUES ($1, $2, $3, extract(epoch FROM clock_timestamp())::bigint + $4) \
         ON CONFLICT (job_id) DO UPDATE SET attempt = EXCLUDED.attempt, \
             token = EXCLUDED.token, lease_until = EXCLUDED.lease_until",
        &[&a.job_id, &a.attempt, &a.token, &LEASE_SECONDS],
    )
    .await
    .map_err(database_error)?;
    recover_locked(tx, a).await
}

/// Read-only lost-ack recovery using the original pre-commit receipt. Absence,
/// expiry, stopped/foreign jobs or token/attempt drift refuse; this never falls
/// back to acquisition, takeover, a current-token lookup or a lease extension.
/// As with every returned handle, revalidate immediately before network work.
pub async fn recover_attempt_acquisition(
    client: &mut Client,
    receipt: &AttemptAcquisition,
) -> Result<AttemptOutcome, StoreError> {
    let a = &receipt.proposed;
    let tx = client.transaction().await.map_err(database_error)?;
    // Match acquisition's owner-before-job-before-attempt lock order.
    if !lock_owner(&tx, &a.owner_id).await? {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    if tx
        .query_opt(
            "SELECT id FROM ingest_jobs WHERE id = $1 AND user_id = $2 AND status = 'running' FOR UPDATE",
            &[&a.job_id, &a.owner_id],
        )
        .await
        .map_err(database_error)?
        .is_none()
    {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    recover_locked(tx, a).await
}

async fn recover_locked(
    tx: Transaction<'_>,
    a: &AttemptHandle,
) -> Result<AttemptOutcome, StoreError> {
    if !lock_attempt(&tx, a).await? {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    // lock_attempt can wait behind either row with a SELECT target/predicate
    // evaluated before that wait. Both rows are now held: sample all conditions
    // and the database clock once more without any remaining row-lock wait.
    let current: bool = tx
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM ingest_jobs j \
             JOIN posted_file_job_attempts a ON a.job_id = j.id \
             WHERE j.id = $1 AND j.user_id = $2 AND j.status = 'running' \
             AND a.attempt = $3 AND a.token = $4 \
             AND a.lease_until > extract(epoch FROM clock_timestamp())::bigint)",
            &[&a.job_id, &a.owner_id, &a.attempt, &a.token],
        )
        .await
        .map_err(database_error)?
        .get(0);
    if !current {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(AttemptOutcome::Acquired(a.clone()))
}
