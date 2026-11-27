//! Statement execution on Hyperdrive uses the driver's unnamed protocol.
//! Connection and transaction control expose only the operations that callers need.
//! Project result columns as built-in PostgreSQL types. The driver can prepare
//! a named catalog lookup when decoding an unfamiliar result type.

use std::future::Future;

use tokio_postgres::{
    types::{BorrowToSql, ToSql, Type},
    Client, Error, IsolationLevel, Row, RowStream, Transaction,
};

/// A connection that can execute only SQL with explicit parameter types.
///
/// ```no_run
/// use nntune_api::postgres::{PgClient, PgQuery, PgTransaction};
/// use tokio_postgres::types::Type;
/// async fn typed_query(client: &PgClient, tx: &PgTransaction<'_>) {
///     client.query_typed_one("SELECT $1::int4", &[(&1i32, Type::INT4)]).await.unwrap();
///     tx.query_typed_one("SELECT $1::int4", &[(&1i32, Type::INT4)]).await.unwrap();
/// }
/// ```
///
/// ```compile_fail
/// use nntune_api::postgres::PgClient;
/// async fn named_query(client: &PgClient) {
///     client.query_one("SELECT 1", &[]).await.unwrap();
/// }
/// ```
/// ```compile_fail
/// use nntune_api::postgres::PgClient;
/// async fn arbitrary_batch(client: &PgClient) {
///     client.batch_execute("SELECT 1").await.unwrap();
/// }
/// ```
pub struct PgClient(Client);

/// A transaction with the same typed statement interface as the connection.
///
/// ```compile_fail
/// use nntune_api::postgres::PgTransaction;
/// fn raw_client(tx: &PgTransaction<'_>) {
///     let _ = tx.client();
/// }
/// ```
pub struct PgTransaction<'a>(Transaction<'a>);

type Param<'a> = (&'a (dyn ToSql + Sync), Type);

// Postgres stores these watchdogs as signed 32-bit milliseconds and uses zero
// to disable them. Keep every production watchdog finite.
fn watchdog_ms(ms: u64) -> u64 {
    ms.clamp(1, i32::MAX as u64)
}

mod sealed {
    pub trait Sealed {}
}

