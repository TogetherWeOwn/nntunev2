//! Edition-suffix preservation in uploaded-release queries (tests-only).
//!
//! Contract under test (`workers/ingest/src/matching.rs`,
//! `derive_upload_query` then `pick_release_mbid`):
//!
//! - A *recognized numeric part counter* is a trailing bracketed or
//!   parenthesized `digits/digits` group (the documented `[01/05]` /
//!   `(1/3)` scene forms), possibly repeated, stacked with the other
//!   bracket form, or wrapped in one extra matching bracket pair.
//!   Recognized counters and a trailing `yEnc` token are stripped.
//! - Everything else trailing is part of the release identity and is
//!   preserved verbatim: edition suffixes such as `(Live)`, `(Deluxe)`
//!   and `[Remastered]`, disc markers, years, and malformed
//!   counter look-alikes (`[aa/bb]`, `[01/]`, `[1/2/3]`,
//!   `[01/05 Live]`, a bare `(2024)` with no slash).
//! - Stripping is trailing-only: a counter that an edition suffix
//!   follows is interior text and is not excavated.
//! - Feeding the derived query to `pick_release_mbid`, a live (or
//!   deluxe / remastered) subject must never silently select the
//!   studio release: it either selects the edition-matching release
//!   or stays unmatched (`None`) — no fuzzy fallback, no loosened
//!   score thresholds, no widened query budget.
//! - Malformed or empty subjects derive no query at all.
//!
//! Sensitivity: the baseline strips *every* trailing bracketed /
//! parenthesized group, so each edition-preservation test and each
//! derive-then-pick test below fails on the baseline (the derived
//! query loses the edition, and the pick then returns the studio
//! MBID). Counter-only and malformed-input guards already pass and
//! pin behavior the correction must not regress.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/ingest/Cargo.toml \
//!     --test upload_query_editions
//! ```

use nntune_ingest::matching::{derive_upload_query, pick_release_mbid, MbSearchHit};

const STUDIO: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const LIVE: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const LIVE_ALT: &str = "cccccccc-3333-4333-8333-333333333333";
const OTHER: &str = "dddddddd-4444-4444-8444-444444444444";

fn subjects(first: &str) -> Vec<String> {
    vec![first.to_string()]
}

fn hit(mbid: &str, title: &str, artist_name: Option<&str>, score: f32) -> MbSearchHit {
    MbSearchHit {
        mbid: mbid.to_string(),
        title: title.to_string(),
        score,
        artist_mbid: None,
        artist_name: artist_name.map(str::to_string),
    }
}

fn studio_hit() -> MbSearchHit {
    hit(STUDIO, "Album", Some("Artist"), 0.99)
}

fn live_hit() -> MbSearchHit {
    hit(LIVE, "Album (Live)", Some("Artist"), 0.8)
}

// ---------------------------------------------------------------------
// derive_upload_query: edition suffixes survive counter stripping
// ---------------------------------------------------------------------

#[test]
fn live_suffix_survives_bracket_counter() {
    let s = subjects(r#"Artist - Album (Live) [01/05] - "01.flac" yEnc (1/3)"#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album (Live)"),
        "the numeric counter is stripped but (Live) is release identity"
    );
}

#[test]
fn live_suffix_without_any_counter_is_preserved() {
    let s = subjects(r#"Artist - Album (Live) - "01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album (Live)"),
        "a trailing edition group with no counter present is not stripped"
    );
}

#[test]
fn deluxe_suffix_survives_paren_counter() {
    let s = subjects(r#"Artist - Album (Deluxe) (1/3) - "01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album (Deluxe)"),
        "(1/3) is a counter; (Deluxe) is not"
    );
}

#[test]
fn remastered_suffix_survives_bracket_counter() {
    let s = subjects(r#"Artist - Album [Remastered] [01/05] - "01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album [Remastered]"),
        "only the numeric bracket group is a counter"
    );
}

#[test]
fn disc_marker_and_year_are_not_counters() {
    let s = subjects(r#"Artist - Album (Disc 1) [01/05] - "01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album (Disc 1)"),
        "(Disc 1) carries a word, so it is identity, not a counter"
    );
    let s = subjects(r#"Artist - Album (2024) [01/05] - "01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album (2024)"),
        "a bare year has no slash: not a recognized digits/digits counter"
    );
}

#[test]
fn malformed_counter_look_alikes_are_preserved() {
    for (subject, expected) in [
        (
            r#"Artist - Album [aa/bb] - "01.flac""#,
            "Artist - Album [aa/bb]",
        ),
        (
            r#"Artist - Album [01/] - "01.flac""#,
            "Artist - Album [01/]",
        ),
        (
            r#"Artist - Album [/05] - "01.flac""#,
            "Artist - Album [/05]",
        ),
        (
            r#"Artist - Album [1/2/3] - "01.flac""#,
            "Artist - Album [1/2/3]",
        ),
        (
            r#"Artist - Album [01/05 Live] - "01.flac""#,
            "Artist - Album [01/05 Live]",
        ),
    ] {
        assert_eq!(
            derive_upload_query(&subjects(subject)).as_deref(),
            Some(expected),
            "subject {subject:?}: only a clean digits/digits group is stripped"
        );
    }
}

