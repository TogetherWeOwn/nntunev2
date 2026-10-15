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
    pub disc: i64,
    pub created: String,
    pub album_id: String,
    pub artist_id: String,
}

#[derive(Clone, Debug)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub owner: String,
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

#[derive(Clone, Copy, Debug)]
pub struct AlbumListQuery<'a> {
    pub identity: &'a Identity,
    pub list_type: &'a str,
    pub size: usize,
    pub offset: usize,
    pub from_year: Option<i64>,
    pub to_year: Option<i64>,
    pub genre: Option<&'a str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum StarKind {
    Artist,
    Album,
    Song,
}

/// Target user record for `getUser`/`getAvatar`. v1 `UserResource` derives
/// `adminRole` from the target's role (not the caller's) and
/// `uploadRole`/`coverArtRole` from admin-or-manager (`$canManage`); the
/// remaining roles are fixed constants. The seam returns the record so a
/// PlanetScale implementation reads the real role instead of guessing.
#[derive(Clone, Debug, PartialEq)]
pub struct UserAccount {
    pub email: String,
    pub is_admin: bool,
    pub is_manager: bool,
}

impl UserAccount {
    pub fn can_manage(&self) -> bool {
        self.is_admin || self.is_manager
    }
}

/// Backing store contract. Reads take `&self`; mutations take `&mut self`.
///
/// Every user-state method takes the authenticated `Identity` so a
/// PlanetScale implementation can scope rows per user (v1 authorizes each of
/// these: playlist delete/collaborate/own checks, `getAllAccessibleByUser`,
/// per-user queue/stars/ratings). Catalogue reads stay global; per-user
/// writes that hit another user's rows answer error 50, never 70.
pub trait Library: AuthStore {
    fn user_account(&self, username: &str) -> Option<UserAccount>;
    fn artists(&self) -> Vec<Artist>;
    fn artist(&self, id: &str) -> Option<Artist>;
    fn albums_for_artist(&self, artist_id: &str) -> Vec<Album>;
    fn album(&self, id: &str) -> Option<Album>;
    fn all_albums(&self) -> Vec<Album>;
    fn songs_for_album(&self, album_id: &str) -> Vec<Song>;
    fn song(&self, id: &str) -> Option<Song>;
    fn all_songs(&self) -> Vec<Song>;
    fn search_artists(&self, query: &str) -> Vec<Artist> {
        self.artists()
            .into_iter()
            .filter(|artist| {
                query.is_empty() || artist.name.to_lowercase().contains(&query.to_lowercase())
            })
            .collect()
    }
    fn search_albums(&self, query: &str) -> Vec<Album> {
        self.all_albums()
            .into_iter()
            .filter(|album| {
                query.is_empty()
                    || album.name.to_lowercase().contains(&query.to_lowercase())
                    || album.artist.to_lowercase().contains(&query.to_lowercase())
            })
            .collect()
    }
    fn search_songs(&self, query: &str) -> Vec<Song> {
        self.all_songs()
            .into_iter()
            .filter(|song| {
                query.is_empty()
                    || song.title.to_lowercase().contains(&query.to_lowercase())
                    || song.album.to_lowercase().contains(&query.to_lowercase())
                    || song.artist.to_lowercase().contains(&query.to_lowercase())
            })
            .collect()
    }
    fn album_list(&self, query: AlbumListQuery<'_>) -> Vec<Album> {
        let AlbumListQuery {
            identity,
            list_type,
            size,
            offset,
            from_year,
            to_year,
            genre,
        } = query;
        let mut albums = self.all_albums();
        if list_type == "starred" {
            let starred_albums: HashSet<String> = self
                .starred(identity, StarKind::Album)
                .into_iter()
                .collect();
            let via_songs: HashSet<String> = self
                .starred(identity, StarKind::Song)
                .into_iter()
                .filter_map(|id| self.song(&id))
                .map(|song| song.album_id)
                .collect();
            albums.retain(|album| {
                starred_albums.contains(&album.id) || via_songs.contains(&album.id)
            });
        }
        if let (Some(from), Some(to)) = (from_year, to_year) {
            albums.retain(|album| {
                self.songs_for_album(&album.id)
                    .iter()
                    .any(|song| (from..=to).contains(&song.year))
            });
        }
        if let Some(genre) = genre {
            let genre = genre.to_lowercase();
            albums.retain(|album| {
                self.songs_for_album(&album.id)
                    .iter()
                    .any(|song| song.genre.to_lowercase() == genre)
            });
        }
        match list_type {
            "alphabeticalByName" => albums.sort_by_key(|album| album.name.to_lowercase()),
            "alphabeticalByArtist" => albums.sort_by_key(|album| album.artist.to_lowercase()),
            "frequent" => {
                albums.sort_by_key(|album| std::cmp::Reverse(self.songs_for_album(&album.id).len()))
            }
            "byYear" => albums.sort_by_key(|album| {
                self.songs_for_album(&album.id)
                    .iter()
                    .map(|song| song.year)
                    .min()
                    .unwrap_or_default()
            }),
            "byGenre" => albums.sort_by_key(|album| {
                self.songs_for_album(&album.id)
                    .first()
                    .map(|song| song.genre.to_lowercase())
                    .unwrap_or_default()
            }),
            "highest" => albums.sort_by_key(|album| {
                std::cmp::Reverse(self.rating(identity, &album.id).unwrap_or_default())
            }),
            "random" => albums.sort_by_key(|album| {
                album.id.bytes().fold(0u64, |hash, byte| {
                    hash.wrapping_mul(31).wrapping_add(u64::from(byte))
                })
            }),
            "recent" | "newest" => {
                albums.sort_by(|a, b| b.created.cmp(&a.created).then(a.id.cmp(&b.id)))
            }
            _ => albums.sort_by(|a, b| b.created.cmp(&a.created).then(a.id.cmp(&b.id))),
        }
        albums.into_iter().skip(offset).take(size).collect()
    }
    fn genres(&self) -> Vec<(String, i64, i64)>;
    fn audio(&self, song_id: &str) -> Option<Vec<u8>>;
    fn cover(&self, cover_id: &str) -> Option<Vec<u8>>;
    fn avatar(&self, username: &str) -> Option<Vec<u8>>;
    fn lyrics(&self, artist: &str, title: &str) -> Option<String>;
    fn playlists(&self, identity: &Identity) -> Vec<Playlist>;
    fn playlist(&self, identity: &Identity, id: &str) -> Option<Playlist>;
    fn create_playlist(&mut self, identity: &Identity, name: String) -> Playlist;
    fn update_playlist(
        &mut self,
        identity: &Identity,
        id: &str,
        name: Option<String>,
        add: Vec<String>,
        remove: Vec<usize>,
    ) -> Result<(), (i64, String)>;
    fn delete_playlist(&mut self, identity: &Identity, id: &str) -> Result<(), (i64, String)>;
    fn star(&mut self, identity: &Identity, kind: StarKind, id: &str) -> bool;
    fn unstar(&mut self, identity: &Identity, kind: StarKind, id: &str);
    fn starred(&self, identity: &Identity, kind: StarKind) -> Vec<String>;
    fn set_rating(&mut self, identity: &Identity, id: &str, rating: i64) -> bool;
    fn rating(&self, identity: &Identity, id: &str) -> Option<i64>;
    fn scrobble(&mut self, identity: &Identity, song_id: &str) -> bool;
    fn queue(&self, identity: &Identity) -> Option<PlayQueue>;
    fn save_queue(&mut self, identity: &Identity, queue: PlayQueue);
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
///
/// `test_auth` gates the hard-coded fixture credentials: `empty()` (used by
/// the production worker shell) accepts NOTHING — every auth attempt fails
/// until the PlanetScale `Library` lands. `seeded()` and
/// `empty_with_test_auth()` enable the fixture users below. The credentials
/// are published in `fixtures/subsonic/README.md`, so they must never
/// authenticate outside tests.
#[derive(Clone, Debug, Default)]
pub struct MemStore {
    test_auth: bool,
    artists: Vec<Artist>,
    albums: Vec<Album>,
    songs: Vec<Song>,
    lyrics: BTreeMap<(String, String), String>,
    playlists: BTreeMap<String, Playlist>,
    playlist_seq: u64,
    stars: HashSet<(String, StarKind, String)>,
    ratings: BTreeMap<(String, String), i64>,
    scrobbles: Vec<(String, String)>,
    queues: BTreeMap<String, PlayQueue>,
}

fn test_identities() -> Vec<(String, String, String, String, bool, bool)> {
    vec![
        (
            "test".to_string(),
            "test-pass".to_string(),
            "test-key-1".to_string(),
            "test@example.com".to_string(),
            false,
            false,
        ),
        (
            "other".to_string(),
            "other-pass".to_string(),
            "other-key-1".to_string(),
            "other@example.com".to_string(),
            false,
            false,
        ),
        (
            "manager".to_string(),
            "manager-pass".to_string(),
            "manager-key-1".to_string(),
            "manager@example.com".to_string(),
            false,
            true,
        ),
        (
            "admin".to_string(),
            "admin-pass".to_string(),
            "admin-key-1".to_string(),
            "admin@example.com".to_string(),
            true,
            false,
        ),
    ]
}

fn not_authorized() -> (i64, String) {
    // v1 `SubsonicAwareErrorRenderer`: authorization failures share one text.
    (
        crate::envelope::code::NOT_AUTHORIZED,
        "User is not authorized for the given operation.".to_string(),
    )
}

fn data_missing() -> (i64, String) {
    // v1 `findOrFail` misses share one text via the same renderer.
    (
        crate::envelope::code::NOT_FOUND,
        "The requested data was not found.".to_string(),
    )
}

impl MemStore {
    /// Production-safe empty store: empty catalogue AND no credentials.
    /// The worker shell uses this, so a deployed worker rejects all auth.
    pub fn empty() -> MemStore {
        MemStore::default()
    }

