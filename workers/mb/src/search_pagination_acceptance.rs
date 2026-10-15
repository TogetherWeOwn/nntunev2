//! Acceptance tests for "mb: search pagination, limits and empty-query
//! handling" ([TOG-25374](/TOG/issues/TOG-25374), roadmap T3 on
//! [TOG-25350](/TOG/issues/TOG-25350#document-roadmap)).
//!
//! Owned by [TOG-25373](/TOG/issues/TOG-25373). The Builder (TOG-25374) builds
//! on this branch and **never edits this file** (nor the golden fixture in
//! `../testdata/`): make the tests below pass by changing non-test code only.
//! Keep the `#[cfg(test)] mod search_pagination_acceptance;` line in
//! `lib.rs`.
//!
//! ## Contract the Builder implements (all in non-test code)
//!
//! 1. `const MAX_PAGE_SIZE: usize = 25;` — the documented max page size.
//!    It matches the existing trigram `LIMIT 25` and the README's `LIMIT 25`.
//! 2. `async fn handle_search_paged<R: MbRepo, C: SuccessCache>(repo: &R,
//!    cache: &C, raw_kind: Option<String>, raw_q: &str, limit: Option<u32>,
//!    offset: Option<u32>) -> ApiResponse` — additive; the existing
//!    4-argument `handle_search` keeps working untouched.
//!    - `limit`: `None` defaults to `MAX_PAGE_SIZE`; `Some(0)` is rejected
//!      with 400 (mirrors ingest's `limit must be 1..=100`); `1..=MAX` is
//!      honored; anything above is **clamped** to `MAX_PAGE_SIZE`, never
//!      rejected. The effective value is echoed as the numeric `limit` field
//!      in the success body.
//!    - `offset`: `None`/unparsable defaults to `0`; an offset past the end
//!      of the result set is a 200 with `"results": []`, never an error and
//!      never a scan. The effective value is echoed as `offset`.
//!    - Success body: `{"q", "type", "limit", "offset", "results"}` — the
//!      existing `q` (normalized)/`type`/`results` keys plus the echoes.
//! 3. `MbRepo::search` gains the page: `async fn search(&self, kind: Kind,
//!    normalized_q: &str, limit: u32, offset: u32) -> ...` so SQL applies
//!    `LIMIT`/`OFFSET` (the handler still truncates defensively, mirroring
//!    the existing `hits.truncate(SEARCH_LIMIT)`). The handler forwards the
//!    clamped limit and the requested offset.
//! 4. Empty or whitespace-only queries return **400** with the stable
//!    envelope `{"code": "MB_SEARCH_EMPTY_QUERY", "message": <non-empty>,
//!    "retryable": false}` (register the code in `docs/api-errors.md` under
//!    [TOG-25315](/TOG/issues/TOG-25315)), `no-store`, without touching the
//!    repo (never a scan) or the cache. The empty check runs before `type`
//!    validation, so the code is stable regardless of `type`.
//!
//! These tests FAIL on the pre-build head (the paged entry point, the const
//! and the trait widening do not exist yet; the empty path has no `code`)
//! and pass once the build card implements the contract above.

use super::{Entity, Kind, MbRepo, RepoError, SearchHit, SuccessCache};
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

/// Golden pagination corpus: 50 deterministic rows, strictly decreasing
/// scores so the documented order (score DESC, label ASC, mbid ASC) is total
/// without depending on tiebreaks.
const GOLDEN_JSON: &str = include_str!("../testdata/search_pagination_golden.json");

#[derive(serde::Deserialize)]
struct FixtureHit {
    mbid: String,
    label: String,
}

fn golden_hits() -> Vec<SearchHit> {
    let fixture: Vec<FixtureHit> = serde_json::from_str(GOLDEN_JSON).unwrap();
    assert_eq!(fixture.len(), 50, "golden fixture must hold two full pages");
    fixture
        .into_iter()
        .enumerate()
        .map(|(i, row)| SearchHit {
            mbid: row.mbid,
            label: row.label,
            score: 0.99 - (i as f32 * 0.001),
        })
        .collect()
}

