//! Production dispatch core for the Subsonic `/rest/*` mount (Scope B).
//!
//! The worker entry ([`crate::worker_entry::subsonic_mount`]) authenticates
//! the request asynchronously against Postgres ([`crate::subsonic_auth`]
//! over [`crate::subsonic_pg::PgStore`]), loads the caller's membership
//! catalogue through the existing [`crate::catalogue`] reads, then serves
//! the request synchronously through [`nntune_subsonic::SubsonicApp`]
//! over a [`ProductionStore`] snapshot built by [`build_snapshot`]. The
//! split keeps this module free of `worker::` types so every rule below
//! is natively testable.
//!
//! Honest gaps, all fail-closed rather than silently dropped:
//!
//! - `audio()` is always `None`: production bytes must come through the
//!   signed-grant, one-stream, metered path ([`_stream.error`]), never
//!   fixture bytes. The mount intercepts `stream` after authentication
//!   and serves it from the stream Worker.
//! - `cover()` is always `None`, so `getCoverArt` answers error 70.
//!   Cover-art rights are flagged (AGENTS.md MetaBrainz rules): the
//!   archive clears no licence, so no Cover Art Archive bytes are served
//!   or cached here — reference/proxy art lands in a separate, reviewed
//!   change. `Song.cover_art` still names the release MBID so the id is
//!   stable once art serving exists.
//! - Per-user state (playlists, stars, ratings, queue, scrobbles) has no
//!   Postgres table yet. `playlists()` is empty, `playlist()` is `None`,
//!   mutations that can signal failure return error 50
//!   (`update_playlist`, `delete_playlist`) or `false`, never a
//!   fabricated success. Two trait shapes cannot signal failure and are
//!   documented at their impls: `create_playlist` (infallible) returns an
//!   id-less placeholder that is never stored, `scrobble`/`star`/`unstar`
//!   (endpoints ignore the return) are accepted without durable history
//!   until the per-user table lands — `getPlaylists`/`getStarred` stay
//!   empty, which is the visible signal.
//! - `Song.genre` is empty (unknown): mirror genres are supplementary
//!   CC BY-NC-SA data that must never ship in this paid product, so no
//!   genre text is mapped from the mirror.
//! - `random_songs`/`album_list("random")` are deterministic id-ordered
//!   prefixes: a per-request snapshot holds no RNG. `recent`/`highest`/
//!   `starred` reflect the empty per-user history above. `frequent`
//!   mirrors the shared-crate contract (`MemStore`): with no history
//!   every album is eligible with play count 0.
//!
//! Stream-time limits, stated honestly:
//!
//! - One stream is enforced per Subsonic credential (the server-owned
//!   compatibility session), not per device: the protocol authenticates
//!   the account, and there is no per-device identity to hang a second
//!   session on. Two devices sharing one credential therefore share one
//!   lease — a second play replaces the first instead of waiting — and
//!   both signed grants stay valid for their 15-minute TTL. The native
//!   path has the same property per client-supplied session id. A
//!   per-device limit needs protocol identities this surface cannot
//!   mint without breaking client compatibility.
//! - Every `stream` request issues a fresh play (`play_id` is never a
//!   continuation): Subsonic clients carry no play identity, so a seek
//!   re-checks subscription/hour-cap and the shared lease. That is the
//!   mandated entitlement check running on each play, not a missed
//!   continuation; within the cap the same compat session replaces the
//!   lease, so resume works. Uncached tracks answer 70 until the stream
//!   Worker's deferred miss adapter serves them (upstream 404/502).

use std::collections::{BTreeMap, HashMap, HashSet};

use nntune_subsonic::auth::{AuthStore, Identity};
use nntune_subsonic::envelope::code;
use nntune_subsonic::library::{
    Album, AlbumListQuery, Artist, Library, PlayQueue, Playlist, Song, StarKind, UserAccount, NOW,
};

use crate::catalogue::{ArtistDbRow, ReleaseDbRow, TrackDbRow};

/// Production FLAC identity: the catalogue is FLAC-only, so every song
/// advertises `audio/flac`/`flac` and the stream mount refuses any
/// transcoding request instead of silently serving original bytes.
pub const FLAC_CONTENT_TYPE: &str = "audio/flac";
pub const FLAC_SUFFIX: &str = "flac";

