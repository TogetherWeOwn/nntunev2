//! Exact offline reads from curated `mbdump-<entity>_alias.tsv` sidecars.

use std::collections::BTreeSet;

use super::{decode_required, is_curated_timestamp, reason, FieldKind};

/// Size limit for an uncompressed fixture slice, not a full dump.
pub const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Artist,
    Recording,
    Release,
    ReleaseGroup,
    Work,
    Label,
}

#[derive(Debug, PartialEq, Eq)]
pub struct EntityMbid {
    pub kind: EntityKind,
    /// Validated UUID in lowercase canonical form.
    pub mbid: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct AliasMatch {
    pub entity: EntityMbid,
    pub credited_name: String,
    pub locale: Option<String>,
    pub sort_name: String,
    pub primary_for_locale: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    InvalidQuery,
    TooLarge,
    InvalidUtf8,
    MissingFinalLf,
    InvalidRow { line: usize, reason: &'static str },
}

/// Look up literal decoded text, with no case, Unicode, or locale folding.
/// The caller supplies the kind of the sidecar (its rows do not encode it).
/// Empty locale fields mean `None`; an empty result means no match in this
/// artifact. Validate every row before returning results in MBID then locale
/// order (`None` first). This order is not a relevance ranking.
pub fn lookup(bytes: &[u8], kind: EntityKind, query: &str) -> Result<Vec<AliasMatch>, ReadError> {
    if query.len() > MAX_ARTIFACT_BYTES || query.contains('\0') {
        return Err(ReadError::InvalidQuery);
    }
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ReadError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ReadError::InvalidUtf8)?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(ReadError::MissingFinalLf);
    }

    let mut seen = BTreeSet::new();
    let mut matches = Vec::new();
    for (index, row) in text.split_terminator('\n').enumerate() {
        let invalid = |reason| ReadError::InvalidRow {
            line: index + 1,
            reason,
        };
        let columns: Vec<_> = row.splitn(7, '\t').collect();
        let &[mbid, name, locale, sort_name, primary, timestamp] = columns.as_slice() else {
            return Err(invalid(reason::COLUMN_COUNT));
        };
        let mbid = FieldKind::Uuid.check(mbid, true).map_err(invalid)?;
        let name = decode_required(name).map_err(invalid)?;
        let locale = (!locale.is_empty())
            .then(|| decode_required(locale))
            .transpose()
            .map_err(invalid)?;
        let sort_name = decode_required(sort_name).map_err(invalid)?;
        // Report poison escapes before enforcing the producer's literal spellings.
        decode_required(primary).map_err(invalid)?;
        let primary_for_locale = match primary {
            "t" => true,
            "f" => false,
            _ => return Err(invalid(reason::BAD_BOOL)),
        };
        decode_required(timestamp).map_err(invalid)?;
        if !is_curated_timestamp(timestamp) {
            return Err(invalid(reason::BAD_TIMESTAMP));
        }
        if name == query {
            matches.push(AliasMatch {
                entity: EntityMbid {
                    kind,
                    mbid: mbid.clone(),
                },
                credited_name: name.clone(),
                locale: locale.clone(),
                sort_name,
                primary_for_locale,
            });
        }
        if !seen.insert((mbid, name, locale)) {
            return Err(invalid("duplicate_alias_key"));
        }
    }
    matches.sort_unstable_by(|a, b| (&a.entity.mbid, &a.locale).cmp(&(&b.entity.mbid, &b.locale)));
    Ok(matches)
}
