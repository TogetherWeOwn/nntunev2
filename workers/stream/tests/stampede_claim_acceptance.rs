//! Acceptance tests for PR #104 (cache stampede guard) review regressions.
//!
//! Do not edit these tests to make them pass: implement the contract.
//! They pin, through the public API only, the two findings raised on the
//! land review:
//!
//! 1. **Abandoned revalidation claims expire.** A claim taken by a stale
//!    reader whose background refresh never lands (no rewrite, no
//!    `finish_revalidate`) must become reclaimable after the documented
//!    bound (`REVALIDATE_CLAIM_TTL_SECS = 300`, `workers/stream/README.md`),
//!    so a later stale serve retries instead of serving stale bytes
//!    forever with `needs_revalidate` stuck `false`. A late owner must
//!    not be able to release a claim that has since been reclaimed.
//! 2. **No third concurrent fetch beside a superseded leader, proven
//!    without sleep-based thread startup.** The original test spawned
//!    the "late" caller after a fixed 10 ms sleep and hoped the fresh
//!    leader had registered by then. Here every thread-startup handoff
//!    is a registration signal waited on with a bounded deadline, and
//!    parked leaders are held on channels the test releases explicitly.
//!    Sleeps appear only *inside* fetch closures, as simulated upstream
//!    latency — never to guess that another thread has started.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nntune_stream::cache::{
    CacheStatus, ContentCache, CACHE_FRESH_TTL_SECS, REVALIDATE_CLAIM_TTL_SECS,
};
use nntune_stream::grant;
use nntune_stream::meter::MemoryMeter;
use nntune_stream::play::{play, PlayInput, PlayResponse};
use nntune_stream::pool::Pool;
use nntune_stream::stampede::{flight_key, Singleflight};

const SECRET: &[u8] = b"stampede-claim-acceptance-secret";

fn stale_serve(
    pool: &mut Pool<nntune_stream::nntp::NntpClient>,
    cache: &mut ContentCache,
    meter: &mut MemoryMeter,
    now_unix: u64,
) -> PlayResponse {
    // Within the grant TTL cap so the test exercises the cache path.
    let token = grant::issue("u", "track-swr", now_unix + 60, SECRET);
    let addr: SocketAddr = "127.0.0.1:119".parse().unwrap();
    let input = PlayInput {
        trace_id: None,
        token: &token,
        track_id: "track-swr",
        range: None,
        server_addr: addr,
        segments: &[],
        cipher_key: 0,
        now_unix,
        up_next: None,
        segment_ranges: None,
    };
    let resp = play(pool, cache, meter, SECRET, &input).expect("stale serves");
    assert_eq!(
        resp.cache_status,
        CacheStatus::Stale,
        "serve at {now_unix} must be stale for this test to mean anything"
    );
    resp
}

/// Given a stale entry whose claimed revalidation is abandoned (the
/// claimant never rewrites and never calls `finish_revalidate`), when
/// the documented claim bound elapses, then a later stale serve claims
/// the revalidation again — and only from the bound, not before it.
#[test]
fn abandoned_revalidation_claim_becomes_reclaimable_after_the_documented_bound() {
    assert_eq!(
        REVALIDATE_CLAIM_TTL_SECS, 300,
        "workers/stream/README.md documents the abandoned-claim bound as 300s; \
         change both together"
    );

    let mut pool = Pool::new(1);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    cache.put_track_at("track-swr", b"0123456789".to_vec(), 1_000);

    let claimed_at = 1_000 + CACHE_FRESH_TTL_SECS + 10;
    let first = stale_serve(&mut pool, &mut cache, &mut meter, claimed_at);
    assert!(
        first.needs_revalidate,
        "the first stale reader claims the one background revalidation"
    );

    // The claimant now disappears: no rewrite lands, no release call.
    let just_before_bound = claimed_at + REVALIDATE_CLAIM_TTL_SECS - 1;
    let early = stale_serve(&mut pool, &mut cache, &mut meter, just_before_bound);
    assert!(
        !early.needs_revalidate,
        "an active claim must still dedupe stale readers 1s before its bound"
    );

    let at_bound = claimed_at + REVALIDATE_CLAIM_TTL_SECS;
    let retry = stale_serve(&mut pool, &mut cache, &mut meter, at_bound);
    assert!(
        retry.needs_revalidate,
        "an abandoned claim must be reclaimable once the documented 300s \
         bound elapses, or the stale entry can never be refreshed again"
    );

    let after_reclaim = stale_serve(&mut pool, &mut cache, &mut meter, at_bound);
    assert!(
        !after_reclaim.needs_revalidate,
        "the reclaimed claim dedupes subsequent stale readers again"
    );
}

/// A claimant that wakes up after its abandoned claim was reclaimed
/// must not release the new owner's claim with its stale timestamp.
#[test]
fn late_claim_owner_cannot_release_a_reclaimed_claim() {
    let mut cache = ContentCache::new();
    assert!(
        cache.try_begin_revalidate("t1", 1_000),
        "first owner claims"
    );

    let reclaimed_at = 1_000 + REVALIDATE_CLAIM_TTL_SECS;
    assert!(
        cache.try_begin_revalidate("t1", reclaimed_at),
        "the abandoned claim is reclaimable at the bound"
    );

    cache.finish_revalidate("t1", 1_000);
    assert!(
        !cache.try_begin_revalidate("t1", reclaimed_at),
        "the late first owner must not release the reclaimed claim"
    );

    cache.finish_revalidate("t1", reclaimed_at);
    assert!(
        cache.try_begin_revalidate("t1", reclaimed_at),
        "the current owner releasing its claim makes it claimable again"
    );
}

