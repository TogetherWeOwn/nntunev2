//! PostgreSQL transport boundary for the native writer sequences.
//!
//! The client comes from `library_db_client` at the edge. The deadline factory
//! is injectable so the same transport can be exercised without Workers timers.
//! This interface is deliberately fail-closed until its independent wire-level
//! acceptance bank is delivered; it is not connected to any edge caller yet.

use std::future::Future;

use crate::writer_execution::{DbFailure, Value, WriterDb};

/// One short-lived client, never retained across queue, R2 or meter operations.
/// Production supplies a Workers timer factory; native tests supply controlled
/// deadlines while driving a real `tokio_postgres::Client` over a test socket.
pub struct PgWriterDb<'a, D> {
    _client: &'a tokio_postgres::Client,
    _deadline: D,
}

impl<'a, D> PgWriterDb<'a, D> {
    pub fn new(client: &'a tokio_postgres::Client, deadline: D) -> Self {
        Self {
            _client: client,
            _deadline: deadline,
        }
    }
}

impl<D, F> WriterDb for PgWriterDb<'_, D>
where
    D: Fn() -> F,
    F: Future<Output = ()>,
{
    async fn execute(&self, _sql: &'static str, _params: &[Value]) -> Result<u64, DbFailure> {
        Err(DbFailure { sqlstate: None })
    }

    async fn query(
        &self,
        _sql: &'static str,
        _params: &[Value],
    ) -> Result<Vec<Vec<Value>>, DbFailure> {
        Err(DbFailure { sqlstate: None })
    }
}
