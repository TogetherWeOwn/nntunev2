//! Newznab category-union serialization (`workers/ingest/src/newznab.rs`).
//!
//! Contract under test (Newznab MUSIC-SEARCH: `cat` is a comma-delimited
//! list of categories, i.e. one parameter carrying the union):
//!
//! - Two categories produce exactly ONE decoded `cat` value, `3000,3040`,
//!   preserving the requested OR/union semantics. A percent-encoded comma
//!   (`3000%2C3040`) decodes to the same value and is equally valid, so
//!   every assertion below compares percent-decoded query values rather
//!   than raw substrings. Repeated `&cat=3000&cat=3040` parameters are the
//!   defect: scalar query parsers can discard all but one of them.
//! - One category produces one `cat` value with no comma; an empty list
//!   omits `cat` entirely; list order is preserved verbatim (no sorting
//!   or deduplication); the `MAX_CATEGORIES` refusal is unchanged.
//! - Pagination, the Unicode-scalar query limit, key injection /
//!   redaction, and endpoint composition are unchanged, including when a
//!   category union rides the same URL.
//!
//! Tests only, through the public seam
//! (`nntune_ingest::newznab::{build_search_url, SearchParams, ..}`) with
//! synthetic fixtures. `build_search_url` is pure: no network, no keys.

use nntune_ingest::newznab::{
    build_search_url, inject_api_key, redact_api_key, SearchParams, MAX_CATEGORIES,
};

fn params<'a>(query: &'a str, categories: &'a [u32], limit: u32, offset: u32) -> SearchParams<'a> {
    SearchParams {
        base_url: "https://indexer.example",
        query,
        categories,
        limit,
        offset,
    }
}

/// Percent-decode one query value (`%2C` -> `,`, `%20` -> ` `, `%C3%A9`
/// -> `é`). Invalid escapes pass through bare; the URLs under test are
/// built by `build_search_url`, which always emits valid escapes.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).expect("hex digits are ASCII");
            out.push(u8::from_str_radix(hex, 16).expect("two hex digits parse"));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("decoded query value is UTF-8")
}

/// All percent-decoded values of query parameter `name`, in URL order.
/// A repeated parameter yields more than one entry — exactly the shape
/// the union contract forbids for `cat`.
fn decoded_values(url: &str, name: &str) -> Vec<String> {
    let query = url
        .split_once('?')
        .unwrap_or_else(|| panic!("url has no query string: {url}"))
        .1;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(key, _)| *key == name)
        .map(|(_, value)| percent_decode(value))
        .collect()
}

#[test]
fn two_categories_produce_exactly_one_decoded_cat_value() {
    // The production change that makes this fail: emitting the union as
    // repeated `&cat=` parameters (or any shape other than one
    // comma-delimited decoded value).
    let url = build_search_url(&params("kind of blue", &[3000, 3040], 25, 0))
        .expect("two categories must build");
    assert_eq!(
        decoded_values(&url, "cat"),
        vec!["3000,3040".to_string()],
        "the union must ride one decoded cat value; url: {url}"
    );
}

#[test]
fn category_order_is_preserved_verbatim_in_the_union() {
    let url = build_search_url(&params("kind", &[3040, 3000, 3030], 25, 0))
        .expect("three categories must build");
    assert_eq!(
        decoded_values(&url, "cat"),
        vec!["3040,3000,3030".to_string()],
        "no sorting or deduplication of the requested union; url: {url}"
    );
    // A repeated category is preserved too: the builder serializes the
    // requested list, it does not editorialize it.
    let url = build_search_url(&params("kind", &[3000, 3000], 25, 0))
        .expect("a repeated category must build");
    assert_eq!(
        decoded_values(&url, "cat"),
        vec!["3000,3000".to_string()],
        "duplicates must not be deduplicated; url: {url}"
    );
}

#[test]
fn one_category_produces_one_cat_value_without_a_comma() {
    let url = build_search_url(&params("kind", &[3040], 25, 0)).expect("one category must build");
    assert_eq!(
        decoded_values(&url, "cat"),
        vec!["3040".to_string()],
        "url: {url}"
    );
}

