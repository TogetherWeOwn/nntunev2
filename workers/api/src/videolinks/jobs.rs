//! App-only retention ownership and bounded latest-state receipts.
//!
//! No Secret, mirror connection or HTTP startup configuration is needed here.
//! Scheduled entry, connection/outer deadlines and operational alerts remain the
//! caller's responsibility. A missing completion receipt is unknown, not success.

use super::retention::{CleanupError, CleanupFailure, CleanupReport, MAX_CLEANUP_BATCHES};
use super::validation::MAX_PURGES_PER_CALL;
use tokio_postgres::Client;

/// Longer than the intended outer invocation budget; not a work/retry budget.
const LEASE_SECONDS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobEnvironment {
    Staging,
    Production,
}

impl JobEnvironment {
    fn label(self) -> &'static str {
        match self {
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobError {
    InvalidInput,
    Store,
    LostLease,
    Deadline,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RetentionTick {
    Suppressed,
    Recorded(Result<CleanupReport, CleanupFailure>),
}

/// One attempt: claim, bounded cleanup/audit, then a fenced durable receipt.
/// The 20-second budget includes ownership and receipt work, but not caller-owned
/// connection acquisition. No retry/drain loop. The eventual scheduled caller
/// must also bound connection acquisition and alert on any error/missing receipt.
pub async fn cleanup_tick(
    app_client: &mut Client,
    environment: JobEnvironment,
    scheduled_at: i64,
    build_sha: &str,
    clock: impl Fn() -> i64,
) -> Result<RetentionTick, JobError> {
    let budget = std::time::Duration::from_secs(20);
    #[cfg(target_arch = "wasm32")]
    let deadline = worker::Delay::from(budget);
    #[cfg(not(target_arch = "wasm32"))]
    let deadline = futures_timer::Delay::new(budget);
    let mut deadline = std::pin::pin!(deadline);
    let operation = async {
        let started_at = clock();
        let Some(lease) = PgRetentionJobs::new(app_client)
            .claim(environment, scheduled_at, started_at, build_sha)
            .await?
        else {
            return Ok(RetentionTick::Suppressed);
        };
        let result = super::retention::cleanup_metadata(
            &mut super::retention::PgRetentionStore::new(app_client),
            started_at,
        )
        .await;
        PgRetentionJobs::new(app_client)
            .finish(&lease, clock(), &result)
            .await?;
        Ok(RetentionTick::Recorded(result))
    };
    let mut operation = std::pin::pin!(operation);
    std::future::poll_fn(|cx| {
        use std::future::Future;
        if deadline.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(Err(JobError::Deadline));
        }
        operation.as_mut().poll(cx)
    })
    .await
}

/// A fencing token, not a session lock. The database clock owns lease expiry.
#[derive(Debug, PartialEq, Eq)]
pub struct RetentionLease {
    environment: JobEnvironment,
    version: i64,
    scheduled_at: i64,
    started_at: i64,
}

/// App HYPERDRIVE only. Each method uses a short transaction and SQL timeout.
/// Only two environment keys exist; receipts replace the previous receipt rather
/// than accumulating an unbounded per-delivery log. They are not an alert queue.
pub struct PgRetentionJobs<'a> {
    client: &'a mut Client,
}

impl<'a> PgRetentionJobs<'a> {
    pub fn new(app_client: &'a mut Client) -> Self {
        Self { client: app_client }
    }

    /// None means busy, an obsolete delivery, or a completed duplicate — never
    /// proof that cleanup ran. Unknown commit acknowledgements are not retried
    /// in this invocation. A missing receipt allows replay after lease expiry.
    pub async fn claim(
        &mut self,
        environment: JobEnvironment,
        scheduled_at: i64,
        started_at: i64,
        build_sha: &str,
    ) -> Result<Option<RetentionLease>, JobError> {
        if scheduled_at <= 0
            || started_at < scheduled_at
            || build_sha.len() != 40
            || !build_sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(JobError::InvalidInput);
        }
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| JobError::Store)?;
        tx.batch_execute("SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| JobError::Store)?;
        let row = tx
            .query_opt(
                CLAIM_SQL,
                &[
                    &environment.label(),
                    &scheduled_at,
                    &started_at,
                    &build_sha,
                    &LEASE_SECONDS,
                ],
            )
            .await
            .map_err(|_| JobError::Store)?;
        let lease = row
            .map(|row| -> Result<RetentionLease, JobError> {
                Ok(RetentionLease {
                    environment,
                    version: row.try_get(0).map_err(|_| JobError::Store)?,
                    scheduled_at,
                    started_at,
                })
            })
            .transpose()?;
        tx.commit().await.map_err(|_| JobError::Store)?;
        Ok(lease)
    }

