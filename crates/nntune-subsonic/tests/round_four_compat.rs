//! Round-four compatibility regressions for the Subsonic port.
//!
//! Derived from the v1 oracle (nntune `app/Values/ParsedLyrics.php`,
//! `ScrobbleRequest` / `ScrobbleController`, `ArtistResource::fromRow`,
//! `IndexesResource::indexLetter`, `GetIndexesController`,
//! `AlbumRepository::getByYearRange`, `StreamableCatalogue::albumList`).
//! These tests fail against the current implementation where the port
//! diverges; they pass once the implementation follows v1.

use nntune_subsonic::library::{
    Album, AlbumListQuery, Artist, Library, PlayQueue, Playlist, Song, StarKind, UserAccount, NOW,
};
use nntune_subsonic::{AuthStore, Identity, MemStore, SubsonicApp};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

const KEY: &str = "apiKey=test-key-1&v=1.16.1&c=t";
const OTHER_KEY: &str = "apiKey=other-key-1&v=1.16.1&c=t";

fn get<S: Library>(app: &SubsonicApp<S>, path: &str, query: &str) -> Vec<u8> {
    app.handle("GET", path, query, "").body
}

fn body_text(app: &SubsonicApp<TestStore>, path: &str, query: &str) -> String {
    String::from_utf8(get(app, path, query)).unwrap()
}

/// Minimal configurable store. Catalogue reads come from the struct fields;
/// user state is stubbed except for the scrobble spy, which records only
/// known song ids (like the in-memory store).
#[derive(Clone, Debug)]
struct TestStore {
    artists: Vec<Artist>,
    albums: Vec<Album>,
    songs: Vec<Song>,
    lyrics: BTreeMap<(String, String), String>,
    scrobble_log: Rc<RefCell<Vec<(String, String)>>>,
}

impl TestStore {
    fn new(
        artists: Vec<Artist>,
        albums: Vec<Album>,
        songs: Vec<Song>,
        lyrics: BTreeMap<(String, String), String>,
    ) -> (TestStore, Rc<RefCell<Vec<(String, String)>>>) {
        let log = Rc::new(RefCell::new(Vec::new()));
        (
            TestStore {
                artists,
                albums,
                songs,
                lyrics,
                scrobble_log: Rc::clone(&log),
            },
            log,
        )
    }

    fn single_song_lyrics(raw: &str) -> (TestStore, Rc<RefCell<Vec<(String, String)>>>) {
        let artists = vec![Artist {
            id: "ar-1".into(),
            name: "Test Artist".into(),
        }];
        let albums = vec![Album {
            id: "al-1".into(),
            name: "Test Album".into(),
            artist: "Test Artist".into(),
            artist_id: "ar-1".into(),
            duration: 200,
            created: NOW.into(),
        }];
        let songs = vec![Song {
            id: "so-1".into(),
            title: "Test Song".into(),
            album: "Test Album".into(),
            artist: "Test Artist".into(),
            track: 1,
            year: 2026,
            genre: "Rock".into(),
            cover_art: "al-1".into(),
            size: 12345,
            content_type: "audio/mpeg".into(),
            suffix: "mp3".into(),
            duration: 200,
            disc: 1,
            created: NOW.into(),
            album_id: "al-1".into(),
            artist_id: "ar-1".into(),
        }];
        let mut lyrics = BTreeMap::new();
        lyrics.insert(
            ("Test Artist".to_string(), "Test Song".to_string()),
            raw.to_string(),
        );
        TestStore::new(artists, albums, songs, lyrics)
    }

