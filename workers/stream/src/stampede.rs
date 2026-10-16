//! Stampede guard primitive: coalesce concurrent identical segment fetches
//! into exactly one upstream NNTP fetch (singleflight, keyed on the
//! normalized segment message-id — see [`flight_key`]).
//!
//! [`Singleflight`] is dependency-free (`std::sync` only) and `Sync`, so one
//! group shared across threads coalesces whoever arrives while a fetch for
//! the same key is in flight. Waiters that out-wait the leader re-run leader
//! election instead of fetching in parallel: exactly one of them becomes the
//! replacement leader and the rest join its flight. A replacement is never
//! itself replaced for slowness, and no flight is led beside a superseded
//! leader that is still fetching, so a stalled key runs at most two upstream
//! fetches at once (see [`SINGLEFLIGHT_WAIT_TIMEOUT_MS`]).
//!
//! Scope, honestly: this module is the primitive only. Nothing in the play
//! path calls it yet — wiring arrives with the Worker / pool Durable Object
//! entry point (deploy slice), which holds one group per isolate. It is also
//! native-only: the `Mutex`/`Condvar` waiter path traps on
//! `wasm32-unknown-unknown`, so `lib.rs` compiles this module out on wasm32
//! and the Worker will need an async shared-future / promise-map equivalent
//! with the same key shape and timeout semantics.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// How long a waiter parks on one flight before re-running leader election.
/// Documented in `workers/stream/README.md`. On timeout a waiter does not
/// fetch in parallel: one replaces the flight and leads, the rest join the
/// replacement. A replacement is never replaced in turn, so a stalled leader
/// costs at most two concurrent upstream fetches per key. 5 s is below the
/// NNTP client's 10 s timeout, but that timeout is per socket *read*
/// (`nntp.rs`), not per article, so a slow-but-healthy leader can still be
/// replaced once and briefly overlap its replacement; the waiters then take
/// whichever result lands first. That bounded overlap is the price of never
/// parking a waiter behind a wedged leader.
pub const SINGLEFLIGHT_WAIT_TIMEOUT_MS: u64 = 5_000;

/// Singleflight key for a segment fetch: the NZB segment message-id,
/// normalized with the same trim and `<>` strip that
/// `NntpClient::article` applies before the id goes on the wire.
///
/// The key deliberately excludes the client's `Range` header. The upstream
/// operation is a whole-article `BODY` by message-id and does not depend on
/// `Range` (the play path resolves `Range`/`206` against the fetched bytes),
/// so a seek storm asking for different ranges of one segment is the same
/// upstream fetch and must share one flight.
pub fn flight_key(segment_id: &str) -> String {
    segment_id
        .trim()
        .trim_matches(|c| c == '<' || c == '>')
        .to_string()
}

struct FlightState<T, E> {
    done: bool,
    result: Option<Result<T, E>>,
    successor: Option<Arc<Flight<T, E>>>,
}

struct Flight<T, E> {
    state: Mutex<FlightState<T, E>>,
    ready: Condvar,
    /// True when this flight is a replacement: elected by a caller that had
    /// already timed out, or led while a superseded leader was still fetching.
    /// Such a flight is never replaced for slowness, which is what bounds a key
    /// at two concurrent fetches.
    is_replacement: bool,
}

impl<T, E> Flight<T, E> {
    fn new(is_replacement: bool) -> Self {
        Self {
            state: Mutex::new(FlightState {
                done: false,
                result: None,
                successor: None,
            }),
            ready: Condvar::new(),
            is_replacement,
        }
    }
}

/// One key's registry entry. `current` is the flight new callers join;
/// `outstanding` counts the leaders inside `fetch` for this key, including a
/// superseded leader still fetching behind its replacement.
struct Slot<T, E> {
    current: Option<Arc<Flight<T, E>>>,
    outstanding: usize,
}

type Registry<T, E> = HashMap<String, Slot<T, E>>;

