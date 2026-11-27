//! Offline reads from curated CC0 directed relationship sidecars.

use std::collections::BTreeSet;

use super::{decode_required, is_curated_timestamp, is_uuid, reason, FieldKind};

/// Intended for curated slices, not full dumps or compressed input.
pub const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;

/// The caller must select the actual sidecar: its rows do not encode entity kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Projection {
    ArtistRecording,
    ArtistRelease,
    ArtistWork,
    RecordingWork,
    RecordingRecording,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetMbid {
    Recording(String),
    Release(String),
    Work(String),
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Relationship {
    pub target: TargetMbid,
    pub link_type: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    InvalidSourceMbid,
    TooLarge,
    InvalidUtf8,
    MissingFinalLf,
    InvalidRow { line: usize, reason: &'static str },
}

/// Validate every row before returning outgoing edges ordered by lowercase
/// target MBID, then decoded link type. An empty vector means no matching edges.
/// The three-column music-video projection is not a relationship sidecar.
pub fn lookup(
    projection: Projection,
    bytes: &[u8],
    source_mbid: &str,
) -> Result<Vec<Relationship>, ReadError> {
    if source_mbid.len() != 36 || !is_uuid(source_mbid) {
        return Err(ReadError::InvalidSourceMbid);
    }
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ReadError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::InvalidUtf8)?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(ReadError::MissingFinalLf);
    }

    let mut edges = BTreeSet::new();
    for (index, row) in text.split_terminator('\n').enumerate() {
        let invalid = |reason| ReadError::InvalidRow {
            line: index + 1,
            reason,
        };
        let columns: Vec<_> = row.splitn(5, '\t').collect();
        let &[source, target, link_type, timestamp] = columns.as_slice() else {
            return Err(invalid(reason::COLUMN_COUNT));
        };
        let source = FieldKind::Uuid.check(source, true).map_err(invalid)?;
        let target = FieldKind::Uuid.check(target, true).map_err(invalid)?;
        let target = match projection {
            Projection::ArtistRecording | Projection::RecordingRecording => {
                TargetMbid::Recording(target)
            }
            Projection::ArtistRelease => TargetMbid::Release(target),
            Projection::ArtistWork | Projection::RecordingWork => TargetMbid::Work(target),
        };
        let link_type = decode_required(link_type).map_err(invalid)?;
        // Python's str.strip() also treats these four separators as whitespace.
        if link_type
            .chars()
            .all(|c| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'))
            || (projection == Projection::RecordingRecording && link_type.chars().count() > 255)
        {
            return Err(invalid("invalid_link_type"));
        }
        let timestamp = decode_required(timestamp).map_err(invalid)?;
        if !is_curated_timestamp(&timestamp) {
            return Err(invalid(reason::BAD_TIMESTAMP));
        }
        if !edges.insert((source, Relationship { target, link_type })) {
            return Err(invalid("duplicate_relationship"));
        }
    }
    Ok(edges
        .into_iter()
        .filter_map(|(source, edge)| source.eq_ignore_ascii_case(source_mbid).then_some(edge))
        .collect())
}
