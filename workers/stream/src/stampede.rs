//! Stampede guard: coalesce concurrent identical Range segment fetches into
//! exactly one upstream NNTP fetch (singleflight, keyed on segment id +
//! range).
//!
//! [`Singleflight`] is dependency-free (`std::sync` only) and `Sync`, so one
//! group shared across worker tasks/threads coalesces whoever arrives while
//! a fetch for the same [`flight_key`] is in flight. Waiters that out-wait
//! the leader fall back to their own queued fetch instead of hanging (see
//! [`SINGLEFLIGHT_WAIT_TIMEOUT_MS`).
//!
//! Production note: the portable core owns the primitive and the key shape;
//! the `stream` Worker / pool Durable Object entry point (deploy slice) holds
//! one group per isolate and routes concurrent segment fetches for the same
//! key through it.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How long a waiter rides another fetch before falling back to its own
/// queued fetch. Documented in `workers/stream/README.md`; 5 s sits under the
/// NNTP read timeout (10 s) so a stalled leader never wedges the edge past
/// the provider's own deadline.
pub const SINGLEFLIGHT_WAIT_TIMEOUT_MS: u64 = 5_000;

/// Singleflight key for a segment fetch: the NZB segment message-id plus the
/// client's raw `Range` header (`#full` when there is none). Identical
/// concurrent Range requests for one segment share one key — and therefore
/// one upstream fetch — while different ranges or segments fly separately.
pub fn flight_key(segment_id: &str, range: Option<&str>) -> String {
    match range {
        Some(r) => format!("{segment_id}#{r}"),
        None => format!("{segment_id}#full"),
    }
}

struct FlightState<T, E> {
    done: bool,
    result: Option<Result<T, E>>,
}

struct Flight<T, E> {
    state: Mutex<FlightState<T, E>>,
    ready: Condvar,
}

impl<T, E> Flight<T, E> {
    fn new() -> Self {
        Self {
            state: Mutex::new(FlightState {
                done: false,
                result: None,
            }),
            ready: Condvar::new(),
        }
    }
}

/// Coalesces concurrent identical fetches. The first caller for a key becomes
/// the leader and runs its `fetch`; callers that arrive while it is in flight
/// wait for its result instead of fetching. A waiter that out-waits
/// [`SINGLEFLIGHT_WAIT_TIMEOUT_MS`] runs its own `fetch` (a queued fallback)
/// rather than hanging. Sharing is in-flight only: once a flight completes,
/// the next call fetches again — freshness is the cache's job, not this
/// group's.
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
        let flight = {
            let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(key) {
                Some(flight) => Some(Arc::clone(flight)),
                None => {
                    let flight = Arc::new(Flight::new());
                    map.insert(key.to_string(), Arc::clone(&flight));
                    None
                }
            }
        };
        match flight {
            Some(flight) => Self::wait(&flight, self.timeout, fetch),
            None => {
                // Leader: a panic in `fetch` must not wedge `key` forever,
                // so the entry is removed unless we complete it below.
                let mut guard = RemoveOnPanic {
                    inner: &self.inner,
                    key,
                    defused: false,
                };
                let result = fetch();
                guard.defused = true;
                let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(flight) = map.remove(key) {
                    let mut state = flight.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.result = Some(result.clone());
                    state.done = true;
                    flight.ready.notify_all();
                }
                result
            }
        }
    }

    /// Wait for the leader's outcome; on timeout run `fetch` directly.
    fn wait(
        flight: &Flight<T, E>,
        timeout: Duration,
        fetch: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let state = flight.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = flight
            .ready
            .wait_timeout_while(state, timeout, |s| !s.done)
            .unwrap_or_else(|e| e.into_inner());
        if state.done {
            return state
                .result
                .clone()
                .expect("a finished flight always carries its result");
        }
        drop(state);
        fetch()
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
/// parked on the old flight still fall back via their own timeout (no hang).
struct RemoveOnPanic<'a, T, E> {
    inner: &'a Mutex<HashMap<String, Arc<Flight<T, E>>>>,
    key: &'a str,
    defused: bool,
}

impl<T, E> Drop for RemoveOnPanic<'_, T, E> {
    fn drop(&mut self) {
        if !self.defused {
            if let Ok(mut map) = self.inner.lock() {
                map.remove(self.key);
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
    fn flight_key_pins_segment_id_plus_range() {
        assert_eq!(
            flight_key("<a-seg0@synthetic.test>", Some("bytes=0-99")),
            "<a-seg0@synthetic.test>#bytes=0-99"
        );
        assert_eq!(
            flight_key("<a-seg0@synthetic.test>", None),
            "<a-seg0@synthetic.test>#full"
        );
        assert_ne!(
            flight_key("<a-seg0@synthetic.test>", Some("bytes=0-99")),
            flight_key("<a-seg0@synthetic.test>", Some("bytes=100-199")),
            "different ranges must not share a flight"
        );
        assert_ne!(
            flight_key("<a-seg0@synthetic.test>", None),
            flight_key("<a-seg1@synthetic.test>", None),
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
        let key = flight_key("<a-seg0@synthetic.test>", Some("bytes=0-99"));

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
    fn different_keys_fetch_independently() {
        let group = Singleflight::<Vec<u8>, String>::new();
        let calls = AtomicUsize::new(0);
        let fetch = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(b"x".to_vec())
        };
        let a = flight_key("<a-seg0@synthetic.test>", Some("bytes=0-99"));
        let b = flight_key("<a-seg0@synthetic.test>", Some("bytes=100-199"));
        assert_eq!(group.call(&a, fetch).unwrap(), b"x");
        // `fetch` is `FnOnce` per call; each `call` takes its own closure.
        let fetch = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(b"x".to_vec())
        };
        assert_eq!(group.call(&b, fetch).unwrap(), b"x");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn singleflight_shares_in_flight_only_not_as_a_cache() {
        let group = Singleflight::<Vec<u8>, String>::new();
        let key = flight_key("<a-seg0@synthetic.test>", None);
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

    #[test]
    fn waiter_timeout_falls_back_to_its_own_queued_fetch() {
        let group = Arc::new(Singleflight::<Vec<u8>, String>::with_timeout(
            Duration::from_millis(50),
        ));
        let leader_calls = Arc::new(AtomicUsize::new(0));
        let waiter_calls = Arc::new(AtomicUsize::new(0));
        let leader_registered = Arc::new(AtomicBool::new(false));
        let key = flight_key("<slow-seg@synthetic.test>", None);

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
            assert_eq!(got, b"waiter-bytes", "timed-out waiter uses its own fetch");
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
            "the timed-out waiter ran exactly one queued fallback fetch"
        );
    }
}
