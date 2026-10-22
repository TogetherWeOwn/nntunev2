//! Golden fixture suite: handler output must equal the committed v1-derived
//! fixtures byte-for-byte. See `fixtures/subsonic/README.md`.

use nntune_subsonic::{MemStore, SubsonicApp};
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "fixtures",
        "subsonic",
        name,
    ]
    .iter()
    .collect();
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn app() -> SubsonicApp<MemStore> {
    SubsonicApp::new(MemStore::seeded())
}

fn get(app: &SubsonicApp<MemStore>, path: &str, query: &str) -> (u16, String, Vec<u8>) {
    let res = app.handle("GET", path, query, "");
    (res.status, res.content_type, res.body)
}

fn post(
    app: &SubsonicApp<MemStore>,
    path: &str,
    query: &str,
    body: &str,
) -> (u16, String, Vec<u8>) {
    let res = app.handle("POST", path, query, body);
    (res.status, res.content_type, res.body)
}

const KEY: &str = "apiKey=test-key-1&v=1.16.1&c=fixture";

#[test]
fn ping_xml_and_json_match_goldens() {
    let app = app();
    let (_, ct, body) = get(&app, "/rest/ping", KEY);
    assert_eq!(ct, "application/xml");
    assert_eq!(body, fixture("ping.xml"));
    let (_, ct, body) = get(&app, "/rest/ping.view", &format!("{KEY}&f=json"));
    assert_eq!(ct, "application/json");
    assert_eq!(body, fixture("ping.json"));
}

#[test]
fn jsonp_wraps_envelope() {
    let app = app();
    let (_, ct, body) = get(&app, "/rest/ping", &format!("{KEY}&f=jsonp&callback=cb"));
    // Symfony framing: `/**/cb({...});` as `text/javascript`.
    assert_eq!(ct, "text/javascript");
    assert_eq!(body, fixture("ping-jsonp.txt"));
}

#[test]
fn jsonp_validator_matches_symfony() {
    let app = app();
    // Unquoted non-digit indexers and reserved words fall back to error 10.
    for bad in ["top[name]", "a[b-c]", "this", "function", "new"] {
        let (_, ct, body) = get(&app, "/rest/ping", &format!("{KEY}&f=jsonp&callback={bad}"));
        assert_eq!(ct, "application/json", "{bad}");
        assert!(
            String::from_utf8(body).unwrap().contains("\"code\":10"),
            "{bad}"
        );
    }
    // Quoted indexers (either quote, `]` inside quotes) serve script.
    for good in ["ns.cb[0].done", "app.fn[\"b-ar\"]", "app.fn['x']"] {
        let (_, ct, _) = get(
            &app,
            "/rest/ping",
            &format!("{KEY}&f=jsonp&callback={good}"),
        );
        assert_eq!(ct, "text/javascript", "{good}");
    }
}

#[test]
fn auth_refusals_match_goldens() {
    let app = app();
    // No credentials at all -> 40, HTTP 200, XML.
    let (status, _, body) = get(&app, "/rest/ping", "v=1.16.1&c=fixture");
    assert_eq!(status, 200);
    assert_eq!(body, fixture("error-auth-required.xml"));
    // Bad key -> 44 JSON envelope (v1 `InvalidApiKeyException`).
    let (_, _, body) = get(&app, "/rest/ping", "apiKey=nope&v=1.16.1&c=x&f=json");
    assert_eq!(body, fixture("error-auth-required.json"));
    // Token without salt -> 10 (v1 `RequiredParameterMissingException`).
    let (status, _, body) = get(&app, "/rest/ping", "u=test&t=abc&v=1.16.1&c=x&f=json");
    assert_eq!(status, 200);
    assert!(String::from_utf8(body).unwrap().contains("\"code\":10"));
    // apiKey mixed with legacy params -> 43.
    let (status, _, body) = get(
        &app,
        "/rest/ping",
        "apiKey=test-key-1&u=test&v=1.16.1&c=x&f=json",
    );
    assert_eq!(status, 200);
    assert_eq!(body, fixture("error-auth-conflict.json"));
    // enc: passwords decode like v1; malformed hex answers 40.
    let (status, _, body) = get(
        &app,
        "/rest/ping",
        "u=test&p=enc:746573742d70617373&v=1.16.1&c=x",
    );
    assert_eq!(status, 200);
    assert!(String::from_utf8(body).unwrap().contains("status=\"ok\""));
    let (_, _, body) = get(&app, "/rest/ping", "u=test&p=enc:zz&v=1.16.1&c=x&f=json");
    assert!(String::from_utf8(body).unwrap().contains("\"code\":40"));
}

