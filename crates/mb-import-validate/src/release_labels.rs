//! Offline release-to-label joins over the two curated CC0 label sidecars.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{decode_copy_field, is_timestamp, is_uuid, reason, FieldKind};

/// Uncompressed byte limit per artifact; not intended for full dumps.
pub const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct Label {
    pub mbid: String,
    pub name: String,
    pub code: Option<i32>,
    pub type_name: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReleaseLabel {
    /// Shared across edges so a label's text is not copied per catalogue number.
    pub label: Arc<Label>,
    pub catalogue_number: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Artifact {
    Labels,
    ReleaseLabels,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    InvalidReleaseMbid,
    TooLarge(Artifact),
    InvalidUtf8(Artifact),
    MissingFinalLf(Artifact),
    InvalidRow(Artifact, usize, &'static str),
}

/// Join `mbdump-release_label.tsv` to `mbdump-label.tsv` by exact canonicalized
/// MBID. Validate both artifacts in full even if the release has no edges.
/// Results sort by lowercase label MBID and literal decoded catalogue number
/// (which may be empty). No edges means no match in this artifact pair.
pub fn lookup(
    labels: &[u8],
    release_labels: &[u8],
    release_mbid: &str,
) -> Result<Vec<ReleaseLabel>, ReadError> {
    if release_mbid.len() != 36 || !is_uuid(release_mbid) {
        return Err(ReadError::InvalidReleaseMbid);
    }
    let mut by_mbid = BTreeMap::new();
    scan::<5>(labels, Artifact::Labels, |[mbid, name, code, kind, ts]| {
        let mbid = FieldKind::Uuid.check(mbid, true)?;
        let name = required_text(name)?;
        let code = optional(code, |token| {
            let text = decode_copy_field(token)?;
            let number = text.parse::<i32>().map_err(|_| reason::BAD_INT)?;
            (text == number.to_string())
                .then_some(number)
                .ok_or(reason::BAD_INT)
        })?;
        let type_name = optional(kind, short_text)?;
        timestamp(ts)?;
        let label = Label {
            mbid: mbid.clone(),
            name,
            code,
            type_name,
        };
        if by_mbid.insert(mbid, Arc::new(label)).is_some() {
            return Err("duplicate_label_mbid");
        }
        Ok(())
    })?;

    let mut edges = BTreeMap::new();
    scan::<4>(
        release_labels,
        Artifact::ReleaseLabels,
        |[release, label, number, ts]| {
            let release = FieldKind::Uuid.check(release, true)?;
            let label = FieldKind::Uuid.check(label, true)?;
            let number = short_text(number)?;
            timestamp(ts)?;
            let resolved = by_mbid.get(&label).ok_or("missing_label_mbid")?;
            if edges
                .insert((release, resolved.mbid.as_str(), number), resolved)
                .is_some()
            {
                return Err("duplicate_release_label_key");
            }
            Ok(())
        },
    )?;

    let release_mbid = release_mbid.to_ascii_lowercase();
    Ok(edges
        .into_iter()
        .filter(|((release, _, _), _)| release == &release_mbid)
        .map(|((_, _, catalogue_number), label)| ReleaseLabel {
            label: Arc::clone(label),
            catalogue_number,
        })
        .collect())
}

fn scan<const N: usize>(
    bytes: &[u8],
    artifact: Artifact,
    mut visit: impl FnMut([&str; N]) -> Result<(), &'static str>,
) -> Result<(), ReadError> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ReadError::TooLarge(artifact));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::InvalidUtf8(artifact))?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(ReadError::MissingFinalLf(artifact));
    }
    for (index, row) in text.split_terminator('\n').enumerate() {
        let invalid = |reason| ReadError::InvalidRow(artifact, index + 1, reason);
        let columns: Vec<_> = row.splitn(N + 1, '\t').collect();
        visit(
            columns
                .try_into()
                .map_err(|_| invalid(reason::COLUMN_COUNT))?,
        )
        .map_err(invalid)?;
    }
    Ok(())
}

fn required_text(token: &str) -> Result<String, &'static str> {
    if token == "\\N" {
        return Err(reason::NULL_IN_NOT_NULL);
    }
    decode_copy_field(token)
}

fn short_text(token: &str) -> Result<String, &'static str> {
    let text = required_text(token)?;
    if text.chars().count() > 255 {
        return Err("text_too_long");
    }
    Ok(text)
}

fn optional<T>(
    token: &str,
    parse: impl FnOnce(&str) -> Result<T, &'static str>,
) -> Result<Option<T>, &'static str> {
    (token != "\\N").then(|| parse(token)).transpose()
}

fn timestamp(token: &str) -> Result<(), &'static str> {
    token
        .strip_suffix('Z')
        .is_some_and(|utc| {
            matches!(utc.len(), 19 | 26)
                && utc.bytes().enumerate().all(|(i, b)| match i {
                    4 | 7 => b == b'-',
                    10 => b == b'T',
                    13 | 16 => b == b':',
                    17 => matches!(b, b'0'..=b'5'),
                    19 => b == b'.',
                    _ => b.is_ascii_digit(),
                })
                && is_timestamp(token)
        })
        .then_some(())
        .ok_or(reason::BAD_TIMESTAMP)
}
