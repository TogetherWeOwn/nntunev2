//! Match score and margin threshold pins (tests-only).
//!
//! Contract under test (`workers/ingest/src/matching.rs:42,49`,
//! enforced at `:321,332`):
//!
//! - `MIN_MATCH_SCORE == 0.6` (floor for a plain-title hit to count);
//! - `MIN_MATCH_MARGIN == 0.1` (lead the top hit must hold over the
//!   runner-up; a gap at or under the margin is ambiguous, not a pick).
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - raising/lowering `MIN_MATCH_SCORE` changes the floor, failing
//!   `min_match_score_is_zero_point_six` plus the boundary pair
//!   (`lone_hit_at_score_floor_matches` / `lone_hit_below_score_floor_rejected`);
//! - raising/lowering `MIN_MATCH_MARGIN` changes the ambiguity band, failing
//!   `min_match_margin_is_zero_point_one` plus the margin pair
//!   (`runner_up_within_margin_blocks_pick` / `runner_up_beyond_margin_admits_pick`);
//! - weakening the `>=` floor check to `>` in `pick_release_mbid` rejects
//!   the exact-0.6 lone hit, failing `lone_hit_at_score_floor_matches`;
//! - weakening the `<=` margin check to `<` admits the exact-0.1-margin
//!   near-tie, failing `runner_up_within_margin_blocks_pick`.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test match_threshold_literals_acceptance
//! ```

use nntune_ingest::matching::{pick_release_mbid, MbSearchHit, MIN_MATCH_MARGIN, MIN_MATCH_SCORE};

const MBID_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const MBID_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";

fn hit(mbid: &str, score: f32) -> MbSearchHit {
    MbSearchHit {
        mbid: mbid.to_string(),
        title: "Album".to_string(),
        score,
        artist_mbid: None,
        artist_name: None,
    }
}

#[test]
fn min_match_score_is_zero_point_six() {
    assert_eq!(
        MIN_MATCH_SCORE, 0.6_f32,
        "plain-title score floor must stay exactly 0.6"
    );
}

#[test]
fn min_match_margin_is_zero_point_one() {
    assert_eq!(
        MIN_MATCH_MARGIN, 0.1_f32,
        "top-hit lead over the runner-up must stay exactly 0.1"
    );
}

#[test]
fn lone_hit_at_score_floor_matches() {
    // A lone hit scoring exactly the floor clears the `>=` check.
    assert_eq!(
        pick_release_mbid("Album", &[hit(MBID_A, 0.6)]).as_deref(),
        Some(MBID_A),
        "a lone 0.6-scored hit must match (floor is inclusive)"
    );
}

#[test]
fn lone_hit_below_score_floor_rejected() {
    // One hundredth under the floor never matches.
    assert_eq!(
        pick_release_mbid("Album", &[hit(MBID_A, 0.59)]),
        None,
        "a lone 0.59-scored hit must not match"
    );
}

#[test]
fn runner_up_within_margin_blocks_pick() {
    // A 0.7 top over a 0.6 runner-up leads by exactly the 0.1 margin:
    // ambiguous, so no pick either way round.
    let hits = vec![hit(MBID_A, 0.7), hit(MBID_B, 0.6)];
    assert_eq!(
        pick_release_mbid("Album", &hits),
        None,
        "a runner-up within a 0.1 margin must block the pick"
    );
    let hits_rev = vec![hit(MBID_B, 0.6), hit(MBID_A, 0.7)];
    assert_eq!(
        pick_release_mbid("Album", &hits_rev),
        None,
        "margin-blocked picks must not depend on wire order"
    );
}

#[test]
fn runner_up_beyond_margin_admits_pick() {
    // A 0.71 top over a 0.6 runner-up leads by 0.11: clear, so the top wins.
    let hits = vec![hit(MBID_B, 0.6), hit(MBID_A, 0.71)];
    assert_eq!(
        pick_release_mbid("Album", &hits).as_deref(),
        Some(MBID_A),
        "a runner-up 0.11 behind must admit the top pick"
    );
}
