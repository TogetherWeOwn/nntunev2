//! Pin the search over-return truncation and the 400 matrix.
//!
//! The paged search handler (`handle_search_paged`) clamps `limit` before
//! the repository call and truncates the returned hits to the effective
//! limit afterwards, so a repository that ignores the SQL `LIMIT` can never
//! widen the page. The existing unit suite only exercises a well-behaved
//! fake that mirrors SQL (`[offset..offset+limit]`), so the defensive
//! `truncate` line has no failing test without this file: remove it and the
//! truncation tests below go red.
//!
//! The 400 arms (empty query, too short, too long, control characters, bad
//! `type`, `limit 0`) fail before the cache lookup and the repository call,
//! so a bad query is never a scan and never populates the cache. This file
//! pins the stable codes/messages of that matrix together with the
//! never-a-scan property (`search_calls == 0`, no cache writes).
//!
//! The offset-bound section at the end pins the documented
//! `MAX_SEARCH_OFFSET` contract: an offset above the cap is a past-the-end
//! page (200, `"results": []`, effective offset echoed) decided BEFORE the
//! cache lookup and the repository call, so a caller walking offsets toward
//! `u32::MAX` can neither mint unbounded cache keys nor force deep
//! `OFFSET` scans. The cap value is pinned as a literal here (see
//! `DOCUMENTED_MAX_SEARCH_OFFSET` below) rather than imported, so the RED
//! state on a tree without the bound is an assertion failure, never a
//! compile error. The implementation must introduce
//! `pub const MAX_SEARCH_OFFSET` with exactly this value and document it
//! in `workers/mb/README.md`.
//!
//! Standalone integration target: no existing suite is edited.

use nntune_mb::{
    handle_search_paged, ApiResponse, Entity, Kind, MbRepo, Recording, RepoError, SearchHit,
    SuccessCache, MAX_PAGE_SIZE,
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

/// Hostile repository: always returns 30 hits, ignoring the requested
/// `limit`/`offset` the way a repo that drops the SQL `LIMIT` would. It
/// records what it was asked for so tests can assert the handler forwards
/// the clamped limit rather than the raw request value.
#[derive(Default)]
struct HostileRepo {
    search_calls: RefCell<u32>,
    last_limit: RefCell<u32>,
    last_offset: RefCell<u32>,
}

fn hostile_hits() -> Vec<SearchHit> {
    (0..30)
        .map(|n| SearchHit {
            mbid: format!("123e4567-e89b-12d3-a456-42661417{n:04}"),
            label: format!("Hostile Hit {n:02}"),
            score: 1.0 - (n as f32 / 100.0),
            artist_mbid: None,
            artist_name: None,
        })
        .collect()
}

impl MbRepo for HostileRepo {
    async fn lookup(&self, _kind: Kind, _mbid: &str) -> Result<Option<Entity>, RepoError> {
        Ok(None)
    }

    async fn search(
        &self,
        _kind: Kind,
        _normalized_q: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchHit>, RepoError> {
        *self.search_calls.borrow_mut() += 1;
        *self.last_limit.borrow_mut() = limit;
        *self.last_offset.borrow_mut() = offset;
        // Hostile: ignore `limit`/`offset`, over-return.
        Ok(hostile_hits())
    }

    async fn release_recordings(
        &self,
        _release_mbid: &str,
    ) -> Result<Option<Vec<Recording>>, RepoError> {
        Ok(None)
    }
}

#[derive(Default)]
struct FakeCache {
    stored: RefCell<HashMap<String, String>>,
    gets: RefCell<u32>,
    puts: RefCell<u32>,
}

impl SuccessCache for FakeCache {
    async fn get(&self, key: &str) -> Option<String> {
        *self.gets.borrow_mut() += 1;
        self.stored.borrow().get(key).cloned()
    }

    async fn put(&self, key: &str, body: &str, _ttl_s: u32) {
        *self.puts.borrow_mut() += 1;
        self.stored
            .borrow_mut()
            .insert(key.to_string(), body.to_string());
    }
}

fn parse_body(resp: &ApiResponse) -> serde_json::Value {
    serde_json::from_str(&resp.body).expect("success body is JSON")
}

#[test]
fn hostile_over_return_is_truncated_to_requested_limit() {
    let repo = HostileRepo::default();
    let cache = FakeCache::default();
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "  Hostile   Query ",
        Some(10),
        None,
    ));
    assert_eq!(resp.status, 200);
    let body = parse_body(&resp);
    let results = body["results"].as_array().unwrap();
    assert_eq!(
        results.len(),
        10,
        "hostile 30-hit return must truncate to 10"
    );
    // The page echo carries the effective coordinates and the normalized
    // query even though the repo ignored the SQL `LIMIT`.
    assert_eq!(body["limit"], 10);
    assert_eq!(body["offset"], 0);
    assert_eq!(body["q"], "hostile query");
    assert_eq!(body["type"], "artist");
    // Truncation keeps the head of the repo order.
    let expected: Vec<String> = hostile_hits()[..10]
        .iter()
        .map(|hit| hit.mbid.clone())
        .collect();
    let actual: Vec<String> = results
        .iter()
        .map(|row| row["mbid"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(actual, expected);
    // The effective (not raw) limit reaches the repo.
    assert_eq!(*repo.last_limit.borrow(), 10);
    assert_eq!(*repo.search_calls.borrow(), 1);
}

#[test]
fn over_limit_clamps_to_max_page_size_despite_hostile_repo() {
    for raw in [26u32, 1000] {
        let repo = HostileRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_search_paged(
            &repo,
            &cache,
            Some("artist".to_string()),
            "hostile query",
            Some(raw),
            None,
        ));
        // Clamped, never rejected: 200, not 400.
        assert_eq!(resp.status, 200, "raw limit {raw}");
        let body = parse_body(&resp);
        assert_eq!(
            body["results"].as_array().unwrap().len(),
            MAX_PAGE_SIZE,
            "raw limit {raw}"
        );
        assert_eq!(body["limit"], MAX_PAGE_SIZE as u64, "raw limit {raw}");
        assert_eq!(*repo.last_limit.borrow(), MAX_PAGE_SIZE as u32);
    }
}

#[test]
fn default_limit_truncates_hostile_over_return() {
    let repo = HostileRepo::default();
    let cache = FakeCache::default();
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "hostile query",
        None,
        None,
    ));
    assert_eq!(resp.status, 200);
    let body = parse_body(&resp);
    assert_eq!(body["results"].as_array().unwrap().len(), MAX_PAGE_SIZE);
    assert_eq!(body["limit"], MAX_PAGE_SIZE as u64);
}

