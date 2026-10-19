//! Treat malformed cached search / release-recordings payloads as misses.
//!
//! `handle_search_paged` and `handle_release_recordings` must minimally
//! validate a cached body before serving it as a cacheable 200:
//!
//! - search: JSON object whose `q` is the normalized query, whose `type`
//!   is the requested kind, whose `limit`/`offset` are the *effective*
//!   page coordinates, and whose `results` is a typed hit list no longer
//!   than the effective limit (artist hits carry a string `name`,
//!   release/recording hits a string `title`, every hit a UUID-shaped
//!   string `mbid` and a numeric `score`);
//! - recordings: JSON object whose `release_mbid` is the requested
//!   release (canonical UUID comparison, case-insensitive) and whose
//!   `recordings` is a typed recording list no longer than the existing
//!   1000-row cap (string UUID `mbid`, string `title`, optional fields
//!   null or correctly typed).
//!
//! Anything else — malformed JSON, wrong shape, wrong identity, wrong
//! page, over-cap — is a cache *miss*: the repository is read, a healthy
//! success replaces the corrupt body through the existing success-only
//! cache path, and a repository failure / missing release stays the
//! existing non-cacheable error without refreshing the corrupt success.
//!
//! Valid hits keep short-circuiting: empty result lists, extra unknown
//! fields, null optional fields and uppercase UUIDs are all still hits,
//! and the repository is not touched.
//!
//! The fake cache returns its seeded body for any key (cache keys are
//! private), so these tests exercise payload validation, not keying.
//! Unpaged entity lookup validation is out of scope here.
//!
//! Standalone integration target: no existing suite is edited, and no
//! production code is touched by this file.

