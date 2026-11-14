//! Inert store-facing preference and history row representations.

/// A taste row as returned by a store; its fields are not validated here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasteDbRow {
    pub kind: String,
    pub target_id: String,
    pub signal: String,
    pub snooze_until: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A history row as returned by a store; its fields are not validated here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryDbRow {
    pub play_id: String,
    pub track_id: String,
    pub played_at: i64,
    pub stream_seconds: i64,
}

#[cfg(test)]
mod tests {
    use crate::preference_rows::{HistoryDbRow, TasteDbRow};

    #[test]
    fn taste_row_preserves_fields_and_optional_snooze() {
        let row = TasteDbRow {
            kind: "Album".into(),
            target_id: "Opaque-ID-aA".into(),
            signal: "Snooze".into(),
            snooze_until: Some(-7),
            created_at: -12,
            updated_at: 42,
        };
        assert_eq!(row.kind, "Album");
        assert_eq!(row.target_id, "Opaque-ID-aA");
        assert_eq!(row.signal, "Snooze");
        assert_eq!(row.snooze_until, Some(-7));
        assert_eq!(row.created_at, -12);
        assert_eq!(row.updated_at, 42);
        let mut copy = row.clone();
        assert_eq!(copy, row);
        copy.snooze_until = None;
        assert_eq!(copy.snooze_until, None);
        assert_ne!(copy, row);
        copy.snooze_until = row.snooze_until;
        copy.target_id = "opaque-id-aa".into();
        assert_ne!(copy, row);
        copy.target_id = row.target_id.clone();
        copy.updated_at = row.created_at;
        assert_ne!(copy, row);
    }

    #[test]
    fn history_row_preserves_fields_and_clone_equality() {
        let row = HistoryDbRow {
            play_id: "Play-aA".into(),
            track_id: "Track-Bb".into(),
            played_at: -123,
            stream_seconds: -9,
        };
        assert_eq!(row.play_id, "Play-aA");
        assert_eq!(row.track_id, "Track-Bb");
        assert_eq!(row.played_at, -123);
        assert_eq!(row.stream_seconds, -9);
        let mut copy = row.clone();
        assert_eq!(copy, row);
        copy.play_id = "play-aa".into();
        assert_ne!(copy, row);
        copy.play_id = row.play_id.clone();
        copy.stream_seconds = row.played_at;
        assert_ne!(copy, row);
    }
}
