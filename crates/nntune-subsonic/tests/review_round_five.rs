//! Round-five review pins (Builder-owned; the Test Author banks are untouched).
//!
//! Each case here fails if the guarded behaviour is reverted: the legacy
//! album-list filters, the catalogue streamable filter, the strict
//! size/offset guard, the multi-character uppercase index initial, and the
//! form-body collector rules shared with the Worker shell.

use nntune_subsonic::library::{Album, Artist, Song, NOW};
use nntune_subsonic::{BodyFault, FormBody, MemStore, SubsonicApp};

const KEY: &str = "apiKey=test-key-1&v=1.16.1&c=t&f=json";

fn album(id: &str, name: &str) -> Album {
    Album {
        id: id.into(),
        name: name.into(),
        artist: "Test Artist".into(),
        artist_id: "ar-1".into(),
        duration: 200,
        created: NOW.into(),
        year: Some(2026),
        release_date: Some("2026-01-01".into()),
        curated_at: Some(NOW.into()),
        artist_sort_name: "Test Artist".into(),
    }
}

fn song(id: &str, album_id: &str, genre: &str) -> Song {
    Song {
        id: id.into(),
        title: format!("Song {id}"),
        album: album_id.into(),
        artist: "Test Artist".into(),
        track: 1,
        year: 2026,
        genre: genre.into(),
        cover_art: album_id.into(),
        size: 1000,
        content_type: "audio/mpeg".into(),
        suffix: "mp3".into(),
        duration: 200,
        disc: 1,
        created: NOW.into(),
        album_id: album_id.into(),
        artist_id: "ar-1".into(),
    }
}

fn artist(id: &str, name: &str) -> Artist {
    Artist {
        id: id.into(),
        name: name.into(),
    }
}

fn app(artists: Vec<Artist>, albums: Vec<Album>, songs: Vec<Song>) -> SubsonicApp<MemStore> {
    SubsonicApp::new(MemStore::empty_with_test_auth().with_catalogue(artists, albums, songs))
}

fn call(app: &SubsonicApp<MemStore>, path: &str, extra: &str) -> String {
    let query = format!("{KEY}&{extra}");
    String::from_utf8(app.handle("GET", path, &query, "").body).unwrap()
}

/// Values of every `"key":"…"` pair, in document order.
fn values_of(text: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\":\"");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find(&needle) {
        rest = &rest[pos + needle.len()..];
        let end = rest.find('"').expect("closed string");
        out.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}

fn ids(text: &str) -> Vec<String> {
    values_of(text, "id")
}

fn is_error_10(text: &str) -> bool {
    text.contains("\"status\":\"failed\"") && text.contains("\"code\":10")
}

fn catalogue() -> SubsonicApp<MemStore> {
    app(
        vec![artist("ar-1", "Test Artist")],
        vec![
            album("al-c", "cherry"),
            album("al-a", "apple"),
            album("al-b", "Banana"),
            album("al-songless", "Zeta"),
        ],
        vec![
            song("so-c", "al-c", "Rock"),
            song("so-a", "al-a", "Jazz"),
            song("so-b", "al-b", "rock"),
        ],
    )
}

#[test]
fn legacy_by_genre_filters_case_insensitively_in_name_order() {
    let app = catalogue();
    let text = call(
        &app,
        "/rest/getAlbumList",
        "type=byGenre&genre=ROCK&size=10",
    );
    // Only the two Rock/rock albums, in plain name order ("Banana" < "cherry").
    assert_eq!(ids(&text), vec!["al-b", "al-c"], "{text}");
    let jazz = call(
        &app,
        "/rest/getAlbumList",
        "type=byGenre&genre=jazz&size=10",
    );
    assert_eq!(ids(&jazz), vec!["al-a"], "{jazz}");
    let none = call(
        &app,
        "/rest/getAlbumList",
        "type=byGenre&genre=metal&size=10",
    );
    assert!(ids(&none).is_empty(), "{none}");
}

#[test]
fn legacy_highest_keeps_only_rated_albums_by_rating() {
    let app = catalogue();
    for (id, rating) in [("al-a", 3), ("al-c", 5)] {
        let set = call(&app, "/rest/setRating", &format!("id={id}&rating={rating}"));
        assert!(set.contains("\"status\":\"ok\""), "{set}");
    }
    let text = call(&app, "/rest/getAlbumList", "type=highest&size=10");
    // al-b and al-songless are unrated and must be excluded.
    assert_eq!(ids(&text), vec!["al-c", "al-a"], "{text}");
}

#[test]
fn legacy_random_ignores_offset_and_alphabetical_uses_plain_name_order() {
    let app = catalogue();
    let paged = call(&app, "/rest/getAlbumList", "type=random&size=10&offset=3");
    // v1's legacy getRandom takes a limit only: an offset past the end of
    // the 4 albums still returns the full shuffle.
    assert_eq!(ids(&paged).len(), 4, "{paged}");
    let sorted = call(
        &app,
        "/rest/getAlbumList",
        "type=alphabeticalByName&size=10",
    );
    // Byte order, so the capital sorts first.
    assert_eq!(
        ids(&sorted),
        vec!["al-b", "al-songless", "al-a", "al-c"],
        "{sorted}"
    );
}