use nntune_mb::{
    handle_release_recordings, handle_search_paged, ApiResponse, Entity, Kind, MbRepo, Recording,
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
const RELEASE_MBID_UPPER: &str = "123E4567-E89B-12D3-A456-426614174000";
const OTHER_MBID: &str = "123e4567-e89b-12d3-a456-426614179999";
const HIT_MBID: &str = "123e4567-e89b-12d3-a456-426614174001";
const HIT_MBID_UPPER: &str = "123E4567-E89B-12D3-A456-426614174001";

/// Pinned TTLs from `workers/mb/src/lib.rs` (`SEARCH_CACHE_TTL_S` /
/// `LOOKUP_CACHE_TTL_S`). Hardcoded on purpose: a TTL change is a
/// separate slice and must turn this file red first.
const SEARCH_TTL: u32 = 300;
const RECORDINGS_TTL: u32 = 86_400;
/// Pinned release-recordings row cap (`RELEASE_RECORDINGS_LIMIT`).
const RECORDINGS_CAP: usize = 1000;

// --- Doubles --------------------------------------------------------------

#[derive(Clone)]
enum SearchOutcome {
    Hits(Vec<SearchHit>),
    Fail,
}

#[derive(Clone)]
enum RecordingsOutcome {
    Some(Vec<Recording>),
    Missing,
    Fail,
}

struct FakeRepo {
    search_outcome: SearchOutcome,
    recordings_outcome: RecordingsOutcome,
    search_calls: RefCell<u32>,
    recordings_calls: RefCell<u32>,
    last_search: RefCell<Option<(Kind, String, u32, u32)>>,
    last_recordings_mbid: RefCell<Option<String>>,
}

impl FakeRepo {
    fn search_returning(hits: Vec<SearchHit>) -> Self {
        Self {
            search_outcome: SearchOutcome::Hits(hits),
            recordings_outcome: RecordingsOutcome::Missing,
            search_calls: RefCell::new(0),
            recordings_calls: RefCell::new(0),
            last_search: RefCell::new(None),
            last_recordings_mbid: RefCell::new(None),
        }
    }

    fn recordings_returning(outcome: RecordingsOutcome) -> Self {
        Self {
            search_outcome: SearchOutcome::Hits(vec![]),
            recordings_outcome: outcome,
            search_calls: RefCell::new(0),
            recordings_calls: RefCell::new(0),
            last_search: RefCell::new(None),
            last_recordings_mbid: RefCell::new(None),
        }
    }

    fn failing() -> Self {
        Self {
            search_outcome: SearchOutcome::Fail,
            recordings_outcome: RecordingsOutcome::Fail,
            search_calls: RefCell::new(0),
            recordings_calls: RefCell::new(0),
            last_search: RefCell::new(None),
            last_recordings_mbid: RefCell::new(None),
        }
    }
}

impl MbRepo for FakeRepo {
    async fn lookup(&self, _kind: Kind, _mbid: &str) -> Result<Option<Entity>, RepoError> {
        Ok(None)
    }

    async fn search(
        &self,
        kind: Kind,
        normalized_q: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>, RepoError> {
        *self.search_calls.borrow_mut() += 1;
        *self.last_search.borrow_mut() = Some((kind, normalized_q.to_string(), limit, offset));
        match &self.search_outcome {
            SearchOutcome::Hits(hits) => Ok(hits.clone()),
            SearchOutcome::Fail => Err(RepoError),
        }
    }

    async fn release_recordings(
        &self,
        release_mbid: &str,
    ) -> Result<Option<Vec<Recording>>, RepoError> {
        *self.recordings_calls.borrow_mut() += 1;
        *self.last_recordings_mbid.borrow_mut() = Some(release_mbid.to_string());
        match &self.recordings_outcome {
            RecordingsOutcome::Some(rows) => Ok(Some(rows.clone())),
            RecordingsOutcome::Missing => Ok(None),
            RecordingsOutcome::Fail => Err(RepoError),
        }
    }
}

/// Seeded cache: `get` returns the seed for any key, `put` is recorded.
struct SeededCache {
    seed: Option<String>,
    gets: RefCell<u32>,
    puts: RefCell<Vec<(String, String, u32)>>,
}

impl SeededCache {
    fn seeded(body: String) -> Self {
        Self {
            seed: Some(body),
            gets: RefCell::new(0),
            puts: RefCell::new(Vec::new()),
        }
    }
}

impl SuccessCache for SeededCache {
    async fn get(&self, _key: &str) -> Option<String> {
        *self.gets.borrow_mut() += 1;
        self.seed.clone()
    }

    async fn put(&self, key: &str, body: &str, ttl_s: u32) {
        self.puts
            .borrow_mut()
            .push((key.to_string(), body.to_string(), ttl_s));
    }
}

// --- Fixture builders -----------------------------------------------------

fn fresh_search_hits() -> Vec<SearchHit> {
    vec![SearchHit {
        mbid: HIT_MBID.to_string(),
        label: "Fresh Artist".to_string(),
        score: 0.9,
        artist_mbid: None,
        artist_name: None,
    }]
}

fn artist_hit_json() -> serde_json::Value {
    serde_json::json!({
        "mbid": HIT_MBID,
        "name": "Cached Artist",
        "score": 0.8,
    })
}

fn cached_search(
    q: &str,
    kind: &str,
    limit: u32,
    offset: u32,
    results: serde_json::Value,
) -> String {
    serde_json::json!({
        "q": q,
        "type": kind,
        "limit": limit,
        "offset": offset,
        "results": results,
    })
    .to_string()
}

fn valid_cached_search() -> String {
    cached_search(
        "blue monday",
        "artist",
        10,
        20,
        serde_json::json!([artist_hit_json()]),
    )
}

fn fresh_recordings() -> Vec<Recording> {
    vec![Recording {
        mbid: HIT_MBID.to_string(),
        release_mbid: Some(RELEASE_MBID.to_string()),
        title: "Fresh Track".to_string(),
        track_no: Some(1),
        duration_ms: Some(180_000),
        medium_no: Some(1),
    }]
}

fn recording_json() -> serde_json::Value {
    serde_json::json!({
        "mbid": HIT_MBID,
        "release_mbid": RELEASE_MBID,
        "title": "Cached Track",
        "track_no": 1,
        "duration_ms": 180000,
        "medium_no": 1,
    })
}

fn cached_recordings(release_mbid: &str, recordings: serde_json::Value) -> String {
    serde_json::json!({
        "release_mbid": release_mbid,
        "recordings": recordings,
    })
    .to_string()
}

fn valid_cached_recordings() -> String {
    cached_recordings(RELEASE_MBID, serde_json::json!([recording_json()]))
}

fn parse_body(resp: &ApiResponse) -> serde_json::Value {
    serde_json::from_str(&resp.body).expect("response body is JSON")
}

/// A cached body served as a hit must come back verbatim, cacheable,
/// with no repository read and no cache rewrite.
fn assert_search_hit(resp: &ApiResponse, seed: &str, repo: &FakeRepo, cache: &SeededCache) {
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, seed, "valid cached body is served verbatim");
    assert_eq!(resp.cache_ttl, Some(SEARCH_TTL));
    assert_eq!(*cache.gets.borrow(), 1);
    assert_eq!(
        *repo.search_calls.borrow(),
        0,
        "valid hit must not read repo"
    );
    assert_eq!(
        cache.puts.borrow().len(),
        0,
        "valid hit must not rewrite cache"
    );
}

fn assert_recordings_hit(resp: &ApiResponse, seed: &str, repo: &FakeRepo, cache: &SeededCache) {
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, seed, "valid cached body is served verbatim");
    assert_eq!(resp.cache_ttl, Some(RECORDINGS_TTL));
    assert_eq!(*cache.gets.borrow(), 1);
    assert_eq!(
        *repo.recordings_calls.borrow(),
        0,
        "valid hit must not read repo"
    );
    assert_eq!(
        cache.puts.borrow().len(),
        0,
        "valid hit must not rewrite cache"
    );
}