#[test]
fn offset_is_echoed_while_page_stays_capped() {
    let repo = HostileRepo::default();
    let cache = FakeCache::default();
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "hostile query",
        Some(10),
        Some(20),
    ));
    assert_eq!(resp.status, 200);
    let body = parse_body(&resp);
    // The hostile repo ignores the offset and still returns 30 rows; the
    // handler caps the page at the effective limit either way.
    assert_eq!(body["results"].as_array().unwrap().len(), 10);
    assert_eq!(body["limit"], 10);
    assert_eq!(body["offset"], 20);
    assert_eq!(*repo.last_offset.borrow(), 20);
}

#[test]
fn zero_limit_is_rejected_not_clamped() {
    let repo = HostileRepo::default();
    let cache = FakeCache::default();
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "hostile query",
        Some(0),
        None,
    ));
    assert_eq!(resp.status, 400);
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "limit must be 1..=25");
    assert_eq!(body["retryable"], false);
    assert_eq!(resp.cache_ttl, None);
    assert_eq!(*repo.search_calls.borrow(), 0);
    assert_eq!(*cache.puts.borrow(), 0);
}

/// The 400 matrix: every bad-query arm fails before the cache lookup and
/// the repository call with a stable code/message, so a bad query is never
/// a scan and never populates the cache.
#[test]
fn bad_query_matrix_is_400_with_stable_codes_and_no_repo_scan() {
    let long_q = "a".repeat(101);
    // (query, type, expected code, expected message)
    let cases: Vec<(String, Option<String>, &str, &str)> = vec![
        (
            "x".to_string(),
            Some("artist".to_string()),
            "invalid_request",
            "query too short (min 2 chars)",
        ),
        (
            long_q,
            Some("artist".to_string()),
            "invalid_request",
            "query too long (max 100 chars)",
        ),
        (
            "ok\u{7}query".to_string(),
            Some("artist".to_string()),
            "invalid_request",
            "query contains control characters",
        ),
        (
            "hostile query".to_string(),
            Some("track".to_string()),
            "invalid_request",
            "type must be one of artist|release|recording",
        ),
        (
            String::new(),
            Some("artist".to_string()),
            "MB_SEARCH_EMPTY_QUERY",
            "search query must not be empty",
        ),
        (
            "  \t  ".to_string(),
            Some("release".to_string()),
            "MB_SEARCH_EMPTY_QUERY",
            "search query must not be empty",
        ),
    ];
    for (q, kind, code, message) in cases {
        let repo = HostileRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_search_paged(&repo, &cache, kind, &q, None, None));
        assert_eq!(resp.status, 400, "q={q:?}");
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["code"], code, "q={q:?}");
        assert_eq!(body["message"], message, "q={q:?}");
        assert_eq!(body["retryable"], false, "q={q:?}");
        assert_eq!(resp.cache_ttl, None, "q={q:?}");
        assert_eq!(*repo.search_calls.borrow(), 0, "q={q:?}");
        assert_eq!(*cache.gets.borrow(), 0, "q={q:?}");
        assert_eq!(*cache.puts.borrow(), 0, "q={q:?}");
    }
}