    fn two_albums() -> (TestStore, Rc<RefCell<Vec<(String, String)>>>) {
        let artists = vec![Artist {
            id: "ar-1".into(),
            name: "Test Artist".into(),
        }];
        let albums = vec![
            Album {
                id: "al-a".into(),
                name: "Album A".into(),
                artist: "Test Artist".into(),
                artist_id: "ar-1".into(),
                duration: 200,
                created: NOW.into(),
            },
            Album {
                id: "al-b".into(),
                name: "Album B".into(),
                artist: "Test Artist".into(),
                artist_id: "ar-1".into(),
                duration: 200,
                created: NOW.into(),
            },
        ];
        let songs = vec![
            Song {
                id: "so-a".into(),
                title: "Song A".into(),
                album: "Album A".into(),
                artist: "Test Artist".into(),
                track: 1,
                year: 2026,
                genre: "Rock".into(),
                cover_art: "al-a".into(),
                size: 1000,
                content_type: "audio/mpeg".into(),
                suffix: "mp3".into(),
                duration: 200,
                disc: 1,
                created: NOW.into(),
                album_id: "al-a".into(),
                artist_id: "ar-1".into(),
            },
            Song {
                id: "so-b".into(),
                title: "Song B".into(),
                album: "Album B".into(),
                artist: "Test Artist".into(),
                track: 1,
                year: 2026,
                genre: "Rock".into(),
                cover_art: "al-b".into(),
                size: 1000,
                content_type: "audio/mpeg".into(),
                suffix: "mp3".into(),
                duration: 200,
                disc: 1,
                created: NOW.into(),
                album_id: "al-b".into(),
                artist_id: "ar-1".into(),
            },
        ];
        TestStore::new(artists, albums, songs, BTreeMap::new())
    }
}

fn test_identity(username: &str) -> Identity {
    Identity {
        username: username.into(),
        email: format!("{username}@example.com"),
        is_admin: false,
    }
}

impl AuthStore for TestStore {
    fn verify_api_key(&self, api_key: &str) -> Option<Identity> {
        match api_key {
            "test-key-1" => Some(test_identity("test")),
            "other-key-1" => Some(test_identity("other")),
            _ => None,
        }
    }

    fn verify_password(&self, username: &str, password: &str) -> Option<Identity> {
        if (username == "test" && password == "test-pass")
            || (username == "other" && password == "other-pass")
        {
            Some(test_identity(username))
        } else {
            None
        }
    }

    fn verify_token(&self, _username: &str, _token: &str, _salt: &str) -> Option<Identity> {
        None
    }
}

impl Library for TestStore {
    fn user_account(&self, username: &str) -> Option<UserAccount> {
        match username {
            "test" | "other" => Some(UserAccount {
                email: format!("{username}@example.com"),
                is_admin: false,
                is_manager: false,
            }),
            _ => None,
        }
    }

    fn artists(&self) -> Vec<Artist> {
        self.artists.clone()
    }

    fn artist(&self, id: &str) -> Option<Artist> {
        self.artists.iter().find(|a| a.id == id).cloned()
    }

    fn albums_for_artist(&self, artist_id: &str) -> Vec<Album> {
        self.albums
            .iter()
            .filter(|a| a.artist_id == artist_id)
            .cloned()
            .collect()
    }

    fn album(&self, id: &str) -> Option<Album> {
        self.albums.iter().find(|a| a.id == id).cloned()
    }

    fn all_albums(&self) -> Vec<Album> {
        self.albums.clone()
    }

    fn songs_for_album(&self, album_id: &str) -> Vec<Song> {
        let mut out: Vec<Song> = self
            .songs
            .iter()
            .filter(|s| s.album_id == album_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.track.cmp(&b.track).then(a.id.cmp(&b.id)));
        out
    }

    fn song(&self, id: &str) -> Option<Song> {
        self.songs.iter().find(|s| s.id == id).cloned()
    }

