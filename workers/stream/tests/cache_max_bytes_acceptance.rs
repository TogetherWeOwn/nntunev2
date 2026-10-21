//! Acceptance tests: bound `ContentCache` with a max-bytes cap.
//!
//! `ContentCache` (`workers/stream/src/cache.rs`) holds three unbounded maps;
//! time-expiry bounds entry lifetime, not capacity. These tests pin the
//! capacity contract the Builder implements in `workers/stream/src/cache.rs`
//! without editing this file:
//!
//! ```rust,ignore
//! /// Hard cap on the total blob bytes a default `ContentCache` holds:
//! /// 64 MiB. Documented next to the TTL constants / in
//! /// `workers/stream/README.md` like the existing cache constants.
//! pub const MAX_CONTENT_CACHE_BYTES: usize = 64 * 1024 * 1024;
//!
//! impl ContentCache {
//!     pub fn new() -> Self; // cap = MAX_CONTENT_CACHE_BYTES
//!     pub fn with_max_bytes(max_bytes: usize) -> Self; // explicit cap (tests)
//!     pub fn max_bytes(&self) -> usize; // the configured cap
//!     /// Sum of the lengths of the distinct blobs currently held. A blob
//!     /// shared (via `Arc`) by several tracks counts once; a blob no track
//!     /// points at any more counts zero (it must not keep exerting cap
//!     /// pressure after a rewrite repoints its last track).
//!     pub fn total_bytes(&self) -> usize;
//!     /// Track entries evicted for capacity so far (the countable probe the
//!     /// existing observability path reports; time-expiry is not an eviction).
//!     pub fn eviction_count(&self) -> u64;
//! }
//! ```
//!
//! Semantics pinned below:
//! - Neither card named a cap value, so this file pins the literal
//!   64 MiB (67_108_864 bytes) — half a 128 MiB Worker isolate, room for
//!   several whole tracks. Changing the value means changing the literal
//!   test here, never silently drifting the constant.
//! - Byte accounting is over distinct blobs: identical plaintext stored for
//!   two tracks is one blob and counts once (`blob_count` already proves
//!   dedup; `total_bytes` must agree with it).
//! - Eviction runs on insert (`put_track` and `put_track_at` alike) while
//!   `total_bytes() > max_bytes()`. It evicts track entries oldest-first,
//!   where age is last-write order (a rewrite is a write: it makes its track
//!   the newest). Evicting a track frees its blob's bytes only once no
//!   surviving track points at that blob, so eviction continues until the
//!   cache is back at or under the cap.
//! - The just-written entry is never evicted by its own insert, even when it
//!   alone exceeds the cap (then it serves alone, over cap, rather than the
//!   write vanishing).
//! - Exactly at the cap is not over it: no eviction at equality.
//! - Survivors keep the existing serve contract untouched: Fresh/`HIT`
//!   inside `CACHE_FRESH_TTL_SECS`, Stale/`STALE` + revalidation owed inside
//!   the SWR window, `Age` = `CacheServe::age_secs()`; evicted and expired
//!   entries return `None` (the MISS path). Untimed entries never go stale.
//!
//! Out of scope: TTL values, R2/archive policy, play-route headers.
//!
//! Failing command:
//! `cargo test --locked -p nntune-stream --test cache_max_bytes_acceptance`
//!
//! RED reason (2026-10-19): `MAX_CONTENT_CACHE_BYTES`,
//! `ContentCache::with_max_bytes`, `max_bytes`, `total_bytes` and
//! `eviction_count` do not exist yet, so this target fails to compile with
//! unresolved-import / no-method errors naming exactly those items. The
//! implementation turns it green without editing this file.
//!
//! In-memory cache only: no sockets, providers, or Durable Objects.

use std::sync::Arc;

use nntune_stream::cache::{
    CacheServe, CacheStatus, ContentCache, CACHE_FRESH_TTL_SECS, CACHE_SWR_WINDOW_SECS,
    MAX_CONTENT_CACHE_BYTES,
};

/// Distinct `len`-byte plaintext filled with `fill` (distinct fill bytes give
/// distinct blob hashes; equal fills dedup to one shared blob).
fn blob(fill: u8, len: usize) -> Vec<u8> {
    vec![fill; len]
}

