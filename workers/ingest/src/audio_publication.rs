//! Publication planning from durable classification winners (ADR 0002 §6).
//!
//! Unmounted: no caller, SQL writer, KV publication or membership is enabled
//! here. This seam joins the accepted manifest winner with the classification
//! checkpoint winner and emits only the surviving entries (entries whose
//! `duplicate_of` is `None`), in audio-ordinal order, each bound to its fixed
//! source selector, job-local hints and classified positions. Losing
//! duplicates write no track, source, membership or media row. Hinted MBIDs
//! stay claims, never mirror authority: positions come only from the
//! checkpoint, and a checked-in checkpoint whose entries disagree with the
//! manifest refuses whole rather than guessing. The future normalized
//! publisher must use this plan from the database winners before any SQL or KV
//! publication; KV is never acceptance authority, and classification grants
//! no membership, catalogue or playback authority.

use std::{collections::BTreeMap, fmt};

use crate::{
    audio_classifications::{ClassKind, ClassificationCheckpoint},
    audio_manifest::{AcceptedAudioManifest, AudioHints, AudioSelector},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationError {
    /// The checkpoint is not bound to the manifest job, owner or digest.
    Identity,
    /// Manifest and checkpoint entries disagree in count, ordinal or group.
    Mismatch,
    /// A position, duplicate linkage or survivor conflict is invalid.
    Positions,
    /// The manifest is a legacy unfrozen selection.
    Unfrozen,
}

/// One surviving entry that a future normalized publisher may write, in
/// ordinal order. Losers (`duplicate_of: Some`) never appear here.
/// Selectors and hints stay redacted in Debug output, matching the
/// manifest's redaction policy.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicationEntry {
    pub audio_ordinal: u32,
    pub release_group_ref: u32,
    pub artist_group_ref: u32,
    pub kind: ClassKind,
    pub medium_no: i32,
    pub track_no: i32,
    pub source_selector: AudioSelector,
    pub hints: AudioHints,
}

impl fmt::Debug for PublicationEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PublicationEntry([redacted])")
    }
}

/// Preparation result only, not a durable publication record. Production
/// must load the database winners before planning; rescanning current tags
/// or minting a fresh classification here would not be accepted.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicationPlan(Vec<PublicationEntry>);

impl fmt::Debug for PublicationPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PublicationPlan([redacted])")
    }
}

impl PublicationPlan {
    /// Includes only surviving entries, in audio-ordinal order.
    pub fn entries(&self) -> &[PublicationEntry] {
        &self.0
    }
}

/// Join the accepted manifest winner with the classification checkpoint
/// winner. Total over well-formed winners (the classifications seam
/// guarantees every entry is classified once); any disagreement returns no
/// partial plan. Legacy unfrozen manifests refuse rather than regenerate a
/// position.
pub fn prepare_publication(
    manifest: &AcceptedAudioManifest,
    checkpoint: &ClassificationCheckpoint,
) -> Result<PublicationPlan, PublicationError> {
    if !checkpoint.is_for(manifest.job_id(), manifest.user_id())
        || checkpoint.original_content_sha256() != manifest.original_content_sha256()
    {
        return Err(PublicationError::Identity);
    }
    if manifest.effective_discs().is_none() {
        return Err(PublicationError::Unfrozen);
    }
    let entries = manifest.entries();
    let classified = checkpoint.entries();
    if classified.len() != entries.len() {
        return Err(PublicationError::Mismatch);
    }
    for (entry, row) in entries.iter().zip(classified.iter()) {
        if row.audio_ordinal != entry.audio_ordinal
            || row.release_group_ref != entry.release_group_ref
            || row.medium_no <= 0
            || row.track_no <= 0
        {
            // Positions <= 0 are publication refusals too, but only after the
            // identity binding above so a wrong-owner checkpoint never leaks
            // a position judgement.
            if row.audio_ordinal != entry.audio_ordinal
                || row.release_group_ref != entry.release_group_ref
            {
                return Err(PublicationError::Mismatch);
            }
            return Err(PublicationError::Positions);
        }
    }
    for row in classified {
        let Some(winner) = row.duplicate_of else {
            continue;
        };
        if winner == 0 || winner >= row.audio_ordinal {
            return Err(PublicationError::Positions);
        }
        let winner_row = &classified[(winner - 1) as usize];
        if winner_row.duplicate_of.is_some()
            || winner_row.release_group_ref != row.release_group_ref
            || (winner_row.medium_no, winner_row.track_no) != (row.medium_no, row.track_no)
        {
            return Err(PublicationError::Positions);
        }
    }
    let mut survivors = BTreeMap::<(u32, i32, i32), u32>::new();
    for row in classified {
        if row.duplicate_of.is_some() {
            continue;
        }
        if survivors
            .insert(
                (row.release_group_ref, row.medium_no, row.track_no),
                row.audio_ordinal,
            )
            .is_some()
        {
            return Err(PublicationError::Positions);
        }
    }
    Ok(PublicationPlan(
        entries
            .iter()
            .zip(classified.iter())
            .filter(|(_, row)| row.duplicate_of.is_none())
            .map(|(entry, row)| PublicationEntry {
                audio_ordinal: row.audio_ordinal,
                release_group_ref: row.release_group_ref,
                artist_group_ref: entry.artist_group_ref,
                kind: row.kind,
                medium_no: row.medium_no,
                track_no: row.track_no,
                source_selector: entry.source_selector.clone(),
                hints: entry.hints.clone(),
            })
            .collect(),
    ))
}
