//! Acceptance: `ContentCache` evicts blobs nothing references any more.
//!
//! Two paths must drop a blob once its last track stops pointing at it:
//! overwriting a track with different bytes, and a timed entry expiring
//! (past fresh + SWR) when it is next accessed. A blob still referenced by
//! another track must survive both paths, and live (fresh or stale) entries
//! must never be evicted. These specs use only the existing public API
//! (`put_track` / `put_track_at` / `get_track` / `get_track_at` /
//! `blob_count`) so they fail on assertions, not compilation, before the fix.
//!
//! Failing command:
//! `cargo test --locked -p nntune-stream --test cache_blob_eviction_acceptance`
//!
//! In-memory cache only: no sockets, providers, or Durable Objects.

use nntune_stream::cache::{CacheServe, ContentCache, CACHE_FRESH_TTL_SECS, CACHE_SWR_WINDOW_SECS};

const V1: &[u8] = b"audio-version-one";
const V2: &[u8] = b"audio-version-two-longer";
const V3: &[u8] = b"audio-version-three";
const SHARED: &[u8] = b"shared-audio-bytes";

fn expired_at(stored_at: u64) -> u64 {
    stored_at + CACHE_FRESH_TTL_SECS + CACHE_SWR_WINDOW_SECS + 1
}

/// Overwriting a track repoints it and drops the now-unreferenced old blob:
/// a chain of distinct rewrites holds exactly one blob throughout.
#[test]
fn overwriting_a_track_drops_the_unreferenced_old_blob() {
    let mut cache = ContentCache::new();
    cache.put_track("t1", V1.to_vec());
    assert_eq!(cache.blob_count(), 1);

    cache.put_track("t1", V2.to_vec());
    assert_eq!(cache.get_track("t1").as_deref(), Some(V2));
    assert_eq!(
        cache.blob_count(),
        1,
        "the replaced blob is referenced by no track and must be evicted"
    );

    cache.put_track("t1", V3.to_vec());
    assert_eq!(cache.get_track("t1").as_deref(), Some(V3));
    assert_eq!(
        cache.blob_count(),
        1,
        "repeated overwrites must not accumulate orphaned blobs"
    );
}

/// The timed write path (the one background revalidation lands on) evicts
/// the replaced blob the same way.
#[test]
fn timed_overwrite_drops_the_unreferenced_old_blob() {
    let mut cache = ContentCache::new();
    cache.put_track_at("t1", V1.to_vec(), 1_000);
    assert_eq!(cache.blob_count(), 1);

    cache.put_track_at("t1", V2.to_vec(), 2_000);
    let (data, serve) = cache
        .get_track_at("t1", 2_000)
        .expect("the rewritten entry serves");
    assert_eq!(data.as_ref(), V2);
    assert_eq!(serve, CacheServe::Fresh { age_secs: 0 });
    assert_eq!(
        cache.blob_count(),
        1,
        "a timed overwrite must evict the blob it replaced"
    );
}

/// Rewriting a track with identical bytes keeps the one deduplicated blob:
/// eviction must not drop the blob the track still points at.
#[test]
fn overwriting_with_identical_bytes_keeps_the_single_blob() {
    let mut cache = ContentCache::new();
    cache.put_track("t1", SHARED.to_vec());
    cache.put_track("t1", SHARED.to_vec());

    assert_eq!(cache.blob_count(), 1);
    assert_eq!(cache.get_track("t1").as_deref(), Some(SHARED));
}

/// An entry past fresh + SWR is pruned on access: the read misses, and
/// afterwards neither the blob nor the track pointer remains.
#[test]
fn expired_entry_is_pruned_on_access() {
    let mut cache = ContentCache::new();
    cache.put_track_at("t1", V1.to_vec(), 1_000);
    assert_eq!(cache.blob_count(), 1);

    assert!(
        cache.get_track_at("t1", expired_at(1_000)).is_none(),
        "past fresh + SWR the entry must re-fetch upstream"
    );
    assert_eq!(
        cache.blob_count(),
        0,
        "the expired entry's blob is referenced by no track and must be evicted"
    );
    assert!(
        cache.get_track("t1").is_none(),
        "the expired track pointer must be pruned, not left serving untimed"
    );
}

