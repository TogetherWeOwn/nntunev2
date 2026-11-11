//! Frozen, structurally accepted audio selections (ADR 0002 §6).
//!
//! Preparation only: no tag extraction, grouping algorithm, occurrence matching,
//! membership or SQL writer is enabled here. Claims and group references are
//! job-local hints, never mirror authority. The selector resolves through the
//! retained, job/hash-bound ingest original, not an expiring KV URL. Producers
//! must validate/stage art bytes before recording their descriptors here.

use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Deserializer, Serialize};

use crate::{jobs::validate_job_id, nzb_intake::MAX_FILES};

// Leave room for the rest of the checkpoint within a Workflow step result.
pub const MAX_MANIFEST_BYTES: usize = 512 * 1024;
pub const MAX_AUDIO_ENTRIES: usize = MAX_FILES;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_ART_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestError {
    Invalid,
    TooLarge,
    Identity,
    Frozen,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSelector {
    /// Zero-based original NZB document position, not an audio ordinal.
    pub nzb_file_ordinal: usize,
    /// Exact, relative archive-member path; None for a direct audio file.
    pub archive_member: Option<String>,
    /// Zero-based audio stream/selection in the chosen file or member.
    pub audio_selection_index: u32,
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioHints {
    pub title: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_artist: Option<String>,
    pub artist_mbid: Option<String>,
    pub album_artist_mbid: Option<String>,
    pub release_mbid: Option<String>,
    pub recording_mbid: Option<String>,
    pub disc_no: Option<i32>,
    pub track_no: Option<i32>,
    pub duration_ms: Option<u64>,
    pub fingerprint: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedArtSelector {
    pub tag_container: String,
    pub picture_ordinal: u32,
    pub declared_mime: String,
    pub content_hash: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtBlob {
    pub content_hash: String,
    pub byte_size: u64,
    pub mime: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioEntry {
    pub audio_ordinal: u32,
    pub source_selector: AudioSelector,
    /// Lowest audio ordinal in each independent job-local group.
    pub artist_group_ref: u32,
    pub release_group_ref: u32,
    #[serde(default)]
    pub hints: AudioHints,
    #[serde(default)]
    pub embedded_art_selector: Option<EmbeddedArtSelector>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRecord {
    version: u8,
    job_id: String,
    user_id: String,
    original_content_sha256: String,
    entries: Vec<AudioEntry>,
    art_blobs: Vec<ArtBlob>,
    /// Indexed by audio_ordinal - 1. None means a legacy, not-yet-frozen
    /// manifest; a null element means that entry's effective disc is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effective_discs: Option<Vec<Option<i32>>>,
}

/// Immutable after parsing; intentionally redacted in checkpoint Debug output.
/// Serialize is for private checkpoint storage, never an HTTP response.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct AcceptedAudioManifest(ManifestRecord);

impl fmt::Debug for AcceptedAudioManifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AcceptedAudioManifest([redacted])")
    }
}

impl<'de> Deserialize<'de> for AcceptedAudioManifest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = ManifestRecord::deserialize(deserializer)?;
        Self::accept(record).map_err(|_| serde::de::Error::custom("invalid audio manifest"))
    }
}

impl AcceptedAudioManifest {
    pub(crate) fn from_entries(
        job_id: &str,
        user_id: &str,
        original_content_sha256: &str,
        entries: Vec<AudioEntry>,
    ) -> Result<Self, ManifestError> {
        Self::accept(ManifestRecord {
            version: 1,
            job_id: job_id.to_owned(),
            user_id: user_id.to_owned(),
            original_content_sha256: original_content_sha256.to_owned(),
            effective_discs: Some(derive_effective_discs(&entries)),
            entries,
            art_blobs: Vec::new(),
        })
    }

    /// Private producer/storage seam. Never accept this JSON from a client.
    pub fn parse(raw: &str) -> Result<Self, ManifestError> {
        if raw.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge);
        }
        let record = serde_json::from_str(raw).map_err(|_| ManifestError::Invalid)?;
        Self::accept(record)
    }

    fn accept(record: ManifestRecord) -> Result<Self, ManifestError> {
        record.validate()?;
        let encoded = serde_json::to_vec(&record).map_err(|_| ManifestError::Invalid)?;
        if encoded.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge);
        }
        Ok(Self(record))
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

    pub fn entries(&self) -> &[AudioEntry] {
        &self.0.entries
    }

    pub fn art_blobs(&self) -> &[ArtBlob] {
        &self.0.art_blobs
    }

    /// Frozen job-local disc hints, never mirror occurrence authority. Consumers
    /// must refuse position/dedup work on legacy manifests returning None; do not
    /// regenerate a durable winner from current tags or silently assume disc 1.
    pub fn effective_discs(&self) -> Option<&[Option<i32>]> {
        self.0.effective_discs.as_deref()
    }
}

