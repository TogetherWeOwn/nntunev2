//! Boundary pins for cached search / release-recordings payload validation.
//!
//! Companion to the protected `cache_payload_validation` suite (which this
//! file does not touch): it pins the exact-cap boundaries the validators
//! allow through as hits, so a `>` → `>=` regression on either length check
//! turns this file red instead of silently degrading every full page /
//! large release into a permanent cache miss. It also pins the per-row
//! `release_mbid` type check as a miss.
//!
//! Standalone integration target: in-memory `SuccessCache` / `MbRepo`
//! doubles, no KV/SQL/service calls.

use nntune_mb::{
    handle_release_recordings, handle_search_paged, ApiResponse, Kind, MbRepo, Recording,
    RepoError, SearchHit, SuccessCache,
};
use std::cell::RefCell;
use std::future::Future;
use std::task::{Context, Poll, Waker};

/// Minimal executor: the fakes below complete on first poll.
fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut future = Box::pin(future);
    for _ in 0..100 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
    panic!("test future did not complete");
}

const RELEASE_MBID: &str = "123e4567-e89b-12d3-a456-426614174000";

/// Pinned TTLs from `workers/mb/src/lib.rs` (`SEARCH_CACHE_TTL_S` /
/// `LOOKUP_CACHE_TTL_S`) and the existing caps (`MAX_PAGE_SIZE` = 25,
/// `RELEASE_RECORDINGS_LIMIT` = 1000). Hardcoded on purpose: a TTL or cap
/// change is a separate slice and must turn this file red first.
const SEARCH_TTL: u32 = 300;
const RECORDINGS_TTL: u32 = 86_400;
const DEFAULT_PAGE_SIZE: usize = 25;
const RECORDINGS_CAP: usize = 1000;

/// Distinct UUID-shaped hit MBIDs (`is_valid_mbid` shape check only).
fn hit_mbid(i: usize) -> String {
    format!("123e4567-e89b-12d3-a456-42661417{i:04}")
}

struct FakeRepo {
    search_calls: RefCell<u32>,
    recordings_calls: RefCell<u32>,
}

impl MbRepo for FakeRepo {
    async fn lookup(
        &self,
        _kind: Kind,
        _mbid: &str,
    ) -> Result<Option<nntune_mb::Entity>, RepoError> {
        Ok(None)
    }

    async fn search(
        &self,
        _kind: Kind,
        _normalized_q: &str,
        _limit: u32,
        _offset: u32,
    ) -> Result<Vec<SearchHit>, RepoError> {
        *self.search_calls.borrow_mut() += 1;
        Ok(vec![])
    }

    async fn release_recordings(
        &self,
        _release_mbid: &str,
    ) -> Result<Option<Vec<Recording>>, RepoError> {
        *self.recordings_calls.borrow_mut() += 1;
        Ok(Some(vec![Recording {
            mbid: hit_mbid(1),
            release_mbid: Some(RELEASE_MBID.to_string()),
            title: "Fresh Track".to_string(),
            track_no: Some(1),
            duration_ms: Some(180_000),
            medium_no: Some(1),
        }]))
    }
}

/// Seeded cache: `get` returns the seed for any key, `put` is recorded.
struct SeededCache {
    seed: Option<String>,
    puts: RefCell<Vec<(String, String, u32)>>,
}

impl SeededCache {
    fn seeded(body: String) -> Self {
        Self {
            seed: Some(body),
            puts: RefCell::new(Vec::new()),
        }
    }
}

impl SuccessCache for SeededCache {
    async fn get(&self, _key: &str) -> Option<String> {
        self.seed.clone()
    }

    async fn put(&self, key: &str, body: &str, ttl_s: u32) {
        self.puts
            .borrow_mut()
            .push((key.to_string(), body.to_string(), ttl_s));
    }
}

fn artist_hit(i: usize) -> serde_json::Value {
    serde_json::json!({
        "mbid": hit_mbid(i),
        "name": format!("Cached Artist {i}"),
        "score": 0.8,
    })
}

fn recording_row(i: usize) -> serde_json::Value {
    serde_json::json!({
        "mbid": hit_mbid(i),
        "release_mbid": RELEASE_MBID,
        "title": format!("Cached Track {i}"),
        "track_no": 1,
        "duration_ms": 180000,
        "medium_no": 1,
    })
}