#[test]
fn cap_constant_pins_the_named_literal_and_the_default_cache_uses_it() {
    assert_eq!(
        MAX_CONTENT_CACHE_BYTES, 67_108_864,
        "the named cap must stay at the pinned 64 MiB literal"
    );
    assert_eq!(MAX_CONTENT_CACHE_BYTES, 64 * 1024 * 1024);

    let cache = ContentCache::new();
    assert_eq!(
        cache.max_bytes(),
        MAX_CONTENT_CACHE_BYTES,
        "ContentCache::new() must be capped at MAX_CONTENT_CACHE_BYTES"
    );
    assert_eq!(
        ContentCache::default().max_bytes(),
        MAX_CONTENT_CACHE_BYTES,
        "Default must agree with new() on the cap"
    );
    assert_eq!(
        ContentCache::with_max_bytes(100).max_bytes(),
        100,
        "with_max_bytes must configure exactly the cap it was given"
    );

    assert_eq!(cache.total_bytes(), 0, "an empty cache holds zero bytes");
    assert_eq!(cache.blob_count(), 0);
    assert_eq!(
        cache.eviction_count(),
        0,
        "an empty cache has evicted nothing"
    );
}

#[test]
fn byte_accounting_counts_a_shared_blob_once_under_arc_sharing() {
    let mut cache = ContentCache::with_max_bytes(1_000);
    cache.put_track("a", blob(0xA1, 100));
    cache.put_track("b", blob(0xA1, 100)); // identical plaintext: same blob

    assert_eq!(
        cache.blob_count(),
        1,
        "identical plaintext dedups to one blob"
    );
    assert_eq!(
        cache.total_bytes(),
        100,
        "a blob shared by two tracks counts once, not per track"
    );
    let from_a = cache.get_track("a").expect("track a serves");
    let from_b = cache.get_track("b").expect("track b serves");
    assert!(
        Arc::ptr_eq(&from_a, &from_b),
        "both tracks must serve the one shared Arc, not copies"
    );

    cache.put_track("c", blob(0xC3, 50));
    assert_eq!(cache.blob_count(), 2);
    assert_eq!(
        cache.total_bytes(),
        150,
        "a distinct blob adds exactly its own length"
    );
    assert_eq!(cache.eviction_count(), 0, "under the cap nothing evicts");
}

#[test]
fn byte_accounting_stops_counting_a_blob_once_no_track_points_at_it() {
    let mut cache = ContentCache::with_max_bytes(1_000);
    cache.put_track("a", blob(0xA1, 100));
    cache.put_track("b", blob(0xA1, 100));
    assert_eq!(cache.total_bytes(), 100);

    // Repointing one of two sharers keeps the shared blob counted.
    cache.put_track("a", blob(0xB2, 40));
    assert_eq!(
        cache.total_bytes(),
        140,
        "the old blob still counts while track b points at it"
    );
    assert_eq!(cache.blob_count(), 2);

    // Repointing the last sharer must release the orphaned blob entirely:
    // it can never serve again, so it must not exert cap pressure.
    cache.put_track("b", blob(0xB2, 40));
    assert_eq!(
        cache.total_bytes(),
        40,
        "an unreferenced blob must stop counting against the cap"
    );
    assert_eq!(
        cache.blob_count(),
        1,
        "an unreferenced blob must not be retained"
    );
    assert_eq!(cache.eviction_count(), 0, "rewrites are not evictions");
}

#[test]
fn exactly_at_the_cap_evicts_nothing() {
    let mut cache = ContentCache::with_max_bytes(100);
    cache.put_track("a", blob(0xA1, 60));
    cache.put_track("b", blob(0xB2, 40));

    assert_eq!(
        cache.total_bytes(),
        100,
        "the cache sits exactly at its cap"
    );
    assert_eq!(
        cache.eviction_count(),
        0,
        "at the cap is not over it: nothing evicts"
    );
    assert!(
        cache.get_track("a").is_some(),
        "the older entry must survive an exactly-at-cap insert"
    );
    assert!(cache.get_track("b").is_some());
}

#[test]
fn over_cap_insert_evicts_oldest_first() {
    let mut cache = ContentCache::with_max_bytes(100);
    cache.put_track("oldest", blob(0x01, 40));
    cache.put_track("middle", blob(0x02, 40));
    cache.put_track("newest", blob(0x03, 40)); // 120 > 100: evicts "oldest"

    assert_eq!(
        cache.total_bytes(),
        80,
        "eviction must bring the cache back under the cap"
    );
    assert_eq!(cache.eviction_count(), 1);
    assert!(
        cache.get_track("oldest").is_none(),
        "the oldest-written entry evicts first"
    );
    assert!(cache.get_track("middle").is_some());
    assert!(
        cache.get_track("newest").is_some(),
        "the just-written entry survives its own insert"
    );

    cache.put_track("fourth", blob(0x04, 40)); // evicts "middle", now oldest
    assert_eq!(cache.total_bytes(), 80);
    assert_eq!(cache.eviction_count(), 2);
    assert!(
        cache.get_track("middle").is_none(),
        "eviction order stays oldest-first across inserts"
    );
    assert!(cache.get_track("newest").is_some());
    assert!(cache.get_track("fourth").is_some());
}

