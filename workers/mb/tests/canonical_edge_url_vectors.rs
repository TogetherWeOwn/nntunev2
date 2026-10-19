//! Vector table for `canonical_edge_url` — the canonical edge-cache key.
//!
//! Behavioral half of the canonical-request pin; the Builder contract and
//! the get/put wiring pins live in `canonical_edge_cache_acceptance.rs`.
//! This target imports the wished-for helper directly, so on a tree
//! without it the target is RED with `E0432` naming the missing
//! `canonical_edge_url` — the card's stated RED ("no `canonical_*` helper
//! exists at `workers/mb/src/lib.rs`"). The Builder adds
//!
//! ```ignore
//! pub fn canonical_edge_url(url: &str) -> String
//! ```
//!
//! to the transport-neutral core of `workers/mb/src/lib.rs` (above
//! `mod http_layer`) and this target compiles; every vector below must
//! then pass WITHOUT editing this file.
//!
//! ## The contract, exactly
//!
//! Input: the absolute request URL string exactly as `edge_hit`/`serve`
//! see it. Output, two cases:
//!
//! - A SERVED SEARCH — path exactly `/v1/mb/search` AND the query decodes
//!   (form-urlencoded query parsing, first value per key — mirroring the
//!   `HashMap` collection in `search_route`, where duplicates collapse
//!   unpredictably and are out of contract) to parameters the handler
//!   would serve as a cacheable search:
//!   - `q` present, non-empty once trimmed, 2..=100 chars (chars, not
//!     bytes, counted on the trimmed value — the handler's rule), no
//!     control characters;
//!   - `type` exactly one of `artist|release|recording`;
//!   - `limit` absent, unparsable as `u32`, or non-zero (zero is the
//!     handler's 400; `+10`/` 10` spellings do not parse as `u32` and fall
//!     back exactly as `search_route` falls back);
//!   - `offset` absent, unparsable as `u32`, or at or below 1000
//!     (`MAX_SEARCH_OFFSET`).
//!
//!   These rewrite to
//!   `<scheme>://<host>/v1/mb/search?q=<encoded normalized q>&type=<kind>&limit=<effective limit>&offset=<effective offset>`
//!   where `<scheme>://<host>` carries over from the input (host including
//!   a non-default port), the normalized `q` is `normalize_query`'s rule
//!   (split-whitespace, single-space join, lowercased), the encoding is
//!   WHATWG `application/x-www-form-urlencoded` (space as `+`, everything
//!   outside `[A-Za-z0-9*._-]` as uppercase-hex `%XX` — the same encoding
//!   the canonical output must decode back through), the effective limit
//!   is 25 when absent/unparsable/above 25 (`MAX_PAGE_SIZE`), the
//!   effective offset is 0 when absent/unparsable, parameters appear in
//!   exactly that order, and unknown parameters are dropped.
//! - EVERYTHING ELSE returns the input unchanged, byte for byte: other
//!   paths, unparsable input (including a search URL whose query is not
//!   valid UTF-8 once percent-decoded), and every search outside the
//!   served set. This is a safety wall, not a leftover: `edge_hit` runs
//!   before validation, so canonicalizing a request the handler would
//!   400 could serve it a cached 200, and canonicalizing an above-cap
//!   offset onto a capped offset's key would serve one page's body for
//!   another offset.
//!
//! Standalone integration target: no existing suite is edited.

use nntune_mb::canonical_edge_url;

fn assert_canonical(raw: &str, expected: &str) {
    assert_eq!(canonical_edge_url(raw), expected, "raw url {raw:?}");
}

/// Every raw spelling in a group must land on the group's one canonical
/// URL — this equality across spellings IS the shared edge entry. The
/// canonical spelling itself is a fixed point, so get and put meet on
/// the served spelling.
fn assert_group(raw_spellings: &[&str], canonical: &str) {
    for raw in raw_spellings {
        assert_canonical(raw, canonical);
    }
    assert_canonical(canonical, canonical);
}

#[test]
fn q_case_and_whitespace_spellings_share_one_edge_key() {
    assert_group(
        &[
            // Case: the KV layer lowercases `q`; the edge key must too.
            "https://mb.test/v1/mb/search?q=Blue+Monday&type=release",
            "https://mb.test/v1/mb/search?q=BLUE+MONDAY&type=release",
            "https://mb.test/v1/mb/search?q=blue+monday&type=release",
            // Surrounding and repeated whitespace collapse exactly like
            // `normalize_query` (split_whitespace + single-space join);
            // `+`, `%20` and `%09` are all whitespace once decoded.
            "https://mb.test/v1/mb/search?q=++Blue+++Monday++&type=release",
            "https://mb.test/v1/mb/search?q=%20Blue%20%20%20Monday%09&type=release",
            // Percent-encoded letters decode to the same normalized text.
            "https://mb.test/v1/mb/search?q=%62lue%20monday&type=release",
        ],
        "https://mb.test/v1/mb/search?q=blue+monday&type=release&limit=25&offset=0",
    );
    // A UTF-8 query lowercases and re-encodes byte-wise (uppercase hex).
    assert_group(
        &[
            "https://mb.test/v1/mb/search?q=CAF%C3%89+M%C3%BCnchen&type=release",
            "https://mb.test/v1/mb/search?q=Caf%C3%A9%20M%C3%BCnchen&type=release",
        ],
        "https://mb.test/v1/mb/search?q=caf%C3%A9+m%C3%BCnchen&type=release&limit=25&offset=0",
    );
}