/// Fresh and stale accesses evict nothing: only expiry past fresh + SWR
/// prunes, so live entries keep serving with their blob intact.
#[test]
fn fresh_and_stale_accesses_do_not_evict() {
    let mut cache = ContentCache::new();
    cache.put_track_at("t1", V1.to_vec(), 1_000);

    let (data, serve) = cache
        .get_track_at("t1", 1_000 + CACHE_FRESH_TTL_SECS)
        .expect("exactly at the fresh TTL the entry is still fresh");
    assert_eq!(data.as_ref(), V1);
    assert!(matches!(serve, CacheServe::Fresh { .. }));
    assert_eq!(cache.blob_count(), 1, "a fresh access must not evict");

    let (data, serve) = cache
        .get_track_at("t1", 1_000 + CACHE_FRESH_TTL_SECS + CACHE_SWR_WINDOW_SECS)
        .expect("exactly at the end of the SWR window the entry still serves stale");
    assert_eq!(data.as_ref(), V1);
    assert!(matches!(serve, CacheServe::Stale { .. }));
    assert_eq!(cache.blob_count(), 1, "a stale access must not evict");
    assert_eq!(cache.get_track("t1").as_deref(), Some(V1));
}

/// A blob still referenced by another track survives an overwrite of one of
/// its tracks: only the newly written blob is added.
#[test]
fn shared_blob_survives_overwrite_of_one_track() {
    let mut cache = ContentCache::new();
    cache.put_track("t1", SHARED.to_vec());
    cache.put_track("t2", SHARED.to_vec());
    assert_eq!(cache.blob_count(), 1, "identical audio is stored once");

    cache.put_track("t1", V2.to_vec());
    assert_eq!(
        cache.blob_count(),
        2,
        "the shared blob is still referenced by t2 and must survive t1's overwrite"
    );
    assert_eq!(cache.get_track("t1").as_deref(), Some(V2));
    assert_eq!(
        cache.get_track("t2").as_deref(),
        Some(SHARED),
        "t2 must keep serving the shared blob"
    );
}

/// A blob still referenced by another track survives the expiry of one of
/// its tracks: the expired track is pruned, the surviving track keeps
/// serving the shared blob.
#[test]
fn shared_blob_survives_expiry_of_one_track() {
    let mut cache = ContentCache::new();
    cache.put_track_at("t1", SHARED.to_vec(), 1_000);
    cache.put_track("t2", SHARED.to_vec());
    assert_eq!(cache.blob_count(), 1, "identical audio is stored once");

    assert!(
        cache.get_track_at("t1", expired_at(1_000)).is_none(),
        "t1 is past fresh + SWR and must miss"
    );
    assert!(
        cache.get_track("t1").is_none(),
        "the expired track pointer must be pruned"
    );
    assert_eq!(
        cache.blob_count(),
        1,
        "the shared blob is still referenced by t2 and must survive t1's expiry"
    );
    let (data, serve) = cache
        .get_track_at("t2", expired_at(1_000))
        .expect("the untimed peer keeps serving the shared blob");
    assert_eq!(data.as_ref(), SHARED);
    assert_eq!(serve, CacheServe::Fresh { age_secs: 0 });
}

/// Expiring one track prunes only its own blob: a distinct live blob held
/// for another track is untouched.
#[test]
fn expired_access_evicts_only_the_expired_blob() {
    let mut cache = ContentCache::new();
    cache.put_track_at("t1", V1.to_vec(), 1_000);
    cache.put_track_at("t2", V2.to_vec(), 5_000);
    assert_eq!(cache.blob_count(), 2);

    // At 5_000, t1 (written at 1_000) is past fresh + SWR; t2 is brand new.
    assert!(
        cache.get_track_at("t1", 5_000).is_none(),
        "t1 is past fresh + SWR and must miss"
    );
    assert_eq!(
        cache.blob_count(),
        1,
        "only the expired entry's blob must be evicted"
    );
    let (data, serve) = cache
        .get_track_at("t2", 5_000)
        .expect("the live entry keeps serving");
    assert_eq!(data.as_ref(), V2);
    assert_eq!(serve, CacheServe::Fresh { age_secs: 0 });
}
