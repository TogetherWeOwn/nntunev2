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

use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    audio_manifest::{valid_identity, AcceptedAudioManifest, MAX_AUDIO_ENTRIES},
    audio_positions::{prepare_all_local_occurrences, PositionError},
    queue::validate_mbid,
};

const CHECKPOINT_VERSION: u8 = 1;
// Fits MAX_AUDIO_ENTRIES even with maximal position values and JSONB spacing.
const MAX_CHECKPOINT_BYTES: usize = 1024 * 1024;

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
struct ClassificationRecord {
    version: u8,
    job_id: String,
    user_id: String,
    original_content_sha256: String,
    entries: Vec<ClassifiedEntry>,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ClassificationCheckpoint(ClassificationRecord);

impl<'de> Deserialize<'de> for ClassificationCheckpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = ClassificationRecord::deserialize(deserializer)?;
        if record.version != CHECKPOINT_VERSION
            || !valid_identity(
                &record.job_id,
                &record.user_id,
                &record.original_content_sha256,
            )
            || record.entries.is_empty()
            || record.entries.len() > MAX_AUDIO_ENTRIES
        {
            return Err(serde::de::Error::custom(
                "invalid classification checkpoint",
            ));
        }
        let mut group_kinds = BTreeMap::new();
        for row in &record.entries {
            if *group_kinds.entry(row.release_group_ref).or_insert(row.kind) != row.kind {
                return Err(serde::de::Error::custom("mixed classification group"));
            }
        }
        Ok(Self(record))
    }
}

impl fmt::Debug for ClassificationCheckpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClassificationCheckpoint([redacted])")
    }
}

impl ClassificationCheckpoint {
    /// Private storage seam. Never accept this JSON from a client.
    pub fn parse(raw: &str) -> Result<Self, ClassError> {
        if raw.len() > MAX_CHECKPOINT_BYTES {
            return Err(ClassError::Evidence);
        }
        serde_json::from_str(raw).map_err(|_| ClassError::Evidence)
    }

    pub fn is_for(&self, job_id: &str, user_id: &str) -> bool {
        self.0.job_id == job_id && self.0.user_id == user_id
    }

    pub fn job_id(&self) -> &str {
        &self.0.job_id
    }

    pub fn user_id(&self) -> &str {
        &self.0.user_id
    }

    pub fn original_content_sha256(&self) -> &str {
        &self.0.original_content_sha256
    }

    pub fn entries(&self) -> &[ClassifiedEntry] {
        &self.0.entries
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
        // One frozen release group must resolve from one mirror snapshot.
        let first = evidence[indexes[0]].as_ref();
        for &i in &indexes[1..] {
            match (first, evidence[i].as_ref()) {
                (Some(a), Some(b))
                    if !a.release_mbid.eq_ignore_ascii_case(&b.release_mbid)
                        || a.snapshot_sha256 != b.snapshot_sha256 =>
                {
                    return Err(ClassError::Evidence);
                }
                (Some(_), Some(_)) | (None, None) => {}
                _ => return Err(ClassError::MixedRelease),
            }
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
    let mut verified_winners = BTreeMap::<(u32, i32, i32), (&str, u32)>::new();
    for (i, entry) in entries.iter().enumerate() {
        if let Some(ev) = &evidence[i] {
            let key = (entry.release_group_ref, ev.medium_no, ev.track_no);
            let duplicate_of = match verified_winners.get(&key) {
                Some(&(recording, winner))
                    if recording.eq_ignore_ascii_case(&ev.recording_mbid) =>
                {
                    Some(winner)
                }
                Some(_) => return Err(ClassError::Evidence),
                None => {
                    verified_winners.insert(key, (&ev.recording_mbid, entry.audio_ordinal));
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
    Ok(ClassificationCheckpoint(ClassificationRecord {
        version: CHECKPOINT_VERSION,
        job_id: manifest.job_id().to_string(),
        user_id: manifest.user_id().to_string(),
        original_content_sha256: manifest.original_content_sha256().to_string(),
        entries: classified,
    }))
}
