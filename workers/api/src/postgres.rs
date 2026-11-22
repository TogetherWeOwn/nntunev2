//! Statement execution on Hyperdrive uses the driver's unnamed protocol.
//! Connection and transaction control expose only the operations that callers need.

use std::future::Future;

use tokio_postgres::{
    types::{BorrowToSql, ToSql, Type},
    Client, Error, IsolationLevel, Row, RowStream, Transaction,
};

/// A connection that can execute only SQL with explicit parameter types.
///
/// ```no_run
/// use nntune_api::postgres::{PgClient, PgQuery};
/// use tokio_postgres::types::Type;
/// async fn typed_query(client: &PgClient) {
///     client.query_typed_one("SELECT $1::int4", &[(&1i32, Type::INT4)]).await.unwrap();
/// }
/// ```
///
/// ```compile_fail
/// use nntune_api::postgres::PgClient;
/// async fn named_query(client: &PgClient) {
///     client.query_one("SELECT 1", &[]).await.unwrap();
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

        }
    };
}

impl_pg_query!(PgClient, 0);
impl_pg_query!(PgTransaction<'_>, 0);
impl_pg_connect!(PgClient, 0);

// Native Postgres fixtures can set up and inspect disposable databases with
// the driver while exercising the same typed adapters as the Worker.
#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
impl_pg_query!(Client);
#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
impl_pg_connect!(Client);

impl PgClient {
    pub(crate) fn new(client: Client) -> Self {
        Self(client)
    }
}

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
}
