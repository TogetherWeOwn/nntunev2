//! Disposable-route cancellation probe; not used by the release Worker.

use crate::billing_owned_socket::SocketGuard;
use futures_util::future::{select, Either};
use std::{future::Future, time::Duration};
use tokio_postgres::{CancelToken, NoTls};

/// Enclose operation startup, barrier observation, cancellation and independent
/// verification in one window shorter than the server's four-second lock limit.
/// Construct the operation inside `probe`, not before this timer starts.
pub(crate) async fn within_blocked_window<T, E>(
    probe: impl FnOnce() -> T,
    timeout: E,
) -> Result<(), E>
where
    T: Future<Output = Result<(), E>>,
{
    let deadline = worker::Delay::from(Duration::from_secs(2));
    match select(Box::pin(deadline), Box::pin(probe())).await {
        Either::Left(_) => Err(timeout),
        Either::Right((result, _)) => result,
    }
}

/// Negative control: with no cancel request, independently verify that the
/// same backend is still blocked by this observer on both sides of a delay.
/// The caller holds the advisory lock and keeps the target operation polled.
pub(crate) async fn observe_no_cancel(
    observer: &tokio_postgres::Client,
    pid: i32,
) -> Result<(), &'static str> {
    for attempt in 0..2 {
        if attempt != 0 {
            worker::Delay::from(Duration::from_millis(250)).await;
        }
        let blocked: bool = observer
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1 AND wait_event_type = 'Lock' AND wait_event = 'advisory' AND pg_backend_pid() = ANY(pg_blocking_pids(pid)))",
                &[&pid],
            )
            .await
            .map_err(|_| "negative control observation failed")?
            .get(0);
        if !blocked {
            return Err("negative control backend did not remain blocked");
        }
    }
    Ok(())
}

/// A successful send is not an acknowledgement; the caller must independently
/// observe the target backend and rollback inside `within_blocked_window`.
pub(crate) async fn send_cancel(token: &CancelToken) -> Result<(), &'static str> {
    let guard = SocketGuard::default();
    let socket = guard
        .open("127.0.0.1", 5432)
        .map_err(|_| "cancel socket open failed")?;
    socket
        .opened()
        .await
        .map_err(|_| "cancel socket connection failed")?;
    token
        .cancel_query_raw(socket, NoTls)
        .await
        .map_err(|_| "cancel request failed")
}
