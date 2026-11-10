//! Unmounted, job-local manifest candidate assembly (ADR 0002 §6).
//!
//! This bounded first implementation handles ASCII grouping text only, where
//! lowercase is case-folding. Non-ASCII grouping text refuses the candidate;
//! Unicode lowercasing alone would silently violate the grouping contract.
//! No mirror identity, positions, dedup, art or membership is inferred here.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    audio_manifest::{AcceptedAudioManifest, AudioEntry, AudioHints, ManifestError},
    audio_selection::{AudioSelectionPlan, ExtractedAudio},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupingError {
    Identity,
    UnsupportedText,
    Manifest(ManifestError),
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ArtistKey {
    Mbid(String),
    Text(String),
    Various,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum CandidateKey {
    Mbid(String),
    Title(String),
    Single(u32),
}

fn text(raw: Option<&str>) -> Result<Option<String>, GroupingError> {
    let Some(raw) = raw else { return Ok(None) };
    if raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err(GroupingError::Manifest(ManifestError::Invalid));
    }
    if !raw.is_ascii() {
        return Err(GroupingError::UnsupportedText);
    }
    let normalized = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    Ok((!normalized.is_empty()).then_some(normalized))
}

fn mbid(raw: Option<&str>) -> Result<Option<String>, GroupingError> {
    raw.map(|raw| {
        if crate::queue::validate_mbid(raw) {
            Ok(raw.to_ascii_lowercase())
        } else {
            Err(GroupingError::Manifest(ManifestError::Invalid))
        }
    })
    .transpose()
}

struct GroupHints {
    release: Option<String>,
    artist_id: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    track_artist: Option<String>,
}

impl GroupHints {
    fn read(hints: &AudioHints) -> Result<Self, GroupingError> {
        Ok(Self {
            release: mbid(hints.release_mbid.as_deref())?,
            artist_id: mbid(
                hints
                    .album_artist_mbid
                    .as_deref()
                    .or(hints.artist_mbid.as_deref()),
            )?,
            album: text(hints.album.as_deref())?,
            album_artist: text(hints.album_artist.as_deref())?,
            track_artist: text(hints.track_artist.as_deref())?,
        })
    }
}

fn without_guest(artist: &str) -> &str {
    let end = [" feat. ", " ft. ", " featuring ", " with "]
        .into_iter()
        .filter_map(|suffix| artist.find(suffix))
        .min()
        .unwrap_or(artist.len());
    artist[..end].trim()
}

fn fallback_artist(indexes: &[usize], hints: &[GroupHints]) -> ArtistKey {
    let first = hints[indexes[0]].track_artist.as_deref();
    if let Some(common) = first.filter(|first| {
        indexes
            .iter()
            .all(|&i| hints[i].track_artist.as_deref() == Some(*first))
    }) {
        return ArtistKey::Text(common.to_owned());
    }
    let mut counts = BTreeMap::<&str, usize>::new();
    for &i in indexes {
        if let Some(artist) = hints[i].track_artist.as_deref() {
            let artist = without_guest(artist);
            if !artist.is_empty() {
                *counts.entry(artist).or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .find(|(_, count)| count * 3 >= indexes.len() * 2)
        .map(|(artist, _)| ArtistKey::Text(artist.to_owned()))
        .unwrap_or(ArtistKey::Various)
}

fn artist_text(hints: &GroupHints, fallback: &ArtistKey) -> ArtistKey {
    hints
        .album_artist
        .as_ref()
        .map(|artist| ArtistKey::Text(artist.clone()))
        .unwrap_or_else(|| fallback.clone())
}

/// Build a candidate, not a first-accepted winner. The caller must persist it
/// through the fenced durable store and consume the returned winner before any
/// mirror lookup or KV save. No caller of this seam is mounted in production.
/// Selections must exactly cover the digest-bound plan; grouping cannot reorder,
/// omit or merge audio files. Art extraction/staging and post-match positions
/// remain separate prerequisites before a production producer can be enabled.
pub fn prepare_manifest(
    job_id: &str,
    user_id: &str,
    plan: &AudioSelectionPlan,
    extracted: Vec<ExtractedAudio>,
) -> Result<AcceptedAudioManifest, GroupingError> {
    if extracted.len() != plan.selections().len()
        || extracted
            .iter()
            .zip(plan.selections())
            .any(|(entry, selection)| {
                entry.audio_ordinal != selection.audio_ordinal()
                    || &entry.selector != selection.selector()
            })
    {
        return Err(GroupingError::Identity);
    }
    let hints = extracted
        .iter()
        .map(|entry| GroupHints::read(&entry.hints))
        .collect::<Result<Vec<_>, _>>()?;
    let mut candidates = BTreeMap::<CandidateKey, Vec<usize>>::new();
    for (i, hint) in hints.iter().enumerate() {
        let key = if let Some(release) = &hint.release {
            CandidateKey::Mbid(release.clone())
        } else if let Some(album) = &hint.album {
            CandidateKey::Title(album.clone())
        } else {
            CandidateKey::Single(extracted[i].audio_ordinal)
        };
        candidates.entry(key).or_default().push(i);
    }
    let mut assignments = vec![(ArtistKey::Various, 0); extracted.len()];
    for (candidate, indexes) in candidates {
        let fallback = fallback_artist(&indexes, &hints);
        match candidate {
            CandidateKey::Mbid(_) => {
                let artists = indexes
                    .iter()
                    .map(|&i| {
                        hints[i]
                            .artist_id
                            .as_ref()
                            .map(|id| ArtistKey::Mbid(id.clone()))
                            .unwrap_or_else(|| artist_text(&hints[i], &fallback))
                    })
                    .collect::<BTreeSet<_>>();
                // Missing keys use the text rule, not an invented MBID alias.
                // Conflicts share Various without splitting the release.
                let artist = if artists.len() == 1 {
                    artists.into_iter().next().unwrap()
                } else {
                    ArtistKey::Various
                };
                let release_ref = extracted[indexes[0]].audio_ordinal;
                for i in indexes {
                    assignments[i] = (artist.clone(), release_ref);
                }
            }
            CandidateKey::Title(_) => {
                let mut releases = BTreeMap::new();
                for i in indexes {
                    let artist = artist_text(&hints[i], &fallback);
                    let release_ref = *releases
                        .entry(artist.clone())
                        .or_insert(extracted[i].audio_ordinal);
                    assignments[i] = (artist, release_ref);
                }
            }
            CandidateKey::Single(_) => {
                let i = indexes[0];
                let artist = hints[i]
                    .artist_id
                    .as_ref()
                    .map(|id| ArtistKey::Mbid(id.clone()))
                    .unwrap_or_else(|| artist_text(&hints[i], &fallback));
                assignments[i] = (artist, extracted[i].audio_ordinal);
            }
        }
    }
    let mut artists = BTreeMap::new();
    let entries = extracted
        .into_iter()
        .zip(assignments)
        .map(|(entry, (artist, release_group_ref))| {
            let artist_group_ref = *artists.entry(artist).or_insert(entry.audio_ordinal);
            AudioEntry {
                audio_ordinal: entry.audio_ordinal,
                source_selector: entry.selector,
                artist_group_ref,
                release_group_ref,
                hints: entry.hints,
                embedded_art_selector: None,
            }
        })
        .collect();
    AcceptedAudioManifest::from_entries(job_id, user_id, plan.original_content_sha256(), entries)
        .map_err(GroupingError::Manifest)
}