#[test]
fn catalogue_list_excludes_albums_with_no_streamable_song() {
    let app = catalogue();
    let text = call(
        &app,
        "/rest/getAlbumList2",
        "type=alphabeticalByName&size=10",
    );
    assert_eq!(ids(&text), vec!["al-b", "al-a", "al-c"], "{text}");
}

#[test]
fn size_and_offset_are_strict_for_album_lists_and_random_songs() {
    let app = catalogue();
    for list in ["getAlbumList2", "getAlbumList"] {
        for size in ["0", "501", "abc", "-1", ""] {
            let text = call(
                &app,
                &format!("/rest/{list}"),
                &format!("type=newest&size={size}"),
            );
            assert!(is_error_10(&text), "{list} size={size}: {text}");
        }
        for offset in ["-1", "abc"] {
            let text = call(
                &app,
                &format!("/rest/{list}"),
                &format!("type=newest&size=5&offset={offset}"),
            );
            assert!(is_error_10(&text), "{list} offset={offset}: {text}");
        }
    }
    for size in ["0", "501", "abc", "-1"] {
        let text = call(&app, "/rest/getRandomSongs", &format!("size={size}"));
        assert!(is_error_10(&text), "getRandomSongs size={size}: {text}");
    }
    // The bounds themselves succeed and page exactly.
    let one = call(
        &app,
        "/rest/getAlbumList2",
        "type=alphabeticalByName&size=1",
    );
    assert_eq!(ids(&one), vec!["al-b"], "{one}");
    let max = call(
        &app,
        "/rest/getAlbumList2",
        "type=alphabeticalByName&size=500",
    );
    assert_eq!(ids(&max).len(), 3, "{max}");
    let one_song = call(&app, "/rest/getRandomSongs", "size=1");
    assert_eq!(values_of(&one_song, "title").len(), 1, "{one_song}");
    let max_songs = call(&app, "/rest/getRandomSongs", "size=500");
    assert_eq!(values_of(&max_songs, "title").len(), 3, "{max_songs}");
    // An offset past the end is an empty page, not an error.
    let past = call(&app, "/rest/getAlbumList2", "type=newest&size=5&offset=99");
    assert!(past.contains("\"status\":\"ok\""), "{past}");
    assert!(ids(&past).is_empty(), "{past}");
}

#[test]
fn index_letter_uppercases_multi_character_initials_before_the_ascii_test() {
    let artists = vec![
        artist("ar-ss", "ßeta"),
        artist("ar-ff", "ﬀoo"),
        artist("ar-a", "apple"),
        artist("ar-cjk", "音楽"),
    ];
    let app = app(artists, Vec::new(), Vec::new());
    let text = call(&app, "/rest/getArtists", "");
    // ß upper-cases to "SS" and the ﬀ ligature to "FF": both are ASCII
    // letters afterwards, so they get their own buckets. CJK stays under '#'.
    let mut buckets = Vec::new();
    let mut rest = text.as_str();
    while let Some(pos) = rest.find("\"name\":\"") {
        rest = &rest[pos + 8..];
        let end = rest.find('"').unwrap();
        if rest[end..].starts_with("\",\"artist\"") {
            buckets.push(rest[..end].to_string());
        }
        rest = &rest[end..];
    }
    assert_eq!(buckets, vec!["#", "A", "FF", "SS"], "{text}");
}

// --- form-body collector (the rules the Worker shell applies) ---

#[test]
fn form_body_short_read_is_unreadable_not_a_smaller_body() {
    let mut body = FormBody::with_cap(Some(20), 1024);
    assert_eq!(body.check_declared(), Ok(()));
    body.push(b"u=test&id=").unwrap();
    // Clean end-of-stream after 10 of 20 declared bytes: a client abort.
    assert_eq!(body.finish(), Err(BodyFault::Unreadable));
}

#[test]
fn form_body_complete_read_returns_the_text() {
    let mut body = FormBody::with_cap(Some(10), 1024);
    body.push(b"u=te").unwrap();
    body.push(b"st&id=").unwrap();
    assert_eq!(body.finish().as_deref(), Ok("u=test&id="));
    // No declared length (chunked): whatever arrived is the body.
    let mut chunked = FormBody::with_cap(None, 1024);
    chunked.push(b"a=1").unwrap();
    assert_eq!(chunked.finish().as_deref(), Ok("a=1"));
    // No body at all is an empty valid form.
    assert_eq!(FormBody::with_cap(None, 1024).finish().as_deref(), Ok(""));
}

#[test]
fn form_body_refuses_oversized_declared_and_actual_lengths() {
    let declared = FormBody::with_cap(Some(11), 10);
    assert_eq!(declared.check_declared(), Err(BodyFault::Oversized));
    assert_eq!(FormBody::with_cap(Some(10), 10).check_declared(), Ok(()));
    // Chunked bodies are capped on the bytes actually received.
    let mut chunked = FormBody::with_cap(None, 10);
    chunked.push(b"123456").unwrap();
    assert_eq!(chunked.push(b"78901"), Err(BodyFault::Oversized));
    let mut exact = FormBody::with_cap(None, 10);
    exact.push(b"1234567890").unwrap();
    assert_eq!(exact.finish().as_deref(), Ok("1234567890"));
}

#[test]
fn form_body_rejects_invalid_utf8() {
    let mut body = FormBody::with_cap(None, 1024);
    body.push(&[0xff, 0xfe]).unwrap();
    assert_eq!(body.finish(), Err(BodyFault::Unreadable));
}
