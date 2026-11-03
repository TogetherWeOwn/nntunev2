//! Contract acceptance for the W6b reads: release groups, artist links,
//! recording music-videos and recording ISRCs.
//!
//! What this file pins, beyond the in-module handler suite:
//!
//! - the five new [`MbRepo`] methods carry **defaults** (`Ok(None)`), so
//!   the existing fakes — including the protected cache-compatibility
//!   suite — keep compiling and read "missing" (404) without a repo call;
//! - the shared [`budgeted`] seam applies the production
//!   [`DB_OPERATION_DEADLINE_MS`] budget: a fast operation keeps its own
//!   result and stage, a hung operation fails as `Timeout`;
//! - the five routes are registered on the production router ahead of the
//!   catch-all, each dispatching to its handler;
//! - the listing envelopes, TTLs and canonical KV keys the W13 consumer
//!   depends on.
//!
//! Standalone integration target: no existing suite is edited.

use nntune_mb::{
    budgeted, handle_artist_urls, handle_group_releases, handle_recording_isrcs,
    handle_recording_videos, handle_release_group, ApiResponse, ArtistUrl, Entity, Kind, MbRepo,
    RecordingVideo, Release, ReleaseGroup, RepoError, RepoStage, SearchHit, SuccessCache,
    DB_OPERATION_DEADLINE_MS,
};
use std::cell::RefCell;
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

/// Minimal parking executor: polls until ready, parking the thread in
/// between so timer-backed futures (the deadline seam) can wake it.
struct Parker(std::thread::Thread);

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let parker = Arc::new(Parker(std::thread::current()));
    let waker = Waker::from(Arc::clone(&parker));
    let mut cx = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        std::thread::park();
    }
}

const MBID: &str = "123e4567-e89b-12d3-a456-426614174000";
const LIB_RS: &str = include_str!("../src/lib.rs");

/// Fake implementing ONLY the original three repository methods. It
/// compiles precisely because the W6b methods default, and every new
/// handler must read it as "missing" (404) without touching a W6b call.
struct BareRepo;

impl MbRepo for BareRepo {
    async fn lookup(&self, _kind: Kind, _mbid: &str) -> Result<Option<Entity>, RepoError> {
        Ok(None)
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
    ) -> Result<Option<Vec<nntune_mb::Recording>>, RepoError> {
        Ok(None)
    }
}

#[derive(Default)]
struct FakeCache {
    stored: RefCell<std::collections::HashMap<String, String>>,
    puts: RefCell<u32>,
}

impl SuccessCache for FakeCache {
    async fn get(&self, key: &str) -> Option<String> {
        self.stored.borrow().get(key).cloned()
    }

    async fn put(&self, key: &str, body: &str, _ttl_s: u32) {
        *self.puts.borrow_mut() += 1;
        self.stored
            .borrow_mut()
            .insert(key.to_string(), body.to_string());
    }
}

/// Full fake for the new surface: one found row per listing plus the
/// group, so envelope shapes, TTLs and keys are pinned end to end.
#[derive(Default)]
struct W6bRepo {
    fail: bool,
}

impl W6bRepo {
    fn group() -> ReleaseGroup {
        ReleaseGroup {
            mbid: MBID.to_string(),
            name: "Group".to_string(),
            artist_mbid: None,
            release_group_type: Some("Album".to_string()),
        }
    }
}

impl MbRepo for W6bRepo {
    async fn lookup(&self, _kind: Kind, _mbid: &str) -> Result<Option<Entity>, RepoError> {
        Ok(None)
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
    ) -> Result<Option<Vec<nntune_mb::Recording>>, RepoError> {
        Ok(None)
    }

    async fn release_group(&self, _mbid: &str) -> Result<Option<ReleaseGroup>, RepoError> {
        if self.fail {
            return Err(RepoError::new(RepoStage::Query));
        }
        Ok(Some(Self::group()))
    }

