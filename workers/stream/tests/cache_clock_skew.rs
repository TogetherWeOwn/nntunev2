//! Cache clock-skew freshness floor: future-stamped reads stay Fresh at age 0.
//!
//! Pins `ContentCache::get_track_at` (`workers/stream/src/cache.rs:217`,
//! `now_secs.saturating_sub(stored)`): when the reader clock runs behind the
//! writer clock, the read clamps to `Fresh { age_secs: 0 }` instead of
//! underflowing. At-stored and just-past-TTL/SWR controls prove the floor is
//! not over-broad: normal ageing still turns Fresh -> Stale -> Miss.
//!
//! Failing command:
//! `cargo test --locked -p nntune-stream --test cache_clock_skew`
//!
//! In-memory cache only: no sockets, providers, or Durable Objects.

use nntune_stream::cache::{
    CacheServe, CacheStatus, ContentCache, CACHE_FRESH_TTL_SECS, CACHE_SWR_WINDOW_SECS,
};

const BYTES: &[u8] = b"clock-skew-bytes";

/// Future-stamped read (writer at 2_000, reader at 1_000) serves Fresh at
/// age 0 with no revalidation owed and a Hit status.
#[test]
fn future_stamped_read_clamps_to_fresh_age_zero() {
    let mut cache = ContentCache::new();
    cache.put_track_at("t", BYTES.to_vec(), 2_000);

    let (data, serve) = cache
        .get_track_at("t", 1_000)
        .expect("future-stamped read must serve instead of underflowing");
    assert_eq!(data.as_ref(), BYTES);
    assert_eq!(serve, CacheServe::Fresh { age_secs: 0 });
    assert_eq!(serve.age_secs(), 0);
    assert!(!serve.needs_revalidate());
    assert_eq!(serve.cache_status(), CacheStatus::Hit);
}

/// At-stored control: reading exactly at the write timestamp is Fresh at
/// age 0, so the skew floor matches the normal zero-age serve.
#[test]
fn read_at_stored_timestamp_is_fresh_age_zero() {
    let mut cache = ContentCache::new();
    cache.put_track_at("t", BYTES.to_vec(), 2_000);

    let (data, serve) = cache
        .get_track_at("t", 2_000)
        .expect("at-stored read serves fresh");
    assert_eq!(data.as_ref(), BYTES);
    assert_eq!(serve, CacheServe::Fresh { age_secs: 0 });
    assert!(!serve.needs_revalidate());
    assert_eq!(serve.cache_status(), CacheStatus::Hit);
}

/// Just-past-TTL/SWR controls: the skew floor does not keep genuinely old
/// entries fresh. Age still counts from the write timestamp.
#[test]
fn floor_is_not_over_broad_past_ttl_and_swr() {
    let mut cache = ContentCache::new();
    cache.put_track_at("t", BYTES.to_vec(), 2_000);

    // Last fresh second still serves Fresh without revalidation.
    let (_, serve) = cache
        .get_track_at("t", 2_000 + CACHE_FRESH_TTL_SECS)
        .expect("exactly at the fresh TTL the entry is still fresh");
    assert_eq!(
        serve,
        CacheServe::Fresh {
            age_secs: CACHE_FRESH_TTL_SECS
        }
    );
    assert!(!serve.needs_revalidate());
    assert_eq!(serve.cache_status(), CacheStatus::Hit);

    // One second past the TTL serves Stale and owes revalidation.
    let (_, serve) = cache
        .get_track_at("t", 2_000 + CACHE_FRESH_TTL_SECS + 1)
        .expect("just past the TTL the entry serves stale");
    assert_eq!(
        serve,
        CacheServe::Stale {
            age_secs: CACHE_FRESH_TTL_SECS + 1
        }
    );
    assert!(serve.needs_revalidate());
    assert_eq!(serve.cache_status(), CacheStatus::Stale);

    // One second past fresh + SWR is a miss so the caller re-fetches.
    assert!(
        cache
            .get_track_at(
                "t",
                2_000 + CACHE_FRESH_TTL_SECS + CACHE_SWR_WINDOW_SECS + 1
            )
            .is_none(),
        "past fresh + SWR the entry must re-fetch upstream"
    );
}
