//! Stampede guard primitive: coalesce concurrent identical segment fetches
//! into exactly one upstream NNTP fetch (singleflight, keyed on the
//! normalized segment message-id — see [`flight_key`]).
//!
//! [`Singleflight`] is dependency-free (`std::sync` only) and `Sync`, so one
//! group shared across threads coalesces whoever arrives while a fetch for
//! the same key is in flight. Waiters that out-wait the leader re-run leader
//! election instead of fetching in parallel: exactly one of them becomes the
//! replacement leader and the rest join its flight, and a replacement flight
//! is never itself replaced for slowness — so one stalled fetch costs at
//! most two concurrent upstream fetches per key, however many waiters pile
//! up (see [`SINGLEFLIGHT_WAIT_TIMEOUT_MS`]).
//!
//! Scope, honestly: this module is the primitive only. Nothing in the play
//! path calls it yet — wiring arrives with the Worker / pool Durable Object
//! entry point (deploy slice), which holds one group per isolate. It is also
//! native-only: the `Mutex`/`Condvar` waiter path traps on
//! `wasm32-unknown-unknown`, so `lib.rs` compiles this module out on wasm32
//! and the Worker will need an async shared-future / promise-map equivalent
//! with the same key shape and timeout semantics.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How long a waiter rides one flight before re-running leader election.
/// Documented in `workers/stream/README.md`. On timeout the waiters do not
/// fetch in parallel: one replaces the flight and leads, the rest join the
/// replacement — and the replacement is never itself replaced for slowness
/// (its waiters keep waiting in timeout-sized chunks, checking the original
/// flight each chunk), so a stalled leader costs at most one extra upstream
/// fetch, never a fan-out and never a chain of generations. 5 s is below
/// the NNTP client's 10 s timeout, but that timeout is per socket *read*
/// (`nntp.rs`), not per article, so a slow-but-healthy leader can still be
/// replaced once and briefly overlap its replacement (its waiters then take
/// whichever of the two results lands first); that bounded overlap is the
/// price of never hanging a waiter behind a wedged leader. Only a
/// *panicking* leader frees a further successor flight.
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
}

struct Flight<T, E> {
    state: Mutex<FlightState<T, E>>,
    ready: Condvar,
    /// True when this flight replaced a timed-out one (or was elected by a
    /// caller that had already timed out once). A replacement flight is
    /// never itself replaced for slowness — that is what bounds a stalled
    /// key at two concurrent upstream fetches instead of one new flight per
    /// timeout generation.
    is_replacement: bool,
}

impl<T, E> Flight<T, E> {
    fn new(is_replacement: bool) -> Self {
        Self {
            state: Mutex::new(FlightState {
                done: false,
                result: None,
            }),
            ready: Condvar::new(),
            is_replacement,
        }
    }
}