#[test]
fn missing_and_unknown_ids_match_goldens() {
    let app = app();
    let (status, _, body) = get(&app, "/rest/getSong", KEY);
    assert_eq!(status, 200);
    assert_eq!(body, fixture("error-missing-param.xml"));
    let (_, _, body) = get(&app, "/rest/stream", &format!("{KEY}&id=nope&f=json"));
    assert_eq!(body, fixture("stream-not-found.json"));
    for (path, fixture_name) in [
        ("/rest/getSong", "getSong-not-found.json"),
        ("/rest/getAlbum", "getAlbum-not-found.json"),
        ("/rest/getArtist", "getArtist-not-found.json"),
    ] {
        let (_, _, body) = get(&app, path, &format!("{KEY}&id=missing&f=json"));
        assert_eq!(body, fixture(fixture_name), "{path}");
    }
}

#[test]
fn unsupported_ops_refuse_with_code_zero() {
    let app = app();
    let (status, _, body) = get(&app, "/rest/jukeboxControl", &format!("{KEY}&action=get"));
    assert_eq!(status, 200);
    assert_eq!(body, fixture("error-unsupported.xml"));
    let (_, _, body) = get(
        &app,
        "/rest/jukeboxControl",
        &format!("{KEY}&action=get&f=json"),
    );
    assert_eq!(body, fixture("error-unsupported.json"));
    for op in [
        "getVideos",
        "getVideoInfo",
        "getCaptions",
        "savePlayQueueByIndex",
        "getInternetRadioStations",
        "createInternetRadioStation",
        "updateInternetRadioStation",
        "deleteInternetRadioStation",
    ] {
        let (status, _, body) = get(&app, &format!("/rest/{op}"), KEY);
        assert_eq!(status, 200, "{op}");
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("code=\"0\""), "{op}");
        assert!(
            text.contains(&format!("The {op} operation is not supported")),
            "{op}"
        );
    }
    // hls.m3u8 is not a Subsonic document: plain 410.
    let (status, _, _) = get(&app, "/rest/hls.m3u8", KEY);
    assert_eq!(status, 410);
}

#[test]
fn license_and_extensions_match_goldens() {
    let app = app();
    let (_, _, body) = get(&app, "/rest/getLicense", KEY);
    assert_eq!(body, fixture("getLicense.xml"));
    let (_, _, body) = get(&app, "/rest/getLicense.view", &format!("{KEY}&f=json"));
    assert_eq!(body, fixture("getLicense.json"));
    let (_, _, body) = get(&app, "/rest/getOpenSubsonicExtensions", KEY);
    assert_eq!(body, fixture("getOpenSubsonicExtensions.xml"));
    let (_, _, body) = get(
        &app,
        "/rest/getOpenSubsonicExtensions",
        &format!("{KEY}&f=json"),
    );
    assert_eq!(body, fixture("getOpenSubsonicExtensions.json"));
}

