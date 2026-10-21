//! Content-addressed audio cache: the R2 shape behind a testable interface.
//!
//! - Blobs live under `audio/<sha256-hex-of-plaintext>`; identical audio
//!   uploaded for two tracks is stored once (dedup).
//! - `track/<track_id>` is a tiny pointer to the current blob hash.
//! - [`MultipartUpload`] models the R2 multipart write the Worker streams
//!   into while serving a miss: parts buffer, `complete` concatenates.
//!
//! [`ContentCache`] is the in-memory stand-in; production swaps it for R2
//! calls with identical semantics (see `wrangler.toml` bindings).
//!
//! Freshness ([`CACHE_FRESH_TTL_SECS`]) and stale-while-revalidate
//! ([`CACHE_SWR_WINDOW_SECS`]): timed entries written with
//! [`ContentCache::put_track_at`] serve fresh as `HIT`, then stale as `STALE`
//! while the worker revalidates in the background (see
//! [`CacheServe::needs_revalidate`]); past both windows the entry is a `MISS`
//! and the play path re-fetches upstream. Untimed [`ContentCache::put_track`]
//! entries never go stale (legacy callers). Freshness is tracked per track,
//! not per deduplicated blob, so restamping one track never changes another
//! track's serve. Revalidation itself is claimed per track via
//! [`ContentCache::try_begin_revalidate`], so concurrent stale reads trigger
//! one background refresh, not one per reader. Claims carry an acquisition
//! timestamp and expire after [`REVALIDATE_CLAIM_TTL_SECS`] if their owner
//! disappears without finishing.

use std::collections::HashMap;
use std::sync::Arc;

use crate::grant::sha256;

/// Fresh window in seconds: a timed entry at most this old serves as `HIT`
/// with no revalidation. Documented in `workers/stream/README.md`.
pub const CACHE_FRESH_TTL_SECS: u64 = 300;

/// Stale-while-revalidate window in seconds past the fresh TTL: the entry
/// still serves immediately (as `STALE`, flagging background revalidation)
/// instead of blocking on upstream. Documented in `workers/stream/README.md`.
pub const CACHE_SWR_WINDOW_SECS: u64 = 3_600;

/// A background revalidation claim is reclaimable after this many seconds if
/// its owner disappears without completing or explicitly releasing it.
pub const REVALIDATE_CLAIM_TTL_SECS: u64 = 300;

/// Hard cap on the total blob bytes a default [`ContentCache`] holds: 64 MiB,
/// about half a 128 MiB Worker isolate, leaving room for several whole
/// tracks plus the runtime. Byte accounting is over distinct blobs — a blob
/// shared by several tracks counts once — and an over-cap insert evicts
/// track entries oldest-first (last-write order) until the cache is back at
/// or under the cap, never evicting the just-written entry. Documented in
/// `workers/stream/README.md` like the TTL constants.
pub const MAX_CONTENT_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// How a cached entry served a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheServe {
    /// Within the fresh TTL: serve, no revalidation owed.
    Fresh { age_secs: u64 },
    /// Past the fresh TTL but inside the SWR window: serve immediately and
    /// revalidate in the background.
    Stale { age_secs: u64 },
}

impl CacheServe {
    /// Seconds since the entry was written.
    pub fn age_secs(&self) -> u64 {
        match *self {
            CacheServe::Fresh { age_secs } | CacheServe::Stale { age_secs } => age_secs,
        }
    }

    /// Whether serving this entry owes a background revalidation.
    pub fn needs_revalidate(&self) -> bool {
        matches!(*self, CacheServe::Stale { .. })
    }

    /// The `Cache-Status` value for this serve.
    pub fn cache_status(&self) -> CacheStatus {
        match *self {
            CacheServe::Fresh { .. } => CacheStatus::Hit,
            CacheServe::Stale { .. } => CacheStatus::Stale,
        }
    }
}

/// Value for the `Cache-Status` response header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheStatus {
    Hit,
    Miss,
    Stale,
}

impl CacheStatus {
    /// The wire value pinned by `cache_status_header_values_pin_the_wire_contract`.
    pub fn header_value(self) -> &'static str {
        match self {
            CacheStatus::Hit => "HIT",
            CacheStatus::Miss => "MISS",
            CacheStatus::Stale => "STALE",
        }
    }
}

