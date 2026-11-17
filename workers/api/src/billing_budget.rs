//! Reserved relative-duration phases for the future owned billing operation.
//!
//! This is not elapsed-time accounting: unused connect and bootstrap time is
//! deliberately not reclaimed. The outer deadline must enclose every phase.

use std::cell::Cell;
use std::future::Future;

use crate::handlers::StoreError;
use crate::worker_entry::with_deadline;

const TOTAL_MS: u64 = 15_000;
const CONNECT_MS: u64 = 5_000;
const BOOTSTRAP_MS: u64 = 1_000;
pub(super) const WORK_MS: u64 = TOTAL_MS - CONNECT_MS - BOOTSTRAP_MS;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Connect,
    Bootstrap,
    Work,
    Running,
    Finished,
}

pub(super) struct BillingBudget {
    phase: Cell<Phase>,
}

impl BillingBudget {
    /// Start the total timer before constructing or polling operation work.
    pub(super) async fn run<T, F>(operation: impl FnOnce(Self) -> F) -> Result<T, StoreError>
    where
        F: Future<Output = Result<T, StoreError>>,
    {
        with_deadline(
            async {
                operation(Self {
                    phase: Cell::new(Phase::Connect),
                })
                .await
            },
            TOTAL_MS,
            StoreError,
        )
        .await
    }

    pub(super) async fn connect<T, F: Future<Output = Result<T, StoreError>>>(
        &self,
        operation: impl FnOnce() -> F,
    ) -> Result<T, StoreError> {
        self.phase(Phase::Connect, Phase::Bootstrap, CONNECT_MS, operation)
            .await
    }

    pub(super) async fn bootstrap<T, F: Future<Output = Result<T, StoreError>>>(
        &self,
        operation: impl FnOnce() -> F,
    ) -> Result<T, StoreError> {
        self.phase(Phase::Bootstrap, Phase::Work, BOOTSTRAP_MS, operation)
            .await
    }

    pub(super) async fn work<T, F: Future<Output = Result<T, StoreError>>>(
        &self,
        operation: impl FnOnce() -> F,
    ) -> Result<T, StoreError> {
        self.phase(Phase::Work, Phase::Finished, WORK_MS, operation)
            .await
    }

    async fn phase<T, F: Future<Output = Result<T, StoreError>>>(
        &self,
        expected: Phase,
        next: Phase,
        duration_ms: u64,
        operation: impl FnOnce() -> F,
    ) -> Result<T, StoreError> {
        if self.phase.replace(Phase::Running) != expected {
            return Err(StoreError);
        }
        // A cancelled or failed phase remains Running and cannot be restarted.
        let result = with_deadline(async { operation().await }, duration_ms, StoreError).await?;
        self.phase.set(next);
        Ok(result)
    }
}

#[cfg(feature = "billing-owned-wasm-test")]
pub(crate) async fn test_budget() -> Result<(), &'static str> {
    use std::{future, task::Context};

    if CONNECT_MS + BOOTSTRAP_MS + WORK_MS != TOTAL_MS || WORK_MS != 9_000 {
        return Err("budget reservations do not sum to total");
    }
    BillingBudget::run(|budget| async move {
        budget.connect(|| async { Ok(()) }).await?;
        budget.bootstrap(|| async { Ok(()) }).await?;
        budget.work(|| async { Ok(()) }).await?;
        if budget.work(|| async { Ok(()) }).await.is_ok() {
            return Err(StoreError);
        }
        Ok(())
    })
    .await
    .map_err(|_| "budget phase sequence failed")?;

    let budget = BillingBudget {
        phase: Cell::new(Phase::Connect),
    };
    if budget.bootstrap(|| async { Ok(()) }).await.is_ok()
        || budget.connect(|| async { Ok(()) }).await.is_ok()
    {
        return Err("out-of-order phase did not fail closed");
    }
    let budget = BillingBudget {
        phase: Cell::new(Phase::Connect),
    };
    if budget.connect(|| async { Err::<(), _>(StoreError) }).await.is_ok()
        || budget.connect(|| async { Ok(()) }).await.is_ok()
    {
        return Err("failed phase restarted");
    }
    let budget = BillingBudget {
        phase: Cell::new(Phase::Connect),
    };
    let mut phase = Box::pin(budget.connect(|| future::pending::<Result<(), StoreError>>()));
    let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
    if phase.as_mut().poll(&mut cx).is_ready() {
        return Err("pending phase completed");
    }
    drop(phase);
    if budget.connect(|| async { Ok(()) }).await.is_ok() {
        return Err("cancelled phase restarted");
    }
    Ok(())
}