    fn all_songs(&self) -> Vec<Song> {
        let mut out = self.songs.clone();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    fn genres(&self) -> Vec<(String, i64, i64)> {
        Vec::new()
    }

    fn audio(&self, song_id: &str) -> Option<Vec<u8>> {
        self.song(song_id).map(|_| b"audio".to_vec())
    }

    fn cover(&self, _cover_id: &str) -> Option<Vec<u8>> {
        None
    }

    fn avatar(&self, _username: &str) -> Option<Vec<u8>> {
        None
    }

    fn lyrics(&self, artist: &str, title: &str) -> Option<String> {
        self.lyrics
            .get(&(artist.to_string(), title.to_string()))
            .cloned()
    }

    fn playlists(&self, _identity: &Identity) -> Vec<Playlist> {
        Vec::new()
    }

    fn playlist(&self, _identity: &Identity, _id: &str) -> Option<Playlist> {
        None
    }

    fn create_playlist(&mut self, identity: &Identity, name: String) -> Playlist {
        Playlist {
            id: "pl-1".into(),
            name,
            owner: identity.username.clone(),
            song_ids: Vec::new(),
            created: NOW.into(),
            changed: NOW.into(),
        }
    }

    fn update_playlist(
        &mut self,
        _identity: &Identity,
        _id: &str,
        _name: Option<String>,
        _add: Vec<String>,
        _remove: Vec<usize>,
    ) -> Result<(), (i64, String)> {
        Err((70, "The requested data was not found.".to_string()))
    }

    fn delete_playlist(&mut self, _identity: &Identity, _id: &str) -> Result<(), (i64, String)> {
        Err((70, "The requested data was not found.".to_string()))
    }

    fn star(&mut self, _identity: &Identity, _kind: StarKind, _id: &str) -> bool {
        false
    }

    fn unstar(&mut self, _identity: &Identity, _kind: StarKind, _id: &str) {}

    fn starred(&self, _identity: &Identity, _kind: StarKind) -> Vec<String> {
        Vec::new()
    }

    fn set_rating(&mut self, _identity: &Identity, _id: &str, _rating: i64) -> bool {
        false
    }

    fn rating(&self, _identity: &Identity, _id: &str) -> Option<i64> {
        None
    }

    fn scrobble(&mut self, identity: &Identity, song_id: &str) -> bool {
        if self.song(song_id).is_none() {
            return false;
        }
        self.scrobble_log
            .borrow_mut()
            .push((identity.username.clone(), song_id.to_string()));
        true
    }

    fn queue(&self, _identity: &Identity) -> Option<PlayQueue> {
        None
    }

    fn save_queue(&mut self, _identity: &Identity, _queue: PlayQueue) {}
}

// Silence dead-code warnings for the query type re-export used in docs.
#[allow(dead_code)]
fn _query_type(_q: AlbumListQuery<'_>) {}

// --- structured lyrics (v1 ParsedLyrics) ---

fn lyrics_body(raw: &str) -> String {
    let (store, _) = TestStore::single_song_lyrics(raw);
    let app = SubsonicApp::new(store);
    body_text(
        &app,
        "/rest/getLyricsBySongId",
        &format!("{KEY}&id=so-1&f=json"),
    )
}

#[test]
fn lyrics_drop_known_metadata_and_keep_only_timed_sorted_trimmed() {
    let raw = "[ar:Test Artist]\n[ti:Test Song]\n[al:Test Album]\n[00:12.00] Hello\n[00:05.50] First\n\n[00:20.00] Last";
    let text = lyrics_body(raw);
    assert!(text.contains("\"synced\":true"), "{text}");
    assert!(text.contains("\"lang\":\"und\""), "{text}");
    // Metadata tags never surface as lyric lines.
    assert!(!text.contains("[ar:"), "{text}");
    assert!(!text.contains("[ti:"), "{text}");
    assert!(!text.contains("[al:"), "{text}");
    // Only timed lines survive, sorted by start, text trimmed.
    let first = text.find("\"value\":\"First\"").expect("{text}");
    let hello = text.find("\"value\":\"Hello\"").expect("{text}");
    let last = text.find("\"value\":\"Last\"").expect("{text}");
    assert!(first < hello && hello < last, "{text}");
    assert!(text.contains("\"start\":5500"), "{text}");
    assert!(text.contains("\"start\":12000"), "{text}");
    assert!(text.contains("\"start\":20000"), "{text}");
    assert!(!text.contains("\" Hello\""), "{text}");
}

#[test]
fn lyrics_offset_accepts_whitespace_case_and_sign() {
    for raw in [
        "[offset: 500]\n[00:01.00] Hi",
        "[OFFSET:500]\n[00:01.00] Hi",
        "[Offset:  500  ]\n[00:01.00] Hi",
        "[offset:+500]\n[00:01.00] Hi",
    ] {
        let text = lyrics_body(raw);
        assert!(text.contains("\"offset\":500"), "{raw} -> {text}");
        assert!(text.contains("\"value\":\"Hi\""), "{raw} -> {text}");
    }
}

#[test]
fn lyrics_split_crlf_cr_and_lf_identically() {
    let lf = "[00:01.00] A\n[00:02.00] B";
    let crlf = "[00:01.00] A\r\n[00:02.00] B";
    let cr = "[00:01.00] A\r[00:02.00] B";
    let a = lyrics_body(lf);
    let b = lyrics_body(crlf);
    let c = lyrics_body(cr);
    assert!(a.contains("\"start\":1000"), "{a}");
    assert!(a.contains("\"start\":2000"), "{a}");
    assert_eq!(a, b, "CRLF must match LF");
    assert_eq!(a, c, "CR must match LF");
}

#[test]
fn lyrics_multi_timestamp_line_expands() {
    let text = lyrics_body("[00:01.00][00:02.00] Chorus");
    assert!(text.contains("\"synced\":true"), "{text}");
    assert!(text.contains("\"start\":1000"), "{text}");
    assert!(text.contains("\"start\":2000"), "{text}");
    assert_eq!(text.matches("\"value\":\"Chorus\"").count(), 2, "{text}");
}

#[test]
fn lyrics_plain_text_has_no_start_and_lang_und() {
    let text = lyrics_body("La la la");
    assert!(text.contains("\"synced\":false"), "{text}");
    assert!(text.contains("\"lang\":\"und\""), "{text}");
    assert!(text.contains("\"value\":\"La la la\""), "{text}");
    assert!(
        !text.contains("\"start\""),
        "plain lines carry no start: {text}"
    );
}

#[test]
fn lyrics_unsynced_path_drops_id_tags() {
    let text = lyrics_body("[ar:Someone]\nLa la la");
    assert!(!text.contains("[ar:"), "{text}");
    assert!(text.contains("\"value\":\"La la la\""), "{text}");
}

#[test]
fn lyrics_seeded_plain_matches_corrected_golden() {
    let app = SubsonicApp::new(MemStore::seeded());
    let body = get(
        &app,
        "/rest/getLyricsBySongId",
        &format!("{KEY}&id=so-1&f=json"),
    );
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("\"lang\":\"und\""), "{text}");
    assert!(
        !text.contains("\"start\""),
        "plain seeded line has no start: {text}"
    );
}

// --- scrobble submission (v1 ScrobbleRequest) ---

fn scrobble_with(submission: Option<&str>) -> (String, usize) {
    let (store, log) = TestStore::single_song_lyrics("La la la");
    let app = SubsonicApp::new(store);
    let query = match submission {
        Some(v) => format!("{KEY}&id=so-1&submission={v}&f=json"),
        None => format!("{KEY}&id=so-1&f=json"),
    };
    let text = body_text(&app, "/rest/scrobble", &query);
    let calls = log.borrow().len();
    (text, calls)
}

#[test]
fn scrobble_defaults_to_submission_true() {
    let (text, calls) = scrobble_with(None);
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    assert_eq!(calls, 1, "missing submission defaults to true");
}

#[test]
fn scrobble_true_spellings_count_as_plays() {
    for value in [
        "true", "True", "TRUE", "1", "yes", "Yes", "YES", "on", "On", "ON",
    ] {
        let (text, calls) = scrobble_with(Some(value));
        assert!(text.contains("\"status\":\"ok\""), "{value} -> {text}");
        assert_eq!(calls, 1, "{value} must count as a play");
    }
}

#[test]
fn scrobble_false_spellings_do_not_count_as_plays() {
    for value in [
        "false", "False", "FALSE", "0", "no", "No", "NO", "off", "Off", "OFF", "maybe", "2", "",
    ] {
        let (text, calls) = scrobble_with(Some(value));
        assert!(text.contains("\"status\":\"ok\""), "{value} -> {text}");
        assert_eq!(calls, 0, "{value} must not count as a play");
    }
}

#[test]
fn scrobble_ignores_unknown_ids() {
    let (store, log) = TestStore::single_song_lyrics("La la la");
    let app = SubsonicApp::new(store);
    let text = body_text(&app, "/rest/scrobble", &format!("{KEY}&id=gone&f=json"));
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    assert!(log.borrow().is_empty(), "unknown ids are skipped");
    let text = body_text(
        &app,
        "/rest/scrobble",
        &format!("{KEY}&id=gone&id=so-1&f=json"),
    );
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    assert_eq!(log.borrow().len(), 1, "known ids still record");
}

#[test]
fn scrobble_now_playing_does_not_affect_frequent_ranking() {
    // Two albums with equal track counts: frequent must follow play counts
    // (v1 getMostPlayed), so only submission=true moves an album first.
    let (store, log) = TestStore::two_albums();
    let app = SubsonicApp::new(store);
    // Now-playing ping for Album B must not move it.
    let text = body_text(
        &app,
        "/rest/scrobble",
        &format!("{KEY}&id=so-b&submission=false&f=json"),
    );
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    assert!(log.borrow().is_empty());
    let text = body_text(
        &app,
        "/rest/getAlbumList",
        &format!("{KEY}&type=frequent&f=json"),
    );
    let pos_a = text.find("\"id\":\"al-a\"").expect("{text}");
    let pos_b = text.find("\"id\":\"al-b\"").expect("{text}");
    assert!(
        pos_a < pos_b,
        "now-playing must not reorder frequent: {text}"
    );
    // A real submission for Album B must move it first.
    let text = body_text(
        &app,
        "/rest/scrobble",
        &format!("{KEY}&id=so-b&submission=true&f=json"),
    );
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    let text = body_text(
        &app,
        "/rest/getAlbumList",
        &format!("{KEY}&type=frequent&f=json"),
    );
    let pos_a = text.find("\"id\":\"al-a\"").expect("{text}");
    let pos_b = text.find("\"id\":\"al-b\"").expect("{text}");
    assert!(
        pos_b < pos_a,
        "submission=true must rank Album B first: {text}"
    );
}

// --- search3 coverArt ---

#[test]
fn search3_artist_carries_no_unservable_cover_art() {
    let app = SubsonicApp::new(MemStore::seeded());
    let body = get(&app, "/rest/search3", &format!("{KEY}&query=Test&f=json"));
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("\"albumCount\":1"), "{text}");
    assert!(
        !text.contains("\"coverArt\":\"ar-1\""),
        "artist coverArt must be absent when unservable: {text}"
    );
    // And the advertised id must not resolve: serving it would 70.
    let body = get(&app, "/rest/getCoverArt", &format!("{KEY}&id=ar-1&f=json"));
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("\"code\":70"), "{text}");
}