/// A rejected cached body is a miss: the repository answer is served
/// instead and replaces the corrupt body via one success-only `put`.
fn assert_search_miss_recovered(
    resp: &ApiResponse,
    seed: &str,
    repo: &FakeRepo,
    cache: &SeededCache,
) {
    assert_eq!(resp.status, 200);
    assert_ne!(resp.body, seed, "corrupt cached body must not be served");
    assert_eq!(resp.cache_ttl, Some(SEARCH_TTL));
    assert_eq!(
        *repo.search_calls.borrow(),
        1,
        "miss must read the repo once"
    );
    let puts = cache.puts.borrow();
    assert_eq!(puts.len(), 1, "healthy success replaces the corrupt body");
    assert_eq!(puts[0].1, resp.body, "put body is the fresh served body");
    assert_eq!(puts[0].2, SEARCH_TTL);
    let body = parse_body(resp);
    assert_eq!(body["results"][0]["name"], "Fresh Artist");
}

fn assert_recordings_miss_recovered(
    resp: &ApiResponse,
    seed: &str,
    repo: &FakeRepo,
    cache: &SeededCache,
) {
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
    let body = parse_body(resp);
    assert_eq!(body["release_mbid"], RELEASE_MBID);
    assert_eq!(body["recordings"][0]["title"], "Fresh Track");
}

fn run_search(seed: String, repo: &FakeRepo, cache: &SeededCache) -> ApiResponse {
    let _ = &seed;
    block_on(handle_search_paged(
        repo,
        cache,
        Some("artist".to_string()),
        "blue monday",
        Some(10),
        Some(20),
    ))
}

