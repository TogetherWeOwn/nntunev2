//! Catalogue + user-state model behind the endpoints.
//!
//! `Library` is the seam where PlanetScale (via Hyperdrive) plugs in: the
//! worker shell implements it with real queries, while tests and fixtures
//! run against `MemStore`. Timestamps are fixed (`NOW`) so golden fixtures
//! are deterministic; the worker shell uses real clock values.

use crate::auth::{md5_hex, AuthStore, Identity};
use std::collections::{BTreeMap, HashSet};

/// Deterministic timestamp used by `MemStore` (fixtures depend on it).
pub const NOW: &str = "2026-01-01T00:00:00";

#[derive(Clone, Debug)]
pub struct Artist {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub artist_id: String,
    pub duration: i64,
    pub created: String,
}

#[derive(Clone, Debug)]
pub struct Song {
    pub id: String,
    pub title: String,
    pub album: String,
    pub artist: String,
    pub track: i64,
    pub year: i64,
    pub genre: String,
    pub cover_art: String,
    pub size: i64,
    pub content_type: String,
    pub suffix: String,
    pub duration: i64,
    pub bitrate: i64,
    pub path: String,
    pub play_count: i64,
    pub disc: i64,
    pub created: String,
    pub album_id: String,
    pub artist_id: String,
}

#[derive(Clone, Debug)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub song_ids: Vec<String>,
    pub created: String,
    pub changed: String,
}

#[derive(Clone, Debug, Default)]
pub struct PlayQueue {
    pub username: String,
    pub current: String,
    pub position: i64,
    pub ids: Vec<String>,
    pub changed: String,
    pub changed_by: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum StarKind {
    Artist,
    Album,
    Song,
}

/// Backing store contract. Reads take `&self`; mutations take `&mut self`.
pub trait Library: AuthStore {
    fn user_email(&self, username: &str) -> Option<String>;
    fn artists(&self) -> Vec<Artist>;
    fn artist(&self, id: &str) -> Option<Artist>;
    fn albums_for_artist(&self, artist_id: &str) -> Vec<Album>;
    fn album(&self, id: &str) -> Option<Album>;
    fn all_albums(&self) -> Vec<Album>;
    fn songs_for_album(&self, album_id: &str) -> Vec<Song>;
    fn song(&self, id: &str) -> Option<Song>;
    fn all_songs(&self) -> Vec<Song>;
    fn genres(&self) -> Vec<(String, i64, i64)>;
    fn audio(&self, song_id: &str) -> Option<Vec<u8>>;
    fn cover(&self, cover_id: &str) -> Option<Vec<u8>>;
    fn avatar(&self, username: &str) -> Option<Vec<u8>>;
    fn lyrics(&self, artist: &str, title: &str) -> Option<String>;
    fn playlists(&self) -> Vec<Playlist>;
    fn playlist(&self, id: &str) -> Option<Playlist>;
    fn create_playlist(&mut self, name: String) -> Playlist;
    fn update_playlist(
        &mut self,
        id: &str,
        name: Option<String>,
        add: Vec<String>,
        remove: Vec<usize>,
    ) -> bool;
    fn delete_playlist(&mut self, id: &str) -> bool;
    fn star(&mut self, kind: StarKind, id: &str) -> bool;
    fn unstar(&mut self, kind: StarKind, id: &str);
    fn starred(&self, kind: StarKind) -> Vec<String>;
    fn set_rating(&mut self, id: &str, rating: i64) -> bool;
    fn rating(&self, id: &str) -> Option<i64>;
    fn scrobble(&mut self, song_id: &str) -> bool;
    fn queue(&self) -> Option<PlayQueue>;
    fn save_queue(&mut self, queue: PlayQueue);
}

/// Transparent 1x1 PNG placeholder used for cover art / avatars in tests.
pub const PLACEHOLDER_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 96, 96, 96, 0, 0, 0, 4, 0,
    1, 39, 35, 101, 168, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// Opaque demo audio bytes (fixtures assert status/type/length, not codecs).
pub const DEMO_AUDIO: &[u8] = b"ID3\x04\x00\x00\x00\x00\x01\x02nntune-demo-audio";

/// In-memory `Library`: powers unit tests, golden fixtures, and local dev.
/// Seeded content mirrors `fixtures/subsonic/README.md`.
#[derive(Clone, Debug, Default)]
pub struct MemStore {
    artists: Vec<Artist>,
    albums: Vec<Album>,
    songs: Vec<Song>,
    lyrics: BTreeMap<(String, String), String>,
    playlists: BTreeMap<String, Playlist>,
    playlist_seq: u64,
    stars: HashSet<(StarKind, String)>,
    ratings: BTreeMap<String, i64>,
    scrobbles: Vec<String>,
    queue: Option<PlayQueue>,
}

impl MemStore {
    pub fn empty() -> MemStore {
        MemStore::default()
    }

    pub fn seeded() -> MemStore {
        let mut store = MemStore::empty();
        store.artists.push(Artist {
            id: "ar-1".into(),
            name: "Test Artist".into(),
        });
        store.albums.push(Album {
            id: "al-1".into(),
            name: "Test Album".into(),
            artist: "Test Artist".into(),
            artist_id: "ar-1".into(),
            duration: 200,
            created: NOW.into(),
        });
        store.songs.push(Song {
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
            bitrate: 128,
            path: "Test Artist/Test Album/01 Test Song.mp3".into(),
            play_count: 0,
            disc: 1,
            created: NOW.into(),
            album_id: "al-1".into(),
            artist_id: "ar-1".into(),
        });
        store.lyrics.insert(
            ("Test Artist".into(), "Test Song".into()),
            "La la la".into(),
        );
        store
    }
}

impl AuthStore for MemStore {
    fn verify_api_key(&self, api_key: &str) -> Option<Identity> {
        (api_key == "test-key-1").then(|| Identity {
            username: "test".into(),
            email: "test@example.com".into(),
        })
    }