#[test]
fn extensions_are_public_and_tokeninfo_reports_caller() {
    let app = app();
    // v1 serves `getOpenSubsonicExtensions` outside the auth group: no
    // credentials at all still answers 200 with the capability list.
    let (status, _, body) = get(
        &app,
        "/rest/getOpenSubsonicExtensions",
        "v=1.16.1&c=fixture",
    );
    assert_eq!(status, 200);
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("apiKeyAuthentication"), "{text}");
    assert!(text.contains("formPost"), "{text}");
    assert!(!text.contains("songLyrics"), "{text}");
    // `tokenInfo` is authenticated and reports the caller.
    let (_, _, body) = get(&app, "/rest/tokenInfo", &format!("{KEY}&f=json"));
    assert_eq!(body, fixture("tokenInfo.json"));
    let (status, _, body) = get(&app, "/rest/tokenInfo", "v=1.16.1&c=fixture&f=json");
    assert_eq!(status, 200);
    assert!(
        String::from_utf8(body).unwrap().contains("\"code\":40"),
        "tokenInfo without credentials must refuse"
    );
}

#[test]
fn avatar_requires_self_or_admin() {
    let app = app();
    const OTHER_KEY: &str = "apiKey=other-key-1&v=1.16.1&c=fixture";
    const ADMIN_KEY: &str = "apiKey=admin-key-1&v=1.16.1&c=fixture";
    // Another user's avatar answers 50 (never 200, never an existence
    // oracle beyond what getUser already enforces).
    let (status, _, body) = get(
        &app,
        "/rest/getAvatar",
        &format!("{OTHER_KEY}&username=test&f=json"),
    );
    assert_eq!(status, 200);
    assert_eq!(body, fixture("getAvatar-forbidden.json"));
    // Admin may fetch anyone's avatar; unknown users are 70 on the
    // self/admin path.
    let (status, ct, _) = get(
        &app,
        "/rest/getAvatar",
        &format!("{ADMIN_KEY}&username=test"),
    );
    assert_eq!(status, 200);
    assert_eq!(ct, "image/png");
    let (status, _, body) = get(
        &app,
        "/rest/getAvatar",
        &format!("{ADMIN_KEY}&username=nobody&f=json"),
    );
    assert_eq!(status, 200);
    assert!(
        String::from_utf8(body).unwrap().contains("\"code\":70"),
        "unknown avatar must be 70"
    );
}

#[test]
fn oversized_post_body_answers_error_10() {
    let app = app();
    let big = "x".repeat(nntune_subsonic::MAX_BODY_BYTES + 1);
    let (status, _, body) = post(&app, "/rest/ping", &format!("{KEY}&f=json"), &big);
    assert_eq!(status, 200);
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("\"code\":10"), "{text}");
    assert!(
        text.contains("POST body exceeds the 10 MB limit."),
        "{text}"
    );
    // Exactly at the cap still parses (the cap refuses *over* 10 MB).
    let exact = "y".repeat(nntune_subsonic::MAX_BODY_BYTES);
    let (status, _, _) = post(&app, "/rest/ping", KEY, &exact);
    assert_eq!(status, 200);
}

#[test]
fn user_roles_match_v1_resource() {
    let app = app();
    const MANAGER_KEY: &str = "apiKey=mgr-key-1&v=1.16.1&c=fixture";
    // Manager: not admin, but manages (upload/coverArt true).
    let (_, _, body) = get(
        &app,
        "/rest/getUser",
        &format!("{MANAGER_KEY}&username=manager&f=json"),
    );
    let text = String::from_utf8(body).unwrap();
    for want in [
        "\"adminRole\":false",
        "\"settingsRole\":true",
        "\"uploadRole\":true",
        "\"coverArtRole\":true",
        "\"commentRole\":false",
    ] {
        assert!(text.contains(want), "{want} in {text}");
    }
    assert!(
        !text.contains("folder"),
        "v1 UserResource has no folder: {text}"
    );
}

