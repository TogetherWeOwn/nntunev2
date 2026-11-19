//! Numeric-only positional eligibility for an already selected release.
//!
//! This does not select a release, parse file numbering, reorder inputs or
//! authorize publication. Callers must still satisfy the matcher's unchanged
//! score/margin rules and the lifecycle's probe, review and publication gates.

/// Numeric projection of a file or a CC0 recording occurrence. Uses the same
/// signed position representation as `super::MbRecording`, without its text.
/// `None` on a file means numbering was absent, not document ordinal numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub medium_no: Option<i16>,
    pub track_no: Option<i16>,
}

/// Internal policy failures, not persisted reasons or an external UI vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionalRefusal {
    CountMismatch,
    EmptyInput,
    InvalidPosition,
    RecordingSequence,
    FileSequence,
    AmbiguousMedium,
}

/// Passing this policy is eligibility only, never publish authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionalEligibility {
    Eligible,
    Refused(PositionalRefusal),
}

/// Check positions in the exact order the caller intends to pair them.
///
/// `files` has one entry per audio file, including entries with absent optional
/// numbering. `recordings` has one entry per occurrence, not per distinct MBID,
/// in release order. The policy never sorts a swapped sequence into a match.
/// Recording media must be known and contiguous from 1; tracks must be exactly
/// 1..N within each medium. Missing recording media cannot prove a single disc.
///
/// Files may omit track numbering for a whole medium. When numbering is present,
/// the whole medium must have the exact 1..N sequence, in pairing order. Partial
/// numbering cannot establish that sequence. A single disc needs no file medium
/// numbers; multiple discs require explicit compatible media on every file, even
/// if globally increasing track numbers might suggest a possible distribution.
/// No absent file number is filled from its ordinal or from a recording.
pub fn evaluate_positions(
    audio_file_count: usize,
    files: &[Position],
    recordings: &[Position],
) -> PositionalEligibility {
    match check_positions(audio_file_count, files, recordings) {
        Ok(()) => PositionalEligibility::Eligible,
        Err(reason) => PositionalEligibility::Refused(reason),
    }
}