#[test]
fn over_cap_insert_never_evicts_the_just_written_entry_even_alone_over_cap() {
    // A single entry bigger than the whole cap still serves: the write must
    // not vanish into its own eviction pass.
    let mut cache = ContentCache::with_max_bytes(100);
    cache.put_track("huge", blob(0x09, 101));
    assert_eq!(cache.total_bytes(), 101);
    assert_eq!(
        cache.eviction_count(),
        0,
        "nothing older exists and the just-written entry is exempt"
    );
    assert_eq!(
        cache.get_track("huge").map(|d| d.len()),
        Some(101),
        "a lone over-cap entry serves instead of being evicted"
    );

    // The same protection when older entries exist: they all go, it stays.
    let mut cache = ContentCache::with_max_bytes(100);
    cache.put_track("a", blob(0xA1, 40));
    cache.put_track("b", blob(0xB2, 40));
    cache.put_track("huge", blob(0x09, 150));
    assert!(cache.get_track("a").is_none());
    assert!(cache.get_track("b").is_none());
    assert_eq!(cache.eviction_count(), 2);
    assert_eq!(
        cache.total_bytes(),
        150,
        "only the just-written entry remains, over cap by itself"
    );
    assert_eq!(cache.get_track("huge").map(|d| d.len()), Some(150));
}

#[test]
fn shared_blob_eviction_continues_until_the_cache_is_under_the_cap() {
    let mut cache = ContentCache::with_max_bytes(100);
    cache.put_track("a", blob(0xA1, 60));
    cache.put_track("b", blob(0xA1, 60)); // shares a's blob: total still 60
    assert_eq!(cache.total_bytes(), 60);

    // 60 + 50 = 110 > 100. Evicting only "a" frees nothing (b still points
    // at the shared 60-byte blob), so eviction must continue to "b".
    cache.put_track("c", blob(0xC3, 50));
    assert_eq!(
        cache.total_bytes(),
        50,
        "evicting one sharer frees no bytes; eviction must continue"
    );
    assert_eq!(
        cache.eviction_count(),
        2,
        "both sharers evict before the cache is back under the cap"
    );
    assert!(cache.get_track("a").is_none());
    assert!(cache.get_track("b").is_none());
    assert!(
        cache.get_track("c").is_some(),
        "the just-written entry stays"
    );
    assert_eq!(cache.blob_count(), 1);
}

#[test]
fn a_rewrite_is_the_newest_write_for_eviction_order() {
    let mut cache = ContentCache::with_max_bytes(100);
    cache.put_track_at("a", blob(0xA1, 40), 1_000);
    cache.put_track_at("b", blob(0xB2, 40), 1_001);
    // Rewriting "a" (this is also how a revalidation lands) makes it newer
    // than "b", even though the blob content is unchanged.
    cache.put_track_at("a", blob(0xA1, 40), 1_002);
    cache.put_track_at("c", blob(0xC3, 40), 1_003); // 120 > 100: evicts "b"

    assert_eq!(cache.eviction_count(), 1);
    assert!(
        cache.get_track("b").is_none(),
        "after a's rewrite, b is the oldest write and evicts first"
    );
    assert!(
        cache.get_track("a").is_some(),
        "a rewritten entry must not be treated as the oldest"
    );
    assert!(cache.get_track("c").is_some());
}