/// Every seed in `seeds` must be treated as a search cache miss for the
/// canonical request (artist / "blue monday" / limit 10 / offset 20).
fn assert_search_seeds_are_misses(seeds: Vec<(&str, String)>) {
    for (label, seed) in seeds {
        let repo = FakeRepo::search_returning(fresh_search_hits());
        let cache = SeededCache::seeded(seed.clone());
        let resp = run_search(seed.clone(), &repo, &cache);
        assert_eq!(*cache.gets.borrow(), 1, "{label}");
        assert_search_miss_recovered(&resp, &seed, &repo, &cache);
        // The repository still receives the effective request coordinates.
        assert_eq!(
            *repo.last_search.borrow(),
            Some((Kind::Artist, "blue monday".to_string(), 10, 20)),
            "{label}"
        );
    }
}

/// Every seed in `seeds` must be treated as a recordings cache miss for
/// `RELEASE_MBID`.
fn assert_recordings_seeds_are_misses(seeds: Vec<(&str, String)>) {
    for (label, seed) in seeds {
        let repo = FakeRepo::recordings_returning(RecordingsOutcome::Some(fresh_recordings()));
        let cache = SeededCache::seeded(seed.clone());
        let resp = block_on(handle_release_recordings(&repo, &cache, RELEASE_MBID));
        assert_eq!(*cache.gets.borrow(), 1, "{label}");
        assert_recordings_miss_recovered(&resp, &seed, &repo, &cache);
        assert_eq!(
            *repo.last_recordings_mbid.borrow(),
            Some(RELEASE_MBID.to_string()),
            "{label}"
        );
    }
}

// --- Search: valid hits (controls) ----------------------------------------

#[test]
fn valid_cached_search_short_circuits_repo() {
    let seed = valid_cached_search();
    let repo = FakeRepo::search_returning(fresh_search_hits());
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_search(seed.clone(), &repo, &cache);
    assert_search_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn valid_cached_search_with_empty_results_is_a_hit() {
    let seed = cached_search("blue monday", "artist", 10, 20, serde_json::json!([]));
    let repo = FakeRepo::search_returning(fresh_search_hits());
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_search(seed.clone(), &repo, &cache);
    assert_search_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn cached_search_matching_normalized_query_and_clamped_limit_is_a_hit() {
    // Raw query differs in case/whitespace and the raw limit is above the
    // page-size cap; identity is the normalized q + effective (clamped)
    // coordinates, so a body cached under those coordinates is valid.
    let seed = cached_search(
        "blue monday",
        "artist",
        25,
        0,
        serde_json::json!([artist_hit_json()]),
    );
    let repo = FakeRepo::search_returning(fresh_search_hits());
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "  Blue   MONDAY ",
        Some(1000),
        None,
    ));
    assert_search_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn cached_search_with_extra_fields_null_optionals_and_uppercase_mbid_is_a_hit() {
    // Release hits: extra unknown fields (top-level and per-hit) are
    // preserved, null artist optionals are fine, and an uppercase UUID
    // is still a canonical MBID.
    let seed = serde_json::json!({
        "q": "blue monday",
        "type": "release",
        "limit": 10,
        "offset": 0,
        "results": [{
            "mbid": HIT_MBID_UPPER,
            "title": "Cached Release",
            "score": 1,
            "artist_mbid": null,
            "artist_name": null,
            "future_field": {"nested": true},
        }],
        "future_top_level": [1, 2, 3],
    })
    .to_string();
    let repo = FakeRepo::search_returning(fresh_search_hits());
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("release".to_string()),
        "blue monday",
        Some(10),
        Some(0),
    ));
    assert_search_hit(&resp, &seed, &repo, &cache);
}

// --- Search: malformed / wrong-identity bodies are misses -----------------

#[test]
fn malformed_cached_search_json_is_a_miss() {
    assert_search_seeds_are_misses(vec![
        ("not json", "not json{".to_string()),
        ("empty body", String::new()),
        ("json array", "[]".to_string()),
        ("json string", "\"blue monday\"".to_string()),
        ("json null", "null".to_string()),
    ]);
}