fn check_positions(
    audio_file_count: usize,
    files: &[Position],
    recordings: &[Position],
) -> Result<(), PositionalRefusal> {
    use PositionalRefusal::*;

    if audio_file_count != files.len() || audio_file_count != recordings.len() {
        return Err(CountMismatch);
    }
    if audio_file_count == 0 {
        return Err(EmptyInput);
    }
    if files.iter().chain(recordings).any(|position| {
        position.medium_no.is_some_and(|n| n <= 0) || position.track_no.is_some_and(|n| n <= 0)
    }) {
        return Err(InvalidPosition);
    }

    let mut medium = 1_i32;
    let mut track = 0_i32;
    for position in recordings {
        let actual_medium = i32::from(position.medium_no.ok_or(AmbiguousMedium)?);
        let actual_track = i32::from(position.track_no.ok_or(RecordingSequence)?);
        if actual_medium == medium + 1 {
            medium += 1;
            track = 0;
        } else if actual_medium != medium {
            return Err(RecordingSequence);
        }
        track += 1;
        if actual_track != track {
            return Err(RecordingSequence);
        }
    }
    // The first medium must also start at 1, not just follow contiguous steps.
    if recordings[0].medium_no != Some(1) {
        return Err(RecordingSequence);
    }
    let multi_disc = medium > 1;
    let mut previous_medium = None;
    let mut numbered = false;
    for (file, recording) in files.iter().zip(recordings) {
        if multi_disc && file.medium_no.is_none() {
            return Err(AmbiguousMedium);
        }
        if file.medium_no.is_some() && file.medium_no != recording.medium_no {
            return Err(FileSequence);
        }
        if previous_medium != recording.medium_no {
            previous_medium = recording.medium_no;
            numbered = file.track_no.is_some();
        }
        if file.track_no.is_some() != numbered || (numbered && file.track_no != recording.track_no)
        {
            return Err(FileSequence);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use PositionalEligibility::{Eligible, Refused};
    use PositionalRefusal::*;

    fn positions(values: &[(Option<i16>, Option<i16>)]) -> Vec<Position> {
        values
            .iter()
            .map(|&(medium_no, track_no)| Position {
                medium_no,
                track_no,
            })
            .collect()
    }

    fn single() -> Vec<Position> {
        positions(&[(Some(1), Some(1)), (Some(1), Some(2)), (Some(1), Some(3))])
    }

    fn multi() -> Vec<Position> {
        positions(&[
            (Some(1), Some(1)),
            (Some(1), Some(2)),
            (Some(2), Some(1)),
            (Some(2), Some(2)),
        ])
    }

    #[test]
    fn equal_counts_and_contiguous_positions_are_eligible() {
        let recording = single();
        assert_eq!(evaluate_positions(3, &recording, &recording), Eligible);
    }

    #[test]
    fn unequal_counts_and_incomplete_numeric_projections_refuse() {
        let recording = single();
        assert_eq!(
            evaluate_positions(2, &recording, &recording),
            Refused(CountMismatch)
        );
        assert_eq!(
            evaluate_positions(3, &recording[..2], &recording),
            Refused(CountMismatch)
        );
        assert_eq!(
            evaluate_positions(3, &recording, &recording[..2]),
            Refused(CountMismatch)
        );
        assert_eq!(evaluate_positions(0, &[], &[]), Refused(EmptyInput));
    }

    #[test]
    fn same_count_swapped_file_order_is_not_repaired_by_sorting() {
        let recording = single();
        let mut files = recording.clone();
        files.swap(0, 1);
        assert_eq!(
            evaluate_positions(3, &files, &recording),
            Refused(FileSequence)
        );
    }

    #[test]
    fn positive_duplicate_gap_and_partial_file_numbering_refuse() {
        let recording = single();
        for numbers in [
            [Some(1), Some(1), Some(3)],
            [Some(1), Some(2), Some(4)],
            [Some(2), Some(3), Some(4)],
            [Some(1), None, Some(3)],
            [None, Some(2), Some(3)],
        ] {
            let files: Vec<_> = numbers
                .into_iter()
                .map(|track_no| Position {
                    medium_no: None,
                    track_no,
                })
                .collect();
            assert_eq!(
                evaluate_positions(3, &files, &recording),
                Refused(FileSequence)
            );
        }
    }

    #[test]
    fn zero_and_negative_positions_refuse_on_both_sides() {
        let recording = single();
        for invalid in [0, -1, i16::MIN] {
            for medium in [false, true] {
                let mut bad = recording.clone();
                if medium {
                    bad[1].medium_no = Some(invalid);
                } else {
                    bad[1].track_no = Some(invalid);
                }
                assert_eq!(
                    evaluate_positions(3, &bad, &recording),
                    Refused(InvalidPosition)
                );
                assert_eq!(
                    evaluate_positions(3, &recording, &bad),
                    Refused(InvalidPosition)
                );
            }
        }
    }

    #[test]
    fn recording_sequences_must_be_complete_and_ordered() {
        let files = single();
        for numbers in [
            [Some(1), Some(1), Some(3)],
            [Some(1), Some(2), Some(4)],
            [Some(2), Some(1), Some(3)],
            [Some(1), None, Some(3)],
            [Some(1), Some(2), Some(i16::MAX)],
        ] {
            let recording: Vec<_> = numbers
                .into_iter()
                .map(|track_no| Position {
                    medium_no: Some(1),
                    track_no,
                })
                .collect();
            assert_eq!(
                evaluate_positions(3, &files, &recording),
                Refused(RecordingSequence)
            );
        }
    }

    #[test]
    fn absent_file_numbering_is_allowed_for_a_known_single_disc() {
        let recording = single();
        let files = vec![
            Position {
                medium_no: None,
                track_no: None
            };
            3
        ];
        assert_eq!(evaluate_positions(3, &files, &recording), Eligible);
        let numbered = positions(&[(None, Some(1)), (None, Some(2)), (None, Some(3))]);
        assert_eq!(evaluate_positions(3, &numbered, &recording), Eligible);
    }

    #[test]
    fn explicit_multi_disc_distribution_is_eligible() {
        let recording = multi();
        assert_eq!(evaluate_positions(4, &recording, &recording), Eligible);
        let unnumbered: Vec<_> = recording
            .iter()
            .map(|p| Position {
                medium_no: p.medium_no,
                track_no: None,
            })
            .collect();
        assert_eq!(evaluate_positions(4, &unnumbered, &recording), Eligible);
    }

    #[test]
    fn missing_multi_disc_allocation_refuses_even_with_global_numbering() {
        let recording = multi();
        for numbered in [false, true] {
            let files: Vec<_> = (1..=4)
                .map(|n| Position {
                    medium_no: None,
                    track_no: numbered.then_some(n),
                })
                .collect();
            assert_eq!(
                evaluate_positions(4, &files, &recording),
                Refused(AmbiguousMedium)
            );
        }
        let mut files = recording.clone();
        files[2].medium_no = None;
        assert_eq!(
            evaluate_positions(4, &files, &recording),
            Refused(AmbiguousMedium)
        );
    }

    #[test]
    fn wrong_multi_disc_distribution_and_interleaving_refuse() {
        let recording = multi();
        let mut files = recording.clone();
        files.swap(1, 2);
        assert_eq!(
            evaluate_positions(4, &files, &recording),
            Refused(FileSequence)
        );
        let mut wrong = recording.clone();
        wrong[1].medium_no = Some(2);
        assert_eq!(
            evaluate_positions(4, &wrong, &recording),
            Refused(FileSequence)
        );
        assert_eq!(
            evaluate_positions(4, &recording, &files),
            Refused(RecordingSequence)
        );
    }

    #[test]
    fn missing_recording_media_never_imply_single_disc() {
        let files = single();
        for index in [None, Some(1)] {
            let mut recording = files.clone();
            for (i, position) in recording.iter_mut().enumerate() {
                if index.is_none() || index == Some(i) {
                    position.medium_no = None;
                }
            }
            assert_eq!(
                evaluate_positions(3, &files, &recording),
                Refused(AmbiguousMedium)
            );
        }
    }

    #[test]
    fn recording_media_must_start_at_one_and_have_no_gaps() {
        let files = multi();
        for media in [[2, 2, 3, 3], [1, 1, 3, 3], [1, 1, 1, 1]] {
            let recording: Vec<_> = files
                .iter()
                .zip(media)
                .map(|(p, medium_no)| Position {
                    medium_no: Some(medium_no),
                    track_no: p.track_no,
                })
                .collect();
            assert_eq!(
                evaluate_positions(4, &files, &recording),
                Refused(RecordingSequence)
            );
        }
    }
}
