//! Reserved relative-duration phases for the future owned billing operation.
//!
//! This is not elapsed-time accounting: unused connect and bootstrap time is
//! deliberately not reclaimed. The outer deadline must enclose every phase.

use std::cell::Cell;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::task::Poll;

use crate::handlers::StoreError;
use crate::postgres::PgBilling;
#[cfg(target_arch = "wasm32")]
use crate::postgres::PgClient;
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

    #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
    async fn work_driven_classified<T, E, D, F>(
        &self,
        driver: Pin<&mut D>,
        operation: impl FnOnce() -> F,
    ) -> Result<T, PhaseFailure>
    where
        D: Future<Output = Result<(), E>>,
        F: Future<Output = Result<T, StoreError>>,
    {
        self.phase_classified(Phase::Work, Phase::Finished, WORK_MS, || {
            drive(driver, operation())
        })
        .await
    }

    async fn phase<T, F: Future<Output = Result<T, StoreError>>>(
        &self,
        expected: Phase,
        next: Phase,
        duration_ms: u64,
        operation: impl FnOnce() -> F,
    ) -> Result<T, StoreError> {
        self.phase_classified(expected, next, duration_ms, operation)
            .await
            .map_err(|_| StoreError)
    }

    async fn phase_classified<T, F: Future<Output = Result<T, StoreError>>>(
        &self,
        expected: Phase,
        next: Phase,
        duration_ms: u64,
        operation: impl FnOnce() -> F,
    ) -> Result<T, PhaseFailure> {
        if self.phase.replace(Phase::Running) != expected {
            return Err(PhaseFailure::Operation);
        }
        // A cancelled or failed phase remains Running and cannot be restarted.
        let result = phase_deadline(operation, duration_ms).await?;
        self.phase.set(next);
        Ok(result)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PhaseFailure {
    Expired,
    Operation,
}

// Keep the timer outcome separate from an operation or driver error until the
// caller deliberately maps both to the public, fail-closed store error.
async fn phase_deadline<T, F: Future<Output = Result<T, StoreError>>>(
    operation: impl FnOnce() -> F,
    duration_ms: u64,
) -> Result<T, PhaseFailure> {
    with_deadline(async { Ok::<_, ()>(operation().await) }, duration_ms, ())
        .await
        .map_err(|_| PhaseFailure::Expired)?
        .map_err(|_| PhaseFailure::Operation)
}

/// Start the future billing transaction with conservative work-phase limits.
/// A failed batch may have begun a transaction, so attempt rollback; failure
/// or cancellation of that attempt does not establish connection cleanup.
pub(super) async fn bootstrap_transaction(client: &mut impl PgBilling) -> Result<(), StoreError> {
    if client.begin_billing(WORK_MS).await.is_err() {
        let _ = client.rollback_billing().await;
        return Err(StoreError);
    }
    Ok(())
}

#[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
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
    work_finished: impl FnOnce(bool),
) -> Result<T, StoreError> {
    drive_owned_connection(
        || Ok((host.to_owned(), port, connection_string.to_owned())),
        std::convert::identity,
        bootstrap,
        async move |client| work(client).await,
        work_finished,
    )
    .await
}

/// Resolve the binding inside the connect phase. Callbacks must not spawn work
/// or let database handles escape.
#[cfg(target_arch = "wasm32")]
pub(super) async fn with_owned_connection_from<T: 'static>(
    target: impl FnOnce() -> Result<(String, u16, String), StoreError>,
    bootstrap: impl for<'a> FnOnce(
        &'a mut PgClient,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + 'a>>,
    work: impl AsyncFnOnce(&mut PgClient) -> Result<T, StoreError>,
    #[cfg(feature = "billing-owned-wasm-test")] work_finished: impl FnOnce(bool),
) -> Result<T, StoreError> {
    drive_owned_connection(
        target,
        PgClient::new,
        bootstrap,
        work,
        #[cfg(feature = "billing-owned-wasm-test")]
        work_finished,
    )
    .await
}

