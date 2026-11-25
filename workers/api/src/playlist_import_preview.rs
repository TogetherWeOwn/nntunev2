//! In-memory preview of the canonical track ID subset of the NNTune queue M3U export.
//!
//! Uses a supplied synthetic set of canonical track IDs. A match describes identity
//! within that set. Active, owned Library membership for live playlist access and
//! append remains the separate gate specified in ADRs 0002 and 0006.

use std::collections::BTreeSet;

use crate::{library::MAX_ITEMS, meter_sink::is_uuid_shaped};

const MAX_PREVIEW_BYTES: usize = 128 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum PreviewError {
    TooManyBytes,
    InvalidUtf8,
    MissingHeader,
    TooManyRows,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PreviewRow<'a> {
    Matched { track_id: &'a str },
    Unresolved,
    Malformed,
}

/// Accepts a UTF-8 `#EXTM3U` header (optionally preceded by BOM) and the
/// export's two-line `#EXTINF:-1,<label>` / canonical UUID records. Empty
/// lines between records are ignored. The output index is the zero-based
/// position, including repeated, unresolved, and malformed records.
///
/// The set contains lowercase canonical `tracks.id` values. UUID casing in
/// the file does not affect matching. Labels and malformed data are discarded.
/// File-level errors return no partial preview.
pub fn preview<'a>(
    input: &[u8],
    synthetic_track_ids: &BTreeSet<&'a str>,
) -> Result<Vec<PreviewRow<'a>>, PreviewError> {
    if input.len() > MAX_PREVIEW_BYTES {
        return Err(PreviewError::TooManyBytes);
    }
    let input = std::str::from_utf8(input).map_err(|_| PreviewError::InvalidUtf8)?;
    let mut lines = input
        .strip_prefix('\u{feff}')
        .unwrap_or(input)
        .lines()
        .peekable();
    if lines.next() != Some("#EXTM3U") {
        return Err(PreviewError::MissingHeader);
    }

    let mut rows = Vec::new();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }
        if rows.len() == MAX_ITEMS {
            return Err(PreviewError::TooManyRows);
        }
        // An incomplete record must leave the next directive available so a
        // malformed row can't swallow the next complete record.
        let id = if line.starts_with("#EXTINF:") {
            lines.next_if(|next| !next.is_empty() && !next.starts_with('#'))
        } else {
            None
        };
        let valid_label = line.strip_prefix("#EXTINF:-1,").is_some_and(|label| {
            !label.trim().is_empty()
                && !label
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        });
        let row = match id.filter(|id| valid_label && is_uuid_shaped(id)) {
            Some(id) => match synthetic_track_ids.get(id.to_ascii_lowercase().as_str()) {
                Some(&track_id) => PreviewRow::Matched { track_id },
                None => PreviewRow::Unresolved,
            },
            None => PreviewRow::Malformed,
        };
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "a0000000-0000-4000-8000-000000000001";
    const B: &str = "b0000000-0000-4000-8000-000000000002";
    const MISSING: &str = "c0000000-0000-4000-8000-000000000003";

    #[test]
    fn preserves_order_repeats_and_unresolved_positions() {
        let input = format!(
            "\u{feff}#EXTM3U\r\n#EXTINF:-1,Écho — Artist\r\n{B}\r\n\r\n\
             #EXTINF:-1,Same label\r\n{MISSING}\r\n#EXTINF:-1,Same label\r\n{}\r\n\
             #EXTINF:-1,Écho — Artist\r\n{B}",
            A.to_ascii_uppercase()
        );
        assert_eq!(
            preview(input.as_bytes(), &BTreeSet::from([A, B])),
            Ok(vec![
                PreviewRow::Matched { track_id: B },
                PreviewRow::Unresolved,
                PreviewRow::Matched { track_id: A },
                PreviewRow::Matched { track_id: B },
            ])
        );
    }

    #[test]
    fn flags_malformed_records_without_consuming_the_next_record() {
        let input = format!(
            "#EXTM3U\n#EXTINF:-1,A URL\nhttps://example.invalid/track\n\
             #EXTINF:-1,A path\n../track\n#EXTINF:-1,Missing ID\n\
             #EXTINF:8,Unsupported duration\n{A}\n#EXTINF:-1,Bad\0label\n{A}\n\
             {A}\n#EXTINF:-1,Valid\n{A}\n#EXTINF:-1,Missing final ID"
        );
        assert_eq!(
            preview(input.as_bytes(), &BTreeSet::from([A])),
            Ok(vec![
                PreviewRow::Malformed,
                PreviewRow::Malformed,
                PreviewRow::Malformed,
                PreviewRow::Malformed,
                PreviewRow::Malformed,
                PreviewRow::Malformed,
                PreviewRow::Matched { track_id: A },
                PreviewRow::Malformed,
            ])
        );
    }

    #[test]
    fn rejects_invalid_documents_and_bounds_bytes_and_rows() {
        let ids = BTreeSet::from([A]);
        assert_eq!(preview(b"#EXTM3U", &ids), Ok(vec![]));
        assert_eq!(preview(b"", &ids), Err(PreviewError::MissingHeader));
        assert_eq!(
            preview(A.as_bytes(), &ids),
            Err(PreviewError::MissingHeader)
        );
        assert_eq!(
            preview(b"#EXTM3U\n\xff", &ids),
            Err(PreviewError::InvalidUtf8)
        );

        let mut input = format!(
            "#EXTM3U\n{}",
            format!("#EXTINF:-1,Fixture\n{A}\n").repeat(MAX_ITEMS)
        );
        let rows = preview(input.as_bytes(), &ids).unwrap();
        assert_eq!(rows.len(), MAX_ITEMS);
        assert_eq!(rows.last(), Some(&PreviewRow::Matched { track_id: A }));
        input.push_str("malformed extra row");
        assert_eq!(
            preview(input.as_bytes(), &ids),
            Err(PreviewError::TooManyRows)
        );

        let mut input = b"#EXTM3U\n".to_vec();
        input.resize(MAX_PREVIEW_BYTES, b'\n');
        assert_eq!(preview(&input, &ids), Ok(vec![]));
        input.push(b'\xff');
        assert_eq!(preview(&input, &ids), Err(PreviewError::TooManyBytes));
    }
}
