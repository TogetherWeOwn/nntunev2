//! Pin the release-recordings 1000-row truncation.
//!
//! [`handle_release_recordings`] truncates the release→recordings expansion
//! to 1000 rows (`RELEASE_RECORDINGS_LIMIT`) before caching/serving, so a
//! repository that over-returns (ignores the SQL `LIMIT 1000`) can never
//! widen the listing. The in-module suite only feeds `LIMIT + 5` rows, so
//! the exact boundary behavior — 1001 and 1500 in, exactly 1000 out, head
//! order kept — has no failing test without this file: remove the
//! `recordings.truncate` line and the cap tests below go red.
//!
//! The empty (`Some(vec![])`) and missing (`None`) controls behave as
//! before: an empty listing is a cacheable 200 with `"recordings": []`,
//! a missing release is a 404 that is never cached.
//!
//! Premise note (checked 2026-10-18): a `tests/` target cannot reach the
//! private handler or build `Recording` values (private fields), so this
//! pin needs the same visibility-only precedent `search_validation` uses —
//! `handle_release_recordings` and `Recording`'s fields are `pub`, mirroring
//! `handle_search_paged`/`SearchHit`. No behavior, SQL, TTL or existing
//! test changed.
//!
//! Standalone integration target: no existing suite is edited.

use nntune_mb::{
    handle_release_recordings, ApiResponse, Entity, Kind, MbRepo, Recording, RepoError, SearchHit,
    SuccessCache,
};
use std::cell::RefCell;
use std::collections::HashMap;
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

/// The pinned cap: `RELEASE_RECORDINGS_LIMIT` in `workers/mb/src/lib.rs`.
/// Hardcoded on purpose — a cap-value change must turn this file red
/// (cap value itself is out of scope).
const PINNED_CAP: usize = 1000;

/// Fake repository holding one release-recordings corpus. It over-returns
/// on purpose: the full corpus, ignoring the SQL `LIMIT`, the way a repo
/// that drops the bound would.
struct CapRepo {
    recordings: Option<Vec<Recording>>,
    recordings_calls: RefCell<u32>,
}

fn recording(n: usize) -> Recording {
    Recording {
        mbid: format!("123e4567-e89b-12d3-a456-42661417{n:04}"),
        release_mbid: Some(MBID.to_string()),
        title: format!("Track {n}"),
        track_no: Some(n as i16),
        duration_ms: Some(180_000),
        medium_no: Some(1),
    }
}

fn corpus(n: usize) -> Vec<Recording> {
    (0..n).map(recording).collect()
}

impl MbRepo for CapRepo {
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
    ) -> Result<Option<Vec<Recording>>, RepoError> {
        *self.recordings_calls.borrow_mut() += 1;
        Ok(self.recordings.clone())
    }
}

#[derive(Default)]
struct FakeCache {
    stored: RefCell<HashMap<String, String>>,
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

fn repo_with(n: usize) -> CapRepo {
    CapRepo {
        recordings: Some(corpus(n)),
        recordings_calls: RefCell::new(0),
    }
}

fn parse_body(resp: &ApiResponse) -> serde_json::Value {
    serde_json::from_str(&resp.body).expect("success body is JSON")
}

/// One row past the cap: the served body holds exactly the pinned cap.
#[test]
fn one_past_cap_yields_exactly_pinned_cap_rows() {
    let repo = repo_with(PINNED_CAP + 1);
    let cache = FakeCache::default();
    let resp = block_on(handle_release_recordings(&repo, &cache, MBID));
    assert_eq!(resp.status, 200);
    let body = parse_body(&resp);
    assert_eq!(body["release_mbid"], MBID);
    let rows = body["recordings"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        PINNED_CAP,
        "1001 in must serve exactly 1000 rows"
    );
    // Truncation keeps the head of the repo order.
    let expected: Vec<String> = corpus(PINNED_CAP + 1)[..PINNED_CAP]
        .iter()
        .map(|r| r.mbid.clone())
        .collect();
    let actual: Vec<String> = rows
        .iter()
        .map(|row| row["mbid"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(*repo.recordings_calls.borrow(), 1);
}

/// Well past the cap: the served body still holds exactly the pinned cap.
#[test]
fn fifty_percent_past_cap_yields_exactly_pinned_cap_rows() {
    let repo = repo_with(1500);
    let cache = FakeCache::default();
    let resp = block_on(handle_release_recordings(&repo, &cache, MBID));
    assert_eq!(resp.status, 200);
    let body = parse_body(&resp);
    let rows = body["recordings"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        PINNED_CAP,
        "1500 in must serve exactly 1000 rows"
    );
    // The cached body is the truncated one, so replays stay capped too.
    assert_eq!(*cache.puts.borrow(), 1);
    let cached: serde_json::Value = serde_json::from_str(
        cache
            .stored
            .borrow()
            .values()
            .next()
            .expect("truncated success is cached"),
    )
    .unwrap();
    assert_eq!(cached["recordings"].as_array().unwrap().len(), PINNED_CAP);
}

/// Empty control: a release with no recordings is a cacheable 200 with
/// `"recordings": []`, as before.
#[test]
fn empty_listing_stays_cacheable_200() {
    let repo = CapRepo {
        recordings: Some(vec![]),
        recordings_calls: RefCell::new(0),
    };
    let cache = FakeCache::default();
    let resp = block_on(handle_release_recordings(&repo, &cache, MBID));
    assert_eq!(resp.status, 200);
    assert!(resp.cache_ttl.is_some());
    let body = parse_body(&resp);
    assert_eq!(body["recordings"].as_array().unwrap().len(), 0);
    assert_eq!(*cache.puts.borrow(), 1);
}

/// Missing control: an unknown release is a 404 that is never cached,
/// as before.
#[test]
fn missing_release_stays_uncached_404() {
    let repo = CapRepo {
        recordings: None,
        recordings_calls: RefCell::new(0),
    };
    let cache = FakeCache::default();
    let resp = block_on(handle_release_recordings(&repo, &cache, MBID));
    assert_eq!(resp.status, 404);
    assert_eq!(resp.cache_ttl, None);
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    assert_eq!(body["code"], "not_found");
    assert_eq!(*cache.puts.borrow(), 0);
}
