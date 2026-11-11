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

use std::collections::HashMap;

use nntune_subsonic::auth::Identity;
use nntune_subsonic::library::{Album, Artist, Song, UserAccount, NOW};

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

#[cfg(test)]
mod tests;