#[test]
fn catalogue_endpoints_match_goldens() {
    let app = app();
    let (_, _, body) = get(&app, "/rest/getArtists", KEY);
    assert_eq!(body, fixture("getArtists-seeded.xml"));
    let (_, _, body) = get(&app, "/rest/getArtists", &format!("{KEY}&f=json"));
    assert_eq!(body, fixture("getArtists-seeded.json"));
    let (_, _, body) = get(&app, "/rest/getIndexes", &format!("{KEY}&f=json"));
    assert_eq!(body, fixture("getIndexes-seeded.json"));
    let (_, _, body) = get(&app, "/rest/getSong", &format!("{KEY}&id=so-1&f=json"));
    assert_eq!(body, fixture("getSong.json"));
    let (_, _, body) = get(&app, "/rest/getAlbum", &format!("{KEY}&id=al-1&f=json"));
    assert_eq!(body, fixture("getAlbum.json"));
    let (_, _, body) = get(&app, "/rest/getArtist", &format!("{KEY}&id=ar-1&f=json"));
    assert_eq!(body, fixture("getArtist.json"));
    let (_, _, body) = get(&app, "/rest/getGenres", KEY);
    assert_eq!(body, fixture("getGenres.xml"));
    let (_, _, body) = get(
        &app,
        "/rest/getMusicDirectory",
        &format!("{KEY}&id=al-1&f=json"),
    );
    assert_eq!(body, fixture("getMusicDirectory.json"));
    let (_, _, body) = get(&app, "/rest/getMusicFolders", &format!("{KEY}&f=json"));
    assert_eq!(body, fixture("getMusicFolders.json"));
    let (_, _, body) = get(
        &app,
        "/rest/getUser",
        &format!("{KEY}&username=test&f=json"),
    );
    assert_eq!(body, fixture("getUser.json"));
    let (_, _, body) = get(&app, "/rest/getRandomSongs", &format!("{KEY}&f=json"));
    assert_eq!(body, fixture("getRandomSongs.json"));
    let (_, _, body) = get(&app, "/rest/getNowPlaying", KEY);
    assert_eq!(body, fixture("getNowPlaying-empty.xml"));
    let (_, _, body) = get(&app, "/rest/getPodcasts", KEY);
    assert_eq!(body, fixture("getPodcasts-empty.xml"));
    let (_, _, body) = get(&app, "/rest/getBookmarks", KEY);
    assert_eq!(body, fixture("getBookmarks-empty.xml"));
    let (_, _, body) = get(
        &app,
        "/rest/getLyrics",
        &format!("{KEY}&artist=Test+Artist&title=Test+Song&f=json"),
    );
    assert_eq!(body, fixture("getLyrics.json"));
    let (_, _, body) = get(
        &app,
        "/rest/getLyricsBySongId",
        &format!("{KEY}&id=so-1&f=json"),
    );
    assert_eq!(body, fixture("getLyricsBySongId.json"));
    let (_, _, body) = get(
        &app,
        "/rest/getLyrics",
        &format!("{KEY}&artist=Missing&title=Missing&f=json"),
    );
    assert!(String::from_utf8(body).unwrap().contains("\"lyrics\":{}"));
}

#[test]
fn empty_library_behaves_like_v1() {
    let app = SubsonicApp::new(MemStore::empty_with_test_auth());
    let (_, _, body) = get(&app, "/rest/getArtists", KEY);
    assert_eq!(body, fixture("getArtists-empty.xml"));
    let (_, _, body) = get(&app, "/rest/search3", &format!("{KEY}&query=nope"));
    assert_eq!(body, fixture("search3-empty.xml"));
    let (_, _, body) = get(&app, "/rest/getStarred2", KEY);
    assert_eq!(body, fixture("starred2-empty.xml"));
}

