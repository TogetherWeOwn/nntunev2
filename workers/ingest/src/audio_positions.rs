//! Unmounted all-local occurrence preparation (ADR 0002 §6).
//!
//! Local identity agreement is only a same-job dedup hint. It never verifies a
//! mirror occurrence, promotes a claimed MBID or grants catalogue membership.

use std::{collections::BTreeMap, fmt};

use crate::{
    audio_grouping::{text, GroupingError},
    audio_manifest::{AcceptedAudioManifest, AudioHints},
};

/// Bound adversarial same-pair identity comparisons across the entire job.
pub const MAX_IDENTITY_COMPARISONS: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionError {
    UnfrozenDiscs,
    UnsupportedText,
    InvalidHints,
    WorkLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalOccurrence {
    pub audio_ordinal: u32,
    pub release_group_ref: u32,
    pub medium_no: i32,
    pub track_no: i32,
    /// A direct reference to a surviving, lower ordinal, never another loser.
    pub duplicate_of: Option<u32>,
}

/// Preparation result only, not a durable classification checkpoint. Production
/// must freeze classifications and trusted mirror snapshots before Writing.
#[derive(Clone, PartialEq, Eq)]
pub struct LocalOccurrencePlan(Vec<LocalOccurrence>);

impl fmt::Debug for LocalOccurrencePlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LocalOccurrencePlan([redacted])")
    }
}

impl LocalOccurrencePlan {
    /// Includes every selected file. Losers retain the winner's local position
    /// for diagnostics, but must write no track, source, membership or media row.
    pub fn entries(&self) -> &[LocalOccurrence] {
        &self.0
    }
}

fn same_identity(
    a: &AudioHints,
    b: &AudioHints,
    a_title: &Option<String>,
    b_title: &Option<String>,
) -> bool {
    let recording = a
        .recording_mbid
        .as_deref()
        .zip(b.recording_mbid.as_deref())
        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b));
    let fingerprint = a
        .fingerprint
        .as_deref()
        .zip(b.fingerprint.as_deref())
        .is_some_and(|(a, b)| !a.is_empty() && a == b);
    let title_duration = a_title.is_some()
        && a_title == b_title
        && a.duration_ms.zip(b.duration_ms).is_some_and(|(a, b)| {
            let delta = u128::from(a.abs_diff(b));
            delta <= 2000 || delta * 100 <= u128::from(a.max(b))
        });
    recording || fingerprint || title_duration
}

/// Prepare positions only for a job whose entries will ALL remain local. Do not
/// combine this result with verified matches: mirror occurrence dedup, resolved
/// artist/release coalescing and classification checkpointing are separate gates.
/// Uses only the accepted manifest, never fresh tags. Legacy unfrozen discs and
/// non-ASCII titles refuse rather than guess normalization or upgrade the winner.
/// No production caller or SQL writer is enabled by this seam.
pub fn prepare_all_local_occurrences(
    manifest: &AcceptedAudioManifest,
) -> Result<LocalOccurrencePlan, PositionError> {
    let discs = manifest
        .effective_discs()
        .ok_or(PositionError::UnfrozenDiscs)?;
    let entries = manifest.entries();
    let titles = entries
        .iter()
        .map(|entry| {
            text(entry.hints.title.as_deref()).map_err(|err| match err {
                GroupingError::UnsupportedText => PositionError::UnsupportedText,
                _ => PositionError::InvalidHints,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut groups = BTreeMap::<u32, Vec<usize>>::new();
    for (i, entry) in entries.iter().enumerate() {
        groups.entry(entry.release_group_ref).or_default().push(i);
    }
    let mut winners = (0..entries.len()).collect::<Vec<_>>();
    let mut positions = vec![(1, 1); entries.len()];
    let mut comparisons = 0;
    for indexes in groups.values() {
        let mut pairs = BTreeMap::<(i32, i32), Vec<usize>>::new();
        let mut fallback = false;
        for &i in indexes {
            let Some(pair) = discs[i].zip(entries[i].hints.track_no.filter(|&n| n > 0)) else {
                fallback = true;
                continue;
            };
            let survivors = pairs.entry(pair).or_default();
            let mut duplicate = None;
            for &winner in survivors.iter() {
                if comparisons == MAX_IDENTITY_COMPARISONS {
                    return Err(PositionError::WorkLimit);
                }
                comparisons += 1;
                if same_identity(
                    &entries[winner].hints,
                    &entries[i].hints,
                    &titles[winner],
                    &titles[i],
                ) {
                    duplicate = Some(winner);
                    break;
                }
            }
            if let Some(winner) = duplicate {
                winners[i] = winner;
            } else {
                // Duration tolerance is not transitive. Only compare against
                // surviving lower ordinals; a discarded copy cannot bridge two
                // otherwise distinct identities into one duplicate set.
                fallback |= !survivors.is_empty();
                survivors.push(i);
            }
        }
        for &i in indexes {
            if winners[i] == i {
                positions[i] = if fallback {
                    (1, entries[i].audio_ordinal as i32)
                } else {
                    // Every entry has a complete pair when fallback is false.
                    (discs[i].unwrap(), entries[i].hints.track_no.unwrap())
                };
            }
        }
    }
    Ok(LocalOccurrencePlan(
        entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let winner = winners[i];
                let (medium_no, track_no) = positions[winner];
                LocalOccurrence {
                    audio_ordinal: entry.audio_ordinal,
                    release_group_ref: entry.release_group_ref,
                    medium_no,
                    track_no,
                    duplicate_of: (winner != i).then_some(entries[winner].audio_ordinal),
                }
            })
            .collect(),
    ))
}
