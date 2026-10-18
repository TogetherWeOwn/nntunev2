//! Acceptance pin: `Singleflight::new()` default 5 s leader-replacement
//! wiring.
//!
//! Tests-only pin of current behavior; do not change implementation to make
//! these pass. If they fail, the default timeout wiring changed: update
//! `workers/stream/README.md` (Stampede guard section) and this file
//! together, with reviewer sign-off.
//!
//! What is pinned:
//!
//! 1. `SINGLEFLIGHT_WAIT_TIMEOUT_MS == 5_000` (the literal the README
//!    documents).
//! 2. `Singleflight::new()` wires that default: a waiter parked on a
//!    wedged leader re-runs leader election after ~5 s, exactly one waiter
//!    becomes the replacement leader, and every waiter takes the
//!    replacement's success.
//!
//! Sensitivity (read before "optimizing" this test): the replacement test
//! costs one real ~5 s wait against the production default — that is the
//! point; a short `with_timeout` would pin the mechanism but not the
//! default wiring. Bounds are `5 s..=9 s`: the lower bound is exact
//! because `Singleflight::wait` uses `Condvar::wait_timeout_while`, which
//! re-waits on spurious wakeups until the full timeout elapses (an early
//! wake cannot read as a timeout), and the clock starts before the waiter
//! parks, so `waited >= 5 s` always; the upper bound tolerates loaded CI
//! runners while still failing a 2x timeout drift (~10.5 s).
//! The wedged leader is parked on a channel the test releases (bounded
//! `recv_timeout` of 20 s), so a regression fails with an assertion —
//! elapsed landing above the upper bound, or a third upstream fetch —
//! instead of hanging the suite. The replacement fetch holds its flight
//! open 500 ms so the second waiter joins it rather than racing a
//! release-and-reelect; that hold is simulated upstream latency inside
//! the fetch closure, never a guess about thread startup (startup
//! handoffs are the `leader_registered` flag and the waiter `Barrier`).

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc, Barrier, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

use nntune_stream::stampede::{flight_key, Singleflight};

/// `Singleflight::new()` replaces a wedged leader after ~5 s and the
/// waiters take the replacement's success (not a third fetch, not a hang).
#[test]
fn new_replaces_wedged_leader_after_about_5s_and_waiters_take_replacement_success() {
    const LOWER: Duration = Duration::from_secs(5);
    const UPPER: Duration = Duration::from_secs(9);

    let group = Arc::new(Singleflight::<Vec<u8>, String>::new());
    let upstream_calls = Arc::new(AtomicUsize::new(0));
    let leader_registered = Arc::new(AtomicBool::new(false));
    let key = flight_key("<default-timeout-seg@synthetic.test>");

    // The wedged leader parks here until the test releases it, so "still
    // fetching" is a test-controlled fact. Bounded so a regression fails
    // instead of hanging the suite.
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);

    thread::scope(|s| {
        let (group_l, calls_l, registered_l) = (
            Arc::clone(&group),
            Arc::clone(&upstream_calls),
            Arc::clone(&leader_registered),
        );
        let key_l = key.clone();
        let leader = s.spawn(move || {
            group_l.call(&key_l, || {
                calls_l.fetch_add(1, Ordering::SeqCst);
                registered_l.store(true, Ordering::SeqCst);
                let _ = release_rx
                    .lock()
                    .expect("release gate lock")
                    .recv_timeout(Duration::from_secs(20));
                Ok::<_, String>(b"leader-bytes".to_vec())
            })
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while !leader_registered.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "leader never registered");
            thread::yield_now();
        }

        // Both waiters rendezvous so each is parked on the leader well
        // before its ~5 s timeout fires; exactly one of them becomes the
        // replacement leader and the other joins it.
        let start = Arc::new(Barrier::new(2));
        let mut waiters = Vec::with_capacity(2);
        for _ in 0..2 {
            let (group_w, calls_w, start_w) = (
                Arc::clone(&group),
                Arc::clone(&upstream_calls),
                Arc::clone(&start),
            );
            let key_w = key.clone();
            waiters.push(s.spawn(move || {
                start_w.wait();
                let t0 = Instant::now();
                let result = group_w.call(&key_w, || {
                    calls_w.fetch_add(1, Ordering::SeqCst);
                    // Hold the replacement flight open so the other waiter
                    // joins it instead of racing a release-and-reelect.
                    thread::sleep(Duration::from_millis(500));
                    Ok::<_, String>(b"replacement-bytes".to_vec())
                });
                (result, t0.elapsed())
            }));
        }

        for waiter in waiters {
            let (result, waited) = waiter.join().expect("waiter thread");
            assert_eq!(
                result.unwrap(),
                b"replacement-bytes",
                "waiters take the replacement success"
            );
            assert!(
                (LOWER..=UPPER).contains(&waited),
                "Singleflight::new() must replace a wedged leader after ~5 s \
                 (generous bounds {LOWER:?}..={UPPER:?}), waited {waited:?}"
            );
        }

        release_tx
            .send(())
            .expect("wedged leader is parked on the release gate");
        assert_eq!(
            leader.join().expect("leader thread").unwrap(),
            b"leader-bytes",
            "the replaced leader still completes with its own result"
        );
    });

    assert_eq!(
        upstream_calls.load(Ordering::SeqCst),
        2,
        "a wedged leader plus its replacement are the only upstream fetches; \
         no waiter may fetch a third time"
    );
}
