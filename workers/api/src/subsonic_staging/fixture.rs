use nntune_subsonic::construction_budget::{BudgetExceeded, ConstructionBudget};

use crate::catalogue::{ArtistDbRow, ReleaseDbRow, TrackDbRow};

#[derive(Clone, Copy)]
pub struct SyntheticCatalogue {
    releases: usize,
    tracks: usize,
    padded_title_bytes: Option<usize>,
}

pub(super) type Rows = (Vec<ArtistDbRow>, Vec<ReleaseDbRow>, Vec<TrackDbRow>);

const MAX_ROWS: usize = 1024;
const MAX_FIELD: usize = 1024;
const MAX_TEXT: usize = 262_144;
const ARTIST: &str = "00000000-0000-4000-8000-000000000001";
const RELEASES: [&str; 4] = [
    "00000000-0000-4000-8000-000000000002",
    "00000000-0000-4000-8000-000000000004",
    "00000000-0000-4000-8000-000000000005",
    "00000000-0000-4000-8000-000000000006",
];
const RECORDING: &str = "00000000-0000-4000-8000-000000000003";
const NAME: &str = "Fixture 雪 & Wind";
const ALBUM: &str = "Fixture <Album>";
const TRACK: &str = "Fixture \"Track\"";
const ESCAPES: &str = "雪<&>\"\\";
const ARTIST_FIELDS: [&str; 3] = [ARTIST, NAME, NAME];
const ESCAPE_REPETITIONS: usize = 8;

impl SyntheticCatalogue {
    /// Only a trusted preset may select a recipe. No request parameters,
    /// external catalogue or DB values enter this generator.
    pub fn from_trusted_id(id: &str) -> Option<Self> {
        let (releases, tracks, padded_title_bytes) = match id {
            "empty" => (0, 0, None),
            "small" => (1, 1, None),
            "rows-below" => (1, 1023, None),
            "rows-exact" => (1, 1024, None),
            "rows-over" => (1, 1025, None),
            "field-below" => (1, 1, Some(1023)),
            "field-exact" => (1, 1, Some(1024)),
            "field-over" => (1, 1, Some(1025)),
            "text-below" => (1, 256, Some(225_870)),
            "text-exact" => (1, 256, Some(225_871)),
            "text-over" => (1, 256, Some(225_872)),
            "output-json-below" => (1, 256, Some(114_652)),
            "output-json-exact" => (1, 256, Some(114_653)),
            "output-json-over" => (1, 256, Some(114_654)),
            "output-xml-below" => (1, 256, Some(91_828)),
            "output-xml-exact" => (1, 256, Some(91_829)),
            "output-xml-over" => (1, 256, Some(91_830)),
            "output-jsonp-below" => (1, 256, Some(114_635)),
            "output-jsonp-exact" => (1, 256, Some(114_636)),
            "output-jsonp-over" => (1, 256, Some(114_637)),
            "large-mixed" => (4, 512, Some(512 * 256)),
            _ => return None,
        };
        Some(Self {
            releases,
            tracks,
            padded_title_bytes,
        })
    }

    fn title_len(self, index: usize) -> usize {
        match self.padded_title_bytes {
            Some(total) => total / self.tracks + usize::from(index < total % self.tracks),
            None => TRACK.len(),
        }
    }

    fn preflight(self, budget: &mut ConstructionBudget) -> Result<(), BudgetExceeded> {
        if self.releases == 0 || self.tracks > MAX_ROWS {
            return Err(BudgetExceeded);
        }
        let mut aggregate = 0usize;
        let mut text = |len| {
            aggregate = aggregate.checked_add(len).ok_or(BudgetExceeded)?;
            if len > MAX_FIELD || aggregate > MAX_TEXT {
                return Err(BudgetExceeded);
            }
            budget.charge(len, 1, 1)
        };
        for field in ARTIST_FIELDS {
            text(field.len())?;
        }
        for release in RELEASES.get(..self.releases).ok_or(BudgetExceeded)? {
            for field in [release, ALBUM, ARTIST, NAME] {
                text(field.len())?;
            }
        }
        for index in 0..self.tracks {
            let release = RELEASES[index * self.releases / self.tracks];
            for field in [release, RECORDING, ALBUM, ARTIST, NAME] {
                text(field.len())?;
            }
            let len = self.title_len(index);
            if self.padded_title_bytes.is_some() && len < ESCAPES.len() * ESCAPE_REPETITIONS {
                return Err(BudgetExceeded);
            }
            text(len)?;
        }
        fn charge<T>(budget: &mut ConstructionBudget, len: usize) -> Result<(), BudgetExceeded> {
            budget.charge(len, std::mem::size_of::<T>(), std::mem::align_of::<T>())
        }
        charge::<ArtistDbRow>(budget, 1)?;
        charge::<ReleaseDbRow>(budget, self.releases)?;
        charge::<TrackDbRow>(budget, self.tracks)
    }