#[test]
fn cached_search_with_wrong_identity_is_a_miss() {
    let hit = serde_json::json!([artist_hit_json()]);
    assert_search_seeds_are_misses(vec![
        (
            "wrong q",
            cached_search("other query", "artist", 10, 20, hit.clone()),
        ),
        (
            "unnormalized q",
            cached_search("Blue Monday", "artist", 10, 20, hit.clone()),
        ),
        (
            "wrong type",
            cached_search("blue monday", "release", 10, 20, hit.clone()),
        ),
        (
            "wrong limit",
            cached_search("blue monday", "artist", 25, 20, hit.clone()),
        ),
        (
            "wrong offset",
            cached_search("blue monday", "artist", 10, 0, hit.clone()),
        ),
        (
            "missing q",
            serde_json::json!({"type": "artist", "limit": 10, "offset": 20, "results": hit})
                .to_string(),
        ),
    ]);
}

#[test]
fn cached_search_with_invalid_schema_is_a_miss() {
    assert_search_seeds_are_misses(vec![
        (
            "limit as string",
            serde_json::json!({
                "q": "blue monday", "type": "artist",
                "limit": "10", "offset": 20,
                "results": [artist_hit_json()],
            })
            .to_string(),
        ),
        (
            "missing results",
            serde_json::json!({
                "q": "blue monday", "type": "artist", "limit": 10, "offset": 20,
            })
            .to_string(),
        ),
        (
            "results not an array",
            cached_search(
                "blue monday",
                "artist",
                10,
                20,
                serde_json::json!({"mbid": HIT_MBID}),
            ),
        ),
        (
            "result entry not an object",
            cached_search(
                "blue monday",
                "artist",
                10,
                20,
                serde_json::json!(["Cached Artist"]),
            ),
        ),
        (
            "result missing mbid",
            cached_search(
                "blue monday",
                "artist",
                10,
                20,
                serde_json::json!([{"name": "Cached Artist", "score": 0.8}]),
            ),
        ),
        (
            "result mbid not a uuid",
            cached_search(
                "blue monday",
                "artist",
                10,
                20,
                serde_json::json!([{"mbid": "not-a-uuid", "name": "Cached Artist", "score": 0.8}]),
            ),
        ),
        (
            "result missing label",
            cached_search(
                "blue monday",
                "artist",
                10,
                20,
                serde_json::json!([{"mbid": HIT_MBID, "score": 0.8}]),
            ),
        ),
        (
            "result score not numeric",
            cached_search(
                "blue monday",
                "artist",
                10,
                20,
                serde_json::json!([{"mbid": HIT_MBID, "name": "Cached Artist", "score": "high"}]),
            ),
        ),
    ]);
}

#[test]
fn cached_search_over_effective_limit_is_a_miss() {
    let results = serde_json::Value::Array((0..11).map(|_| artist_hit_json()).collect());
    assert_search_seeds_are_misses(vec![(
        "11 results cached for limit 10",
        cached_search("blue monday", "artist", 10, 20, results),
    )]);
}

#[test]
fn cached_release_search_hit_without_title_is_a_miss() {
    // Typed results are kind-aware: a release hit carries `title`, not
    // the artist label key `name`.
    let seed = cached_search(
        "blue monday",
        "release",
        10,
        0,
        serde_json::json!([{
            "mbid": HIT_MBID,
            "name": "Wrong Label Key",
            "score": 0.8,
            "artist_mbid": null,
            "artist_name": null,
        }]),
    );
    let repo = FakeRepo::search_returning(vec![SearchHit {
        mbid: HIT_MBID.to_string(),
        label: "Fresh Release".to_string(),
        score: 0.9,
        artist_mbid: None,
        artist_name: None,
    }]);
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("release".to_string()),
        "blue monday",
        Some(10),
        Some(0),
    ));
    assert_eq!(resp.status, 200);
    assert_ne!(resp.body, seed);
    assert_eq!(*repo.search_calls.borrow(), 1);
    assert_eq!(cache.puts.borrow().len(), 1);
    assert_eq!(parse_body(&resp)["results"][0]["title"], "Fresh Release");
}

