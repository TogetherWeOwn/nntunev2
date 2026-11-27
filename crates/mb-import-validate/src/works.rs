//! Offline reads from the curated `mbdump-work.tsv` sidecar.

use std::collections::BTreeSet;

use super::{decode_copy_field, decode_required, is_curated_timestamp, is_uuid, reason, FieldKind};

/// Limit for an uncompressed fixture slice, not a full dump.
pub const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct WorkMbid(String);

impl WorkMbid {
    /// The validated UUID in lowercase canonical form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Work {
    pub mbid: WorkMbid,
    pub title: String,
    pub work_type: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    InvalidWorkMbid,
    TooLarge,
    InvalidUtf8,
    MissingFinalLf,
    InvalidRow { line: usize, reason: &'static str },
}

/// Look up a case-insensitive work MBID. `None` means no match in this
/// artifact, and is returned only after validating every row, as is a hit.
pub fn lookup(bytes: &[u8], work_mbid: &str) -> Result<Option<Work>, ReadError> {
    if work_mbid.len() != 36 || !is_uuid(work_mbid) {
        return Err(ReadError::InvalidWorkMbid);
    }
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ReadError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::InvalidUtf8)?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(ReadError::MissingFinalLf);
    }

    let mut seen = BTreeSet::new();
    let mut found = None;
    for (index, row) in text.split_terminator('\n').enumerate() {
        let invalid = |reason| ReadError::InvalidRow {
            line: index + 1,
            reason,
        };
        let mut fields = row.split('\t');
        let (Some(mbid), Some(title), Some(work_type), Some(timestamp), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err(invalid(reason::COLUMN_COUNT));
        };
        let mbid = FieldKind::Uuid.check(mbid, true).map_err(invalid)?;
        let title = decode_required(title).map_err(invalid)?;
        let work_type = (work_type != r"\N")
            .then(|| decode_copy_field(work_type))
            .transpose()
            .map_err(invalid)?;
        let timestamp = decode_required(timestamp).map_err(invalid)?;
        if !is_curated_timestamp(&timestamp) {
            return Err(invalid(reason::BAD_TIMESTAMP));
        }
        if mbid.eq_ignore_ascii_case(work_mbid) {
            found = Some(Work {
                mbid: WorkMbid(mbid.clone()),
                title,
                work_type,
            });
        }
        if !seen.insert(mbid) {
            return Err(invalid("duplicate_work_mbid"));
        }
    }
    Ok(found)
}
