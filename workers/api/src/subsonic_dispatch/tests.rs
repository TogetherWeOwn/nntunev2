//! Native tests for the production dispatch core. Every assertion here is
//! about caller-visible Subsonic behaviour of [`ProductionStore`] and the
//! error/stream helpers — no database, no worker types.

use super::{
    auth_error_response, build_snapshot, compat_session_id, issuance_refusal_response,
    merged_query_params, parse_error_response, rejects_transcode, transcode_refusal_response,
    upstream_refusal_response, ProductionStore, FLAC_CONTENT_TYPE, FLAC_SUFFIX,
};
use crate::catalogue::{ArtistDbRow, ReleaseDbRow, TrackDbRow};
use crate::ledger::STREAM_BYTES_PER_SEC;
use crate::subsonic_auth::AuthError;
use crate::subsonic_http::CredentialParseError;
use nntune_subsonic::auth::Identity;
use nntune_subsonic::endpoints::Params;
use nntune_subsonic::library::{AlbumListQuery, Library, Playlist, StarKind, UserAccount, NOW};

const ALBUM_A: &str = "11111111-1111-4111-8111-111111111111";
const ALBUM_B: &str = "22222222-2222-4222-8222-222222222222";
const SONG_1: &str = "33333333-3333-4333-8333-333333333333";
const SONG_2: &str = "44444444-4444-4444-8444-444444444444";
const SONG_3: &str = "55555555-5555-4555-8555-555555555555";
const ARTIST_A: &str = "66666666-6666-4666-8666-666666666666";
const ARTIST_Z: &str = "77777777-7777-4777-8777-777777777777";
const TEST_NOW_MILLIS: i64 = 1_766_000_000_000;

fn artist_row(mbid: &str, name: &str, sort_name: &str) -> ArtistDbRow {
    ArtistDbRow {
        mbid: mbid.to_string(),
        name: name.to_string(),
        sort_name: sort_name.to_string(),
        artist_type: None,
        country: None,
    }
}

fn release_row(mbid: &str, title: &str, year: Option<i16>, artist_mbid: &str) -> ReleaseDbRow {
    ReleaseDbRow {
        mbid: mbid.to_string(),
        title: title.to_string(),
        release_type: None,
        year,
        artist_mbid: Some(artist_mbid.to_string()),
        artist_name: Some("Artist A".to_string()),
    }
}

fn track_row(
    release_mbid: &str,
    recording_mbid: &str,
    title: &str,
    duration_ms: Option<i32>,
    medium_no: Option<i16>,
    track_no: Option<i16>,
) -> TrackDbRow {
    TrackDbRow {
        release_mbid: release_mbid.to_string(),
        recording_mbid: recording_mbid.to_string(),
        title: title.to_string(),
        duration_ms,
        medium_no,
        track_no,
        release_title: "Album A".to_string(),
        artist_mbid: Some(ARTIST_A.to_string()),
        artist_name: Some("Artist A".to_string()),
    }
}

fn fixture_rows() -> (Vec<ArtistDbRow>, Vec<ReleaseDbRow>, Vec<TrackDbRow>) {
    // Two artists prove browse order follows sort_name: "Zebra" sorts by
    // "0,0" ahead of "Artist A" even though its display name sorts last.
    let artists = vec![
        artist_row(ARTIST_A, "Artist A", "A, Artist"),
        artist_row(ARTIST_Z, "Zebra", "0,0"),
    ];
    let releases = vec![
        release_row(ALBUM_A, "Album A", Some(2001), ARTIST_A),
        release_row(ALBUM_B, "Album B", Some(1999), ARTIST_A),
    ];
    let tracks = vec![
        track_row(ALBUM_A, SONG_1, "Song One", Some(200_000), Some(1), Some(1)),
        track_row(ALBUM_A, SONG_2, "Song Two", Some(180_500), Some(1), Some(2)),
        track_row(ALBUM_B, SONG_3, "Song Three", None, None, None),
    ];
    (artists, releases, tracks)
}