struct BlobEntry {
    data: Arc<[u8]>,
}

struct TrackEntry {
    hash: String,
    /// `None` for untimed [`ContentCache::put_track`] writes, which never go
    /// stale.
    stored_at_secs: Option<u64>,
    /// Monotonic last-write order (see [`ContentCache::write_seq`]): a
    /// rewrite restamps this, so eviction order treats a rewrite as the
    /// newest write even when the blob content is unchanged.
    last_write_seq: u64,
}

pub struct ContentCache {
    blobs: HashMap<String, BlobEntry>,
    index: HashMap<String, TrackEntry>,
    /// Tracks with a background revalidation already claimed, with claim time
    /// so an abandoned claim can expire (see [`ContentCache::try_begin_revalidate`]).
    revalidating: HashMap<String, u64>,
    /// Byte cap enforced on insert (see [`MAX_CONTENT_CACHE_BYTES`]).
    max_bytes: usize,
    /// Monotonic counter stamping each write's eviction rank.
    write_seq: u64,
    /// Track entries evicted for capacity so far (the countable probe the
    /// observability path reports; time-expiry prunes are not evictions).
    evictions: u64,
}

impl ContentCache {
    pub fn new() -> Self {
        Self::with_max_bytes(MAX_CONTENT_CACHE_BYTES)
    }

    /// A cache capped at `max_bytes` instead of [`MAX_CONTENT_CACHE_BYTES`]
    /// (the acceptance tests pin small caps against this constructor).
    pub fn with_max_bytes(max_bytes: usize) -> Self {
        Self {
            blobs: HashMap::new(),
            index: HashMap::new(),
            revalidating: HashMap::new(),
            max_bytes,
            write_seq: 0,
            evictions: 0,
        }
    }

    /// The configured byte cap.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Sum of the lengths of the distinct blobs currently held. A blob
    /// shared (via `Arc`) by several tracks counts once; a blob no track
    /// points at any more is dropped on rewrite/evict and counts zero.
    pub fn total_bytes(&self) -> usize {
        self.blobs.values().map(|b| b.data.len()).sum()
    }

    /// Track entries evicted for capacity so far. Time-expiry prunes
    /// ([`ContentCache::get_track_at`]) never increment this.
    pub fn eviction_count(&self) -> u64 {
        self.evictions
    }

    /// Store `plaintext` for `track_id`; returns the blob hash hex.
    /// Overwriting with different bytes evicts the old blob once its last
    /// track stops pointing at it; a blob still referenced by another track
    /// survives. An over-cap insert evicts track entries oldest-first (see
    /// [`MAX_CONTENT_CACHE_BYTES`]); the just-written entry is exempt.
    pub fn put_track(&mut self, track_id: &str, plaintext: Vec<u8>) -> String {
        let hash = Self::hex_hash(&plaintext);
        let old_hash = self.index.get(track_id).map(|e| e.hash.clone());
        self.blobs.entry(hash.clone()).or_insert_with(|| BlobEntry {
            data: Arc::from(plaintext),
        });
        self.write_seq += 1;
        let seq = self.write_seq;
        self.index.insert(
            track_id.to_string(),
            TrackEntry {
                hash: hash.clone(),
                stored_at_secs: None,
                last_write_seq: seq,
            },
        );
        self.revalidating.remove(track_id);
        if let Some(old) = old_hash {
            if old != hash {
                self.remove_blob_if_unreferenced(&old);
            }
        }
        self.evict_over_cap(track_id);
        hash
    }