/// Fake repo that honors the paged contract the way SQL `LIMIT`/`OFFSET`
/// will: slice `[offset..offset+limit]` over the fixture hits.
#[derive(Default)]
struct FakePagedRepo {
    hits: Vec<SearchHit>,
    search_calls: RefCell<u32>,
    last_limit: RefCell<u32>,
    last_offset: RefCell<u32>,
    last_q: RefCell<String>,
}

impl FakePagedRepo {
    fn with_golden() -> Self {
        Self {
            hits: golden_hits(),
            ..Default::default()
        }
    }
}

impl MbRepo for FakePagedRepo {
    async fn lookup(&self, _kind: Kind, _mbid: &str) -> Result<Option<Entity>, RepoError> {
        Ok(None)
    }

    async fn search(
        &self,
        _kind: Kind,
        normalized_q: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>, RepoError> {
        *self.search_calls.borrow_mut() += 1;
        *self.last_limit.borrow_mut() = limit;
        *self.last_offset.borrow_mut() = offset;
        *self.last_q.borrow_mut() = normalized_q.to_string();
        let from = (offset as usize).min(self.hits.len());
        let to = from.saturating_add(limit as usize).min(self.hits.len());
        Ok(self.hits[from..to].to_vec())
    }
}

#[derive(Default)]
struct FakePagedCache {
    stored: RefCell<std::collections::HashMap<String, String>>,
    puts: RefCell<u32>,
}

impl SuccessCache for FakePagedCache {
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

fn result_mbids(body: &serde_json::Value) -> Vec<String> {
    body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["mbid"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn over_limit_request_is_clamped_to_max_page_size() {
    let repo = FakePagedRepo::with_golden();
    let cache = FakePagedCache::default();
    let resp = block_on(super::handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "golden query",
        Some(100),
        None,
    ));
    assert_eq!(resp.status, 200);
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), super::MAX_PAGE_SIZE);
    assert_eq!(body["limit"], super::MAX_PAGE_SIZE as u64);
    assert_eq!(body["offset"], 0);
    // The clamped limit (not the requested 100) reaches the repo, so SQL
    // never scans more than one page; the repo sees the normalized query,
    // matching the cache key.
    assert_eq!(*repo.last_limit.borrow(), super::MAX_PAGE_SIZE as u32);
    assert_eq!(*repo.last_offset.borrow(), 0);
    assert_eq!(repo.last_q.borrow().as_str(), "golden query");
}

#[test]
fn empty_query_returns_stable_4xx_code() {
    for (kind, q) in [
        (Some("artist".to_string()), ""),
        // The empty check runs before `type` validation: the code is stable
        // even without a valid type.
        (None, ""),
        (Some("track".to_string()), ""),
    ] {
        let repo = FakePagedRepo::with_golden();
        let cache = FakePagedCache::default();
        let resp = block_on(super::handle_search_paged(
            &repo, &cache, kind, q, None, None,
        ));
        assert_eq!(resp.status, 400, "q={q:?}");
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["code"], "MB_SEARCH_EMPTY_QUERY", "q={q:?}");
        assert!(
            body["message"].as_str().is_some_and(|m| !m.is_empty()),
            "q={q:?}"
        );
        assert_eq!(body["retryable"], false, "q={q:?}");
        assert_eq!(resp.cache_ttl, None, "q={q:?}");
        // Never a scan, never cached.
        assert_eq!(*repo.search_calls.borrow(), 0, "q={q:?}");
        assert_eq!(*cache.puts.borrow(), 0, "q={q:?}");
    }
}

#[test]
fn whitespace_query_returns_stable_4xx_code() {
    let repo = FakePagedRepo::with_golden();
    let cache = FakePagedCache::default();
    let resp = block_on(super::handle_search_paged(
        &repo,
        &cache,
        Some("release".to_string()),
        "  \t  ",
        None,
        None,
    ));
    assert_eq!(resp.status, 400);
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    assert_eq!(body["code"], "MB_SEARCH_EMPTY_QUERY");
    assert_eq!(body["retryable"], false);
    assert_eq!(*repo.search_calls.borrow(), 0);
    assert_eq!(*cache.puts.borrow(), 0);
}

