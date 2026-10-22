//! Test-only native REST surface contract; fixtures and identifiers are synthetic.
//! Builder must not edit this acceptance file. Expected failures are assertion-RED,
//! not missing APIs: use the existing router and authorization entry points.

use nntune_api::native_rest::{scope_for_operation, RouteDecision, Scope};

/// Pinned copy of the preflight method grant: exactly the methods the
/// scope map serves, plus the OPTIONS self-grant. The suite — not the
/// entry symbol — owns this list, so a silent grant change turns red.
/// If the builder replaces the static grant with per-route
/// advertisement, this pin moves to the per-route source of truth.
const PINNED_PREFLIGHT_METHODS: [&str; 7] =
    ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"];

const ENTRY_SRC: &str = include_str!("../src/worker_entry.rs");

/// The `access-control-allow-methods` value the entry renders on a 204
/// preflight, parsed out of the entry source (the grant lives in private
/// `cors_headers`, so the contract is pinned at the literal).
fn advertised_preflight_methods() -> Vec<String> {
    let anchor = ENTRY_SRC
        .find("access-control-allow-methods")
        .expect("entry must name the preflight methods grant");
    let mut methods = None;
    for line in ENTRY_SRC[anchor..].lines().take(8).skip(1) {
        let line = line.trim();
        if !(line.starts_with('"') && line.contains(',')) {
            continue;
        }
        let literal = line
            .strip_prefix('"')
            .and_then(|rest| rest.find('"').map(|end| &rest[..end]))
            .expect("grant line must hold one string literal");
        methods = Some(literal.split(", ").map(str::to_string).collect::<Vec<_>>());
        break;
    }
    let mut methods = methods.expect("grant literal must follow the header name");
    methods.sort();
    methods
}

#[test]
fn head_revalidates_every_get_readable_catalog_root() {
    // Catch a HEAD allowlist that only covers search, or a scope regression on GET.
    for path in [
        "/v1/library",
        "/v1/artists",
        "/v1/releases",
        "/v1/tracks",
        "/v1/search",
        "/v1/playlists",
    ] {
        assert_eq!(
            scope_for_operation("GET", path),
            RouteDecision::Bearer(Scope::LibraryRead),
            "GET {path} remains library:read"
        );
        assert_eq!(
            scope_for_operation("HEAD", path),
            RouteDecision::Bearer(Scope::LibraryRead),
            "HEAD {path} must revalidate the same catalog read"
        );
    }
}

#[test]
fn preflight_advertises_exactly_the_scope_map_served_methods() {
    // Catch a grant that omits served HEAD/PATCH (lawful bearer HEAD
    // revalidation and PATCH edits stay preflight-blocked) or that
    // advertises methods the map never serves (preflight maps fiction).
    for (method, path) in [
        ("GET", "/v1/search"),
        ("HEAD", "/v1/search"),
        ("POST", "/v1/plays"),
        ("PUT", "/v1/playlists/playlist-synthetic"),
        ("PATCH", "/v1/playlists/playlist-synthetic"),
        ("DELETE", "/v1/playlists/playlist-synthetic"),
    ] {
        assert_ne!(
            scope_for_operation(method, path),
            RouteDecision::Deny,
            "{method} must stay a served method for the grant cross-check"
        );
    }
    assert_eq!(
        advertised_preflight_methods(),
        PINNED_PREFLIGHT_METHODS,
        "preflight grant must equal the served methods plus OPTIONS"
    );
}

#[test]
fn playlist_item_writes_accept_exactly_one_nonempty_segment() {
    // Catch prefix-based matching that admits descendants before auth/store spend.
    for method in ["PUT", "PATCH", "DELETE"] {
        assert_eq!(
            scope_for_operation(method, "/v1/playlists/playlist-synthetic"),
            RouteDecision::Bearer(Scope::PlaylistsWrite),
            "{method} must retain the existing single-item route"
        );
        for path in [
            "/v1/playlists",
            "/v1/playlists/",
            "/v1/playlists//",
            "/v1/playlists/playlist-synthetic/",
            "/v1/playlists/playlist-synthetic/tracks",
            "/v1/playlists/playlist-synthetic/tracks/track-synthetic",
            "/v1/playlists-suffix/playlist-synthetic",
        ] {
            assert_eq!(
                scope_for_operation(method, path),
                RouteDecision::Deny,
                "{method} {path} must be denied before bearer lookup or budget spend"
            );
        }
    }
}
