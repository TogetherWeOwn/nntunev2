//! Credential-independent metadata cleanup and backlog receipts.
//!
//! The caller still owns the app connection, trusted clock, schedule and
//! receipt/alert delivery. A point-in-time audit is not a retention guarantee.

use super::validation::{PgValidationStore, ValidationError, ValidationStore, MAX_PURGES_PER_CALL};
use super::VALIDATION_TTL_S;
use serde::Serialize;
use std::future::Future;
use std::task::Poll;
use std::time::Duration;
use tokio_postgres::Client;

pub const MAX_CLEANUP_BATCHES: usize = 4;
const CLEANUP_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupError {
    InvalidTime,
    InvalidReceipt,
    Store,
    Deadline,
}

/// Counts recording/video rows, not unique IDs. Includes locked rows visible
/// in the statement snapshot; reads never use SKIP LOCKED. Invalid timestamps
/// remain literal values (including zero), not evidence of a known metadata age.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetadataBacklog {
    pub retained_rows: u64,
    pub expired_rows: u64,
    pub oldest_expired_validated_at: Option<i64>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct CleanupReport {
    /// Trusted invocation clock used for every expiry comparison, not the
    /// wall-clock completion time of the audit or a durable scheduler receipt.
    pub evaluated_at: i64,
    pub batches: usize,
    pub purged_rows: u64,
    pub backlog: Option<MetadataBacklog>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CleanupFailure {
    pub error: CleanupError,
    pub progress: CleanupReport,
}

#[allow(async_fn_in_trait)]
pub trait RetentionStore {
    /// Acknowledged writes only, at most MAX_PURGES_PER_CALL. A failed commit
    /// can have an unknown outcome. Zero does not establish an empty backlog.
    async fn purge_metadata(&mut self, now: i64) -> Result<u64, CleanupError>;
    async fn metadata_backlog(&mut self, now: i64) -> Result<MetadataBacklog, CleanupError>;
}

/// At most four 200-row purge calls followed by an independent backlog audit.
/// Stops issuing writes on zero, but always audits before returning success.
/// No YouTube client/credential is needed. Failures retain earlier acknowledged
/// progress without retrying, and never invent a successful/empty audit.
pub async fn cleanup_metadata(
    store: &mut impl RetentionStore,
    now: i64,
) -> Result<CleanupReport, CleanupFailure> {
    cleanup_with_budget(store, now, CLEANUP_DEADLINE).await
}

async fn cleanup_with_budget(
    store: &mut impl RetentionStore,
    now: i64,
    budget: Duration,
) -> Result<CleanupReport, CleanupFailure> {
    let mut progress = CleanupReport {
        evaluated_at: now,
        batches: 0,
        purged_rows: 0,
        backlog: None,
    };
    let result = if now <= 0 {
        Err(CleanupError::InvalidTime)
    } else {
        with_deadline(budget, async {
            for _ in 0..MAX_CLEANUP_BATCHES {
                let rows = store.purge_metadata(now).await?;
                if rows > MAX_PURGES_PER_CALL as u64 {
                    return Err(CleanupError::InvalidReceipt);
                }
                progress.batches += 1;
                progress.purged_rows += rows;
                if rows == 0 {
                    break;
                }
            }
            progress.backlog = Some(store.metadata_backlog(now).await?);
            Ok(())
        })
        .await
    };
    match result {
        Ok(()) => Ok(progress),
        Err(error) => Err(CleanupFailure { error, progress }),
    }
}

async fn with_deadline<T>(
    budget: Duration,
    operation: impl Future<Output = Result<T, CleanupError>>,
) -> Result<T, CleanupError> {
    #[cfg(target_arch = "wasm32")]
    let deadline = worker::Delay::from(budget);
    #[cfg(not(target_arch = "wasm32"))]
    let deadline = futures_timer::Delay::new(budget);
    let mut deadline = std::pin::pin!(deadline);
    let mut operation = std::pin::pin!(operation);
    std::future::poll_fn(|cx| {
        if deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(CleanupError::Deadline));
        }
        operation.as_mut().poll(cx)
    })
    .await
}

