//! Newznab paged-window and saturating page-offset pins
//! (`workers/ingest/src/newznab.rs`).
//!
//! Contract under test:
//!
//! - `limit` 0 and 101 reject with `limit must be 1..=100`;
//!   `MAX_LIMIT == 100`.
//! - `offset` 10_000 builds; 10_001 rejects with
//!   `offset exceeds the paged search window`; `MAX_OFFSET == 10_000`
//!   (`MAX_PAGE * MAX_LIMIT`, with `MAX_PAGE == 100`).
//! - 16 categories build; 17 reject with `too many categories (max 16)`;
//!   `MAX_CATEGORIES == 16`.
//! - [`page_offset`] saturates: `page_offset(u32::MAX, u32::MAX) == u32::MAX`
//!   (a wrapping/`checked` mutant overflows or panics instead).
//!
//! Tests-only pin: no `src/` change. Each bound is sensitivity-checked by
//! mutating it (0/101 limit, 10_001 offset, 17 categories, wrapping offset)
//! and observing a failure.

use nntune_ingest::newznab::{
    build_search_url, page_offset, SearchParams, MAX_CATEGORIES, MAX_LIMIT, MAX_OFFSET, MAX_PAGE,
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

#[test]
fn limit_zero_and_one_over_max_reject_with_exact_message() {
    assert_eq!(MAX_LIMIT, 100);
    for limit in [0, 101] {
        assert_eq!(
            build_search_url(&params("x", &[], limit, 0)).err(),
            Some("limit must be 1..=100"),
            "limit {limit} must reject with the exact message"
        );
    }
    // Boundaries on either side still build.
    assert!(build_search_url(&params("x", &[], 1, 0)).is_ok());
    assert!(build_search_url(&params("x", &[], MAX_LIMIT, 0)).is_ok());
}

#[test]
fn offset_window_boundary_builds_at_cap_and_rejects_one_past() {
    assert_eq!(MAX_PAGE, 100);
    assert_eq!(MAX_OFFSET, 10_000);
    assert_eq!(MAX_OFFSET, MAX_PAGE * MAX_LIMIT);
    assert!(build_search_url(&params("x", &[], 25, 10_000)).is_ok());
    assert_eq!(
        build_search_url(&params("x", &[], 25, 10_001)).err(),
        Some("offset exceeds the paged search window")
    );
}

#[test]
fn category_cap_boundary_builds_at_sixteen_and_rejects_at_seventeen() {
    assert_eq!(MAX_CATEGORIES, 16);
    let sixteen: Vec<u32> = (0..16).collect();
    let seventeen: Vec<u32> = (0..17).collect();
    let url = build_search_url(&params("x", &sixteen, 25, 0)).expect("16 categories must build");
    // The union rides one comma-delimited `cat` value (Newznab MUSIC-SEARCH);
    // repeated `&cat=` params are the defect (scalar parsers keep one).
    let joined = sixteen
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        url.matches("&cat=").count(),
        1,
        "union must ride one cat value: {url}"
    );
    assert!(
        url.contains(&format!("&cat={joined}")),
        "all 16 categories must ride the union in order: {url}"
    );
    assert_eq!(
        build_search_url(&params("x", &seventeen, 25, 0)).err(),
        Some("too many categories (max 16)")
    );
}

#[test]
fn page_offset_saturates_at_u32_max() {
    assert_eq!(page_offset(2, 25), 50);
    assert_eq!(page_offset(MAX_PAGE, MAX_LIMIT), MAX_OFFSET);
    assert_eq!(page_offset(u32::MAX, u32::MAX), u32::MAX);
}
