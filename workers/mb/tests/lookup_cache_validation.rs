//! Treat malformed unpaged lookup cache payloads as misses.
//!
//! `handle_lookup` must minimally validate a cached body before serving
//! it as a cacheable 200. A valid hit is a JSON object in exactly the
//! shape a fresh lookup emits for the requested kind:
//!
//! - `kind` is the requested kind (`artist` / `release` / `recording`);
//! - `mbid` is the requested MBID (canonical UUID comparison,
//!   case-insensitive — the cache key and the repository read are
//!   lowercased, so case is not identity);
//! - every field the fresh body always carries is present and correctly
//!   typed: artist `name`/`sort_name` strings with `type`/`country`
//!   null-or-string; release `title` string with `artist_mbid` /
//!   `release_type` / `caa_url` null-or-string and `year`
//!   null-or-integer; recording `title` string with `release_mbid`
//!   null-or-string and `track_no` / `duration_ms` / `medium_no`
//!   null-or-integer.
//!
//! Anything else — truncated or non-JSON writes, non-object JSON,
//! wrong kind, wrong MBID, missing fields, wrongly typed fields — is a
//! cache *miss*: the repository is read (with the lowercased MBID), a
//! healthy row is served through the existing success-only cache path
//! so the poisoned entry is overwritten by exactly one `put`, and a
//! missing row (404) or repository failure (500) stays the existing
//! non-cacheable error without refreshing the corrupt success.
//!
//! Valid hits keep short-circuiting: null optional fields, extra
//! unknown fields, and uppercase UUID spellings (in the request or in
//! the cached body) are all still hits served verbatim, and the
//! repository is not touched.
//!
//! The fake cache returns its seeded body for any key (cache keys are
//! private), so these tests exercise payload validation, not keying.
//! Paged search / release-recordings validation is out of scope here.
//!
//! Visibility note for the builder: this target links the crate as an
//! external consumer, so `handle_lookup` and the `Artist` / `Release`
//! fields must be `pub` (the existing `Recording` / `SearchHit`
//! precedent: "`pub` for the `tests/` integration pin") without
//! changing any serialization. Making only that visibility change
//! against the baseline leaves every malformed case below red: the
//! handler serves the seeded body as 200 without reading the repo.
//!
//! Standalone integration target: no existing suite is edited, and no
//! production code is touched by this file.

