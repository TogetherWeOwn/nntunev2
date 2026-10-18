//! Runner-up comparison for plain-title picks (tests-only).
//!
//! Contract under test (`workers/ingest/src/matching.rs`):
//!
//! - the winning hit must itself meet `MIN_MATCH_SCORE` (0.6);
//! - its lead is then compared against the next distinct valid hit
//!   whose normalized title matches the query, even when that
//!   runner-up scores just below the winner floor;
//! - a lead at or under `MIN_MATCH_MARGIN` (0.1) is ambiguous and
//!   yields no pick; a genuinely clear lead still picks the winner.
//!
//! The score floor is an eligibility bar for the winner, not a filter
//! that may erase a near-tied rival before the margin comparison: two
//! distinct same-title releases at 0.65 / 0.59 are only 0.06 apart and
//! must not be accepted as a lone winner.
//!
//! Sensitivity (implementation stays untouched by this file):
//!
//! - applying the floor to the candidate list before finding the
//!   runner-up fails every `*_rejects` near-tie test below;
//! - dropping the winner floor entirely fails
//!   `top_below_floor_never_wins_even_with_clear_lead` and
//!   `lone_below_floor_rejects`;
//! - letting any below-floor rival block regardless of lead fails
//!   `clear_lead_over_below_floor_runner_up_accepts`;
//! - comparing against a hit that is not the nearest rival fails
//!   `nearest_runner_up_governs_not_the_lowest_hit`;
//! - letting malformed-MBID or wrong-title hits count as rivals fails
//!   the corresponding `*_does_not_block` tests.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/ingest/Cargo.toml \
//!     --test match_runner_up_floor
//! ```

use nntune_ingest::matching::{pick_release_mbid, MbSearchHit};

const MBID_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const MBID_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const MBID_C: &str = "cccccccc-3333-4333-8333-333333333333";

fn hit(mbid: &str, score: f32) -> MbSearchHit {
    hit_titled(mbid, "Album", score)
}

fn hit_titled(mbid: &str, title: &str, score: f32) -> MbSearchHit {
    MbSearchHit {
        mbid: mbid.to_string(),
        title: title.to_string(),
        score,
        artist_mbid: None,
        artist_name: None,
    }
}

fn hit_by(mbid: &str, title: &str, artist: &str, score: f32) -> MbSearchHit {
    MbSearchHit {
        mbid: mbid.to_string(),
        title: title.to_string(),
        score,
        artist_mbid: Some("dddddddd-4444-4444-8444-444444444444".to_string()),
        artist_name: Some(artist.to_string()),
    }
}

// ---------------------------------------------------------------------
// The near-tie below the winner floor must reject.
// ---------------------------------------------------------------------

#[test]
fn near_tied_below_floor_runner_up_rejects() {
    // 0.65 over 0.59 leads by 0.06, inside the 0.1 margin: ambiguous,
    // even though the runner-up is itself below the 0.6 winner floor.
    let hits = vec![hit(MBID_A, 0.65), hit(MBID_B, 0.59)];
    assert_eq!(
        pick_release_mbid("Album", &hits),
        None,
        "a below-floor runner-up 0.06 behind must block the pick"
    );
    let hits_rev = vec![hit(MBID_B, 0.59), hit(MBID_A, 0.65)];
    assert_eq!(
        pick_release_mbid("Album", &hits_rev),
        None,
        "the below-floor near-tie rejection must not depend on wire order"
    );
}

#[test]
fn floor_winner_with_below_floor_near_runner_up_rejects() {
    // The winner sits exactly on the floor and the rival one hundredth
    // behind it: the smallest possible below-floor near-tie.
    let hits = vec![hit(MBID_B, 0.59), hit(MBID_A, 0.6)];
    assert_eq!(
        pick_release_mbid("Album", &hits),
        None,
        "a 0.6 winner over a distinct 0.59 same-title rival is a near-tie"
    );
}

#[test]
fn exact_margin_below_floor_runner_up_rejects() {
    // 0.65 over 0.55 leads by exactly the 0.1 margin in f32: at the
    // margin is still ambiguous, matching the above-floor boundary.
    let hits = vec![hit(MBID_A, 0.65), hit(MBID_B, 0.55)];
    assert_eq!(
        pick_release_mbid("Album", &hits),
        None,
        "a lead of exactly the margin over a below-floor rival must reject"
    );
}

#[test]
fn normalized_title_below_floor_runner_up_rejects() {
    // The rival counts through the same normalized-title comparison as
    // the winner: case/punctuation variants of the title are rivals.
    let hits = vec![hit(MBID_A, 0.65), hit_titled(MBID_B, "album!", 0.59)];
    assert_eq!(
        pick_release_mbid("Album", &hits),
        None,
        "a normalized-title rival below the floor must still block the pick"
    );
}