// ---------------------------------------------------------------------------
// Offset bound (`MAX_SEARCH_OFFSET`)
//
// The documented cap the implementation must introduce as
// `pub const MAX_SEARCH_OFFSET`. 1,000 rows is 40 full pages at
// `MAX_PAGE_SIZE` (25) — far deeper than any UI page walk — and matches the
// existing 1,000-row recordings bound in this worker. Pinned as a literal
// (not an import of the not-yet-existing constant) so RED on an unbounded
// tree is an assertion failure, not a compile error.
// ---------------------------------------------------------------------------

const DOCUMENTED_MAX_SEARCH_OFFSET: u32 = 1_000;

/// Above the cap the handler must short-circuit BEFORE the cache and the
/// repo: a well-formed past-the-end success page (200, empty results,
/// effective offset echoed) with zero cache reads/writes and zero repo
/// calls. On an unbounded tree the hostile repo is actually queried and
/// its 30 hits come back truncated — that is the intended RED.
#[test]
fn offset_above_cap_short_circuits_before_cache_and_repo() {
    for offset in [
        DOCUMENTED_MAX_SEARCH_OFFSET + 1,
        10_000,
        1_000_000,
        u32::MAX,
    ] {
        let repo = HostileRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_search_paged(
            &repo,
            &cache,
            Some("artist".to_string()),
            "  Hostile   Query ",
            Some(10),
            Some(offset),
        ));
        assert_eq!(resp.status, 200, "offset {offset}");
        let body = parse_body(&resp);
        assert_eq!(
            body["results"].as_array().unwrap().len(),
            0,
            "offset {offset} above the cap must be an empty page"
        );
        // The short-circuit page is a well-formed search success body with
        // the effective coordinates echoed, not an error shape.
        assert_eq!(body["offset"], offset as u64, "offset {offset}");
        assert_eq!(body["limit"], 10, "offset {offset}");
        assert_eq!(body["q"], "hostile query", "offset {offset}");
        assert_eq!(body["type"], "artist", "offset {offset}");
        assert_eq!(
            *repo.search_calls.borrow(),
            0,
            "offset {offset} must never reach the repo (no deep OFFSET scan)"
        );
        assert_eq!(
            *cache.gets.borrow(),
            0,
            "offset {offset} must never read the cache"
        );
        assert_eq!(
            *cache.puts.borrow(),
            0,
            "offset {offset} must never mint a cache key"
        );
        assert!(cache.stored.borrow().is_empty(), "offset {offset}");
    }
}

/// The boundary itself: exactly at the cap the search runs exactly as
/// today (cache miss, one repo call at the requested offset, page capped
/// at the effective limit, result cached); cap+1 short-circuits.
#[test]
fn offset_at_cap_queries_and_cap_plus_one_short_circuits() {
    // At the cap: queried as before.
    let repo = HostileRepo::default();
    let cache = FakeCache::default();
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "hostile query",
        Some(10),
        Some(DOCUMENTED_MAX_SEARCH_OFFSET),
    ));
    assert_eq!(resp.status, 200);
    let body = parse_body(&resp);
    assert_eq!(body["results"].as_array().unwrap().len(), 10);
    assert_eq!(body["offset"], DOCUMENTED_MAX_SEARCH_OFFSET as u64);
    assert_eq!(body["limit"], 10);
    assert_eq!(*repo.search_calls.borrow(), 1);
    assert_eq!(*repo.last_offset.borrow(), DOCUMENTED_MAX_SEARCH_OFFSET);
    assert_eq!(*cache.gets.borrow(), 1);
    assert_eq!(*cache.puts.borrow(), 1);

    // Cap+1: short-circuits with zero calls.
    let repo = HostileRepo::default();
    let cache = FakeCache::default();
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "hostile query",
        Some(10),
        Some(DOCUMENTED_MAX_SEARCH_OFFSET + 1),
    ));
    assert_eq!(resp.status, 200);
    let body = parse_body(&resp);
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
    assert_eq!(body["offset"], (DOCUMENTED_MAX_SEARCH_OFFSET + 1) as u64);
    assert_eq!(*repo.search_calls.borrow(), 0);
    assert_eq!(*cache.gets.borrow(), 0);
    assert_eq!(*cache.puts.borrow(), 0);
}