    async fn group_releases(&self, _group_mbid: &str) -> Result<Option<Vec<Release>>, RepoError> {
        if self.fail {
            return Err(RepoError::new(RepoStage::Query));
        }
        Ok(Some(vec![Release {
            mbid: MBID.to_string(),
            artist_mbid: None,
            title: "Album".to_string(),
            release_type: Some("Album".to_string()),
            year: Some(1990),
            caa_url: None,
        }]))
    }

    async fn artist_urls(&self, _artist_mbid: &str) -> Result<Option<Vec<ArtistUrl>>, RepoError> {
        if self.fail {
            return Err(RepoError::new(RepoStage::Query));
        }
        Ok(Some(vec![ArtistUrl {
            url_mbid: MBID.to_string(),
            url: "https://example.test/a".to_string(),
            link_name: "youtube".to_string(),
        }]))
    }

    async fn recording_videos(
        &self,
        _recording_mbid: &str,
    ) -> Result<Option<Vec<RecordingVideo>>, RepoError> {
        if self.fail {
            return Err(RepoError::new(RepoStage::Query));
        }
        Ok(Some(vec![RecordingVideo {
            recording_mbid: MBID.to_string(),
            url: "https://example.test/watch".to_string(),
            link_name: "free streaming".to_string(),
        }]))
    }

    async fn recording_isrcs(
        &self,
        _recording_mbid: &str,
    ) -> Result<Option<Vec<String>>, RepoError> {
        if self.fail {
            return Err(RepoError::new(RepoStage::Query));
        }
        Ok(Some(vec!["USABC2400001".to_string()]))
    }
}

fn parse_body(resp: &ApiResponse) -> serde_json::Value {
    serde_json::from_str(&resp.body).expect("success body is JSON")
}

/// New trait methods default to missing, so pre-W6b fakes read 404
/// without a W6b call and without caching the miss.
#[test]
fn trait_defaults_keep_old_fakes_reading_missing() {
    let repo = BareRepo;
    let cache = FakeCache::default();
    for resp in [
        block_on(handle_release_group(&repo, &cache, MBID)),
        block_on(handle_group_releases(&repo, &cache, MBID)),
        block_on(handle_artist_urls(&repo, &cache, MBID)),
        block_on(handle_recording_videos(&repo, &cache, MBID)),
        block_on(handle_recording_isrcs(&repo, &cache, MBID)),
    ] {
        assert_eq!(resp.status, 404);
        assert_eq!(resp.cache_ttl, None);
    }
    assert_eq!(*cache.puts.borrow(), 0);
}

/// A fast operation through the shared seam keeps its own value.
#[test]
fn budgeted_keeps_fast_success_and_stage() {
    let ok = block_on(budgeted(async { Ok::<u32, RepoError>(7) }));
    assert_eq!(ok, Ok(7));
    let err = block_on(budgeted(async {
        Err::<u32, RepoError>(RepoError::new(RepoStage::Query))
    }));
    assert_eq!(err, Err(RepoError::new(RepoStage::Query)));
}

/// A hung operation through the shared seam fails as Timeout at the
/// production budget — the same default the HTTP-layer repository feeds
/// every query.
#[test]
fn budgeted_times_out_a_hung_operation() {
    use std::time::{Duration, Instant};
    // The budget is the documented production default: finite, and at
    // least the 600 ms search SLO.
    assert_eq!(DB_OPERATION_DEADLINE_MS, 1_500);
    let started = Instant::now();
    let err = block_on(budgeted(async {
        std::future::pending::<Result<u32, RepoError>>().await
    }));
    assert_eq!(err, Err(RepoError::new(RepoStage::Timeout)));
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(DB_OPERATION_DEADLINE_MS),
        "the deadline must not fire early, fired after {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the deadline must bound the wait, took {elapsed:?}"
    );
}