// --- album lists and random songs ---

#[test]
fn album_list_reversed_by_year_is_accepted() {
    let app = SubsonicApp::new(MemStore::seeded());
    // Legacy list reverses descending (v1 AlbumRepository::getByYearRange).
    let body = get(
        &app,
        "/rest/getAlbumList",
        &format!("{KEY}&type=byYear&fromYear=2030&toYear=2020&f=json"),
    );
    let text = String::from_utf8(body).unwrap();
    assert!(
        !text.contains("\"code\":10"),
        "reversed range is accepted: {text}"
    );
    assert!(text.contains("al-1"), "{text}");
    // The catalogue list answers ok as well (never error 10 for a
    // well-formed reversed range).
    let body = get(
        &app,
        "/rest/getAlbumList2",
        &format!("{KEY}&type=byYear&fromYear=2030&toYear=2020&f=json"),
    );
    let text = String::from_utf8(body).unwrap();
    assert!(
        !text.contains("\"code\":10"),
        "reversed range is accepted: {text}"
    );
    assert!(text.contains("\"status\":\"ok\""), "{text}");
}

#[test]
fn album_list2_refuses_schema_unservable_types_with_code_zero() {
    let app = SubsonicApp::new(MemStore::seeded());
    // Current v1 catalogue serves newest/random/byYear/alphabeticalByName/
    // alphabeticalByArtist; frequent/recent/starred/highest/byGenre refuse
    // with error 0 (UnsupportedAlbumListTypeException).
    for list_type in ["frequent", "recent", "starred", "highest", "byGenre"] {
        let extra = if list_type == "byGenre" {
            "&genre=Rock"
        } else {
            ""
        };
        let body = get(
            &app,
            "/rest/getAlbumList2",
            &format!("{KEY}&type={list_type}{extra}&f=json"),
        );
        let text = String::from_utf8(body).unwrap();
        assert!(
            text.contains("\"code\":0"),
            "{list_type} must refuse with 0: {text}"
        );
    }
    // The legacy list still serves frequent/recent from the scrobble log.
    for list_type in ["frequent", "recent"] {
        let body = get(
            &app,
            "/rest/getAlbumList",
            &format!("{KEY}&type={list_type}&f=json"),
        );
        let text = String::from_utf8(body).unwrap();
        assert!(
            text.contains("\"status\":\"ok\""),
            "legacy {list_type}: {text}"
        );
        assert!(!text.contains("\"code\":0"), "legacy {list_type}: {text}");
    }
}