fn store() -> ProductionStore {
    let (artists, releases, tracks) = fixture_rows();
    let (artists, albums, songs) = build_snapshot(&artists, &releases, &tracks);
    ProductionStore::new(
        Identity {
            username: "mel".to_string(),
            email: "mel@example.com".to_string(),
            is_admin: false,
        },
        UserAccount {
            email: "mel@example.com".to_string(),
            is_admin: false,
            is_manager: false,
        },
        artists,
        albums,
        songs,
        TEST_NOW_MILLIS,
    )
}

fn mel() -> Identity {
    Identity {
        username: "mel".to_string(),
        email: "mel@example.com".to_string(),
        is_admin: false,
    }
}

fn query<'a>(identity: &'a Identity, list_type: &'a str) -> AlbumListQuery<'a> {
    AlbumListQuery {
        identity,
        list_type,
        size: 10,
        offset: 0,
        from_year: None,
        to_year: None,
        genre: None,
    }
}

#[test]
fn snapshot_maps_flac_size_duration_and_year() {
    let (artists, releases, tracks) = fixture_rows();
    let (_, albums, songs) = build_snapshot(&artists, &releases, &tracks);
    // Album A duration: (200 + 180) seconds from the member tracks.
    let album_a = albums.iter().find(|a| a.id == ALBUM_A).expect("album A");
    assert_eq!(album_a.duration, 380);
    assert_eq!(album_a.year, Some(2001));
    assert_eq!(album_a.created, NOW);
    assert_eq!(album_a.artist_sort_name, "A, Artist");
    assert_eq!(album_a.release_date, None);
    // FLAC identity and metering-sized bytes, never a transcoder's output.
    let song1 = songs.iter().find(|s| s.id == SONG_1).expect("song 1");
    assert_eq!(song1.content_type, FLAC_CONTENT_TYPE);
    assert_eq!(song1.suffix, FLAC_SUFFIX);
    assert_eq!(song1.duration, 200);
    assert_eq!(song1.size, 200 * STREAM_BYTES_PER_SEC as i64);
    assert_eq!(song1.year, 2001);
    assert_eq!(song1.genre, "");
    assert_eq!(song1.cover_art, ALBUM_A);
    assert_eq!(song1.disc, 1);
    // Missing duration/track fall back to zero, never a panic.
    let song3 = songs.iter().find(|s| s.id == SONG_3).expect("song 3");
    assert_eq!(song3.duration, 0);
    assert_eq!(song3.size, 0);
    assert_eq!(song3.year, 1999);
}

#[test]
fn snapshot_ordering_is_deterministic() {
    let (artists, releases, tracks) = fixture_rows();
    let (a1, b1, s1) = build_snapshot(&artists, &releases, &tracks);
    let (a2, b2, s2) = build_snapshot(&artists, &releases, &tracks);
    assert_eq!(
        a1.iter().map(|a| &a.id).collect::<Vec<_>>(),
        a2.iter().map(|a| &a.id).collect::<Vec<_>>()
    );
    assert_eq!(
        b1.iter().map(|a| &a.id).collect::<Vec<_>>(),
        b2.iter().map(|a| &a.id).collect::<Vec<_>>()
    );
    // Songs sort by album, track, id — Album A's tracks come first.
    assert_eq!(s1.first().map(|s| s.id.as_str()), Some(SONG_1));
    assert_eq!(s2.first().map(|s| s.id.as_str()), Some(SONG_1));
    // Albums sort by name.
    assert_eq!(b1.first().map(|a| a.id.as_str()), Some(ALBUM_A));
    // Artists sort by sort_name, not display name: "Zebra" ("0,0") leads.
    assert_eq!(
        a1.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        vec![ARTIST_Z, ARTIST_A]
    );
}

#[test]
fn store_serves_membership_reads_only() {
    let store = store();
    assert_eq!(store.artists().len(), 2);
    assert!(store.artist(ARTIST_A).is_some());
    assert!(store.artist("unknown").is_none());
    let albums = store.all_albums();
    assert_eq!(albums.len(), 2);
    assert_eq!(
        store.album(ALBUM_A).map(|a| a.id),
        Some(ALBUM_A.to_string())
    );
    let songs = store.songs_for_album(ALBUM_A);
    assert_eq!(
        songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec![SONG_1, SONG_2]
    );
    assert!(store.genres().is_empty(), "empty genre is filtered out");
    assert_eq!(
        store.user_account("mel").map(|u| u.email),
        Some("mel@example.com".into())
    );
    assert!(store.user_account("other").is_none());
    // The index freshness marker is the request time, never a fixture date.
    assert_eq!(store.artists_last_modified(), TEST_NOW_MILLIS);
}