    /// Store `plaintext` for `track_id` at time `now_secs` (unix seconds);
    /// returns the blob hash hex. Rewriting a track repoints it and restarts
    /// its freshness clock — this is also how a background revalidation
    /// refreshes a stale entry. An over-cap insert evicts track entries
    /// oldest-first (see [`MAX_CONTENT_CACHE_BYTES`]); the just-written
    /// entry is exempt.
    pub fn put_track_at(&mut self, track_id: &str, plaintext: Vec<u8>, now_secs: u64) -> String {
        let hash = Self::hex_hash(&plaintext);
        let old_hash = self.index.get(track_id).map(|e| e.hash.clone());
        // A revalidation may race a content change: always restamp this
        // track, so its age counts from the latest write without affecting
        // other tracks that share the deduplicated blob.
        self.blobs.entry(hash.clone()).or_insert_with(|| BlobEntry {
            data: Arc::from(plaintext),
        });
        self.write_seq += 1;
        let seq = self.write_seq;
        self.index.insert(
            track_id.to_string(),
            TrackEntry {
                hash: hash.clone(),
                stored_at_secs: Some(now_secs),
                last_write_seq: seq,
            },
        );
        // The rewrite *is* the revalidation landing: release the claim so a
        // later stale serve can claim the next one.
        self.revalidating.remove(track_id);
        if let Some(old) = old_hash {
            if old != hash {
                self.remove_blob_if_unreferenced(&old);
            }
        }
        self.evict_over_cap(track_id);
        hash
    }

    /// Claim the background revalidation for `track_id`: returns `true` to
    /// exactly one caller until the claim is released — by the rewrite
    /// landing ([`ContentCache::put_track_at`]), by
    /// [`ContentCache::finish_revalidate`] after a failed attempt, or by
    /// [`REVALIDATE_CLAIM_TTL_SECS`] if the owner disappears. `now_secs` is
    /// unix time, in the same clock domain as the cache entry timestamps.
    /// This dedupes concurrent stale readers while ensuring an abandoned
    /// revalidation does not suppress retries indefinitely.
    pub fn try_begin_revalidate(&mut self, track_id: &str, now_secs: u64) -> bool {
        match self.revalidating.get(track_id) {
            Some(claimed_at)
                if now_secs.saturating_sub(*claimed_at) < REVALIDATE_CLAIM_TTL_SECS =>
            {
                false
            }
            _ => {
                self.revalidating.insert(track_id.to_string(), now_secs);
                true
            }
        }
    }

    /// Release `track_id`'s claim without a rewrite (the background re-fetch
    /// failed or was abandoned). `claimed_at_secs` must be the timestamp used
    /// to acquire that claim, so a late owner cannot release a newer claim.
    pub fn finish_revalidate(&mut self, track_id: &str, claimed_at_secs: u64) {
        if self.revalidating.get(track_id) == Some(&claimed_at_secs) {
            self.revalidating.remove(track_id);
        }
    }

    /// Fetch the cached plaintext for `track_id`, if any. Shared, not
    /// copied: a hit never duplicates the track in memory.
    pub fn get_track(&self, track_id: &str) -> Option<Arc<[u8]>> {
        self.index
            .get(track_id)
            .and_then(|entry| self.blobs.get(&entry.hash))
            .map(|e| Arc::clone(&e.data))
    }

    /// Fetch the cached plaintext for `track_id` at time `now_secs` (unix
    /// seconds) with its freshness. Fresh entries serve as-is; stale entries
    /// serve immediately while owing a background revalidation (see
    /// [`CacheServe`]); expired entries (past fresh + SWR) and unknown
    /// tracks return `None` so the caller re-fetches upstream. An expired
    /// access also prunes the track pointer and evicts its blob once no
    /// remaining track references it; fresh and stale accesses evict
    /// nothing. Takes `&mut` for that prune-on-access (the play path already
    /// holds `&mut ContentCache`).
    pub fn get_track_at(
        &mut self,
        track_id: &str,
        now_secs: u64,
    ) -> Option<(Arc<[u8]>, CacheServe)> {
        let (hash, stored_at_secs) = {
            let track = self.index.get(track_id)?;
            (track.hash.clone(), track.stored_at_secs)
        };
        let data = match self.blobs.get(&hash) {
            Some(blob) => Arc::clone(&blob.data),
            None => {
                // Dangling pointer: no blob behind it, so drop the pointer.
                self.index.remove(track_id);
                return None;
            }
        };
        match stored_at_secs {
            None => Some((data, CacheServe::Fresh { age_secs: 0 })),
            Some(stored) => {
                let age = now_secs.saturating_sub(stored);
                if age <= CACHE_FRESH_TTL_SECS {
                    Some((data, CacheServe::Fresh { age_secs: age }))
                } else if age <= CACHE_FRESH_TTL_SECS + CACHE_SWR_WINDOW_SECS {
                    Some((data, CacheServe::Stale { age_secs: age }))
                } else {
                    self.index.remove(track_id);
                    self.remove_blob_if_unreferenced(&hash);
                    None
                }
            }
        }
    }