/// Below the cap — including an explicit 0 and cap-1 — behavior is exactly
/// as today: one repo call forwarding the requested offset, the page
/// capped at the effective limit, coordinates echoed.
#[test]
fn offsets_below_cap_including_zero_behave_as_today() {
    for offset in [0, 1, DOCUMENTED_MAX_SEARCH_OFFSET - 1] {
        let repo = HostileRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_search_paged(
            &repo,
            &cache,
            Some("release".to_string()),
            "hostile query",
            Some(10),
            Some(offset),
        ));
        assert_eq!(resp.status, 200, "offset {offset}");
        let body = parse_body(&resp);
        assert_eq!(
            body["results"].as_array().unwrap().len(),
            10,
            "offset {offset}"
        );
        assert_eq!(body["offset"], offset as u64, "offset {offset}");
        assert_eq!(*repo.search_calls.borrow(), 1, "offset {offset}");
        assert_eq!(*repo.last_offset.borrow(), offset, "offset {offset}");
        assert_eq!(*cache.gets.borrow(), 1, "offset {offset}");
        assert_eq!(*cache.puts.borrow(), 1, "offset {offset}");
    }
}

/// The short-circuit composes with the existing coordinate rules: with no
/// explicit limit the echoed limit is still the default page size, and an
/// over-limit request is still clamped (never rejected) on the empty page.
#[test]
fn offset_above_cap_echoes_effective_limit() {
    for (raw_limit, expected_limit) in [
        (None, MAX_PAGE_SIZE),
        (Some(1000), MAX_PAGE_SIZE),
        (Some(7), 7),
    ] {
        let repo = HostileRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_search_paged(
            &repo,
            &cache,
            Some("recording".to_string()),
            "hostile query",
            raw_limit,
            Some(DOCUMENTED_MAX_SEARCH_OFFSET + 1),
        ));
        assert_eq!(resp.status, 200, "raw limit {raw_limit:?}");
        let body = parse_body(&resp);
        assert_eq!(
            body["results"].as_array().unwrap().len(),
            0,
            "raw limit {raw_limit:?}"
        );
        assert_eq!(
            body["limit"], expected_limit as u64,
            "raw limit {raw_limit:?}"
        );
        assert_eq!(
            body["offset"],
            (DOCUMENTED_MAX_SEARCH_OFFSET + 1) as u64,
            "raw limit {raw_limit:?}"
        );
        assert_eq!(*repo.search_calls.borrow(), 0, "raw limit {raw_limit:?}");
        assert_eq!(*cache.gets.borrow(), 0, "raw limit {raw_limit:?}");
        assert_eq!(*cache.puts.borrow(), 0, "raw limit {raw_limit:?}");
    }
}

/// The offset bound must not bypass query validation: a bad query (or a
/// zero limit) with an above-cap offset is still the same 400 as today,
/// still before any cache or repo call. This passes on an unbounded tree
/// and guards the validation-before-short-circuit ordering.
#[test]
fn offset_above_cap_does_not_bypass_validation() {
    // Empty query + above-cap offset: still 400 MB_SEARCH_EMPTY_QUERY.
    let repo = HostileRepo::default();
    let cache = FakeCache::default();
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "   ",
        Some(10),
        Some(DOCUMENTED_MAX_SEARCH_OFFSET + 1),
    ));
    assert_eq!(resp.status, 400);
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    assert_eq!(body["code"], "MB_SEARCH_EMPTY_QUERY");
    assert_eq!(*repo.search_calls.borrow(), 0);
    assert_eq!(*cache.gets.borrow(), 0);
    assert_eq!(*cache.puts.borrow(), 0);

    // Zero limit + above-cap offset: still 400, not an empty 200 page.
    let repo = HostileRepo::default();
    let cache = FakeCache::default();
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "hostile query",
        Some(0),
        Some(DOCUMENTED_MAX_SEARCH_OFFSET + 1),
    ));
    assert_eq!(resp.status, 400);
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "limit must be 1..=25");
    assert_eq!(*repo.search_calls.borrow(), 0);
    assert_eq!(*cache.gets.borrow(), 0);
    assert_eq!(*cache.puts.borrow(), 0);
}