/// Bounded wait on a registration signal: the only way this file waits
/// for another thread to start. Fails fast instead of hanging the
/// suite if the thread never registers.
fn wait_registered(flag: &AtomicBool, who: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !flag.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "{who} never registered");
        thread::yield_now();
    }
}

/// Given a stalled leader superseded by a replacement, when a fresh
/// caller then leads beside the still-fetching stalled leader and a
/// late caller times out on that fresh flight, then the late caller
/// re-waits instead of leading a third concurrent fetch: peak
/// concurrent upstream fetches for the key stays at 2.
#[test]
fn no_third_concurrent_fetch_beside_a_superseded_leader_without_sleep_startup() {
    let group = Singleflight::<Vec<u8>, String>::with_timeout(Duration::from_millis(50));
    let inflight = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let calls = AtomicUsize::new(0);
    let leader_registered = AtomicBool::new(false);
    let fresh_registered = AtomicBool::new(false);
    let late_calling = AtomicBool::new(false);
    let key = flight_key("<stall-seg@synthetic.test>");

    // Parked leaders are held on channels released explicitly below, so
    // "still fetching" is a test-controlled fact, not a sleep duration.
    // `Mutex` so a borrowed receiver can cross into a scoped thread
    // (`mpsc::Receiver` is `Send` but not `Sync`).
    let (stall_tx, stall_rx) = mpsc::channel::<()>();
    let stall_rx = Mutex::new(stall_rx);
    let (fresh_tx, fresh_rx) = mpsc::channel::<()>();
    let fresh_rx = Mutex::new(fresh_rx);

    thread::scope(|s| {
        let stalled = s.spawn(|| {
            group.call(&key, || {
                calls.fetch_add(1, Ordering::SeqCst);
                let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                leader_registered.store(true, Ordering::SeqCst);
                // Bounded even if the test fails before releasing it.
                let _ = stall_rx
                    .lock()
                    .expect("stall gate lock")
                    .recv_timeout(Duration::from_secs(10));
                inflight.fetch_sub(1, Ordering::SeqCst);
                Ok::<_, String>(b"stalled".to_vec())
            })
        });
        wait_registered(&leader_registered, "stalled leader");

        // Times out on the stalled leader, replaces it, and finishes
        // while the stalled leader is still parked on its channel.
        let replaced = group.call(&key, || {
            calls.fetch_add(1, Ordering::SeqCst);
            let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            inflight.fetch_sub(1, Ordering::SeqCst);
            Ok::<_, String>(b"replacement".to_vec())
        });
        assert_eq!(replaced, Ok(b"replacement".to_vec()));

        // A fresh caller leads beside the stalled leader. Its fetch
        // registers, waits for the test to open the gate, then spends
        // 250 ms of simulated upstream latency so the late caller —
        // released only after this registration signal — joins the
        // flight while it is still in the air.
        let fresh = s.spawn(|| {
            group.call(&key, || {
                calls.fetch_add(1, Ordering::SeqCst);
                let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                fresh_registered.store(true, Ordering::SeqCst);
                let _ = fresh_rx
                    .lock()
                    .expect("fresh gate lock")
                    .recv_timeout(Duration::from_secs(10));
                thread::sleep(Duration::from_millis(250));
                inflight.fetch_sub(1, Ordering::SeqCst);
                Ok::<_, String>(b"fresh".to_vec())
            })
        });
        wait_registered(&fresh_registered, "fresh leader");

        // The late caller signals immediately before calling, so the
        // gate below opens only once it is at the call boundary; it
        // times out on the fresh flight (50 ms chunks vs the 250 ms
        // fetch) and must re-wait rather than replace it.
        let late = s.spawn(|| {
            late_calling.store(true, Ordering::SeqCst);
            group.call(&key, || {
                calls.fetch_add(1, Ordering::SeqCst);
                let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                inflight.fetch_sub(1, Ordering::SeqCst);
                Ok::<_, String>(b"late".to_vec())
            })
        });
        wait_registered(&late_calling, "late caller");
        fresh_tx
            .send(())
            .expect("fresh fetch is parked on the gate");

        // Collect every result and release the parked leader before
        // asserting, so a failing assertion cannot strand a thread on
        // its gate until the bounded recv_timeout expires.
        let fresh_result = fresh.join().expect("fresh thread");
        let late_result = late.join().expect("late thread");
        stall_tx
            .send(())
            .expect("stalled fetch is parked on its channel");
        let stalled_result = stalled.join().expect("stalled thread");

        assert_eq!(fresh_result, Ok(b"fresh".to_vec()));
        assert_eq!(
            late_result,
            Ok(b"fresh".to_vec()),
            "the late caller rides the fresh flight instead of leading its own"
        );
        assert_eq!(stalled_result, Ok(b"stalled".to_vec()));
    });

    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "stalled + replacement + fresh are the only upstream fetches; \
         the late caller must not fetch"
    );
    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "at most two concurrent upstream fetches per key, saw {}",
        peak.load(Ordering::SeqCst)
    );
}