    /// Atomically save the sanitized report and release this exact live lease.
    /// A stale/expired worker cannot replace a newer receipt. This fence does
    /// not cancel an already-running purge; those writes remain idempotent and
    /// bounded, and a later invocation audits the actual database again.
    pub async fn finish(
        &mut self,
        lease: &RetentionLease,
        finished_at: i64,
        result: &Result<CleanupReport, CleanupFailure>,
    ) -> Result<(), JobError> {
        let (report, outcome, failure) = receipt(result)?;
        if finished_at < lease.started_at || report.evaluated_at != lease.started_at {
            return Err(JobError::InvalidInput);
        }
        let json = serde_json::to_string(report).map_err(|_| JobError::InvalidInput)?;
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| JobError::Store)?;
        tx.batch_execute("SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| JobError::Store)?;
        // Lock/recheck ownership first; receipt insertion and release commit
        // together. No receipt is manufactured if either statement fails.
        let row = tx
            .query_opt(
                "UPDATE public.video_retention_jobs SET lease_until = 0, completed_version = version
                 WHERE environment = $1 AND version = $2 AND scheduled_at = $3
                   AND lease_until > floor(extract(epoch FROM clock_timestamp()))::bigint
                 RETURNING build_sha, started_at",
                &[&lease.environment.label(), &lease.version, &lease.scheduled_at],
            )
            .await
            .map_err(|_| JobError::Store)?
            .ok_or(JobError::LostLease)?;
        let build_sha: String = row.try_get(0).map_err(|_| JobError::Store)?;
        let started_at: i64 = row.try_get(1).map_err(|_| JobError::Store)?;
        tx.execute(
            "INSERT INTO public.video_retention_receipts
             (environment, version, scheduled_at, build_sha, started_at, finished_at,
              outcome, failure, report)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::jsonb)
             ON CONFLICT (environment) DO UPDATE SET
               version = EXCLUDED.version, scheduled_at = EXCLUDED.scheduled_at,
               build_sha = EXCLUDED.build_sha, started_at = EXCLUDED.started_at,
               finished_at = EXCLUDED.finished_at, outcome = EXCLUDED.outcome,
               failure = EXCLUDED.failure, report = EXCLUDED.report",
            &[
                &lease.environment.label(),
                &lease.version,
                &lease.scheduled_at,
                &build_sha,
                &started_at,
                &finished_at,
                &outcome,
                &failure,
                &json,
            ],
        )
        .await
        .map_err(|_| JobError::Store)?;
        tx.commit().await.map_err(|_| JobError::Store)
    }
}

const CLAIM_SQL: &str = "\
WITH wall AS MATERIALIZED (
    SELECT floor(extract(epoch FROM clock_timestamp()))::bigint AS now
)
INSERT INTO public.video_retention_jobs AS job
    (environment, version, scheduled_at, build_sha, started_at, lease_until)
SELECT $1, 1, $2, $4, $3, wall.now + $5 FROM wall
ON CONFLICT (environment) DO UPDATE SET
    version = job.version + 1, scheduled_at = EXCLUDED.scheduled_at,
    build_sha = EXCLUDED.build_sha, started_at = EXCLUDED.started_at,
    lease_until = EXCLUDED.lease_until
WHERE job.lease_until <= (SELECT now FROM wall)
  AND (EXCLUDED.scheduled_at > job.scheduled_at
       OR (EXCLUDED.scheduled_at = job.scheduled_at AND job.completed_version < job.version))
RETURNING version";

