//! Shared request authority for the off-route article supplier and response
//! control. The shared reader/response traits forward to this same authority;
//! no second clock, timer or cancellation state is constructed. No runtime caller.

use std::{
    cell::{Cell, RefCell},
    future::{poll_fn, Future},
    pin::Pin,
    rc::{Rc, Weak},
    task::{Context, Poll, Waker},
};

use futures_util::{future::Shared, FutureExt};
use nntune_core::original_source::{
    canonical_track_id,
    reader::{AsyncReadControl, ReadControl},
    SourceError,
};

use crate::grant;

/// Runtime timers and abort signals must register the current task's waker.
/// No detached task is needed; dropping the last control drops both futures.
pub type Signal<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;
type Waiter = RefCell<Option<Waker>>;

struct State<'a> {
    track_id: String,
    deadline_ms: u64,
    now_ms: &'a dyn Fn() -> u64,
    last_ms: Cell<u64>,
    stopped: Cell<bool>,
    waiters: RefCell<Vec<Weak<Waiter>>>,
}

/// Clones share one terminal authority, clock watermark and timer. A source
/// supplier and response control must use clones of this value, not separately
/// constructed controls. No grants, references or internal identities are logged.
pub struct RequestControl<'a> {
    state: Rc<State<'a>>,
    waiter: Rc<Waiter>,
    abort: RefCell<Shared<Signal<'a>>>,
    timer: RefCell<Shared<Signal<'a>>>,
}

impl Clone for RequestControl<'_> {
    fn clone(&self) -> Self {
        let waiter = Rc::new(RefCell::new(None));
        let mut waiters = self.state.waiters.borrow_mut();
        waiters.retain(|waiter| waiter.strong_count() > 0);
        waiters.push(Rc::downgrade(&waiter));
        Self {
            state: self.state.clone(),
            waiter,
            abort: RefCell::new(self.abort.borrow().clone()),
            timer: RefCell::new(self.timer.borrow().clone()),
        }
    }
}

impl<'a> RequestControl<'a> {
    /// Verify a canonical track-bound v2 grant before creating a timer or doing
    /// catalogue/account I/O. `timer` receives the remaining milliseconds until
    /// the earlier of request deadline and grant expiry; it must wake by then.
    /// The abort future must wake on request cancellation. Runtime qualification
    /// and the hard dialing/ingress gate still precede construction.
    pub fn new(
        token: &str,
        track_id: &str,
        key: &[u8],
        now_ms: &'a dyn Fn() -> u64,
        request_deadline_ms: u64,
        abort: Signal<'a>,
        timer: impl FnOnce(u64) -> Signal<'a>,
    ) -> Result<Self, SourceError> {
        let now = now_ms();
        if key.len() < 32 || !canonical_track_id(track_id) {
            return Err(SourceError::Cancelled);
        }
        let grant = grant::verify(token, key, now / 1000).map_err(|_| SourceError::Cancelled)?;
        if grant.play_id.is_none() || grant.track_id != track_id {
            return Err(SourceError::Cancelled);
        }
        let deadline_ms = request_deadline_ms.min(grant.exp_unix.saturating_mul(1000));
        let remaining = deadline_ms
            .checked_sub(now)
            .filter(|&remaining| remaining > 0)
            .ok_or(SourceError::Cancelled)?;
        let waiter = Rc::new(RefCell::new(None));
        Ok(Self {
            state: Rc::new(State {
                track_id: track_id.into(),
                deadline_ms,
                now_ms,
                last_ms: Cell::new(now),
                stopped: Cell::new(false),
                waiters: RefCell::new(vec![Rc::downgrade(&waiter)]),
            }),
            waiter,
            abort: RefCell::new(abort.shared()),
            timer: RefCell::new(timer(remaining).shared()),
        })
    }

    pub fn track_id(&self) -> &str {
        &self.state.track_id
    }

