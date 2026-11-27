//! Offline reads from the curated `mbdump-release_credit.tsv` sidecar.

use std::collections::BTreeSet;

use super::{decode_required, is_curated_timestamp, is_uuid, reason, FieldKind};

/// Intended for curated fixture slices, not full dumps or compressed input.
pub const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct ReleaseCredit {
    pub position: i32,
    pub artist_mbid: String,
    pub credited_name: String,
    pub join_phrase: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    InvalidReleaseMbid,
    TooLarge,
    InvalidUtf8,
    MissingFinalLf,
    InvalidRow { line: usize, reason: &'static str },
}

/// Validate the entire bounded, uncompressed artifact and return the requested
/// release's credits in position order. MBIDs are case-insensitive; an empty
/// vector means no matching credits in this artifact, not a missing release.
pub fn lookup(bytes: &[u8], release_mbid: &str) -> Result<Vec<ReleaseCredit>, ReadError> {
    if release_mbid.len() != 36 || !is_uuid(release_mbid) {
        return Err(ReadError::InvalidReleaseMbid);
    }
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ReadError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::InvalidUtf8)?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(ReadError::MissingFinalLf);
    }

    let mut seen = BTreeSet::new();
    let mut credits = Vec::new();
    for (index, row) in text.split_terminator('\n').enumerate() {
        let invalid = |reason| ReadError::InvalidRow {
            line: index + 1,
            reason,
        };
        let columns: Vec<_> = row.splitn(7, '\t').collect();
        let &[release, position, artist, name, join, timestamp] = columns.as_slice() else {
            return Err(invalid(reason::COLUMN_COUNT));
        };
        let release = FieldKind::Uuid.check(release, true).map_err(invalid)?;
        let position_text = decode_required(position).map_err(invalid)?;
        let position = position_text
            .parse::<i32>()
            .ok()
            .filter(|&n| n >= 0 && position_text == n.to_string())
            .ok_or_else(|| invalid("invalid_credit_position"))?;
        let credit = ReleaseCredit {
            position,
            artist_mbid: FieldKind::Uuid.check(artist, true).map_err(invalid)?,
            credited_name: decode_required(name).map_err(invalid)?,
            join_phrase: decode_required(join).map_err(invalid)?,
        };
        let timestamp = decode_required(timestamp).map_err(invalid)?;
        if !is_curated_timestamp(&timestamp) {
            return Err(invalid(reason::BAD_TIMESTAMP));
        }
        let matches = release.eq_ignore_ascii_case(release_mbid);
        if !seen.insert((release, position)) {
            return Err(invalid("duplicate_credit_position"));
        }
        if matches {
            credits.push(credit);
        }
    }
    credits.sort_unstable_by_key(|credit| credit.position);
    Ok(credits)
}