#[test]
fn store_never_serves_fixture_bytes_or_rights_flagged_art() {
    let store = store();
    assert!(store.audio(SONG_1).is_none());
    assert!(store.cover(ALBUM_A).is_none());
    assert!(store.avatar("mel").is_none());
    assert!(store.lyrics("a", "b").is_none());
}

#[test]
fn per_user_state_fails_closed_without_a_history_store() {
    let mut store = store();
    let identity = Identity {
        username: "mel".to_string(),
        email: "mel@example.com".to_string(),
        is_admin: false,
    };
    assert!(store.playlists(&identity).is_empty());
    assert!(store.playlist(&identity, "anything").is_none());
    assert_eq!(
        store.update_playlist(&identity, "id", None, vec![], vec![]),
        Err((
            50,
            "User is not authorized for the given operation.".to_string()
        ))
    );
    // delete maps to error 50 through the endpoint, never a silent ok.
    assert_eq!(
        store.delete_playlist(&identity, "id"),
        Err((
            50,
            "User is not authorized for the given operation.".to_string()
        ))
    );
    assert!(!store.star(&identity, StarKind::Song, SONG_1));
    assert!(store.starred(&identity, StarKind::Song).is_empty());
    assert!(!store.set_rating(&identity, SONG_1, 5));
    assert!(store.rating(&identity, SONG_1).is_none());
    // The scrobble endpoint ignores this return; with no history table,
    // recent stays empty while frequent keeps every album eligible
    // (see module docs and the album_list test below).
    assert!(!store.scrobble(&identity, SONG_1));
    assert!(store.queue(&identity).is_none());
    // create_playlist is infallible: id-less placeholder, never stored.
    let created = store.create_playlist(&identity, "Evening".to_string());
    assert_eq!(created.id, "");
    assert_eq!(created.owner, "mel");
    assert!(store.playlists(&identity).is_empty());
    assert!(store.playlist(&identity, "").is_none());
    let _: Playlist = created;
}

#[test]
fn album_list_semantics_match_the_fixture_library() {
    let store = store();
    let identity = mel();
    let mut q = query(&identity, "random");
    assert_eq!(store.album_list(q).len(), 2);
    // Random ignores offset.
    q.offset = 1;
    assert_eq!(store.album_list(q).len(), 2);
    // frequent keeps every album eligible (no history yet).
    assert_eq!(store.album_list(query(&identity, "frequent")).len(), 2);
    // recent/highest/starred/byGenre are history- or genre-backed: empty.
    assert!(store.album_list(query(&identity, "recent")).is_empty());
    assert!(store.album_list(query(&identity, "highest")).is_empty());
    assert!(store.album_list(query(&identity, "starred")).is_empty());
    assert!(store.album_list(query(&identity, "byGenre")).is_empty());
    // Unknown types are rejected upstream; the fallback is empty.
    assert!(store.album_list(query(&identity, "podcast")).is_empty());
    // byYear filters on the release year across swapped bounds.
    let mut q = query(&identity, "byYear");
    q.from_year = Some(2005);
    q.to_year = Some(2000);
    let filtered = store.album_list(q);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, ALBUM_A);
}

#[test]
fn album_list2_supports_five_types_and_rejects_the_rest() {
    let store = store();
    let identity = mel();
    assert_eq!(
        store.album_list2(query(&identity, "random")).unwrap().len(),
        2
    );
    assert_eq!(
        store.album_list2(query(&identity, "newest")).unwrap().len(),
        2
    );
    assert!(store.album_list2(query(&identity, "recent")).is_err());
    assert!(store.album_list2(query(&identity, "frequent")).is_err());
    let (code, message) = store.album_list2(query(&identity, "recent")).unwrap_err();
    assert_eq!(code, 0);
    assert!(message.contains("recent"));
}

#[test]
fn random_songs_is_deterministic_and_bounded() {
    let store = store();
    let identity = Identity {
        username: "mel".to_string(),
        email: String::new(),
        is_admin: false,
    };
    let first = store.random_songs(&identity, 2);
    let second = store.random_songs(&identity, 2);
    assert_eq!(first.len(), 2);
    assert_eq!(
        first.iter().map(|s| &s.id).collect::<Vec<_>>(),
        second.iter().map(|s| &s.id).collect::<Vec<_>>()
    );
}