#[test]
fn edition_survives_yenc_after_the_counter() {
    // No quoted file name: yEnc itself is the trailing token, then the
    // counter, then the edition. Stripping must stop at the edition.
    let s = subjects("Artist - Album (Live) [01/05] yEnc");
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album (Live)")
    );
    let s = subjects("Artist - Album (Deluxe) YENC");
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album (Deluxe)"),
        "yEnc stripping is case-insensitive and still stops at the edition"
    );
}

#[test]
fn dotted_scene_subject_keeps_its_edition() {
    let s = subjects(r#"Artist.-.Album.(Live).[01/05].-."01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album (Live)"),
        "dots become spaces first; the edition still survives the counter"
    );
}

#[test]
fn interior_counter_is_not_excavated_past_an_edition() {
    // Stripping is trailing-only: once the trailing group is identity
    // ((Live)), an earlier counter stays put rather than being dug out.
    let s = subjects(r#"Artist - Album [01/05] (Live) - "01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album [01/05] (Live)")
    );
}

#[test]
fn first_subject_wins_and_keeps_its_edition() {
    let s = vec![
        r#"Artist - Album (Live) [01/05] - "01.flac""#.to_string(),
        r#"Someone Else - Other [02/05] - "02.flac""#.to_string(),
    ];
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album (Live)")
    );
}

// ---------------------------------------------------------------------
// derive_upload_query: recognized counters still strip (guards)
// ---------------------------------------------------------------------

#[test]
fn ordinary_counter_only_subject_is_unchanged() {
    let s = subjects(r#"Pink Floyd - Animals [01/05] - "01 pigs.flac" yEnc (1/3)"#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Pink Floyd - Animals")
    );
    let s = subjects(r#"Artist - Album (1/3) - "01.flac""#);
    assert_eq!(derive_upload_query(&s).as_deref(), Some("Artist - Album"));
}

#[test]
fn repeated_and_stacked_counters_all_strip() {
    let s = subjects(r#"Artist - Album [01/05] [02/05] - "01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album"),
        "repeated same-form counters strip one after another"
    );
    let s = subjects(r#"Artist - Album [123/456] (1/3) [2/2] - "01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album"),
        "mixed bracket/paren counters stack and all strip"
    );
    let s = subjects("Artist - Album yEnc [01/05]");
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album"),
        "a counter after yEnc still leaves yEnc trailing, and it strips too"
    );
}

#[test]
fn nested_counter_wrapping_strips_as_one_counter() {
    let s = subjects(r#"Artist - Album [(1/3)] - "01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album"),
        "a counter nested in the other bracket form is still one counter"
    );
    let s = subjects(r#"Artist - Album [[01/05]] - "01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album"),
        "a doubly wrapped counter leaves no stray bracket behind"
    );
}