#[test]
fn nearest_runner_up_governs_not_the_lowest_hit() {
    // A distant third hit does not rescue the pick: the comparison is
    // against the nearest rival (0.59), not the lowest score present.
    let hits = vec![hit(MBID_C, 0.3), hit(MBID_A, 0.65), hit(MBID_B, 0.59)];
    assert_eq!(
        pick_release_mbid("Album", &hits),
        None,
        "the nearest below-floor rival must govern the margin comparison"
    );
}

// ---------------------------------------------------------------------
// A genuinely clear lead still accepts.
// ---------------------------------------------------------------------

#[test]
fn clear_lead_over_below_floor_runner_up_accepts() {
    let hits = vec![hit(MBID_B, 0.59), hit(MBID_A, 0.9)];
    assert_eq!(
        pick_release_mbid("Album", &hits).as_deref(),
        Some(MBID_A),
        "a 0.31 lead over a below-floor rival must admit the top pick"
    );
    // Just beyond the margin (0.12 in f32) is likewise a clear lead.
    let just_beyond = vec![hit(MBID_A, 0.71), hit(MBID_B, 0.59)];
    assert_eq!(
        pick_release_mbid("Album", &just_beyond).as_deref(),
        Some(MBID_A),
        "a lead beyond the margin over a below-floor rival must admit the pick"
    );
}

#[test]
fn clear_lead_over_nearest_rival_accepts_with_below_floor_third() {
    // The nearest rival is above the floor and clearly beaten; the
    // below-floor third hit is farther away still and cannot block.
    let hits = vec![hit(MBID_C, 0.59), hit(MBID_B, 0.65), hit(MBID_A, 0.9)];
    assert_eq!(
        pick_release_mbid("Album", &hits).as_deref(),
        Some(MBID_A),
        "a clear lead over the nearest rival must admit the top pick"
    );
}

// ---------------------------------------------------------------------
// The winner floor itself is unchanged.
// ---------------------------------------------------------------------

#[test]
fn lone_at_floor_accepts() {
    assert_eq!(
        pick_release_mbid("Album", &[hit(MBID_A, 0.6)]).as_deref(),
        Some(MBID_A),
        "a lone hit exactly at the 0.6 floor retains its eligibility"
    );
}

#[test]
fn lone_below_floor_rejects() {
    assert_eq!(
        pick_release_mbid("Album", &[hit(MBID_A, 0.59)]),
        None,
        "a lone hit below the 0.6 floor must not match"
    );
}

#[test]
fn top_below_floor_never_wins_even_with_clear_lead() {
    // Widening the rival comparison must not lower the winner bar: a
    // top hit under the floor has no winner to defend, lead or not.
    let hits = vec![hit(MBID_A, 0.59), hit(MBID_B, 0.3)];
    assert_eq!(
        pick_release_mbid("Album", &hits),
        None,
        "no hit below the floor may win, however clear its lead"
    );
}

// ---------------------------------------------------------------------
// Only valid, matching-title hits count as rivals.
// ---------------------------------------------------------------------

#[test]
fn malformed_mbid_runner_up_does_not_block() {
    let hits = vec![hit("not-an-mbid", 0.64), hit(MBID_A, 0.65)];
    assert_eq!(
        pick_release_mbid("Album", &hits).as_deref(),
        Some(MBID_A),
        "a malformed-MBID hit is not a valid rival and must not block"
    );
}

#[test]
fn wrong_title_runner_up_does_not_block() {
    let hits = vec![
        hit_titled(MBID_B, "Something Else", 0.64),
        hit(MBID_A, 0.65),
    ];
    assert_eq!(
        pick_release_mbid("Album", &hits).as_deref(),
        Some(MBID_A),
        "a wrong-title hit is not a rival for this query and must not block"
    );
}

// ---------------------------------------------------------------------
// The verified "Artist - Album" path is untouched by score or margin.
// ---------------------------------------------------------------------

#[test]
fn artist_album_verified_path_unchanged() {
    // Verified by artist + title, never scored: the real album wins at
    // 0.333 over the higher-scored self-titled release.
    let hits = vec![
        hit_by(MBID_B, "Metallica", "Metallica", 0.667),
        hit_by(MBID_A, "Load", "Metallica", 0.333),
    ];
    assert_eq!(
        pick_release_mbid("Metallica - Load", &hits).as_deref(),
        Some(MBID_A),
        "the verified artist/album pick must ignore score and margin"
    );
    // Two distinct verified releases for the same artist and title
    // remain ambiguous and unmatched.
    let dupes = vec![
        hit_by(MBID_A, "Load", "Metallica", 0.333),
        hit_by(MBID_B, "Load", "Metallica", 0.333),
    ];
    assert_eq!(
        pick_release_mbid("Metallica - Load", &dupes),
        None,
        "distinct verified releases with one artist/title must stay unmatched"
    );
}
