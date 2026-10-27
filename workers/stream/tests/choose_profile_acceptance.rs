//! Profile-artifact selection acceptance: the pure opus-64 chooser.
//!
//! Pins `nntune_stream::choose::choose_profile_artifact` over caller-supplied
//! `MediaRow` values mirroring `track_media(format, bitrate, content_hash)`
//! (see the oracle doc `choose-slice-acceptance.md` for the transcribed
//! contract and decision table). Vectors only: no DB, R2, grants, HTTP,
//! sockets, clock or network. The hashes below are inert synthetic fixtures,
//! not authorized artifacts, and a well-shaped row grants no authority to
//! serve bytes.
//!
//! Failing command (worktree root):
//! `python3 scripts/cargo_cache.py run -- test -p nntune-stream --test choose_profile_acceptance`
//!
//! Sensitivity: returning the first row instead of scanning the ladder must
//! fail `exact_profile_hit_wins_regardless_of_order` and
//! `ladder_fallback_takes_first_opus_row_in_candidate_order`; accepting a
//! malformed row must fail `malformed_rows_are_skipped_for_well_formed_opus`
//! and `all_malformed_set_returns_malformed_miss`; serving a cross-profile
//! row must fail `cross_profile_rows_are_never_served`; swapping any miss
//! code or next action must fail the code asserts in the miss tests.

use nntune_stream::choose::{
    choose_profile_artifact, ChoiceMiss, MatchLevel, MediaRow, NextAction, StreamProfile,
};

const OPUS_64_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OPUS_128_HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OPUS_32_HASH: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const FLAC_HASH: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const MP3_HASH: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const WAV_HASH: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

fn row<'a>(format: &'a str, bitrate: i32, content_hash: &'a str) -> MediaRow<'a> {
    MediaRow {
        format,
        bitrate,
        content_hash,
    }
}

/// An exact opus-64 row wins even when it is not first in candidate order.
#[test]
fn exact_profile_hit_wins_regardless_of_order() {
    for rows in [
        vec![
            row("flac", 1411, FLAC_HASH),
            row("opus", 128, OPUS_128_HASH),
            row("opus", 64, OPUS_64_HASH),
            row("opus", 32, OPUS_32_HASH),
        ],
        vec![
            row("opus", 32, OPUS_32_HASH),
            row("mp3", 320, MP3_HASH),
            row("opus", 64, OPUS_64_HASH),
        ],
    ] {
        let chosen = choose_profile_artifact(&rows, StreamProfile::Opus64)
            .expect("exact opus-64 row must be chosen");
        assert_eq!(
            chosen.content_hash, OPUS_64_HASH,
            "exact hit must carry the opus-64 artifact hash"
        );
        assert_eq!(chosen.format, "opus");
        assert_eq!(chosen.bitrate, 64);
        assert_eq!(
            chosen.match_level,
            MatchLevel::Exact,
            "the opus-64 row is an exact hit, not a fallback"
        );
    }
}

/// Without an exact row, the first well-formed opus row in candidate order is
/// the documented fallback; candidate order, not bitrate, breaks ties.
#[test]
fn ladder_fallback_takes_first_opus_row_in_candidate_order() {
    let rows = [
        row("flac", 1411, FLAC_HASH),
        row("opus", 128, OPUS_128_HASH),
        row("opus", 32, OPUS_32_HASH),
    ];
    let chosen = choose_profile_artifact(&rows, StreamProfile::Opus64)
        .expect("opus rows without an exact hit must fall back");
    assert_eq!(
        chosen.content_hash, OPUS_128_HASH,
        "fallback must take the first opus row in candidate order"
    );
    assert_eq!(chosen.format, "opus");
    assert_eq!(chosen.bitrate, 128);
    assert_eq!(chosen.match_level, MatchLevel::Fallback);

    let swapped = [
        row("opus", 32, OPUS_32_HASH),
        row("opus", 128, OPUS_128_HASH),
    ];
    let chosen = choose_profile_artifact(&swapped, StreamProfile::Opus64)
        .expect("opus rows without an exact hit must fall back");
    assert_eq!(
        chosen.content_hash, OPUS_32_HASH,
        "reordered candidates must move the fallback with them"
    );
    assert_eq!(chosen.match_level, MatchLevel::Fallback);
}

