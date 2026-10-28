//! PostgreSQL transport boundary for the native writer sequences.
//!
//! The client comes from `library_db_client` at the edge. Each method's deadline
//! includes prepare, typed binds and execution. Expiry drops the caller's future;
//! it does not claim to cancel a statement already running on PostgreSQL.

use std::future::Future;

use tokio_postgres::types::{ToSql, Type};
use tokio_postgres::{Row, Statement};
use uuid::Uuid;

use crate::writer_execution::{DbFailure, Value, WriterDb};

/// One short-lived client, never retained across queue, R2 or meter operations.
/// Production supplies a Workers timer factory; native tests supply controlled
/// deadlines while driving a real `tokio_postgres::Client` over a test socket.
pub struct PgWriterDb<'a, D> {
    client: &'a tokio_postgres::Client,
    deadline: D,
}

impl<'a, D> PgWriterDb<'a, D> {
    pub fn new(client: &'a tokio_postgres::Client, deadline: D) -> Self {
        Self { client, deadline }
    }
}

impl<D, F> WriterDb for PgWriterDb<'_, D>
where
    D: Fn() -> F,
    F: Future<Output = ()>,
{
    async fn execute(&self, sql: &'static str, params: &[Value]) -> Result<u64, DbFailure> {
        crate::library::with_db_deadline(
            async {
                let statement = self.client.prepare(sql).await.map_err(driver_error)?;
                let values = bind_values(&statement, params)?;
                let binds: Vec<&(dyn ToSql + Sync)> = values.iter().map(|v| v.as_ref()).collect();
                self.client
                    .execute(&statement, &binds)
                    .await
                    .map_err(driver_error)
            },
            (self.deadline)(),
            transport_error,
        )
        .await
    }

    async fn query(
        &self,
        sql: &'static str,
        params: &[Value],
    ) -> Result<Vec<Vec<Value>>, DbFailure> {
        crate::library::with_db_deadline(
            async {
                let statement = self.client.prepare(sql).await.map_err(driver_error)?;
                let values = bind_values(&statement, params)?;
                let binds: Vec<&(dyn ToSql + Sync)> = values.iter().map(|v| v.as_ref()).collect();
                let rows = self
                    .client
                    .query(&statement, &binds)
                    .await
                    .map_err(driver_error)?;
                rows.iter().map(decode_row).collect()
            },
            (self.deadline)(),
            transport_error,
        )
        .await
    }
}

// SQL parameter OIDs, not string appearance or parameter position, decide UUID
// encoding. The track statements deliberately accept UUIDs as TEXT casts while
// the media/grant statements bind UUID directly. Both keep the pinned SQL shape.
fn bind_values<'a>(
    statement: &Statement,
    params: &'a [Value],
) -> Result<Vec<Box<dyn ToSql + Sync + 'a>>, DbFailure> {
    if statement.params().len() != params.len() {
        return Err(data_error());
    }
    statement
        .params()
        .iter()
        .zip(params)
        .map(|(ty, value)| {
            let bind: Box<dyn ToSql + Sync> = match value {
                Value::Text(value) if *ty == Type::UUID => {
                    Box::new(Uuid::parse_str(value).map_err(|_| data_error())?)
                }
                Value::Text(value) => Box::new(value.as_str()),
                Value::I64(value) => Box::new(value),
                Value::I32(value) => Box::new(value),
            };
            Ok(bind)
        })
        .collect()
}

// Do not silently coerce unsupported types or return a valid prefix when a
// later row is malformed. NULL is invalid for every selected writer column.
fn decode_row(row: &Row) -> Result<Vec<Value>, DbFailure> {
    row.columns()
        .iter()
        .enumerate()
        .map(|(index, column)| match *column.type_() {
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => row
                .try_get::<_, String>(index)
                .map(Value::Text)
                .map_err(|_| data_error()),
            Type::UUID => row
                .try_get::<_, Uuid>(index)
                .map(|value| Value::Text(value.hyphenated().to_string()))
                .map_err(|_| data_error()),
            Type::INT8 => row
                .try_get::<_, i64>(index)
                .map(Value::I64)
                .map_err(|_| data_error()),
            Type::INT4 => row
                .try_get::<_, i32>(index)
                .map(Value::I32)
                .map_err(|_| data_error()),
            _ => Err(data_error()),
        })
        .collect()
}

fn driver_error(error: tokio_postgres::Error) -> DbFailure {
    DbFailure {
        sqlstate: error.code().map(|code| code.code().to_owned()),
    }
}

fn transport_error() -> DbFailure {
    DbFailure { sqlstate: None }
}

fn data_error() -> DbFailure {
    DbFailure {
        sqlstate: Some("22000".to_owned()),
    }
}
