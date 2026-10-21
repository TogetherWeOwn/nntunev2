//! Pin the edge Cache API key on the canonical request — wiring + grounding.
//!
//! The KV layer already keys one search under one entry:
//! `handle_search_paged` normalizes `q` (`normalize_query`: trimmed,
//! lowercased, interior whitespace collapsed), clamps/defaults `limit`,
//! defaults `offset`, and keys `search_paged_cache_key(kind, normalized_q,
//! effective_limit, effective_offset)` (`workers/mb/src/lib.rs:692`). The
//! edge layer does not: `edge_hit` (`lib.rs:1160-1163`) and `serve`
//! (`lib.rs:1150-1158`) key the Cache API by the RAW request URL, so the
//! same search under different `q` case, param order, limit form or offset
//! form mints a separate edge entry for every spelling and misses the copy
//! a sibling spelling already stored.
//!
//! ## Wished-for implementation (Builder contract)
//!
//! Add ONE pure, transport-neutral helper in the core of
//! `workers/mb/src/lib.rs` — outside the wasm-only `http_layer` module,
//! beside `normalize_query` / `search_paged_cache_key`, so the native
//! vector suite can link it — with exactly this signature:
//!
//! ```ignore
//! pub fn canonical_edge_url(url: &str) -> String
//! ```
//!
//! `pub` for the integration pins, following the `MAX_PAGE_SIZE` /
//! `MAX_SEARCH_OFFSET` precedent in the same file. Nothing in this
//! contract renames routes, query parameters, response bodies or KV keys.
//!
//! The helper takes the absolute request URL exactly as `edge_hit`/`serve`
//! see it (`Request::url()` rendered with `to_string()`) and returns the
//! ONE URL both must key on. The full input→output contract is pinned
//! behaviorally in the sibling target `canonical_edge_url_vectors.rs`;
//! in short: a served search (path exactly `/v1/mb/search`, handler-valid
//! `q`/`type`/`limit`/`offset`) rewrites to
//! `<scheme>://<host>/v1/mb/search?q=<normalized q>&type=<kind>&limit=<effective limit>&offset=<effective offset>`
//! and EVERYTHING else returns the input unchanged — a safety property,
//! because `edge_hit` runs before validation and must never be able to
//! serve a request the handler would reject, nor a real page's body for an
//! above-cap offset.
//!
//! `edge_hit` and `serve` must both route their Cache API key through the
//! helper (`serve` canonicalizing its `req_url` argument itself, so no
//! call site can forget); `serve` keeps its success-only guard
//! (`api.cache_ttl.is_some()`). Lookup/recordings routes keep raw-URL
//! keys (their only variance, MBID case, is out of scope here).
//!
//! ## How this file pins it
//!
//! - Source pins: the Cache API needs the Workers runtime, so get/put
//!   wiring cannot execute natively; the pins slice each function body the
//!   way `trace_propagation_acceptance.rs` pins the wasm-only trace
//!   adapter. These are ASSERTION-red on the current head.
//! - Handler grounding: the REAL `handle_search_paged` facts the
//!   canonical coordinates must mirror (normalized `q`, effective
//!   limit/offset echoes, the 400 set, the above-cap empty page, one KV
//!   entry across spellings). Green today; if the handler's normalization
//!   drifts from the vectors, these go red together with them instead of
//!   the two layers silently diverging.
//!
//! Out of scope (per the card): KV-layer normalization (already
//! canonical), TTL/SWR policy, the offset bound itself (landed #545),
//! lookup/recordings edge keys.
//!
//! Standalone integration target: no existing suite is edited.

use nntune_mb::{
    handle_search_paged, Entity, Kind, MbRepo, Recording, RepoError, SearchHit, SuccessCache,
    MAX_PAGE_SIZE, MAX_SEARCH_OFFSET,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::task::{Context, Poll, Waker};

const LIB_RS: &str = include_str!("../src/lib.rs");

// ---------------------------------------------------------------------------
// Source pins: edge get and put both go through the one canonical key
// ---------------------------------------------------------------------------

fn production_source() -> &'static str {
    LIB_RS
        .split_once("#[cfg(test)]")
        .expect("test module must exist")
        .0
}

/// Slice one function body out of the production source, from its `fn`
/// signature to the matching closing brace (brace-depth scan; the sliced
/// functions carry no braces inside string literals).
fn fn_body(source: &str, signature: &str) -> String {
    let rest = source
        .split_once(signature)
        .unwrap_or_else(|| panic!("{signature} must exist"))
        .1;
    let mut depth = 0i32;
    let mut end = rest.len();
    for (i, ch) in rest.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    rest[..end].to_string()
}