/// An empty candidate set is a final typed miss: nothing exists to lease,
/// dial or fetch before the choice.
#[test]
fn empty_candidate_set_returns_final_empty_miss() {
    let rows: &[MediaRow<'_>] = &[];
    match choose_profile_artifact(rows, StreamProfile::Opus64) {
        Ok(chosen) => panic!("empty set must miss, chose {chosen:?}"),
        Err(miss) => {
            assert_eq!(miss, ChoiceMiss::EmptyCandidateSet);
            assert_eq!(miss.code(), "profile_artifact_empty");
            assert_eq!(miss.next_action(), NextAction::TranscodeThenRetry);
        }
    }
}

/// Well-formed rows outside the opus family are never served for an opus-64
/// request: cross-profile serve is refused with a typed miss.
#[test]
fn cross_profile_rows_are_never_served() {
    for rows in [
        vec![
            row("flac", 1411, FLAC_HASH),
            row("mp3", 320, MP3_HASH),
            row("wav", 1536, WAV_HASH),
        ],
        vec![row("flac", 1411, FLAC_HASH)],
    ] {
        match choose_profile_artifact(&rows, StreamProfile::Opus64) {
            Ok(chosen) => panic!("cross-profile rows must miss, chose {chosen:?}"),
            Err(miss) => {
                assert_eq!(miss, ChoiceMiss::CrossProfileRefused);
                assert_eq!(miss.code(), "profile_artifact_cross_profile");
                assert_eq!(miss.next_action(), NextAction::TranscodeThenRetry);
            }
        }
    }
}

/// Profile matching is case-sensitive: `OPUS` is not the opus family.
#[test]
fn uppercase_format_is_cross_profile_not_opus() {
    let rows = [row("OPUS", 64, OPUS_64_HASH)];
    match choose_profile_artifact(&rows, StreamProfile::Opus64) {
        Ok(chosen) => panic!("uppercase format must miss, chose {chosen:?}"),
        Err(miss) => {
            assert_eq!(miss, ChoiceMiss::CrossProfileRefused);
            assert_eq!(miss.code(), "profile_artifact_cross_profile");
        }
    }
}

/// Malformed rows are skipped: a malformed opus-64-shaped row must not shadow
/// a well-formed one, in either position.
#[test]
fn malformed_rows_are_skipped_for_well_formed_opus() {
    let malformed_exact = row("opus", 64, "");
    for rows in [
        vec![malformed_exact, row("opus", 64, OPUS_64_HASH)],
        vec![row("opus", 64, OPUS_64_HASH), malformed_exact],
        vec![
            row("opus", 0, OPUS_128_HASH),
            row("opus", 128, OPUS_128_HASH),
        ],
    ] {
        let chosen = choose_profile_artifact(&rows, StreamProfile::Opus64)
            .expect("well-formed opus row must survive malformed siblings");
        assert!(
            !chosen.content_hash.is_empty(),
            "a malformed row must never be chosen"
        );
    }
    let rows = [row("opus", 64, ""), row("opus", 128, OPUS_128_HASH)];
    let chosen = choose_profile_artifact(&rows, StreamProfile::Opus64)
        .expect("well-formed fallback must survive a malformed exact-shaped row");
    assert_eq!(chosen.content_hash, OPUS_128_HASH);
    assert_eq!(chosen.match_level, MatchLevel::Fallback);
}

/// Every malformation shape on its own yields the malformed miss, and so does
/// a set where no row is well-formed.
#[test]
fn all_malformed_set_returns_malformed_miss() {
    const CASES: &[(&str, &str, i32, &str)] = &[
        ("empty format", "", 64, OPUS_64_HASH),
        ("zero bitrate", "opus", 0, OPUS_64_HASH),
        ("negative bitrate", "opus", -1, OPUS_64_HASH),
        ("empty hash", "opus", 64, ""),
        ("short hash", "opus", 64, "abc123"),
        (
            "uppercase hash",
            "opus",
            64,
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ),
        (
            "non-hex hash",
            "opus",
            64,
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        ),
        (
            "long hash",
            "opus",
            64,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    ];
    for (name, format, bitrate, hash) in CASES {
        let rows = [row(format, *bitrate, hash)];
        match choose_profile_artifact(&rows, StreamProfile::Opus64) {
            Ok(chosen) => panic!("{name} must miss, chose {chosen:?}"),
            Err(miss) => {
                assert_eq!(miss, ChoiceMiss::NoWellFormedCandidate, "{name}");
                assert_eq!(miss.code(), "profile_artifact_malformed", "{name}");
                assert_eq!(miss.next_action(), NextAction::RepairCandidates, "{name}");
            }
        }
    }
    let rows = [row("", 0, ""), row("opus", -5, "not-a-hash")];
    match choose_profile_artifact(&rows, StreamProfile::Opus64) {
        Ok(chosen) => panic!("all-malformed set must miss, chose {chosen:?}"),
        Err(miss) => {
            assert_eq!(miss, ChoiceMiss::NoWellFormedCandidate);
            assert_eq!(miss.code(), "profile_artifact_malformed");
            assert_eq!(miss.next_action(), NextAction::RepairCandidates);
        }
    }
}

/// A well-formed cross-profile row beats malformed opus-shaped rows: the
/// actionable "source exists, transcode it" verdict wins over "repair".
#[test]
fn well_formed_cross_profile_beats_malformed_opus() {
    let rows = [row("opus", 64, ""), row("flac", 1411, FLAC_HASH)];
    match choose_profile_artifact(&rows, StreamProfile::Opus64) {
        Ok(chosen) => panic!("no well-formed opus row exists, chose {chosen:?}"),
        Err(miss) => {
            assert_eq!(miss, ChoiceMiss::CrossProfileRefused);
            assert_eq!(miss.code(), "profile_artifact_cross_profile");
            assert_eq!(miss.next_action(), NextAction::TranscodeThenRetry);
        }
    }
}

/// Pure choice is deterministic: the same slice chooses the same outcome.
#[test]
fn choice_is_deterministic() {
    let rows = [row("flac", 1411, FLAC_HASH), row("opus", 64, OPUS_64_HASH)];
    let first = choose_profile_artifact(&rows, StreamProfile::Opus64);
    let second = choose_profile_artifact(&rows, StreamProfile::Opus64);
    assert_eq!(first, second);

    let empty: &[MediaRow<'_>] = &[];
    let miss_first = choose_profile_artifact(empty, StreamProfile::Opus64);
    let miss_second = choose_profile_artifact(empty, StreamProfile::Opus64);
    assert_eq!(miss_first, miss_second);
}