/// A cached body served as a hit comes back verbatim with no repo read
/// and no cache rewrite.
fn assert_hit(resp: &ApiResponse, seed: &str, ttl: u32, repo_calls: u32, cache: &SeededCache) {
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, seed, "valid cached body is served verbatim");
    assert_eq!(resp.cache_ttl, Some(ttl));
    assert_eq!(repo_calls, 0, "valid hit must not read repo");
    assert_eq!(
        cache.puts.borrow().len(),
        0,
        "valid hit must not rewrite cache"
    );
}

#[test]
fn cached_search_with_exactly_limit_results_is_a_hit() {
    // A full page (exactly `limit` hits) is the common cached shape; the
    // validator allows `len <= limit`, so 10-for-10 is a hit.
    let results: Vec<serde_json::Value> = (0..10).map(artist_hit).collect();
    let seed = serde_json::json!({
        "q": "blue monday",
        "type": "artist",
        "limit": 10,
        "offset": 20,
        "results": results,
    })
    .to_string();
    let repo = FakeRepo {
        search_calls: RefCell::new(0),
        recordings_calls: RefCell::new(0),
    };
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "blue monday",
        Some(10),
        Some(20),
    ));
    assert_hit(
        &resp,
        &seed,
        SEARCH_TTL,
        *repo.search_calls.borrow(),
        &cache,
    );
}

#[test]
fn cached_search_with_exactly_default_page_results_is_a_hit() {
    // The default page (`limit: None` → 25) cached full is also a hit.
    let results: Vec<serde_json::Value> = (0..DEFAULT_PAGE_SIZE).map(artist_hit).collect();
    let seed = serde_json::json!({
        "q": "blue monday",
        "type": "artist",
        "limit": DEFAULT_PAGE_SIZE,
        "offset": 0,
        "results": results,
    })
    .to_string();
    let repo = FakeRepo {
        search_calls: RefCell::new(0),
        recordings_calls: RefCell::new(0),
    };
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "blue monday",
        None,
        None,
    ));
    assert_hit(
        &resp,
        &seed,
        SEARCH_TTL,
        *repo.search_calls.borrow(),
        &cache,
    );
}

#[test]
fn cached_recordings_with_exactly_row_cap_is_a_hit() {
    // SQL caps at exactly 1000 rows and the handler truncates to 1000, so
    // a 1000-row cached body is the normal large-release shape and a hit.
    let rows: Vec<serde_json::Value> = (0..RECORDINGS_CAP).map(recording_row).collect();
    let seed = serde_json::json!({
        "release_mbid": RELEASE_MBID,
        "recordings": rows,
    })
    .to_string();
    let repo = FakeRepo {
        search_calls: RefCell::new(0),
        recordings_calls: RefCell::new(0),
    };
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_release_recordings(&repo, &cache, RELEASE_MBID));
    assert_hit(
        &resp,
        &seed,
        RECORDINGS_TTL,
        *repo.recordings_calls.borrow(),
        &cache,
    );
}

#[test]
fn cached_recordings_with_non_uuid_row_release_mbid_is_a_miss() {
    // Per-row `release_mbid`, when present and non-null, must be a
    // UUID-shaped string; a non-UUID value is a miss recovered from the
    // repo through the existing success-only `put`.
    let mut bad_row = recording_row(0);
    bad_row["release_mbid"] = serde_json::json!("not-a-uuid");
    let seed = serde_json::json!({
        "release_mbid": RELEASE_MBID,
        "recordings": [bad_row],
    })
    .to_string();
    let repo = FakeRepo {
        search_calls: RefCell::new(0),
        recordings_calls: RefCell::new(0),
    };
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_release_recordings(&repo, &cache, RELEASE_MBID));
    assert_eq!(resp.status, 200);
    assert_ne!(resp.body, seed, "corrupt cached body must not be served");
    assert_eq!(resp.cache_ttl, Some(RECORDINGS_TTL));
    assert_eq!(
        *repo.recordings_calls.borrow(),
        1,
        "miss must read the repo once"
    );
    let puts = cache.puts.borrow();
    assert_eq!(puts.len(), 1, "healthy success replaces the corrupt body");
    assert_eq!(puts[0].1, resp.body, "put body is the fresh served body");
    assert_eq!(puts[0].2, RECORDINGS_TTL);
}
