//! Logical byte accounting for snapshot construction, not an allocator bound.
//!
//! The caller must account for every raw row and every output copy before it
//! constructs them, and discard all partial output on any error.

use nntune_subsonic::construction_budget::BudgetExceeded;

use crate::catalogue::{ArtistDbRow, ReleaseDbRow, TrackDbRow};

fn optional_len(value: &Option<String>) -> usize {
    value.as_deref().map_or(0, str::len)
}

#[allow(dead_code)]
pub(super) struct SnapshotBytes {
    per_row: usize,
    total_limit: usize,
    total: usize,
}

#[allow(dead_code)]
impl SnapshotBytes {
    pub(super) fn new(per_row: usize, total_limit: usize) -> Self {
        Self {
            per_row,
            total_limit,
            total: 0,
        }
    }

    pub(super) fn artist(&mut self, row: &ArtistDbRow) -> Result<(), BudgetExceeded> {
        self.row(&[
            row.mbid.len(),
            row.name.len(),
            row.sort_name.len(),
            optional_len(&row.artist_type),
            optional_len(&row.country),
        ])
    }

    pub(super) fn release(&mut self, row: &ReleaseDbRow) -> Result<(), BudgetExceeded> {
        self.row(&[
            row.mbid.len(),
            row.title.len(),
            optional_len(&row.release_type),
            optional_len(&row.artist_mbid),
            optional_len(&row.artist_name),
        ])
    }

    pub(super) fn track(&mut self, row: &TrackDbRow) -> Result<(), BudgetExceeded> {
        self.row(&[
            row.release_mbid.len(),
            row.recording_mbid.len(),
            row.title.len(),
            row.release_title.len(),
            optional_len(&row.artist_mbid),
            optional_len(&row.artist_name),
        ])
    }

    pub(super) fn output_artist(&mut self, row: &ArtistDbRow) -> Result<(), BudgetExceeded> {
        self.row(&[row.mbid.len(), row.name.len()])
    }

    pub(super) fn output_release(
        &mut self,
        row: &ReleaseDbRow,
        sort_len: usize,
    ) -> Result<(), BudgetExceeded> {
        self.row(&[
            row.mbid.len(),
            row.title.len(),
            optional_len(&row.artist_name),
            optional_len(&row.artist_mbid),
            nntune_subsonic::library::NOW.len(),
            sort_len,
        ])
    }

    pub(super) fn output_track(&mut self, row: &TrackDbRow) -> Result<(), BudgetExceeded> {
        self.row(&[
            row.recording_mbid.len(),
            row.title.len(),
            row.release_title.len(),
            optional_len(&row.artist_name),
            row.release_mbid.len(), // cover art
            crate::subsonic_dispatch::FLAC_CONTENT_TYPE.len(),
            crate::subsonic_dispatch::FLAC_SUFFIX.len(),
            nntune_subsonic::library::NOW.len(),
            row.release_mbid.len(), // album ID is a separate copy
            optional_len(&row.artist_mbid),
        ])
    }

    pub(super) fn raw_and_output(
        &mut self,
        artists: &[ArtistDbRow],
        releases: &[ReleaseDbRow],
        tracks: &[TrackDbRow],
    ) -> Result<(), BudgetExceeded> {
        for artist in artists {
            self.artist(artist)?;
        }
        for release in releases {
            self.release(release)?;
        }
        for track in tracks {
            self.track(track)?;
        }
        self.output(artists, releases, tracks)
    }

    pub(super) fn output(
        &mut self,
        artists: &[ArtistDbRow],
        releases: &[ReleaseDbRow],
        tracks: &[TrackDbRow],
    ) -> Result<(), BudgetExceeded> {
        for artist in artists {
            self.output_artist(artist)?;
        }
        for release in releases {
            let id = release.artist_mbid.as_deref().unwrap_or("");
            let sort_len = artists
                .iter()
                .rev()
                .find(|artist| artist.mbid == id)
                .map_or_else(
                    || optional_len(&release.artist_name),
                    |artist| artist.sort_name.len(),
                );
            self.output_release(release, sort_len)?;
        }
        for track in tracks {
            self.output_track(track)?;
        }
        Ok(())
    }