fn derive_effective_discs(entries: &[AudioEntry]) -> Vec<Option<i32>> {
    let tagged = entries
        .iter()
        .filter(|entry| entry.hints.disc_no.is_some())
        .map(|entry| entry.release_group_ref)
        .collect::<std::collections::BTreeSet<_>>();
    entries
        .iter()
        .map(|entry| {
            if tagged.contains(&entry.release_group_ref) {
                entry.hints.disc_no.filter(|&disc| disc > 0)
            } else {
                Some(1)
            }
        })
        .collect()
}

fn hash(raw: &str) -> bool {
    raw.len() == 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn text(raw: &str) -> bool {
    raw.len() <= MAX_TEXT_BYTES && !raw.chars().any(char::is_control)
}

pub(crate) fn valid_identity(job_id: &str, user_id: &str, original_hash: &str) -> bool {
    validate_job_id(job_id)
        && !user_id.is_empty()
        && user_id.len() <= 128
        && text(user_id)
        && hash(original_hash)
}

fn image_mime(raw: &str) -> bool {
    matches!(raw, "image/jpeg" | "image/png" | "image/webp")
}

fn archive_path(raw: &str) -> bool {
    !raw.is_empty()
        && text(raw)
        && !raw.contains(['\\', ':'])
        && raw.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

impl ManifestRecord {
    fn validate(&self) -> Result<(), ManifestError> {
        if self.version != 1
            || !valid_identity(&self.job_id, &self.user_id, &self.original_content_sha256)
            || self.entries.is_empty()
            || self.entries.len() > MAX_AUDIO_ENTRIES
            || self.art_blobs.len() > self.entries.len()
        {
            return Err(ManifestError::Invalid);
        }
        let mut blobs = BTreeMap::new();
        for blob in &self.art_blobs {
            if !hash(&blob.content_hash)
                || !(1..=MAX_ART_BYTES).contains(&blob.byte_size)
                || !image_mime(&blob.mime)
                || blobs.insert(blob.content_hash.as_str(), blob).is_some()
            {
                return Err(ManifestError::Invalid);
            }
        }
        let mut used_blobs = std::collections::BTreeSet::new();
        let mut artists = BTreeMap::new();
        let mut releases = BTreeMap::new();
        let mut previous = None;
        for (index, entry) in self.entries.iter().enumerate() {
            let ordinal = (index + 1) as u32;
            let source = &entry.source_selector;
            let selection = (
                source.nzb_file_ordinal,
                source.archive_member.as_deref(),
                source.audio_selection_index,
            );
            if entry.audio_ordinal != ordinal
                || source.nzb_file_ordinal >= MAX_FILES
                || source
                    .archive_member
                    .as_deref()
                    .is_some_and(|p| !archive_path(p))
                || previous.is_some_and(|p| p >= selection)
            {
                return Err(ManifestError::Invalid);
            }
            previous = Some(selection);
            // The referenced entry must itself start that group. Separate maps
            // allow one artist to span several release groups, never vice versa.
            if entry.artist_group_ref == ordinal {
                artists.insert(ordinal, ());
            } else if !artists.contains_key(&entry.artist_group_ref) {
                return Err(ManifestError::Invalid);
            }
            if entry.release_group_ref == ordinal {
                releases.insert(ordinal, entry.artist_group_ref);
            } else if releases.get(&entry.release_group_ref) != Some(&entry.artist_group_ref) {
                return Err(ManifestError::Invalid);
            }
            let hints = &entry.hints;
            for value in [
                &hints.title,
                &hints.album,
                &hints.album_artist,
                &hints.track_artist,
                &hints.fingerprint,
            ]
            .into_iter()
            .flatten()
            {
                if !text(value) {
                    return Err(ManifestError::Invalid);
                }
            }
            for mbid in [
                &hints.artist_mbid,
                &hints.album_artist_mbid,
                &hints.release_mbid,
                &hints.recording_mbid,
            ]
            .into_iter()
            .flatten()
            {
                if !crate::queue::validate_mbid(mbid) {
                    return Err(ManifestError::Invalid);
                }
            }
            if let Some(art) = &entry.embedded_art_selector {
                if art.tag_container.is_empty()
                    || !text(&art.tag_container)
                    || !image_mime(&art.declared_mime)
                    || blobs
                        .get(art.content_hash.as_str())
                        .map(|b| b.mime.as_str())
                        != Some(art.declared_mime.as_str())
                {
                    return Err(ManifestError::Invalid);
                }
                used_blobs.insert(art.content_hash.as_str());
            }
        }
        if used_blobs.len() != blobs.len()
            || self
                .effective_discs
                .as_ref()
                .is_some_and(|discs| *discs != derive_effective_discs(&self.entries))
        {
            return Err(ManifestError::Invalid);
        }
        Ok(())
    }
}
