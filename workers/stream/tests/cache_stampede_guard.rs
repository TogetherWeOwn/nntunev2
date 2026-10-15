//! Cache stampede guard acceptance: singleflight + stale-while-revalidate.
//!
//! Paired acceptance-test child for the `stream: cache stampede guard` build
//! card ([TOG-25311](/TOG/issues/TOG-25311); roadmap slice S1 on
//! [TOG-25278](/TOG/issues/TOG-25278#document-roadmap)). The Builder builds on
//! this card's branch and never edits this file.
//!
//! Acceptance mapping:
//!
//! - 50-way concurrent identical Range fetch == 1 upstream call:
//!   [`fifty_way_concurrent_identical_range_fetch_coalesces_to_one_upstream_call`].
//! - SWR header assertions:
//!   [`swr_serves_stale_immediately_with_stale_cache_status_and_revalidates_in_background`].
//! - Waiter-timeout fallback (no hung requests):
//!   [`singleflight_waiter_timeout_falls_back_without_hanging`].
//!
//! Run only this file (the workspace stays green around it until the build
//! lands):
//!
//! ```sh
//! cargo test -p nntune-stream --test cache_stampede_guard
//! ```
//!
//! Fixtures are synthetic byte vectors assembled here; no network, no real
//! provider, no Cloudflare bindings (the crate stays dependency-free, so this
//! file is `std`-only: threads, atomics, barriers, channels).
//!
//! # BUILDER CONTRACT (implement to make this file compile green)
//!
//! Add two modules to `workers/stream/src` and re-export them from `lib.rs`:
//!
//! ```text
//! nntune_stream::singleflight::Singleflight
//! nntune_stream::swr::{SwrCache, SwrServed}
//! ```
//!
//! Minimum shapes used below (you may extend, never narrow):
//!
//! - `Singleflight::new(waiter_timeout: Duration) -> Singleflight`
//!   (`Clone + Send + Sync`; shares one in-flight upstream call per key).
//! - `Singleflight::waiter_timeout(&self) -> Duration` (the configured value,
//!   so the worker README can document the exact number).
//! - `Singleflight::fetch(&self, key: &str, upstream: impl FnOnce() -> Vec<u8>)`
//!   `-> FetchOutcome`, where the key is the segment id + Range window
//!   (different ranges are different keys) and
//!   `FetchOutcome { body: Arc<[u8]>, coalesced: bool, did_fallback: bool }`:
//!   exactly one caller runs `upstream` (leader, `coalesced == false`);
//!   overlapping same-key waiters share the leader's bytes
//!   (`coalesced == true`); a waiter that exceeds `waiter_timeout` runs its
//!   own queued `upstream` instead of hanging
//!   (`did_fallback == true`, own bytes).
//! - `SwrCache::new(max_age_secs: u64, swr_window_secs: u64) -> SwrCache`
//!   (interior mutability: all methods take `&self`).
//! - `SwrCache::put(&self, key: &str, body: Vec<u8>, now_secs: u64)`.
//! - `SwrCache::get(&self, key: &str, now_secs: u64) -> Option<SwrServed>`
//!   (plain read; never triggers a fetch).
//! - `SwrCache::get_or_revalidate(&self, key: &str, now_secs: u64, revalidate:
//!   impl FnOnce() -> Vec<u8>) -> Option<SwrServed>`: fresh entries serve with
//!   a `hit` status; entries older than `max_age_secs` but inside the
//!   `swr_window_secs` window serve the stale bytes *immediately* with a
//!   `stale` status while `revalidate` runs in the background and refreshes
//!   the entry.
//! - `SwrServed { body: Arc<[u8]>, cache_status: String, age_secs: u64 }`:
//!   `cache_status` is the `Cache-Status` value (`"hit"` fresh, `"stale"`
//!   while revalidating); `age_secs` is the entry age (`now - stored_at`).
//!
//! EXPECTED STATE: this file does NOT compile until the build lands
//! (`E0432: unresolved imports`). That red target is the acceptance proof.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc, Barrier,
};
use std::thread;
use std::time::{Duration, Instant};

use nntune_stream::singleflight::Singleflight;
use nntune_stream::swr::SwrCache;