#[cfg(target_arch = "wasm32")]
async fn drive_owned_connection<T: 'static, C>(
    target: impl FnOnce() -> Result<(String, u16, String), StoreError>,
    wrap: impl FnOnce(tokio_postgres::Client) -> C,
    bootstrap: impl for<'a> FnOnce(
        &'a mut C,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + 'a>>,
    work: impl AsyncFnOnce(&mut C) -> Result<T, StoreError>,
    #[cfg(feature = "billing-owned-wasm-test")] work_finished: impl FnOnce(bool),
) -> Result<T, StoreError> {
    use crate::billing_owned_socket::SocketGuard;

    BillingBudget::run(|budget| async move {
        let guard = SocketGuard::default();
        let (mut client, connection) = budget
            .connect(|| async {
                let (host, port, connection_string) = target()?;
                let socket = guard.open(&host, port).map_err(|_| StoreError)?;
                socket.opened().await.map_err(|_| StoreError)?;
                let config: tokio_postgres::Config =
                    connection_string.parse().map_err(|_| StoreError)?;
                config
                    .connect_raw(socket, guard.tls())
                    .await
                    .map(|(client, connection)| (wrap(client), connection))
                    .map_err(|_| StoreError)
            })
            .await?;
        let mut connection = std::pin::pin!(connection);
        budget
            .bootstrap_driven(connection.as_mut(), || bootstrap(&mut client))
            .await?;
        #[cfg(not(feature = "billing-owned-wasm-test"))]
        let result = budget
            .work_driven(connection.as_mut(), || work(&mut client))
            .await;
        #[cfg(feature = "billing-owned-wasm-test")]
        let result = {
            let result = budget
                .work_driven_classified(connection.as_mut(), || work(&mut client))
                .await;
            work_finished(matches!(result, Err(PhaseFailure::Expired)));
            result.map_err(|_| StoreError)
        };
        result
    })
    .await
}

// The test Worker has no configurable destination; the hosted Actions harness
// supplies the disposable service with these fixed loopback credentials.
#[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
pub(crate) async fn test_real_connection() -> Result<(i32, i32), &'static str> {
    use std::rc::Rc;

    const CONNECTION: &str = "host=127.0.0.1 user=postgres password=ci-throwaway dbname=nntune_ci sslmode=disable application_name=owned_transport_test";
    let attempted = Cell::new(false);
    let failure = with_owned_connection_from::<()>(
        || {
            attempted.set(true);
            Err(StoreError)
        },
        |_| panic!("bootstrap ran without a connection target"),
        async |_| panic!("work ran without a connection target"),
        |_| panic!("work observer ran without a connection target"),
    );
    if attempted.get() || failure.await.is_ok() || !attempted.get() {
        return Err("connection target failure did not stop before bootstrap");
    }

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
        |_| {},
    )
    .await
    .map_err(|_| "real connection failed")?;

    let failure_observed = Rc::new(Cell::new(None));
    let failure_signal = failure_observed.clone();
    let failure = with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        |_| Box::pin(async { Err::<(), _>(StoreError) }),
        move |expired| failure_signal.set(Some(expired)),
    )
    .await;
    if failure.is_ok() || failure_observed.get() != Some(false) {
        return Err("operation error was classified as deadline expiry");
    }

    let timeout_pid = Rc::new(Cell::new(0));
    let started = Rc::new(Cell::new(0.0));
    let phase_finished = Rc::new(Cell::new(false));
    let observed = timeout_pid.clone();
    let work_started = started.clone();
    let phase_observed = phase_finished.clone();
    let result = with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |client| {
            Box::pin(async move {
                work_started.set(worker::js_sys::Date::now());
                let row = client
                    .query_one("SELECT pg_backend_pid()", &[])
                    .await
                    .map_err(|_| StoreError)?;
                observed.set(row.get::<_, i32>(0));
                std::future::pending::<Result<(), StoreError>>().await
            })
        },
        move |expired| phase_observed.set(expired),
    )
    .await;
    if result.is_ok()
        || success_pid <= 0
        || timeout_pid.get() <= 0
        || !phase_finished.get()
        || worker::js_sys::Date::now() - started.get() < 8_000.0
    {
        return Err("real connection work phase did not expire after starting");
    }
    Ok((success_pid, timeout_pid.get()))
}

