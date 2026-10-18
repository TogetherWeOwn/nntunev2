//! Pin the search query-length exact pass/reject boundary.
//!
//! `handle_search_paged` (`workers/mb/src/lib.rs:58,579`) bounds `q` by chars:
//! `q.chars().count() > MAX_QUERY_CHARS` (100) rejects with
//! `invalid_request` / `query too long (max 100 chars)` before the cache
//! lookup and the repository call. The existing `search_validation.rs` pin
//! only feeds a 101-char query, so a downward drift of the bound (for
//! example 100 becoming 99, or a bytes-instead-of-chars count) still passes
//! the suite while the message still claims 100.
//!
//! Sensitivity (what goes red on drift):
//! - If the bound drifts down (99) or the count switches to bytes/trimmed
//!   length, `exactly_100_chars_reaches_the_repository` goes red (400
//!   instead of 200, `search_calls == 0`).
//! - If the bound drifts up (101) or the check becomes `>=`, the 101-char
//!   arm of `boundary_pair_pass_reject` goes red (200 with a repo scan and
//!   a cache put instead of 400 with neither).
//!
//! Standalone integration target: no existing suite is edited.

use nntune_mb::{
    handle_search_paged, Entity, Kind, MbRepo, Recording, RepoError, SearchHit, SuccessCache,
    MAX_PAGE_SIZE,
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

/// Counting repository: returns no hits and records whether it was scanned.
#[derive(Default)]
struct CountingRepo {
    search_calls: RefCell<u32>,
}

impl MbRepo for CountingRepo {
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
        *self.search_calls.borrow_mut() += 1;
        Ok(Vec::new())
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

#[test]
fn exactly_100_chars_reaches_the_repository() {
    let repo = CountingRepo::default();
    let cache = FakeCache::default();
    // Multibyte tail: exactly 100 chars but 101 bytes, so a
    // bytes-instead-of-chars count would wrongly reject this.
    let q = format!("{}é", "a".repeat(99));
    assert_eq!(q.chars().count(), 100);
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        &q,
        None,
        None,
    ));
    assert_eq!(resp.status, 200, "exactly 100 chars must pass, not 400");
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    assert_eq!(body["q"], q);
    assert_eq!(body["limit"], MAX_PAGE_SIZE as u64);
    assert_eq!(
        *repo.search_calls.borrow(),
        1,
        "100-char query must scan once"
    );
}

#[test]
fn boundary_pair_pass_reject() {
    // 100 chars (multibyte tail: 101 bytes): pass with a repo scan and a
    // cache put. A bytes-instead-of-chars count would wrongly reject this.
    let repo = CountingRepo::default();
    let cache = FakeCache::default();
    let pass_q = format!("{}é", "a".repeat(99));
    assert_eq!(pass_q.chars().count(), 100);
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        &pass_q,
        None,
        None,
    ));
    assert_eq!(resp.status, 200);
    assert_eq!(*repo.search_calls.borrow(), 1);
    assert_eq!(*cache.puts.borrow(), 1);

    // 101 chars: 400 `invalid_request` with no repo scan and no cache put.
    let repo = CountingRepo::default();
    let cache = FakeCache::default();
    let reject_q = "a".repeat(101);
    assert_eq!(reject_q.chars().count(), 101);
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        &reject_q,
        None,
        None,
    ));
    assert_eq!(resp.status, 400);
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "query too long (max 100 chars)");
    assert_eq!(body["retryable"], false);
    assert_eq!(resp.cache_ttl, None);
    assert_eq!(
        *repo.search_calls.borrow(),
        0,
        "101-char query must never scan"
    );
    assert_eq!(
        *cache.gets.borrow(),
        0,
        "101-char query must skip cache lookup"
    );
    assert_eq!(
        *cache.puts.borrow(),
        0,
        "101-char query must never populate cache"
    );
}