/// Per-request catalogue snapshot: the authenticated identity, their own
/// account record, and the membership catalogue as Subsonic rows. The
/// store exists only for requests whose async authentication already
/// succeeded, so the `AuthStore` answers below are safe by construction
/// (see the `AuthStore` impl note) and never consult fixture credentials.
#[derive(Clone, Debug)]
// Interim (slice 2 of 6): the `AuthStore`/`Library` impls that read these
// fields land in slices 3-4; this allow is removed by the final slice so
// the cumulative diff vs the reviewed tree is empty.
#[allow(dead_code)]
pub struct ProductionStore {
    identity: Identity,
    account: UserAccount,
    artists: Vec<Artist>,
    albums: Vec<Album>,
    songs: Vec<Song>,
    /// Request time (epoch millis), captured at the mount. Reported as
    /// the artists index's `lastModified`: the member reads carry no
    /// per-artist modification signal, so the honest freshness marker is
    /// "may have changed as of this request" rather than a frozen
    /// timestamp that would pin client caches forever.
    now_millis: i64,
}

impl ProductionStore {
    pub fn new(
        identity: Identity,
        account: UserAccount,
        artists: Vec<Artist>,
        albums: Vec<Album>,
        songs: Vec<Song>,
        now_millis: i64,
    ) -> Self {
        Self {
            identity,
            account,
            artists,
            albums,
            songs,
            now_millis,
        }
    }
}

/// Map membership-catalogue rows (` PgStore::catalogue_artists`,
/// `catalogue_releases`, `catalogue_tracks` — all already member-scoped)
/// onto Subsonic catalogue rows. Pure and total: a malformed or
/// artist-less row degrades to empty strings/zeros, never a panic, and
/// membership was already enforced by the reads.
pub fn build_snapshot(
    artists: &[ArtistDbRow],
    releases: &[ReleaseDbRow],
    tracks: &[TrackDbRow],
) -> (Vec<Artist>, Vec<Album>, Vec<Song>) {
    let artist_sort: HashMap<&str, &str> = artists
        .iter()
        .map(|a| (a.mbid.as_str(), a.sort_name.as_str()))
        .collect();
    let release_year: HashMap<&str, Option<i16>> =
        releases.iter().map(|r| (r.mbid.as_str(), r.year)).collect();

    let mut out_artists: Vec<Artist> = artists
        .iter()
        .map(|a| Artist {
            id: a.mbid.clone(),
            name: a.name.clone(),
        })
        .collect();
    // Browse order follows the MusicBrainz sort name (falling back to the
    // display name), matching the native catalogue ordering contract; the
    // mapped Artist carries only name, so the sort reads the row map.
    out_artists.sort_by(|a, b| {
        let sort_a = artist_sort
            .get(a.id.as_str())
            .copied()
            .unwrap_or(a.name.as_str());
        let sort_b = artist_sort
            .get(b.id.as_str())
            .copied()
            .unwrap_or(b.name.as_str());
        sort_a
            .cmp(sort_b)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });

    // Album duration is the summed member track durations (seconds) — from
    // the rows above, never an estimate or a transcoder's output.
    let mut duration_secs: HashMap<&str, i64> = HashMap::new();
    for track in tracks {
        let secs = i64::from(track.duration_ms.unwrap_or(0)) / 1000;
        *duration_secs
            .entry(track.release_mbid.as_str())
            .or_insert(0) += secs;
    }

    let mut out_albums: Vec<Album> = releases
        .iter()
        .map(|r| {
            let artist_id = r.artist_mbid.clone().unwrap_or_default();
            let artist = r.artist_name.clone().unwrap_or_default();
            let sort = artist_sort
                .get(artist_id.as_str())
                .map(|s| (*s).to_string())
                .unwrap_or_else(|| artist.clone());
            Album {
                id: r.mbid.clone(),
                name: r.title.clone(),
                artist,
                artist_id,
                duration: duration_secs.get(r.mbid.as_str()).copied().unwrap_or(0),
                created: NOW.to_string(),
                year: r.year.map(i64::from),
                release_date: None,
                curated_at: None,
                artist_sort_name: sort,
            }
        })
        .collect();
    out_albums.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));

    let mut out_songs: Vec<Song> = tracks
        .iter()
        .map(|t| {
            let duration = i64::from(t.duration_ms.unwrap_or(0)) / 1000;
            Song {
                id: t.recording_mbid.clone(),
                title: t.title.clone(),
                album: t.release_title.clone(),
                artist: t.artist_name.clone().unwrap_or_default(),
                track: t.track_no.map(i64::from).unwrap_or(0),
                year: release_year
                    .get(t.release_mbid.as_str())
                    .copied()
                    .flatten()
                    .map(i64::from)
                    .unwrap_or(0),
                genre: String::new(),
                cover_art: t.release_mbid.clone(),
                // FLAC byte size from the settled metering constant, not a
                // guess: bytes = seconds * STREAM_BYTES_PER_SEC.
                size: duration * (crate::ledger::STREAM_BYTES_PER_SEC as i64),
                content_type: FLAC_CONTENT_TYPE.to_string(),
                suffix: FLAC_SUFFIX.to_string(),
                duration,
                disc: t.medium_no.map(i64::from).unwrap_or(1),
                created: NOW.to_string(),
                album_id: t.release_mbid.clone(),
                artist_id: t.artist_mbid.clone().unwrap_or_default(),
            }
        })
        .collect();
    out_songs.sort_by(|a, b| {
        a.album_id
            .cmp(&b.album_id)
            .then_with(|| a.track.cmp(&b.track))
            .then_with(|| a.id.cmp(&b.id))
    });

    (out_artists, out_albums, out_songs)
}