#[test]
fn malformed_cached_search_with_repo_failure_is_uncached_500() {
    let seed = "not json{".to_string();
    let repo = FakeRepo::failing();
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_search(seed.clone(), &repo, &cache);
    assert_eq!(resp.status, 500);
    assert_ne!(
        resp.body, seed,
        "corrupt body must not be promoted to a success"
    );
    assert_eq!(resp.cache_ttl, None, "failure is never cacheable");
    assert_eq!(*repo.search_calls.borrow(), 1);
    assert_eq!(
        cache.puts.borrow().len(),
        0,
        "failure must not refresh the corrupt cached success"
    );
    let body = parse_body(&resp);
    assert!(body.get("results").is_none());
}

// --- Recordings: valid hits (controls) ------------------------------------

#[test]
fn valid_cached_recordings_short_circuit_repo() {
    let seed = valid_cached_recordings();
    let repo = FakeRepo::recordings_returning(RecordingsOutcome::Some(fresh_recordings()));
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_release_recordings(&repo, &cache, RELEASE_MBID));
    assert_recordings_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn valid_cached_recordings_with_empty_list_is_a_hit() {
    let seed = cached_recordings(RELEASE_MBID, serde_json::json!([]));
    let repo = FakeRepo::recordings_returning(RecordingsOutcome::Some(fresh_recordings()));
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_release_recordings(&repo, &cache, RELEASE_MBID));
    assert_recordings_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn cached_recordings_match_release_mbid_case_insensitively() {
    // Requested uppercase, cached lowercase (the stored canonical form).
    let seed = valid_cached_recordings();
    let repo = FakeRepo::recordings_returning(RecordingsOutcome::Some(fresh_recordings()));
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_release_recordings(&repo, &cache, RELEASE_MBID_UPPER));
    assert_recordings_hit(&resp, &seed, &repo, &cache);

    // Cached uppercase, requested lowercase: same canonical UUID.
    let seed = cached_recordings(RELEASE_MBID_UPPER, serde_json::json!([recording_json()]));
    let repo = FakeRepo::recordings_returning(RecordingsOutcome::Some(fresh_recordings()));
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_release_recordings(&repo, &cache, RELEASE_MBID));
    assert_recordings_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn cached_recordings_with_extra_fields_and_null_optionals_is_a_hit() {
    let seed = serde_json::json!({
        "release_mbid": RELEASE_MBID,
        "recordings": [{
            "mbid": HIT_MBID_UPPER,
            "release_mbid": null,
            "title": "Cached Track",
            "track_no": null,
            "duration_ms": null,
            "medium_no": null,
            "future_field": "kept",
        }],
        "future_top_level": true,
    })
    .to_string();
    let repo = FakeRepo::recordings_returning(RecordingsOutcome::Some(fresh_recordings()));
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_release_recordings(&repo, &cache, RELEASE_MBID));
    assert_recordings_hit(&resp, &seed, &repo, &cache);
}

// --- Recordings: malformed / wrong-identity bodies are misses -------------

#[test]
fn malformed_cached_recordings_json_is_a_miss() {
    assert_recordings_seeds_are_misses(vec![
        ("not json", "not json{".to_string()),
        ("empty body", String::new()),
        ("json array", "[]".to_string()),
        ("json null", "null".to_string()),
    ]);
}

#[test]
fn cached_recordings_with_wrong_identity_is_a_miss() {
    let rows = serde_json::json!([recording_json()]);
    assert_recordings_seeds_are_misses(vec![
        ("other release", cached_recordings(OTHER_MBID, rows.clone())),
        (
            "missing release_mbid",
            serde_json::json!({"recordings": rows}).to_string(),
        ),
        (
            "release_mbid not a string",
            serde_json::json!({"release_mbid": 42, "recordings": []}).to_string(),
        ),
    ]);
}