#[test]
fn golden_pages_one_and_two_are_stable_and_disjoint() {
    let fixture: Vec<FixtureHit> = serde_json::from_str(GOLDEN_JSON).unwrap();
    let page_one_mbids: Vec<String> = fixture[..25].iter().map(|row| row.mbid.clone()).collect();
    let page_two_mbids: Vec<String> = fixture[25..].iter().map(|row| row.mbid.clone()).collect();

    let repo = FakePagedRepo::with_golden();
    let cache = FakePagedCache::default();

    let first = block_on(super::handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "  Golden   Query ",
        None,
        None,
    ));
    assert_eq!(first.status, 200);
    let first_body: serde_json::Value = serde_json::from_str(&first.body).unwrap();
    assert_eq!(first_body["q"], "golden query");
    assert_eq!(first_body["type"], "artist");
    assert_eq!(first_body["limit"], 25);
    assert_eq!(first_body["offset"], 0);
    assert_eq!(result_mbids(&first_body), page_one_mbids);
    // Artist hits answer `name`, matching the lookup field name.
    assert_eq!(first_body["results"][0]["name"], "Golden Song 001");

    let second = block_on(super::handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "golden query",
        Some(25),
        Some(25),
    ));
    assert_eq!(second.status, 200);
    let second_body: serde_json::Value = serde_json::from_str(&second.body).unwrap();
    assert_eq!(second_body["offset"], 25);
    assert_eq!(result_mbids(&second_body), page_two_mbids);

    // Pages are disjoint: no mbid appears on both.
    let first_set: std::collections::HashSet<&str> =
        page_one_mbids.iter().map(String::as_str).collect();
    assert!(
        page_two_mbids
            .iter()
            .all(|mbid| !first_set.contains(mbid.as_str())),
        "pages 1 and 2 must be disjoint"
    );

    // A small window inside the corpus pages the same rows.
    let window = block_on(super::handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "golden query",
        Some(10),
        Some(5),
    ));
    assert_eq!(window.status, 200);
    let window_body: serde_json::Value = serde_json::from_str(&window.body).unwrap();
    let window_mbids = result_mbids(&window_body);
    assert_eq!(window_mbids.len(), 10);
    assert_eq!(
        window_mbids,
        page_one_mbids[5..15].to_vec(),
        "offset window must slice the same order"
    );

    // Stability: repeating page 1 returns byte-identical results.
    let repeat = block_on(super::handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "Golden Query",
        None,
        None,
    ));
    assert_eq!(repeat.status, 200);
    let repeat_body: serde_json::Value = serde_json::from_str(&repeat.body).unwrap();
    assert_eq!(result_mbids(&repeat_body), page_one_mbids);
}

#[test]
fn offset_beyond_results_is_empty_200() {
    let repo = FakePagedRepo::with_golden();
    let cache = FakePagedCache::default();
    let resp = block_on(super::handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "golden query",
        None,
        Some(10_000),
    ));
    assert_eq!(resp.status, 200);
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
    assert_eq!(body["offset"], 10_000);
}

#[test]
fn zero_limit_is_rejected_not_clamped() {
    let repo = FakePagedRepo::with_golden();
    let cache = FakePagedCache::default();
    let resp = block_on(super::handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "golden query",
        Some(0),
        None,
    ));
    assert_eq!(resp.status, 400);
    assert_eq!(*repo.search_calls.borrow(), 0);
}

#[test]
fn invalid_type_still_rejected_on_paged_path() {
    let repo = FakePagedRepo::with_golden();
    let cache = FakePagedCache::default();
    let resp = block_on(super::handle_search_paged(
        &repo,
        &cache,
        Some("track".to_string()),
        "golden query",
        None,
        None,
    ));
    assert_eq!(resp.status, 400);
    assert_eq!(*repo.search_calls.borrow(), 0);
    assert_eq!(*cache.puts.borrow(), 0);
}