impl AuthStore for ProductionStore {
    // The snapshot is constructed only after the async core succeeded, so
    // any verification the sync dispatcher asks for is a re-statement of
    // that fact. The sync dispatcher never calls these with credentials
    // the async core rejected (error 42/43/44/40/10 are answered before
    // dispatch), and fixture credentials never reach this store.
    fn verify_api_key(&self, _api_key: &str) -> Option<Identity> {
        Some(self.identity.clone())
    }

    fn verify_password(&self, _username: &str, _password: &str) -> Option<Identity> {
        Some(self.identity.clone())
    }

    fn verify_token(&self, _username: &str, _token: &str, _salt: &str) -> Option<Identity> {
        Some(self.identity.clone())
    }
}

fn not_authorized() -> (i64, String) {
    (
        code::NOT_AUTHORIZED,
        "User is not authorized for the given operation.".to_string(),
    )
}

fn catalogue_date_desc(left: &Option<String>, right: &Option<String>) -> std::cmp::Ordering {
    left.as_ref()
        .map(std::cmp::Reverse)
        .cmp(&right.as_ref().map(std::cmp::Reverse))
}

impl Library for ProductionStore {
    fn user_account(&self, username: &str) -> Option<UserAccount> {
        (username == self.identity.username).then(|| self.account.clone())
    }

    fn artists(&self) -> Vec<Artist> {
        self.artists.clone()
    }

