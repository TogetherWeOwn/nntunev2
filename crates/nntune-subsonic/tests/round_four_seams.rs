//! Required store seams for the round-four fixes.
//!
//! v1 computes `getIndexes.lastModified` from the newest artist touch
//! (falling back to now), so the port needs a store accessor for it. The
//! seeded store keeps the fixed fixture time; a custom store timestamp must
//! propagate to the response.

use nntune_subsonic::library::{
    Album, AlbumListQuery, Artist, Library, PlayQueue, Playlist, Song, StarKind, UserAccount, NOW,
};
use nntune_subsonic::{AuthStore, Identity, MemStore, SubsonicApp};

const KEY: &str = "apiKey=test-key-1&v=1.16.1&c=t";

/// Store with a caller-supplied index timestamp and an empty catalogue.
/// Sampling and history delegate to the real empty store so the seam carries
/// no reimplemented shuffle or refusal text.
#[derive(Clone, Debug)]
struct TimestampStore {
    last_modified: i64,
}

impl AuthStore for TimestampStore {
    fn verify_api_key(&self, api_key: &str) -> Option<Identity> {
        if api_key == "test-key-1" {
            Some(Identity {
                username: "test".into(),
                email: "test@example.com".into(),
                is_admin: false,
            })
        } else {
            None
        }
    }

    fn verify_password(&self, _username: &str, _password: &str) -> Option<Identity> {
        None
    }

    fn verify_token(&self, _username: &str, _token: &str, _salt: &str) -> Option<Identity> {
        None
    }
}

impl Library for TimestampStore {
    fn artists_last_modified(&self) -> i64 {
        self.last_modified
    }

    fn user_account(&self, username: &str) -> Option<UserAccount> {
        if username == "test" {
            Some(UserAccount {
                email: "test@example.com".into(),
                is_admin: false,
                is_manager: false,
            })
        } else {
            None
        }
    }

    fn artists(&self) -> Vec<Artist> {
        vec![Artist {
            id: "ar-1".into(),
            name: "Test Artist".into(),
        }]
    }

    fn artist(&self, id: &str) -> Option<Artist> {
        (id == "ar-1").then(|| Artist {
            id: "ar-1".into(),
            name: "Test Artist".into(),
        })
    }

    fn albums_for_artist(&self, _artist_id: &str) -> Vec<Album> {
        Vec::new()
    }

    fn album(&self, _id: &str) -> Option<Album> {
        None
    }

    fn all_albums(&self) -> Vec<Album> {
        Vec::new()
    }

    fn album_list(&self, query: AlbumListQuery<'_>) -> Vec<Album> {
        MemStore::empty_with_test_auth().album_list(query)
    }

    fn album_list2(&self, query: AlbumListQuery<'_>) -> Result<Vec<Album>, (i64, String)> {
        MemStore::empty_with_test_auth().album_list2(query)
    }

    fn random_songs(&self, identity: &Identity, size: usize) -> Vec<Song> {
        MemStore::empty_with_test_auth().random_songs(identity, size)
    }

    fn songs_for_album(&self, _album_id: &str) -> Vec<Song> {
        Vec::new()
    }

    fn song(&self, _id: &str) -> Option<Song> {
        None
    }

    fn all_songs(&self) -> Vec<Song> {
        Vec::new()
    }

    fn genres(&self) -> Vec<(String, i64, i64)> {
        Vec::new()
    }

    fn audio(&self, _song_id: &str) -> Option<Vec<u8>> {
        None
    }

    fn cover(&self, _cover_id: &str) -> Option<Vec<u8>> {
        None
    }

    fn avatar(&self, _username: &str) -> Option<Vec<u8>> {
        None
    }

    fn lyrics(&self, _artist: &str, _title: &str) -> Option<String> {
        None
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

    fn scrobble(&mut self, _identity: &Identity, _song_id: &str) -> bool {
        false
    }

    fn queue(&self, _identity: &Identity) -> Option<PlayQueue> {
        None
    }

    fn save_queue(&mut self, _identity: &Identity, _queue: PlayQueue) {}
}

#[test]
fn memstore_keeps_the_fixed_fixture_time_through_the_accessor() {
    assert_eq!(
        MemStore::seeded().artists_last_modified(),
        1_767_225_600_000
    );
}

#[test]
fn custom_store_timestamp_propagates_to_get_indexes() {
    let app = SubsonicApp::new(TimestampStore {
        last_modified: 1_700_000_000_000,
    });
    let res = app.handle("GET", "/rest/getIndexes", &format!("{KEY}&f=json"), "");
    let text = String::from_utf8(res.body).unwrap();
    assert!(
        text.contains("\"lastModified\":1700000000000"),
        "custom timestamp must propagate: {text}"
    );
}

#[test]
fn empty_seam_delegates_list2_refusal_text() {
    let app = SubsonicApp::new(TimestampStore {
        last_modified: 1_700_000_000_000,
    });
    let res = app.handle(
        "GET",
        "/rest/getAlbumList2",
        &format!("{KEY}&type=frequent&f=json"),
        "",
    );
    let text = String::from_utf8(res.body).unwrap();
    assert!(text.contains("\"code\":0"), "{text}");
    assert!(
        text.contains("Unsupported album list type: frequent"),
        "{text}"
    );
}