use nntune_mb::{
    handle_lookup, ApiResponse, Artist, Entity, Kind, MbRepo, Recording, Release, RepoError,
    RepoStage, SearchHit, SuccessCache,
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

const MBID: &str = "123e4567-e89b-12d3-a456-426614174000";
const MBID_UPPER: &str = "123E4567-E89B-12D3-A456-426614174000";
const OTHER_MBID: &str = "123e4567-e89b-12d3-a456-426614179999";
const ARTIST_MBID: &str = "123e4567-e89b-12d3-a456-426614174001";
const RELEASE_MBID: &str = "123e4567-e89b-12d3-a456-426614174002";

/// Pinned lookup TTL from `workers/mb/src/lib.rs` (`LOOKUP_CACHE_TTL_S`).
/// Hardcoded on purpose: a TTL change is a separate slice and must turn
/// this file red first.
const LOOKUP_TTL: u32 = 86_400;

// --- Doubles --------------------------------------------------------------

#[derive(Clone)]
enum LookupOutcome {
    Found(Entity),
    Missing,
    Fail,
}

struct FakeRepo {
    outcome: LookupOutcome,
    lookup_calls: RefCell<u32>,
    last_lookup: RefCell<Option<(Kind, String)>>,
}

impl FakeRepo {
    fn found(entity: Entity) -> Self {
        Self {
            outcome: LookupOutcome::Found(entity),
            lookup_calls: RefCell::new(0),
            last_lookup: RefCell::new(None),
        }
    }

    fn missing() -> Self {
        Self {
            outcome: LookupOutcome::Missing,
            lookup_calls: RefCell::new(0),
            last_lookup: RefCell::new(None),
        }
    }

    fn failing() -> Self {
        Self {
            outcome: LookupOutcome::Fail,
            lookup_calls: RefCell::new(0),
            last_lookup: RefCell::new(None),
        }
    }
}

impl MbRepo for FakeRepo {
    async fn lookup(&self, kind: Kind, mbid: &str) -> Result<Option<Entity>, RepoError> {
        *self.lookup_calls.borrow_mut() += 1;
        *self.last_lookup.borrow_mut() = Some((kind, mbid.to_string()));
        match &self.outcome {
            LookupOutcome::Found(entity) => Ok(Some(entity.clone())),
            LookupOutcome::Missing => Ok(None),
            LookupOutcome::Fail => Err(RepoError::new(RepoStage::Query)),
        }
    }

    async fn search(
        &self,
        _kind: Kind,
        _normalized_q: &str,
        _limit: u32,
        _offset: u32,
    ) -> Result<Vec<SearchHit>, RepoError> {
        Ok(vec![])
    }

    async fn release_recordings(
        &self,
        _release_mbid: &str,
    ) -> Result<Option<Vec<Recording>>, RepoError> {
        Ok(None)
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

    fn empty() -> Self {
        Self {
            seed: None,
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

fn fresh_entity(kind: Kind) -> Entity {
    match kind {
        Kind::Artist => Entity::Artist(Artist {
            mbid: MBID.to_string(),
            name: "Fresh Artist".to_string(),
            sort_name: "Artist, Fresh".to_string(),
            artist_type: Some("Group".to_string()),
            country: Some("US".to_string()),
        }),
        Kind::Release => Entity::Release(Release {
            mbid: MBID.to_string(),
            artist_mbid: Some(ARTIST_MBID.to_string()),
            title: "Fresh Release".to_string(),
            release_type: Some("Album".to_string()),
            year: Some(1983),
            caa_url: None,
        }),
        Kind::Recording => Entity::Recording(Recording {
            mbid: MBID.to_string(),
            release_mbid: Some(RELEASE_MBID.to_string()),
            title: "Fresh Track".to_string(),
            track_no: Some(1),
            duration_ms: Some(180_000),
            medium_no: Some(1),
        }),
    }
}

fn fresh_label(kind: Kind) -> (&'static str, &'static str) {
    match kind {
        Kind::Artist => ("name", "Fresh Artist"),
        Kind::Release => ("title", "Fresh Release"),
        Kind::Recording => ("title", "Fresh Track"),
    }
}

fn cached_artist(mbid: &str) -> serde_json::Value {
    serde_json::json!({
        "mbid": mbid,
        "name": "Cached Artist",
        "sort_name": "Artist, Cached",
        "type": "Group",
        "country": "US",
        "kind": "artist",
    })
}

fn cached_release(mbid: &str) -> serde_json::Value {
    serde_json::json!({
        "mbid": mbid,
        "artist_mbid": ARTIST_MBID,
        "title": "Cached Release",
        "release_type": "Album",
        "year": 1983,
        "caa_url": null,
        "kind": "release",
    })
}

fn cached_recording(mbid: &str) -> serde_json::Value {
    serde_json::json!({
        "mbid": mbid,
        "release_mbid": RELEASE_MBID,
        "title": "Cached Track",
        "track_no": 1,
        "duration_ms": 180000,
        "medium_no": 1,
        "kind": "recording",
    })
}

fn valid_cached(kind: Kind) -> String {
    match kind {
        Kind::Artist => cached_artist(MBID).to_string(),
        Kind::Release => cached_release(MBID).to_string(),
        Kind::Recording => cached_recording(MBID).to_string(),
    }
}

/// Copy of a valid cached body with one field replaced.
fn with_field(kind: Kind, key: &str, value: serde_json::Value) -> String {
    let mut body: serde_json::Value =
        serde_json::from_str(&valid_cached(kind)).expect("valid fixture parses");
    body[key] = value;
    body.to_string()
}

/// Copy of a valid cached body with one field removed.
fn without_field(kind: Kind, key: &str) -> String {
    let mut body: serde_json::Value =
        serde_json::from_str(&valid_cached(kind)).expect("valid fixture parses");
    body.as_object_mut()
        .expect("fixture is an object")
        .remove(key);
    body.to_string()
}

fn parse_body(resp: &ApiResponse) -> serde_json::Value {
    serde_json::from_str(&resp.body).expect("response body is JSON")
}

fn run_lookup(kind: Kind, mbid: &str, repo: &FakeRepo, cache: &SeededCache) -> ApiResponse {
    block_on(handle_lookup(repo, cache, kind, mbid))
}

// --- Assertion helpers ----------------------------------------------------

/// A cached body served as a hit must come back verbatim, cacheable,
/// with no repository read and no cache rewrite.
fn assert_hit(resp: &ApiResponse, seed: &str, repo: &FakeRepo, cache: &SeededCache) {
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, seed, "valid cached body is served verbatim");
    assert_eq!(resp.cache_ttl, Some(LOOKUP_TTL));
    assert_eq!(*cache.gets.borrow(), 1);
    assert_eq!(
        *repo.lookup_calls.borrow(),
        0,
        "valid hit must not read repo"
    );
    assert_eq!(
        cache.puts.borrow().len(),
        0,
        "valid hit must not rewrite cache"
    );
}

/// A rejected cached body is a miss: the fresh repository row is served
/// instead and replaces the corrupt body via one success-only `put`.
fn assert_miss_recovered(
    resp: &ApiResponse,
    seed: &str,
    kind: Kind,
    repo: &FakeRepo,
    cache: &SeededCache,
) {
    assert_eq!(resp.status, 200);
    assert_ne!(resp.body, seed, "corrupt cached body must not be served");
    assert_eq!(resp.cache_ttl, Some(LOOKUP_TTL));
    assert_eq!(*cache.gets.borrow(), 1);
    assert_eq!(
        *repo.lookup_calls.borrow(),
        1,
        "miss must read the repo once"
    );
    assert_eq!(
        *repo.last_lookup.borrow(),
        Some((kind, MBID.to_string())),
        "repo read uses the requested kind and lowercased MBID"
    );
    let puts = cache.puts.borrow();
    assert_eq!(puts.len(), 1, "healthy success replaces the corrupt body");
    assert_eq!(puts[0].1, resp.body, "put body is the fresh served body");
    assert_eq!(puts[0].2, LOOKUP_TTL);
    drop(puts);
    let body = parse_body(resp);
    assert_eq!(body["kind"], kind.as_str());
    assert_eq!(body["mbid"], MBID);
    let (label_key, label) = fresh_label(kind);
    assert_eq!(body[label_key], label, "fresh row is served, not the seed");
}

/// Every seed in `seeds` must be treated as a lookup cache miss for a
/// request of `kind` / `MBID` whose repository holds the fresh row.
fn assert_seeds_are_misses(kind: Kind, seeds: Vec<(&str, String)>) {
    for (label, seed) in seeds {
        let repo = FakeRepo::found(fresh_entity(kind));
        let cache = SeededCache::seeded(seed.clone());
        let resp = run_lookup(kind, MBID, &repo, &cache);
        assert_miss_recovered(&resp, &seed, kind, &repo, &cache);
        let _ = label;
    }
}

// --- Valid hits (controls) ------------------------------------------------

#[test]
fn valid_cached_artist_short_circuits_repo() {
    let seed = valid_cached(Kind::Artist);
    let repo = FakeRepo::found(fresh_entity(Kind::Artist));
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Artist, MBID, &repo, &cache);
    assert_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn valid_cached_release_short_circuits_repo() {
    let seed = valid_cached(Kind::Release);
    let repo = FakeRepo::found(fresh_entity(Kind::Release));
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Release, MBID, &repo, &cache);
    assert_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn valid_cached_recording_short_circuits_repo() {
    let seed = valid_cached(Kind::Recording);
    let repo = FakeRepo::found(fresh_entity(Kind::Recording));
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Recording, MBID, &repo, &cache);
    assert_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn valid_cached_artist_with_null_optionals_is_a_hit() {
    let mut seed = with_field(Kind::Artist, "type", serde_json::Value::Null);
    seed = {
        let mut body: serde_json::Value = serde_json::from_str(&seed).unwrap();
        body["country"] = serde_json::Value::Null;
        body.to_string()
    };
    let repo = FakeRepo::found(fresh_entity(Kind::Artist));
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Artist, MBID, &repo, &cache);
    assert_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn valid_cached_release_with_null_optionals_is_a_hit() {
    let seed = serde_json::json!({
        "mbid": MBID,
        "artist_mbid": null,
        "title": "Cached Release",
        "release_type": null,
        "year": null,
        "caa_url": null,
        "kind": "release",
    })
    .to_string();
    let repo = FakeRepo::found(fresh_entity(Kind::Release));
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Release, MBID, &repo, &cache);
    assert_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn valid_cached_recording_with_null_optionals_is_a_hit() {
    let seed = serde_json::json!({
        "mbid": MBID,
        "release_mbid": null,
        "title": "Cached Track",
        "track_no": null,
        "duration_ms": null,
        "medium_no": null,
        "kind": "recording",
    })
    .to_string();
    let repo = FakeRepo::found(fresh_entity(Kind::Recording));
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Recording, MBID, &repo, &cache);
    assert_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn valid_cached_lookup_with_extra_unknown_fields_is_a_hit() {
    let seed = with_field(
        Kind::Artist,
        "future_field",
        serde_json::json!({"nested": [1, 2, 3]}),
    );
    let repo = FakeRepo::found(fresh_entity(Kind::Artist));
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Artist, MBID, &repo, &cache);
    assert_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn valid_cached_lookup_with_uppercase_cached_mbid_is_a_hit() {
    let seed = cached_artist(MBID_UPPER).to_string();
    let repo = FakeRepo::found(fresh_entity(Kind::Artist));
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Artist, MBID, &repo, &cache);
    assert_hit(&resp, &seed, &repo, &cache);
}

#[test]
fn valid_cached_lookup_with_uppercase_request_mbid_is_a_hit() {
    let seed = valid_cached(Kind::Release);
    let repo = FakeRepo::found(fresh_entity(Kind::Release));
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Release, MBID_UPPER, &repo, &cache);
    assert_hit(&resp, &seed, &repo, &cache);
}

// --- Malformed bodies are misses ------------------------------------------

#[test]
fn truncated_or_non_json_cached_artist_is_a_miss() {
    assert_seeds_are_misses(
        Kind::Artist,
        vec![
            (
                "truncated mid-string",
                "{\"kind\":\"artist\",\"mbid\":\"123e4567-e8".to_string(),
            ),
            ("truncated mid-object", "{\"kind\":\"artist\"".to_string()),
            ("not json", "not json".to_string()),
            ("empty body", String::new()),
            ("whitespace only", "  \n ".to_string()),
            ("json array", "[]".to_string()),
            ("json string", "\"artist\"".to_string()),
            ("json number", "200".to_string()),
            ("json null", "null".to_string()),
            ("empty object", "{}".to_string()),
        ],
    );
}

#[test]
fn wrong_identity_cached_artist_is_a_miss() {
    assert_seeds_are_misses(
        Kind::Artist,
        vec![
            ("wrong kind: release body", cached_release(MBID).to_string()),
            (
                "wrong kind: recording body",
                cached_recording(MBID).to_string(),
            ),
            ("wrong mbid", cached_artist(OTHER_MBID).to_string()),
            (
                "mbid not a uuid",
                with_field(Kind::Artist, "mbid", serde_json::json!("hello")),
            ),
            ("missing kind", without_field(Kind::Artist, "kind")),
            ("missing mbid", without_field(Kind::Artist, "mbid")),
            (
                "kind wrong type",
                with_field(Kind::Artist, "kind", serde_json::json!(1)),
            ),
            (
                "mbid wrong type",
                with_field(Kind::Artist, "mbid", serde_json::json!(42)),
            ),
        ],
    );
}

#[test]
fn wrongly_shaped_cached_artist_is_a_miss() {
    assert_seeds_are_misses(
        Kind::Artist,
        vec![
            ("missing name", without_field(Kind::Artist, "name")),
            (
                "name wrong type",
                with_field(Kind::Artist, "name", serde_json::json!(7)),
            ),
            (
                "missing sort_name",
                without_field(Kind::Artist, "sort_name"),
            ),
            (
                "sort_name null",
                with_field(Kind::Artist, "sort_name", serde_json::Value::Null),
            ),
            ("missing type", without_field(Kind::Artist, "type")),
            (
                "type wrong type",
                with_field(Kind::Artist, "type", serde_json::json!(true)),
            ),
            ("missing country", without_field(Kind::Artist, "country")),
            (
                "country wrong type",
                with_field(Kind::Artist, "country", serde_json::json!(["US"])),
            ),
        ],
    );
}

#[test]
fn malformed_cached_release_is_a_miss() {
    assert_seeds_are_misses(
        Kind::Release,
        vec![
            ("truncated", "{\"kind\":\"release\",\"mbid\":".to_string()),
            ("wrong kind: artist body", cached_artist(MBID).to_string()),
            ("wrong mbid", cached_release(OTHER_MBID).to_string()),
            ("missing kind", without_field(Kind::Release, "kind")),
            ("missing mbid", without_field(Kind::Release, "mbid")),
            ("missing title", without_field(Kind::Release, "title")),
            (
                "title wrong type",
                with_field(Kind::Release, "title", serde_json::json!(9)),
            ),
            (
                "artist_mbid wrong type",
                with_field(Kind::Release, "artist_mbid", serde_json::json!(3)),
            ),
            (
                "release_type wrong type",
                with_field(Kind::Release, "release_type", serde_json::json!(1.5)),
            ),
            (
                "year as string",
                with_field(Kind::Release, "year", serde_json::json!("1983")),
            ),
            (
                "year as float",
                with_field(Kind::Release, "year", serde_json::json!(1983.5)),
            ),
            ("missing year", without_field(Kind::Release, "year")),
            (
                "caa_url wrong type",
                with_field(Kind::Release, "caa_url", serde_json::json!(false)),
            ),
        ],
    );
}

#[test]
fn malformed_cached_recording_is_a_miss() {
    assert_seeds_are_misses(
        Kind::Recording,
        vec![
            ("truncated", "{\"mbid\":\"123e4567".to_string()),
            ("wrong kind: release body", cached_release(MBID).to_string()),
            ("wrong mbid", cached_recording(OTHER_MBID).to_string()),
            ("missing kind", without_field(Kind::Recording, "kind")),
            ("missing mbid", without_field(Kind::Recording, "mbid")),
            ("missing title", without_field(Kind::Recording, "title")),
            (
                "title wrong type",
                with_field(Kind::Recording, "title", serde_json::Value::Null),
            ),
            (
                "release_mbid wrong type",
                with_field(Kind::Recording, "release_mbid", serde_json::json!(5)),
            ),
            (
                "track_no as string",
                with_field(Kind::Recording, "track_no", serde_json::json!("1")),
            ),
            (
                "duration_ms as float",
                with_field(Kind::Recording, "duration_ms", serde_json::json!(1.5)),
            ),
            (
                "medium_no wrong type",
                with_field(Kind::Recording, "medium_no", serde_json::json!(true)),
            ),
            (
                "missing track_no",
                without_field(Kind::Recording, "track_no"),
            ),
        ],
    );
}

#[test]
fn poisoned_cache_with_uppercase_request_recovers_via_lowercased_repo_read() {
    let seed = "{\"kind\":\"artist\",\"cached\":true}".to_string();
    let repo = FakeRepo::found(fresh_entity(Kind::Artist));
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Artist, MBID_UPPER, &repo, &cache);
    assert_miss_recovered(&resp, &seed, Kind::Artist, &repo, &cache);
}

// --- Poisoned entry + DB miss / failure stays non-cacheable ---------------

#[test]
fn poisoned_cache_plus_missing_row_is_404_without_put() {
    let seed = "{\"kind\":\"artist\",\"mbid\":\"123e4567-e8".to_string();
    let repo = FakeRepo::missing();
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Artist, MBID, &repo, &cache);
    assert_eq!(resp.status, 404);
    assert_ne!(resp.body, seed, "corrupt cached body must not be served");
    assert_eq!(resp.cache_ttl, None);
    assert_eq!(parse_body(&resp)["code"], "not_found");
    assert_eq!(*repo.lookup_calls.borrow(), 1);
    assert_eq!(
        cache.puts.borrow().len(),
        0,
        "404 must not refresh the corrupt cached success"
    );
}