/// Whitespace-free view for token matching: rustfmt may break a method
/// chain across lines (`Cache::default()\n.put(`), and the pins care
/// about the tokens, not the layout.
fn squashed(source: &str) -> String {
    source.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn edge_hit_keys_its_get_through_the_canonical_url() {
    let source = production_source();
    let body = fn_body(source, "async fn edge_hit(");
    assert!(
        body.contains("canonical_edge_url"),
        "edge_hit must key its Cache API get through canonical_edge_url, \
         not the raw request URL; body was:\n{body}"
    );
    assert!(
        squashed(&body).contains("Cache::default().get("),
        "edge_hit must still read the edge Cache API; body was:\n{body}"
    );
}

#[test]
fn serve_keys_its_put_through_the_canonical_url() {
    let source = production_source();
    let body = fn_body(source, "async fn serve(");
    // `serve` takes `req_url: String` and must canonicalize that argument
    // itself — one chokepoint, so no call site can forget.
    assert!(
        body.contains("canonical_edge_url"),
        "serve must key its Cache API put through canonical_edge_url, \
         not the raw request URL; body was:\n{body}"
    );
    assert!(
        squashed(&body).contains("Cache::default().put("),
        "serve must still write the edge Cache API; body was:\n{body}"
    );
    assert!(
        squashed(&body).contains("cache_ttl.is_some()"),
        "serve must keep storing successes only; body was:\n{body}"
    );
}

#[test]
fn no_other_edge_cache_call_site_bypasses_the_helper() {
    let source = production_source();
    // Exactly the get in `edge_hit` and the put in `serve` may touch the
    // edge Cache API; a third call site could key raw and split entries.
    assert_eq!(
        squashed(source).matches("Cache::default().get(").count(),
        1,
        "exactly one edge get call site (edge_hit)"
    );
    assert_eq!(
        squashed(source).matches("Cache::default().put(").count(),
        1,
        "exactly one edge put call site (serve)"
    );
    // The helper is defined once, in the core (before the wasm-only
    // http_layer module) so both the routes and the native vector suite
    // share the one implementation.
    assert_eq!(
        source.matches("fn canonical_edge_url(").count(),
        1,
        "canonical_edge_url must be defined exactly once"
    );
    let helper_at = source
        .find("fn canonical_edge_url(")
        .expect("canonical_edge_url must exist");
    let http_layer_at = source
        .find("mod http_layer")
        .expect("http_layer module must exist");
    assert!(
        helper_at < http_layer_at,
        "canonical_edge_url must live in the transport-neutral core, \
         above mod http_layer"
    );
    assert!(
        source.contains("pub fn canonical_edge_url("),
        "canonical_edge_url must be pub for the tests/ integration pin"
    );
}

// ---------------------------------------------------------------------------
// Handler grounding: the canonical coordinates mirror what the handler
// actually serves (normalized q, effective limit/offset echoes, 400 set)
// ---------------------------------------------------------------------------

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

/// The echoed body coordinates are the ground truth the canonical URL
/// must encode: normalized `q`, clamped/defaulted `limit`, defaulted
/// `offset`. One served body per canonical key group in the vector
/// target.
#[test]
fn handler_echoes_the_coordinates_the_canonical_url_encodes() {
    // (raw q, raw limit, raw offset) -> (echoed q, echoed limit, echoed offset)
    type EchoCase = (
        &'static str,
        Option<u32>,
        Option<u32>,
        &'static str,
        u64,
        u64,
    );
    let cases: Vec<EchoCase> = vec![
        ("  Blue   Monday ", None, None, "blue monday", 25, 0),
        ("BLUE MONDAY", Some(1000), Some(0), "blue monday", 25, 0),
        ("Blue Monday", Some(10), Some(20), "blue monday", 10, 20),
        (
            "blue monday",
            Some(10),
            Some(MAX_SEARCH_OFFSET),
            "blue monday",
            10,
            1_000,
        ),
        ("Café München", None, None, "café münchen", 25, 0),
    ];
    assert_eq!(MAX_PAGE_SIZE, 25, "cases pin the documented page size");
    assert_eq!(MAX_SEARCH_OFFSET, 1_000, "cases pin the documented cap");
    for (raw_q, raw_limit, raw_offset, q, limit, offset) in cases {
        let repo = CountingRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_search_paged(
            &repo,
            &cache,
            Some("artist".to_string()),
            raw_q,
            raw_limit,
            raw_offset,
        ));
        assert_eq!(resp.status, 200, "q={raw_q:?}");
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["q"], q, "q={raw_q:?}");
        assert_eq!(body["limit"], limit, "q={raw_q:?} limit={raw_limit:?}");
        assert_eq!(body["offset"], offset, "q={raw_q:?} offset={raw_offset:?}");
        assert_eq!(body["type"], "artist");
        // And the KV layer stores exactly one entry for the search.
        assert_eq!(*cache.puts.borrow(), 1, "q={raw_q:?}");
        assert_eq!(cache.stored.borrow().len(), 1, "q={raw_q:?}");
    }
}

