//! Native tests for the production dispatch core. Every assertion here is
//! about caller-visible Subsonic behaviour of [`ProductionStore`] and the
//! error/stream helpers — no database, no worker types.

use super::{build_snapshot, ProductionStore, FLAC_CONTENT_TYPE, FLAC_SUFFIX};
use crate::catalogue::{ArtistDbRow, ReleaseDbRow, TrackDbRow};
use crate::ledger::STREAM_BYTES_PER_SEC;
use nntune_subsonic::auth::Identity;
use nntune_subsonic::library::{Library, UserAccount, NOW};

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
