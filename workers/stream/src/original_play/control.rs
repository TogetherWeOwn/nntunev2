//! Request-owned cancellation and grant timer, independent of the runtime.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll, Wake, Waker},
};

use nntune_core::original_source::{
    reader::{AsyncReadControl, ReadControl},
    SourceError,
};

use super::OriginalControl;

#[derive(Default)]
struct RequestWake {
    cancelled: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl RequestWake {
    fn notify(&self) {
        // Never invoke a task while holding the registration lock.
        let waker = self.waker.lock().expect("request waker lock").take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl Wake for RequestWake {
    fn wake(self: Arc<Self>) {
        self.notify();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notify();
    }
}

/// Explicit request cancellation only; this does not release an account or
/// cancel its physical transport. Clones do not own the response lifetime.
#[derive(Clone)]
pub struct RequestCancellation(Arc<RequestWake>);

impl RequestCancellation {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.notify();
    }
}

/// A grant-bound control with an injected clock and deadline future. The timer
/// must complete at the absolute grant expiry, even if first polled later.
/// The account supplier retains its separate operation deadline/close duties.
/// Drop releases the timer; runtime wrappers must also remove abort listeners.
pub struct OriginalRequestControl<T, N> {
    expiry_unix: u64,
    timer: Pin<Box<T>>,
    now: N,
    wake: Arc<RequestWake>,
    expired: bool,
}

impl<T: Future<Output = ()>, N: Fn() -> u64> OriginalRequestControl<T, N> {
    pub fn new(expiry_unix: u64, timer: T, now: N) -> Self {
        Self {
            expiry_unix,
            timer: Box::pin(timer),
            now,
            wake: Arc::default(),
            expired: false,
        }
    }

    pub fn cancellation(&self) -> RequestCancellation {
        RequestCancellation(self.wake.clone())
    }
}

impl<T: Future<Output = ()>, N: Fn() -> u64> ReadControl for OriginalRequestControl<T, N> {
    fn check(&mut self) -> Result<(), SourceError> {
        self.expired |= self.now_unix() >= self.expiry_unix;
        if self.expired || self.wake.cancelled.load(Ordering::Acquire) {
            return Err(SourceError::Cancelled);
        }
        Ok(())
    }
}

impl<T: Future<Output = ()>, N: Fn() -> u64> AsyncReadControl for OriginalRequestControl<T, N> {
    fn poll_cancel(&mut self, cx: &mut Context<'_>) -> Poll<SourceError> {
        // The SDK timer may retain its first waker. Give it a stable relay,
        // replacing only the actual task registration on every poll.
        *self.wake.waker.lock().expect("request waker lock") = Some(cx.waker().clone());
        if let Err(error) = self.check() {
            return Poll::Ready(error);
        }
        let relay = Waker::from(self.wake.clone());
        if self
            .timer
            .as_mut()
            .poll(&mut Context::from_waker(&relay))
            .is_ready()
        {
            self.expired = true;
        }
        match self.check() {
            Ok(()) => Poll::Pending,
            Err(error) => Poll::Ready(error),
        }
    }
}

impl<T: Future<Output = ()>, N: Fn() -> u64> OriginalControl for OriginalRequestControl<T, N> {
    fn now_unix(&self) -> u64 {
        (self.now)()
    }
}

impl<T, N> Drop for OriginalRequestControl<T, N> {
    fn drop(&mut self) {
        // A retained cancellation handle must not retain or wake a dead task.
        self.wake.waker.lock().expect("request waker lock").take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct Event {
        ready: AtomicBool,
        first_waker: Mutex<Option<Waker>>,
        drops: AtomicUsize,
        polls: AtomicUsize,
    }

    impl Event {
        fn fire(&self) {
            self.ready.store(true, Ordering::Release);
            if let Some(waker) = self.first_waker.lock().unwrap().take() {
                waker.wake();
            }
        }
    }

    struct Timer(Arc<Event>);

    impl Future for Timer {
        type Output = ();

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            self.0.polls.fetch_add(1, Ordering::Relaxed);
            let mut waker = self.0.first_waker.lock().unwrap();
            waker.get_or_insert_with(|| cx.waker().clone());
            if self.0.ready.load(Ordering::Acquire) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            self.0.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[derive(Default)]
    struct Counter(AtomicUsize);

    impl Wake for Counter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn cancellation_and_first_waker_timer_notify_only_latest_task() {
        for cancel in [true, false] {
            let event = Arc::<Event>::default();
            let mut control = OriginalRequestControl::new(100, Timer(event.clone()), || 1);
            let first = Arc::<Counter>::default();
            let latest = Arc::<Counter>::default();
            for counter in [&first, &latest] {
                let waker = Waker::from(counter.clone());
                assert!(control
                    .poll_cancel(&mut Context::from_waker(&waker))
                    .is_pending());
            }
            if cancel {
                control.cancellation().cancel();
            } else {
                event.fire();
            }
            assert_eq!(first.0.load(Ordering::Relaxed), 0);
            assert_eq!(latest.0.load(Ordering::Relaxed), 1);
            let waker = Waker::from(latest);
            assert_eq!(
                control.poll_cancel(&mut Context::from_waker(&waker)),
                Poll::Ready(SourceError::Cancelled)
            );
            assert_eq!(control.check(), Err(SourceError::Cancelled));
        }
    }

    #[test]
    fn already_cancelled_or_expired_never_polls_timer() {
        for cancel in [true, false] {
            let event = Arc::<Event>::default();
            let mut control = OriginalRequestControl::new(
                if cancel { 100 } else { 1 },
                Timer(event.clone()),
                || 1,
            );
            if cancel {
                control.cancellation().cancel();
            }
            let waker = Waker::from(Arc::<Counter>::default());
            assert_eq!(
                control.poll_cancel(&mut Context::from_waker(&waker)),
                Poll::Ready(SourceError::Cancelled)
            );
            assert_eq!(event.polls.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn ready_timer_is_terminal_even_if_wall_clock_moves_backwards() {
        let event = Arc::<Event>::default();
        event.fire();
        let mut control = OriginalRequestControl::new(100, Timer(event), || 0);
        let waker = Waker::from(Arc::<Counter>::default());
        assert_eq!(
            control.poll_cancel(&mut Context::from_waker(&waker)),
            Poll::Ready(SourceError::Cancelled)
        );
        assert_eq!(control.check(), Err(SourceError::Cancelled));
    }

    #[test]
    fn observed_wall_clock_expiry_is_terminal() {
        let now = std::cell::Cell::new(1);
        let mut control = OriginalRequestControl::new(100, std::future::pending(), || now.get());
        assert_eq!(control.check(), Ok(()));
        now.set(100);
        assert_eq!(control.check(), Err(SourceError::Cancelled));
        now.set(1);
        assert_eq!(control.check(), Err(SourceError::Cancelled));
    }

    #[test]
    fn dropping_unpolled_or_pending_control_drops_owned_timer_once() {
        for poll in [true, false] {
            let event = Arc::<Event>::default();
            let mut control = OriginalRequestControl::new(100, Timer(event.clone()), || 1);
            let cancellation = control.cancellation();
            let counter = Arc::<Counter>::default();
            if poll {
                let waker = Waker::from(counter.clone());
                assert!(control
                    .poll_cancel(&mut Context::from_waker(&waker))
                    .is_pending());
            }
            drop(control);
            assert_eq!(event.drops.load(Ordering::Relaxed), 1);
            cancellation.cancel();
            assert_eq!(counter.0.load(Ordering::Relaxed), 0);
            assert_eq!(event.drops.load(Ordering::Relaxed), 1);
        }
    }
}
