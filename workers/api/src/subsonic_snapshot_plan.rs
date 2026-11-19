//! Inert accounting for snapshot output allocations only.
//!
//! Success does not admit raw rows or authorize construction or mounting.
//! Raw row allocations, database collection, ordering work and other runtime
//! allocations require separate accounting. Charges are cumulative; discard
//! the budget after an error.

use nntune_subsonic::construction_budget::{BudgetExceeded, ConstructionBudget};
use nntune_subsonic::library::{Album, Artist, Song, NOW};

use crate::catalogue::{ArtistDbRow, ReleaseDbRow, TrackDbRow};
use crate::subsonic_dispatch::{FLAC_CONTENT_TYPE, FLAC_SUFFIX};

#[path = "subsonic_raw_fixture_plan.rs"]
mod raw_fixture_plan;
#[path = "subsonic_snapshot_bytes.rs"]
mod snapshot_bytes;
#[path = "subsonic_snapshot_work.rs"]
mod snapshot_work;

fn vec_of<T>(b: &mut ConstructionBudget, n: usize) -> Result<(), BudgetExceeded> {
    if n == 0 || std::mem::size_of::<T>() == 0 {
        return Ok(());
    }
    b.charge(n, std::mem::size_of::<T>(), std::mem::align_of::<T>())
}

fn string(b: &mut ConstructionBudget, len: usize) -> Result<(), BudgetExceeded> {
    if len == 0 {
        return Ok(());
    }
    b.charge(len, 1, 1)
}

fn opt(b: &mut ConstructionBudget, s: &Option<String>) -> Result<(), BudgetExceeded> {
    string(b, s.as_deref().map_or(0, str::len))
}

/// Charge output vectors and string copies, without allocating or admitting rows.
/// The future constructor must reserve the planned capacities before copying.
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
        opt(budget, &r.artist_name)?;
        string(budget, artist_id.len())?;
        string(budget, NOW.len())?;
        // The existing mapper keeps the last artist with a duplicate ID.
        let sort_len = match artists.iter().rev().find(|a| a.mbid == artist_id) {
            Some(a) => a.sort_name.len(),
            None => r.artist_name.as_deref().map_or(0, str::len),
        };
        string(budget, sort_len)?;
    }
    for t in tracks {
        string(budget, t.recording_mbid.len())?;
        string(budget, t.title.len())?;
        string(budget, t.release_title.len())?;
        opt(budget, &t.artist_name)?;
        string(budget, t.release_mbid.len())?; // cover art
        string(budget, FLAC_CONTENT_TYPE.len())?;
        string(budget, FLAC_SUFFIX.len())?;
        string(budget, NOW.len())?;
        string(budget, t.release_mbid.len())?; // album ID
        opt(budget, &t.artist_mbid)?;
    }
    Ok(())
}

// This receipt owns its rows and exclusively borrows the ledger for their
// entire lifetime. It deliberately has no mutable row access or constructor
// accepting caller-owned vectors. Only the zero-allocation case is proven:
// try_reserve_exact does not promise an upper bound on allocator capacity.
// Nonempty construction must remain unavailable until that bound is proven.
#[allow(dead_code)]
struct OwnedRawRows<'a> {
    budget: &'a mut ConstructionBudget,
    artists: Vec<ArtistDbRow>,
    releases: Vec<ReleaseDbRow>,
    tracks: Vec<TrackDbRow>,
}

#[allow(dead_code)]
impl<'a> OwnedRawRows<'a> {
    fn try_copy(
        budget: &'a mut ConstructionBudget,
        artists: &[ArtistDbRow],
        releases: &[ReleaseDbRow],
        tracks: &[TrackDbRow],
    ) -> Result<Self, BudgetExceeded> {
        // The caller's storage is not retained. No reservation or copy is
        // attempted until its maximum retained capacity can be established.
        if !artists.is_empty() || !releases.is_empty() || !tracks.is_empty() {
            return Err(BudgetExceeded);
        }
        Ok(Self {
            budget,
            artists: Vec::new(),
            releases: Vec::new(),
            tracks: Vec::new(),
        })
    }