#[test]
fn stacked_counters_after_an_edition_stop_at_the_edition() {
    let s = subjects(r#"Artist - Album (Live) [01/05] (1/3) - "01.flac""#);
    assert_eq!(
        derive_upload_query(&s).as_deref(),
        Some("Artist - Album (Live)"),
        "every trailing counter strips; stripping stops at (Live)"
    );
}

// ---------------------------------------------------------------------
// derive_upload_query: malformed / empty inputs derive nothing
// ---------------------------------------------------------------------

#[test]
fn empty_and_identity_free_subjects_derive_no_query() {
    assert_eq!(derive_upload_query(&[]), None, "no subjects at all");
    assert_eq!(derive_upload_query(&subjects("")), None, "empty subject");
    assert_eq!(
        derive_upload_query(&subjects("   ")),
        None,
        "whitespace-only subject"
    );
    assert_eq!(
        derive_upload_query(&subjects(r#""only.flac" yEnc"#)),
        None,
        "nothing before the quoted file name"
    );
    assert_eq!(
        derive_upload_query(&subjects(r#"[01/05] - "01.flac""#)),
        None,
        "a counter alone is not a release identity"
    );
    assert_eq!(
        derive_upload_query(&subjects(r#"A [01/05] - "01.flac""#)),
        None,
        "one surviving character is under the query length floor"
    );
    assert_eq!(
        derive_upload_query(&subjects("Ar\u{1}tist - Album [01/05]")),
        None,
        "a control character invalidates the derived query"
    );
}

// ---------------------------------------------------------------------
// derive_upload_query -> pick_release_mbid: never the studio release
// ---------------------------------------------------------------------

#[test]
fn live_subject_picks_the_live_release_not_the_studio_one() {
    let query = derive_upload_query(&subjects(
        r#"Artist - Album (Live) [01/05] - "01.flac" yEnc (1/3)"#,
    ))
    .expect("live subject must derive a query");
    // Studio hit deliberately scores higher: the verified
    // "Artist - Album" path must still never land on it.
    let hits = vec![studio_hit(), live_hit()];
    assert_eq!(
        pick_release_mbid(&query, &hits).as_deref(),
        Some(LIVE),
        "query {query:?} must select the live release, never the studio one"
    );
    let hits_rev = vec![live_hit(), studio_hit()];
    assert_eq!(
        pick_release_mbid(&query, &hits_rev).as_deref(),
        Some(LIVE),
        "the live pick must not depend on wire order"
    );
}

#[test]
fn live_subject_with_only_a_studio_candidate_stays_unmatched() {
    let query = derive_upload_query(&subjects(r#"Artist - Album (Live) [01/05] - "01.flac""#))
        .expect("live subject must derive a query");
    assert_eq!(
        pick_release_mbid(&query, &[studio_hit()]),
        None,
        "query {query:?}: no live candidate means unmatched, not studio"
    );
}

#[test]
fn live_subject_with_unverifiable_live_hit_stays_unmatched() {
    let query = derive_upload_query(&subjects(r#"Artist - Album (Live) [01/05] - "01.flac""#))
        .expect("live subject must derive a query");
    // The live hit carries no artist to verify against, so it cannot
    // be picked — and the verifiable studio hit must not take its place.
    let hits = vec![studio_hit(), hit(LIVE, "Album (Live)", None, 0.8)];
    assert_eq!(pick_release_mbid(&query, &hits), None);
}

#[test]
fn live_subject_with_two_live_releases_stays_unmatched() {
    let query = derive_upload_query(&subjects(r#"Artist - Album (Live) [01/05] - "01.flac""#))
        .expect("live subject must derive a query");
    let hits = vec![
        studio_hit(),
        live_hit(),
        hit(LIVE_ALT, "Album (Live)", Some("Artist"), 0.79),
    ];
    assert_eq!(
        pick_release_mbid(&query, &hits),
        None,
        "two distinct live releases are ambiguous: unmatched, not studio"
    );
}

#[test]
fn live_subject_with_malformed_live_mbid_stays_unmatched() {
    let query = derive_upload_query(&subjects(r#"Artist - Album (Live) [01/05] - "01.flac""#))
        .expect("live subject must derive a query");
    let hits = vec![
        studio_hit(),
        hit("not-an-mbid", "Album (Live)", Some("Artist"), 0.8),
    ];
    assert_eq!(
        pick_release_mbid(&query, &hits),
        None,
        "an invalid live MBID is filtered; the studio hit must not inherit"
    );
}

#[test]
fn deluxe_subject_picks_the_deluxe_release_not_the_studio_one() {
    let query = derive_upload_query(&subjects(r#"Artist - Album (Deluxe) (1/3) - "01.flac""#))
        .expect("deluxe subject must derive a query");
    let hits = vec![
        studio_hit(),
        hit(OTHER, "Album (Deluxe)", Some("Artist"), 0.8),
    ];
    assert_eq!(
        pick_release_mbid(&query, &hits).as_deref(),
        Some(OTHER),
        "query {query:?} must select the deluxe release"
    );
}

#[test]
fn remastered_plain_title_subject_picks_the_remaster_not_the_plain_album() {
    // No "Artist - " shape: the scored plain-title path must compare
    // against the full "Album [Remastered]" title, so the higher
    // scoring plain "Album" hit is not an exact-title candidate.
    let query = derive_upload_query(&subjects(r#"Album [Remastered] [01/05] - "01.flac""#))
        .expect("remastered subject must derive a query");
    let hits = vec![
        hit(STUDIO, "Album", None, 0.99),
        hit(OTHER, "Album (Remastered)", None, 0.8),
    ];
    assert_eq!(
        pick_release_mbid(&query, &hits).as_deref(),
        Some(OTHER),
        "query {query:?} must select the remastered release"
    );
}

#[test]
fn counter_only_studio_subject_still_picks_the_studio_release() {
    // Guard: preserving editions must not break the ordinary path.
    let query = derive_upload_query(&subjects(
        r#"Artist - Album [01/05] - "01.flac" yEnc (1/3)"#,
    ))
    .expect("counter-only subject must derive a query");
    let hits = vec![live_hit(), studio_hit()];
    assert_eq!(
        pick_release_mbid(&query, &hits).as_deref(),
        Some(STUDIO),
        "query {query:?} is the plain studio identity and picks studio"
    );
}

// ---------------------------------------------------------------------
// pick_release_mbid directly: the pick itself stays exact (guards)
// ---------------------------------------------------------------------

#[test]
fn edition_query_never_matches_a_studio_title_directly() {
    assert_eq!(
        pick_release_mbid("Artist - Album (Live)", &[studio_hit()]),
        None,
        "the correction must not loosen the pick into fuzzy matching"
    );
    assert_eq!(
        pick_release_mbid("Artist - Album (Live)", &[studio_hit(), live_hit()]).as_deref(),
        Some(LIVE)
    );
}
