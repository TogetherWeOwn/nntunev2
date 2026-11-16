//! Inert output-allocation planner for `subsonic_dispatch::build_snapshot`.
//!
//! The inert builder rejects raw rows without precharge provenance.
//! It plans output charges before allocating and uses borrowed scans and bounded
//! in-place ordering instead of the existing mapper's maps and sort scratch.
//! Charges are cumulative; after an error discard the budget. This does not
//! authorize mounting or account for database collection or runtime memory.

use nntune_subsonic::construction_budget::{BudgetExceeded, ConstructionBudget};
use nntune_subsonic::library::{Album, Artist, Song, NOW};

use crate::catalogue::{ArtistDbRow, ReleaseDbRow, TrackDbRow};
use crate::subsonic_dispatch::{FLAC_CONTENT_TYPE, FLAC_SUFFIX};

type Snapshot = (Vec<Artist>, Vec<Album>, Vec<Song>);

fn vec_of<T>(b: &mut ConstructionBudget, n: usize) -> Result<(), BudgetExceeded> {
    // An empty Vec or ZST buffer does not allocate.
    if n == 0 || std::mem::size_of::<T>() == 0 {
        return Ok(());
    }
    b.charge(n, std::mem::size_of::<T>(), std::mem::align_of::<T>())
}

/// A cloned `String` has capacity == len; an empty one does not allocate.
fn string(b: &mut ConstructionBudget, len: usize) -> Result<(), BudgetExceeded> {
    if len == 0 {
        return Ok(());
    }
    b.charge(len, 1, 1)
}

fn opt(b: &mut ConstructionBudget, s: &Option<String>) -> Result<(), BudgetExceeded> {
    string(b, s.as_deref().map_or(0, str::len))
}

/// Even an empty borrowed slice can have a retained, allocated backing vector.
/// Without a raw-row receipt no input is admitted.
pub fn reject_unproven_rows(
    _artists: &[ArtistDbRow],
    _releases: &[ReleaseDbRow],
    _tracks: &[TrackDbRow],
) -> Result<(), BudgetExceeded> {
    Err(BudgetExceeded)
}

/// Charge the exact output vectors and per-field string copies of
/// `build_snapshot(artists, releases, tracks)`, in a fixed order.
pub fn plan_snapshot_output(
    budget: &mut ConstructionBudget,
    artists: &[ArtistDbRow],
    releases: &[ReleaseDbRow],
    tracks: &[TrackDbRow],
) -> Result<(), BudgetExceeded> {
    vec_of::<Artist>(budget, artists.len())?;
    vec_of::<Album>(budget, releases.len())?;
    vec_of::<Song>(budget, tracks.len())?;
    for a in artists {
        string(budget, a.mbid.len())?;
        string(budget, a.name.len())?;
    }
    for r in releases {
        let artist_id = r.artist_mbid.as_deref().unwrap_or("");
        string(budget, r.mbid.len())?;
        string(budget, r.title.len())?;
        opt(budget, &r.artist_name)?; // artist
        string(budget, artist_id.len())?;
        string(budget, NOW.len())?;
        // HashMap collect keeps the LAST artist for a duplicate mbid, so the
        // reverse scan finds the same row; otherwise the artist name is copied.
        let sort_len = match artists.iter().rev().find(|a| a.mbid == artist_id) {
            Some(a) => a.sort_name.len(),
            None => r.artist_name.as_deref().map_or(0, str::len),
        };
        string(budget, sort_len)?; // artist_sort_name
    }
    // Duplicate release IDs only change `Song::year` (numeric), not any copy.
    for t in tracks {
        string(budget, t.recording_mbid.len())?;
        string(budget, t.title.len())?;
        string(budget, t.release_title.len())?;
        opt(budget, &t.artist_name)?;
        // genre is empty and never allocates; release_mbid is copied twice.
        string(budget, t.release_mbid.len())?; // cover_art
        string(budget, FLAC_CONTENT_TYPE.len())?;
        string(budget, FLAC_SUFFIX.len())?;
        string(budget, NOW.len())?;
        string(budget, t.release_mbid.len())?; // album_id
        opt(budget, &t.artist_mbid)?;
    }
    Ok(())
}

/// Construct only when retained raw allocations have a trustworthy receipt.
/// No such receipt is currently available: all inputs are rejected before
/// any charge or allocation. This entry point is not connected to the mount.
pub fn build_bounded_snapshot(
    budget: &mut ConstructionBudget,
    artists: &[ArtistDbRow],
    releases: &[ReleaseDbRow],
    tracks: &[TrackDbRow],
    work: crate::subsonic_dispatch::snapshot_order::OrderWork,
) -> Result<Snapshot, BudgetExceeded> {
    reject_unproven_rows(artists, releases, tracks)?;
    construct_snapshot(budget, artists, releases, tracks, work)
}

