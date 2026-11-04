use super::*;
use std::{
    future::{pending, ready},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Wake, Waker},
};

const NOW: u64 = 1_700_000_000_125;
const TRACK: &str = "00000001-1234-4234-8234-123456789abc";
const KEY: &[u8] = &[7; 32];

#[derive(Default)]
struct SignalState {
    ready: Cell<bool>,
    polls: Cell<usize>,
    dropped: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}
impl SignalState {
    fn fire(&self) {
        self.ready.set(true);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}
struct TestSignal(Rc<SignalState>);
impl Future for TestSignal {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.0.polls.set(self.0.polls.get() + 1);
        *self.0.waker.borrow_mut() = Some(cx.waker().clone());
        if self.0.ready.get() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}
impl Drop for TestSignal {
    fn drop(&mut self) {
        self.0.dropped.set(true);
    }
}
fn signal(state: &Rc<SignalState>) -> Signal<'static> {
    Box::pin(TestSignal(state.clone()))
}
#[derive(Default)]
struct Wakes(AtomicUsize);
impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}
fn token() -> String {
    grant::issue_v2("user", TRACK, "play", 1, NOW / 1000 + 60, KEY)
}
fn control<'a>(clock: &'a dyn Fn() -> u64) -> RequestControl<'a> {
    RequestControl::new(
        &token(),
        TRACK,
        KEY,
        clock,
        NOW + 120_000,
        Box::pin(pending()),
        |_| Box::pin(pending()),
    )
    .unwrap()
}
fn poll<T>(future: Pin<&mut impl Future<Output = T>>, waker: &Waker) -> Poll<T> {
    future.poll(&mut Context::from_waker(waker))
}

#[test]
fn invalid_authority_creates_no_timer() {
    let clock = || NOW;
    let timer_calls = Cell::new(0);
    let v1 = grant::issue("user", TRACK, NOW / 1000 + 60, KEY);
    let wrong = grant::issue_v2("user", "other", "play", 1, NOW / 1000 + 60, KEY);
    let expired = grant::issue_v2("user", TRACK, "play", 1, NOW / 1000, KEY);
    for (token, track, key, deadline) in [
        ("bad", TRACK, KEY, NOW + 1),
        (v1.as_str(), TRACK, KEY, NOW + 1),
        (wrong.as_str(), TRACK, KEY, NOW + 1),
        (expired.as_str(), TRACK, KEY, NOW + 1),
        (token().as_str(), TRACK, &KEY[..31], NOW + 1),
        (token().as_str(), "noncanonical", KEY, NOW + 1),
        (token().as_str(), TRACK, KEY, NOW),
        (token().as_str(), TRACK, KEY, NOW - 1),
    ] {
        let result = RequestControl::new(
            token,
            track,
            key,
            &clock,
            deadline,
            Box::pin(pending()),
            |_| {
                timer_calls.set(timer_calls.get() + 1);
                Box::pin(pending())
            },
        );
        assert!(matches!(result, Err(SourceError::Cancelled)));
    }
    assert_eq!(timer_calls.get(), 0);
}

#[test]
fn timer_is_capped_by_request_or_grant_with_subsecond_precision() {
    let clock = || NOW;
    for (deadline, remaining) in [(NOW + 2000, 2000), (u64::MAX, 59_875)] {
        let observed = Cell::new(0);
        let mut control = RequestControl::new(
            &token(),
            TRACK,
            KEY,
            &clock,
            deadline,
            Box::pin(pending()),
            |ms| {
                observed.set(ms);
                Box::pin(pending())
            },
        )
        .unwrap();
        assert_eq!(observed.get(), remaining);
        assert_eq!(control.track_id(), TRACK);
        assert_eq!(control.now_unix(), NOW / 1000);
        assert_eq!(control.check(), Ok(()));
    }
}

#[test]
fn clock_rollback_permanently_poisons_every_clone() {
    let now = Cell::new(NOW);
    let clock = || now.get();
    let mut first = control(&clock);
    let mut second = first.clone();
    now.set(NOW + 1000);
    first.check().unwrap();
    assert_eq!(second.now_unix(), (NOW + 1000) / 1000);
    now.set(NOW + 999);
    assert_eq!(second.check(), Err(SourceError::Cancelled));
    now.set(NOW + 1001);
    assert_eq!(first.check(), Err(SourceError::Cancelled));
}

#[test]
fn expiry_is_terminal_even_with_a_pending_timer() {
    let now = Cell::new(NOW);
    let clock = || now.get();
    let mut authority = control(&clock);
    now.set((NOW / 1000 + 60) * 1000);
    assert_eq!(authority.check(), Err(SourceError::Cancelled));
    now.set(NOW);
    assert_eq!(authority.check(), Err(SourceError::Cancelled));
}