    /// Observed authority time, never a fresh unvalidated clock read. Consumers
    /// must check/poll authority before relying on it, including after awaits.
    pub fn now_unix(&self) -> u64 {
        self.state.last_ms.get() / 1000
    }

    pub(super) fn observed_ms(&self) -> u64 {
        self.state.last_ms.get()
    }

    pub(super) fn deadline_ms(&self) -> u64 {
        self.state.deadline_ms
    }

    /// Terminal across all clones. This revokes local response/source authority;
    /// it never releases an account slot or claims physical transport closure.
    pub fn stop(&self) {
        self.state.stopped.set(true);
        let wakers: Vec<_> = self
            .state
            .waiters
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .filter_map(|waiter| waiter.borrow_mut().take())
            .collect();
        for waker in wakers {
            waker.wake();
        }
    }

    pub(super) fn check_live(&self) -> Result<(), SourceError> {
        if self.state.stopped.get() {
            return Err(SourceError::Cancelled);
        }
        let now = (self.state.now_ms)();
        let last = self.state.last_ms.get();
        self.state.last_ms.set(last.max(now));
        if now < last || now >= self.state.deadline_ms {
            self.stop();
            return Err(SourceError::Cancelled);
        }
        Ok(())
    }

    /// Shared implementation behind the reader/response cancellation seam.
    /// Signals win ties, are never re-polled after completion, and are checked
    /// alongside the clock even if one pending signal advances it while polled.
    pub fn poll_cancel(&self, cx: &mut Context<'_>) -> Poll<SourceError> {
        if self.check_live().is_err() {
            return Poll::Ready(SourceError::Cancelled);
        }
        *self.waiter.borrow_mut() = Some(cx.waker().clone());
        if Pin::new(&mut *self.abort.borrow_mut()).poll(cx).is_ready() {
            self.stop();
            return Poll::Ready(SourceError::Cancelled);
        }
        if Pin::new(&mut *self.timer.borrow_mut()).poll(cx).is_ready() {
            self.stop();
            return Poll::Ready(SourceError::Cancelled);
        }
        match self.check_live() {
            Ok(()) => Poll::Pending,
            Err(error) => Poll::Ready(error),
        }
    }

    /// Enclose catalogue/account work in the shared request authority. Each
    /// operation must additionally supply its own <=20 s timer. Cancellation
    /// wins before and after polling work, including a simultaneous success.
    /// A failed or dropped polled call poisons every clone; an unpolled call
    /// performs no I/O and leaves authority unchanged. There is no retry here.
    pub async fn run<T>(
        &self,
        work: impl Future<Output = Result<T, SourceError>>,
    ) -> Result<T, SourceError> {
        let mut guard = StopOnDrop {
            control: self.clone(),
            completed: false,
        };
        let mut work = std::pin::pin!(work);
        let result = poll_fn(|cx| {
            if let Poll::Ready(error) = guard.control.poll_cancel(cx) {
                return Poll::Ready(Err(error));
            }
            let result = work.as_mut().poll(cx);
            if let Poll::Ready(error) = guard.control.poll_cancel(cx) {
                return Poll::Ready(Err(error));
            }
            result
        })
        .await;
        guard.completed = result.is_ok();
        result
    }
}

impl ReadControl for RequestControl<'_> {
    fn check(&mut self) -> Result<(), SourceError> {
        self.check_live()
    }
}

impl AsyncReadControl for RequestControl<'_> {
    fn poll_cancel(&mut self, cx: &mut Context<'_>) -> Poll<SourceError> {
        RequestControl::poll_cancel(self, cx)
    }
}

impl crate::original_play::OriginalControl for RequestControl<'_> {
    fn now_unix(&self) -> u64 {
        RequestControl::now_unix(self)
    }
}

struct StopOnDrop<'a> {
    control: RequestControl<'a>,
    completed: bool,
}

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.control.stop();
        }
    }
}

#[cfg(test)]
mod tests;
