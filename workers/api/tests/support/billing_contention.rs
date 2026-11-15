//! Coordination helpers for disposable PostgreSQL contention tests.

use std::time::Duration;
use tokio::{sync::oneshot, task::JoinHandle, time::timeout};
use tokio_postgres::Client;

const PHASE_TIMEOUT: Duration = Duration::from_secs(5);

/// Signals that a client task has reached its next command, not that PostgreSQL
/// has started or blocked that command. Observe the server wait separately.
pub fn phase() -> (oneshot::Sender<()>, oneshot::Receiver<()>) {
    oneshot::channel()
}

pub async fn reached(receiver: oneshot::Receiver<()>) -> Result<(), String> {
    timeout(PHASE_TIMEOUT, receiver)
        .await
        .map_err(|_| "phase barrier timed out".to_string())?
        .map_err(|_| "client exited before phase barrier".to_string())
}

/// Require both the requested ungranted relation lock and the actual blocker.
/// The independent observer must not be either participant's connection.
/// An early completion is an error even when the command itself succeeded.
pub async fn relation_wait<T: Send + 'static>(
    observer: &Client,
    task: &mut JoinHandle<T>,
    blocker_pid: i32,
    waiter_pid: i32,
    relation: &str,
    mode: &str,
) -> Result<(), String> {
    timeout(PHASE_TIMEOUT, async {
        loop {
            tokio::select! {
                biased;
                _ = &mut *task => return Err("lock finished before wait barrier".to_string()),
                row = observer.query_one(
                    "SELECT $1 = ANY(pg_blocking_pids($2)) AND EXISTS (SELECT 1 FROM pg_locks WHERE pid = $2 AND relation = to_regclass($3) AND mode = $4 AND NOT granted)",
                    &[&blocker_pid, &waiter_pid, &relation, &mode],
                ) => {
                    if row.map_err(|e| e.to_string())?.get::<_, bool>(0) {
                        return Ok(());
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| "lock wait barrier timed out".to_string())?
}