#[test]
fn request_deadline_precedes_grant_expiry() {
    let now = Cell::new(NOW);
    let clock = || now.get();
    let mut authority = RequestControl::new(
        &token(),
        TRACK,
        KEY,
        &clock,
        NOW + 1,
        Box::pin(pending()),
        |_| Box::pin(pending()),
    )
    .unwrap();
    now.set(NOW + 1);
    assert_eq!(authority.check(), Err(SourceError::Cancelled));
}

#[test]
fn cancellation_and_timer_wake_pending_work_and_drop_it() {
    for fire_abort in [true, false] {
        let abort = Rc::new(SignalState::default());
        let timer = Rc::new(SignalState::default());
        let work = Rc::new(SignalState::default());
        let clock = || NOW;
        let authority = RequestControl::new(
            &token(),
            TRACK,
            KEY,
            &clock,
            NOW + 100,
            signal(&abort),
            |_| signal(&timer),
        )
        .unwrap();
        let wakes = Arc::new(Wakes::default());
        let waker = Waker::from(wakes.clone());
        let mut running = Box::pin(authority.run(async {
            TestSignal(work.clone()).await;
            Ok(())
        }));
        assert!(poll(running.as_mut(), &waker).is_pending());
        if fire_abort {
            abort.fire();
        } else {
            timer.fire();
        }
        assert!(wakes.0.load(Ordering::Relaxed) > 0);
        assert_eq!(
            poll(running.as_mut(), &waker),
            Poll::Ready(Err(SourceError::Cancelled))
        );
        assert!(work.dropped.get());
        assert_eq!(authority.clone().check(), Err(SourceError::Cancelled));
    }
}

#[test]
fn ready_signals_win_before_work_and_are_not_repolled() {
    for fire_abort in [true, false] {
        let abort = Rc::new(SignalState::default());
        let timer = Rc::new(SignalState::default());
        if fire_abort {
            abort.fire();
        } else {
            timer.fire();
        }
        let clock = || NOW;
        let authority = RequestControl::new(
            &token(),
            TRACK,
            KEY,
            &clock,
            NOW + 100,
            signal(&abort),
            |_| signal(&timer),
        )
        .unwrap();
        let calls = Cell::new(0);
        let mut running = Box::pin(authority.run(async {
            calls.set(1);
            Ok(())
        }));
        let waker = Waker::noop();
        assert_eq!(
            poll(running.as_mut(), waker),
            Poll::Ready(Err(SourceError::Cancelled))
        );
        let polls = (abort.polls.get(), timer.polls.get());
        for _ in 0..2 {
            assert_eq!(
                authority.poll_cancel(&mut Context::from_waker(waker)),
                Poll::Ready(SourceError::Cancelled)
            );
        }
        assert_eq!((abort.polls.get(), timer.polls.get()), polls);
        assert_eq!(calls.get(), 0);
    }
}

#[test]
fn signals_and_expiry_during_success_discard_the_result() {
    for mode in 0..3 {
        let abort = Rc::new(SignalState::default());
        let timer = Rc::new(SignalState::default());
        let now = Cell::new(NOW);
        let clock = || now.get();
        let authority = RequestControl::new(
            &token(),
            TRACK,
            KEY,
            &clock,
            NOW + 100,
            signal(&abort),
            |_| signal(&timer),
        )
        .unwrap();
        let mut running = Box::pin(authority.run(async {
            match mode {
                0 => abort.fire(),
                1 => timer.fire(),
                _ => now.set(NOW + 100),
            }
            Ok(123)
        }));
        assert_eq!(
            poll(running.as_mut(), Waker::noop()),
            Poll::Ready(Err(SourceError::Cancelled))
        );
    }
}