#[test]
fn search_and_album_lists_match_goldens() {
    let app = app();
    let (_, _, body) = get(&app, "/rest/search3", &format!("{KEY}&query=Test&f=json"));
    assert_eq!(body, fixture("search3-match.json"));
    let (_, _, body) = get(
        &app,
        "/rest/search3",
        &format!("{KEY}&query=&size=10&offset=0&f=json"),
    );
    assert_eq!(body, fixture("search3-empty-query.json"));
    let (_, _, body) = get(
        &app,
        "/rest/search3",
        &format!("{KEY}&query=Test&artistCount=0&f=json"),
    );
    assert!(String::from_utf8(body).unwrap().contains("\"artist\":[]"));
    let (_, _, body) = get(
        &app,
        "/rest/getAlbumList2",
        &format!("{KEY}&type=newest&f=json"),
    );
    assert_eq!(body, fixture("getAlbumList2-newest.json"));
    let (_, _, body) = get(
        &app,
        "/rest/getAlbumList2",
        &format!("{KEY}&type=alphabeticalByName&f=json"),
    );
    assert_eq!(body, fixture("getAlbumList2-alphabetical.json"));
    let (_, _, body) = get(
        &app,
        "/rest/getAlbumList2",
        &format!("{KEY}&type=byYear&fromYear=2020&toYear=2030&f=json"),
    );
    assert_eq!(body, fixture("getAlbumList2-byYear.json"));
    let (_, _, body) = get(
        &app,
        "/rest/getAlbumList2",
        &format!("{KEY}&type=byYear&f=json"),
    );
    assert!(String::from_utf8(body).unwrap().contains("\"code\":10"));
    let (_, _, body) = get(
        &app,
        "/rest/getAlbumList2",
        &format!("{KEY}&type=invalid&f=json"),
    );
    assert!(String::from_utf8(body).unwrap().contains("\"code\":10"));
}

#[test]
fn playlist_lifecycle_matches_goldens() {
    let app = app();
    let (_, _, body) = get(
        &app,
        "/rest/createPlaylist",
        &format!("{KEY}&name=Road+Trip&f=json"),
    );
    assert_eq!(body, fixture("playlist-create.json"));
    let (_, _, body) = get(
        &app,
        "/rest/updatePlaylist",
        &format!("{KEY}&playlistId=pl-1&songIdToAdd=so-1"),
    );
    assert_eq!(body, fixture("ping.xml")); // bare ok envelope
    let (_, _, body) = get(&app, "/rest/getPlaylist", &format!("{KEY}&id=pl-1&f=json"));
    assert_eq!(body, fixture("playlist-get.json"));
    let (_, _, body) = get(&app, "/rest/deletePlaylist", &format!("{KEY}&id=pl-1"));
    assert_eq!(body, fixture("ping.xml"));
    let (status, _, body) = get(&app, "/rest/getPlaylist", &format!("{KEY}&id=pl-1&f=json"));
    assert_eq!(status, 200);
    assert!(String::from_utf8(body).unwrap().contains("\"code\":70"));
}

#[test]
fn star_flow_matches_goldens() {
    let app = app();
    let (_, _, body) = get(&app, "/rest/star", &format!("{KEY}&id=so-1"));
    assert_eq!(body, fixture("ping.xml"));
    let (_, _, body) = get(&app, "/rest/getStarred2", &format!("{KEY}&f=json"));
    assert_eq!(body, fixture("starred2-after-star.json"));
    let (_, _, body) = get(&app, "/rest/unstar", &format!("{KEY}&id=so-1"));
    assert_eq!(body, fixture("ping.xml"));
    let (_, _, body) = get(&app, "/rest/getStarred2", KEY);
    assert_eq!(body, fixture("starred2-empty.xml"));
    let (_, _, body) = get(&app, "/rest/star", &format!("{KEY}&id=gone&id=so-1"));
    assert_eq!(body, fixture("ping.xml"));
    let (_, _, body) = get(&app, "/rest/getStarred2", &format!("{KEY}&f=json"));
    assert_eq!(body, fixture("starred2-after-star.json"));
    let (_, _, body) = get(&app, "/rest/scrobble", &format!("{KEY}&id=gone&id=so-1"));
    assert_eq!(body, fixture("ping.xml"));
}

#[test]
fn playqueue_roundtrip_matches_golden() {
    let app = app();
    let (_, _, body) = get(
        &app,
        "/rest/savePlayQueue",
        &format!("{KEY}&id=so-1&current=so-1&position=0"),
    );
    assert_eq!(body, fixture("ping.xml"));
    let (_, _, body) = get(&app, "/rest/getPlayQueue", &format!("{KEY}&f=json"));
    assert_eq!(body, fixture("getPlayQueue-saved.json"));
}