    fn artists_last_modified(&self) -> i64 {
        self.now_millis
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

    fn album_list(&self, query: AlbumListQuery<'_>) -> Vec<Album> {
        let mut albums = self.all_albums();
        let mut offset = query.offset;
        match query.list_type {
            "random" => {
                // Deterministic id order: no RNG in a per-request snapshot.
                albums.sort_by(|a, b| a.id.cmp(&b.id));
                offset = 0;
            }
            // No play history exists yet: frequent keeps every album eligible
            // (play_count 0, MemStore order), recent/highest/starred are empty.
            "frequent" => {}
            "recent" => return Vec::new(),
            "byYear" => {
                if let (Some(from), Some(to)) = (query.from_year, query.to_year) {
                    albums.retain(|album| {
                        album
                            .year
                            .is_some_and(|year| (from.min(to)..=from.max(to)).contains(&year))
                    });
                    albums.sort_by(|a, b| {
                        let years = if from > to {
                            b.year.cmp(&a.year)
                        } else {
                            a.year.cmp(&b.year)
                        };
                        years.then(a.name.cmp(&b.name))
                    });
                }
            }
            "byGenre" => return Vec::new(),
            "highest" => return Vec::new(),
            "starred" => return Vec::new(),
            "alphabeticalByName" => albums.sort_by(|a, b| a.name.cmp(&b.name)),
            "alphabeticalByArtist" => {
                albums.sort_by(|a, b| a.artist.cmp(&b.artist).then(a.name.cmp(&b.name)))
            }
            "newest" => albums.sort_by(|a, b| b.created.cmp(&a.created)),
            _ => return Vec::new(),
        }
        albums.into_iter().skip(offset).take(query.size).collect()
    }

    fn album_list2(&self, query: AlbumListQuery<'_>) -> Result<Vec<Album>, (i64, String)> {
        // Albums with at least one member song, via a set so the filter is
        // O(songs + albums) rather than a per-album scan of the snapshot.
        let albums_with_songs: HashSet<&str> = self
            .songs
            .iter()
            .map(|song| song.album_id.as_str())
            .collect();
        let mut albums = self.all_albums();
        albums.retain(|album| albums_with_songs.contains(album.id.as_str()));
        match query.list_type {
            "random" => albums.sort_by(|a, b| a.id.cmp(&b.id)),
            "newest" => albums.sort_by(|a, b| catalogue_date_desc(&a.curated_at, &b.curated_at)),
            "alphabeticalByName" => albums.sort_by(|a, b| a.name.cmp(&b.name)),
            "alphabeticalByArtist" => albums.sort_by(|a, b| {
                a.artist_sort_name
                    .cmp(&b.artist_sort_name)
                    .then(a.name.cmp(&b.name))
            }),
            "byYear" => {
                if let (Some(from), Some(to)) = (query.from_year, query.to_year) {
                    albums
                        .retain(|album| album.year.is_some_and(|year| (from..=to).contains(&year)));
                }
                albums.sort_by(|a, b| {
                    catalogue_date_desc(&a.release_date, &b.release_date)
                        .then_with(|| catalogue_date_desc(&a.curated_at, &b.curated_at))
                });
            }
            kind => {
                return Err((
                    code::GENERIC,
                    format!("Unsupported album list type: {kind}"),
                ))
            }
        }
        Ok(albums
            .into_iter()
            .skip(query.offset)
            .take(query.size)
            .collect())
    }

    fn random_songs(&self, _identity: &Identity, size: usize) -> Vec<Song> {
        let mut songs = self.all_songs();
        songs.sort_by(|a, b| a.id.cmp(&b.id));
        songs.truncate(size);
        songs
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
        let mut counts: BTreeMap<String, (std::collections::HashSet<String>, i64)> =
            BTreeMap::new();
        for song in &self.songs {
            if song.genre.is_empty() {
                continue;
            }
            let entry = counts.entry(song.genre.clone()).or_default();
            entry.0.insert(song.album_id.clone());
            entry.1 += 1;
        }
        counts
            .into_iter()
            .map(|(name, (albums, songs))| (name, songs, albums.len() as i64))
            .collect()
    }

    fn audio(&self, _song_id: &str) -> Option<Vec<u8>> {
        // Never fixture bytes; the mount serves `stream` through grants.
        None
    }

    fn cover(&self, _cover_id: &str) -> Option<Vec<u8>> {
        // Rights-flagged art: never cached CAA bytes (see module docs).
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
        // The trait is infallible, so refusal is signalled with an id-less
        // placeholder: nothing is stored, getPlaylists stays empty, and a
        // following getPlaylist(id) can never resolve this id. Clients
        // must treat an empty playlist id as refusal.
        Playlist {
            id: String::new(),
            name,
            owner: identity.username.clone(),
            song_ids: Vec::new(),
            created: NOW.to_string(),
            changed: NOW.to_string(),
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
        Err(not_authorized())
    }

    fn delete_playlist(&mut self, _identity: &Identity, _id: &str) -> Result<(), (i64, String)> {
        Err(not_authorized())
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
        // The endpoint ignores this return (fire-and-forget reporting); no
        // history table exists, so nothing is durably recorded.
        false
    }

    fn queue(&self, _identity: &Identity) -> Option<PlayQueue> {
        None
    }

    fn save_queue(&mut self, _identity: &Identity, _queue: PlayQueue) {}
}

#[cfg(test)]
mod tests;