/// Coalesces concurrent identical fetches. The first caller for a key becomes
/// the leader and runs its `fetch`; callers that arrive while it is in flight
/// wait for its result instead of fetching. A waiter that out-waits
/// [`SINGLEFLIGHT_WAIT_TIMEOUT_MS`] re-runs leader election under the registry
/// lock: if the timed-out flight is still registered, that waiter replaces it
/// and leads the next flight while the other waiters join the replacement; if
/// a newer flight is registered, it joins that one. A replacement is never
/// replaced for slowness, and no flight is led while a superseded leader is
/// still fetching unless it is itself a replacement, so a stalled key runs at
/// most two upstream fetches at once. When a leader settles, its outcome goes
/// to its own waiters and to the replacement it was superseded by, so the
/// first result the chain produces is the one every waiter takes. Sharing is
/// in-flight only: once the registered flight's leader returns, the next call
/// fetches again — freshness is the cache's job, not this group's.
///
/// `T`/`E` need `Clone` so every waiter gets its own copy of the leader's
/// outcome, and `Send` so one group can be shared across threads.
pub struct Singleflight<T, E> {
    inner: Mutex<Registry<T, E>>,
    timeout: Duration,
}

impl<T, E> Singleflight<T, E>
where
    T: Clone + Send,
    E: Clone + Send,
{
    /// A group with the documented default waiter timeout.
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_millis(SINGLEFLIGHT_WAIT_TIMEOUT_MS))
    }

    /// A group with an explicit waiter timeout (the unit tests use short
    /// timeouts so the fallback path stays fast).
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            timeout,
        }
    }

    /// Run `fetch` for `key`, coalesced with any identical flight already in
    /// the air.
    pub fn call(&self, key: &str, fetch: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let mut fetch = Some(fetch);
        // A flight a timed-out waiter was told to join, if any.
        let mut joined: Option<Arc<Flight<T, E>>> = None;
        // Whether this caller has already timed out once. A flight it goes on
        // to lead is then a replacement, even when elected fresh after a
        // leader panic.
        let mut has_timed_out = false;
        loop {
            let flight = match joined.take() {
                Some(flight) => flight,
                None => {
                    let mut registry = self.registry();
                    let slot = registry.entry(key.to_string()).or_insert_with(|| Slot {
                        current: None,
                        outstanding: 0,
                    });
                    if let Some(flight) = slot.current.clone() {
                        flight
                    } else {
                        let is_replacement = has_timed_out || slot.outstanding > 0;
                        let flight = Arc::new(Flight::new(is_replacement));
                        slot.current = Some(Arc::clone(&flight));
                        slot.outstanding += 1;
                        drop(registry);
                        let fetch = fetch.take().expect("fetch is consumed only by a leader");
                        return self.lead(key, flight, fetch);
                    }
                }
            };

            if let Some(result) = Self::wait(&flight, self.timeout) {
                return result;
            }

            // Timed out on `flight`. Checks run under the registry lock, so a
            // leader that settles in the gap is seen as completed here.
            has_timed_out = true;
            let mut registry = self.registry();
            if let Some(result) = Self::completed(&flight) {
                return result;
            }
            if let Some(successor) = Self::successor(&flight) {
                joined = Some(successor);
                continue;
            }
            let current = registry.get(key).and_then(|slot| slot.current.clone());
            match current {
                Some(current) if Arc::ptr_eq(&current, &flight) => {
                    if flight.is_replacement {
                        joined = Some(flight);
                        continue;
                    }
                    let replacement = Arc::new(Flight::new(true));
                    flight
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .successor = Some(Arc::clone(&replacement));
                    let slot = registry
                        .get_mut(key)
                        .expect("a registered flight has a slot");
                    slot.current = Some(Arc::clone(&replacement));
                    slot.outstanding += 1;
                    drop(registry);
                    let fetch = fetch.take().expect("fetch is consumed only by a leader");
                    return self.lead(key, replacement, fetch);
                }
                Some(current) => joined = Some(current),
                // The flight's leader panicked and released the key; elect afresh.
                None => {}
            }
        }
    }

    /// Run `fetch` as the leader of `flight`, then settle: the outcome goes to
    /// the flight and to the replacement it was superseded by (if still
    /// pending), and the leader's slot is released. A panicking `fetch`
    /// releases the slot without a result; its waiters recover via timeout.
    fn lead(
        &self,
        key: &str,
        flight: Arc<Flight<T, E>>,
        fetch: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let mut unsettled = Unsettled {
            registry: &self.inner,
            key,
            flight: &flight,
            armed: true,
        };
        let result = fetch();
        unsettled.armed = false;
        let mut registry = self.registry();
        Self::publish(&flight, result.clone());
        if let Some(successor) = Self::successor(&flight) {
            Self::publish(&successor, result.clone());
        }
        release(&mut registry, key, &flight);
        result
    }

    fn registry(&self) -> MutexGuard<'_, Registry<T, E>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Set a flight's outcome for its waiters. A flight already settled keeps
    /// its first result.
    fn publish(flight: &Flight<T, E>, result: Result<T, E>) {
        let mut state = flight.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.done {
            state.result = Some(result);
            state.done = true;
            flight.ready.notify_all();
        }
    }

    /// Wait for the leader's outcome; `None` on timeout signals that the
    /// caller should re-run leader election.
    fn wait(flight: &Flight<T, E>, timeout: Duration) -> Option<Result<T, E>> {
        let state = flight.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = flight
            .ready
            .wait_timeout_while(state, timeout, |s| !s.done)
            .unwrap_or_else(|e| e.into_inner());
        state.done.then(|| {
            state
                .result
                .clone()
                .expect("a finished flight always carries its result")
        })
    }

    /// The replacement flight installed after this flight timed out, if any.
    fn successor(flight: &Flight<T, E>) -> Option<Arc<Flight<T, E>>> {
        flight
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .successor
            .clone()
    }

    /// The flight's outcome if it has already completed, without waiting.
    fn completed(flight: &Flight<T, E>) -> Option<Result<T, E>> {
        let state = flight.state.lock().unwrap_or_else(|e| e.into_inner());
        state.done.then(|| {
            state
                .result
                .clone()
                .expect("a finished flight always carries its result")
        })
    }
}