/// 50 threads fetch the same segment id + Range window at the same instant;
/// the counting fake upstream must run exactly once and every waiter must see
/// the leader's bytes. A second phase pins the keying: a different Range
/// window is a different key and fetches again.
#[test]
fn fifty_way_concurrent_identical_range_fetch_coalesces_to_one_upstream_call() {
    let key = "segment:<track-a-seg0@synthetic.test>:bytes=0-99";
    let expected: Vec<u8> = (0..100u8)
        .map(|i| i.wrapping_mul(7).wrapping_add(3))
        .collect();

    let guard = Arc::new(Singleflight::new(Duration::from_secs(5)));
    let upstream_calls = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(50));

    let handles: Vec<_> = (0..50)
        .map(|_| {
            let guard = Arc::clone(&guard);
            let upstream_calls = Arc::clone(&upstream_calls);
            let barrier = Arc::clone(&barrier);
            let expected = expected.clone();
            thread::spawn(move || {
                barrier.wait();
                guard.fetch(key, || {
                    upstream_calls.fetch_add(1, Ordering::SeqCst);
                    // Hold the in-flight window open so all 49 waiters overlap
                    // it: a microsecond-fast upstream could otherwise finish
                    // before the last thread arrives and the count would flake.
                    thread::sleep(Duration::from_millis(200));
                    expected
                })
            })
        })
        .collect();

    let mut leaders = 0usize;
    let mut coalesced = 0usize;
    for handle in handles {
        let outcome = handle.join().expect("worker thread panicked");
        assert_eq!(
            &outcome.body[..],
            expected.as_slice(),
            "every waiter must see the leader's bytes"
        );
        assert!(
            !outcome.did_fallback,
            "nobody waits out the 5 s timeout on a 200 ms fetch"
        );
        if outcome.coalesced {
            coalesced += 1;
        } else {
            leaders += 1;
        }
    }
    assert_eq!(
        upstream_calls.load(Ordering::SeqCst),
        1,
        "50-way concurrent identical Range fetch == 1 upstream call"
    );
    assert_eq!(leaders, 1, "exactly one leader runs the upstream fetch");
    assert_eq!(coalesced, 49, "every other waiter coalesces onto it");

    // Keying: segment id + range. A different window must NOT coalesce.
    let before = upstream_calls.load(Ordering::SeqCst);
    let outcome = guard.fetch(
        "segment:<track-a-seg0@synthetic.test>:bytes=100-199",
        || {
            upstream_calls.fetch_add(1, Ordering::SeqCst);
            vec![0xABu8; 100]
        },
    );
    assert_eq!(&outcome.body[..], &[0xABu8; 100][..]);
    assert_eq!(
        upstream_calls.load(Ordering::SeqCst),
        before + 1,
        "a different Range window is a different singleflight key"
    );
}

