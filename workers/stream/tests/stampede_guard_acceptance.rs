//! Acceptance tests (failing-first) for PR #58 (cache stampede guard):
//! the open Paperclip Review findings the Land card must resolve.
//!
//! Do not edit these tests to make them pass: implement the contract.
//! Two of the three tests FAIL on the current PR head; the third pins an
//! untested branch the reviewer mutation-tested (it passes on the head and
//! fails if the `has_timed_out ||` guard at `stampede.rs:159` is dropped).
//!
//! Contracts to implement (test files only in this card; fixes belong to
//! the Land card):
//!
//! 1. **Stale observability row** (reviewer nit, still open at the 5/5
//!    head): a stale serve carries `Cache-Status: STALE` on the wire but
//!    `PlayStream::observe_event` projects it to the Analytics Engine row
//!    as `hit` — `observability::CacheStatus` has only Hit/Miss while
//!    `cache::CacheStatus` has Hit/Miss/Stale, so the dashboard can never
//!    show stale traffic and the header and the row disagree for one play.
//!    Fix: give the row a distinct stale status (a `Stale` variant rendered
//!    as `stale`, plus the staging query template and pinned column
//!    tests), or a distinct row type name. (If the Land doer instead
//!    documents "stale is intentionally reported as hit", this test needs
//!    updating with reviewer sign-off — it currently pins distinct.)
//! 2. **CI keeps the migrations gate**: the PR's `ci.yml` edit drops the
//!    `migrations-gate` job (manifest integrity + drift, landed on main)
//!    and removes it from `ci-ok`'s `needs`, so the aggregate check no
//!    longer requires it. Fix: restore the job and the `needs` entry.
//! 3. **Post-panic fresh election is a replacement** (reviewer nit,
//!    carried across two reviews): the doc says a flight a timed-out
//!    caller goes on to lead is a replacement "even when elected fresh
//!    after a leader panic" — the only effect of `has_timed_out ||` at
//!    `stampede.rs:159`. No test pins it: dropping the guard survives the
//!    whole suite. Fix: keep the guard; this test pins it (exactly 2
//!    upstream fetches, not 3, when a panicking leader is followed by a
//!    stalling fresh election).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use nntune_stream::cache::{ContentCache, CACHE_FRESH_TTL_SECS};
use nntune_stream::grant;
use nntune_stream::meter::MemoryMeter;
use nntune_stream::play::{play, PlayInput};
use nntune_stream::pool::Pool;
use nntune_stream::stampede::{flight_key, Singleflight};

const SECRET: &[u8] = b"stampede-guard-acceptance-secret";

/// A stale serve must project to a stale observability row, not a hit:
/// the wire header says STALE, so the Analytics Engine row must too.
#[test]
fn stale_serve_projects_to_a_stale_observability_row_not_hit() {
    let stale_now = 1_000 + CACHE_FRESH_TTL_SECS + 10;
    // Within the grant TTL cap so the test exercises the cache path.
    let token = grant::issue("u", "track-swr", stale_now + 60, SECRET);
    let mut pool = Pool::new(1);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    cache.put_track_at("track-swr", b"0123456789".to_vec(), 1_000);

    let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
    let input = PlayInput {
        token: &token,
        track_id: "track-swr",
        range: None,
        server_addr: addr,
        segments: &[],
        cipher_key: 0,
        now_unix: stale_now,
        up_next: None,
        segment_ranges: None,
    };
    let resp = play(&mut pool, &mut cache, &mut meter, SECRET, &input).expect("stale serves");
    assert_eq!(
        resp.cache_headers()[0],
        ("Cache-Status".to_string(), "STALE".to_string()),
        "the wire header already says STALE"
    );
    let ev = resp.stream.observe_event("req-stale-row", 7);
    assert_eq!(
        ev.cache_status.as_str(),
        "stale",
        "the Analytics row must distinguish a STALE serve from a HIT: \
         header and row disagree for the same play while the row says hit"
    );
}