/// Coalesces concurrent identical fetches. The first caller for a key becomes
/// the leader and runs its `fetch`; callers that arrive while it is in flight
/// wait for its result instead of fetching. A waiter that out-waits
/// [`SINGLEFLIGHT_WAIT_TIMEOUT_MS`] re-runs leader election under the map
/// lock: if the timed-out flight is still the registered one, that waiter
/// replaces it and leads the next flight while the other waiters join the
/// replacement (never a parallel fan-out); if a newer flight is already
/// registered, it joins that one. A replacement flight is never replaced
/// for slowness in turn — its waiters re-wait in timeout chunks, taking the
/// original flight's result if it lands — so slow leaders cost at most two
/// concurrent fetches per key. Sharing is in-flight only: once a flight
/// completes, the next call fetches again — freshness is the cache's job,
/// not this group's.
///
/// `T`/`E` need `Clone` so every waiter gets its own copy of the leader's
/// outcome, and `Send` so one group can be shared across threads.
pub struct Singleflight<T, E> {
    inner: Mutex<HashMap<String, Arc<Flight<T, E>>>>,
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
        // A newer flight a timed-out waiter was told to join, if any.
        let mut joined: Option<Arc<Flight<T, E>>> = None;
        // The flight this caller last timed out on before joining a newer
        // one: a slow-but-healthy original leader may still land first, and
        // its result is as good as the replacement's.
        let mut previous: Option<Arc<Flight<T, E>>> = None;
        // Whether this caller has already timed out once. A flight it goes
        // on to lead is then a replacement (never replaced for slowness in
        // turn), even when elected fresh after a leader panic.
        let mut has_timed_out = false;
        loop {
            let (flight, is_leader) = match joined.take() {
                Some(flight) => (flight, false),
                None => {
                    let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                    match map.get(key) {
                        Some(flight) => (Arc::clone(flight), false),
                        None => {
                            let flight = Arc::new(Flight::new(has_timed_out));
                            map.insert(key.to_string(), Arc::clone(&flight));
                            (flight, true)
                        }
                    }
                }
            };

            if is_leader {
                let fetch = fetch.take().expect("fetch is consumed only by a leader");
                return self.lead(key, flight, fetch);
            }

            if let Some(result) = Self::wait(&flight, self.timeout) {
                return result;
            }

            // Timed out on `flight`. Either leader may have finished in the
            // gap between the wait expiring and this check; never replace a
            // completed flight, and take the original's result if it landed.
            has_timed_out = true;
            if let Some(result) = Self::completed(&flight) {
                return result;
            }
            if let Some(result) = previous.as_deref().and_then(Self::completed) {
                return result;
            }

            // Re-run leader election. Invariant: a leader marks its flight
            // done only while holding the map lock, after removing its own
            // entry (see `lead`), so a flight still registered here is
            // guaranteed unfinished.
            let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(key) {
                Some(current) if Arc::ptr_eq(current, &flight) => {
                    if flight.is_replacement {
                        // Already the replacement flight: a slow leader gets
                        // exactly one understudy, not a chain of them. Keep
                        // waiting on it in timeout chunks (the checks above
                        // still catch the original flight landing). Only a
                        // panic takes us out of this branch, via the `None`
                        // arm below on a later pass.
                        joined = Some(Arc::clone(&flight));
                        continue;
                    }
                    // The flight we waited on is still registered and still
                    // unfinished after a full timeout: replace it and become
                    // the one replacement leader. The old leader may yet
                    // finish (or panic), but its completion path only removes
                    // the map entry if it is still its own (`Arc::ptr_eq`),
                    // so it cannot evict this new flight.
                    let replacement = Arc::new(Flight::new(true));
                    map.insert(key.to_string(), Arc::clone(&replacement));
                    drop(map);
                    let fetch = fetch.take().expect("fetch is consumed only by a leader");
                    return self.lead(key, replacement, fetch);
                }
                Some(current) => {
                    // Another waiter already replaced the flight: join the
                    // newer one instead of fetching, remembering this one in
                    // case its (slow, healthy) leader lands first.
                    previous = Some(Arc::clone(&flight));
                    joined = Some(Arc::clone(current));
                }
                None => {
                    // The flight ended and was removed: either it completed
                    // in the gap above, or its leader panicked (entry
                    // removed, never marked done). Recheck completion, then
                    // loop to elect a fresh flight.
                    drop(map);
                    if let Some(result) = Self::completed(&flight) {
                        return result;
                    }
                    if let Some(result) = previous.as_deref().and_then(Self::completed) {
                        return result;
                    }
                }
            }
        }
    }

    /// Run `fetch` as the leader of `flight`, publish its outcome to the
    /// flight's waiters, and unregister — without evicting a replacement
    /// flight a timed-out waiter may have installed in the meantime.
    fn lead(
        &self,
        key: &str,
        flight: Arc<Flight<T, E>>,
        fetch: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        // A panic in `fetch` must not wedge `key` forever, so the entry is
        // removed unless we complete it below.
        let mut guard = RemoveOnPanic {
            inner: &self.inner,
            key,
            flight: Arc::clone(&flight),
            defused: false,
        };
        let result = fetch();
        guard.defused = true;
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if map
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, &flight))
        {
            map.remove(key);
        }
        let mut state = flight.state.lock().unwrap_or_else(|e| e.into_inner());
        state.result = Some(result.clone());
        state.done = true;
        flight.ready.notify_all();
        result
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

/// Removes a leader's map entry if its `fetch` panics, so later calls for
/// the key start a fresh flight instead of joining a corpse. Waiters already
/// parked on the old flight still recover via their own timeout (no hang).
/// Only removes the entry if it is still this leader's flight: a panicking
/// leader that timed-out waiters already replaced must not evict theirs.
struct RemoveOnPanic<'a, T, E> {
    inner: &'a Mutex<HashMap<String, Arc<Flight<T, E>>>>,
    key: &'a str,
    flight: Arc<Flight<T, E>>,
    defused: bool,
}

impl<T, E> Drop for RemoveOnPanic<'_, T, E> {
    fn drop(&mut self) {
        if !self.defused {
            if let Ok(mut map) = self.inner.lock() {
                if map
                    .get(self.key)
                    .is_some_and(|current| Arc::ptr_eq(current, &self.flight))
                {
                    map.remove(self.key);
                }
            }
        }
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

            // A caller arriving now must join the replacement flight, not
            // start a third fetch — the old leader's completion must not
            // have evicted it from the map.
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
