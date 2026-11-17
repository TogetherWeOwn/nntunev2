//! Reserved relative-duration phases for the future owned billing operation.
//!
//! This is not elapsed-time accounting: unused connect and bootstrap time is
//! deliberately not reclaimed. The outer deadline must enclose every phase.

use std::cell::Cell;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::task::Poll;

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

    /// Keep the same driver in the request while each active phase races its timer.
    pub(super) async fn bootstrap_driven<T, E, D, F>(
        &self,
        driver: Pin<&mut D>,
        operation: impl FnOnce() -> F,
    ) -> Result<T, StoreError>
    where
        D: Future<Output = Result<(), E>>,
        F: Future<Output = Result<T, StoreError>>,
    {
        self.bootstrap(|| drive(driver, operation())).await
    }

    pub(super) async fn work_driven<T, E, D, F>(
        &self,
        driver: Pin<&mut D>,
        operation: impl FnOnce() -> F,
    ) -> Result<T, StoreError>
    where
        D: Future<Output = Result<(), E>>,
        F: Future<Output = Result<T, StoreError>>,
    {
        self.work(|| drive(driver, operation())).await
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

/// Inactive owned transport preparation. Callbacks must not spawn work or let
/// database handles escape; transaction setup and outcome handling remain the
/// responsibility of the future billing operation.
#[cfg(target_arch = "wasm32")]
pub(super) async fn with_owned_connection<T: 'static>(
    host: &str,
    port: u16,
    connection_string: &str,
    bootstrap: impl for<'a> FnOnce(
        &'a mut tokio_postgres::Client,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + 'a>>,
    work: impl for<'a> FnOnce(
        &'a mut tokio_postgres::Client,
    ) -> Pin<Box<dyn Future<Output = Result<T, StoreError>> + 'a>>,
) -> Result<T, StoreError> {
    use crate::billing_owned_socket::SocketGuard;

    BillingBudget::run(|budget| async move {
        let guard = SocketGuard::default();
        let (mut client, connection) = budget
            .connect(|| async {
                let socket = guard.open(host, port).map_err(|_| StoreError)?;
                socket.opened().await.map_err(|_| StoreError)?;
                let config: tokio_postgres::Config =
                    connection_string.parse().map_err(|_| StoreError)?;
                config
                    .connect_raw(socket, guard.tls())
                    .await
                    .map_err(|_| StoreError)
            })
            .await?;
        let mut connection = std::pin::pin!(connection);
        budget
            .bootstrap_driven(connection.as_mut(), || bootstrap(&mut client))
            .await?;
        budget
            .work_driven(connection.as_mut(), || work(&mut client))
            .await
    })
    .await
}

// The test Worker has no configurable destination; the hosted Actions harness
// supplies the disposable service with these fixed loopback credentials.
#[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
pub(crate) async fn test_real_connection() -> Result<(i32, i32), &'static str> {
    use std::rc::Rc;

    const CONNECTION: &str = "host=127.0.0.1 user=postgres password=ci-throwaway dbname=nntune_ci sslmode=disable application_name=owned_transport_test";
    let success_pid = with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        |client| {
            Box::pin(async move {
                let row = client
                    .query_one("SELECT pg_backend_pid()", &[])
                    .await
                    .map_err(|_| StoreError)?;
                Ok(row.get::<_, i32>(0))
            })
        },
    )
    .await
    .map_err(|_| "real connection failed")?;

    let timeout_pid = Rc::new(Cell::new(0));
    let started = Rc::new(Cell::new(0.0));
    let observed = timeout_pid.clone();
    let work_started = started.clone();
    let result = with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |client| {
            Box::pin(async move {
                let row = client
                    .query_one("SELECT pg_backend_pid()", &[])
                    .await
                    .map_err(|_| StoreError)?;
                observed.set(row.get::<_, i32>(0));
                work_started.set(worker::js_sys::Date::now());
                std::future::pending::<Result<(), StoreError>>().await
            })
        },
    )
    .await;
    if result.is_ok()
        || success_pid <= 0
        || timeout_pid.get() <= 0
        || worker::js_sys::Date::now() - started.get() < 8_000.0
    {
        return Err("real connection timeout did not occur after query start");
    }
    Ok((success_pid, timeout_pid.get()))
}

// Called only within a phase deadline, itself enclosed by the total deadline.
async fn drive<T, E, D, F>(mut driver: Pin<&mut D>, operation: F) -> Result<T, StoreError>
where
    D: Future<Output = Result<(), E>>,
    F: Future<Output = Result<T, StoreError>>,
{
    let mut operation = std::pin::pin!(operation);
    poll_fn(|cx| {
        if driver.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(StoreError));
        }
        operation.as_mut().poll(cx)
    })
    .await
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

    let polls = Cell::new(0);
    BillingBudget::run(|budget| async move {
        budget.connect(|| async { Ok(()) }).await?;
        let driver = poll_fn(|_| {
            polls.set(polls.get() + 1);
            Poll::Pending::<Result<(), ()>>
        });
        let mut driver = std::pin::pin!(driver);
        budget
            .bootstrap_driven(driver.as_mut(), || async {
                if polls.get() != 1 {
                    return Err(StoreError);
                }
                Ok(())
            })
            .await?;
        budget
            .work_driven(driver.as_mut(), || async {
                if polls.get() != 2 {
                    return Err(StoreError);
                }
                Ok(())
            })
            .await
    })
    .await
    .map_err(|_| "phase driver was not co-polled")?;

    let budget = BillingBudget {
        phase: Cell::new(Phase::Bootstrap),
    };
    let mut failed_driver = std::pin::pin!(future::ready(Err::<(), ()>(())));
    let operation_polled = Cell::new(false);
    if budget
        .bootstrap_driven(failed_driver.as_mut(), || async {
            operation_polled.set(true);
            Ok(())
        })
        .await
        .is_ok()
        || operation_polled.get()
    {
        return Err("driver failure did not stop operation");
    }

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
    if budget
        .connect(|| async { Err::<(), _>(StoreError) })
        .await
        .is_ok()
        || budget.connect(|| async { Ok(()) }).await.is_ok()
    {
        return Err("failed phase restarted");
    }
    // Exercise expiry and destruction inside the actual Worker, without waiting
    // for the production connect allowance or relying on a native mock timer.
    struct DropMarker<'a>(&'a Cell<bool>);
    impl Drop for DropMarker<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }
    let dropped = Cell::new(false);
    let budget = BillingBudget {
        phase: Cell::new(Phase::Connect),
    };
    if budget
        .phase(Phase::Connect, Phase::Bootstrap, 10, || async {
            let _marker = DropMarker(&dropped);
            future::pending::<Result<(), StoreError>>().await
        })
        .await
        .is_ok()
        || !dropped.get()
        || budget.connect(|| async { Ok(()) }).await.is_ok()
    {
        return Err("Worker phase expiry did not drop work and fail closed");
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