#[test]
fn per_user_isolation_matches_v1() {
    let app = app();
    const OTHER_KEY: &str = "apiKey=other-key-1&v=1.16.1&c=fixture";
    const ADMIN_KEY: &str = "apiKey=admin-key-1&v=1.16.1&c=fixture";
    // test creates a playlist; other cannot read, update or delete it (50).
    let (_, _, body) = get(
        &app,
        "/rest/createPlaylist",
        &format!("{KEY}&name=Mine&f=json"),
    );
    assert!(String::from_utf8(body)
        .unwrap()
        .contains("\"name\":\"Mine\""));
    for (path, query) in [
        ("/rest/getPlaylist", format!("{OTHER_KEY}&id=pl-1&f=json")),
        (
            "/rest/updatePlaylist",
            format!("{OTHER_KEY}&playlistId=pl-1&name=Hijacked&f=json"),
        ),
        (
            "/rest/deletePlaylist",
            format!("{OTHER_KEY}&id=pl-1&f=json"),
        ),
    ] {
        let (status, _, body) = get(&app, path, &query);
        assert_eq!(status, 200, "{path}");
        assert!(
            String::from_utf8(body).unwrap().contains("\"code\":50"),
            "{path}"
        );
    }
    // Other user's stars do not leak into test's starred set.
    let (_, _, body) = get(&app, "/rest/star", &format!("{OTHER_KEY}&id=so-1"));
    assert!(String::from_utf8(body).unwrap().contains("status=\"ok\""));
    let (_, _, body) = get(&app, "/rest/getStarred2", KEY);
    assert_eq!(body, fixture("starred2-empty.xml"));
    // getUser: other username answers 50; admin may read anyone.
    let (_, _, body) = get(
        &app,
        "/rest/getUser",
        &format!("{KEY}&username=other&f=json"),
    );
    assert!(String::from_utf8(body).unwrap().contains("\"code\":50"));
    let (status, _, body) = get(
        &app,
        "/rest/getUser",
        &format!("{ADMIN_KEY}&username=test&f=json"),
    );
    assert_eq!(status, 200);
    assert!(String::from_utf8(body)
        .unwrap()
        .contains("\"username\":\"test\""));
}

#[test]
fn jsonp_and_post_body_match_v1() {
    let app = app();
    // Invalid JSONP callback falls back to error-10 JSON, never script.
    let (_, ct, body) = get(
        &app,
        "/rest/ping",
        &format!("{KEY}&f=jsonp&callback=alert(document.domain);//"),
    );
    assert_eq!(ct, "application/json");
    assert_eq!(body, fixture("error-jsonp-callback.json"));
    // POST form body authenticates exactly like the query string.
    let (status, _, body) = post(
        &app,
        "/rest/ping",
        "v=1.16.1&c=fixture",
        "apiKey=test-key-1",
    );
    assert_eq!(status, 200);
    assert_eq!(body, fixture("ping.xml"));
}

#[test]
fn utf8_params_survive_roundtrip() {
    let app = app();
    // Percent-encoded UTF-8 decodes to real UTF-8 (no Latin-1 mojibake).
    let (_, _, body) = get(
        &app,
        "/rest/createPlaylist",
        &format!("{KEY}&name=Caf%C3%A9&f=json"),
    );
    assert_eq!(body, fixture("playlist-create-utf8.json"));
    let (_, _, body) = get(
        &app,
        "/rest/search3",
        &format!("{KEY}&query=Bj%C3%B6rk&f=json"),
    );
    assert!(String::from_utf8(body).unwrap().contains("searchResult3"));
}