#[test]
fn parameter_order_and_unknown_params_share_one_edge_key() {
    assert_group(
        &[
            "https://mb.test/v1/mb/search?type=artist&offset=20&limit=10&q=blue+monday",
            "https://mb.test/v1/mb/search?offset=20&q=Blue+Monday&limit=10&type=artist",
            // Unknown parameters are not part of the search identity: the
            // handler ignores them, so they must not split the edge entry.
            "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=20&utm=x&debug=1",
            "https://mb.test/v1/mb/search?foo=bar&q=blue+monday&type=artist&limit=10&offset=20",
        ],
        "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=20",
    );
}

#[test]
fn limit_forms_share_the_effective_limit_edge_key() {
    // Default + clamp group: absent, empty/unparsable, exactly the page
    // size and everything above it all serve `limit: 25` — one key.
    assert_group(
        &[
            "https://mb.test/v1/mb/search?q=blue+monday&type=recording",
            "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=",
            "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=abc",
            "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=-5",
            "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=25",
            "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=26",
            "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=1000",
            "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=4294967295",
        ],
        "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=25&offset=0",
    );
    // In-range forms: leading zeros are the same `u32` the handler parses.
    assert_group(
        &[
            "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=10",
            "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=010",
            "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=00010",
        ],
        "https://mb.test/v1/mb/search?q=blue+monday&type=recording&limit=10&offset=0",
    );
}

#[test]
fn offset_forms_share_the_effective_offset_edge_key() {
    // Default group: absent, empty/unparsable, `0` and zero-padded `0`
    // all serve the first page.
    assert_group(
        &[
            "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10",
            "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=",
            "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=abc",
            "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=0",
            "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=000",
        ],
        "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=0",
    );
    // In-range forms: leading zeros; the cap itself is a served page.
    assert_group(
        &[
            "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=20",
            "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=020",
        ],
        "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=20",
    );
    assert_group(
        &[
            "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=1000",
            "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=01000",
        ],
        "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=1000",
    );
}

#[test]
fn all_four_axes_at_once_share_one_edge_key() {
    // The acceptance vector: `q` case, param order, limit form and
    // offset form varied together must still be one edge entry.
    assert_group(
        &[
            "https://mb.test/v1/mb/search?q=blue+monday&type=release&limit=10&offset=20",
            "https://mb.test/v1/mb/search?offset=020&limit=010&type=release&q=BLUE++Monday",
            "https://mb.test/v1/mb/search?type=release&q=%20Blue%20Monday%20&offset=20&limit=10",
        ],
        "https://mb.test/v1/mb/search?q=blue+monday&type=release&limit=10&offset=20",
    );
}

#[test]
fn host_and_scheme_carry_over_to_the_canonical_key() {
    // The canonical key stays in the request's own namespace: a staging
    // or local origin must never be rewritten onto another host's entry.
    assert_canonical(
        "https://mb-staging.test/v1/mb/search?q=Blue+Monday&type=artist",
        "https://mb-staging.test/v1/mb/search?q=blue+monday&type=artist&limit=25&offset=0",
    );
    assert_canonical(
        "http://localhost:8787/v1/mb/search?q=Blue+Monday&type=artist&limit=010",
        "http://localhost:8787/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=0",
    );
}

#[test]
fn encoding_hostiles_in_q_never_merge_distinct_searches() {
    // A literal plus inside the decoded query is not a space: "a+b" and
    // "a b" are different searches and must key apart. The canonical
    // form keeps the literal plus percent-encoded so it can never
    // collide with the space spelling.
    assert_group(
        &["https://mb.test/v1/mb/search?q=a%2Bb&type=artist"],
        "https://mb.test/v1/mb/search?q=a%2Bb&type=artist&limit=25&offset=0",
    );
    assert_group(
        &["https://mb.test/v1/mb/search?q=a+b&type=artist"],
        "https://mb.test/v1/mb/search?q=a+b&type=artist&limit=25&offset=0",
    );
    assert_ne!(
        canonical_edge_url("https://mb.test/v1/mb/search?q=a%2Bb&type=artist"),
        canonical_edge_url("https://mb.test/v1/mb/search?q=a+b&type=artist"),
        "\"a+b\" and \"a b\" are different searches and must key apart"
    );
    // A percent-encoded slash or question mark inside `q` is query text,
    // never structure: it comes back encoded, not smuggled into the
    // canonical URL as a path or query separator.
    assert_canonical(
        "https://mb.test/v1/mb/search?q=ac%2Fdc%3F&type=artist",
        "https://mb.test/v1/mb/search?q=ac%2Fdc%3F&type=artist&limit=25&offset=0",
    );
    // Likewise an encoded `&`/`=` inside `q` cannot split into a forged
    // extra parameter in the canonical spelling.
    assert_canonical(
        "https://mb.test/v1/mb/search?q=a%26limit%3D1&type=artist&limit=10",
        "https://mb.test/v1/mb/search?q=a%26limit%3D1&type=artist&limit=10&offset=0",
    );
}