/// The PR's `ci.yml` edit must not drop the migrations gate: the
/// `migrations-gate` job (manifest integrity + drift) stays defined and
/// `ci-ok` still requires it.
#[test]
fn ci_keeps_the_migrations_gate_job_required() {
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml");
    let body =
        std::fs::read_to_string(&path).expect("ci.yml must be readable from the stream crate");
    assert!(
        body.lines().any(|l| l == "  migrations-gate:"),
        "ci.yml must still define the migrations-gate job"
    );
    let needs = body
        .lines()
        .find(|l| l.trim_start().starts_with("needs:") && l.contains("rust-test"))
        .expect("ci-ok must still declare its needs list");
    assert!(
        needs.contains("migrations-gate"),
        "ci-ok needs must still require migrations-gate, got: {needs}"
    );
}

/// A flight elected fresh after a leader panic is a replacement when the
/// electing caller already timed out: its own waiters must re-wait on it
/// instead of replacing it again, so one panic plus one stall costs
/// exactly 2 upstream fetches, not 3.
#[test]
fn fresh_flight_elected_after_a_leader_panic_is_a_replacement() {
    // Keep the leader parked until all waiters reach the call boundary, then
    // leave a generous window for them to enter Singleflight before it panics.
    // Their first timeout happens after that panic; the fresh flight then stalls
    // past another timeout chunk to distinguish a replacement from a new flight.
    let group = Arc::new(Singleflight::<Vec<u8>, String>::with_timeout(
        Duration::from_millis(500),
    ));
    let upstream_calls = Arc::new(AtomicUsize::new(0));
    let leader_registered = Arc::new(AtomicBool::new(false));
    let start = Arc::new(std::sync::Barrier::new(4));
    let key = flight_key("<panic-seg@synthetic.test>");

    thread::scope(|s| {
        let (group_l, calls_l, registered_l, start_l) = (
            Arc::clone(&group),
            Arc::clone(&upstream_calls),
            Arc::clone(&leader_registered),
            Arc::clone(&start),
        );
        let key_l = key.clone();
        let leader = s.spawn(move || {
            group_l.call(&key_l, || -> Result<Vec<u8>, String> {
                calls_l.fetch_add(1, Ordering::SeqCst);
                registered_l.store(true, Ordering::SeqCst);
                // Include the leader in the rendezvous so the waiters are all
                // at the call boundary before the panic window starts.
                start_l.wait();
                // Panic before any waiter's 500 ms timeout; the rendezvous plus
                // this wider interval lets them enter the flight on loaded runners.
                thread::sleep(Duration::from_millis(250));
                panic!("simulated upstream panic before any timeout")
            })
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while !leader_registered.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "leader never registered");
            thread::yield_now();
        }

        let mut waiters = Vec::with_capacity(3);
        for _ in 0..3 {
            let (group_w, calls_w, start_w) = (
                Arc::clone(&group),
                Arc::clone(&upstream_calls),
                Arc::clone(&start),
            );
            let key_w = key.clone();
            waiters.push(s.spawn(move || {
                // Meet the leader and the other waiters before entering the
                // flight, keeping post-barrier scheduling inside a wide window.
                start_w.wait();
                group_w.call(&key_w, || {
                    calls_w.fetch_add(1, Ordering::SeqCst);
                    // Stall past multiple 500 ms timeout chunks.
                    thread::sleep(Duration::from_millis(1_500));
                    Ok::<_, String>(b"fresh-bytes".to_vec())
                })
            }));
        }

        assert!(leader.join().is_err(), "old leader panicked as designed");
        for waiter in waiters {
            assert_eq!(
                waiter.join().expect("waiter thread").unwrap(),
                b"fresh-bytes",
                "every waiter rides the one fresh flight"
            );
        }
    });

    assert_eq!(
        upstream_calls.load(Ordering::SeqCst),
        2,
        "a panicking leader plus one stalling fresh election must cost \
         exactly 2 upstream fetches: the fresh flight is a replacement and \
         is never replaced for slowness"
    );
}