fn hash_order(ids: &[&str]) -> Vec<String> {
    let mut order: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    order.sort_by_key(|id| {
        id.bytes().fold(0u64, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(u64::from(byte))
        })
    });
    order
}

#[test]
fn random_album_list_is_not_a_fixed_hash_order() {
    // Five albums: the fixed id-hash order must not pin the random list
    // (v1 uses inRandomOrder). A shuffle matching the hash order exactly
    // is possible but negligible (1/120).
    let artists = vec![Artist {
        id: "ar-1".into(),
        name: "Test Artist".into(),
    }];
    let albums: Vec<Album> = (0..5)
        .map(|i| Album {
            id: format!("al-{i}"),
            name: format!("Album {i}"),
            artist: "Test Artist".into(),
            artist_id: "ar-1".into(),
            duration: 200,
            created: NOW.into(),
        })
        .collect();
    let songs: Vec<Song> = (0..5)
        .map(|i| Song {
            id: format!("so-{i}"),
            title: format!("Song {i}"),
            album: format!("Album {i}"),
            artist: "Test Artist".into(),
            track: 1,
            year: 2026,
            genre: "Rock".into(),
            cover_art: format!("al-{i}"),
            size: 1000,
            content_type: "audio/mpeg".into(),
            suffix: "mp3".into(),
            duration: 200,
            disc: 1,
            created: NOW.into(),
            album_id: format!("al-{i}"),
            artist_id: "ar-1".into(),
        })
        .collect();
    let (store, _) = TestStore::new(artists, albums, songs, BTreeMap::new());
    let app = SubsonicApp::new(store);
    let text = body_text(
        &app,
        "/rest/getAlbumList2",
        &format!("{KEY}&type=random&size=5&f=json"),
    );
    // Reconstruct response order from positions.
    let mut positioned: Vec<(&str, usize)> = ["al-0", "al-1", "al-2", "al-3", "al-4"]
        .iter()
        .map(|id| (*id, text.find(&format!("\"id\":\"{id}\"")).expect(&text)))
        .collect();
    positioned.sort_by_key(|(_, pos)| *pos);
    let got: Vec<String> = positioned
        .into_iter()
        .map(|(id, _)| id.to_string())
        .collect();
    assert_eq!(got.len(), 5, "{text}");
    assert_ne!(
        got,
        hash_order(&["al-0", "al-1", "al-2", "al-3", "al-4"]),
        "random must not be the fixed hash order: {text}"
    );
    let _ = got;
}

