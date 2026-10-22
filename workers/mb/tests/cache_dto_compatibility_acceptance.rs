//! Protected behavioral acceptance for cached/fresh MB DTO compatibility.
//!
//! The Builder must not edit this file. These tests extend the shipped cache
//! validation suites, not replace them. No provider, SQL, KV or service access.
//!
//! Breaks caught: accepting integers outside the fresh signed DTO width;
//! validating only the first recordings/search row; accepting non-string
//! release-search artist fields; adding positivity/required-optional rules;
//! serving rejected cache bodies instead of the repo result; caching errors;
//! changing canonical keys, effective coordinates, success TTLs or hit behavior.
//! Documentation of this contract in the existing MB README belongs to the
//! paired implementation, not this tests-only handoff.

use nntune_mb::{
    handle_lookup, handle_release_recordings, handle_search_paged, ApiResponse, Entity, Kind,
    MbRepo, Recording, Release, RepoError, RepoStage, SearchHit, SuccessCache,
};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::future::Future;
use std::task::{Context, Poll, Waker};

const MBID: &str = "123e4567-e89b-12d3-a456-426614174000";
const UPPER_MBID: &str = "123E4567-E89B-12D3-A456-426614174000";
const ROW_MBID: &str = "123e4567-e89b-12d3-a456-426614174001";
const UPPER_ROW_MBID: &str = "123E4567-E89B-12D3-A456-426614174001";

fn block_on<F: Future>(future: F) -> F::Output {
    let mut cx = Context::from_waker(Waker::noop());
    let mut future = Box::pin(future);
    for _ in 0..100 {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return result;
        }
    }
    panic!("immediate fake did not complete");
}

#[derive(Clone, Copy, Debug)]
enum Route {
    Release,
    Recording,
    Recordings,
    Search(Kind),
}

impl Route {
    fn key(self) -> String {
        match self {
            Self::Release => format!("mb:lookup:release:{MBID}"),
            Self::Recording => format!("mb:lookup:recording:{MBID}"),
            Self::Recordings => format!("mb:release-recordings:{MBID}"),
            Self::Search(kind) => format!("mb:search:{}:blue monday:25:20", kind.as_str()),
        }
    }

    fn ttl(self) -> u32 {
        match self {
            Self::Search(_) => 300,
            _ => 86_400,
        }
    }

    fn call(self) -> Call {
        match self {
            Self::Release => Call::Lookup(Kind::Release, MBID.to_owned()),
            Self::Recording => Call::Lookup(Kind::Recording, MBID.to_owned()),
            Self::Recordings => Call::Recordings(MBID.to_owned()),
            Self::Search(kind) => Call::Search(kind, "blue monday".to_owned(), 25, 20),
        }
    }