impl<T, E> Default for Singleflight<T, E>
where
    T: Clone + Send,
    E: Clone + Send,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Unregisters a leader's slot when its `fetch` panics. Disarmed on the
/// normal path, where `lead` settles the flight under the same lock.
struct Unsettled<'a, T, E> {
    registry: &'a Mutex<Registry<T, E>>,
    key: &'a str,
    flight: &'a Arc<Flight<T, E>>,
    armed: bool,
}

impl<T, E> Drop for Unsettled<'_, T, E> {
    fn drop(&mut self) {
        if self.armed {
            let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            release(&mut registry, self.key, self.flight);
        }
    }
}

/// Ends one leader's claim on `key`. The registered flight is cleared only if
/// it is still this leader's, so a superseded leader never evicts its
/// replacement; the slot goes away once no leader and no flight remain.
fn release<T, E>(registry: &mut Registry<T, E>, key: &str, flight: &Arc<Flight<T, E>>) {
    let Some(slot) = registry.get_mut(key) else {
        return;
    };
    slot.outstanding -= 1;
    if slot
        .current
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, flight))
    {
        slot.current = None;
    }
    if slot.outstanding == 0 && slot.current.is_none() {
        registry.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Barrier,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{flight_key, Singleflight, SINGLEFLIGHT_WAIT_TIMEOUT_MS};

    fn tracked_fetch(
        inflight: &AtomicUsize,
        peak: &AtomicUsize,
        hold: Duration,
        body: &str,
    ) -> Result<Vec<u8>, String> {
        let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(now, Ordering::SeqCst);
        thread::sleep(hold);
        inflight.fetch_sub(1, Ordering::SeqCst);
        Ok(body.as_bytes().to_vec())
    }

    #[test]
    fn flight_key_is_the_normalized_segment_id_only() {
        // Same normalization as the wire (`NntpClient::article`): outer
        // whitespace and `<>` stripped, so NZB spellings of one article
        // share one flight. There is no range component by design: the
        // upstream fetch is a whole-article BODY, so different `Range`
        // values for one segment are the same fetch and must coalesce.
        assert_eq!(
            flight_key("<a-seg0@synthetic.test>"),
            "a-seg0@synthetic.test"
        );
        assert_eq!(
            flight_key("  <a-seg0@synthetic.test>  "),
            flight_key("a-seg0@synthetic.test")
        );
        assert_ne!(
            flight_key("<a-seg0@synthetic.test>"),
            flight_key("<a-seg1@synthetic.test>"),
            "different segments must not share a flight"
        );
    }

    #[test]
    fn default_wait_timeout_is_the_documented_5s() {
        assert_eq!(
            SINGLEFLIGHT_WAIT_TIMEOUT_MS, 5_000,
            "worker README documents this value; change both together"
        );
    }

    #[test]
    fn fifty_way_concurrent_same_segment_coalesces_to_one_fetch() {
        let group = Arc::new(Singleflight::<Vec<u8>, String>::new());
        let upstream_calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(50));
        let key = flight_key("<a-seg0@synthetic.test>");

        thread::scope(|s| {
            let mut handles = Vec::with_capacity(50);
            for _ in 0..50 {
                let (group, upstream_calls, start) = (
                    Arc::clone(&group),
                    Arc::clone(&upstream_calls),
                    Arc::clone(&start),
                );
                let key = key.clone();
                handles.push(s.spawn(move || {
                    // Line everybody up so all 50 arrive while the flight is
                    // still in the air.
                    start.wait();
                    group.call(&key, || {
                        upstream_calls.fetch_add(1, Ordering::SeqCst);
                        // Hold the flight open long enough for every waiter
                        // to join it (scheduling jitter is microseconds).
                        thread::sleep(Duration::from_millis(100));
                        Ok::<_, String>(b"segment-bytes".to_vec())
                    })
                }));
            }
            for h in handles {
                let got = h.join().expect("worker thread").expect("fetch ok");
                assert_eq!(got, b"segment-bytes");
            }
        });
        assert_eq!(
            upstream_calls.load(Ordering::SeqCst),
            1,
            "50 concurrent identical fetches must cost 1 upstream call"
        );
    }

    #[test]
    fn slow_healthy_leader_costs_at_most_one_extra_fetch() {
        // The stampede scenario in miniature: every caller's fetch is slow
        // (300 ms) and the waiter timeout is short (50 ms) — the shape of a
        // slow-but-healthy upstream under the 5 s production timeout. Naive
        // timeout handling (each waiter fetches) makes 50 upstream calls;
        // naive re-election makes one per 50 ms generation. The bound here:
        // the leader plus at most one replacement, however many waiters.
        let group = Arc::new(Singleflight::<Vec<u8>, String>::with_timeout(
            Duration::from_millis(50),
        ));
        let upstream_calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(50));
        let key = flight_key("<slow-seg@synthetic.test>");

        thread::scope(|s| {
            let mut handles = Vec::with_capacity(50);
            for _ in 0..50 {
                let (group, upstream_calls, start) = (
                    Arc::clone(&group),
                    Arc::clone(&upstream_calls),
                    Arc::clone(&start),
                );
                let key = key.clone();
                handles.push(s.spawn(move || {
                    start.wait();
                    group.call(&key, || {
                        upstream_calls.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(300));
                        Ok::<_, String>(b"segment-bytes".to_vec())
                    })
                }));
            }
            for h in handles {
                let got = h.join().expect("worker thread").expect("fetch ok");
                assert_eq!(got, b"segment-bytes");
            }
        });
        let calls = upstream_calls.load(Ordering::SeqCst);
        assert!(
            (1..=2).contains(&calls),
            "50 callers behind a slow healthy leader cost at most one extra fetch, got {calls}"
        );
    }

    #[test]
    fn staggered_waiter_uses_completed_replacement_instead_of_starting_third_fetch() {
        let group = Arc::new(Singleflight::<Vec<u8>, String>::with_timeout(
            Duration::from_millis(300),
        ));
        let upstream_calls = Arc::new(AtomicUsize::new(0));
        let leader_registered = Arc::new(AtomicBool::new(false));
        let key = flight_key("<slow-seg@synthetic.test>");

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
                    thread::sleep(Duration::from_millis(500));
                    Ok::<_, String>(b"leader-bytes".to_vec())
                })
            });

            let deadline = Instant::now() + Duration::from_secs(5);
            while !leader_registered.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "leader never registered");
                thread::yield_now();
            }

            let (group_w1, calls_w1) = (Arc::clone(&group), Arc::clone(&upstream_calls));
            let key_w1 = key.clone();
            let first_waiter = s.spawn(move || {
                group_w1.call(&key_w1, || {
                    calls_w1.fetch_add(1, Ordering::SeqCst);
                    // Complete the replacement before the staggered waiter
                    // reaches its own timeout on the original flight.
                    thread::sleep(Duration::from_millis(50));
                    Ok::<_, String>(b"replacement-bytes".to_vec())
                })
            });

            thread::sleep(Duration::from_millis(100));
            let (group_w2, calls_w2) = (Arc::clone(&group), Arc::clone(&upstream_calls));
            let w2_started = Arc::new(AtomicBool::new(false));
            let w2_started_thread = Arc::clone(&w2_started);
            let key_w2 = key.clone();
            let staggered_waiter = s.spawn(move || {
                w2_started_thread.store(true, Ordering::SeqCst);
                group_w2.call(&key_w2, || {
                    calls_w2.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>(b"third-fetch".to_vec())
                })
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !w2_started.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "staggered waiter never started");
                thread::yield_now();
            }

            assert_eq!(
                first_waiter.join().expect("first waiter thread").unwrap(),
                b"replacement-bytes"
            );
            assert_eq!(
                staggered_waiter
                    .join()
                    .expect("staggered waiter thread")
                    .unwrap(),
                b"replacement-bytes",
                "original-flight waiters must retain access to replacement results"
            );
            assert_eq!(
                leader.join().expect("leader thread").unwrap(),
                b"leader-bytes"
            );
        });

        assert_eq!(
            upstream_calls.load(Ordering::SeqCst),
            2,
            "a completed replacement must not be followed by a third fetch"
        );
    }

    #[test]
    fn timed_out_waiters_elect_exactly_one_replacement_leader() {
        const WAITERS: usize = 12;
        let group = Arc::new(Singleflight::<Vec<u8>, String>::with_timeout(
            Duration::from_millis(50),
        ));
        let upstream_calls = Arc::new(AtomicUsize::new(0));
        let leader_registered = Arc::new(AtomicBool::new(false));
        let start = Arc::new(Barrier::new(WAITERS));
        let key = flight_key("<slow-seg@synthetic.test>");

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
                    // Stall far past the 50 ms waiter timeout and past the
                    // replacement flight, then complete normally.
                    thread::sleep(Duration::from_millis(600));
                    Ok::<_, String>(b"leader-bytes".to_vec())
                })
            });

            let deadline = Instant::now() + Duration::from_secs(5);
            while !leader_registered.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "leader never registered");
                thread::yield_now();
            }

            let mut waiters = Vec::with_capacity(WAITERS);
            for _ in 0..WAITERS {
                let (group_w, calls_w, start_w) = (
                    Arc::clone(&group),
                    Arc::clone(&upstream_calls),
                    Arc::clone(&start),
                );
                let key_w = key.clone();
                waiters.push(s.spawn(move || {
                    start_w.wait();
                    group_w.call(&key_w, || {
                        calls_w.fetch_add(1, Ordering::SeqCst);
                        // Hold the replacement flight open so every other
                        // timed-out waiter joins it instead of electing again.
                        thread::sleep(Duration::from_millis(150));
                        Ok::<_, String>(b"replacement-bytes".to_vec())
                    })
                }));
            }

            for waiter in waiters {
                assert_eq!(
                    waiter.join().expect("waiter thread").unwrap(),
                    b"replacement-bytes",
                    "every waiter rides the one replacement flight"
                );
            }
            assert_eq!(
                leader.join().expect("leader thread").unwrap(),
                b"leader-bytes",
                "the replaced leader still completes with its own result"
            );
        });

        assert_eq!(
            upstream_calls.load(Ordering::SeqCst),
            2,
            "12 timed-out waiters must cost exactly one extra fetch, not 12"
        );
    }

    #[test]
    fn late_old_leader_completion_does_not_evict_the_replacement_flight() {
        let group = Arc::new(Singleflight::<Vec<u8>, String>::with_timeout(
            Duration::from_millis(50),
        ));
        let upstream_calls = Arc::new(AtomicUsize::new(0));
        let leader_registered = Arc::new(AtomicBool::new(false));
        let key = flight_key("<slow-seg@synthetic.test>");

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
                    // Past the waiter's 50 ms timeout, done while the
                    // replacement flight is still in the air.
                    thread::sleep(Duration::from_millis(150));
                    Ok::<_, String>(b"leader-bytes".to_vec())
                })
            });

            let deadline = Instant::now() + Duration::from_secs(5);
            while !leader_registered.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "leader never registered");
                thread::yield_now();
            }

            let (group_w, calls_w) = (Arc::clone(&group), Arc::clone(&upstream_calls));
            let key_w = key.clone();
            let waiter = s.spawn(move || {
                group_w.call(&key_w, || {
                    calls_w.fetch_add(1, Ordering::SeqCst);
                    // The replacement flight outlives the old leader.
                    thread::sleep(Duration::from_millis(400));
                    Ok::<_, String>(b"replacement-bytes".to_vec())
                })
            });

            // The old leader finishes at ~150 ms, mid-replacement-flight.
            assert_eq!(
                leader.join().expect("leader thread").unwrap(),
                b"leader-bytes"
            );

            // A caller arriving now joins the live replacement flight rather
            // than starting a third fetch, and takes the first result the
            // chain produced: the old leader's, which landed mid-replacement.
            let got = group
                .call(&key, || {
                    upstream_calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>(b"third-fetch".to_vec())
                })
                .expect("join replacement flight");
            assert_eq!(got, b"leader-bytes");
            assert_eq!(
                waiter.join().expect("waiter thread").unwrap(),
                b"replacement-bytes"
            );
        });

        assert_eq!(upstream_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn original_result_releases_waiters_parked_on_the_replacement() {
        let group = Arc::new(Singleflight::<Vec<u8>, String>::with_timeout(
            Duration::from_millis(100),
        ));
        let upstream_calls = Arc::new(AtomicUsize::new(0));
        let leader_registered = Arc::new(AtomicBool::new(false));
        let key = flight_key("<slow-seg@synthetic.test>");

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
                    // Lands at ~200 ms, while the waiter is parked on the
                    // replacement, which fails at ~230 ms.
                    thread::sleep(Duration::from_millis(200));
                    Ok::<_, String>(b"leader-bytes".to_vec())
                })
            });

            let deadline = Instant::now() + Duration::from_secs(5);
            while !leader_registered.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "leader never registered");
                thread::yield_now();
            }

            // Times out on the leader at ~100 ms, becomes the replacement.
            let (group_r, calls_r) = (Arc::clone(&group), Arc::clone(&upstream_calls));
            let key_r = key.clone();
            let replacement = s.spawn(move || {
                group_r.call(&key_r, || {
                    calls_r.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(130));
                    Err::<Vec<u8>, _>("replacement-failed".to_string())
                })
            });

            thread::sleep(Duration::from_millis(50));
            let (group_w, calls_w) = (Arc::clone(&group), Arc::clone(&upstream_calls));
            let key_w = key.clone();
            let staggered = s.spawn(move || {
                // Joins the original, times out, and parks on the replacement.
                group_w.call(&key_w, || {
                    calls_w.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>(b"third-fetch".to_vec())
                })
            });

            assert_eq!(
                staggered.join().expect("staggered waiter thread"),
                Ok(b"leader-bytes".to_vec()),
                "a waiter parked on the replacement takes the original's result when it lands first"
            );
            assert_eq!(
                leader.join().expect("leader thread"),
                Ok(b"leader-bytes".to_vec())
            );
            assert!(replacement.join().expect("replacement thread").is_err());
        });

        assert_eq!(upstream_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn no_third_concurrent_fetch_beside_a_superseded_leader() {
        let group = Singleflight::<Vec<u8>, String>::with_timeout(Duration::from_millis(50));
        let inflight = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let leader_registered = AtomicBool::new(false);
        let key = flight_key("<stall-seg@synthetic.test>");

        thread::scope(|s| {
            let stalled = s.spawn(|| {
                group.call(&key, || {
                    leader_registered.store(true, Ordering::SeqCst);
                    tracked_fetch(&inflight, &peak, Duration::from_millis(1_000), "stalled")
                })
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !leader_registered.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "leader never registered");
                thread::yield_now();
            }

            // Times out on the stalled leader, replaces it, and finishes while
            // the stalled leader is still fetching.
            let replaced = group.call(&key, || {
                tracked_fetch(&inflight, &peak, Duration::from_millis(20), "replacement")
            });
            assert_eq!(replaced, Ok(b"replacement".to_vec()));

            // A fresh caller leads beside the stalled leader; a second caller
            // times out on that fresh flight. Neither may replace it.
            let fresh = s.spawn(|| {
                group.call(&key, || {
                    tracked_fetch(&inflight, &peak, Duration::from_millis(400), "fresh")
                })
            });
            thread::sleep(Duration::from_millis(10));
            let late = s.spawn(|| {
                group.call(&key, || {
                    tracked_fetch(&inflight, &peak, Duration::from_millis(400), "late")
                })
            });

            assert_eq!(fresh.join().expect("fresh thread"), Ok(b"fresh".to_vec()));
            assert_eq!(late.join().expect("late thread"), Ok(b"fresh".to_vec()));
            assert_eq!(
                stalled.join().expect("stalled thread"),
                Ok(b"stalled".to_vec())
            );
        });

        assert!(
            peak.load(Ordering::SeqCst) <= 2,
            "at most two concurrent upstream fetches per key, saw {}",
            peak.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn panicking_old_leader_does_not_evict_the_replacement_flight() {
        let group = Arc::new(Singleflight::<Vec<u8>, String>::with_timeout(
            Duration::from_millis(50),
        ));
        let upstream_calls = Arc::new(AtomicUsize::new(0));
        let leader_registered = Arc::new(AtomicBool::new(false));
        let key = flight_key("<slow-seg@synthetic.test>");

        thread::scope(|s| {
            let (group_l, calls_l, registered_l) = (
                Arc::clone(&group),
                Arc::clone(&upstream_calls),
                Arc::clone(&leader_registered),
            );
            let key_l = key.clone();
            let leader = s.spawn(move || {
                group_l.call(&key_l, || -> Result<Vec<u8>, String> {
                    calls_l.fetch_add(1, Ordering::SeqCst);
                    registered_l.store(true, Ordering::SeqCst);
                    // Panic after the waiter's 50 ms timeout has replaced
                    // this flight: the panic cleanup must remove only its
                    // own entry, not the replacement's.
                    thread::sleep(Duration::from_millis(150));
                    panic!("simulated upstream panic after replacement")
                })
            });

            let deadline = Instant::now() + Duration::from_secs(5);
            while !leader_registered.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "leader never registered");
                thread::yield_now();
            }

            let (group_w, calls_w) = (Arc::clone(&group), Arc::clone(&upstream_calls));
            let key_w = key.clone();
            let waiter = s.spawn(move || {
                group_w.call(&key_w, || {
                    calls_w.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(400));
                    Ok::<_, String>(b"replacement-bytes".to_vec())
                })
            });

            assert!(leader.join().is_err(), "old leader panicked as designed");

            let got = group
                .call(&key, || {
                    upstream_calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>(b"third-fetch".to_vec())
                })
                .expect("join replacement flight");
            assert_eq!(got, b"replacement-bytes");
            assert_eq!(
                waiter.join().expect("waiter thread").unwrap(),
                b"replacement-bytes"
            );
        });

        assert_eq!(upstream_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn timed_out_waiter_becomes_replacement_leader_instead_of_hanging() {
        let group = Arc::new(Singleflight::<Vec<u8>, String>::with_timeout(
            Duration::from_millis(50),
        ));
        let leader_calls = Arc::new(AtomicUsize::new(0));
        let waiter_calls = Arc::new(AtomicUsize::new(0));
        let leader_registered = Arc::new(AtomicBool::new(false));
        let key = flight_key("<slow-seg@synthetic.test>");

        thread::scope(|s| {
            let (group_l, leader_calls_l, leader_registered_l) = (
                Arc::clone(&group),
                Arc::clone(&leader_calls),
                Arc::clone(&leader_registered),
            );
            let key_l = key.clone();
            let leader = s.spawn(move || {
                group_l.call(&key_l, || {
                    leader_calls_l.fetch_add(1, Ordering::SeqCst);
                    leader_registered_l.store(true, Ordering::SeqCst);
                    // Stall far past the 50 ms waiter timeout.
                    thread::sleep(Duration::from_millis(500));
                    Ok::<_, String>(b"leader-bytes".to_vec())
                })
            });

            // Join only once the leader's flight is registered, so this
            // call is guaranteed to be a waiter, not a second leader.
            let deadline = Instant::now() + Duration::from_secs(5);
            while !leader_registered.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "leader never registered");
                thread::yield_now();
            }
            let t0 = Instant::now();
            let got = group
                .call(&key, || {
                    waiter_calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>(b"waiter-bytes".to_vec())
                })
                .expect("fallback fetch ok");
            let waited = t0.elapsed();
            assert_eq!(
                got, b"waiter-bytes",
                "timed-out waiter leads the replacement flight with its own fetch"
            );
            assert!(
                waited < Duration::from_millis(400),
                "waiter must not hang behind the stalled leader: {waited:?}"
            );

            let leader_got = leader.join().expect("leader thread").expect("leader ok");
            assert_eq!(
                leader_got, b"leader-bytes",
                "leader still completes normally"
            );
        });
        assert_eq!(leader_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            waiter_calls.load(Ordering::SeqCst),
            1,
            "the timed-out waiter ran exactly one replacement fetch"
        );
    }

    #[test]
    fn panicking_leader_releases_key_for_a_later_successful_fetch() {
        let group = Singleflight::<Vec<u8>, String>::new();
        let key = flight_key("<panic-seg@synthetic.test>");
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = group.call(&key, || -> Result<Vec<u8>, String> {
                panic!("simulated upstream panic")
            });
        }));
        assert!(panic.is_err());

        let result = group.call(&key, || Ok::<_, String>(b"recovered".to_vec()));
        assert_eq!(result.unwrap(), b"recovered");
    }

    #[test]
    fn different_keys_fetch_independently() {
        let group = Singleflight::<Vec<u8>, String>::new();
        let calls = AtomicUsize::new(0);
        let a = flight_key("<a-seg0@synthetic.test>");
        let b = flight_key("<a-seg1@synthetic.test>");
        for key in [&a, &b] {
            // `fetch` is `FnOnce` per call; each `call` takes its own closure.
            let got = group
                .call(key, || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>(b"x".to_vec())
                })
                .unwrap();
            assert_eq!(got, b"x");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn singleflight_shares_in_flight_only_not_as_a_cache() {
        let group = Singleflight::<Vec<u8>, String>::new();
        let key = flight_key("<a-seg0@synthetic.test>");
        let calls = AtomicUsize::new(0);
        for _ in 0..2 {
            let got = group
                .call(&key, || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>(b"v".to_vec())
                })
                .unwrap();
            assert_eq!(got, b"v");
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a completed flight must not serve later calls; freshness is the cache's job"
        );
    }
}
