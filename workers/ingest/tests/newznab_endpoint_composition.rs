//! Saved indexer API-endpoint composition pins
//! (`workers/ingest/src/newznab.rs`, public `build_search_url` seam).
//!
//! The BYOP vault accepts a saved indexer URL that already ends at the
//! Newznab API endpoint (e.g. `https://news.example.com/api`), but the
//! search-URL builder historically appended `/api` unconditionally, so a
//! valid saved endpoint composed to `/api/api` and the configured endpoint
//! was never queried.
//!
//! Contract under test:
//!
//! - A bare origin (with or without a trailing slash) composes to exactly
//!   one `/api` endpoint.
//! - An input whose final path segment is exactly `api` (with or without
//!   trailing slashes) is preserved as-is: it never becomes `/api/api`.
//! - A deployment prefix composes (`/newznab` -> `/newznab/api`), and an
//!   existing prefixed endpoint is preserved (`/newznab/api` stays
//!   `/newznab/api`). Deeper prefixes are kept verbatim.
//! - Only an exact, lowercase, final `api` path segment counts. Segments
//!   that merely contain or end with the letters (`capi`, `apiary`,
//!   `apis`), a different-case segment (`API`), an `api` segment that is
//!   not final (`/api/newznab`), and an `api` host label all still get
//!   `/api` appended — no broad arbitrary URL rewriting.
//! - Every pre-existing guard still applies to endpoint-shaped inputs:
//!   HTTPS only, no embedded credentials, no query/fragment, no host-less
//!   URL, and the query/limit/offset/category caps. The built URL stays
//!   key-free (no `apikey`): key injection remains a separate seam.
//!
//! Tests-only: no `src/` change. Synthetic hosts only; no network.

use nntune_ingest::newznab::{
    build_search_url, SearchParams, MAX_CATEGORIES, MAX_LIMIT, MAX_OFFSET,
};

/// Query string every successful build below must carry verbatim.
const QUERY: &str = "?t=music&o=json&q=dark%20side%20moon&limit=25&offset=50&cat=3000&cat=3040";

fn params<'a>(
    base_url: &'a str,
    query: &'a str,
    categories: &'a [u32],
    limit: u32,
    offset: u32,
) -> SearchParams<'a> {
    SearchParams {
        base_url,
        query,
        categories,
        limit,
        offset,
    }
}

/// Build with the shared fixture query/cats/window and demand success.
fn built(base_url: &str) -> String {
    build_search_url(&params(base_url, "dark side moon", &[3000, 3040], 25, 50))
        .unwrap_or_else(|e| panic!("base {base_url:?} must build, got error: {e}"))
}

#[test]
fn bare_origin_and_trailing_slash_origin_produce_one_api_endpoint() {
    for base in [
        "https://news.example.com",
        "https://news.example.com/",
        "https://news.example.com:8443",
        "https://news.example.com:8443/",
    ] {
        let url = built(base);
        let origin = base.trim_end_matches('/');
        assert_eq!(url, format!("{origin}/api{QUERY}"), "base {base:?}");
        assert!(!url.contains("/api/api"), "base {base:?}: {url}");
    }
}

#[test]
fn saved_api_endpoint_is_preserved_without_doubling() {
    for base in [
        "https://news.example.com/api",
        "https://news.example.com/api/",
        "https://news.example.com/api//",
        "https://news.example.com:8443/api",
        "https://news.example.com:8443/api/",
    ] {
        let url = built(base);
        let endpoint = base.trim_end_matches('/');
        assert_eq!(url, format!("{endpoint}{QUERY}"), "base {base:?}");
        assert!(!url.contains("/api/api"), "base {base:?}: {url}");
        assert_eq!(
            url.matches("/api").count(),
            1,
            "base {base:?} must keep exactly one /api segment: {url}"
        );
    }
}

#[test]
fn deployment_prefix_composes_and_existing_prefixed_api_is_preserved() {
    // Prefix without the endpoint: `/api` is appended after the prefix.
    for base in [
        "https://news.example.com/newznab",
        "https://news.example.com/newznab/",
        "https://news.example.com/newznab-v2",
        "https://news.example.com/a/b/c",
        "https://news.example.com/a/b/c/",
    ] {
        let url = built(base);
        let prefix = base.trim_end_matches('/');
        assert_eq!(url, format!("{prefix}/api{QUERY}"), "base {base:?}");
    }
    // Prefix that already ends at the endpoint: preserved verbatim.
    for base in [
        "https://news.example.com/newznab/api",
        "https://news.example.com/newznab/api/",
        "https://news.example.com/a/b/api",
        "https://news.example.com/a/b/api/",
    ] {
        let url = built(base);
        let endpoint = base.trim_end_matches('/');
        assert_eq!(url, format!("{endpoint}{QUERY}"), "base {base:?}");
        assert!(!url.contains("/api/api"), "base {base:?}: {url}");
    }
}

