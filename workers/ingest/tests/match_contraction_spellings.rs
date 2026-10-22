//! Contraction spellings in release matching (tests-only, PROTECTED).
//!
//! The paired builder makes these pass without editing this file.
//!
//! Contract under test (`pick_release_mbid`, fed directly and via
//! `derive_upload_query` in `workers/ingest/src/matching.rs`):
//!
//! - Normalized title/artist comparison treats the documented
//!   contraction variant as the same identity: an ASCII apostrophe
//!   (`'`, U+0027) or a typographic apostrophe (`’`, U+2019) *inside a
//!   word* contributes nothing, so `Don't Stop!` matches `dont stop`
//!   in both title and artist comparison.
//! - Only apostrophes are ignored. Every other punctuation mark and
//!   every whitespace run still marks a genuine word boundary, so
//!   separate words never merge into one (`ab cd` never equals
//!   `abcd`, `don t stop` never equals `dont stop`), and edition
//!   suffixes still distinguish releases. A correction that deletes
//!   *all* punctuation, or all spaces, fails the guards below.
//! - Wrong artists, distinct-release ambiguity and different edition
//!   titles still reject; plain-title and `Artist - Album` controls
//!   stay deterministic (wire order never changes the outcome).
//!
//! Sensitivity: on the unmodified baseline the apostrophe is turned
//! into a word boundary, so each contraction-equivalence test below
//! fails (the pick returns `None` instead of the matching MBID),
//! the ambiguity test fails the other way (only the already-plain
//! spelling matches, so the baseline wrongly picks it), and
//! `spaced_contraction_fragments_do_not_merge` fails because the
//! baseline equates `Don't Stop` with the spaced spelling
//! `Don T Stop`. The remaining boundary, wrong-artist, edition and
//! control guards already pass and pin behavior the correction must
//! not widen.
//!
//! Only U+0027 and U+2019 are pinned as apostrophes; no other quote
//! characters, transliteration, or fuzzy matching is specified here.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/ingest/Cargo.toml \
//!     --test match_contraction_spellings
//! ```

use nntune_ingest::matching::{derive_upload_query, pick_release_mbid, MbSearchHit};

const MBID_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const MBID_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const MBID_C: &str = "cccccccc-3333-4333-8333-333333333333";

fn hit(mbid: &str, title: &str, artist_name: Option<&str>, score: f32) -> MbSearchHit {
    MbSearchHit {
        mbid: mbid.to_string(),
        title: title.to_string(),
        score,
        artist_mbid: None,
        artist_name: artist_name.map(str::to_string),
    }
}

fn plain_contraction_hit() -> MbSearchHit {
    // The unapostrophed spelling of the contraction title, no artist.
    hit(MBID_A, "Dont Stop", None, 0.99)
}

fn verified_plain_hit() -> MbSearchHit {
    hit(MBID_A, "Dont Stop", Some("Cant Sing"), 0.8)
}

// ---------------------------------------------------------------------
// Plain-title path: contractions match the plain spelling (RED)
// ---------------------------------------------------------------------

#[test]
fn plain_title_ascii_apostrophe_matches_unapostrophed_spelling() {
    assert_eq!(
        pick_release_mbid("Don't Stop", &[plain_contraction_hit()]).as_deref(),
        Some(MBID_A),
        "the documented contraction title must match its plain spelling"
    );
}

