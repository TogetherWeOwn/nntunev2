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

/// Configurable store that delegates sampling and history to the real
/// in-memory store instead of reimplementing shuffle or play-history logic.
/// Catalogue reads, album lists, random songs and scrobble history all come
/// from `inner`; only lyrics stay custom and the scrobble spy counts calls.
/// Shared scrobble-spy handle for the test store.
type ScrobbleLog = Rc<RefCell<Vec<(String, String)>>>;

#[derive(Clone, Debug)]
struct TestStore {
    inner: MemStore,
    lyrics: BTreeMap<(String, String), String>,
    scrobble_log: ScrobbleLog,
}

impl TestStore {
    fn new(
        artists: Vec<Artist>,
        albums: Vec<Album>,
        songs: Vec<Song>,
        lyrics: BTreeMap<(String, String), String>,
    ) -> (TestStore, ScrobbleLog) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let inner = MemStore::empty_with_test_auth().with_catalogue(artists, albums, songs);
        (
            TestStore {
                inner,
                lyrics,
                scrobble_log: Rc::clone(&log),
            },
            log,
        )
    }

    fn single_song_lyrics(raw: &str) -> (TestStore, ScrobbleLog) {
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
            year: Some(2026),
            release_date: Some("2026-01-01".into()),
            curated_at: Some(NOW.into()),
            artist_sort_name: "Test Artist".into(),
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

    fn two_albums() -> (TestStore, ScrobbleLog) {
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
                year: Some(2026),
                release_date: Some("2026-01-01".into()),
                curated_at: Some(NOW.into()),
                artist_sort_name: "Test Artist".into(),
            },
            Album {
                id: "al-b".into(),
                name: "Album B".into(),
                artist: "Test Artist".into(),
                artist_id: "ar-1".into(),
                duration: 200,
                created: NOW.into(),
                year: Some(2026),
                release_date: Some("2026-01-02".into()),
                curated_at: Some(NOW.into()),
                artist_sort_name: "Test Artist".into(),
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
        self.inner.artists()
    }

    fn artists_last_modified(&self) -> i64 {
        self.inner.artists_last_modified()
    }

    fn artist(&self, id: &str) -> Option<Artist> {
        self.inner.artist(id)
    }

    fn albums_for_artist(&self, artist_id: &str) -> Vec<Album> {
        self.inner.albums_for_artist(artist_id)
    }

    fn album(&self, id: &str) -> Option<Album> {
        self.inner.album(id)
    }

    fn all_albums(&self) -> Vec<Album> {
        self.inner.all_albums()
    }

    fn album_list(&self, query: AlbumListQuery<'_>) -> Vec<Album> {
        self.inner.album_list(query)
    }

    fn album_list2(&self, query: AlbumListQuery<'_>) -> Result<Vec<Album>, (i64, String)> {
        self.inner.album_list2(query)
    }

    fn random_songs(&self, identity: &Identity, size: usize) -> Vec<Song> {
        self.inner.random_songs(identity, size)
    }

    fn songs_for_album(&self, album_id: &str) -> Vec<Song> {
        self.inner.songs_for_album(album_id)
    }

    fn song(&self, id: &str) -> Option<Song> {
        self.inner.song(id)
    }

    fn all_songs(&self) -> Vec<Song> {
        self.inner.all_songs()
    }

    fn genres(&self) -> Vec<(String, i64, i64)> {
        self.inner.genres()
    }

    fn audio(&self, song_id: &str) -> Option<Vec<u8>> {
        self.inner.audio(song_id)
    }

    fn cover(&self, _cover_id: &str) -> Option<Vec<u8>> {
        self.inner.cover(_cover_id)
    }

    fn avatar(&self, _username: &str) -> Option<Vec<u8>> {
        self.inner.avatar(_username)
    }

    fn lyrics(&self, artist: &str, title: &str) -> Option<String> {
        self.lyrics
            .get(&(artist.to_string(), title.to_string()))
            .cloned()
    }

    fn playlists(&self, identity: &Identity) -> Vec<Playlist> {
        self.inner.playlists(identity)
    }

    fn playlist(&self, identity: &Identity, id: &str) -> Option<Playlist> {
        self.inner.playlist(identity, id)
    }

    fn create_playlist(&mut self, identity: &Identity, name: String) -> Playlist {
        self.inner.create_playlist(identity, name)
    }

    fn update_playlist(
        &mut self,
        identity: &Identity,
        id: &str,
        name: Option<String>,
        add: Vec<String>,
        remove: Vec<usize>,
    ) -> Result<(), (i64, String)> {
        self.inner.update_playlist(identity, id, name, add, remove)
    }

    fn delete_playlist(&mut self, identity: &Identity, id: &str) -> Result<(), (i64, String)> {
        self.inner.delete_playlist(identity, id)
    }

    fn star(&mut self, identity: &Identity, kind: StarKind, id: &str) -> bool {
        self.inner.star(identity, kind, id)
    }

    fn unstar(&mut self, identity: &Identity, kind: StarKind, id: &str) {
        self.inner.unstar(identity, kind, id)
    }

    fn starred(&self, identity: &Identity, kind: StarKind) -> Vec<String> {
        self.inner.starred(identity, kind)
    }

    fn set_rating(&mut self, identity: &Identity, id: &str, rating: i64) -> bool {
        self.inner.set_rating(identity, id, rating)
    }

    fn rating(&self, identity: &Identity, id: &str) -> Option<i64> {
        self.inner.rating(identity, id)
    }

    fn scrobble(&mut self, identity: &Identity, song_id: &str) -> bool {
        if self.inner.song(song_id).is_none() {
            return false;
        }
        self.scrobble_log
            .borrow_mut()
            .push((identity.username.clone(), song_id.to_string()));
        self.inner.scrobble(identity, song_id)
    }

    fn queue(&self, identity: &Identity) -> Option<PlayQueue> {
        self.inner.queue(identity)
    }

    fn save_queue(&mut self, identity: &Identity, queue: PlayQueue) {
        self.inner.save_queue(identity, queue)
    }
}

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
    // The catalogue list answers ok with an empty page for a reversed range
    // (v1 StreamableCatalogue keeps the ascending bound, so nothing matches).
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
    assert!(
        !text.contains("al-1"),
        "reversed catalogue range is empty: {text}"
    );
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
        assert!(
            text.contains(&format!("Unsupported album list type: {list_type}")),
            "{list_type} must carry the exact refusal text: {text}"
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

fn eight_album_catalogue() -> (Vec<Artist>, Vec<Album>, Vec<Song>) {
    let artists = vec![Artist {
        id: "ar-1".into(),
        name: "Test Artist".into(),
    }];
    let albums: Vec<Album> = (0..8)
        .map(|i| Album {
            id: format!("al-{i}"),
            name: format!("Album {i}"),
            artist: "Test Artist".into(),
            artist_id: "ar-1".into(),
            duration: 200,
            created: NOW.into(),
            year: Some(2026),
            release_date: Some("2026-01-01".into()),
            curated_at: Some(NOW.into()),
            artist_sort_name: "Test Artist".into(),
        })
        .collect();
    let songs: Vec<Song> = (0..8)
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
    (artists, albums, songs)
}

fn album_order_in_list2(text: &str, ids: &[&str]) -> Vec<String> {
    let mut positioned: Vec<(&str, usize)> = ids
        .iter()
        .map(|id| (*id, text.find(&format!("\"id\":\"{id}\"")).expect(text)))
        .collect();
    positioned.sort_by_key(|(_, pos)| *pos);
    positioned
        .into_iter()
        .map(|(id, _)| id.to_string())
        .collect()
}

fn song_order_in_random_songs(text: &str, ids: &[&str]) -> Vec<String> {
    album_order_in_list2(text, ids)
}

#[test]
fn random_album_list_is_seeded_and_successive_samples_vary() {
    let ids = [
        "al-0", "al-1", "al-2", "al-3", "al-4", "al-5", "al-6", "al-7",
    ];
    let (artists, albums, songs) = eight_album_catalogue();
    let first = {
        let store = MemStore::empty_with_test_auth()
            .with_catalogue(artists.clone(), albums.clone(), songs.clone())
            .with_random_seed(7);
        let app = SubsonicApp::new(store);
        let body = get(
            &app,
            "/rest/getAlbumList2",
            &format!("{KEY}&type=random&size=8&f=json"),
        );
        String::from_utf8(body).unwrap()
    };
    let first_order = album_order_in_list2(&first, &ids);
    assert_eq!(first_order.len(), 8, "{first}");
    // Same seed and catalogue reproduce the same shuffle.
    let (replay_artists, replay_albums, replay_songs) = eight_album_catalogue();
    let replay_store = MemStore::empty_with_test_auth()
        .with_catalogue(replay_artists, replay_albums, replay_songs)
        .with_random_seed(7);
    let replay_app = SubsonicApp::new(replay_store);
    let replay_body = get(
        &replay_app,
        "/rest/getAlbumList2",
        &format!("{KEY}&type=random&size=8&f=json"),
    );
    let replay_text = String::from_utf8(replay_body).unwrap();
    assert_eq!(
        album_order_in_list2(&replay_text, &ids),
        first_order,
        "same seed must reproduce the shuffle: {replay_text}"
    );
    // Successive samples from one store advance the sequence: across several
    // draws at least two distinct orders appear (eight items, so a stuck
    // shuffle would be a real bug, not chance).
    let store = MemStore::empty_with_test_auth()
        .with_catalogue(artists, albums, songs)
        .with_random_seed(7);
    let app = SubsonicApp::new(store);
    let mut orders = std::collections::HashSet::new();
    for _ in 0..10 {
        let body = get(
            &app,
            "/rest/getAlbumList2",
            &format!("{KEY}&type=random&size=8&f=json"),
        );
        let text = String::from_utf8(body).unwrap();
        let order = album_order_in_list2(&text, &ids);
        assert_eq!(order.len(), 8, "{text}");
        // Every sample keeps the full set; only the order varies.
        let mut sorted = order.clone();
        sorted.sort();
        let mut expected: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(sorted, expected, "{text}");
        orders.insert(order.join(","));
    }
    assert!(
        orders.len() > 1,
        "successive shuffles must advance across draws"
    );
    let _ = first_order;
}

#[test]
fn random_songs_are_seeded_and_successive_samples_vary() {
    let ids = [
        "so-0", "so-1", "so-2", "so-3", "so-4", "so-5", "so-6", "so-7",
    ];
    let (artists, albums, songs) = eight_album_catalogue();
    let first_store = MemStore::empty_with_test_auth()
        .with_catalogue(artists.clone(), albums.clone(), songs.clone())
        .with_random_seed(11);
    let first_app = SubsonicApp::new(first_store);
    let first_body = get(
        &first_app,
        "/rest/getRandomSongs",
        &format!("{KEY}&size=8&f=json"),
    );
    let first_text = String::from_utf8(first_body).unwrap();
    let first_order = song_order_in_random_songs(&first_text, &ids);
    assert_eq!(first_order.len(), 8, "{first_text}");
    let (replay_artists, replay_albums, replay_songs) = eight_album_catalogue();
    let replay_store = MemStore::empty_with_test_auth()
        .with_catalogue(replay_artists, replay_albums, replay_songs)
        .with_random_seed(11);
    let replay_app = SubsonicApp::new(replay_store);
    let replay_body = get(
        &replay_app,
        "/rest/getRandomSongs",
        &format!("{KEY}&size=8&f=json"),
    );
    let replay_text = String::from_utf8(replay_body).unwrap();
    assert_eq!(
        song_order_in_random_songs(&replay_text, &ids),
        first_order,
        "same seed must reproduce song sampling: {replay_text}"
    );
    let store = MemStore::empty_with_test_auth()
        .with_catalogue(artists, albums, songs)
        .with_random_seed(11);
    let app = SubsonicApp::new(store);
    let mut orders = std::collections::HashSet::new();
    for _ in 0..10 {
        let body = get(
            &app,
            "/rest/getRandomSongs",
            &format!("{KEY}&size=8&f=json"),
        );
        let text = String::from_utf8(body).unwrap();
        let order = song_order_in_random_songs(&text, &ids);
        assert_eq!(order.len(), 8, "{text}");
        orders.insert(order.join(","));
    }
    assert!(
        orders.len() > 1,
        "successive song samples must vary across draws"
    );
}

#[test]
fn album_list_history_is_per_user_and_recent_orders_by_recency() {
    let (store, _) = TestStore::two_albums();
    let app = SubsonicApp::new(store);
    // Test plays A once and B twice; other plays only A.
    for (key, song) in [
        (KEY, "so-a"),
        (KEY, "so-b"),
        (KEY, "so-b"),
        (OTHER_KEY, "so-a"),
    ] {
        let text = body_text(
            &app,
            "/rest/scrobble",
            &format!("{key}&id={song}&submission=true&f=json"),
        );
        assert!(text.contains("\"status\":\"ok\""), "{text}");
    }
    // Frequent is per-user: test ranks B first (2 plays), other ranks A first.
    let text = body_text(
        &app,
        "/rest/getAlbumList",
        &format!("{KEY}&type=frequent&f=json"),
    );
    let pos_a = text.find("\"id\":\"al-a\"").expect("{text}");
    let pos_b = text.find("\"id\":\"al-b\"").expect("{text}");
    assert!(pos_b < pos_a, "test frequent ranks B first: {text}");
    let text = body_text(
        &app,
        "/rest/getAlbumList",
        &format!("{OTHER_KEY}&type=frequent&f=json"),
    );
    let pos_a = text.find("\"id\":\"al-a\"").expect("{text}");
    let pos_b = text.find("\"id\":\"al-b\"").expect("{text}");
    assert!(pos_a < pos_b, "other frequent is isolated: {text}");
    // Recent orders by recency for test (B last) and isolates the other user
    // to only the album they played.
    let text = body_text(
        &app,
        "/rest/getAlbumList",
        &format!("{KEY}&type=recent&f=json"),
    );
    let pos_a = text.find("\"id\":\"al-a\"").expect("{text}");
    let pos_b = text.find("\"id\":\"al-b\"").expect("{text}");
    assert!(pos_b < pos_a, "test recent puts B first: {text}");
    let text = body_text(
        &app,
        "/rest/getAlbumList",
        &format!("{OTHER_KEY}&type=recent&f=json"),
    );
    assert!(text.contains("al-a"), "{text}");
    assert!(
        !text.contains("al-b"),
        "recent excludes never-played albums: {text}"
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
fn unreadable_body_answers_error_10_in_json_xml_and_jsonp() {
    let json = nntune_subsonic::unreadable_body_response(&format!("{KEY}&f=json"));
    assert_eq!(json.status, 200);
    assert!(
        json.content_type.contains("application/json"),
        "{}",
        json.content_type
    );
    let text = String::from_utf8(json.body).unwrap();
    assert!(text.contains("\"code\":10"), "{text}");
    assert!(text.contains("Unable to read request body."), "{text}");
    let xml = nntune_subsonic::unreadable_body_response(KEY);
    assert_eq!(xml.status, 200);
    assert!(
        xml.content_type.contains("application/xml"),
        "{}",
        xml.content_type
    );
    let text = String::from_utf8(xml.body).unwrap();
    assert!(text.contains("code=\"10\""), "{text}");
    assert!(text.contains("Unable to read request body."), "{text}");
    let jsonp = nntune_subsonic::unreadable_body_response(&format!("{KEY}&f=jsonp&callback=cb"));
    assert_eq!(jsonp.status, 200);
    assert!(
        jsonp.content_type.contains("text/javascript"),
        "{}",
        jsonp.content_type
    );
    let text = String::from_utf8(jsonp.body).unwrap();
    assert!(text.starts_with("/**/cb("), "{text}");
    assert!(text.contains("\"code\":10"), "{text}");
    assert!(text.contains("Unable to read request body."), "{text}");
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

// --- edge cases: metadata-only lyrics, starred order, catalogue sorts ---
//
// Source-verified against v1 (`GetLyricsBySongIdController:18-25` answers
// `lyricsList=[]` when `ParsedLyrics.lines` is empty; starred repositories
// order by `favorites.position`; the catalogue `newest` sorts `curated_at`
// and `byYear` sorts `release_date DESC, curated_at DESC` with Postgres
// `DESC NULLS FIRST`; catalogue `alphabeticalByArtist` sorts
// `artist_sort_name` then album name while the legacy list uses the display
// artist). Sorting and history come from the real `MemStore` via
// `with_catalogue`; nothing here reimplements a sort or shuffle.

/// Album builder with catalogue defaults; callers override the fields their
/// case pins (`created`, `year`, `release_date`, `curated_at`,
/// `artist_sort_name`).
fn edge_album(id: &str, name: &str, artist: &str, artist_id: &str) -> Album {
    Album {
        id: id.into(),
        name: name.into(),
        artist: artist.into(),
        artist_id: artist_id.into(),
        duration: 200,
        created: NOW.into(),
        year: Some(2026),
        release_date: Some("2026-01-01".into()),
        curated_at: Some(NOW.into()),
        artist_sort_name: artist.into(),
    }
}

/// One streamable song per album so catalogue lists keep the album.
fn edge_song(id: &str, album_id: &str, album_name: &str, artist_id: &str) -> Song {
    Song {
        id: id.into(),
        title: format!("Song {id}"),
        album: album_name.into(),
        artist: "Test Artist".into(),
        track: 1,
        year: 2026,
        genre: "Rock".into(),
        cover_art: album_id.into(),
        size: 1000,
        content_type: "audio/mpeg".into(),
        suffix: "mp3".into(),
        duration: 200,
        disc: 1,
        created: NOW.into(),
        album_id: album_id.into(),
        artist_id: artist_id.into(),
    }
}

fn id_order(text: &str, ids: &[&str]) -> Vec<String> {
    album_order_in_list2(text, ids)
}

/// Response text for any store type (`body_text` is pinned to `TestStore`).
fn store_text<S: Library>(app: &SubsonicApp<S>, path: &str, query: &str) -> String {
    String::from_utf8(get(app, path, query)).unwrap()
}

#[test]
fn lyrics_metadata_only_input_answers_empty_lyrics_list() {
    // No lyric lines survive: tag-only and offset-only payloads must answer
    // `lyricsList=[]`, never `structuredLyrics:[{line:[]}]`.
    for raw in [
        "[ar:Test Artist]\n[ti:Test Song]\n[al:Test Album]",
        "[ar:Test Artist]",
        "[offset:500]",
        "[OFFSET: 500 ]",
        "[ar:Test Artist]\n[offset:500]\n[ti:Test Song]",
    ] {
        let text = lyrics_body(raw);
        assert!(text.contains("\"lyricsList\":[]"), "{raw} -> {text}");
        assert!(!text.contains("structuredLyrics"), "{raw} -> {text}");
        assert!(!text.contains("\"line\""), "{raw} -> {text}");
    }
}

fn starred_catalogue() -> (Vec<Artist>, Vec<Album>, Vec<Song>) {
    let artists = vec![
        Artist {
            id: "ar-1".into(),
            name: "Artist One".into(),
        },
        Artist {
            id: "ar-2".into(),
            name: "Artist Two".into(),
        },
        Artist {
            id: "ar-3".into(),
            name: "Artist Three".into(),
        },
    ];
    let albums = vec![
        edge_album("al-1", "Album One", "Artist One", "ar-1"),
        edge_album("al-2", "Album Two", "Artist Two", "ar-2"),
        edge_album("al-3", "Album Three", "Artist Three", "ar-3"),
    ];
    let songs = vec![
        edge_song("so-1", "al-1", "Album One", "ar-1"),
        edge_song("so-2", "al-2", "Album Two", "ar-2"),
        edge_song("so-3", "al-3", "Album Three", "ar-3"),
    ];
    (artists, albums, songs)
}

fn starred_app() -> SubsonicApp<TestStore> {
    let (artists, albums, songs) = starred_catalogue();
    let (store, _) = TestStore::new(artists, albums, songs, BTreeMap::new());
    SubsonicApp::new(store)
}

#[test]
fn starred_kinds_preserve_favorite_insertion_order() {
    // Favorites inserted in non-id order must come back in that favorite
    // order from both getStarred and getStarred2, for every kind. Starring
    // through the non-`id` params (`artistId=`, `albumId=`) counts as
    // insertion too.
    let app = starred_app();
    for (param, value) in [
        ("artistId", "ar-3"),
        ("artistId", "ar-1"),
        ("artistId", "ar-2"),
        ("albumId", "al-3"),
        ("albumId", "al-1"),
        ("albumId", "al-2"),
        ("id", "so-3"),
        ("id", "so-1"),
        ("id", "so-2"),
    ] {
        let text = body_text(&app, "/rest/star", &format!("{KEY}&{param}={value}&f=json"));
        assert!(
            text.contains("\"status\":\"ok\""),
            "{param}={value}: {text}"
        );
    }
    for path in ["/rest/getStarred", "/rest/getStarred2"] {
        let text = body_text(&app, path, &format!("{KEY}&f=json"));
        assert_eq!(
            id_order(&text, &["ar-3", "ar-1", "ar-2"]),
            vec!["ar-3".to_string(), "ar-1".to_string(), "ar-2".to_string()],
            "{path} artists: {text}"
        );
        assert_eq!(
            id_order(&text, &["al-3", "al-1", "al-2"]),
            vec!["al-3".to_string(), "al-1".to_string(), "al-2".to_string()],
            "{path} albums: {text}"
        );
        assert_eq!(
            id_order(&text, &["so-3", "so-1", "so-2"]),
            vec!["so-3".to_string(), "so-1".to_string(), "so-2".to_string()],
            "{path} songs: {text}"
        );
    }
}

#[test]
fn legacy_starred_album_list_preserves_favorite_position() {
    let app = starred_app();
    for album in ["al-3", "al-1", "al-2"] {
        let text = body_text(&app, "/rest/star", &format!("{KEY}&albumId={album}&f=json"));
        assert!(text.contains("\"status\":\"ok\""), "{album}: {text}");
    }
    let text = body_text(
        &app,
        "/rest/getAlbumList",
        &format!("{KEY}&type=starred&f=json"),
    );
    assert_eq!(
        id_order(&text, &["al-3", "al-1", "al-2"]),
        vec!["al-3".to_string(), "al-1".to_string(), "al-2".to_string()],
        "legacy starred must follow favorite position: {text}"
    );
}

#[test]
fn restar_moves_existing_favorite_to_end_and_unstar_restars_at_end() {
    let app = starred_app();
    for artist in ["ar-3", "ar-1", "ar-2"] {
        let text = body_text(
            &app,
            "/rest/star",
            &format!("{KEY}&artistId={artist}&f=json"),
        );
        assert!(text.contains("\"status\":\"ok\""), "{artist}: {text}");
    }
    // v1 FavoriteService::batchFavorite assigns max(position)+1 per target and
    // the upsert overwrites position, so re-star MOVES the existing favorite
    // to the end while staying deduplicated: star A, star B, star A => [B, A].
    let text = body_text(&app, "/rest/star", &format!("{KEY}&artistId=ar-1&f=json"));
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    let text = body_text(&app, "/rest/getStarred2", &format!("{KEY}&f=json"));
    assert_eq!(
        id_order(&text, &["ar-3", "ar-2", "ar-1"]),
        vec!["ar-3".to_string(), "ar-2".to_string(), "ar-1".to_string()],
        "re-star must move the favorite to the end: {text}"
    );
    assert_eq!(
        text.matches("\"id\":\"ar-1\"").count(),
        1,
        "re-star must not duplicate the favorite: {text}"
    );
    // Unstar then re-star takes a fresh position at the end.
    let text = body_text(&app, "/rest/unstar", &format!("{KEY}&artistId=ar-3&f=json"));
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    let text = body_text(&app, "/rest/star", &format!("{KEY}&artistId=ar-3&f=json"));
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    let text = body_text(&app, "/rest/getStarred2", &format!("{KEY}&f=json"));
    assert_eq!(
        id_order(&text, &["ar-2", "ar-1", "ar-3"]),
        vec!["ar-2".to_string(), "ar-1".to_string(), "ar-3".to_string()],
        "re-starred favorite moves to the end: {text}"
    );
}

#[test]
fn restar_batch_with_existing_and_new_applies_in_request_order() {
    // Mixed batches apply sequentially: each target takes max(position)+1, so
    // an existing favorite moves to the end before later targets append after
    // it. Minimal case first: star A, star B, star A => [B, A].
    let app = starred_app();
    for artist in ["ar-1", "ar-2"] {
        let text = body_text(
            &app,
            "/rest/star",
            &format!("{KEY}&artistId={artist}&f=json"),
        );
        assert!(text.contains("\"status\":\"ok\""), "{artist}: {text}");
    }
    let text = body_text(
        &app,
        "/rest/star",
        &format!("{KEY}&artistId=ar-1&artistId=ar-3&f=json"),
    );
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    let text = body_text(&app, "/rest/getStarred2", &format!("{KEY}&f=json"));
    assert_eq!(
        id_order(&text, &["ar-2", "ar-1", "ar-3"]),
        vec!["ar-2".to_string(), "ar-1".to_string(), "ar-3".to_string()],
        "batch [existing, new] must move existing then append new: {text}"
    );
    // Reverse request order appends the new favorite before moving the
    // existing one to the very end.
    let app = starred_app();
    for artist in ["ar-1", "ar-2"] {
        let text = body_text(
            &app,
            "/rest/star",
            &format!("{KEY}&artistId={artist}&f=json"),
        );
        assert!(text.contains("\"status\":\"ok\""), "{artist}: {text}");
    }
    let text = body_text(
        &app,
        "/rest/star",
        &format!("{KEY}&artistId=ar-3&artistId=ar-1&f=json"),
    );
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    let text = body_text(&app, "/rest/getStarred2", &format!("{KEY}&f=json"));
    assert_eq!(
        id_order(&text, &["ar-2", "ar-3", "ar-1"]),
        vec!["ar-2".to_string(), "ar-3".to_string(), "ar-1".to_string()],
        "batch [new, existing] must append new then move existing: {text}"
    );
}

#[test]
fn starred_state_is_per_user() {
    let app = starred_app();
    let text = body_text(&app, "/rest/star", &format!("{KEY}&artistId=ar-1&f=json"));
    assert!(text.contains("\"status\":\"ok\""), "{text}");
    // The other user stars in a different order; neither side leaks.
    for artist in ["ar-2", "ar-3"] {
        let text = body_text(
            &app,
            "/rest/star",
            &format!("{OTHER_KEY}&artistId={artist}&f=json"),
        );
        assert!(text.contains("\"status\":\"ok\""), "{artist}: {text}");
    }
    let text = body_text(&app, "/rest/getStarred2", &format!("{KEY}&f=json"));
    assert!(text.contains("ar-1"), "{text}");
    assert!(!text.contains("ar-2"), "{text}");
    assert!(!text.contains("ar-3"), "{text}");
    let text = body_text(&app, "/rest/getStarred2", &format!("{OTHER_KEY}&f=json"));
    assert_eq!(
        id_order(&text, &["ar-2", "ar-3"]),
        vec!["ar-2".to_string(), "ar-3".to_string()],
        "other user's own order is kept: {text}"
    );
}

#[test]
fn album_list2_newest_orders_by_curated_at_not_created() {
    // `created` and `curated_at` disagree on purpose; the catalogue follows
    // `curated_at` (Postgres `DESC NULLS FIRST`), the legacy list keeps
    // following `created`.
    let artists = vec![Artist {
        id: "ar-1".into(),
        name: "Test Artist".into(),
    }];
    let mut al_old = edge_album("al-old", "Old Created", "Test Artist", "ar-1");
    al_old.created = "2026-03-01T00:00:00".into();
    al_old.curated_at = Some("2026-01-01T00:00:00".into());
    let mut al_new = edge_album("al-new", "New Created", "Test Artist", "ar-1");
    al_new.created = "2026-01-01T00:00:00".into();
    al_new.curated_at = Some("2026-03-01T00:00:00".into());
    let mut al_null = edge_album("al-null", "Null Curated", "Test Artist", "ar-1");
    al_null.created = "2026-02-01T00:00:00".into();
    al_null.curated_at = None;
    let albums = vec![al_old, al_new, al_null];
    let songs = vec![
        edge_song("so-old", "al-old", "Old Created", "ar-1"),
        edge_song("so-new", "al-new", "New Created", "ar-1"),
        edge_song("so-null", "al-null", "Null Curated", "ar-1"),
    ];
    let store = MemStore::empty_with_test_auth().with_catalogue(artists, albums, songs);
    let app = SubsonicApp::new(store);
    let text = store_text(
        &app,
        "/rest/getAlbumList2",
        &format!("{KEY}&type=newest&f=json"),
    );
    assert_eq!(
        id_order(&text, &["al-old", "al-new", "al-null"]),
        vec![
            "al-null".to_string(),
            "al-new".to_string(),
            "al-old".to_string()
        ],
        "List2 newest sorts curated_at, nulls first: {text}"
    );
    let text = store_text(
        &app,
        "/rest/getAlbumList",
        &format!("{KEY}&type=newest&f=json"),
    );
    assert_eq!(
        id_order(&text, &["al-old", "al-new", "al-null"]),
        vec![
            "al-old".to_string(),
            "al-null".to_string(),
            "al-new".to_string()
        ],
        "legacy newest keeps created order: {text}"
    );
}

#[test]
fn album_list2_by_year_orders_release_date_then_curated_at_nulls_first() {
    // Every eligible album shares `year`; `release_date` decides first and
    // `curated_at` breaks ties, both Postgres `DESC NULLS FIRST`. The 1999
    // album stays out of the 2020-2030 window.
    let artists = vec![Artist {
        id: "ar-1".into(),
        name: "Test Artist".into(),
    }];
    let mut al_none = edge_album("al-none", "No Release", "Test Artist", "ar-1");
    al_none.release_date = None;
    al_none.curated_at = None;
    let mut al_tie_loose = edge_album("al-tie-loose", "Tie Loose", "Test Artist", "ar-1");
    al_tie_loose.release_date = Some("2026-05-01".into());
    al_tie_loose.curated_at = None;
    let mut al_tie_set = edge_album("al-tie-set", "Tie Set", "Test Artist", "ar-1");
    al_tie_set.release_date = Some("2026-05-01".into());
    al_tie_set.curated_at = Some("2026-02-01T00:00:00".into());
    let mut al_winter = edge_album("al-winter", "Winter", "Test Artist", "ar-1");
    al_winter.release_date = Some("2026-01-01".into());
    al_winter.curated_at = Some("2026-09-01T00:00:00".into());
    let mut al_old = edge_album("al-old", "Ancient", "Test Artist", "ar-1");
    al_old.year = Some(1999);
    al_old.release_date = Some("1999-06-01".into());
    al_old.curated_at = Some("2026-12-01T00:00:00".into());
    let albums = vec![al_none, al_tie_loose, al_tie_set, al_winter, al_old];
    let songs = vec![
        edge_song("so-none", "al-none", "No Release", "ar-1"),
        edge_song("so-tie-loose", "al-tie-loose", "Tie Loose", "ar-1"),
        edge_song("so-tie-set", "al-tie-set", "Tie Set", "ar-1"),
        edge_song("so-winter", "al-winter", "Winter", "ar-1"),
        edge_song("so-old", "al-old", "Ancient", "ar-1"),
    ];
    let store = MemStore::empty_with_test_auth().with_catalogue(artists, albums, songs);
    let app = SubsonicApp::new(store);
    let text = store_text(
        &app,
        "/rest/getAlbumList2",
        &format!("{KEY}&type=byYear&fromYear=2020&toYear=2030&f=json"),
    );
    assert_eq!(
        id_order(
            &text,
            &["al-none", "al-tie-loose", "al-tie-set", "al-winter"]
        ),
        vec![
            "al-none".to_string(),
            "al-tie-loose".to_string(),
            "al-tie-set".to_string(),
            "al-winter".to_string()
        ],
        "byYear sorts release_date then curated_at, nulls first: {text}"
    );
    assert!(
        !text.contains("al-old"),
        "out-of-range year stays excluded: {text}"
    );
}

#[test]
fn album_list2_alphabetical_by_artist_uses_sort_name() {
    // Display artists and sort names disagree on purpose: the catalogue
    // follows `artist_sort_name` then album name, the legacy list follows
    // the display artist then album name.
    let artists = vec![
        Artist {
            id: "ar-x".into(),
            name: "Beta".into(),
        },
        Artist {
            id: "ar-y".into(),
            name: "Alpha".into(),
        },
        Artist {
            id: "ar-z".into(),
            name: "Charlie".into(),
        },
        Artist {
            id: "ar-s".into(),
            name: "Solo".into(),
        },
    ];
    let mut al_x = edge_album("al-x", "Same Name", "Beta", "ar-x");
    al_x.artist_sort_name = "Charlie".into();
    let mut al_y = edge_album("al-y", "Same Name", "Alpha", "ar-y");
    al_y.artist_sort_name = "Bravo".into();
    let mut al_z = edge_album("al-z", "Same Name", "Charlie", "ar-z");
    al_z.artist_sort_name = "Alpha".into();
    let mut al_t1 = edge_album("al-t1", "Second", "Solo", "ar-s");
    al_t1.artist_sort_name = "Same".into();
    let mut al_t2 = edge_album("al-t2", "First", "Solo", "ar-s");
    al_t2.artist_sort_name = "Same".into();
    let albums = vec![al_x, al_y, al_z, al_t1, al_t2];
    let songs = vec![
        edge_song("so-x", "al-x", "Same Name", "ar-x"),
        edge_song("so-y", "al-y", "Same Name", "ar-y"),
        edge_song("so-z", "al-z", "Same Name", "ar-z"),
        edge_song("so-t1", "al-t1", "Second", "ar-s"),
        edge_song("so-t2", "al-t2", "First", "ar-s"),
    ];
    let store = MemStore::empty_with_test_auth().with_catalogue(artists, albums, songs);
    let app = SubsonicApp::new(store);
    let ids = ["al-x", "al-y", "al-z", "al-t1", "al-t2"];
    let text = store_text(
        &app,
        "/rest/getAlbumList2",
        &format!("{KEY}&type=alphabeticalByArtist&f=json"),
    );
    assert_eq!(
        id_order(&text, &ids),
        vec![
            "al-z".to_string(),
            "al-y".to_string(),
            "al-x".to_string(),
            "al-t2".to_string(),
            "al-t1".to_string()
        ],
        "List2 follows artist_sort_name then album name: {text}"
    );
    let text = store_text(
        &app,
        "/rest/getAlbumList",
        &format!("{KEY}&type=alphabeticalByArtist&f=json"),
    );
    assert_eq!(
        id_order(&text, &ids),
        vec![
            "al-y".to_string(),
            "al-x".to_string(),
            "al-z".to_string(),
            "al-t2".to_string(),
            "al-t1".to_string()
        ],
        "legacy List1 follows the display artist: {text}"
    );
}
