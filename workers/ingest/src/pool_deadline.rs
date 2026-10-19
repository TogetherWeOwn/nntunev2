//! Generic deadline race for pool stub calls.
//!
//! The probe (`pool_probe`) and body (`pool_body`) stub calls each race two
//! sequential operations (headers fetch, then body drain) against ONE shared
//! deadline. This helper is that race, generic over the operation and the
//! deadline futures so native tests can drive it with ready/pending fakes:
//! production passes a `worker::Delay`, tests pass `ready(())` / `pending()`.
//! Same polling shape as `collect_mb_body` in `queue_consumer`: the deadline
//! is polled first, so an already-fired deadline aborts even when the
//! operation is also ready, and partial progress never re-arms it (the
//! caller reuses the SAME deadline across both stages).

use std::future::Future;
use std::task::Poll;

/// Race `op` against `deadline`, polling the deadline first.
///
/// Returns `Some(output)` when `op` completes before the deadline fires,
/// `None` when the deadline fires first (or is already fired). The caller
/// keeps owning `deadline` and reuses it for the second stage, so headers
/// plus body share one total budget.
pub async fn race_against_deadline<F, D, T>(op: F, deadline: &mut D) -> Option<T>
where
    F: Future<Output = T> + Unpin,
    D: Future<Output = ()> + Unpin,
{
    let mut op = op;
    std::future::poll_fn(|cx| {
        // Deadline first: an already-fired deadline aborts even when the
        // operation is also ready.
        if std::pin::Pin::new(&mut *deadline).poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        match std::pin::Pin::new(&mut op).poll(cx) {
            Poll::Ready(output) => Poll::Ready(Some(output)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}