fn body_str(resp: &nntune_subsonic::HttpResponse) -> String {
    String::from_utf8(resp.body.clone()).expect("utf8 body")
}

#[test]
fn error_rendering_selects_format_and_never_leaks() {
    let xml = parse_error_response("u=mel", CredentialParseError::DuplicateCredential);
    assert_eq!(xml.content_type, "application/xml");
    assert!(xml.body.starts_with(b"<?xml"));
    let xml_text = body_str(&xml);
    assert!(
        xml_text.contains("code=\"0\""),
        "parse errors are generic 0: {xml_text}"
    );
    assert!(
        !xml_text.contains("mel"),
        "no credential material: {xml_text}"
    );

    let json = auth_error_response("f=json&u=mel", AuthError::InvalidToken);
    assert_eq!(json.content_type, "application/json");
    assert!(body_str(&json).contains("\"code\":40"));

    let jsonp = auth_error_response("f=jsonp&callback=cb&u=mel", AuthError::Unsupported);
    assert_eq!(jsonp.content_type, "text/javascript");
    assert!(body_str(&jsonp).starts_with("/**/cb("));
    assert!(body_str(&jsonp).contains("\"code\":42"));

    // A broken callback falls back to JSON error 10, not to the error body.
    let bad = auth_error_response("f=jsonp&callback=not-valid&u=mel", AuthError::Unsupported);
    assert_eq!(bad.content_type, "application/json");
    assert!(body_str(&bad).contains("\"code\":10"));

    for (err, code, fragment) in [
        (AuthError::Conflicting, "43", "Conflicting"),
        (AuthError::MissingSalt, "10", "missing"),
        (AuthError::InvalidKey, "44", "Invalid API key"),
        (AuthError::Unavailable, "0", "Internal server error"),
    ] {
        let resp = auth_error_response("f=json", err);
        let text = body_str(&resp);
        assert!(text.contains(&format!("\"code\":{code}")), "{text}");
        assert!(text.contains(fragment), "{text}");
    }
}

#[test]
fn merged_params_carry_body_selected_format_into_error_documents() {
    // Only selectors survive, with query values winning duplicates.
    assert_eq!(merged_query_params("POST", "", "f=json"), "f=json");
    assert_eq!(
        merged_query_params("POST", "f=json", "f=jsonp&callback=cb"),
        "f=json&callback=cb"
    );
    assert_eq!(merged_query_params("GET", "f=json", "f=jsonp"), "f=json");
    assert_eq!(merged_query_params("POST", "f=json", ""), "f=json");
    assert_eq!(merged_query_params("POST", "", ""), "");

    // Body-only `f=json` on a POST auth failure renders JSON, not XML —
    // the mount passes the merged string to its error renderers.
    let merged = merged_query_params("POST", "", "apiKey=invalid&f=json");
    let resp = auth_error_response(&merged, AuthError::InvalidKey);
    assert_eq!(resp.content_type, "application/json");
    assert!(body_str(&resp).contains("\"code\":44"));

    // Body-only `f=jsonp&callback=cb` renders JSONP with the callback.
    let merged = merged_query_params("POST", "", "apiKey=invalid&f=jsonp&callback=cb");
    let resp = auth_error_response(&merged, AuthError::InvalidKey);
    assert_eq!(resp.content_type, "text/javascript");
    assert!(body_str(&resp).starts_with("/**/cb("));
    assert!(body_str(&resp).contains("\"code\":44"));

    // Query `f` still wins over body `f`: query `f=json` over body
    // `f=jsonp` renders JSON, not JSONP.
    let merged = merged_query_params("POST", "f=json", "apiKey=invalid&f=jsonp&callback=cb");
    let resp = auth_error_response(&merged, AuthError::InvalidKey);
    assert_eq!(resp.content_type, "application/json");
    assert!(body_str(&resp).contains("\"code\":44"));

    // Parse errors likewise honour the merged format selector.
    let merged = merged_query_params("POST", "", "f=json");
    let resp = parse_error_response(&merged, CredentialParseError::DuplicateCredential);
    assert_eq!(resp.content_type, "application/json");
    assert!(body_str(&resp).contains("\"code\":0"));
}