    pub(super) fn rows(self, budget: &mut ConstructionBudget) -> Result<Rows, BudgetExceeded> {
        if self.releases == 0 && self.tracks == 0 && self.padded_title_bytes.is_none() {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }
        self.preflight(budget)?;
        let (mut artists, mut releases, mut tracks) =
            (exact(1)?, exact(self.releases)?, exact(self.tracks)?);
        artists.push(ArtistDbRow {
            mbid: copy(ARTIST)?,
            name: copy(NAME)?,
            sort_name: copy(NAME)?,
            artist_type: None,
            country: None,
        });
        for &release in &RELEASES[..self.releases] {
            releases.push(ReleaseDbRow {
                mbid: copy(release)?,
                title: copy(ALBUM)?,
                release_type: None,
                year: Some(2026),
                artist_mbid: Some(copy(ARTIST)?),
                artist_name: Some(copy(NAME)?),
            });
        }
        for index in 0..self.tracks {
            let release = RELEASES[index * self.releases / self.tracks];
            let title = if self.padded_title_bytes.is_some() {
                let len = self.title_len(index);
                let mut bytes = exact(len)?;
                for _ in 0..ESCAPE_REPETITIONS {
                    bytes.extend_from_slice(ESCAPES.as_bytes());
                }
                bytes.resize(len, b'x');
                String::from_utf8(bytes).map_err(|_| BudgetExceeded)?
            } else {
                copy(TRACK)?
            };
            tracks.push(TrackDbRow {
                release_mbid: copy(release)?,
                recording_mbid: copy(RECORDING)?,
                title,
                duration_ms: Some(123_000),
                medium_no: Some(1),
                track_no: Some(index as i16 + 1),
                release_title: copy(ALBUM)?,
                artist_mbid: Some(copy(ARTIST)?),
                artist_name: Some(copy(NAME)?),
            });
        }
        Ok((artists, releases, tracks))
    }
}

fn exact<T>(len: usize) -> Result<Vec<T>, BudgetExceeded> {
    let mut values = Vec::new();
    values.try_reserve_exact(len).map_err(|_| BudgetExceeded)?;
    (values.capacity() == len)
        .then_some(values)
        .ok_or(BudgetExceeded)
}

fn copy(text: &str) -> Result<String, BudgetExceeded> {
    let mut bytes = exact(text.len())?;
    bytes.extend_from_slice(text.as_bytes());
    String::from_utf8(bytes).map_err(|_| BudgetExceeded)
}

#[cfg(all(target_arch = "wasm32", feature = "staging-body-wasm-test"))]
mod wasm_test {
    use super::*;
    use worker::wasm_bindgen::{self, prelude::*};

    #[wasm_bindgen]
    pub fn staging_fixture_charge(limit: u32) -> Option<u32> {
        let mut budget =
            ConstructionBudget::new((limit as usize).min(super::super::CONSTRUCTION_BYTES));
        SyntheticCatalogue::from_trusted_id("large-mixed")?
            .rows(&mut budget)
            .ok()?;
        Some(budget.charged() as u32)
    }
}

#[cfg(test)]
#[test]
fn empty_needs_no_charge_and_small_carries_prior_charge() {
    let mut budget = ConstructionBudget::new(0);
    let fixture = |id| SyntheticCatalogue::from_trusted_id(id).unwrap();
    let (a, r, t) = fixture("empty").rows(&mut budget).unwrap();
    assert_eq!(
        (a.capacity(), r.capacity(), t.capacity(), budget.charged()),
        (0, 0, 0, 0)
    );
    let mut budget = ConstructionBudget::new(4096);
    budget.charge(10, 1, 1).unwrap();
    let carried = budget.charged();
    let (a, r, t) = fixture("small").rows(&mut budget).unwrap();
    assert_eq!((a.capacity(), r.capacity(), t.capacity()), (1, 1, 1));
    assert!(budget.charged() > carried);
}

#[cfg(test)]
#[test]
fn tracks_without_releases_are_not_an_empty_fixture() {
    let mut budget = ConstructionBudget::new(4096);
    let fixture = SyntheticCatalogue {
        releases: 0,
        tracks: 1,
        padded_title_bytes: None,
    };
    assert!(fixture.rows(&mut budget).is_err());
    assert_eq!(budget.charged(), 0);
}