/// A stale-but-in-window entry serves its bytes immediately (faster than the
/// slow revalidation), pins `Cache-Status: stale` + `Age`, and the background
/// refresh lands so the next read is a fresh `hit`.
#[test]
fn swr_serves_stale_immediately_with_stale_cache_status_and_revalidates_in_background() {
    const NOW: u64 = 1_700_000_000;
    const MAX_AGE: u64 = 60;
    const SWR_WINDOW: u64 = 300;

    let stale: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
    let fresh: Vec<u8> = (0..1024u32).map(|i| (255 - (i % 251)) as u8).collect();
    assert_ne!(stale, fresh, "fixture sanity: refresh must change bytes");

    let mut cache = SwrCache::new(MAX_AGE, SWR_WINDOW);
    cache.put("track-a", stale.clone(), NOW);

    // 120 s later: stale (120 > max-age 60) but inside the SWR window
    // (120 < 60 + 300). The revalidation sleeps 300 ms; serving stale must
    // beat that by returning immediately while it runs in the background.
    let fresh_for_bg = fresh.clone();
    let t0 = Instant::now();
    let served = cache
        .get_or_revalidate("track-a", NOW + 120, move || {
            thread::sleep(Duration::from_millis(300));
            fresh_for_bg
        })
        .expect("stale entry inside the SWR window must serve");
    let elapsed = t0.elapsed();
    assert_eq!(
        &served.body[..],
        stale.as_slice(),
        "SWR serves the stale bytes, not an error and not a blocking fetch"
    );
    assert!(
        served.cache_status.contains("stale"),
        "Cache-Status must pin the stale serve, got {:?}",
        served.cache_status
    );
    assert!(
        served.age_secs >= 120,
        "Age must pin the staleness (120 s), got {}",
        served.age_secs
    );
    assert!(
        elapsed < Duration::from_millis(200),
        "stale must serve immediately while revalidation runs in the background, took {elapsed:?}"
    );

    // The background refresh lands: poll with the non-triggering read until
    // the fresh bytes are visible (bounded so a missing refresh fails fast).
    let deadline = Instant::now() + Duration::from_secs(5);
    let refreshed = loop {
        if let Some(hit) = cache.get("track-a", NOW + 121) {
            if hit.body[..] == fresh[..] {
                break hit;
            }
        }
        if Instant::now() > deadline {
            panic!("background revalidation never refreshed the entry");
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        refreshed.cache_status.contains("hit"),
        "refreshed entry must read as a hit, got {:?}",
        refreshed.cache_status
    );
    assert!(
        refreshed.age_secs < 120,
        "refresh must reset the age, got {}",
        refreshed.age_secs
    );
}

/// The leader parks in a gated (never-returning-until-released) upstream
/// fetch; the waiter must give up after the 50 ms timeout and serve its own
/// queued fetch instead of hanging. Both upstream calls happen; nothing hangs.
#[test]
fn singleflight_waiter_timeout_falls_back_without_hanging() {
    let guard = Arc::new(Singleflight::new(Duration::from_millis(50)));
    assert_eq!(
        guard.waiter_timeout(),
        Duration::from_millis(50),
        "the timeout is explicit on the guard so the README can document the exact value"
    );

    let key = "segment:<track-b-seg0@synthetic.test>:bytes=0-49";
    let upstream_calls = Arc::new(AtomicUsize::new(0));
    let (release_tx, release_rx) = mpsc::channel::<()>();

    // Leader: simulates a slow NNTP fetch, gated until the test releases it.
    let leader_guard = Arc::clone(&guard);
    let leader_calls = Arc::clone(&upstream_calls);
    let leader = thread::spawn(move || {
        leader_guard.fetch(key, || {
            leader_calls.fetch_add(1, Ordering::SeqCst);
            release_rx.recv().expect("test releases the leader");
            vec![0x11u8; 50]
        })
    });

    // Wait until the leader is parked inside its upstream call.
    let entered = Instant::now() + Duration::from_secs(5);
    while upstream_calls.load(Ordering::SeqCst) == 0 {
        if Instant::now() > entered {
            panic!("leader never entered its upstream fetch");
        }
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(20));

    // Waiter with a fast fallback upstream, on its own thread with a bounded
    // receive: a guard without a timeout would hang here and fail the test
    // instead of hanging CI.
    let waiter_guard = Arc::clone(&guard);
    let waiter_calls = Arc::clone(&upstream_calls);
    let (waiter_tx, waiter_rx) = mpsc::channel();
    thread::spawn(move || {
        let t0 = Instant::now();
        let outcome = waiter_guard.fetch(key, || {
            waiter_calls.fetch_add(1, Ordering::SeqCst);
            vec![0x22u8; 50]
        });
        let _ = waiter_tx.send((outcome, t0.elapsed()));
    });
    let (outcome, waited) = waiter_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("waiter hung: no waiter-timeout fallback");
    assert!(
        waited < Duration::from_secs(2),
        "the waiter must fall back instead of hanging on the leader, took {waited:?}"
    );
    assert!(
        outcome.did_fallback,
        "a timed-out waiter must flag its queued fallback fetch"
    );
    assert!(
        !outcome.coalesced,
        "a fallback serve is its own fetch, not the leader's bytes"
    );
    assert_eq!(
        &outcome.body[..],
        &[0x22u8; 50][..],
        "the fallback serves its own fetched bytes"
    );

    release_tx
        .send(())
        .expect("leader still parked in its upstream fetch");
    let leader_outcome = leader.join().expect("leader thread panicked");
    assert!(
        !leader_outcome.coalesced && !leader_outcome.did_fallback,
        "the leader is a plain uncoalesced fetch"
    );
    assert_eq!(&leader_outcome.body[..], &[0x11u8; 50][..]);
    assert_eq!(
        upstream_calls.load(Ordering::SeqCst),
        2,
        "leader + one queued fallback fetch, nothing more"
    );
}