/// A successful audit with backlog is not healthy retention. Do not persist
/// opaque errors or API fields: this exhaustive mapping permits fixed labels only.
fn receipt(
    result: &Result<CleanupReport, CleanupFailure>,
) -> Result<(&CleanupReport, &'static str, Option<&'static str>), JobError> {
    let (report, failure) = match result {
        Ok(report) => (report, None),
        Err(failure) => (
            &failure.progress,
            Some(match failure.error {
                CleanupError::InvalidTime => "invalid_time",
                CleanupError::InvalidReceipt => "invalid_receipt",
                CleanupError::Store => "store",
                CleanupError::Deadline => "deadline",
            }),
        ),
    };
    if report.evaluated_at <= 0
        || report.safety_window_s != super::retention::CLEANUP_SAFETY_WINDOW_S
        || report.batches > MAX_CLEANUP_BATCHES
        || report.purged_rows > (report.batches * MAX_PURGES_PER_CALL) as u64
    {
        return Err(JobError::InvalidInput);
    }
    if let Some(backlog) = &report.backlog {
        if backlog.expired_rows > backlog.due_rows
            || backlog.due_rows > backlog.retained_rows
            || (backlog.due_rows == 0) != backlog.oldest_due_validated_at.is_none()
            || (backlog.expired_rows == 0) != backlog.oldest_expired_validated_at.is_none()
        {
            return Err(JobError::InvalidInput);
        }
    }
    let outcome = if failure.is_some() {
        "cleanup_failed"
    } else {
        let backlog = report.backlog.as_ref().ok_or(JobError::InvalidInput)?;
        if backlog.expired_rows > 0 {
            "expired_backlog"
        } else if backlog.due_rows > 0 {
            "due_backlog"
        } else {
            "drained"
        }
    };
    Ok((report, outcome, failure))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::videolinks::retention::{MetadataBacklog, CLEANUP_SAFETY_WINDOW_S};

    fn report() -> CleanupReport {
        CleanupReport {
            evaluated_at: 1_700_000_000,
            safety_window_s: CLEANUP_SAFETY_WINDOW_S,
            batches: 1,
            purged_rows: 0,
            backlog: Some(MetadataBacklog {
                retained_rows: 10,
                due_rows: 0,
                oldest_due_validated_at: None,
                expired_rows: 0,
                oldest_expired_validated_at: None,
            }),
        }
    }

    #[test]
    fn outcomes_distinguish_due_expired_failure_and_missing_audit() {
        let mut result = Ok(report());
        assert_eq!(receipt(&result).unwrap().1, "drained");
        let backlog = result.as_mut().unwrap().backlog.as_mut().unwrap();
        backlog.due_rows = 2;
        backlog.oldest_due_validated_at = Some(0);
        assert_eq!(receipt(&result).unwrap().1, "due_backlog");
        let backlog = result.as_mut().unwrap().backlog.as_mut().unwrap();
        backlog.expired_rows = 1;
        backlog.oldest_expired_validated_at = Some(-1);
        assert_eq!(receipt(&result).unwrap().1, "expired_backlog");
        result.as_mut().unwrap().backlog = None;
        assert_eq!(receipt(&result), Err(JobError::InvalidInput));
        result = Err(CleanupFailure {
            error: CleanupError::Deadline,
            progress: report(),
        });
        assert_eq!(receipt(&result).unwrap().1, "cleanup_failed");
        assert_eq!(receipt(&result).unwrap().2, Some("deadline"));
    }

    #[test]
    fn invalid_reports_never_become_durable_success() {
        let mut invalid = report();
        invalid.purged_rows = 201;
        assert_eq!(receipt(&Ok(invalid)), Err(JobError::InvalidInput));
        let mut invalid = report();
        invalid.backlog.as_mut().unwrap().due_rows = 1;
        assert_eq!(receipt(&Ok(invalid)), Err(JobError::InvalidInput));
        let mut invalid = report();
        invalid.batches = MAX_CLEANUP_BATCHES + 1;
        assert_eq!(receipt(&Ok(invalid)), Err(JobError::InvalidInput));
    }
}
