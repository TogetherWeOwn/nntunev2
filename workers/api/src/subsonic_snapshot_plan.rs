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

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(n: usize, size: usize) -> usize {
        ((n * size + 11) & !7).max(16) + 8
    }

    #[test]
    fn output_field_accounting_and_limit_boundaries() {
        let artists = [ArtistDbRow {
            mbid: "a".into(), name: "artist".into(), sort_name: "ignored".into(),
            artist_type: None, country: None,
        }, ArtistDbRow {
            mbid: "a".into(), name: "last".into(), sort_name: "a much longer sort name".into(),
            artist_type: None, country: None,
        }];
        let releases = [ReleaseDbRow {
            mbid: "release".into(), title: "title".into(), release_type: None, year: None,
            artist_mbid: Some("a".into()), artist_name: Some("fallback".into()),
        }, ReleaseDbRow {
            mbid: "other".into(), title: "".into(), release_type: None, year: None,
            artist_mbid: None, artist_name: Some("fallback name".into()),
        }];
        let tracks = [TrackDbRow {
            release_mbid: "release".into(), recording_mbid: "recording".into(),
            title: "song".into(), duration_ms: None, medium_no: None, track_no: None,
            release_title: "album".into(), artist_mbid: Some("artist-id".into()),
            artist_name: Some("artist-name".into()),
        }];
        let expected = cost(2, std::mem::size_of::<Artist>())
            + cost(2, std::mem::size_of::<Album>())
            + cost(1, std::mem::size_of::<Song>())
            + [1, 6, 1, 4, 7, 5, 8, 1, NOW.len(), 23, 5, 13, NOW.len(), 13,
                9, 4, 5, 11, 7, FLAC_CONTENT_TYPE.len(), FLAC_SUFFIX.len(), NOW.len(), 7, 9]
                .into_iter().map(|n| cost(n, 1)).sum::<usize>();
        let mut exact = ConstructionBudget::new(expected);
        assert_eq!(plan_snapshot_output(&mut exact, &artists, &releases, &tracks), Ok(()));
        assert_eq!(exact.charged(), expected);
        let mut short = ConstructionBudget::new(expected - 1);
        assert_eq!(plan_snapshot_output(&mut short, &artists, &releases, &tracks), Err(BudgetExceeded));
        assert!(short.charged() < expected);
        let mut extra = ConstructionBudget::new(expected + 1);
        assert_eq!(plan_snapshot_output(&mut extra, &artists, &releases, &tracks), Ok(()));
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