#[test]
fn cached_recordings_with_invalid_schema_is_a_miss() {
    assert_recordings_seeds_are_misses(vec![
        (
            "missing recordings",
            serde_json::json!({"release_mbid": RELEASE_MBID}).to_string(),
        ),
        (
            "recordings not an array",
            cached_recordings(RELEASE_MBID, serde_json::json!({"mbid": HIT_MBID})),
        ),
        (
            "recording entry not an object",
            cached_recordings(RELEASE_MBID, serde_json::json!(["Cached Track"])),
        ),
        (
            "recording missing mbid",
            cached_recordings(
                RELEASE_MBID,
                serde_json::json!([{
                    "release_mbid": RELEASE_MBID,
                    "title": "Cached Track",
                    "track_no": 1, "duration_ms": 1, "medium_no": 1,
                }]),
            ),
        ),
        (
            "recording mbid not a uuid",
            cached_recordings(
                RELEASE_MBID,
                serde_json::json!([{
                    "mbid": "not-a-uuid", "release_mbid": RELEASE_MBID,
                    "title": "Cached Track",
                    "track_no": 1, "duration_ms": 1, "medium_no": 1,
                }]),
            ),
        ),
        (
            "recording missing title",
            cached_recordings(
                RELEASE_MBID,
                serde_json::json!([{
                    "mbid": HIT_MBID, "release_mbid": RELEASE_MBID,
                    "track_no": 1, "duration_ms": 1, "medium_no": 1,
                }]),
            ),
        ),
        (
            "recording optional field wrong type",
            cached_recordings(
                RELEASE_MBID,
                serde_json::json!([{
                    "mbid": HIT_MBID, "release_mbid": RELEASE_MBID,
                    "title": "Cached Track",
                    "track_no": "one", "duration_ms": 1, "medium_no": 1,
                }]),
            ),
        ),
    ]);
}

#[test]
fn cached_recordings_over_row_cap_is_a_miss() {
    let rows = serde_json::Value::Array((0..=RECORDINGS_CAP).map(|_| recording_json()).collect());
    assert_recordings_seeds_are_misses(vec![(
        "1001 recordings cached over the 1000-row cap",
        cached_recordings(RELEASE_MBID, rows),
    )]);
}

#[test]
fn malformed_cached_recordings_with_missing_release_is_uncached_404() {
    let seed = "not json{".to_string();
    let repo = FakeRepo::recordings_returning(RecordingsOutcome::Missing);
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_release_recordings(&repo, &cache, RELEASE_MBID));
    assert_eq!(resp.status, 404);
    assert_ne!(
        resp.body, seed,
        "corrupt body must not be promoted to a success"
    );
    assert_eq!(resp.cache_ttl, None);
    assert_eq!(*repo.recordings_calls.borrow(), 1);
    assert_eq!(
        cache.puts.borrow().len(),
        0,
        "404 must not refresh the corrupt cached success"
    );
    assert_eq!(parse_body(&resp)["code"], "not_found");
}

#[test]
fn wrong_identity_cached_recordings_with_repo_failure_is_uncached_500() {
    let seed = cached_recordings(OTHER_MBID, serde_json::json!([recording_json()]));
    let repo = FakeRepo::failing();
    let cache = SeededCache::seeded(seed.clone());
    let resp = block_on(handle_release_recordings(&repo, &cache, RELEASE_MBID));
    assert_eq!(resp.status, 500);
    assert_ne!(resp.body, seed, "another release's body must not be served");
    assert_eq!(resp.cache_ttl, None);
    assert_eq!(*repo.recordings_calls.borrow(), 1);
    assert_eq!(
        cache.puts.borrow().len(),
        0,
        "failure must not refresh the corrupt cached success"
    );
}