#[test]
fn poisoned_wrong_kind_cache_plus_missing_row_is_404_without_put() {
    let seed = cached_artist(MBID).to_string();
    let repo = FakeRepo::missing();
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Release, MBID, &repo, &cache);
    assert_eq!(resp.status, 404);
    assert_ne!(resp.body, seed, "wrong-kind cached body must not be served");
    assert_eq!(resp.cache_ttl, None);
    assert_eq!(*repo.lookup_calls.borrow(), 1);
    assert_eq!(cache.puts.borrow().len(), 0);
}

#[test]
fn poisoned_cache_plus_repo_failure_is_500_without_put() {
    let seed = cached_recording(OTHER_MBID).to_string();
    let repo = FakeRepo::failing();
    let cache = SeededCache::seeded(seed.clone());
    let resp = run_lookup(Kind::Recording, MBID, &repo, &cache);
    assert_eq!(resp.status, 500);
    assert_ne!(resp.body, seed, "corrupt cached body must not be served");
    assert_eq!(resp.cache_ttl, None);
    assert_eq!(
        resp.body,
        r#"{"code":"internal_error","message":"unexpected error","retryable":true}"#
    );
    assert_eq!(*repo.lookup_calls.borrow(), 1);
    assert_eq!(
        cache.puts.borrow().len(),
        0,
        "500 must not refresh the corrupt cached success"
    );
}