    fn rows(&self) -> (&[ArtistDbRow], &[ReleaseDbRow], &[TrackDbRow]) {
        (&self.artists, &self.releases, &self.tracks)
    }
}

type Snapshot = (Vec<Artist>, Vec<Album>, Vec<Song>);

/// Even an empty borrowed slice can have a retained, allocated backing vector.
/// Without a raw-row receipt no input is admitted.
pub fn reject_unproven_rows(
    _artists: &[ArtistDbRow],
    _releases: &[ReleaseDbRow],
    _tracks: &[TrackDbRow],
) -> Result<(), BudgetExceeded> {
    Err(BudgetExceeded)
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
    fn empty_owned_receipt_retains_ledger_and_no_allocations() {
        let mut budget = ConstructionBudget::new(usize::MAX);
        budget.charge(1, 1, 1).unwrap();
        let prior = budget.charged();
        let borrowed_artists = Vec::<ArtistDbRow>::with_capacity(1);
        let borrowed_releases = Vec::<ReleaseDbRow>::with_capacity(1);
        let borrowed_tracks = Vec::<TrackDbRow>::with_capacity(1);
        {
            let receipt = OwnedRawRows::try_copy(
                &mut budget,
                &borrowed_artists,
                &borrowed_releases,
                &borrowed_tracks,
            )
            .unwrap();
            assert_eq!(receipt.budget.charged(), prior);
            assert_eq!(receipt.artists.capacity(), 0);
            assert_eq!(receipt.releases.capacity(), 0);
            assert_eq!(receipt.tracks.capacity(), 0);
            let (artists, releases, tracks) = receipt.rows();
            assert_eq!(
                reject_unproven_rows(artists, releases, tracks),
                Err(BudgetExceeded)
            );
        }
        assert_eq!(budget.charged(), prior);
    }

    #[test]
    fn owned_copy_rejects_each_unproven_nonempty_input_without_charging() {
        let artists = [artist("", "", "")];
        let releases = [release("", None)];
        let tracks = [track("")];
        let mut budget = ConstructionBudget::new(usize::MAX);
        budget.charge(1, 1, 1).unwrap();
        let prior = budget.charged();
        for (a, r, t) in [
            (artists.as_slice(), &[][..], &[][..]),
            (&[][..], releases.as_slice(), &[][..]),
            (&[][..], &[][..], tracks.as_slice()),
        ] {
            assert!(OwnedRawRows::try_copy(&mut budget, a, r, t).is_err());
            assert_eq!(budget.charged(), prior);
        }
    }

    #[test]
    fn empty_receipt_at_exhausted_limit_does_not_carry_borrowed_storage() {
        let mut budget = ConstructionBudget::new(0);
        let artists = Vec::<ArtistDbRow>::with_capacity(32);
        let receipt = OwnedRawRows::try_copy(&mut budget, &artists, &[], &[]).unwrap();
        assert_eq!(receipt.budget.charged(), 0);
        assert_eq!(receipt.artists.capacity(), 0);
        assert!(receipt.rows().0.is_empty());
    }

    #[test]
    fn raw_copy_plan_rejects_overflow_with_existing_charge() {
        let mut budget = ConstructionBudget::new(usize::MAX);
        budget.charge(1, 1, 1).unwrap();
        let prior = budget.charged();
        assert_eq!(
            vec_of::<TrackDbRow>(&mut budget, usize::MAX),
            Err(BudgetExceeded)
        );
        assert_eq!(string(&mut budget, usize::MAX), Err(BudgetExceeded));
        assert_eq!(budget.charged(), prior);
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
        let empty_artists: Vec<ArtistDbRow> = Vec::with_capacity(1);
        let empty_releases: Vec<ReleaseDbRow> = Vec::with_capacity(1);
        let empty_tracks: Vec<TrackDbRow> = Vec::with_capacity(1);
        for (artists, releases, tracks) in [
            (empty_artists.as_slice(), &[][..], &[][..]),
            (&[][..], empty_releases.as_slice(), &[][..]),
            (&[][..], &[][..], empty_tracks.as_slice()),
        ] {
            assert!(build_bounded_snapshot(&mut budget, artists, releases, tracks, work).is_err());
            assert_eq!(budget.charged(), 0);
        }
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

    fn cost(n: usize, size: usize) -> usize {
        ((n * size + 11) & !7).max(16) + 8
    }

    #[test]
    fn output_field_accounting_and_limit_boundaries() {
        let artists = [
            ArtistDbRow {
                mbid: "a".into(),
                name: "artist".into(),
                sort_name: "ignored".into(),
                artist_type: None,
                country: None,
            },
            ArtistDbRow {
                mbid: "a".into(),
                name: "last".into(),
                sort_name: "a much longer sort name".into(),
                artist_type: None,
                country: None,
            },
        ];
        let releases = [
            ReleaseDbRow {
                mbid: "release".into(),
                title: "title".into(),
                release_type: None,
                year: None,
                artist_mbid: Some("a".into()),
                artist_name: Some("fallback".into()),
            },
            ReleaseDbRow {
                mbid: "other".into(),
                title: "".into(),
                release_type: None,
                year: None,
                artist_mbid: None,
                artist_name: Some("fallback name".into()),
            },
        ];
        let tracks = [TrackDbRow {
            release_mbid: "release".into(),
            recording_mbid: "recording".into(),
            title: "song".into(),
            duration_ms: None,
            medium_no: None,
            track_no: None,
            release_title: "album".into(),
            artist_mbid: Some("artist-id".into()),
            artist_name: Some("artist-name".into()),
        }];
        let expected = cost(2, std::mem::size_of::<Artist>())
            + cost(2, std::mem::size_of::<Album>())
            + cost(1, std::mem::size_of::<Song>())
            + [
                1,
                6,
                1,
                4,
                7,
                5,
                8,
                1,
                NOW.len(),
                23,
                5,
                13,
                NOW.len(),
                13,
                9,
                4,
                5,
                11,
                7,
                FLAC_CONTENT_TYPE.len(),
                FLAC_SUFFIX.len(),
                NOW.len(),
                7,
                9,
            ]
            .into_iter()
            .map(|n| cost(n, 1))
            .sum::<usize>();
        let mut exact = ConstructionBudget::new(expected);
        assert_eq!(
            plan_snapshot_output(&mut exact, &artists, &releases, &tracks),
            Ok(())
        );
        assert_eq!(exact.charged(), expected);
        let mut short = ConstructionBudget::new(expected - 1);
        assert_eq!(
            plan_snapshot_output(&mut short, &artists, &releases, &tracks),
            Err(BudgetExceeded)
        );
        assert!(short.charged() < expected);
        let mut extra = ConstructionBudget::new(expected + 1);
        assert_eq!(
            plan_snapshot_output(&mut extra, &artists, &releases, &tracks),
            Ok(())
        );
        assert_eq!(extra.charged(), expected);
    }

    #[test]
    fn empty_and_overflow() {
        let mut budget = ConstructionBudget::new(0);
        assert_eq!(plan_snapshot_output(&mut budget, &[], &[], &[]), Ok(()));
        assert_eq!(budget.charged(), 0);
        let mut budget = ConstructionBudget::new(usize::MAX);
        assert_eq!(vec_of::<Song>(&mut budget, usize::MAX), Err(BudgetExceeded));
        assert_eq!(string(&mut budget, usize::MAX), Err(BudgetExceeded));
        assert_eq!(budget.charged(), 0);
    }
}