#[test]
fn clock_mutation_while_polling_signal_is_checked() {
    let now = Cell::new(NOW);
    let clock = || now.get();
    let authority = RequestControl::new(
        &token(),
        TRACK,
        KEY,
        &clock,
        NOW + 100,
        Box::pin(poll_fn(|_| {
            now.set(NOW - 1);
            Poll::Pending
        })),
        |_| Box::pin(pending()),
    )
    .unwrap();
    assert_eq!(
        authority.poll_cancel(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(SourceError::Cancelled)
    );
}

#[test]
fn dropping_polled_work_stops_siblings_without_claiming_account_release() {
    let clock = || NOW;
    let authority = control(&clock);
    let mut sibling = authority.clone();
    let dropped = Rc::new(SignalState::default());
    let mut running = Box::pin(authority.run(async {
        TestSignal(dropped.clone()).await;
        Ok(())
    }));
    assert!(poll(running.as_mut(), Waker::noop()).is_pending());
    drop(running);
    assert!(dropped.dropped.get());
    assert_eq!(sibling.check(), Err(SourceError::Cancelled));
}

#[test]
fn unpolled_work_does_nothing_and_does_not_stop_authority() {
    let clock = || NOW;
    let mut authority = control(&clock);
    let calls = Cell::new(0);
    drop(authority.run(async {
        calls.set(1);
        Ok(())
    }));
    assert_eq!(calls.get(), 0);
    assert_eq!(authority.check(), Ok(()));
}

#[test]
fn success_preserves_authority_but_failure_is_terminal() {
    let clock = || NOW;
    let authority = control(&clock);
    let mut success = Box::pin(authority.run(ready(Ok(123))));
    assert_eq!(poll(success.as_mut(), Waker::noop()), Poll::Ready(Ok(123)));
    drop(success);
    assert_eq!(authority.clone().check(), Ok(()));
    let mut failed = Box::pin(authority.run(ready(Err::<(), _>(SourceError::Transport))));
    assert_eq!(
        poll(failed.as_mut(), Waker::noop()),
        Poll::Ready(Err(SourceError::Transport))
    );
    assert_eq!(authority.clone().check(), Err(SourceError::Cancelled));
}

#[test]
fn explicit_stop_wakes_all_live_handles() {
    let clock = || NOW;
    let authority = control(&clock);
    let sibling = authority.clone();
    let first = Arc::new(Wakes::default());
    let second = Arc::new(Wakes::default());
    for (control, wakes) in [(&authority, &first), (&sibling, &second)] {
        assert!(control
            .poll_cancel(&mut Context::from_waker(&Waker::from(wakes.clone())))
            .is_pending());
    }
    authority.stop();
    assert!(first.0.load(Ordering::Relaxed) > 0);
    assert!(second.0.load(Ordering::Relaxed) > 0);
    assert_eq!(sibling.clone().check(), Err(SourceError::Cancelled));
}

#[test]
fn signals_wake_all_handles_even_after_last_polling_handle_drops() {
    for fire_abort in [true, false] {
        let clock = || NOW;
        let abort = Rc::new(SignalState::default());
        let timer = Rc::new(SignalState::default());
        let authority = RequestControl::new(
            &token(),
            TRACK,
            KEY,
            &clock,
            NOW + 100,
            signal(&abort),
            |_| signal(&timer),
        )
        .unwrap();
        let sibling = authority.clone();
        let first = Arc::new(Wakes::default());
        let second = Arc::new(Wakes::default());
        for (control, wakes) in [(&authority, &first), (&sibling, &second)] {
            assert!(control
                .poll_cancel(&mut Context::from_waker(&Waker::from(wakes.clone())))
                .is_pending());
        }
        drop(sibling);
        if fire_abort {
            abort.fire();
        } else {
            timer.fire();
        }
        assert!(first.0.load(Ordering::Relaxed) > 0);
        assert_eq!(
            authority.poll_cancel(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(SourceError::Cancelled)
        );
    }
}

#[test]
fn dropped_clone_registration_does_not_accumulate() {
    let clock = || NOW;
    let authority = control(&clock);
    for _ in 0..100 {
        drop(authority.clone());
    }
    assert!(authority.state.waiters.borrow().len() <= 2);
}

#[test]
fn concurrent_runs_on_one_handle_keep_independent_cancel_registrations() {
    for mode in 0..3 {
        let clock = || NOW;
        let abort = Rc::new(SignalState::default());
        let timer = Rc::new(SignalState::default());
        let authority = RequestControl::new(
            &token(),
            TRACK,
            KEY,
            &clock,
            NOW + 100,
            signal(&abort),
            |_| signal(&timer),
        )
        .unwrap();
        let first_wakes = Arc::new(Wakes::default());
        let second_wakes = Arc::new(Wakes::default());
        let first_waker = Waker::from(first_wakes.clone());
        let second_waker = Waker::from(second_wakes.clone());
        let mut first = Box::pin(authority.run(pending::<Result<(), SourceError>>()));
        let mut second = Box::pin(authority.run(pending::<Result<(), SourceError>>()));
        assert!(poll(first.as_mut(), &first_waker).is_pending());
        assert!(poll(second.as_mut(), &second_waker).is_pending());
        match mode {
            0 => abort.fire(),
            1 => timer.fire(),
            _ => authority.stop(),
        }
        assert!(
            first_wakes.0.load(Ordering::Relaxed) > 0,
            "first run stranded in mode {mode}"
        );
        assert!(
            second_wakes.0.load(Ordering::Relaxed) > 0,
            "second run stranded in mode {mode}"
        );
        assert_eq!(
            poll(first.as_mut(), &first_waker),
            Poll::Ready(Err(SourceError::Cancelled))
        );
        assert_eq!(
            poll(second.as_mut(), &second_waker),
            Poll::Ready(Err(SourceError::Cancelled))
        );
    }
}

#[test]
fn last_control_drop_owns_signal_cleanup() {
    let clock = || NOW;
    let abort = Rc::new(SignalState::default());
    let timer = Rc::new(SignalState::default());
    let authority = RequestControl::new(
        &token(),
        TRACK,
        KEY,
        &clock,
        NOW + 100,
        signal(&abort),
        |_| signal(&timer),
    )
    .unwrap();
    let sibling = authority.clone();
    drop(authority);
    assert!(!abort.dropped.get() && !timer.dropped.get());
    drop(sibling);
    assert!(abort.dropped.get() && timer.dropped.get());
}
