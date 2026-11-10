//! Mirror-evidence retrieval for classification (ADR 0002 §6).
//!
//! Unmounted: no caller, transport or publisher is enabled here. For each
//! accepted manifest entry carrying both a recording-MBID and a
//! release-MBID hint, this seam binds the hinted recording to one exact
//! mirror snapshot — the release-recordings projection whose SHA-256 digest
//! the snapshot carries — and returns digest-bound [`MirrorEvidence`] the
//! classification seam can consume. An entry without both hints, or whose
//! recording is absent from the hinted release's snapshot, yields `None`
//! (all-local); the classifier, not this seam, decides positions. An
//! invalid snapshot (malformed MBIDs, non-positive positions, conflicting
//! duplicate recordings, digest mismatch, release mismatch) refuses the
//! whole derivation rather than guessing. Hinted MBIDs are claims, never
//! proof: only presence in the digest-bound snapshot counts. This seam
//! grants no membership, catalogue, KV or playback authority.

use sha2::{Digest, Sha256};

use crate::{
    audio_classifications::MirrorEvidence, audio_manifest::AcceptedAudioManifest,
    queue::validate_mbid,
};

/// One verified mirror occurrence row: a recording's position within one
/// release of one mirror snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorRecording {
    pub recording_mbid: String,
    pub medium_no: i32,
    pub track_no: i32,
}

/// One exact mirror snapshot: the release-recordings projection of one
/// release, plus the hex SHA-256 digest of that projection. The digest is
/// verified against [`mirror_snapshot_digest`] before any evidence is
/// derived; a caller-supplied digest that does not match refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorSnapshot {
    pub release_mbid: String,
    pub snapshot_sha256: String,
    pub recordings: Vec<MirrorRecording>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceError {
    /// A snapshot carries a malformed MBID or non-positive position.
    Identity,
    /// A snapshot conflicts with itself or its release hint (duplicate
    /// recording with conflicting positions, or release mismatch).
    Snapshot,
    /// The supplied digest does not match the snapshot projection.
    Digest,
}

/// Canonical hex SHA-256 of one snapshot projection: the lowercased
/// release MBID, then one `recording|medium|track` line per recording
/// (lowercased MBIDs, sorted), joined by newlines under a version tag.
pub fn mirror_snapshot_digest(release_mbid: &str, recordings: &[MirrorRecording]) -> String {
    let mut lines: Vec<String> = recordings
        .iter()
        .map(|r| {
            format!(
                "{}|{}|{}",
                r.recording_mbid.to_ascii_lowercase(),
                r.medium_no,
                r.track_no
            )
        })
        .collect();
    lines.sort();
    let mut canonical = format!(
        "nntune-mirror-snapshot-v1\n{}\n",
        release_mbid.to_ascii_lowercase()
    );
    canonical.push_str(&lines.join("\n"));
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hex::encode(hasher.finalize())
}

fn validate_snapshot(snapshot: &MirrorSnapshot) -> Result<(), EvidenceError> {
    if !validate_mbid(&snapshot.release_mbid) {
        return Err(EvidenceError::Identity);
    }
    if mirror_snapshot_digest(&snapshot.release_mbid, &snapshot.recordings)
        != snapshot.snapshot_sha256
    {
        return Err(EvidenceError::Digest);
    }
    let mut seen = std::collections::BTreeMap::<String, (i32, i32)>::new();
    for recording in &snapshot.recordings {
        if !validate_mbid(&recording.recording_mbid)
            || recording.medium_no <= 0
            || recording.track_no <= 0
        {
            return Err(EvidenceError::Identity);
        }
        let key = recording.recording_mbid.to_ascii_lowercase();
        let at = (recording.medium_no, recording.track_no);
        match seen.get(&key) {
            Some(&other) if other != at => return Err(EvidenceError::Snapshot),
            _ => {
                seen.insert(key, at);
            }
        }
    }
    Ok(())
}

/// Derive one evidence slot per accepted entry, indexed by
/// `audio_ordinal - 1` and aligned with `classify_entries`. `snapshot_for`
/// returns the exact mirror snapshot for a hinted release MBID, or `None`
/// when the mirror has no snapshot for it. Any refusal returns no partial
/// vector.
pub fn derive_evidence<F>(
    manifest: &AcceptedAudioManifest,
    snapshot_for: F,
) -> Result<Vec<Option<MirrorEvidence>>, EvidenceError>
where
    F: Fn(&str) -> Option<MirrorSnapshot>,
{
    let mut slots: Vec<Option<MirrorEvidence>> = Vec::with_capacity(manifest.entries().len());
    for entry in manifest.entries() {
        let hints = &entry.hints;
        let (Some(recording_mbid), Some(release_mbid)) = (
            hints.recording_mbid.as_deref(),
            hints.release_mbid.as_deref(),
        ) else {
            slots.push(None);
            continue;
        };
        let Some(snapshot) = snapshot_for(release_mbid) else {
            slots.push(None);
            continue;
        };
        validate_snapshot(&snapshot)?;
        if !snapshot.release_mbid.eq_ignore_ascii_case(release_mbid) {
            return Err(EvidenceError::Snapshot);
        }
        let position = snapshot
            .recordings
            .iter()
            .find(|r| r.recording_mbid.eq_ignore_ascii_case(recording_mbid))
            .map(|r| (r.medium_no, r.track_no));
        slots.push(position.map(|(medium_no, track_no)| MirrorEvidence {
            recording_mbid: recording_mbid.to_owned(),
            release_mbid: snapshot.release_mbid.to_ascii_lowercase(),
            snapshot_sha256: snapshot.snapshot_sha256.clone(),
            medium_no,
            track_no,
        }));
    }
    Ok(slots)
}