    /// Evict track entries oldest-first (last-write order) while the cache
    /// holds more than [`ContentCache::max_bytes`] blob bytes. Exactly at
    /// the cap is not over it. `just_written` is exempt: a lone over-cap
    /// entry serves alone rather than the write vanishing into its own
    /// eviction pass. Each evicted track counts one eviction even when its
    /// blob is shared and frees no bytes, so eviction continues past
    /// surviving sharers until the cache is back at or under the cap.
    fn evict_over_cap(&mut self, just_written: &str) {
        let mut total = self.total_bytes();
        while total > self.max_bytes {
            let oldest = self
                .index
                .iter()
                .filter(|(id, _)| id.as_str() != just_written)
                .min_by_key(|(_, entry)| entry.last_write_seq)
                .map(|(id, _)| id.clone());
            let Some(victim) = oldest else {
                break;
            };
            let hash = self
                .index
                .remove(&victim)
                .map(|entry| entry.hash)
                .unwrap_or_default();
            // The evicted track can never serve or revalidate again: drop
            // its claim so the map cannot grow on dead entries.
            self.revalidating.remove(&victim);
            self.evictions += 1;
            total = total.saturating_sub(self.remove_blob_if_unreferenced(&hash));
        }
    }

    /// Drop the blob under `hash` once no track in the index references it.
    /// A blob still shared by another track survives. Returns the bytes
    /// freed (the blob's length, or zero when it survives).
    fn remove_blob_if_unreferenced(&mut self, hash: &str) -> usize {
        if self.index.values().any(|t| t.hash == hash) {
            return 0;
        }
        self.blobs.remove(hash).map(|b| b.data.len()).unwrap_or(0)
    }

    /// Distinct blobs held (dedup probe).
    pub fn blob_count(&self) -> usize {
        self.blobs.len()
    }