    // Each invocation represents one occurrence, including duplicate output.
    // A failed charge leaves the previous total unchanged.
    pub(super) fn row(&mut self, fields: &[usize]) -> Result<(), BudgetExceeded> {
        let bytes = fields
            .iter()
            .try_fold(0usize, |sum, &len| sum.checked_add(len))
            .ok_or(BudgetExceeded)?;
        let total = self.total.checked_add(bytes).ok_or(BudgetExceeded)?;
        if bytes > self.per_row || total > self.total_limit {
            return Err(BudgetExceeded);
        }
        self.total = total;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_and_total_boundaries_include_duplicates() {
        for (size, accepted) in [(3, true), (4, true), (5, false)] {
            let mut budget = SnapshotBytes::new(4, 20);
            assert_eq!(budget.row(&[size]).is_ok(), accepted);
        }
        for (limit, accepted) in [(7, false), (8, true), (9, true)] {
            let mut budget = SnapshotBytes::new(4, limit);
            budget.row(&[1, 3]).unwrap();
            assert_eq!(budget.row(&[1, 3]).is_ok(), accepted);
            assert_eq!(budget.total, if accepted { 8 } else { 4 });
        }
    }

    #[test]
    fn raw_fields_and_duplicate_occurrences_are_counted() {
        let artist = ArtistDbRow {
            mbid: "a".into(),
            name: "bb".into(),
            sort_name: "ccc".into(),
            artist_type: Some("dddd".into()),
            country: Some("eeeee".into()),
        };
        let release = ReleaseDbRow {
            mbid: "a".into(),
            title: "bb".into(),
            release_type: Some("ccc".into()),
            year: None,
            artist_mbid: Some("dddd".into()),
            artist_name: Some("eeeee".into()),
        };
        let track = TrackDbRow {
            release_mbid: "a".into(),
            recording_mbid: "bb".into(),
            title: "ccc".into(),
            duration_ms: None,
            medium_no: None,
            track_no: None,
            release_title: "dddd".into(),
            artist_mbid: Some("eeeee".into()),
            artist_name: Some("ffffff".into()),
        };
        let mut exact = SnapshotBytes::new(21, 72);
        exact.artist(&artist).unwrap();
        exact.release(&release).unwrap();
        exact.track(&track).unwrap();
        exact.track(&track).unwrap();
        assert_eq!(exact.total, 72);
        assert_eq!(exact.artist(&artist), Err(BudgetExceeded));
        assert_eq!(exact.total, 72);
        let mut short = SnapshotBytes::new(20, usize::MAX);
        assert_eq!(short.track(&track), Err(BudgetExceeded));
        assert_eq!(short.total, 0);
    }

    #[test]
    fn output_release_counts_last_matching_artist_sort_name() {
        let artists = [
            ArtistDbRow {
                mbid: "a".into(),
                name: "x".into(),
                sort_name: "s".into(),
                artist_type: None,
                country: None,
            },
            ArtistDbRow {
                mbid: "a".into(),
                name: "x".into(),
                sort_name: "long".into(),
                artist_type: None,
                country: None,
            },
        ];
        let release = ReleaseDbRow {
            mbid: "r".into(),
            title: "t".into(),
            release_type: None,
            year: None,
            artist_mbid: Some("a".into()),
            artist_name: Some("fallback".into()),
        };
        let expected = 4 + 1 + 1 + 1 + 8 + nntune_subsonic::library::NOW.len() + 4;
        let mut exact = SnapshotBytes::new(expected, expected);
        exact.output(&artists, &[release], &[]).unwrap();
        assert_eq!(exact.total, expected);
    }

    #[test]
    fn output_counts_duplicate_tracks_and_both_album_id_copies() {
        let track = TrackDbRow {
            release_mbid: "album".into(),
            recording_mbid: "id".into(),
            title: "song".into(),
            duration_ms: None,
            medium_no: None,
            track_no: None,
            release_title: "title".into(),
            artist_mbid: None,
            artist_name: None,
        };
        let size = 2
            + 4
            + 5
            + 2 * 5
            + crate::subsonic_dispatch::FLAC_CONTENT_TYPE.len()
            + crate::subsonic_dispatch::FLAC_SUFFIX.len()
            + nntune_subsonic::library::NOW.len();
        let mut exact = SnapshotBytes::new(size, 2 * size);
        exact
            .output(&[], &[], &[track.clone(), track.clone()])
            .unwrap();
        assert_eq!(exact.total, 2 * size);
        let mut short = SnapshotBytes::new(size, 2 * size - 1);
        assert_eq!(
            short.output(&[], &[], &[track.clone(), track]),
            Err(BudgetExceeded)
        );
        assert_eq!(short.total, size);
    }

    #[test]
    fn arithmetic_overflow_fails_without_changing_total() {
        let mut budget = SnapshotBytes::new(usize::MAX, usize::MAX);
        budget.row(&[1]).unwrap();
        assert_eq!(budget.row(&[usize::MAX, 1]), Err(BudgetExceeded));
        assert_eq!(budget.row(&[usize::MAX]), Err(BudgetExceeded));
        assert_eq!(budget.total, 1);
    }
}
