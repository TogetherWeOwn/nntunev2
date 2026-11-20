use nntune_subsonic::construction_budget::{BudgetExceeded, ConstructionBudget};

use crate::catalogue::{ArtistDbRow, ReleaseDbRow, TrackDbRow};

#[derive(Clone, Copy)]
pub enum SyntheticCatalogue {
    Empty,
    Small,
}

pub(super) type Rows = (Vec<ArtistDbRow>, Vec<ReleaseDbRow>, Vec<TrackDbRow>);

const ARTIST: &str = "00000000-0000-4000-8000-000000000001";
const RELEASE: &str = "00000000-0000-4000-8000-000000000002";
const RECORDING: &str = "00000000-0000-4000-8000-000000000003";
const NAME: &str = "Fixture 雪 & Wind";
const ALBUM: &str = "Fixture <Album>";
const TRACK: &str = "Fixture \"Track\"";
const TEXT: [&str; 13] = [
    ARTIST, NAME, NAME, RELEASE, ALBUM, ARTIST, NAME, RELEASE, RECORDING, TRACK, ALBUM, ARTIST,
    NAME,
];

impl SyntheticCatalogue {
    /// One row of each kind at most; no caller text, external catalogue or DB.
    pub(super) fn rows(self, budget: &mut ConstructionBudget) -> Result<Rows, BudgetExceeded> {
        if matches!(self, Self::Empty) {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }
        let mut aggregate = 0usize;
        for value in TEXT {
            aggregate = aggregate.checked_add(value.len()).ok_or(BudgetExceeded)?;
            if value.len() > 1024 || aggregate > 262_144 {
                return Err(BudgetExceeded);
            }
            budget.charge(value.len(), 1, 1)?;
        }
        fn charge_row<T>(budget: &mut ConstructionBudget) -> Result<(), BudgetExceeded> {
            budget.charge(1, std::mem::size_of::<T>(), std::mem::align_of::<T>())
        }
        charge_row::<ArtistDbRow>(budget)?;
        charge_row::<ReleaseDbRow>(budget)?;
        charge_row::<TrackDbRow>(budget)?;

        let [artist_id, name, sort_name, release_id, album, release_artist, release_name, track_release, recording, track, track_album, track_artist, track_name] =
            TEXT;
        let copy = |text: &str| -> Result<String, BudgetExceeded> {
            let mut value = String::new();
            value
                .try_reserve_exact(text.len())
                .map_err(|_| BudgetExceeded)?;
            if value.capacity() != text.len() {
                return Err(BudgetExceeded);
            }
            value.push_str(text);
            Ok(value)
        };
        Ok((
            singleton(ArtistDbRow {
                mbid: copy(artist_id)?,
                name: copy(name)?,
                sort_name: copy(sort_name)?,
                artist_type: None,
                country: None,
            })?,
            singleton(ReleaseDbRow {
                mbid: copy(release_id)?,
                title: copy(album)?,
                release_type: None,
                year: Some(2026),
                artist_mbid: Some(copy(release_artist)?),
                artist_name: Some(copy(release_name)?),
            })?,
            singleton(TrackDbRow {
                release_mbid: copy(track_release)?,
                recording_mbid: copy(recording)?,
                title: copy(track)?,
                duration_ms: Some(123_000),
                medium_no: Some(1),
                track_no: Some(1),
                release_title: copy(track_album)?,
                artist_mbid: Some(copy(track_artist)?),
                artist_name: Some(copy(track_name)?),
            })?,
        ))
    }
}

fn singleton<T>(row: T) -> Result<Vec<T>, BudgetExceeded> {
    let mut rows = Vec::new();
    rows.try_reserve_exact(1).map_err(|_| BudgetExceeded)?;
    if rows.capacity() != 1 {
        return Err(BudgetExceeded);
    }
    rows.push(row);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_rows_consume_the_carried_budget() {
        let mut budget = ConstructionBudget::new(0);
        let (a, r, t) = SyntheticCatalogue::Empty.rows(&mut budget).unwrap();
        assert!(a.is_empty() && r.is_empty() && t.is_empty());
        assert_eq!(budget.charged(), 0);

        let mut measure = ConstructionBudget::new(usize::MAX);
        measure.charge(10, 1, 1).unwrap();
        SyntheticCatalogue::Small.rows(&mut measure).unwrap();
        let exact = measure.charged();
        for limit in [exact - 1, exact] {
            let mut budget = ConstructionBudget::new(limit);
            budget.charge(10, 1, 1).unwrap();
            let result = SyntheticCatalogue::Small.rows(&mut budget);
            if limit == exact {
                let (a, r, t) = result.unwrap();
                assert_eq!((a.capacity(), r.capacity(), t.capacity()), (1, 1, 1));
                assert_eq!(budget.charged(), exact);
            } else {
                assert_eq!(result.unwrap_err(), BudgetExceeded);
            }
        }
    }
}
