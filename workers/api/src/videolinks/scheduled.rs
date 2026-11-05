//! Minimal scheduled retention boundary. No HTTP/auth startup, Secret or mirror.
//! This entry does not install triggers or establish deployed retention health.

use super::jobs::{JobEnvironment, JobError, RetentionTick};
use std::future::Future;

pub const RETENTION_CRON: &str = "*/15 * * * *";
/// Includes app connection acquisition plus the tick's 20-second attempt.
const INVOCATION_TIMEOUT_MS: u64 = 40_000;
const TICK_MS: f64 = 15.0 * 60.0 * 1000.0;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    UnknownCron,
    InvalidTime,
    Configuration,
    Connect,
    Job(JobError),
    Deadline,
}

pub struct RetentionInvocation {
    pub environment: JobEnvironment,
    pub scheduled_at: i64,
    pub build_sha: String,
}

/// Validate trusted event/configuration before constructing any I/O. The runtime
/// supplies a vars-only lookup; absent authentication/validation secrets cannot
/// suppress cleanup. A single deadline owns connection, claim, work and receipt.
/// Delayed deliveries retain their original tick identity; future/off-grid events
/// fail closed. The job lease handles obsolete/overlapping/completed delivery.
pub async fn run_retention<F: Future<Output = Result<RetentionTick, ScheduleError>>>(
    cron: &str,
    scheduled_ms: f64,
    now_ms: u64,
    lookup: impl FnMut(&str) -> Option<String>,
    operation: impl FnOnce(RetentionInvocation) -> F,
) -> Result<RetentionTick, ScheduleError> {
    run_with_budget(
        cron,
        scheduled_ms,
        now_ms,
        lookup,
        operation,
        INVOCATION_TIMEOUT_MS,
    )
    .await
}

async fn run_with_budget<F: Future<Output = Result<RetentionTick, ScheduleError>>>(
    cron: &str,
    scheduled_ms: f64,
    now_ms: u64,
    mut lookup: impl FnMut(&str) -> Option<String>,
    operation: impl FnOnce(RetentionInvocation) -> F,
    timeout_ms: u64,
) -> Result<RetentionTick, ScheduleError> {
    if cron != RETENTION_CRON {
        return Err(ScheduleError::UnknownCron);
    }
    if !scheduled_ms.is_finite()
        || scheduled_ms <= 0.0
        || scheduled_ms > MAX_SAFE_INTEGER
        || scheduled_ms % TICK_MS != 0.0
        || now_ms == 0
        || now_ms > MAX_SAFE_INTEGER as u64
        || scheduled_ms > now_ms as f64
    {
        return Err(ScheduleError::InvalidTime);
    }
    let environment = match lookup("ENVIRONMENT").as_deref() {
        Some("staging") => JobEnvironment::Staging,
        Some("production") => JobEnvironment::Production,
        _ => return Err(ScheduleError::Configuration),
    };
    let build_sha = crate::logging::parse_build_sha(lookup("BUILD_SHA").as_deref());
    if build_sha == crate::logging::BUILD_SHA_UNKNOWN
        || !crate::worker_entry::hyperdrive_caching_disabled_attested(lookup(
            "HYPERDRIVE_CACHING_DISABLED",
        ))
    {
        return Err(ScheduleError::Configuration);
    }
    let invocation = RetentionInvocation {
        environment,
        scheduled_at: (scheduled_ms / 1000.0) as i64,
        build_sha,
    };
    crate::worker_entry::with_deadline(
        async { operation(invocation).await },
        timeout_ms,
        ScheduleError::Deadline,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};

    struct Pending<'a>(&'a Cell<usize>);

    impl Future for Pending<'_> {
        type Output = Result<RetentionTick, ScheduleError>;
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }

    impl Drop for Pending<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn outer_deadline_owns_pending_connection_or_tick_and_caller_cancellation() {
        for connection_ready in [false, true] {
            let drops = Cell::new(0);
            let connected = Cell::new(false);
            let future = run_with_budget(
                RETENTION_CRON,
                1_800_000_000_000.0,
                1_800_000_005_000,
                |name| {
                    Some(
                        match name {
                            "ENVIRONMENT" => "staging",
                            "BUILD_SHA" => "0123456789abcdef0123456789abcdef01234567",
                            "HYPERDRIVE_CACHING_DISABLED" => "1",
                            _ => panic!("unexpected lookup"),
                        }
                        .into(),
                    )
                },
                |_| async {
                    if connection_ready {
                        // Connection time consumes the SAME outer budget.
                        futures_timer::Delay::new(Duration::from_millis(10)).await;
                        connected.set(true);
                    }
                    Pending(&drops).await
                },
                25,
            );
            let mut future = Box::pin(future);
            let start = Instant::now();
            let mut cx = Context::from_waker(std::task::Waker::noop());
            loop {
                if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
                    assert_eq!(result, Err(ScheduleError::Deadline));
                    break;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(2),
                    "outer budget hung"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(connected.get(), connection_ready);
            assert_eq!(drops.get(), 1, "pending phase drops before result");
            assert!(start.elapsed() >= Duration::from_millis(25));
            drop(future);
            assert_eq!(drops.get(), 1);
        }

        let drops = Cell::new(0);
        let mut future = Box::pin(run_with_budget(
            RETENTION_CRON,
            1_800_000_000_000.0,
            1_800_000_005_000,
            |name| {
                Some(
                    match name {
                        "ENVIRONMENT" => "production",
                        "BUILD_SHA" => "0123456789abcdef0123456789abcdef01234567",
                        "HYPERDRIVE_CACHING_DISABLED" => "true",
                        _ => panic!("unexpected lookup"),
                    }
                    .into(),
                )
            },
            |_| Pending(&drops),
            40_000,
        ));
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(future.as_mut().poll(&mut cx).is_pending());
        drop(future);
        assert_eq!(drops.get(), 1);
    }
}