/// App-side only, never the mirror's MB_DB. Query results contain only aggregate
/// counts/timestamps, not API display fields, IDs, raw URLs or credentials.
pub struct PgRetentionStore<'a> {
    client: &'a mut Client,
}

impl<'a> PgRetentionStore<'a> {
    pub fn new(app_client: &'a mut Client) -> Self {
        Self { client: app_client }
    }
}

// Keep eligibility aligned with validation::PURGE_EXPIRED_SQL, including
// API-derived terminal states even when their display fields are all absent.
const BACKLOG_SQL: &str = "\
SELECT count(*), count(*) FILTER (WHERE expired),
       min(validated_at) FILTER (WHERE expired)
FROM (
    SELECT validated_at, (expires_at <= $1 OR validated_at <= 0 OR validated_at <= $2) AS expired
    FROM public.video_link_candidates
    WHERE status IN ('validated', 'rejected') OR title IS NOT NULL
       OR channel_id IS NOT NULL OR duration_secs IS NOT NULL OR embeddable IS NOT NULL
) AS retained";

impl RetentionStore for PgRetentionStore<'_> {
    async fn purge_metadata(&mut self, now: i64) -> Result<u64, CleanupError> {
        PgValidationStore::new(self.client)
            .purge_expired(now)
            .await
            .map_err(|error| match error {
                ValidationError::InvalidTime => CleanupError::InvalidTime,
                _ => CleanupError::Store,
            })
    }

    /// One read-only aggregate statement with a 1.5-second server timeout. It
    /// may scan the table: fixed result size is not a bound on scan work. A
    /// timeout is an error, never a zero-backlog receipt. Dropping the waiter
    /// does not guarantee server cancellation; row locks do not hide backlog.
    async fn metadata_backlog(&mut self, now: i64) -> Result<MetadataBacklog, CleanupError> {
        if now <= 0 {
            return Err(CleanupError::InvalidTime);
        }
        let oldest = now - VALIDATION_TTL_S;
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| CleanupError::Store)?;
        tx.batch_execute("SET TRANSACTION READ ONLY; SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| CleanupError::Store)?;
        let row = tx
            .query_one(BACKLOG_SQL, &[&now, &oldest])
            .await
            .map_err(|_| CleanupError::Store)?;
        let retained: i64 = row.try_get(0).map_err(|_| CleanupError::Store)?;
        let expired: i64 = row.try_get(1).map_err(|_| CleanupError::Store)?;
        let backlog = MetadataBacklog {
            retained_rows: retained.try_into().map_err(|_| CleanupError::Store)?,
            expired_rows: expired.try_into().map_err(|_| CleanupError::Store)?,
            oldest_expired_validated_at: row.try_get(2).map_err(|_| CleanupError::Store)?,
        };
        tx.commit().await.map_err(|_| CleanupError::Store)?;
        Ok(backlog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Wake, Waker};

    struct ThreadWake(std::thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct PendingAudit {
        dropped: Arc<AtomicBool>,
        purges: usize,
    }

    impl RetentionStore for PendingAudit {
        async fn purge_metadata(&mut self, _: i64) -> Result<u64, CleanupError> {
            self.purges += 1;
            Ok(if self.purges == 1 { 200 } else { 0 })
        }

        async fn metadata_backlog(&mut self, _: i64) -> Result<MetadataBacklog, CleanupError> {
            let _guard = Dropped(self.dropped.clone());
            std::future::pending().await
        }
    }

    #[test]
    fn deadline_cancels_audit_without_losing_acknowledged_progress() {
        let dropped = Arc::new(AtomicBool::new(false));
        let mut store = PendingAudit {
            dropped: dropped.clone(),
            purges: 0,
        };
        let mut future =
            std::pin::pin!(cleanup_with_budget(&mut store, 1, Duration::from_millis(5)));
        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let limit = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
                let failure = result.unwrap_err();
                assert_eq!(failure.error, CleanupError::Deadline);
                assert_eq!(
                    (failure.progress.batches, failure.progress.purged_rows),
                    (2, 200)
                );
                assert!(failure.progress.backlog.is_none());
                break;
            }
            assert!(std::time::Instant::now() < limit, "deadline must fire");
            std::thread::park_timeout(Duration::from_millis(20));
        }
        assert!(dropped.load(Ordering::SeqCst));
    }
}
