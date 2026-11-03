//! Real CC0 album track sizes against the `ContentCache` byte cap
//! (tests-only, in-memory).
//!
//! The Holizna "Busted Guitar (JAZZ)" fixture manifest carries the true
//! FLAC byte sizes. Contract pinned here (`workers/stream/src/cache.rs`,
//! `put_track_at` + `evict_over_cap`): eviction runs on insert while
//! `total_bytes() > MAX_CONTENT_CACHE_BYTES`, oldest write first, and the
//! just-written entry is exempt. With the real sizes:
//!
//! - the whole album (133_098_109 bytes) exceeds the cap;
//! - tracks 1+2 coexist; adding track 3 evicts only track 1;
//! - writing the album in position order leaves only track 4.
//!
//! Blobs are synthetic (distinct fill byte per track, since identical
//! content dedupes to one blob) at the manifest's exact lengths.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/stream/Cargo.toml \
//!     --test cache_cc0_album_sizes
//! ```

use nntune_stream::cache::{ContentCache, MAX_CONTENT_CACHE_BYTES};

const MANIFEST: &str =
    include_str!("../../../fixtures/cc0/holiznacc0-busted-guitar-jazz-66ef0593/manifest.json");

/// Track byte sizes in position order, read from the manifest.
fn sizes() -> Vec<usize> {
    let m: serde_json::Value = serde_json::from_str(MANIFEST).expect("manifest parses");
    let mut t: Vec<(u64, usize)> = m["tracks"]
        .as_array()
        .expect("tracks")
        .iter()
        .map(|t| {
            (
                t["position"].as_u64().unwrap(),
                t["bytes"].as_u64().unwrap() as usize,
            )
        })
        .collect();
    t.sort();
    t.into_iter().map(|(_, b)| b).collect()
}

fn id(i: usize) -> String {
    format!("cc0-track-{}", i + 1)
}

/// Put track `i` (0-based) at time `100 + i` with a per-track fill byte.
fn put(cache: &mut ContentCache, sizes: &[usize], i: usize) {
    cache.put_track_at(&id(i), vec![i as u8 + 1; sizes[i]], 100 + i as u64);
}

#[test]
fn manifest_sizes_pin_and_album_exceeds_the_cap() {
    let s = sizes();
    assert_eq!(s, vec![23_512_966, 29_950_043, 32_003_566, 47_631_534]);
    assert_eq!(s.iter().sum::<usize>(), 133_098_109);
    assert!(s.iter().sum::<usize>() > MAX_CONTENT_CACHE_BYTES);
}

#[test]
fn two_smallest_tracks_coexist_under_the_cap() {
    let s = sizes();
    let mut c = ContentCache::new();
    put(&mut c, &s, 0);
    put(&mut c, &s, 1);
    assert!(c.get_track(&id(0)).is_some());
    assert!(c.get_track(&id(1)).is_some());
    assert_eq!(c.total_bytes(), s[0] + s[1]);
    assert_eq!(c.eviction_count(), 0);
}

#[test]
fn third_track_evicts_only_the_oldest() {
    let s = sizes();
    let mut c = ContentCache::new();
    for i in 0..3 {
        put(&mut c, &s, i);
    }
    assert!(c.get_track(&id(0)).is_none());
    assert!(c.get_track(&id(1)).is_some());
    assert!(c.get_track(&id(2)).is_some());
    assert_eq!(c.total_bytes(), s[1] + s[2]);
    assert!(c.total_bytes() <= MAX_CONTENT_CACHE_BYTES);
}

#[test]
fn whole_album_in_order_leaves_only_the_last_track() {
    let s = sizes();
    let mut c = ContentCache::new();
    for i in 0..4 {
        put(&mut c, &s, i);
    }
    for i in 0..3 {
        assert!(
            c.get_track(&id(i)).is_none(),
            "track {} should be evicted",
            i + 1
        );
    }
    assert!(c.get_track(&id(3)).is_some());
    assert_eq!(c.total_bytes(), s[3]);
}