    fn verify_password(&self, username: &str, password: &str) -> Option<Identity> {
        (username == "test" && password == "test-pass").then(|| Identity {
            username: "test".into(),
            email: "test@example.com".into(),
        })
    }

    fn verify_token(&self, username: &str, token: &str, salt: &str) -> Option<Identity> {
        let expect = md5_hex(format!("test-pass{salt}").as_bytes());
        (username == "test" && token == expect).then(|| Identity {
            username: "test".into(),
            email: "test@example.com".into(),
        })
    }
}

impl Library for MemStore {
    fn user_email(&self, username: &str) -> Option<String> {
        (username == "test").then(|| "test@example.com".to_string())
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
        let mut counts: BTreeMap<String, (HashSet<String>, i64)> = BTreeMap::new();
        for song in &self.songs {
            let entry = counts.entry(song.genre.clone()).or_default();
            entry.0.insert(song.album_id.clone());
            entry.1 += 1;
        }
        counts
            .into_iter()
            .map(|(name, (albums, songs))| (name, songs, albums.len() as i64))
            .collect()
    }

    fn audio(&self, song_id: &str) -> Option<Vec<u8>> {
        self.song(song_id).map(|_| DEMO_AUDIO.to_vec())
    }

    fn cover(&self, cover_id: &str) -> Option<Vec<u8>> {
        let known = self.albums.iter().any(|a| a.id == cover_id)
            || self.songs.iter().any(|s| s.cover_art == cover_id);
        known.then(|| PLACEHOLDER_PNG.to_vec())
    }

    fn avatar(&self, username: &str) -> Option<Vec<u8>> {
        (username == "test").then(|| PLACEHOLDER_PNG.to_vec())
    }

    fn lyrics(&self, artist: &str, title: &str) -> Option<String> {
        self.lyrics
            .get(&(artist.to_string(), title.to_string()))
            .cloned()
    }

    fn playlists(&self) -> Vec<Playlist> {
        self.playlists.values().cloned().collect()
    }

    fn playlist(&self, id: &str) -> Option<Playlist> {
        self.playlists.get(id).cloned()
    }

    fn create_playlist(&mut self, name: String) -> Playlist {
        self.playlist_seq += 1;
        let id = format!("pl-{}", self.playlist_seq);
        let playlist = Playlist {
            id: id.clone(),
            name,
            song_ids: Vec::new(),
            created: NOW.into(),
            changed: NOW.into(),
        };
        self.playlists.insert(id, playlist.clone());
        playlist
    }

    fn update_playlist(
        &mut self,
        id: &str,
        name: Option<String>,
        add: Vec<String>,
        remove: Vec<usize>,
    ) -> bool {
        let Some(playlist) = self.playlists.get_mut(id) else {
            return false;
        };
        if let Some(name) = name {
            playlist.name = name;
        }
        let mut remove: Vec<usize> = remove;
        remove.sort_unstable();
        remove.dedup();
        for index in remove.into_iter().rev() {
            if index < playlist.song_ids.len() {
                playlist.song_ids.remove(index);
            }
        }
        for song_id in add {
            if self.songs.iter().any(|s| s.id == song_id) {
                playlist.song_ids.push(song_id);
            }
        }
        playlist.changed = NOW.into();
        true
    }

    fn delete_playlist(&mut self, id: &str) -> bool {
        self.playlists.remove(id).is_some()
    }

    fn star(&mut self, kind: StarKind, id: &str) -> bool {
        let known = match kind {
            StarKind::Artist => self.artist(id).is_some(),
            StarKind::Album => self.album(id).is_some(),
            StarKind::Song => self.song(id).is_some(),
        };
        if known {
            self.stars.insert((kind, id.to_string()));
        }
        known
    }

    fn unstar(&mut self, kind: StarKind, id: &str) {
        self.stars.remove(&(kind, id.to_string()));
    }

    fn starred(&self, kind: StarKind) -> Vec<String> {
        let mut out: Vec<String> = self
            .stars
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, id)| id.clone())
            .collect();
        out.sort();
        out
    }

    fn set_rating(&mut self, id: &str, rating: i64) -> bool {
        let known =
            self.song(id).is_some() || self.album(id).is_some() || self.artist(id).is_some();
        if known {
            if rating == 0 {
                self.ratings.remove(id);
            } else {
                self.ratings.insert(id.to_string(), rating.clamp(1, 5));
            }
        }
        known
    }

    fn rating(&self, id: &str) -> Option<i64> {
        self.ratings.get(id).copied()
    }

    fn scrobble(&mut self, song_id: &str) -> bool {
        if self.song(song_id).is_none() {
            return false;
        }
        self.scrobbles.push(song_id.to_string());
        true
    }

    fn queue(&self) -> Option<PlayQueue> {
        self.queue.clone()
    }

    fn save_queue(&mut self, queue: PlayQueue) {
        self.queue = Some(queue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_store_holds_demo_library() {
        let store = MemStore::seeded();
        assert_eq!(store.artists().len(), 1);
        assert_eq!(store.all_songs().len(), 1);
        assert_eq!(store.genres(), vec![("Rock".to_string(), 1, 1)]);
        assert!(store.audio("so-1").is_some());
        assert!(store.cover("al-1").is_some());
        assert!(store.cover("nope").is_none());
    }

    #[test]
    fn playlist_ids_are_sequential() {
        let mut store = MemStore::empty();
        assert_eq!(store.create_playlist("a".into()).id, "pl-1");
        assert_eq!(store.create_playlist("b".into()).id, "pl-2");
    }
}