#[test]
fn rejected_parameters_have_bounded_error_rendering() {
    use crate::subsonic_http::{parse_credentials, MAX_PARAMS_PER_SOURCE, MAX_SOURCE_BYTES};

    for (query, error) in [
        (
            format!("f=json&x={}", "a".repeat(MAX_SOURCE_BYTES)),
            CredentialParseError::SourceTooLarge,
        ),
        (
            format!("f=json&{}", vec!["x=1"; MAX_PARAMS_PER_SOURCE].join("&")),
            CredentialParseError::TooManyParams,
        ),
    ] {
        assert_eq!(parse_credentials(Some(&query), None).unwrap_err(), error);
        assert_eq!(
            merged_query_params("POST", &query, "f=jsonp&callback=cb"),
            ""
        );
        assert_eq!(
            parse_error_response(&query, error).content_type,
            "application/xml"
        );
    }
    let long = "a".repeat(129);
    let merged = merged_query_params("POST", "f=jsonp", &format!("callback={long}&callback=cb"));
    let response = parse_error_response(&merged, CredentialParseError::MalformedEncoding);
    assert_eq!(response.content_type, "application/json");
    assert!(body_str(&response).contains("\"code\":10"));
    let merged = merged_query_params("POST", "%66=jsonp&callback=%63b", "f=json&callback=other");
    assert_eq!(
        parse_error_response(&merged, CredentialParseError::MalformedEncoding).content_type,
        "text/javascript"
    );
    let malformed = parse_error_response(
        "f=jsonp&callback=%zz",
        CredentialParseError::MalformedEncoding,
    );
    assert_eq!(malformed.content_type, "application/json");
}

#[test]
fn issuance_refusals_map_status_only() {
    for status in [402u16, 409, 429] {
        let resp = issuance_refusal_response("f=json", status);
        assert!(body_str(&resp).contains("\"code\":50"), "status {status}");
    }
    for status in [0u16, 200, 400, 500, 503] {
        let resp = issuance_refusal_response("f=json", status);
        assert!(body_str(&resp).contains("\"code\":0"), "status {status}");
    }
    // The native body ("paywall", plan names) is never echoed back.
    let resp = issuance_refusal_response("f=json&u=secret-user", 402);
    assert!(!body_str(&resp).contains("secret-user"));
}

#[test]
fn upstream_refusals_map_missing_sources_to_70() {
    for status in [404u16, 502] {
        let resp = upstream_refusal_response("f=json", status);
        assert!(body_str(&resp).contains("\"code\":70"), "status {status}");
        assert!(body_str(&resp).contains("Song not found"));
    }
    for status in [200u16, 206, 403, 500] {
        let resp = upstream_refusal_response("f=json", status);
        assert!(body_str(&resp).contains("\"code\":0"), "status {status}");
    }
}

#[test]
fn compat_session_id_is_stable_distinct_and_bounded() {
    let a = compat_session_id("user-1", "cred-1");
    let b = compat_session_id("user-1", "cred-1");
    let c = compat_session_id("user-1", "cred-2");
    let d = compat_session_id("user-2", "cred-1");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_ne!(a, d);
    assert!(!a.is_empty() && a.len() <= 128);
    assert!(
        !a.contains("user-1") && !a.contains("cred-1"),
        "ids are not recoverable: {a}"
    );
}

#[test]
fn transcode_requests_are_refused_flac_only() {
    let rejects = |q: &str| rejects_transcode(&Params::parse(q));
    assert!(rejects("format=mp3"));
    assert!(rejects("format=opus"));
    assert!(rejects("maxBitRate=128"));
    assert!(rejects("maxBitRate=abc"), "unparseable cap fails closed");
    assert!(!rejects(""));
    assert!(!rejects("format=flac"));
    assert!(!rejects("format=FLAC"));
    assert!(!rejects("maxBitRate=0"));
    assert!(!rejects("format=flac&maxBitRate=0"));
    let resp = transcode_refusal_response("f=json");
    assert!(body_str(&resp).contains("\"code\":0"));
    assert!(body_str(&resp).contains("Transcoding is not supported"));
}
