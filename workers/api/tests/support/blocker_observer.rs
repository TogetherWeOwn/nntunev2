//! Bounded observation of a specific PostgreSQL lock blocker.
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio_postgres::Client;

/// Drive an operation until PostgreSQL reports the expected backend as its blocker.
/// The caller retains the pinned future to release the holder and await completion.
pub async fn observe_blocked<F: Future>(
    observer: &Client,
    holder_pid: i32,
    waiter_pid: i32,
    mut pending: Pin<&mut F>,
) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let blocker_params: &[&(dyn tokio_postgres::types::ToSql + Sync)] =
            &[&holder_pid, &waiter_pid];
        loop {
            tokio::select! {
                _ = &mut pending => return Err("waiter completed before release".to_owned()),
                result = observer.query_one("SELECT $1 = ANY(pg_blocking_pids($2))", blocker_params) => {
                    let row = result.map_err(|error| error.to_string())?;
                    if row.get::<_, bool>(0) { return Ok(()); }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| "waiter did not block".to_owned())?
}