    /// Lowercase hex SHA-256 of `data`.
    pub fn hex_hash(data: &[u8]) -> String {
        sha256(data).iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl Default for ContentCache {
    fn default() -> Self {
        Self::new()
    }
}

/// R2 multipart part floor: every part except the last must be at least
/// this big, and all non-final parts must be the same size (R2 errors
/// 10011/10048 otherwise). Flushed parts are exactly this size.
pub const R2_MIN_PART: usize = 5 * 1024 * 1024;

/// A streaming R2 multipart write. Decoded segments (typically < 1 MiB)
/// coalesce into fixed [`R2_MIN_PART`]-byte parts: each full part is flushed
/// (production uploads it to R2 and retains nothing) while the remainder
/// stages toward the next. Steady-state memory is one part plus the segment
/// being decoded. This in-memory stand-in keeps flushed parts so `complete`
/// can hand the bytes over without a copy.
pub struct MultipartUpload {
    flushed: Vec<Vec<u8>>,
    pending: Vec<u8>,
}

impl MultipartUpload {
    pub fn begin(track_id: &str) -> Self {
        let _ = track_id;
        Self {
            flushed: Vec::new(),
            pending: Vec::new(),
        }
    }

    pub fn part(&mut self, data: &[u8]) {
        self.pending.extend_from_slice(data);
        while self.pending.len() >= R2_MIN_PART {
            let rest = self.pending.split_off(R2_MIN_PART);
            self.flushed
                .push(std::mem::replace(&mut self.pending, rest));
        }
    }

    pub fn complete(mut self) -> Vec<u8> {
        let mut full: Vec<u8> = self.flushed.concat();
        full.append(&mut self.pending);
        full
    }

    /// Total bytes staged (flushed + pending).
    pub fn bytes_staged(&self) -> u64 {
        (self.flushed.iter().map(Vec::len).sum::<usize>() + self.pending.len()) as u64
    }

    /// Sizes of the flushed full parts (each exactly [`R2_MIN_PART`]).
    pub fn flushed_part_sizes(&self) -> Vec<usize> {
        self.flushed.iter().map(Vec::len).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_resolve_to_their_plaintext() {
        let mut cache = ContentCache::new();
        assert!(cache.get_track("t1").is_none());
        cache.put_track("t1", b"audio-bytes".to_vec());
        assert_eq!(cache.get_track("t1").as_deref(), Some(&b"audio-bytes"[..]));
    }

    #[test]
    fn identical_audio_is_stored_once() {
        let mut cache = ContentCache::new();
        let h1 = cache.put_track("t1", b"same-bytes".to_vec());
        let h2 = cache.put_track("t2", b"same-bytes".to_vec());
        assert_eq!(h1, h2);
        assert_eq!(cache.blob_count(), 1);
    }

    #[test]
    fn overwriting_a_track_repoints_it() {
        let mut cache = ContentCache::new();
        cache.put_track("t1", b"v1".to_vec());
        cache.put_track("t1", b"v2-longer".to_vec());
        assert_eq!(cache.get_track("t1").as_deref(), Some(&b"v2-longer"[..]));
    }

    #[test]
    fn multipart_buffers_parts_until_complete() {
        let mut up = MultipartUpload::begin("t1");
        up.part(b"hel");
        up.part(b"lo");
        assert_eq!(up.bytes_staged(), 5);
        assert!(up.flushed_part_sizes().is_empty());
        assert_eq!(up.complete(), b"hello");
    }

    #[test]
    fn swr_windows_are_the_documented_values() {
        assert_eq!(
            CACHE_FRESH_TTL_SECS, 300,
            "worker README documents this value; change both together"
        );
        assert_eq!(
            CACHE_SWR_WINDOW_SECS, 3_600,
            "worker README documents this value; change both together"
        );
    }

    #[test]
    fn cache_status_header_values_pin_the_wire_contract() {
        assert_eq!(CacheStatus::Hit.header_value(), "HIT");
        assert_eq!(CacheStatus::Miss.header_value(), "MISS");
        assert_eq!(CacheStatus::Stale.header_value(), "STALE");
    }

    #[test]
    fn fresh_entry_serves_hit_without_revalidation() {
        let mut cache = ContentCache::new();
        cache.put_track_at("t1", b"audio-bytes".to_vec(), 1_000);
        let (data, serve) = cache
            .get_track_at("t1", 1_000 + CACHE_FRESH_TTL_SECS - 1)
            .expect("fresh entry serves");
        assert_eq!(data.as_ref(), b"audio-bytes");
        assert_eq!(
            serve,
            CacheServe::Fresh {
                age_secs: CACHE_FRESH_TTL_SECS - 1
            }
        );
        assert!(!serve.needs_revalidate());
        assert_eq!(serve.cache_status(), CacheStatus::Hit);
    }

    #[test]
    fn stale_entry_serves_data_while_flagging_revalidation() {
        let mut cache = ContentCache::new();
        cache.put_track_at("t1", b"audio-bytes".to_vec(), 1_000);
        let (data, serve) = cache
            .get_track_at("t1", 1_000 + CACHE_FRESH_TTL_SECS + 10)
            .expect("stale entry still serves immediately");
        assert_eq!(data.as_ref(), b"audio-bytes");
        assert_eq!(
            serve,
            CacheServe::Stale {
                age_secs: CACHE_FRESH_TTL_SECS + 10
            }
        );
        assert!(serve.needs_revalidate());
        assert_eq!(serve.cache_status(), CacheStatus::Stale);
    }

    #[test]
    fn expired_entry_is_a_miss() {
        let mut cache = ContentCache::new();
        cache.put_track_at("t1", b"audio-bytes".to_vec(), 1_000);
        assert!(
            cache
                .get_track_at(
                    "t1",
                    1_000 + CACHE_FRESH_TTL_SECS + CACHE_SWR_WINDOW_SECS + 1
                )
                .is_none(),
            "past fresh + SWR the entry must re-fetch upstream"
        );
        assert!(
            cache.get_track_at("missing", 1_000).is_none(),
            "unknown tracks miss too"
        );
    }

    #[test]
    fn swr_boundaries_are_fresh_then_stale_then_miss() {
        let mut cache = ContentCache::new();
        cache.put_track_at("t1", b"v".to_vec(), 1_000);
        assert!(
            matches!(
                cache
                    .get_track_at("t1", 1_000 + CACHE_FRESH_TTL_SECS)
                    .map(|(_, s)| s),
                Some(CacheServe::Fresh { .. })
            ),
            "exactly at the fresh TTL the entry is still fresh"
        );
        assert!(
            matches!(
                cache
                    .get_track_at("t1", 1_000 + CACHE_FRESH_TTL_SECS + CACHE_SWR_WINDOW_SECS)
                    .map(|(_, s)| s),
                Some(CacheServe::Stale { .. })
            ),
            "exactly at the end of the SWR window the entry still serves stale"
        );
    }

    #[test]
    fn untimed_puts_stay_fresh_for_legacy_callers() {
        let mut cache = ContentCache::new();
        cache.put_track("t1", b"audio-bytes".to_vec());
        let (data, serve) = cache
            .get_track_at("t1", 9_999_999_999)
            .expect("untimed entries never go stale");
        assert_eq!(data.as_ref(), b"audio-bytes");
        assert_eq!(serve, CacheServe::Fresh { age_secs: 0 });
    }

    #[test]
    fn shared_blob_freshness_is_tracked_per_track() {
        let mut cache = ContentCache::new();
        let t1_hash = cache.put_track_at("t1", b"shared-bytes".to_vec(), 1_000);
        let t2_hash = cache.put_track("t2", b"shared-bytes".to_vec());
        assert_eq!(t1_hash, t2_hash);

        let old = 1_000 + CACHE_FRESH_TTL_SECS + 1;
        assert!(matches!(
            cache.get_track_at("t1", old).map(|(_, serve)| serve),
            Some(CacheServe::Stale { .. })
        ));
        assert_eq!(
            cache.get_track_at("t2", old).map(|(_, serve)| serve),
            Some(CacheServe::Fresh { age_secs: 0 }),
            "a timed write for one track must not make an untimed dedup peer stale"
        );

        cache.put_track_at("t2", b"shared-bytes".to_vec(), old);
        // t1 was stamped at 1_000 and is read at 1_000 + 2*(FRESH+1): its
        // age counts from its own write. Had t2's restamp refreshed the
        // shared blob, t1 would read Fresh/at a younger age instead.
        assert_eq!(
            cache
                .get_track_at("t1", old + CACHE_FRESH_TTL_SECS + 1)
                .map(|(_, serve)| serve),
            Some(CacheServe::Stale {
                age_secs: 2 * (CACHE_FRESH_TTL_SECS + 1)
            }),
            "restamping a deduplicated peer must not refresh the other track"
        );
        assert_eq!(
            cache.get_track_at("t2", old).map(|(_, serve)| serve),
            Some(CacheServe::Fresh { age_secs: 0 })
        );
    }

    #[test]
    fn revalidation_is_claimed_once_per_track_until_it_resolves() {
        let mut cache = ContentCache::new();
        cache.put_track_at("t1", b"bytes".to_vec(), 1_000);

        assert!(
            cache.try_begin_revalidate("t1", 1_000),
            "first stale reader claims"
        );
        assert!(
            !cache.try_begin_revalidate("t1", 1_001),
            "concurrent stale readers must not each start a revalidation"
        );
        assert!(
            cache.try_begin_revalidate("t2", 1_000),
            "claims are per track, not global"
        );

        // A failed attempt releases the claim so a later reader retries.
        cache.finish_revalidate("t1", 1_000);
        assert!(cache.try_begin_revalidate("t1", 1_002));

        // The rewrite landing also releases it (and refreshes the entry).
        cache.put_track_at("t1", b"bytes".to_vec(), 2_000);
        assert!(cache.try_begin_revalidate("t1", 2_000));
    }

    #[test]
    fn abandoned_revalidation_claim_expires_and_can_be_reclaimed() {
        let mut cache = ContentCache::new();
        assert!(cache.try_begin_revalidate("t1", 1_000));
        assert!(
            !cache.try_begin_revalidate("t1", 1_000 + REVALIDATE_CLAIM_TTL_SECS - 1),
            "an active claim remains owned until its TTL elapses"
        );
        assert!(
            cache.try_begin_revalidate("t1", 1_000 + REVALIDATE_CLAIM_TTL_SECS),
            "an abandoned claim is available at the TTL boundary"
        );
        let reclaimed_at = 1_000 + REVALIDATE_CLAIM_TTL_SECS;
        assert!(
            !cache.try_begin_revalidate("t1", reclaimed_at),
            "the reclaimed claim dedupes subsequent readers"
        );
        cache.finish_revalidate("t1", 1_000);
        assert!(
            !cache.try_begin_revalidate("t1", reclaimed_at),
            "a late failed owner cannot release a newer claim"
        );
        cache.finish_revalidate("t1", reclaimed_at);
        assert!(cache.try_begin_revalidate("t1", reclaimed_at));
    }

    #[test]
    fn revalidation_refreshes_a_stale_entry_back_to_fresh() {
        let mut cache = ContentCache::new();
        cache.put_track_at("t1", b"v1".to_vec(), 1_000);
        let now = 1_000 + CACHE_FRESH_TTL_SECS + 10;
        let (_, serve) = cache.get_track_at("t1", now).expect("stale serves");
        assert!(serve.needs_revalidate());
        // The background refresh completes: the entry is rewritten at `now`.
        cache.put_track_at("t1", b"v2".to_vec(), now);
        let (data, serve) = cache.get_track_at("t1", now).expect("refreshed serves");
        assert_eq!(data.as_ref(), b"v2");
        assert_eq!(serve, CacheServe::Fresh { age_secs: 0 });
    }

    #[test]
    fn sub_5mib_segments_coalesce_into_fixed_r2_parts() {
        // Usenet segments (~0.7 MB) can never go to R2 one-per-part: every
        // non-final part must be >= 5 MiB and all non-final parts the same
        // size. Sixteen 700 KiB segments = 11.2 MiB must flush exactly two
        // full 5 MiB parts and keep the 1.2 MiB remainder staged.
        let mut up = MultipartUpload::begin("t1");
        let seg = vec![0xABu8; 700 * 1024];
        for _ in 0..16 {
            up.part(&seg);
        }
        assert_eq!(up.flushed_part_sizes(), &[R2_MIN_PART, R2_MIN_PART]);
        assert_eq!(up.bytes_staged(), 16 * seg.len() as u64);
        let full = up.complete();
        assert_eq!(full.len(), 16 * seg.len());
        assert!(full.iter().all(|&b| b == 0xAB));
    }

    #[test]
    fn capacity_eviction_drops_the_victims_revalidation_claim() {
        let mut cache = ContentCache::with_max_bytes(10);
        cache.put_track_at("victim", vec![1u8; 6], 1_000);
        let stale_at = 1_000 + CACHE_FRESH_TTL_SECS + 10;
        assert!(
            cache.try_begin_revalidate("victim", stale_at),
            "stale victim claims its revalidation"
        );
        // Over-cap insert: 6 + 6 bytes against a 10-byte cap evicts the
        // victim (oldest write) while the just-written entry is exempt.
        cache.put_track_at("newcomer", vec![2u8; 6], stale_at);
        assert!(
            cache.get_track("victim").is_none(),
            "victim must be capacity-evicted"
        );
        assert!(
            cache.try_begin_revalidate("victim", stale_at),
            "eviction drops the claim: a re-cached track is reclaimable \
             immediately instead of waiting out the claim TTL"
        );
    }

    #[test]
    fn time_expiry_prune_does_not_count_as_a_capacity_eviction() {
        let mut cache = ContentCache::new();
        cache.put_track_at("t1", b"audio-bytes".to_vec(), 1_000);
        assert!(
            cache
                .get_track_at(
                    "t1",
                    1_000 + CACHE_FRESH_TTL_SECS + CACHE_SWR_WINDOW_SECS + 1
                )
                .is_none(),
            "past fresh + SWR the entry expires"
        );
        assert_eq!(
            cache.eviction_count(),
            0,
            "a time-expiry prune is not a capacity eviction"
        );
    }
}
