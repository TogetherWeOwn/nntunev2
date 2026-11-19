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
    fn arithmetic_overflow_fails_without_changing_total() {
        let mut budget = SnapshotBytes::new(usize::MAX, usize::MAX);
        budget.row(&[1]).unwrap();
        assert_eq!(budget.row(&[usize::MAX, 1]), Err(BudgetExceeded));
        assert_eq!(budget.row(&[usize::MAX]), Err(BudgetExceeded));
        assert_eq!(budget.total, 1);
    }
}