fn construct_snapshot(
    budget: &mut ConstructionBudget,
    artists: &[ArtistDbRow],
    releases: &[ReleaseDbRow],
    tracks: &[TrackDbRow],
    work: crate::subsonic_dispatch::snapshot_order::OrderWork,
) -> Result<Snapshot, BudgetExceeded> {
    use crate::subsonic_dispatch::snapshot_order::{order_snapshot, order_work};
    let needed = order_work(artists.len(), releases.len(), tracks.len(), artists.len())
        .ok_or(BudgetExceeded)?;
    if needed.comparisons > work.comparisons || needed.artist_row_probes > work.artist_row_probes {
        return Err(BudgetExceeded);
    }
    plan_snapshot_output(budget, artists, releases, tracks)?;
    fn copy(s: &str) -> Result<String, BudgetExceeded> {
        let mut out = String::new();
        out.try_reserve_exact(s.len()).map_err(|_| BudgetExceeded)?;
        out.push_str(s);
        Ok(out)
    }
    fn vector<T>(len: usize) -> Result<Vec<T>, BudgetExceeded> {
        let mut out = Vec::new();
        out.try_reserve_exact(len).map_err(|_| BudgetExceeded)?;
        Ok(out)
    }
    let mut out_artists = vector(artists.len())?;
    let mut out_albums = vector(releases.len())?;
    let mut out_songs = vector(tracks.len())?;
    for a in artists {
        out_artists.push(Artist {
            id: copy(&a.mbid)?,
            name: copy(&a.name)?,
        });
    }
    for r in releases {
        let artist_id = r.artist_mbid.as_deref().unwrap_or("");
        let artist = r.artist_name.as_deref().unwrap_or("");
        let sort = artists
            .iter()
            .rev()
            .find(|a| a.mbid == artist_id)
            .map_or(artist, |a| a.sort_name.as_str());
        let mut duration = 0i64;
        for t in tracks.iter().filter(|t| t.release_mbid == r.mbid) {
            duration = duration
                .checked_add(i64::from(t.duration_ms.unwrap_or(0)) / 1000)
                .ok_or(BudgetExceeded)?;
        }
        out_albums.push(Album {
            id: copy(&r.mbid)?,
            name: copy(&r.title)?,
            artist: copy(artist)?,
            artist_id: copy(artist_id)?,
            duration,
            created: copy(NOW)?,
            year: r.year.map(i64::from),
            release_date: None,
            curated_at: None,
            artist_sort_name: copy(sort)?,
        });
    }
    for t in tracks {
        let duration = i64::from(t.duration_ms.unwrap_or(0)) / 1000;
        let year = releases
            .iter()
            .rev()
            .find(|r| r.mbid == t.release_mbid)
            .and_then(|r| r.year)
            .map(i64::from)
            .unwrap_or(0);
        out_songs.push(Song {
            id: copy(&t.recording_mbid)?,
            title: copy(&t.title)?,
            album: copy(&t.release_title)?,
            artist: copy(t.artist_name.as_deref().unwrap_or(""))?,
            track: t.track_no.map(i64::from).unwrap_or(0),
            year,
            genre: String::new(),
            cover_art: copy(&t.release_mbid)?,
            size: duration
                .checked_mul(crate::ledger::STREAM_BYTES_PER_SEC as i64)
                .ok_or(BudgetExceeded)?,
            content_type: copy(FLAC_CONTENT_TYPE)?,
            suffix: copy(FLAC_SUFFIX)?,
            duration,
            disc: t.medium_no.map(i64::from).unwrap_or(1),
            created: copy(NOW)?,
            album_id: copy(&t.release_mbid)?,
            artist_id: copy(t.artist_mbid.as_deref().unwrap_or(""))?,
        });
    }
    order_snapshot(
        &mut out_artists,
        &mut out_albums,
        &mut out_songs,
        artists,
        work,
    )
    .map_err(|_| BudgetExceeded)?;
    Ok((out_artists, out_albums, out_songs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(cap: usize, size: usize) -> usize {
        ((cap * size + 11) & !7).max(16) + 8
    }
    fn artist(mbid: &str, name: &str, sort: &str) -> ArtistDbRow {
        ArtistDbRow {
            mbid: mbid.into(),
            name: name.into(),
            sort_name: sort.into(),
            artist_type: None,
            country: None,
        }
    }
    fn release(mbid: &str, artist: Option<(&str, &str)>) -> ReleaseDbRow {
        ReleaseDbRow {
            mbid: mbid.into(),
            title: "T".into(),
            release_type: None,
            year: None,
            artist_mbid: artist.map(|a| a.0.into()),
            artist_name: artist.map(|a| a.1.into()),
        }
    }
    fn track(release: &str) -> TrackDbRow {
        TrackDbRow {
            release_mbid: release.into(),
            recording_mbid: "rec".into(),
            title: "tt".into(),
            duration_ms: None,
            medium_no: None,
            track_no: None,
            release_title: "".into(),
            artist_mbid: None,
            artist_name: None,
        }
    }
    fn total(a: &[ArtistDbRow], r: &[ReleaseDbRow], t: &[TrackDbRow]) -> usize {
        let mut b = ConstructionBudget::new(usize::MAX);
        plan_snapshot_output(&mut b, a, r, t).unwrap();
        b.charged()
    }

    #[test]
    fn builder_rejects_unproven_rows_before_charging() {
        let work = crate::subsonic_dispatch::snapshot_order::OrderWork {
            comparisons: usize::MAX,
            artist_row_probes: usize::MAX,
        };
        let mut budget = ConstructionBudget::new(usize::MAX);
        assert!(
            build_bounded_snapshot(&mut budget, &[artist("a", "n", "s")], &[], &[], work).is_err()
        );
        assert_eq!(budget.charged(), 0);
        let empty: Vec<ArtistDbRow> = Vec::with_capacity(1);
        assert!(build_bounded_snapshot(&mut budget, &empty, &[], &[], work).is_err());
        assert_eq!(budget.charged(), 0);
    }

    #[test]
    fn internal_mapper_preserves_duplicate_occurrences_and_fields() {
        let artists = [artist("a", "First", "Z"), artist("a", "Last", "A")];
        let mut releases = [release("r", Some(("a", "First"))), release("r", None)];
        releases[0].year = Some(2001);
        releases[1].year = Some(2002);
        let mut tracks = [track("r"), track("r")];
        tracks[0].duration_ms = Some(2500);
        tracks[1].duration_ms = Some(3500);
        let work = crate::subsonic_dispatch::snapshot_order::OrderWork {
            comparisons: usize::MAX,
            artist_row_probes: usize::MAX,
        };
        let need = total(&artists, &releases, &tracks);
        let mut budget = ConstructionBudget::new(need);
        let actual = construct_snapshot(&mut budget, &artists, &releases, &tracks, work).unwrap();
        let expected = crate::subsonic_dispatch::build_snapshot(&artists, &releases, &tracks);
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        let mut short = ConstructionBudget::new(need - 1);
        assert!(construct_snapshot(&mut short, &artists, &releases, &tracks, work).is_err());
    }

    #[test]
    fn unproven_rows_fail_closed_for_each_row_kind() {
        assert_eq!(reject_unproven_rows(&[], &[], &[]), Err(BudgetExceeded));
        assert_eq!(
            reject_unproven_rows(&[artist("a", "n", "s")], &[], &[]),
            Err(BudgetExceeded)
        );
        assert_eq!(
            reject_unproven_rows(&[], &[release("r", None)], &[]),
            Err(BudgetExceeded)
        );
        assert_eq!(
            reject_unproven_rows(&[], &[], &[track("r")]),
            Err(BudgetExceeded)
        );
    }

    #[test]
    fn empty_input_charges_nothing() {
        assert_eq!(total(&[], &[], &[]), 0);
    }

    #[test]
    fn charges_vectors_and_each_distinct_field_copy() {
        let a = [artist("a1", "Name", "Sort")];
        let r = [release("r1", Some(("a1", "Name")))];
        let t = [track("r1")];
        let sz = std::mem::size_of::<Artist>();
        let al = std::mem::size_of::<Album>();
        let so = std::mem::size_of::<Song>();
        let vectors = cost(1, sz) + cost(1, al) + cost(1, so);
        // artist: id, name
        let artists = cost(2, 1) + cost(4, 1);
        // album: id, title, artist, artist_id, created, sort ("Sort", not name)
        let albums = [2, 1, 4, 2, NOW.len(), 4]
            .map(|n| cost(n, 1))
            .iter()
            .sum::<usize>();
        // song: id, title, cover_art, content_type, suffix, created, album_id
        // (empty album/artist/genre/artist_id skipped; release_mbid twice)
        let songs = [
            3,
            2,
            2,
            FLAC_CONTENT_TYPE.len(),
            FLAC_SUFFIX.len(),
            NOW.len(),
            2,
        ]
        .map(|n| cost(n, 1))
        .iter()
        .sum::<usize>();
        assert_eq!(total(&a, &r, &t), vectors + artists + albums + songs);
    }

    #[test]
    fn checked_overflow_fails_closed() {
        let mut b = ConstructionBudget::new(usize::MAX);
        assert_eq!(vec_of::<Song>(&mut b, usize::MAX), Err(BudgetExceeded));
        assert_eq!(b.charged(), 0);
    }
}