#[test]
fn random_songs_are_not_fixed_id_order() {
    let artists = vec![Artist {
        id: "ar-1".into(),
        name: "Test Artist".into(),
    }];
    let albums = vec![Album {
        id: "al-1".into(),
        name: "Test Album".into(),
        artist: "Test Artist".into(),
        artist_id: "ar-1".into(),
        duration: 200,
        created: NOW.into(),
    }];
    let songs: Vec<Song> = (0..5)
        .map(|i| Song {
            id: format!("so-{i}"),
            title: format!("Song {i}"),
            album: "Test Album".into(),
            artist: "Test Artist".into(),
            track: i as i64 + 1,
            year: 2026,
            genre: "Rock".into(),
            cover_art: "al-1".into(),
            size: 1000,
            content_type: "audio/mpeg".into(),
            suffix: "mp3".into(),
            duration: 200,
            disc: 1,
            created: NOW.into(),
            album_id: "al-1".into(),
            artist_id: "ar-1".into(),
        })
        .collect();
    let (store, _) = TestStore::new(artists, albums, songs, BTreeMap::new());
    let app = SubsonicApp::new(store);
    let text = body_text(
        &app,
        "/rest/getRandomSongs",
        &format!("{KEY}&size=5&f=json"),
    );
    let mut positioned: Vec<(&str, usize)> = ["so-0", "so-1", "so-2", "so-3", "so-4"]
        .iter()
        .map(|id| (*id, text.find(&format!("\"id\":\"{id}\"")).expect(&text)))
        .collect();
    positioned.sort_by_key(|(_, pos)| *pos);
    let got: Vec<String> = positioned
        .into_iter()
        .map(|(id, _)| id.to_string())
        .collect();
    assert_ne!(
        got,
        vec!["so-0", "so-1", "so-2", "so-3", "so-4"],
        "random songs must not be fixed id order: {text}"
    );
}

