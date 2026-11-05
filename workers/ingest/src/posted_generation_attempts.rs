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
    // All preparation/acquisition paths serialize on the owning job first.
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
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
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
    // Match acquisition's job-before-attempt lock order.
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