/// Every raw spelling of one search already shares ONE KV entry (the
/// second spelling is a KV hit: no second repo scan, no second put).
/// This is the behavior the edge layer must match; it also guards the
/// premise of the whole card — if KV sharing ever regresses, the
/// canonical edge key alone would paper over it.
#[test]
fn kv_layer_already_shares_one_entry_across_spellings() {
    let repo = CountingRepo::default();
    let cache = FakeCache::default();
    for (raw_q, raw_limit) in [
        ("Blue Monday", Some(10u32)),
        ("  blue   monday ", Some(10)),
        ("BLUE MONDAY", Some(10)),
    ] {
        let resp = block_on(handle_search_paged(
            &repo,
            &cache,
            Some("release".to_string()),
            raw_q,
            raw_limit,
            Some(20),
        ));
        assert_eq!(resp.status, 200, "q={raw_q:?}");
    }
    assert_eq!(
        *repo.search_calls.borrow(),
        1,
        "sibling spellings must be KV hits, not new scans"
    );
    assert_eq!(*cache.puts.borrow(), 1, "one KV entry for one search");
    assert_eq!(cache.stored.borrow().len(), 1);
    assert_eq!(*cache.gets.borrow(), 3);
}

/// The vector target's unchanged-URL set is exactly what the handler does
/// not serve as a cacheable search success: the 400s, plus the above-cap
/// synthetic page (200, but zero cache access — there is no stored body
/// an edge hit may replay for a different offset).
#[test]
fn unserved_set_matches_the_handlers_reject_and_short_circuit_sets() {
    let long_q = "a".repeat(101);
    // (raw q, raw type, raw limit) -> handler must 400 with no cache use.
    let reject: Vec<(String, Option<String>, Option<u32>)> = vec![
        (String::new(), Some("artist".to_string()), None),
        ("  ".to_string(), Some("artist".to_string()), None),
        ("x".to_string(), Some("artist".to_string()), None),
        (long_q, Some("artist".to_string()), None),
        ("ok\u{7}query".to_string(), Some("artist".to_string()), None),
        ("blue monday".to_string(), None, None),
        ("blue monday".to_string(), Some("track".to_string()), None),
        ("blue monday".to_string(), Some("ARTIST".to_string()), None),
        (
            "blue monday".to_string(),
            Some("artist".to_string()),
            Some(0),
        ),
    ];
    for (raw_q, raw_type, raw_limit) in reject {
        let repo = CountingRepo::default();
        let cache = FakeCache::default();
        let resp = block_on(handle_search_paged(
            &repo,
            &cache,
            raw_type.clone(),
            &raw_q,
            raw_limit,
            None,
        ));
        assert_eq!(resp.status, 400, "q={raw_q:?} type={raw_type:?}");
        assert_eq!(resp.cache_ttl, None, "q={raw_q:?}");
        assert_eq!(*repo.search_calls.borrow(), 0, "q={raw_q:?}");
        assert_eq!(*cache.gets.borrow(), 0, "q={raw_q:?}");
        assert_eq!(*cache.puts.borrow(), 0, "q={raw_q:?}");
    }

    // Above-cap offset: 200 empty page, still zero cache access.
    let repo = CountingRepo::default();
    let cache = FakeCache::default();
    let resp = block_on(handle_search_paged(
        &repo,
        &cache,
        Some("artist".to_string()),
        "blue monday",
        Some(10),
        Some(MAX_SEARCH_OFFSET + 1),
    ));
    assert_eq!(resp.status, 200);
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    assert_eq!(body["results"].as_array().unwrap().len(), 0);
    assert_eq!(body["offset"], (MAX_SEARCH_OFFSET + 1) as u64);
    assert_eq!(*repo.search_calls.borrow(), 0);
    assert_eq!(*cache.gets.borrow(), 0);
    assert_eq!(*cache.puts.borrow(), 0);
}
