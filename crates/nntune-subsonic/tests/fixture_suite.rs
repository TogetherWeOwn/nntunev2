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
    let res = app.handle("GET", path, query);
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
    assert_eq!(ct, "application/javascript");
    assert_eq!(body, fixture("ping-jsonp.txt"));
}

#[test]
fn auth_refusals_match_goldens() {
    let app = app();
    // No credentials at all -> 40, HTTP 200, XML.
    let (status, _, body) = get(&app, "/rest/ping", "v=1.16.1&c=fixture");
    assert_eq!(status, 200);
    assert_eq!(body, fixture("error-auth-required.xml"));
    // Bad key -> same 40 JSON envelope.
    let (_, _, body) = get(&app, "/rest/ping", "apiKey=nope&v=1.16.1&c=x&f=json");
    assert_eq!(body, fixture("error-auth-required.json"));
    // Token without salt -> 41.
    let (status, _, body) = get(&app, "/rest/ping", "u=test&t=abc&v=1.16.1&c=x&f=json");
    assert_eq!(status, 200);
    assert!(String::from_utf8(body).unwrap().contains("\"code\":41"));
}

#[test]
fn missing_and_unknown_ids_match_goldens() {
    let app = app();
    let (status, _, body) = get(&app, "/rest/getSong", KEY);
    assert_eq!(status, 200);
    assert_eq!(body, fixture("error-missing-param.xml"));
    let (_, _, body) = get(&app, "/rest/stream", &format!("{KEY}&id=nope&f=json"));
    assert_eq!(body, fixture("error-not-found.json"));
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
fn catalogue_endpoints_match_goldens() {
    let app = app();
    let (_, _, body) = get(&app, "/rest/getArtists", KEY);
    assert_eq!(body, fixture("getArtists-seeded.xml"));
    let (_, _, body) = get(&app, "/rest/getArtists", &format!("{KEY}&f=json"));
    assert_eq!(body, fixture("getArtists-seeded.json"));
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
}

#[test]
fn empty_library_behaves_like_v1() {
    let app = SubsonicApp::new(MemStore::empty());
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
        "/rest/getAlbumList2",
        &format!("{KEY}&type=newest&f=json"),
    );
    assert_eq!(body, fixture("getAlbumList2-newest.json"));
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
    assert_eq!(supported.len(), 48);
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