// --- index letters (v1 IndexesResource::indexLetter) ---

fn index_names(json_text: &str) -> Vec<String> {
    // Extract index bucket names: `"name":"X","artist"` marks an index entry,
    // while artist items carry `"name":"X","albumCount"`.
    let mut out = Vec::new();
    let mut rest = json_text;
    while let Some(pos) = rest.find("\"name\":\"") {
        rest = &rest[pos + 8..];
        if let Some(end) = rest.find('"') {
            let value = &rest[..end];
            let tail = &rest[end..];
            if tail.starts_with("\",\"artist\"") {
                out.push(value.to_string());
            }
            rest = &rest[end..];
        } else {
            break;
        }
    }
    out
}

#[test]
fn index_letters_group_non_ascii_under_hash() {
    let artists = vec![
        ("ar-ascii", "apple"),
        ("ar-beatles", "The Beatles"),
        ("ar-who", "the who"),
        ("ar-accent", "Émilie"),
        ("ar-cyrillic", "Жуки"),
        ("ar-cjk", "音楽"),
        ("ar-numeric", "123 Band"),
    ]
    .into_iter()
    .map(|(id, name)| Artist {
        id: id.into(),
        name: name.into(),
    })
    .collect::<Vec<_>>();
    let (store, _) = TestStore::new(artists, Vec::new(), Vec::new(), BTreeMap::new());
    let app = SubsonicApp::new(store);
    let text = body_text(&app, "/rest/getArtists", &format!("{KEY}&f=json"));
    let names = index_names(&text);
    // ASCII uppercased, leading articles stripped, every non-ASCII initial
    // filed under '#' (v1 ctype_alpha is byte-based).
    assert!(names.contains(&"A".to_string()), "{text}");
    assert!(names.contains(&"B".to_string()), "{text}");
    assert!(names.contains(&"W".to_string()), "{text}");
    assert!(names.contains(&"#".to_string()), "{text}");
    assert_eq!(
        names,
        vec!["#", "A", "B", "W"],
        "one bucket per non-ASCII initial is wrong: {text}"
    );
}

// --- getIndexes lastModified ---

#[test]
fn memstore_indexes_keep_the_fixed_fixture_time() {
    let app = SubsonicApp::new(MemStore::seeded());
    let body = get(&app, "/rest/getIndexes", &format!("{KEY}&f=json"));
    let text = String::from_utf8(body).unwrap();
    assert!(
        text.contains("\"lastModified\":1767225600000"),
        "seeded store keeps the fixture time: {text}"
    );
}

// --- worker body handling (transport-free part) ---

#[test]
fn null_body_form_post_processes_the_query() {
    let app = SubsonicApp::new(MemStore::seeded());
    // A form POST carrying params in the query string with an empty body
    // must be processed, not rejected.
    let res = app.handle("POST", "/rest/ping", KEY, "");
    assert_eq!(res.status, 200);
    assert!(String::from_utf8(res.body)
        .unwrap()
        .contains("status=\"ok\""));
}

#[test]
fn oversized_body_answers_error_10_envelope() {
    let res = nntune_subsonic::oversized_body_response(&format!("{KEY}&f=json"));
    assert_eq!(res.status, 200);
    let text = String::from_utf8(res.body).unwrap();
    assert!(text.contains("\"code\":10"), "{text}");
    assert!(
        text.contains("POST body exceeds the 10 MB limit."),
        "{text}"
    );
}

#[test]
fn post_byte_cap_remains_enforced() {
    let app = SubsonicApp::new(MemStore::seeded());
    let big = "x".repeat(nntune_subsonic::MAX_BODY_BYTES + 1);
    let res = app.handle("POST", "/rest/ping", &format!("{KEY}&f=json"), &big);
    assert_eq!(res.status, 200);
    let text = String::from_utf8(res.body).unwrap();
    assert!(text.contains("\"code\":10"), "{text}");
}

#[test]
fn other_user_key_authenticates() {
    let (store, _) = TestStore::single_song_lyrics("La la la");
    let app = SubsonicApp::new(store);
    let text = body_text(&app, "/rest/ping", OTHER_KEY);
    assert!(text.contains("status=\"ok\""), "{text}");
}
