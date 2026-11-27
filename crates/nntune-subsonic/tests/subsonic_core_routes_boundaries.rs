use nntune_subsonic::endpoints::{handle_endpoint, EndpointResult};
use nntune_subsonic::envelope::code;
use nntune_subsonic::library::AlbumListQuery;
use nntune_subsonic::{AuthStore, HttpResponse, Library, MemStore, Params, SubsonicApp};

const FIXTURE_QUERY: &str = "apiKey=test-key-1&v=1.16.1&c=boundary-test";

fn body(response: &HttpResponse) -> &str {
    std::str::from_utf8(&response.body).unwrap()
}

#[test]
fn unregistered_methods_cannot_dispatch_public_reads_or_mutations() {
    let app = SubsonicApp::new(MemStore::seeded());
    for method in ["DELETE", "PATCH", "HEAD", "post", "GET "] {
        let response = app.handle(method, "/rest/getOpenSubsonicExtensions.view", "", "");
        assert_eq!(response.status, 404, "{method}");
        let response = app.handle(
            method,
            "/rest/createPlaylist.view",
            &format!("{FIXTURE_QUERY}&name=must-not-exist"),
            "",
        );
        assert_eq!(response.status, 404, "{method}");
    }
    let list = app.handle("GET", "/rest/getPlaylists", FIXTURE_QUERY, "");
    assert!(body(&list).contains("<playlists/>"));
    let allowed = app.handle(
        "POST",
        "/rest/createPlaylist.view",
        FIXTURE_QUERY,
        "name=ok",
    );
    assert!(body(&allowed).contains("id=\"pl-1\""));
}

#[test]
fn direct_nonlegacy_dispatch_refuses_unsupported_writes() {
    let mut store = MemStore::seeded();
    let identity = store.verify_api_key("test-key-1").unwrap();
    let params = Params::parse("name=example&id=so-1&rating=4");
    for endpoint in [
        "createPlaylist",
        "updatePlaylist",
        "deletePlaylist",
        "scrobble",
        "star",
        "unstar",
        "setRating",
        "savePlayQueue",
        "refreshPodcasts",
        "createPodcastChannel",
        "deletePodcastChannel",
    ] {
        assert!(
            matches!(
                handle_endpoint(&mut store, &identity, endpoint, &params, false),
                EndpointResult::Err(code::GENERIC, _)
            ),
            "{endpoint}"
        );
    }
    assert!(store.playlists(&identity).is_empty());
    assert!(store.rating(&identity, "so-1").is_none());
    assert!(store.queue(&identity).is_none());
}

#[test]
fn page_sizes_cannot_bypass_the_result_limit_through_aliases() {
    let app = SubsonicApp::production(MemStore::seeded());
    for path in ["/rest/search3", "/rest/getSongsByGenre"] {
        for selector in ["size", "count"] {
            let call = |size| {
                app.handle(
                    "GET",
                    path,
                    &format!("{FIXTURE_QUERY}&genre=Rock&f=json&{selector}={size}"),
                    "",
                )
            };
            assert!(body(&call(500)).contains("\"status\":\"ok\""));
            assert!(
                body(&call(501)).contains("\"code\":10"),
                "{path} {selector}"
            );
        }
    }
}

#[test]
fn empty_store_does_not_expose_fixture_accounts_or_avatars() {
    let empty = MemStore::empty();
    let fixtures = MemStore::empty_with_test_auth();
    assert!(empty.user_account("test").is_none());
    assert!(empty.avatar("test").is_none());
    assert!(fixtures.user_account("test").is_some());
    assert!(fixtures.avatar("test").is_some());
}

#[test]
fn missing_year_bound_never_falls_back_to_an_unfiltered_catalogue() {
    let store = MemStore::seeded();
    let identity = store.verify_api_key("test-key-1").unwrap();
    let query = AlbumListQuery {
        identity: &identity,
        list_type: "byYear",
        size: 10,
        offset: 0,
        from_year: Some(2026),
        to_year: Some(2026),
        genre: None,
    };
    assert_eq!(store.album_list(query).len(), 1);
    assert_eq!(store.album_list2(query).unwrap().len(), 1);
    for query in [
        AlbumListQuery {
            from_year: None,
            ..query
        },
        AlbumListQuery {
            to_year: None,
            ..query
        },
    ] {
        assert!(store.album_list(query).is_empty());
        assert_eq!(
            store.album_list2(query).unwrap_err().0,
            code::REQUIRED_PARAM_MISSING
        );
    }
}

#[test]
fn invalid_xml_catalogue_text_is_rejected_without_reflecting_the_document() {
    let store = MemStore::seeded();
    let mut song = store.song("so-1").unwrap();
    song.title = "catalogue-canary\u{0}".into();
    let store = store.with_catalogue(vec![], vec![], vec![song]);
    for app in [
        SubsonicApp::new(store.clone()),
        SubsonicApp::production(store),
    ] {
        let query = format!("{FIXTURE_QUERY}&id=so-1");
        let xml = app.handle("GET", "/rest/getSong", &query, "");
        assert_eq!(xml.status, 503);
        assert!(body(&xml).contains("status=\"failed\""));
        assert!(!body(&xml).contains("catalogue-canary"));
        let json = app.handle("GET", "/rest/getSong", &format!("{query}&f=json"), "");
        assert!(body(&json).contains("catalogue-canary\\u0000"));
    }
}