// --- Uncached baseline behavior (controls) --------------------------------

#[test]
fn cache_miss_reads_repo_and_caches_fresh_success() {
    let repo = FakeRepo::found(fresh_entity(Kind::Artist));
    let cache = SeededCache::empty();
    let resp = run_lookup(Kind::Artist, MBID, &repo, &cache);
    assert_eq!(resp.status, 200);
    assert_eq!(resp.cache_ttl, Some(LOOKUP_TTL));
    assert_eq!(*cache.gets.borrow(), 1);
    assert_eq!(*repo.lookup_calls.borrow(), 1);
    let puts = cache.puts.borrow();
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0].1, resp.body);
    assert_eq!(puts[0].2, LOOKUP_TTL);
    drop(puts);
    assert_eq!(parse_body(&resp)["name"], "Fresh Artist");
}

#[test]
fn uncached_missing_row_is_404_without_put() {
    let repo = FakeRepo::missing();
    let cache = SeededCache::empty();
    let resp = run_lookup(Kind::Release, MBID, &repo, &cache);
    assert_eq!(resp.status, 404);
    assert_eq!(resp.cache_ttl, None);
    assert_eq!(*repo.lookup_calls.borrow(), 1);
    assert_eq!(cache.puts.borrow().len(), 0);
}

#[test]
fn uncached_repo_failure_is_500_without_put() {
    let repo = FakeRepo::failing();
    let cache = SeededCache::empty();
    let resp = run_lookup(Kind::Recording, MBID, &repo, &cache);
    assert_eq!(resp.status, 500);
    assert_eq!(resp.cache_ttl, None);
    assert_eq!(*repo.lookup_calls.borrow(), 1);
    assert_eq!(cache.puts.borrow().len(), 0);
}

#[test]
fn invalid_request_mbid_is_400_before_cache_or_repo() {
    let repo = FakeRepo::found(fresh_entity(Kind::Artist));
    let cache = SeededCache::seeded(valid_cached(Kind::Artist));
    let resp = run_lookup(Kind::Artist, "nope", &repo, &cache);
    assert_eq!(resp.status, 400);
    assert_eq!(resp.cache_ttl, None);
    assert_eq!(
        *cache.gets.borrow(),
        0,
        "invalid MBID is rejected before any cache read"
    );
    assert_eq!(*repo.lookup_calls.borrow(), 0);
    assert_eq!(cache.puts.borrow().len(), 0);
}
