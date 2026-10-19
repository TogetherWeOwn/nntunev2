//! Test-only native REST surface contract; fixtures and identifiers are synthetic.
//! Builder must not edit this acceptance file. Expected failures are assertion-RED,
//! not missing APIs: use the existing router and authorization entry points.

use nntune_api::native_rest::{scope_for_operation, RouteDecision, Scope};

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