/// The five routes exist on the production router ahead of the
/// catch-all, each dispatching to its handler.
#[test]
fn router_registers_the_w6b_routes_before_the_catchall() {
    let production = LIB_RS
        .split_once("#[cfg(test)]")
        .expect("test module must exist")
        .0;
    let router = production
        .split_once("worker::Router::new()")
        .expect("worker entrypoint must exist")
        .1;
    // Collapse whitespace: rustfmt may wrap long route registrations, and
    // the pin is about registration order, not line layout.
    let flat: String = router.split_whitespace().collect();
    let router = flat.as_str();
    let catchall = router
        .find(".or_else_any_method_async")
        .expect("router must retain its catch-all");
    for (path, handler) in [
        (
            "/v1/mb/release-group/:mbid",
            "http_layer::release_group_route",
        ),
        (
            "/v1/mb/release-group/:mbid/releases",
            "http_layer::group_releases_route",
        ),
        ("/v1/mb/artist/:mbid/urls", "http_layer::artist_urls_route"),
        (
            "/v1/mb/recording/:mbid/videos",
            "http_layer::recording_videos_route",
        ),
        (
            "/v1/mb/recording/:mbid/isrcs",
            "http_layer::recording_isrcs_route",
        ),
    ] {
        let route = router
            .find(&format!(".get_async(\"{path}\""))
            .unwrap_or_else(|| panic!("router must register GET {path}"));
        assert!(
            route < catchall,
            "GET {path} must be registered before the catch-all"
        );
        assert!(
            router[route..catchall].contains(handler),
            "GET {path} must dispatch to {handler}"
        );
    }
}

/// Envelopes, lookup TTL and canonical KV keys, end to end.
#[test]
fn w6b_envelopes_ttl_and_keys() {
    let repo = W6bRepo::default();
    let cache = FakeCache::default();
    let group = block_on(handle_release_group(&repo, &cache, MBID));
    assert_eq!(group.status, 200);
    assert_eq!(group.cache_ttl, Some(86_400));
    let body = parse_body(&group);
    assert_eq!(body["kind"], "release_group");
    assert_eq!(body["name"], "Group");
    for (resp, envelope, rows) in [
        (
            block_on(handle_group_releases(&repo, &cache, MBID)),
            "release_group_mbid",
            "releases",
        ),
        (
            block_on(handle_artist_urls(&repo, &cache, MBID)),
            "artist_mbid",
            "urls",
        ),
        (
            block_on(handle_recording_videos(&repo, &cache, MBID)),
            "recording_mbid",
            "videos",
        ),
        (
            block_on(handle_recording_isrcs(&repo, &cache, MBID)),
            "recording_mbid",
            "isrcs",
        ),
    ] {
        assert_eq!(resp.status, 200);
        assert_eq!(resp.cache_ttl, Some(86_400));
        let body = parse_body(&resp);
        assert_eq!(body[envelope], MBID);
        assert_eq!(body[rows].as_array().unwrap().len(), 1);
    }
    let keys: Vec<String> = cache.stored.borrow().keys().cloned().collect();
    for key in [
        format!("mb:lookup:release-group:{MBID}"),
        format!("mb:group-releases:{MBID}"),
        format!("mb:artist-urls:{MBID}"),
        format!("mb:recording-videos:{MBID}"),
        format!("mb:recording-isrcs:{MBID}"),
    ] {
        assert!(keys.contains(&key), "KV key {key} must be written");
    }
}

/// Failures stay the generic 500 and are never cached.
#[test]
fn w6b_failures_are_generic_500s() {
    let repo = W6bRepo { fail: true };
    let cache = FakeCache::default();
    for resp in [
        block_on(handle_release_group(&repo, &cache, MBID)),
        block_on(handle_group_releases(&repo, &cache, MBID)),
        block_on(handle_artist_urls(&repo, &cache, MBID)),
        block_on(handle_recording_videos(&repo, &cache, MBID)),
        block_on(handle_recording_isrcs(&repo, &cache, MBID)),
    ] {
        assert_eq!(resp.status, 500);
        assert_eq!(resp.cache_ttl, None);
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["code"], "internal_error");
    }
    assert_eq!(*cache.puts.borrow(), 0);
}