#[test]
fn fresh_and_swr_serves_and_headers_are_unchanged_at_and_over_the_cap() {
    let mut cache = ContentCache::with_max_bytes(100);
    cache.put_track_at("victim", blob(0x01, 40), 1_000);
    cache.put_track_at("survivor", blob(0x02, 40), 1_000);
    // Exactly at the cap both still serve fresh.
    let (_, serve) = cache
        .get_track_at("victim", 1_000)
        .expect("at-cap entry serves before any over-cap insert");
    assert_eq!(serve, CacheServe::Fresh { age_secs: 0 });

    // Over-cap insert evicts "victim" (oldest) and keeps the rest.
    cache.put_track_at("newest", blob(0x03, 40), 1_000);
    assert_eq!(cache.eviction_count(), 1);
    assert!(
        cache.get_track_at("victim", 1_000).is_none() && cache.get_track("victim").is_none(),
        "an evicted entry is a miss at every clock, not a stale serve"
    );

    // Survivor at the last fresh second: Fresh, HIT, Age = TTL, no
    // revalidation owed — identical to the uncapped contract.
    let (data, serve) = cache
        .get_track_at("survivor", 1_000 + CACHE_FRESH_TTL_SECS)
        .expect("survivor serves fresh at the TTL boundary");
    assert_eq!(data.as_ref(), blob(0x02, 40).as_slice());
    assert_eq!(
        serve,
        CacheServe::Fresh {
            age_secs: CACHE_FRESH_TTL_SECS
        }
    );
    assert_eq!(serve.age_secs(), CACHE_FRESH_TTL_SECS, "Age is unchanged");
    assert!(!serve.needs_revalidate());
    assert_eq!(serve.cache_status(), CacheStatus::Hit);
    assert_eq!(serve.cache_status().header_value(), "HIT");

    // Survivor one second into SWR: Stale, STALE, revalidation owed.
    let (_, serve) = cache
        .get_track_at("survivor", 1_000 + CACHE_FRESH_TTL_SECS + 1)
        .expect("survivor serves stale inside the SWR window");
    assert_eq!(
        serve,
        CacheServe::Stale {
            age_secs: CACHE_FRESH_TTL_SECS + 1
        }
    );
    assert_eq!(serve.age_secs(), CACHE_FRESH_TTL_SECS + 1);
    assert!(serve.needs_revalidate());
    assert_eq!(serve.cache_status(), CacheStatus::Stale);
    assert_eq!(serve.cache_status().header_value(), "STALE");

    // Fresh-write control and past-SWR expiry are likewise untouched.
    let (_, serve) = cache
        .get_track_at("newest", 1_000)
        .expect("the just-written entry serves fresh");
    assert_eq!(serve, CacheServe::Fresh { age_secs: 0 });
    assert_eq!(serve.cache_status().header_value(), "HIT");
    assert!(
        cache
            .get_track_at(
                "survivor",
                1_000 + CACHE_FRESH_TTL_SECS + CACHE_SWR_WINDOW_SECS + 1
            )
            .is_none(),
        "past fresh+SWR the survivor still expires to a miss"
    );
    assert_eq!(
        CacheStatus::Miss.header_value(),
        "MISS",
        "the miss header value is part of the unchanged wire contract"
    );
}

#[test]
fn untimed_entries_never_go_stale_under_eviction_pressure() {
    let mut cache = ContentCache::with_max_bytes(100);
    cache.put_track("a", blob(0xA1, 40));
    cache.put_track("b", blob(0xB2, 40));
    cache.put_track("c", blob(0xC3, 40)); // evicts untimed "a" too
    assert!(cache.get_track("a").is_none(), "untimed entries also evict");

    let far_future = CACHE_FRESH_TTL_SECS + CACHE_SWR_WINDOW_SECS + 1_000_000;
    let (_, serve) = cache
        .get_track_at("b", far_future)
        .expect("an untimed survivor never expires");
    assert_eq!(
        serve,
        CacheServe::Fresh { age_secs: 0 },
        "untimed entries keep serving Fresh at age 0"
    );
    assert_eq!(serve.cache_status().header_value(), "HIT");
    assert!(!serve.needs_revalidate());
}

#[test]
fn empty_blobs_count_zero_bytes_and_never_trigger_eviction() {
    let mut cache = ContentCache::with_max_bytes(10);
    cache.put_track("empty-a", Vec::new());
    cache.put_track("full", blob(0xF0, 10)); // exactly at cap
    cache.put_track("empty-b", Vec::new()); // same empty blob as empty-a

    assert_eq!(
        cache.total_bytes(),
        10,
        "empty blobs add zero bytes (and dedup to one blob)"
    );
    assert_eq!(cache.blob_count(), 2);
    assert_eq!(cache.eviction_count(), 0);
    assert_eq!(cache.get_track("empty-a").map(|d| d.len()), Some(0));
    assert_eq!(cache.get_track("empty-b").map(|d| d.len()), Some(0));
    assert_eq!(cache.get_track("full").map(|d| d.len()), Some(10));
}

#[test]
fn revalidate_claims_still_work_for_survivors_after_an_eviction() {
    let mut cache = ContentCache::with_max_bytes(100);
    cache.put_track_at("victim", blob(0x01, 40), 1_000);
    cache.put_track_at("survivor", blob(0x02, 40), 1_000);
    cache.put_track_at("newest", blob(0x03, 40), 1_000); // evicts "victim"
    assert_eq!(cache.eviction_count(), 1);

    let stale_at = 1_000 + CACHE_FRESH_TTL_SECS + 10;
    assert!(
        cache.try_begin_revalidate("survivor", stale_at),
        "a survivor's first stale reader still claims the revalidation"
    );
    assert!(
        !cache.try_begin_revalidate("survivor", stale_at),
        "the survivor's claim still dedupes concurrent stale readers"
    );
    cache.finish_revalidate("survivor", stale_at);
    assert!(
        cache.try_begin_revalidate("survivor", stale_at),
        "releasing the survivor's claim still re-arms it"
    );
}