// ---------------------------------------------------------------------------
// The no-broadening wall: requests outside the served search set keep
// their raw URL, byte for byte
// ---------------------------------------------------------------------------

#[test]
fn unserved_requests_are_returned_unchanged() {
    let long_q = "a".repeat(101);
    let unchanged: Vec<String> = vec![
        // Other routes keep raw-URL edge keys (only search canonicalizes).
        "https://mb.test/healthz".to_string(),
        "https://mb.test/v1/mb/artist/123E4567-E89B-12D3-A456-426614174000".to_string(),
        "https://mb.test/v1/mb/release/123e4567-e89b-12d3-a456-426614174000/recordings".to_string(),
        // Near-miss paths are not the search route.
        "https://mb.test/v1/mb/search/?q=blue+monday&type=artist".to_string(),
        "https://mb.test/v1/mb/searchx?q=blue+monday&type=artist".to_string(),
        // Unparsable inputs pass through untouched, including a query
        // that is not valid UTF-8 once percent-decoded.
        "not a url".to_string(),
        String::new(),
        "https://mb.test/v1/mb/search?q=%FF%FE&type=artist".to_string(),
        // The handler-400 set: canonicalizing any of these onto a served
        // key would let `edge_hit` answer a request validation rejects.
        "https://mb.test/v1/mb/search?type=artist".to_string(),
        "https://mb.test/v1/mb/search?q=&type=artist".to_string(),
        "https://mb.test/v1/mb/search?q=++&type=artist".to_string(),
        "https://mb.test/v1/mb/search?q=x&type=artist".to_string(),
        // One char after trimming is still too short, however padded.
        "https://mb.test/v1/mb/search?q=+x+&type=artist".to_string(),
        format!("https://mb.test/v1/mb/search?q={long_q}&type=artist"),
        "https://mb.test/v1/mb/search?q=ok%07query&type=artist".to_string(),
        "https://mb.test/v1/mb/search?q=blue+monday".to_string(),
        "https://mb.test/v1/mb/search?q=blue+monday&type=track".to_string(),
        "https://mb.test/v1/mb/search?q=blue+monday&type=ARTIST".to_string(),
        "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=0".to_string(),
        "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=00".to_string(),
        // Above the offset cap the handler serves a synthetic empty page
        // that never touches KV; keying it on a capped offset would
        // serve a real page's body for the wrong offset (and vice versa).
        "https://mb.test/v1/mb/search?q=blue+monday&type=artist&offset=1001".to_string(),
        "https://mb.test/v1/mb/search?q=blue+monday&type=artist&offset=4294967295".to_string(),
    ];
    for raw in &unchanged {
        let expected = raw.clone();
        assert_canonical(raw, &expected);
    }
}

#[test]
fn boundary_spellings_just_inside_the_served_set_do_canonicalize() {
    // Exactly 2 and exactly 100 chars canonicalize; the wall above holds
    // the 1- and 101-char spellings out.
    assert_canonical(
        "https://mb.test/v1/mb/search?q=AB&type=artist",
        "https://mb.test/v1/mb/search?q=ab&type=artist&limit=25&offset=0",
    );
    let q100 = "A".repeat(100);
    assert_canonical(
        &format!("https://mb.test/v1/mb/search?q={q100}&type=artist"),
        &format!(
            "https://mb.test/v1/mb/search?q={}&type=artist&limit=25&offset=0",
            "a".repeat(100)
        ),
    );
    // A padded 100-char query (102 raw chars) is counted trimmed, like
    // the handler counts it, so it canonicalizes too.
    assert_canonical(
        &format!("https://mb.test/v1/mb/search?q=+{q100}+&type=artist"),
        &format!(
            "https://mb.test/v1/mb/search?q={}&type=artist&limit=25&offset=0",
            "a".repeat(100)
        ),
    );
    // Smallest served limit.
    assert_canonical(
        "https://mb.test/v1/mb/search?q=ab&type=artist&limit=1",
        "https://mb.test/v1/mb/search?q=ab&type=artist&limit=1&offset=0",
    );
}

#[test]
fn distinct_searches_stay_distinct() {
    let base = canonical_edge_url(
        "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=20",
    );
    for other in [
        // Different query text, kind or page coordinates are different
        // searches/pages: one page must never serve another's body.
        "https://mb.test/v1/mb/search?q=blue+tuesday&type=artist&limit=10&offset=20",
        "https://mb.test/v1/mb/search?q=blue+monday&type=release&limit=10&offset=20",
        "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=11&offset=20",
        "https://mb.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=21",
        // A different host is a different cache namespace.
        "https://other.test/v1/mb/search?q=blue+monday&type=artist&limit=10&offset=20",
    ] {
        assert_ne!(canonical_edge_url(other), base, "{other:?} must key apart");
    }
}