// This runs only in the disposable Worker test bundle, against its fixed service.
#[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
pub(crate) async fn test_transaction_bootstrap() -> Result<(), &'static str> {
    const CONNECTION: &str = "host=127.0.0.1 user=postgres password=ci-throwaway dbname=nntune_ci sslmode=disable application_name=bootstrap_test";
    with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |client| Box::pin(bootstrap_transaction(client)),
        |client| {
            Box::pin(async move {
                let row = client.query_one(
                    "SELECT current_setting('transaction_isolation'), current_setting('transaction_timeout'), current_setting('lock_timeout'), current_setting('statement_timeout'), current_setting('idle_in_transaction_session_timeout')",
                    &[],
                ).await.map_err(|_| StoreError)?;
                let settings: Vec<String> = (0..5).map(|i| row.get(i)).collect();
                if settings != ["read committed", "9s", "4s", "5s", "5s"] {
                    return Err(StoreError);
                }
                client.batch_execute("CREATE TEMP TABLE bootstrap_worker_probe (id integer); ROLLBACK")
                    .await.map_err(|_| StoreError)?;
                let row = client.query_one("SELECT to_regclass('pg_temp.bootstrap_worker_probe') IS NULL", &[])
                    .await.map_err(|_| StoreError)?;
                if !row.get::<_, bool>(0) {
                    return Err(StoreError);
                }
                Ok(())
            })
        },
        |_| {},
    ).await.map_err(|_| "Worker transaction bootstrap failed")
}

#[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
pub(crate) const BILLING_FIXTURE: &str = "host=127.0.0.1 user=postgres password=ci-throwaway dbname=nntune_billing_ack sslmode=disable application_name=billing_ack_test";

// Standalone transport control; this does not invoke billing reconciliation.
#[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
pub(crate) async fn test_deadline_after_write() -> Result<i32, &'static str> {
    let ready_pid = std::rc::Rc::new(Cell::new(0));
    let expired = std::rc::Rc::new(Cell::new(false));
    let observed_ready_pid = ready_pid.clone();
    let observed_expiry = expired.clone();
    let result = with_owned_connection(
        "127.0.0.1",
        5432,
        BILLING_FIXTURE,
        |client| Box::pin(bootstrap_transaction(client)),
        move |client| Box::pin(async move {
            let row = client.query_one(
                "INSERT INTO public.users (id,email,created_at) VALUES ('deadline_probe','deadline_probe',0) RETURNING pg_backend_pid()", &[],
            ).await.map_err(|_| StoreError)?;
            let backend_pid = row.get::<_, i32>(0);
            let row = client.query_one("SELECT count(*) FROM public.users WHERE id = 'deadline_probe'", &[]).await.map_err(|_| StoreError)?;
            if row.get::<_, i64>(0) != 1 { return Err(StoreError); }
            client.batch_execute("SET LOCAL idle_in_transaction_session_timeout = 0; SET LOCAL transaction_timeout = 0").await.map_err(|_| StoreError)?;
            // Expiry only proves Worker cleanup once PostgreSQL has acknowledged both settings.
            observed_ready_pid.set(backend_pid);
            std::future::pending::<Result<(), StoreError>>().await
        }),
        move |was_expired| observed_expiry.set(was_expired),
    ).await;
    if result.is_ok() || !expired.get() || ready_pid.get() <= 0 {
        return Err("write control did not reach the Worker deadline after setup");
    }
    Ok(ready_pid.get())
}

#[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
pub(crate) async fn test_commit_ack() -> Result<i32, &'static str> {
    let prepared_pid = std::rc::Rc::new(Cell::new(0));
    let observed = prepared_pid.clone();
    let result = with_owned_connection(
        "127.0.0.1",
        15432,
        BILLING_FIXTURE,
        |client| Box::pin(bootstrap_transaction(client)),
        move |client| {
            Box::pin(async move {
                let row = client
                    .query_one(
                        "INSERT INTO users (id,email,created_at) VALUES ('withheld','withheld',0) RETURNING pg_backend_pid()",
                        &[],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                prepared_pid.set(row.get::<_, i32>(0));
                client.batch_execute("COMMIT").await.map_err(|_| StoreError)
            })
        },
        |_| {},
    )
    .await;
    let pid = observed.get();
    if pid <= 0 || result.is_ok() {
        return Err("withheld COMMIT did not fail after a successful insert");
    }
    Ok(pid)
}

#[cfg(all(test, feature = "postgres-tests", not(target_arch = "wasm32")))]
mod postgres_tests {
    use super::bootstrap_transaction;
    use tokio_postgres::NoTls;

