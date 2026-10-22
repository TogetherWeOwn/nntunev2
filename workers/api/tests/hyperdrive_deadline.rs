//! Native contract for the deadline used by PgStore::connect. No Worker sockets,
//! database, or test-side timeout result: the watchdog only fails a stuck test.
//!
//! Builder seam: export worker_entry::with_deadline(future, timeout_ms, error),
//! generic over Result<T, E>, using the platform timer. The source pins in
//! hyperdrive_store_deadline_wiring.rs require PgStore to use THIS helper.
//! The absent export is intentional on the test-only baseline; do not introduce
//! a test fallback or merge this branch without its implementation.
//!
//! Breaks caught: awaiting I/O before racing it, returning success on expiry,
//! discarding the caller's error, returning before the deadline, retaining the
//! losing future, delaying a ready result, or resetting the overall budget.

use nntune_api::worker_entry::with_deadline;
use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

const WATCHDOG: Duration = Duration::from_secs(2);

// Deliberately neither Clone nor Copy: the helper returns the supplied error,
// rather than requiring a Worker error formatter or reconstructing its value.
#[derive(Debug, PartialEq, Eq)]
struct Failure(&'static str);

struct Never<'a> {
    polls: &'a Cell<usize>,
    drops: &'a Cell<usize>,
}

impl Future for Never<'_> {
    type Output = Result<u8, Failure>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        self.polls.set(self.polls.get() + 1);
        Poll::Pending
    }
}

impl Drop for Never<'_> {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

// Retain the caller-owned future through the cancellation assertions below.
// Test cleanup must not manufacture evidence that the helper dropped its loser.
fn finish<F: Future>(mut future: Pin<&mut F>) -> F::Output {
    let end = Instant::now() + WATCHDOG;
    let mut cx = Context::from_waker(std::task::Waker::noop());
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        assert!(
            Instant::now() < end,
            "production deadline still pending at the independent 2s watchdog"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn pending_io_expires_with_the_callers_error_and_drops_before_returning() {
    let polls = Cell::new(0);
    let drops = Cell::new(0);
    let mut deadline = Box::pin(with_deadline(
        Never {
            polls: &polls,
            drops: &drops,
        },
        25,
        Failure("caller timeout"),
    ));
    // Start the clock before the first poll, not before constructing the future.
    let start = Instant::now();
    assert_eq!(finish(deadline.as_mut()), Err(Failure("caller timeout")));
    assert!(polls.get() > 0, "the pending operation must actually start");
    assert!(
        start.elapsed() >= Duration::from_millis(25),
        "expiry must not be an immediate unconditional error"
    );
    assert_eq!(
        drops.get(),
        1,
        "losing I/O must drop before the helper returns"
    );
    drop(deadline);
    assert_eq!(drops.get(), 1);
}

#[test]
fn ready_success_does_not_wait_out_the_budget() {
    let mut deadline = Box::pin(with_deadline(
        async { Ok::<_, Failure>(42u8) },
        5_000,
        Failure("unused timeout"),
    ));
    assert_eq!(finish(deadline.as_mut()), Ok(42));
}

#[test]
fn ready_driver_error_is_not_replaced_with_the_expiry_error() {
    let mut deadline = Box::pin(with_deadline(
        async { Err::<u8, _>(Failure("driver refused")) },
        5_000,
        Failure("unused timeout"),
    ));
    assert_eq!(finish(deadline.as_mut()), Err(Failure("driver refused")));
}

#[test]
fn overall_budget_includes_time_spent_in_the_first_phase() {
    let polls = Cell::new(0);
    let drops = Cell::new(0);
    let mut deadline = Box::pin(with_deadline(
        async {
            with_deadline(
                async {
                    futures_timer::Delay::new(Duration::from_millis(25)).await;
                    Ok::<_, Failure>(())
                },
                500,
                Failure("dial timeout"),
            )
            .await?;
            with_deadline(
                Never {
                    polls: &polls,
                    drops: &drops,
                },
                500,
                Failure("handshake timeout"),
            )
            .await
        },
        100,
        Failure("overall timeout"),
    ));
    assert_eq!(finish(deadline.as_mut()), Err(Failure("overall timeout")));
    assert!(polls.get() > 0, "the second phase must be reached");
    assert_eq!(
        drops.get(),
        1,
        "overall expiry must cancel the pending phase"
    );
    drop(deadline);
}

#[test]
fn caller_cancellation_drops_the_pending_operation() {
    let polls = Cell::new(0);
    let drops = Cell::new(0);
    let mut deadline = Box::pin(with_deadline(
        Never {
            polls: &polls,
            drops: &drops,
        },
        5_000,
        Failure("unused timeout"),
    ));
    let mut cx = Context::from_waker(std::task::Waker::noop());
    assert!(deadline.as_mut().poll(&mut cx).is_pending());
    assert!(polls.get() > 0);
    assert_eq!(drops.get(), 0);
    drop(deadline);
    assert_eq!(drops.get(), 1);
}
