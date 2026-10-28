//! Waker-sensitivity pin for `worker_entry::with_deadline`.
//!
//! Companion to the acceptance suites, which this file does not modify: the
//! shared helper must register the caller's waker on its timer, not merely
//! become ready under repeated polling. One poll followed by parking the test
//! thread until the waker fires proves expiry arrives through the wake — the
//! only re-poll signal a real executor provides. No Worker sockets, database,
//! or test-side timeout result: the watchdog only fails a stuck test.
//!
//! Breaks caught: polling the deadline with a foreign or no-op waker context
//! while the operation itself never resolves.

use nntune_api::worker_entry::with_deadline;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::time::{Duration, Instant};

const WATCHDOG: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
struct Failure(&'static str);

struct Never;

impl Future for Never {
    type Output = Result<u8, Failure>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Pending
    }
}

struct Parker {
    woken: AtomicBool,
    thread: std::thread::Thread,
}

unsafe fn clone_parker(data: *const ()) -> RawWaker {
    Arc::increment_strong_count(data as *const Parker);
    RawWaker::new(data, &PARKER_VTABLE)
}

unsafe fn wake_parker(data: *const ()) {
    let parker = Arc::from_raw(data as *const Parker);
    parker.woken.store(true, Ordering::SeqCst);
    parker.thread.unpark();
}

unsafe fn wake_parker_by_ref(data: *const ()) {
    let parker = &*(data as *const Parker);
    parker.woken.store(true, Ordering::SeqCst);
    parker.thread.unpark();
}

unsafe fn drop_parker(data: *const ()) {
    drop(Arc::from_raw(data as *const Parker));
}

static PARKER_VTABLE: RawWakerVTable =
    RawWakerVTable::new(clone_parker, wake_parker, wake_parker_by_ref, drop_parker);

fn parker_waker(parker: &Arc<Parker>) -> Waker {
    let raw = RawWaker::new(
        Arc::into_raw(Arc::clone(parker)) as *const (),
        &PARKER_VTABLE,
    );
    // SAFETY: the vtable manages one Arc strong count per RawWaker exactly
    // like the Waker contract requires (clone adds, drop/wake releases).
    unsafe { Waker::from_raw(raw) }
}

#[test]
fn expiry_arrives_through_the_callers_wake_without_repolling() {
    let parker = Arc::new(Parker {
        woken: AtomicBool::new(false),
        thread: std::thread::current(),
    });
    let waker = parker_waker(&parker);
    let mut cx = Context::from_waker(&waker);
    let mut deadline = Box::pin(with_deadline(Never, 25, Failure("caller timeout")));
    let start = Instant::now();
    // Exactly one poll: Pending, with the timer armed on OUR waker.
    assert!(deadline.as_mut().poll(&mut cx).is_pending());
    // Park until the timer's wake — no manual re-polling. The independent
    // watchdog only fails a test whose wake never arrives.
    let end = start + WATCHDOG;
    while !parker.woken.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < end,
            "expiry never woke the caller within the 2s watchdog"
        );
        std::thread::park_timeout(Duration::from_millis(50));
    }
    assert!(
        start.elapsed() >= Duration::from_millis(25),
        "the wake must follow the budget, not precede it"
    );
    // The wake delivered expiry: one more poll observes it.
    assert_eq!(
        deadline.as_mut().poll(&mut cx),
        Poll::Ready(Err(Failure("caller timeout")))
    );
}