#[test]
fn byte_routes_return_typed_bodies() {
    let app = app();
    let (status, ct, body) = get(&app, "/rest/stream", &format!("{KEY}&id=so-1"));
    assert_eq!(status, 200);
    assert_eq!(ct, "audio/mpeg");
    assert!(!body.is_empty());
    let (status, ct, body) = get(&app, "/rest/download", &format!("{KEY}&id=so-1"));
    assert_eq!(status, 200);
    assert_eq!(ct, "audio/mpeg");
    assert!(!body.is_empty());
    let (status, ct, body) = get(&app, "/rest/getCoverArt", &format!("{KEY}&id=al-1"));
    assert_eq!(status, 200);
    assert_eq!(ct, "image/png");
    assert!(!body.is_empty());
    let (status, ct, body) = get(&app, "/rest/getAvatar", &format!("{KEY}&username=test"));
    assert_eq!(status, 200);
    assert_eq!(ct, "image/png");
    assert!(!body.is_empty());
}

#[test]
fn route_table_covers_v1_surface() {
    let supported = nntune_subsonic::SUPPORTED_ENDPOINTS;
    let unsupported = nntune_subsonic::UNSUPPORTED_ENDPOINTS;
    assert_eq!(supported.len(), 49);
    assert_eq!(unsupported.len(), 9);
    let want: serde_like::Table = serde_like::parse(&fixture("routes-table.json"));
    let supported_str: Vec<&str> = supported.to_vec();
    let unsupported_str: Vec<&str> = unsupported.to_vec();
    assert_eq!(want.supported, supported_str);
    assert_eq!(want.unsupported, unsupported_str);
    // Every supported endpoint answers 200 behind auth (never 404).
    let app = app();
    for name in supported {
        let extra = default_params(name);
        let (status, _, _) = get(&app, &format!("/rest/{name}"), &format!("{KEY}{extra}"));
        assert_eq!(status, 200, "{name}");
    }
    // Unknown ops are plain 404s, outside the envelope.
    let (status, _, _) = get(&app, "/rest/bogusOp", KEY);
    assert_eq!(status, 404);
}

fn default_params(name: &str) -> &'static str {
    match name {
        "getSong"
        | "getAlbum"
        | "getArtist"
        | "getPlaylist"
        | "deletePlaylist"
        | "stream"
        | "download"
        | "getCoverArt"
        | "getLyricsBySongId"
        | "getAlbumInfo"
        | "getAlbumInfo2"
        | "deletePodcastChannel" => "&id=so-1",
        "getMusicDirectory" => "&id=al-1",
        "search2" | "search3" => "&query=Test",
        "getAlbumList" | "getAlbumList2" => "&type=newest",
        "createPlaylist" => "&name=x",
        "updatePlaylist" => "&playlistId=pl-9",
        "getUser" | "getAvatar" => "&username=test",
        "getSongsByGenre" => "&genre=Rock",
        "getLyrics" => "&artist=Test+Artist&title=Test+Song",
        "getArtistInfo" | "getArtistInfo2" => "&id=ar-1",
        "createPodcastChannel" => "&url=https://example.com/feed",
        _ => "",
    }
}

// Minimal JSON string-list parser for the routes manifest (no dependencies).
mod serde_like {
    pub struct Table {
        pub supported: Vec<&'static str>,
        pub unsupported: Vec<&'static str>,
    }

    pub fn parse(bytes: &[u8]) -> Table {
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        Table {
            supported: array(&s, "\"supported\""),
            unsupported: array(&s, "\"unsupported\""),
        }
    }

    fn array(s: &str, key: &str) -> Vec<&'static str> {
        // Leak the manifest: the suite runs once and the borrow checker
        // cannot see that `s` outlives the test.
        let owned: &'static str = Box::leak(s.to_string().into_boxed_str());
        let start = owned.find(key).unwrap();
        let open = owned[start..].find('[').unwrap() + start;
        let close = owned[open..].find(']').unwrap() + open;
        owned[open + 1..close]
            .split(',')
            .map(|x| x.trim().trim_matches('"'))
            .filter(|x| !x.is_empty())
            .collect()
    }
}