#[test]
fn empty_category_list_omits_cat_entirely() {
    let url = build_search_url(&params("kind", &[], 25, 0)).expect("no categories must build");
    assert!(
        decoded_values(&url, "cat").is_empty(),
        "no cat parameter (not even an empty one) may ride the URL: {url}"
    );
}

#[test]
fn max_categories_join_into_one_value_and_one_over_is_refused() {
    assert_eq!(MAX_CATEGORIES, 16);
    let sixteen: Vec<u32> = (3000..3016).collect();
    let url = build_search_url(&params("kind", &sixteen, 25, 0)).expect("16 categories must build");
    let joined = sixteen
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        decoded_values(&url, "cat"),
        vec![joined],
        "all 16 categories ride one decoded cat value in order; url: {url}"
    );

    let seventeen: Vec<u32> = (3000..3017).collect();
    assert_eq!(
        build_search_url(&params("kind", &seventeen, 25, 0)).err(),
        Some("too many categories (max 16)")
    );
}

#[test]
fn union_leaves_endpoint_pagination_and_query_untouched() {
    let url = build_search_url(&SearchParams {
        base_url: "https://indexer.example/",
        query: "kind of blue",
        categories: &[3000, 3040],
        limit: 10,
        offset: 20,
    })
    .expect("paged union search must build");
    assert!(
        url.starts_with("https://indexer.example/api?"),
        "endpoint composition unchanged (trailing slash trimmed): {url}"
    );
    assert_eq!(decoded_values(&url, "t"), vec!["music".to_string()]);
    assert_eq!(decoded_values(&url, "o"), vec!["json".to_string()]);
    assert_eq!(
        decoded_values(&url, "q"),
        vec!["kind of blue".to_string()],
        "the raw query still rides percent-encoded; url: {url}"
    );
    assert_eq!(decoded_values(&url, "limit"), vec!["10".to_string()]);
    assert_eq!(decoded_values(&url, "offset"), vec!["20".to_string()]);
    assert_eq!(
        decoded_values(&url, "cat"),
        vec!["3000,3040".to_string()],
        "url: {url}"
    );
    assert!(
        decoded_values(&url, "apikey").is_empty(),
        "the bare URL never carries a key: {url}"
    );
}

#[test]
fn union_leaves_unicode_query_limit_untouched() {
    // 200 Unicode scalars (400 UTF-8 bytes) build; 201 reject with the
    // exact message — with a category union riding both attempts.
    let exact = "\u{e9}".repeat(200);
    let over = "\u{e9}".repeat(201);
    let url = build_search_url(&params(&exact, &[3000, 3040], 25, 0))
        .expect("200 scalars must be accepted");
    assert_eq!(decoded_values(&url, "q"), vec![exact], "url: {url}");
    assert_eq!(
        decoded_values(&url, "cat"),
        vec!["3000,3040".to_string()],
        "url: {url}"
    );
    assert_eq!(
        build_search_url(&params(&over, &[3000, 3040], 25, 0)).err(),
        Some("query must be 1..=200 chars")
    );
}

#[test]
fn union_survives_key_injection_and_redaction() {
    let bare =
        build_search_url(&params("kind", &[3000, 3040], 25, 0)).expect("union search must build");
    let full = inject_api_key(&bare, "synthetic-key-1").expect("synthetic key must inject");
    assert_eq!(
        decoded_values(&full, "apikey"),
        vec!["synthetic-key-1".to_string()],
        "url: {full}"
    );
    assert_eq!(
        decoded_values(&full, "cat"),
        vec!["3000,3040".to_string()],
        "injecting the key must not reshape the union; url: {full}"
    );

    let redacted = redact_api_key(&full);
    assert!(
        !redacted.contains("synthetic-key-1"),
        "the key must be scrubbed: {redacted}"
    );
    assert_eq!(
        decoded_values(&redacted, "apikey"),
        vec!["***".to_string()],
        "url: {redacted}"
    );
    assert_eq!(
        decoded_values(&redacted, "cat"),
        vec!["3000,3040".to_string()],
        "redaction scrubs only the key value, never the union; url: {redacted}"
    );
}