#[test]
fn plain_title_unapostrophed_query_matches_apostrophed_hit() {
    // Normalization runs on both sides, so the direction must not matter.
    let hits = vec![hit(MBID_A, "Don't Stop", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Dont Stop", &hits).as_deref(),
        Some(MBID_A),
        "a plain-spelled query must match the apostrophed hit title"
    );
}

#[test]
fn plain_title_typographic_apostrophe_matches_plain_spelling() {
    assert_eq!(
        pick_release_mbid("Don\u{2019}t Stop", &[plain_contraction_hit()]).as_deref(),
        Some(MBID_A),
        "a typographic apostrophe (U+2019) inside a word is also ignored"
    );
    let hits = vec![hit(MBID_A, "Don\u{2019}t Stop", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Dont Stop", &hits).as_deref(),
        Some(MBID_A),
        "a plain query must match a typographic-apostrophe hit title"
    );
}

#[test]
fn ascii_and_typographic_apostrophes_are_the_same_identity() {
    let hits = vec![hit(MBID_A, "Don\u{2019}t Stop", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Don't Stop", &hits).as_deref(),
        Some(MBID_A),
        "ASCII and typographic apostrophes spell the same contraction"
    );
}

#[test]
fn every_contraction_in_a_title_matches_not_just_the_first() {
    let hits = vec![hit(MBID_A, "Cant Dont Wont", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Can't Don't Won't", &hits).as_deref(),
        Some(MBID_A),
        "each in-word apostrophe is ignored, not only the first"
    );
}

#[test]
fn documented_punctuated_contraction_example_matches() {
    // The documented example: `Don't Stop!` == `dont stop` (trailing
    // punctuation and letter case carry no identity either).
    let hits = vec![hit(MBID_A, "dont stop", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Don't Stop!", &hits).as_deref(),
        Some(MBID_A),
        "the documented equivalent spelling must match"
    );
}

// ---------------------------------------------------------------------
// Verified `Artist - Album` path: contractions in artist and title
// ---------------------------------------------------------------------

#[test]
fn artist_and_title_contractions_match_plain_spellings() {
    assert_eq!(
        pick_release_mbid("Can't Sing - Don't Stop", &[verified_plain_hit()]).as_deref(),
        Some(MBID_A),
        "contractions are ignored in both artist and title comparison"
    );
    // Reverse direction: plain query, apostrophed hit identity.
    let hits = vec![hit(MBID_A, "Don't Stop", Some("Can't Sing"), 0.8)];
    assert_eq!(
        pick_release_mbid("Cant Sing - Dont Stop", &hits).as_deref(),
        Some(MBID_A),
        "a plain query must match the apostrophed hit artist and title"
    );
}

#[test]
fn typographic_apostrophes_match_in_artist_and_title() {
    assert_eq!(
        pick_release_mbid(
            "Can\u{2019}t Sing - Don\u{2019}t Stop",
            &[verified_plain_hit()],
        )
        .as_deref(),
        Some(MBID_A),
        "typographic apostrophes are ignored in artist and title alike"
    );
}

#[test]
fn contraction_hit_wins_over_a_higher_scored_decoy_regardless_of_order() {
    let decoy = || hit(MBID_B, "Different Album", Some("Cant Sing"), 0.99);
    let hits = vec![decoy(), verified_plain_hit()];
    assert_eq!(
        pick_release_mbid("Can't Sing - Don't Stop", &hits).as_deref(),
        Some(MBID_A),
        "the contraction-equivalent release is the pick, not the decoy"
    );
    let hits_rev = vec![verified_plain_hit(), decoy()];
    assert_eq!(
        pick_release_mbid("Can't Sing - Don't Stop", &hits_rev).as_deref(),
        Some(MBID_A),
        "the contraction pick must not depend on wire order"
    );
}

#[test]
fn contraction_subject_derives_and_picks_the_plain_spelled_release() {
    let subjects = vec![r#"Can't Sing - Don't Stop [01/05] - "01.flac" yEnc (1/3)"#.to_string()];
    let query = derive_upload_query(&subjects).expect("contraction subject must derive a query");
    assert_eq!(
        pick_release_mbid(&query, &[verified_plain_hit()]).as_deref(),
        Some(MBID_A),
        "query {query:?} must match the plain-spelled artist/title hit"
    );
}

// ---------------------------------------------------------------------
// Guards: word boundaries survive (no punctuation-wide deletion)
// ---------------------------------------------------------------------

#[test]
fn separate_words_never_merge_into_one_word() {
    let hits = vec![hit(MBID_A, "Abcd", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Ab Cd", &hits),
        None,
        "the two words `ab cd` are not the one word `abcd`"
    );
    let hits = vec![hit(MBID_A, "Ab Cd", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Abcd", &hits),
        None,
        "the one word `abcd` is not the two words `ab cd`"
    );
}

#[test]
fn spaced_contraction_fragments_do_not_merge() {
    // A space is a real boundary: `don t stop` is three words and
    // must never collapse to the contraction `dont stop`.
    assert_eq!(
        pick_release_mbid("Don T Stop", &[plain_contraction_hit()]),
        None,
        "spaced fragments must not merge into the contraction"
    );
    let hits = vec![hit(MBID_A, "Don T Stop", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Dont Stop", &hits),
        None,
        "the contraction must not match the spaced-fragment spelling"
    );
    let hits = vec![hit(MBID_A, "Don T Stop", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Don't Stop", &hits),
        None,
        "an apostrophed contraction must not match spaced fragments either"
    );
}

#[test]
fn other_punctuation_still_marks_a_word_boundary() {
    // Only apostrophes are ignored. Deleting *all* punctuation would
    // merge these words into `abcd`; the correction must not do that.
    let merged = vec![hit(MBID_A, "Abcd", None, 0.99)];
    for query in ["Ab-Cd", "Ab.Cd", "Ab, Cd", "Ab/Cd"] {
        assert_eq!(
            pick_release_mbid(query, &merged),
            None,
            "query {query:?}: non-apostrophe punctuation never merges words"
        );
    }
    // And the other way round: a one-word query never matches a
    // punctuation-separated two-word title.
    let separated = vec![hit(MBID_A, "Ab-Cd", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Abcd", &separated),
        None,
        "one-word query must not match a hyphen-separated title"
    );
}

#[test]
fn other_punctuation_still_separates_like_a_space() {
    // Existing documented behavior the fix must keep: punctuation
    // other than the apostrophe separates words, exactly like a
    // space, so these spellings stay equivalent.
    let hits = vec![hit(MBID_A, "Ab Cd", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Ab-Cd", &hits).as_deref(),
        Some(MBID_A),
        "a hyphen boundary still equals a space boundary"
    );
}

// ---------------------------------------------------------------------
// Guards: rejection paths stay closed
// ---------------------------------------------------------------------

#[test]
fn wrong_artist_still_rejects_a_contraction_title_match() {
    let hits = vec![hit(MBID_A, "Dont Stop", Some("Another Band"), 0.8)];
    assert_eq!(
        pick_release_mbid("Can't Sing - Don't Stop", &hits),
        None,
        "a contraction-equivalent title with the wrong artist must reject"
    );
    // The query artist's own fragments must not merge to fit either:
    // `Can T Sing` is a different (three-word) artist name.
    assert_eq!(
        pick_release_mbid("Can T Sing - Don't Stop", &[verified_plain_hit()]),
        None,
        "spaced artist fragments must not merge into the contraction"
    );
}

#[test]
fn distinct_releases_both_matching_via_contractions_are_ambiguous() {
    // Two distinct releases whose identities both become the plain
    // query once in-word apostrophes are ignored: ambiguous, so no
    // pick — the correction must not resolve the tie by spelling.
    let hits = vec![
        hit(MBID_A, "Dont Stop", Some("Cant Sing"), 0.8),
        hit(MBID_C, "Don't Stop", Some("Can't Sing"), 0.79),
    ];
    assert_eq!(
        pick_release_mbid("Cant Sing - Dont Stop", &hits),
        None,
        "two distinct contraction-equivalent releases must stay unpicked"
    );
    let hits_rev = vec![
        hit(MBID_C, "Don't Stop", Some("Can't Sing"), 0.79),
        hit(MBID_A, "Dont Stop", Some("Cant Sing"), 0.8),
    ];
    assert_eq!(
        pick_release_mbid("Cant Sing - Dont Stop", &hits_rev),
        None,
        "ambiguity must not depend on wire order"
    );
}

#[test]
fn edition_suffix_still_distinguishes_contraction_titles() {
    // The plain (studio) contraction release is not the live one:
    // ignoring apostrophes must not normalize the edition away.
    let hits = vec![hit(MBID_A, "Dont Stop", Some("Artist"), 0.8)];
    assert_eq!(
        pick_release_mbid("Artist - Don't Stop (Live)", &hits),
        None,
        "a live-edition query must not fall back to the plain release"
    );
}

#[test]
fn contraction_live_edition_picks_the_live_release() {
    let hits = vec![
        hit(MBID_A, "Dont Stop", Some("Artist"), 0.99),
        hit(MBID_B, "Dont Stop (Live)", Some("Artist"), 0.8),
    ];
    assert_eq!(
        pick_release_mbid("Artist - Don't Stop (Live)", &hits).as_deref(),
        Some(MBID_B),
        "the contraction live query selects the live release"
    );
    let hits_rev = vec![
        hit(MBID_B, "Dont Stop (Live)", Some("Artist"), 0.8),
        hit(MBID_A, "Dont Stop", Some("Artist"), 0.99),
    ];
    assert_eq!(
        pick_release_mbid("Artist - Don't Stop (Live)", &hits_rev).as_deref(),
        Some(MBID_B),
        "the live contraction pick must not depend on wire order"
    );
}

// ---------------------------------------------------------------------
// Controls: existing deterministic behavior is untouched
// ---------------------------------------------------------------------

#[test]
fn plain_and_verified_controls_stay_deterministic() {
    let hits = vec![hit(MBID_A, "Album", None, 0.99)];
    assert_eq!(
        pick_release_mbid("Album", &hits).as_deref(),
        Some(MBID_A),
        "the plain-title control still matches exactly"
    );
    let hits = vec![
        hit(MBID_B, "Album", Some("Someone Else"), 0.99),
        hit(MBID_A, "Album", Some("Artist"), 0.8),
    ];
    assert_eq!(
        pick_release_mbid("Artist - Album", &hits).as_deref(),
        Some(MBID_A),
        "the verified control still picks the matching-artist release"
    );
    assert_eq!(
        pick_release_mbid("Album", &[]),
        None,
        "no candidates still means no pick"
    );
}