    #[tokio::test]
    async fn bootstrap_overrides_session_isolation_and_commit_restores_limits() {
        // Fixed disposable Actions service; never accept an arbitrary database URL.
        let (mut client, connection) = tokio_postgres::connect(
            "host=127.0.0.1 user=postgres password=ci-throwaway dbname=nntune_ci sslmode=disable",
            NoTls,
        )
        .await
        .expect("connect disposable database");
        let driver = tokio::spawn(connection);
        client.batch_execute("SET default_transaction_isolation = 'repeatable read'; SET transaction_timeout = 30000; SET lock_timeout = 11000; SET statement_timeout = 12000; SET idle_in_transaction_session_timeout = 13000")
            .await.unwrap();
        let before = client.query_one("SELECT current_setting('transaction_isolation'), current_setting('transaction_timeout'), current_setting('lock_timeout'), current_setting('statement_timeout'), current_setting('idle_in_transaction_session_timeout')", &[])
            .await.unwrap();
        let before: Vec<String> = (0..5).map(|i| before.get(i)).collect();
        assert_eq!(before, ["repeatable read", "30s", "11s", "12s", "13s"]);

        bootstrap_transaction(&mut client).await.expect("bootstrap");
        let during = client.query_one("SELECT current_setting('transaction_isolation'), current_setting('transaction_timeout'), current_setting('lock_timeout'), current_setting('statement_timeout'), current_setting('idle_in_transaction_session_timeout')", &[])
            .await.unwrap();
        let during: Vec<String> = (0..5).map(|i| during.get(i)).collect();
        assert_eq!(during, ["read committed", "9s", "4s", "5s", "5s"]);
        client.batch_execute("COMMIT").await.unwrap();
        let after = client.query_one("SELECT current_setting('transaction_isolation'), current_setting('transaction_timeout'), current_setting('lock_timeout'), current_setting('statement_timeout'), current_setting('idle_in_transaction_session_timeout')", &[])
            .await.unwrap();
        let after: Vec<String> = (0..5).map(|i| after.get(i)).collect();
        assert_eq!(after, before);
        drop(client);
        driver.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn bootstrap_sets_local_limits_and_rollback_restores_them() {
        // Fixed disposable Actions service; never accept an arbitrary database URL.
        let (mut client, connection) = tokio_postgres::connect(
            "host=127.0.0.1 user=postgres password=ci-throwaway dbname=nntune_ci sslmode=disable",
            NoTls,
        )
        .await
        .expect("connect disposable database");
        let driver = tokio::spawn(connection);
        let before = client
            .query_one("SELECT current_setting('transaction_timeout'), current_setting('lock_timeout'), current_setting('statement_timeout'), current_setting('idle_in_transaction_session_timeout')", &[])
            .await
            .unwrap();
        let before: Vec<String> = (0..4).map(|i| before.get(i)).collect();
        bootstrap_transaction(&mut client).await.expect("bootstrap");
        let row = client
            .query_one("SELECT current_setting('transaction_isolation'), current_setting('transaction_timeout'), current_setting('lock_timeout'), current_setting('statement_timeout'), current_setting('idle_in_transaction_session_timeout')", &[])
            .await
            .unwrap();
        let settings: Vec<String> = (0..5).map(|i| row.get(i)).collect();
        assert_eq!(settings, ["read committed", "9s", "4s", "5s", "5s"]);
        client
            .batch_execute("CREATE TEMP TABLE bootstrap_rollback_probe (id integer); ROLLBACK")
            .await
            .unwrap();
        let after = client
            .query_one("SELECT current_setting('transaction_timeout'), current_setting('lock_timeout'), current_setting('statement_timeout'), current_setting('idle_in_transaction_session_timeout'), to_regclass('pg_temp.bootstrap_rollback_probe') IS NULL", &[])
            .await
            .unwrap();
        let restored: Vec<String> = (0..4).map(|i| after.get(i)).collect();
        assert_eq!(restored, before);
        assert!(after.get::<_, bool>(4));
        drop(client);
        driver.await.unwrap().unwrap();
    }
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
    // An operation failure must never be mistaken for a Worker timer firing.
    if !matches!(
        phase_deadline(|| async { Err::<(), _>(StoreError) }, 100).await,
        Err(PhaseFailure::Operation)
    ) || !matches!(
        phase_deadline(future::pending::<Result<(), StoreError>>, 10).await,
        Err(PhaseFailure::Expired)
    ) {
        return Err("phase failure classification was incorrect");
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