    /// Empty catalogue that still accepts the fixture test users.
    /// Used by the empty-library fixture suite (auth must succeed while
    /// the catalogue stays empty, exactly like v1 with no rows).
    pub fn empty_with_test_auth() -> MemStore {
        MemStore {
            test_auth: true,
            ..MemStore::default()
        }
    }

    pub fn seeded() -> MemStore {
        let mut store = MemStore::empty_with_test_auth();
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
        if !self.test_auth {
            return None;
        }
        test_identities()
            .into_iter()
            .find(|(_, _, key, _, _, _)| key == api_key)
            .map(|(username, _, _, email, is_admin, _)| Identity {
                username,
                email,
                is_admin,
            })
    }

    fn verify_password(&self, username: &str, password: &str) -> Option<Identity> {
        if !self.test_auth {
            return None;
        }
        test_identities()
            .into_iter()
            .find(|(u, p, _, _, _, _)| u == username && p == password)
            .map(|(username, _, _, email, is_admin, _)| Identity {
                username,
                email,
                is_admin,
            })
    }

    fn verify_token(&self, username: &str, token: &str, salt: &str) -> Option<Identity> {
        if !self.test_auth {
            return None;
        }
        test_identities()
            .into_iter()
            .find(|(u, p, _, _, _, _)| {
                u == username && token == md5_hex(format!("{p}{salt}").as_bytes())
            })
            .map(|(username, _, _, email, is_admin, _)| Identity {
                username,
                email,
                is_admin,
            })
    }
}

impl Library for MemStore {
    fn user_account(&self, username: &str) -> Option<UserAccount> {
        test_identities()
            .into_iter()
            .find(|(u, _, _, _, _, _)| u == username)
            .map(|(_, _, _, email, is_admin, is_manager)| UserAccount {
                email,
                is_admin,
                is_manager,
            })
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
        self.user_account(username)
            .map(|_| PLACEHOLDER_PNG.to_vec())
    }