#[test]
fn only_an_exact_final_lowercase_api_segment_is_preserved() {
    // None of these end at the API endpoint, so each must still gain
    // exactly one trailing `/api` segment, with the rest of the URL kept
    // verbatim (no case folding, no substring or suffix matching).
    for base in [
        "https://news.example.com/capi",
        "https://news.example.com/apiary",
        "https://news.example.com/apis",
        "https://news.example.com/API",
        "https://news.example.com/newznab/API",
        "https://news.example.com/api/newznab",
        "https://api.news.example.com",
        "https://api.news.example.com/",
    ] {
        let url = built(base);
        let prefix = base.trim_end_matches('/');
        assert_eq!(url, format!("{prefix}/api{QUERY}"), "base {base:?}");
    }
}

#[test]
fn preserved_endpoint_keeps_the_full_key_free_query_shape() {
    // The exact, complete URL for a saved endpoint: query encoding,
    // pagination, and category order are unchanged, and no key material
    // of any kind is introduced by composition.
    let url = built("https://news.example.com/newznab/api/");
    assert_eq!(
        url,
        "https://news.example.com/newznab/api\
         ?t=music&o=json&q=dark%20side%20moon&limit=25&offset=50&cat=3000&cat=3040"
    );
    assert!(!url.to_ascii_lowercase().contains("apikey"), "{url}");
    assert!(!url.contains("/api/api"), "{url}");
}

#[test]
fn unsafe_bases_still_reject_even_when_they_end_in_api() {
    for base in [
        // HTTPS only: the key would travel in cleartext.
        "http://news.example.com/api",
        "http://news.example.com/newznab/api",
        // Embedded credentials never ride a saved base URL.
        "https://user:pass@news.example.com/api",
        "https://user@news.example.com/newznab/api",
        // A pasted query/fragment must not smuggle a key past the vault
        // flow or swallow the search query, endpoint-shaped or not.
        "https://news.example.com/api?apikey=secret",
        "https://news.example.com/api?t=music",
        "https://news.example.com/newznab/api#frag",
        "https://news.example.com/api#frag",
        // Whitespace and host-less URLs stay rejected.
        "https://news.example.com/new znab/api",
        "https:///api",
        "https://",
    ] {
        assert!(
            build_search_url(&params(base, "x", &[], 25, 0)).is_err(),
            "base {base:?} must reject"
        );
    }
}

#[test]
fn query_pagination_and_category_caps_still_apply_to_saved_endpoints() {
    // Caps are enforced identically when the base already ends in `/api`:
    // preservation must not become an early-return bypass.
    for base in [
        "https://news.example.com/api",
        "https://news.example.com/newznab/api/",
    ] {
        assert_eq!(
            build_search_url(&params(base, "", &[], 25, 0)).err(),
            Some("query must be 1..=200 chars"),
            "base {base:?}"
        );
        assert_eq!(
            build_search_url(&params(base, &"x".repeat(201), &[], 25, 0)).err(),
            Some("query must be 1..=200 chars"),
            "base {base:?}"
        );
        for limit in [0, MAX_LIMIT + 1] {
            assert_eq!(
                build_search_url(&params(base, "x", &[], limit, 0)).err(),
                Some("limit must be 1..=100"),
                "base {base:?} limit {limit}"
            );
        }
        assert_eq!(
            build_search_url(&params(base, "x", &[], 25, MAX_OFFSET + 1)).err(),
            Some("offset exceeds the paged search window"),
            "base {base:?}"
        );
        let too_many: Vec<u32> = (0..(MAX_CATEGORIES as u32 + 1)).collect();
        assert_eq!(
            build_search_url(&params(base, "x", &too_many, 25, 0)).err(),
            Some("too many categories (max 16)"),
            "base {base:?}"
        );
    }
}

#[test]
fn cap_boundaries_still_build_on_saved_endpoints() {
    // The flip side of the cap pins: at-cap inputs on a preserved endpoint
    // build the same URL they would on a bare origin.
    let sixteen: Vec<u32> = (0..MAX_CATEGORIES as u32).collect();
    let url = build_search_url(&params(
        "https://news.example.com/api/",
        &"x".repeat(200),
        &sixteen,
        MAX_LIMIT,
        MAX_OFFSET,
    ))
    .expect("at-cap inputs on a saved endpoint must build");
    assert!(
        url.starts_with("https://news.example.com/api?t=music&o=json&q="),
        "{url}"
    );
    assert!(url.contains("&limit=100&offset=10000"), "{url}");
    assert!(!url.contains("/api/api"), "{url}");
    for cat in &sixteen {
        assert!(url.contains(&format!("&cat={cat}")), "cat {cat}: {url}");
    }
}