pub trait PgQuery: sealed::Sealed {
    fn query_typed(
        &self,
        sql: &str,
        params: &[Param<'_>],
    ) -> impl Future<Output = Result<Vec<Row>, Error>> + Send;

    fn query_typed_opt(
        &self,
        sql: &str,
        params: &[Param<'_>],
    ) -> impl Future<Output = Result<Option<Row>, Error>> + Send;

    fn query_typed_one(
        &self,
        sql: &str,
        params: &[Param<'_>],
    ) -> impl Future<Output = Result<Row, Error>> + Send;

    fn execute_typed(
        &self,
        sql: &str,
        params: &[Param<'_>],
    ) -> impl Future<Output = Result<u64, Error>> + Send;
}

pub trait PgConnect: PgQuery {
    fn transaction(&mut self) -> impl Future<Output = Result<PgTransaction<'_>, Error>> + Send;

    fn read_only_snapshot(
        &mut self,
    ) -> impl Future<Output = Result<PgTransaction<'_>, Error>> + Send;

    fn read_committed(&mut self) -> impl Future<Output = Result<PgTransaction<'_>, Error>> + Send;
}

// Forward to the inherent driver methods without boxing each query future.
macro_rules! impl_pg_query {
    ($pg:ty $(, $inner:tt)?) => {
        impl sealed::Sealed for $pg {}

        impl PgQuery for $pg {
            async fn query_typed(&self, sql: &str, params: &[Param<'_>]) -> Result<Vec<Row>, Error> {
                self$(.$inner)?.query_typed(sql, params).await
            }

            async fn query_typed_opt(
                &self,
                sql: &str,
                params: &[Param<'_>],
            ) -> Result<Option<Row>, Error> {
                self$(.$inner)?.query_typed_opt(sql, params).await
            }

            async fn query_typed_one(&self, sql: &str, params: &[Param<'_>]) -> Result<Row, Error> {
                self$(.$inner)?.query_typed_one(sql, params).await
            }

            async fn execute_typed(&self, sql: &str, params: &[Param<'_>]) -> Result<u64, Error> {
                self$(.$inner)?.execute_typed(sql, params).await
            }
        }
    };
}

macro_rules! impl_pg_connect {
    ($pg:ty $(, $inner:tt)?) => {
        impl PgConnect for $pg {
            async fn transaction(&mut self) -> Result<PgTransaction<'_>, Error> {
                self$(.$inner)?.transaction().await.map(PgTransaction)
            }

            async fn read_only_snapshot(&mut self) -> Result<PgTransaction<'_>, Error> {
                self$(.$inner)?
                    .build_transaction()
                    .isolation_level(IsolationLevel::RepeatableRead)
                    .read_only(true)
                    .start()
                    .await
                    .map(PgTransaction)
            }

            async fn read_committed(&mut self) -> Result<PgTransaction<'_>, Error> {
                self$(.$inner)?
                    .build_transaction()
                    .isolation_level(IsolationLevel::ReadCommitted)
                    .start()
                    .await
                    .map(PgTransaction)
            }

        }
    };
}

impl_pg_query!(PgClient, 0);
impl_pg_query!(PgTransaction<'_>, 0);
impl_pg_connect!(PgClient, 0);

// Native Postgres fixtures can set up and inspect disposable databases with
// the driver while exercising the same typed adapters as the Worker.
#[cfg(any(
    all(any(test, feature = "postgres-tests"), not(target_arch = "wasm32")),
    all(feature = "billing-owned-wasm-test", target_arch = "wasm32"),
))]
impl_pg_query!(Client);
#[cfg(all(any(test, feature = "postgres-tests"), not(target_arch = "wasm32")))]
impl_pg_connect!(Client);
#[cfg(all(any(test, feature = "postgres-tests"), not(target_arch = "wasm32")))]
impl_pg_query!(Transaction<'_>);

impl PgClient {
    /// Wrap an established connection's client. The caller still drives the connection.
    pub fn new(client: Client) -> Self {
        Self(client)
    }

    pub(crate) async fn set_statement_timeout(&self, ms: u64) -> Result<(), Error> {
        let ms = watchdog_ms(ms);
        self.0
            .batch_execute(&format!("SET statement_timeout = {ms}"))
            .await
    }

    #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
    pub(crate) fn cancel_token(&self) -> tokio_postgres::CancelToken {
        self.0.cancel_token()
    }
}

// The owned billing transport drives one connection across separate phase
// timers. Keep BEGIN and the local watchdog settings in one wire batch.
pub(crate) trait PgBilling: PgQuery {
    fn begin_billing(&self, remaining_ms: u64) -> impl Future<Output = Result<(), Error>> + Send;
    fn commit_billing(&self) -> impl Future<Output = Result<(), Error>> + Send;
    fn rollback_billing(&self) -> impl Future<Output = Result<(), Error>> + Send;

    #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
    fn disable_billing_watchdogs(&self) -> impl Future<Output = Result<(), Error>> + Send;
}

macro_rules! impl_pg_billing {
    ($pg:ty $(, $inner:tt)?) => {
        impl PgBilling for $pg {
            async fn begin_billing(&self, remaining_ms: u64) -> Result<(), Error> {
                let remaining_ms = watchdog_ms(remaining_ms);
                let lock_ms = remaining_ms.min(4_000);
                let statement_ms = remaining_ms.min(5_000);
                let idle_ms = remaining_ms.min(5_000);
                self$(.$inner)?.batch_execute(&format!(
                    "BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL transaction_timeout = {remaining_ms}; SET LOCAL lock_timeout = {lock_ms}; SET LOCAL statement_timeout = {statement_ms}; SET LOCAL idle_in_transaction_session_timeout = {idle_ms}"
                )).await
            }

            async fn commit_billing(&self) -> Result<(), Error> {
                self$(.$inner)?.batch_execute("COMMIT").await
            }

            async fn rollback_billing(&self) -> Result<(), Error> {
                self$(.$inner)?.batch_execute("ROLLBACK").await
            }

            #[cfg(all(target_arch = "wasm32", feature = "billing-owned-wasm-test"))]
            async fn disable_billing_watchdogs(&self) -> Result<(), Error> {
                self$(.$inner)?.batch_execute(
                    "SET LOCAL idle_in_transaction_session_timeout = 0; SET LOCAL transaction_timeout = 0",
                ).await
            }
        }
    };
}

impl_pg_billing!(PgClient, 0);
#[cfg(any(
    all(any(test, feature = "postgres-tests"), not(target_arch = "wasm32")),
    all(feature = "billing-owned-wasm-test", target_arch = "wasm32"),
))]
impl_pg_billing!(Client);

impl PgTransaction<'_> {
    pub(crate) async fn query_typed_raw<P, I>(
        &self,
        sql: &str,
        params: I,
    ) -> Result<RowStream, Error>
    where
        P: BorrowToSql,
        I: IntoIterator<Item = (P, Type)>,
    {
        self.0.query_typed_raw(sql, params).await
    }

    pub(crate) async fn set_local_statement_timeout(&self, ms: u64) -> Result<(), Error> {
        let ms = watchdog_ms(ms);
        self.0
            .batch_execute(&format!("SET LOCAL statement_timeout = {ms}"))
            .await
    }

    pub(crate) async fn set_local_subsonic_timeouts(&self) -> Result<(), Error> {
        self.0
            .batch_execute("SET LOCAL statement_timeout = '1s'; SET LOCAL lock_timeout = '1s'")
            .await
    }

    pub(crate) async fn commit(self) -> Result<(), Error> {
        self.0.commit().await
    }

    pub(crate) async fn rollback(self) -> Result<(), Error> {
        self.0.rollback().await
    }
}

#[cfg(test)]
mod tests {
    use super::watchdog_ms;

    #[test]
    fn watchdog_stays_enabled_and_within_postgres_range() {
        let max = i32::MAX as u64;
        for (requested, bounded) in [(0, 1), (1, 1), (500, 500), (max, max), (u64::MAX, max)] {
            assert_eq!(watchdog_ms(requested), bounded);
        }
    }
}
