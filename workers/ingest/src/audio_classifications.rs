//! Durable classification checkpoint preparation (ADR 0002 §6).
//!
//! Unmounted: no caller, SQL writer or KV publication is enabled here.
//! A checkpoint classifies every accepted entry exactly once, either as a
//! verified mirror occurrence (bound to the mirror snapshot digest that
//! produced the evidence) or as all-local (positions from the local planner).
//! Verified and all-local entries never share one release group: mixing is
//! refused whole rather than guessed. Consumers must still refuse legacy
//! unfrozen manifests; this seam never upgrades a durable winner in place.

use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};

use crate::{
    audio_manifest::AcceptedAudioManifest,
    audio_positions::{prepare_all_local_occurrences, PositionError},
    queue::validate_mbid,
};

const CHECKPOINT_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassError {
    Identity,
    Evidence,
    MixedRelease,
    Positions(PositionError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorEvidence {
    pub recording_mbid: String,
    pub release_mbid: String,
    /// Hex SHA-256 of the exact mirror snapshot (release-recordings
    /// projection) the verified occurrence was derived from.
    pub snapshot_sha256: String,
    pub medium_no: i32,
    pub track_no: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassKind {
    AllLocal,
    Verified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifiedEntry {
    pub audio_ordinal: u32,
    pub release_group_ref: u32,
    pub kind: ClassKind,
    pub medium_no: i32,
    pub track_no: i32,
    /// A direct reference to a surviving, lower ordinal, never another loser.
    pub duplicate_of: Option<u32>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassificationCheckpoint {
    version: u8,
    job_id: String,
    user_id: String,
    original_content_sha256: String,
    entries: Vec<ClassifiedEntry>,
}

impl fmt::Debug for ClassificationCheckpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClassificationCheckpoint([redacted])")
    }
}

impl ClassificationCheckpoint {
    pub fn entries(&self) -> &[ClassifiedEntry] {
        &self.entries
    }
}

fn sha256_hex(raw: &str) -> bool {
    raw.len() == 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_evidence(evidence: &MirrorEvidence, entry_hint_release: Option<&str>) -> bool {
    validate_mbid(&evidence.recording_mbid)
        && validate_mbid(&evidence.release_mbid)
        && sha256_hex(&evidence.snapshot_sha256)
        && evidence.medium_no > 0
        && evidence.track_no > 0
        && entry_hint_release.is_none_or(|hint| hint.eq_ignore_ascii_case(&evidence.release_mbid))
}

/// Classify every accepted entry once. `evidence` is indexed by
/// audio_ordinal - 1; `None` means no verified mirror occurrence was
/// produced for that entry. A verified entry's positions come only from
/// its evidence; an all-local entry's positions come only from the local
/// planner for its release group. Any refusal returns no partial checkpoint.
pub fn classify_entries(
    manifest: &AcceptedAudioManifest,
    evidence: &[Option<MirrorEvidence>],
) -> Result<ClassificationCheckpoint, ClassError> {
    let entries = manifest.entries();
    if evidence.len() != entries.len() {
        return Err(ClassError::Evidence);
    }
    if manifest.effective_discs().is_none() {
        return Err(ClassError::Positions(PositionError::UnfrozenDiscs));
    }
    for (entry, slot) in entries.iter().zip(evidence.iter()) {
        if slot
            .as_ref()
            .is_some_and(|ev| !valid_evidence(ev, entry.hints.release_mbid.as_deref()))
        {
            return Err(ClassError::Evidence);
        }
    }
    let mut groups = BTreeMap::<u32, Vec<usize>>::new();
    for (i, entry) in entries.iter().enumerate() {
        groups.entry(entry.release_group_ref).or_default().push(i);
    }
    for indexes in groups.values() {
        let verified = indexes.iter().filter(|&&i| evidence[i].is_some()).count();
        if verified != 0 && verified != indexes.len() {
            return Err(ClassError::MixedRelease);
        }
    }
    let needs_local = groups
        .values()
        .any(|indexes| evidence[indexes[0]].is_none());
    let local = needs_local
        .then(|| prepare_all_local_occurrences(manifest))
        .transpose()
        .map_err(ClassError::Positions)?;
    let mut classified: Vec<ClassifiedEntry> = Vec::with_capacity(entries.len());
    let mut verified_winners = BTreeMap::<(String, i32, i32), u32>::new();
    for (i, entry) in entries.iter().enumerate() {
        if let Some(ev) = &evidence[i] {
            let key = (
                ev.recording_mbid.to_ascii_lowercase(),
                ev.medium_no,
                ev.track_no,
            );
            let duplicate_of = match verified_winners.get(&key) {
                Some(&winner) => Some(winner),
                None => {
                    verified_winners.insert(key, entry.audio_ordinal);
                    None
                }
            };
            classified.push(ClassifiedEntry {
                audio_ordinal: entry.audio_ordinal,
                release_group_ref: entry.release_group_ref,
                kind: ClassKind::Verified,
                medium_no: ev.medium_no,
                track_no: ev.track_no,
                duplicate_of,
            });
        } else {
            let occurrence = &local
                .as_ref()
                .expect("all-local group implies planner ran")
                .entries()[i];
            classified.push(ClassifiedEntry {
                audio_ordinal: occurrence.audio_ordinal,
                release_group_ref: occurrence.release_group_ref,
                kind: ClassKind::AllLocal,
                medium_no: occurrence.medium_no,
                track_no: occurrence.track_no,
                duplicate_of: occurrence.duplicate_of,
            });
        }
    }
    Ok(ClassificationCheckpoint {
        version: CHECKPOINT_VERSION,
        job_id: manifest.job_id().to_string(),
        user_id: manifest.user_id().to_string(),
        original_content_sha256: manifest.original_content_sha256().to_string(),
        entries: classified,
    })
}