    fn lyrics(&self, artist: &str, title: &str) -> Option<String> {
        self.lyrics
            .get(&(artist.to_string(), title.to_string()))
            .cloned()
    }

    fn playlists(&self, identity: &Identity) -> Vec<Playlist> {
        let mut out: Vec<Playlist> = self
            .playlists
            .values()
            .filter(|p| p.owner == identity.username)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    fn playlist(&self, _identity: &Identity, id: &str) -> Option<Playlist> {
        // Returned regardless of owner so the endpoint can answer 50
        // (forbidden) vs 70 (missing) distinctly; listing stays filtered.
        self.playlists.get(id).cloned()
    }

    fn create_playlist(&mut self, identity: &Identity, name: String) -> Playlist {
        self.playlist_seq += 1;
        let id = format!("pl-{}", self.playlist_seq);
        let playlist = Playlist {
            id: id.clone(),
            name,
            owner: identity.username.clone(),
            song_ids: Vec::new(),
            created: NOW.into(),
            changed: NOW.into(),
        };
        self.playlists.insert(id, playlist.clone());
        playlist
    }

    fn update_playlist(
        &mut self,
        identity: &Identity,
        id: &str,
        name: Option<String>,
        add: Vec<String>,
        remove: Vec<usize>,
    ) -> Result<(), (i64, String)> {
        let Some(playlist) = self.playlists.get_mut(id) else {
            return Err(data_missing());
        };
        if playlist.owner != identity.username {
            return Err(not_authorized());
        }
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
        Ok(())
    }

    fn delete_playlist(&mut self, identity: &Identity, id: &str) -> Result<(), (i64, String)> {
        match self.playlists.get(id) {
            None => Err(data_missing()),
            Some(p) if p.owner != identity.username => Err(not_authorized()),
            Some(_) => {
                self.playlists.remove(id);
                Ok(())
            }
        }
    }

    fn star(&mut self, identity: &Identity, kind: StarKind, id: &str) -> bool {
        let known = match kind {
            StarKind::Artist => self.artist(id).is_some(),
            StarKind::Album => self.album(id).is_some(),
            StarKind::Song => self.song(id).is_some(),
        };
        if known {
            self.stars
                .insert((identity.username.clone(), kind, id.to_string()));
        }
        known
    }

    fn unstar(&mut self, identity: &Identity, kind: StarKind, id: &str) {
        self.stars
            .remove(&(identity.username.clone(), kind, id.to_string()));
    }

    fn starred(&self, identity: &Identity, kind: StarKind) -> Vec<String> {
        let mut out: Vec<String> = self
            .stars
            .iter()
            .filter(|(u, k, _)| u == &identity.username && *k == kind)
            .map(|(_, _, id)| id.clone())
            .collect();
        out.sort();
        out
    }

    fn set_rating(&mut self, identity: &Identity, id: &str, rating: i64) -> bool {
        let known =
            self.song(id).is_some() || self.album(id).is_some() || self.artist(id).is_some();
        if known {
            let key = (identity.username.clone(), id.to_string());
            if rating == 0 {
                self.ratings.remove(&key);
            } else {
                self.ratings.insert(key, rating.clamp(1, 5));
            }
        }
        known
    }

    fn rating(&self, identity: &Identity, id: &str) -> Option<i64> {
        self.ratings
            .get(&(identity.username.clone(), id.to_string()))
            .copied()
    }

    fn scrobble(&mut self, identity: &Identity, song_id: &str) -> bool {
        if self.song(song_id).is_none() {
            return false;
        }
        self.scrobbles
            .push((identity.username.clone(), song_id.to_string()));
        true
    }

    fn queue(&self, identity: &Identity) -> Option<PlayQueue> {
        self.queues.get(&identity.username).cloned()
    }

    fn save_queue(&mut self, identity: &Identity, queue: PlayQueue) {
        self.queues.insert(identity.username.clone(), queue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_identity() -> Identity {
        Identity {
            username: "test".into(),
            email: "test@example.com".into(),
            is_admin: false,
        }
    }

    fn other_identity() -> Identity {
        Identity {
            username: "other".into(),
            email: "other@example.com".into(),
            is_admin: false,
        }
    }

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
    fn empty_store_rejects_all_auth() {
        let store = MemStore::empty();
        assert!(store.verify_api_key("test-key-1").is_none());
        assert!(store.verify_password("test", "test-pass").is_none());
        assert!(store
            .verify_token("test", &md5_hex(b"test-passabc"), "abc")
            .is_none());
    }

    #[test]
    fn playlist_ids_are_sequential() {
        let mut store = MemStore::empty_with_test_auth();
        let id = test_identity();
        assert_eq!(store.create_playlist(&id, "a".into()).id, "pl-1");
        assert_eq!(store.create_playlist(&id, "b".into()).id, "pl-2");
    }

    #[test]
    fn playlists_are_per_user() {
        let mut store = MemStore::empty_with_test_auth();
        let a = test_identity();
        let b = other_identity();
        let pl = store.create_playlist(&a, "a".into());
        // Owner sees it; the other user lists nothing and cannot mutate it.
        assert_eq!(store.playlists(&a).len(), 1);
        assert!(store.playlists(&b).is_empty());
        assert_eq!(store.delete_playlist(&b, &pl.id), Err(not_authorized()));
        assert!(store.delete_playlist(&a, &pl.id).is_ok());
    }

    #[test]
    fn stars_ratings_and_queue_are_per_user() {
        let mut store = MemStore::seeded();
        let a = test_identity();
        let b = other_identity();
        assert!(store.star(&a, StarKind::Song, "so-1"));
        assert!(store
            .starred(&a, StarKind::Song)
            .contains(&"so-1".to_string()));
        assert!(store.starred(&b, StarKind::Song).is_empty());
        assert!(store.set_rating(&a, "so-1", 5));
        assert_eq!(store.rating(&a, "so-1"), Some(5));
        assert_eq!(store.rating(&b, "so-1"), None);
        store.save_queue(
            &a,
            PlayQueue {
                username: "test".into(),
                current: "so-1".into(),
                position: 0,
                ids: vec!["so-1".into()],
                changed: NOW.into(),
                changed_by: "test".into(),
            },
        );
        assert!(store.queue(&a).is_some());
        assert!(store.queue(&b).is_none());
    }
}