    fn run(self, repo: &FakeRepo, cache: &MemoryCache) -> ApiResponse {
        match self {
            Self::Release => block_on(handle_lookup(repo, cache, Kind::Release, UPPER_MBID)),
            Self::Recording => block_on(handle_lookup(repo, cache, Kind::Recording, UPPER_MBID)),
            Self::Recordings => block_on(handle_release_recordings(repo, cache, UPPER_MBID)),
            Self::Search(kind) => block_on(handle_search_paged(
                repo,
                cache,
                Some(kind.as_str().to_owned()),
                "  Blue   MONDAY  ",
                Some(1000),
                Some(20),
            )),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Outcome {
    Success,
    Missing,
    Error,
}

#[derive(Debug, PartialEq)]
enum Call {
    Lookup(Kind, String),
    Recordings(String),
    Search(Kind, String, u32, u32),
}

struct FakeRepo {
    outcome: Outcome,
    calls: RefCell<Vec<Call>>,
}

impl FakeRepo {
    fn new(outcome: Outcome) -> Self {
        Self {
            outcome,
            calls: RefCell::new(vec![]),
        }
    }
}

fn fresh_recording(mbid: &str) -> Recording {
    Recording {
        mbid: mbid.to_owned(),
        release_mbid: Some(MBID.to_owned()),
        title: "Fresh Track".to_owned(),
        track_no: Some(-1),
        duration_ms: Some(0),
        medium_no: Some(-2),
    }
}

impl MbRepo for FakeRepo {
    async fn lookup(&self, kind: Kind, mbid: &str) -> Result<Option<Entity>, RepoError> {
        self.calls
            .borrow_mut()
            .push(Call::Lookup(kind, mbid.to_owned()));
        match self.outcome {
            Outcome::Error => Err(RepoError::new(RepoStage::Query)),
            Outcome::Missing => Ok(None),
            Outcome::Success => Ok(Some(match kind {
                Kind::Release => Entity::Release(Release {
                    mbid: MBID.to_owned(),
                    artist_mbid: Some(ROW_MBID.to_owned()),
                    title: "Fresh Release".to_owned(),
                    release_type: Some("Album".to_owned()),
                    year: Some(-1),
                    caa_url: None,
                }),
                Kind::Recording => Entity::Recording(fresh_recording(MBID)),
                Kind::Artist => panic!("unexpected artist lookup"),
            })),
        }
    }

    async fn release_recordings(&self, mbid: &str) -> Result<Option<Vec<Recording>>, RepoError> {
        self.calls
            .borrow_mut()
            .push(Call::Recordings(mbid.to_owned()));
        match self.outcome {
            Outcome::Error => Err(RepoError::new(RepoStage::Query)),
            Outcome::Missing => Ok(None),
            Outcome::Success => Ok(Some(vec![fresh_recording(ROW_MBID)])),
        }
    }

    async fn search(
        &self,
        kind: Kind,
        q: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>, RepoError> {
        self.calls
            .borrow_mut()
            .push(Call::Search(kind, q.to_owned(), limit, offset));
        match self.outcome {
            Outcome::Error => Err(RepoError::new(RepoStage::Query)),
            Outcome::Missing => Ok(vec![]),
            Outcome::Success => Ok(vec![SearchHit {
                mbid: ROW_MBID.to_owned(),
                label: "Fresh Release".to_owned(),
                score: 0.5,
                artist_mbid: Some(MBID.to_owned()),
                artist_name: Some("Fresh Artist".to_owned()),
            }]),
        }
    }
}

#[derive(Default)]
struct MemoryCache {
    body: RefCell<Option<String>>,
    gets: RefCell<Vec<String>>,
    puts: RefCell<Vec<(String, String, u32)>>,
}

impl MemoryCache {
    fn seeded(body: String) -> Self {
        Self {
            body: RefCell::new(Some(body)),
            ..Self::default()
        }
    }
}

impl SuccessCache for MemoryCache {
    async fn get(&self, key: &str) -> Option<String> {
        self.gets.borrow_mut().push(key.to_owned());
        self.body.borrow().clone()
    }

    async fn put(&self, key: &str, body: &str, ttl_s: u32) {
        self.puts
            .borrow_mut()
            .push((key.to_owned(), body.to_owned(), ttl_s));
        *self.body.borrow_mut() = Some(body.to_owned());
    }
}

fn cached_recording(mbid: &str) -> Value {
    json!({
        "mbid": mbid, "release_mbid": MBID, "title": "Cached Track",
        "track_no": 1, "duration_ms": 180000, "medium_no": 1,
    })
}

fn seed(route: Route) -> Value {
    match route {
        Route::Release => json!({
            "kind": "release", "mbid": UPPER_MBID, "title": "Cached Release",
            "artist_mbid": null, "release_type": null, "year": 1983, "caa_url": null,
        }),
        Route::Recording => {
            let mut row = cached_recording(UPPER_MBID);
            row["kind"] = json!("recording");
            row
        }
        Route::Recordings => json!({
            "release_mbid": UPPER_MBID,
            // Poison the SECOND row: validating only the first is insufficient.
            "recordings": [cached_recording(MBID), cached_recording(UPPER_ROW_MBID)],
        }),
        Route::Search(kind) => {
            let label = if kind == Kind::Artist {
                "name"
            } else {
                "title"
            };
            let mut hit = json!({"mbid": UPPER_ROW_MBID, "score": 0.5});
            hit[label] = json!("Cached Label");
            if kind == Kind::Release {
                hit["artist_mbid"] = json!(MBID);
                hit["artist_name"] = json!("Cached Artist");
            }
            json!({
                "q": "blue monday", "type": kind.as_str(), "limit": 25, "offset": 20,
                "results": [hit.clone(), hit],
            })
        }
    }
}

fn target(body: &mut Value, route: Route) -> &mut Value {
    match route {
        Route::Recordings => &mut body["recordings"][1],
        Route::Search(_) => &mut body["results"][1],
        _ => body,
    }
}

fn changed(route: Route, field: &str, value: Value) -> String {
    let mut body = seed(route);
    target(&mut body, route)[field] = value;
    body.to_string()
}

fn expected_success(route: Route, outcome: Outcome) -> Value {
    match route {
        Route::Release => json!({
            "kind": "release", "mbid": MBID, "title": "Fresh Release",
            "artist_mbid": ROW_MBID, "release_type": "Album", "year": -1, "caa_url": null,
        }),
        Route::Recording => json!({
            "kind": "recording", "mbid": MBID, "title": "Fresh Track",
            "release_mbid": MBID, "track_no": -1, "duration_ms": 0, "medium_no": -2,
        }),
        Route::Recordings => json!({
            "release_mbid": MBID, "recordings": [{
                "mbid": ROW_MBID, "title": "Fresh Track", "release_mbid": MBID,
                "track_no": -1, "duration_ms": 0, "medium_no": -2,
            }],
        }),
        Route::Search(Kind::Release) => json!({
            "q": "blue monday", "type": "release", "limit": 25, "offset": 20,
            "results": if matches!(outcome, Outcome::Missing) { json!([]) } else { json!([{
                "mbid": ROW_MBID, "title": "Fresh Release", "score": 0.5,
                "artist_mbid": MBID, "artist_name": "Fresh Artist",
            }]) },
        }),
        Route::Search(_) => panic!("fresh-search assertions are release-only"),
    }
}

fn assert_rejected(route: Route, body: String, outcome: Outcome) {
    let repo = FakeRepo::new(outcome);
    let cache = MemoryCache::seeded(body.clone());
    let response = route.run(&repo, &cache);
    let error_status = match (route, outcome) {
        (_, Outcome::Error) => Some(500),
        (Route::Search(_), Outcome::Missing) => None,
        (_, Outcome::Missing) => Some(404),
        (_, Outcome::Success) => None,
    };
    assert_eq!(
        response.status,
        error_status.unwrap_or(200),
        "{route:?}/{outcome:?}"
    );
    assert_ne!(
        response.body, body,
        "incompatible cached DTO must be a miss: {route:?}/{outcome:?}"
    );
    assert_eq!(
        *repo.calls.borrow(),
        vec![route.call()],
        "exact repo fallback"
    );
    assert_eq!(
        *cache.gets.borrow(),
        vec![route.key()],
        "canonical cache read key"
    );
    let parsed: Value = serde_json::from_str(&response.body).unwrap();
    if let Some(status) = error_status {
        assert_eq!(
            response.cache_ttl, None,
            "repo errors/missing entities are no-store"
        );
        assert!(
            cache.puts.borrow().is_empty(),
            "never refresh a failed fallback"
        );
        assert_eq!(
            parsed["code"],
            if status == 404 {
                "not_found"
            } else {
                "internal_error"
            }
        );
        assert_eq!(
            *cache.body.borrow(),
            Some(body),
            "error cannot replace success cache"
        );
    } else {
        assert_eq!(
            parsed,
            expected_success(route, outcome),
            "fresh DTO replaces poison"
        );
        assert_eq!(response.cache_ttl, Some(route.ttl()));
        assert_eq!(
            *cache.puts.borrow(),
            vec![(route.key(), response.body.clone(), route.ttl())]
        );
        // The replacement itself must be a hit, not rejected and refetched forever.
        assert_eq!(route.run(&repo, &cache), response);
        assert_eq!(*repo.calls.borrow(), vec![route.call()]);
        assert_eq!(cache.puts.borrow().len(), 1);
        assert_eq!(*cache.gets.borrow(), vec![route.key(), route.key()]);
    }
}

fn assert_hit(route: Route, mut body: Value) {
    body["future_envelope"] = json!({"kept": [1, true]});
    target(&mut body, route)["future_row"] = json!(["kept"]);
    let body = body.to_string();
    let repo = FakeRepo::new(Outcome::Error);
    let cache = MemoryCache::seeded(body.clone());
    let response = route.run(&repo, &cache);
    assert_eq!(response.status, 200);
    assert_eq!(
        response.body, body,
        "compatible cached DTO is served verbatim"
    );
    assert_eq!(response.cache_ttl, Some(route.ttl()));
    assert_eq!(*cache.gets.borrow(), vec![route.key()]);
    assert!(
        repo.calls.borrow().is_empty(),
        "valid hit must bypass even a failing repo"
    );
    assert!(cache.puts.borrow().is_empty(), "valid hit is not rewritten");
}

macro_rules! rejection_case {
    ($name:ident, $route:expr, $field:literal, $($value:tt)+) => {
        mod $name {
            use super::*;
            #[test]
            fn fresh_success_replaces_cache() {
                assert_rejected($route, changed($route, $field, json!($($value)+)), Outcome::Success);
            }
            #[test]
            fn missing_repo_result_uses_existing_semantics() {
                assert_rejected($route, changed($route, $field, json!($($value)+)), Outcome::Missing);
            }
            #[test]
            fn repo_error_is_not_cached() {
                assert_rejected($route, changed($route, $field, json!($($value)+)), Outcome::Error);
            }
        }
    };
}

macro_rules! numeric_contract {
    ($name:ident, $route:expr, $field:literal, $min:expr, $max:expr, $below:expr, $above:expr) => {
        mod $name {
            use super::*;
            rejection_case!(below_signed_min, $route, $field, $below);
            rejection_case!(above_signed_max, $route, $field, $above);
            rejection_case!(
                huge_unsigned,
                $route,
                $field,
                18_446_744_073_709_551_615_u64
            );
            rejection_case!(
                huge_signed_positive,
                $route,
                $field,
                9_223_372_036_854_775_807_i64
            );
            rejection_case!(
                huge_signed_negative,
                $route,
                $field,
                -9_223_372_036_854_775_808_i64
            );
            rejection_case!(fractional, $route, $field, 1.5);

            #[test]
            fn boundaries_negative_zero_and_null_remain_hits() {
                for value in [json!($min), json!($max), json!(-1), json!(0), Value::Null] {
                    let mut body = seed($route);
                    target(&mut body, $route)[$field] = value;
                    assert_hit($route, body);
                }
            }
        }
    };
}

numeric_contract!(
    lookup_year,
    Route::Release,
    "year",
    -32768,
    32767,
    -32769,
    32768
);
numeric_contract!(
    lookup_track_no,
    Route::Recording,
    "track_no",
    -32768,
    32767,
    -32769,
    32768
);
numeric_contract!(
    lookup_medium_no,
    Route::Recording,
    "medium_no",
    -32768,
    32767,
    -32769,
    32768
);
numeric_contract!(
    lookup_duration_ms,
    Route::Recording,
    "duration_ms",
    -2_147_483_648_i64,
    2_147_483_647,
    -2_147_483_649_i64,
    2_147_483_648_i64
);
numeric_contract!(
    list_track_no,
    Route::Recordings,
    "track_no",
    -32768,
    32767,
    -32769,
    32768
);
numeric_contract!(
    list_medium_no,
    Route::Recordings,
    "medium_no",
    -32768,
    32767,
    -32769,
    32768
);
numeric_contract!(
    list_duration_ms,
    Route::Recordings,
    "duration_ms",
    -2_147_483_648_i64,
    2_147_483_647,
    -2_147_483_649_i64,
    2_147_483_648_i64
);

mod release_search_artist_mbid {
    use super::*;
    rejection_case!(object, Route::Search(Kind::Release), "artist_mbid", {"id": MBID});
    rejection_case!(number, Route::Search(Kind::Release), "artist_mbid", 42);
    rejection_case!(boolean, Route::Search(Kind::Release), "artist_mbid", false);
    rejection_case!(array, Route::Search(Kind::Release), "artist_mbid", [MBID]);
}

mod release_search_artist_name {
    use super::*;
    rejection_case!(object, Route::Search(Kind::Release), "artist_name", {"name": "Artist"});
    rejection_case!(number, Route::Search(Kind::Release), "artist_name", 42);
    rejection_case!(boolean, Route::Search(Kind::Release), "artist_name", false);
    rejection_case!(
        array,
        Route::Search(Kind::Release),
        "artist_name",
        ["Artist"]
    );
}

#[test]
fn release_search_artist_optionals_allow_strings_null_absence_and_mixed_presence() {
    let route = Route::Search(Kind::Release);
    // Option<String> is a type contract, not UUID/business-policy validation.
    for mbid in [
        None,
        Some(Value::Null),
        Some(json!(UPPER_MBID)),
        Some(json!("")),
        Some(json!("artist-token")),
    ] {
        for name in [
            None,
            Some(Value::Null),
            Some(json!("Artist")),
            Some(json!("")),
        ] {
            let mut body = seed(route);
            // Exercise both rows; neither optional is required on any release hit.
            for hit in body["results"].as_array_mut().unwrap() {
                for (field, value) in [("artist_mbid", mbid.clone()), ("artist_name", name.clone())]
                {
                    match value {
                        Some(value) => hit[field] = value,
                        None => {
                            hit.as_object_mut().unwrap().remove(field);
                        }
                    }
                }
            }
            assert_hit(route, body);
        }
    }
}

#[test]
fn artist_and_recording_search_do_not_require_release_artist_fields() {
    for kind in [Kind::Artist, Kind::Recording] {
        let route = Route::Search(kind);
        assert_hit(route, seed(route));
    }
}

#[test]
fn recording_list_keeps_absent_and_null_optionals_compatible() {
    let route = Route::Recordings;
    for null in [false, true] {
        let mut body = seed(route);
        for row in body["recordings"].as_array_mut().unwrap() {
            for field in ["release_mbid", "track_no", "duration_ms", "medium_no"] {
                if null {
                    row[field] = Value::Null;
                } else {
                    row.as_object_mut().unwrap().remove(field);
                }
            }
        }
        assert_hit(route, body);
    }
}

#[test]
fn uncached_successes_still_use_existing_keys_coordinates_and_ttls() {
    for route in [
        Route::Release,
        Route::Recording,
        Route::Recordings,
        Route::Search(Kind::Release),
    ] {
        let repo = FakeRepo::new(Outcome::Success);
        let cache = MemoryCache::default();
        let response = route.run(&repo, &cache);
        assert_eq!(response.status, 200);
        assert_eq!(
            serde_json::from_str::<Value>(&response.body).unwrap(),
            expected_success(route, Outcome::Success)
        );
        assert_eq!(response.cache_ttl, Some(route.ttl()));
        assert_eq!(*repo.calls.borrow(), vec![route.call()]);
        assert_eq!(*cache.gets.borrow(), vec![route.key()]);
        assert_eq!(
            *cache.puts.borrow(),
            vec![(route.key(), response.body, route.ttl())]
        );
    }
}
