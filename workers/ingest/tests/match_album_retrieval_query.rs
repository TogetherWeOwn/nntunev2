//! Album-title retrieval without losing artist verification (tests-only).
//!
//! Contract under test (`workers/ingest/src/matching.rs`,
//! `mb_search_url` for retrieval + `pick_release_mbid` for the pick;
//! `workers/ingest/src/queue_consumer.rs` passes the *same* original
//! query to both):
//!
//! - The MB release search is a title-only predicate. For the
//!   already-recognized `Artist - Album` shape (including the
//!   typographic-dash forms the pick already recognizes), the `q=`
//!   retrieval text sent to MB is therefore the album title alone —
//!   edition text such as `(Live)` / `[Remastered]` included — so a
//!   long artist name cannot push a short correct title below the
//!   retrieval similarity bar before verification ever runs.
//! - Verification loses nothing: `pick_release_mbid` still receives
//!   the original `Artist - Album` identity, so a title-only retrieval
//!   can never accept a release on title alone. Wrong-artist hits,
//!   hits with no artist to verify, and several distinct releases
//!   matching the full identity all stay unmatched (`None`).
//! - Plain-title queries (no recognized `Artist - Album` shape) are
//!   sent unchanged.
//! - Whatever is sent still respects the MB query contract enforced
//!   by `validate_match_query` (2..=100 chars, no control characters)
//!   and is percent-encoded in the URL. An album side that would by
//!   itself be an invalid MB query (e.g. a one-character title) is
//!   never sent as-is.
//!
//! The end-to-end tests drive a native fake of the title-only MB
//! search: candidates are returned when the trigram similarity
//! between the *sent* `q=` text and the release *title* clears 0.3
//! (the pg_trgm-style bar the real predicate uses), scored by that
//! similarity, exactly as a title-only mirror would. The pick then
//! runs on the original identity, mirroring the queue caller.
//!
//! Sensitivity: the baseline sends the entire `Artist - Album`
//! identity as `q=`, so every album-extraction assertion fails, and
//! in the fake search an artist-heavy identity retrieves nothing for
//! a short album that exists in the catalogue. Plain-title,
//! encoding, validity and pick-strictness guards already pass and
//! pin behavior the correction must not regress. Assertions compare
//! the normalized retrieval text (lowercased, non-alphanumeric runs
//! collapsed — the pick's own comparison), so sending the album side
//! verbatim or normalized both satisfy them; sending any part of the
//! artist side, or dropping edition words, does not.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/ingest/Cargo.toml \
//!     --test match_album_retrieval_query
//! ```

use std::collections::HashSet;

use nntune_ingest::matching::{
    derive_upload_query, mb_search_url, pick_release_mbid, validate_match_query, MbSearchHit,
};

const CORRECT: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const CORRECT_ALT: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const WRONG_ARTIST: &str = "cccccccc-3333-4333-8333-333333333333";
const STUDIO: &str = "dddddddd-4444-4444-8444-444444444444";
const LIVE: &str = "eeeeeeee-5555-4555-8555-555555555555";

/// A deliberately artist-heavy identity: a long artist name and a
/// two-character album, still inside the 100-char MB query budget.
const LONG_ARTIST: &str = "The International Noise Conspiracy Orchestra Collective";
const LONG_ARTIST_GO: &str = "The International Noise Conspiracy Orchestra Collective - Go";

fn hit(mbid: &str, title: &str, artist_name: Option<&str>, score: f32) -> MbSearchHit {
    MbSearchHit {
        mbid: mbid.to_string(),
        title: title.to_string(),
        score,
        artist_mbid: None,
        artist_name: artist_name.map(str::to_string),
    }
}

fn hex_val(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => panic!("invalid percent-encoding in URL"),
    }
}

/// The decoded `q=` retrieval text `mb_search_url` sends for `query`.
fn retrieval_q(query: &str) -> String {
    let url = mb_search_url(query);
    let encoded = url
        .strip_prefix("https://mb-api.invalid/v1/mb/search?type=release&q=")
        .unwrap_or_else(|| panic!("search URL must pin type=release and carry q=: {url}"));
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(hex_val(bytes[i + 1]) * 16 + hex_val(bytes[i + 2]));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("decoded q= must be UTF-8")
}

/// The pick's comparison form: lowercased, every non-alphanumeric run
/// collapsed to single spaces.
fn normalized(raw: &str) -> String {
    raw.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// pg_trgm-style trigram set: each word padded with two leading and
/// one trailing space before slicing into 3-char grams.
fn trigrams(normalized_text: &str) -> HashSet<String> {
    let mut grams = HashSet::new();
    for word in normalized_text.split_whitespace() {
        let padded: Vec<char> = format!("  {word} ").chars().collect();
        for window in padded.windows(3) {
            grams.insert(window.iter().collect());
        }
    }
    grams
}

fn trigram_similarity(a: &str, b: &str) -> f32 {
    let (ga, gb) = (trigrams(&normalized(a)), trigrams(&normalized(b)));
    if ga.is_empty() || gb.is_empty() {
        return 0.0;
    }
    let shared = ga.intersection(&gb).count() as f32;
    let union = ga.union(&gb).count() as f32;
    shared / union
}

/// Fake title-only MB search: a catalogue release is a candidate only
/// when the sent retrieval text is similar enough to its *title*
/// (never to artist + title), and it is scored by that similarity.
fn fake_title_search(sent_q: &str, catalogue: &[MbSearchHit]) -> Vec<MbSearchHit> {
    catalogue
        .iter()
        .filter_map(|entry| {
            let score = trigram_similarity(sent_q, &entry.title);
            (score >= 0.3).then(|| {
                let mut found = entry.clone();
                found.score = score;
                found
            })
        })
        .collect()
}

/// The queue caller's path in miniature: retrieve with the URL built
/// from `original`, then pick against the *original* identity.
fn retrieve_then_pick(
    original: &str,
    catalogue: &[MbSearchHit],
) -> (Vec<MbSearchHit>, Option<String>) {
    let sent = retrieval_q(original);
    let hits = fake_title_search(&sent, catalogue);
    let picked = pick_release_mbid(original, &hits);
    (hits, picked)
}

fn retrieved_mbids(hits: &[MbSearchHit]) -> Vec<&str> {
    hits.iter().map(|h| h.mbid.as_str()).collect()
}

// ---------------------------------------------------------------------
// mb_search_url: the retrieval text for Artist - Album is album-only
// ---------------------------------------------------------------------

#[test]
fn artist_heavy_identity_retrieves_by_short_album_title_alone() {
    assert!(
        validate_match_query(LONG_ARTIST_GO).is_some(),
        "fixture must be a query the caller would actually send"
    );
    let sent = retrieval_q(LONG_ARTIST_GO);
    assert_eq!(
        normalized(&sent),
        "go",
        "retrieval q={sent:?}: a title-only search must be sent the album \
         title, not the whole Artist - Album identity"
    );
}

#[test]
fn ordinary_artist_album_identity_retrieves_by_album_title() {
    let sent = retrieval_q("Pink Floyd - Animals");
    assert_eq!(normalized(&sent), "animals", "retrieval q={sent:?}");
}

#[test]
fn album_extraction_preserves_edition_text() {
    for (query, expected) in [
        (
            "The International Noise Conspiracy Orchestra Collective - Go (Live)",
            "go live",
        ),
        ("Artist - Album (Deluxe Edition)", "album deluxe edition"),
        ("Artist - Album [Remastered]", "album remastered"),
    ] {
        let sent = retrieval_q(query);
        assert_eq!(
            normalized(&sent),
            expected,
            "query {query:?}: retrieval q={sent:?} must keep the edition \
             words — dropping them would retrieve the wrong release family"
        );
    }
}

#[test]
fn typographic_dash_identity_retrieves_by_album_title() {
    // The pick already recognizes en/em dashes and the minus sign as
    // the separator, so retrieval must extract for those shapes too.
    for query in [
        "The International Noise Conspiracy Orchestra Collective \u{2013} Go",
        "The International Noise Conspiracy Orchestra Collective \u{2014} Go",
        "The International Noise Conspiracy Orchestra Collective \u{2212} Go",
    ] {
        let sent = retrieval_q(query);
        assert_eq!(
            normalized(&sent),
            "go",
            "query {query:?}: retrieval q={sent:?}"
        );
    }
}

#[test]
fn self_titled_identity_retrieves_by_album_title_once() {
    let sent = retrieval_q("Metallica - Metallica");
    assert_eq!(
        normalized(&sent),
        "metallica",
        "retrieval q={sent:?}: the album title once, not artist + album"
    );
}

#[test]
fn remainder_after_first_separator_is_the_album_title() {
    // "Artist - Album - Subtitle": the pick treats everything after
    // the first " - " as the album side; retrieval must match it or
    // the verified title can never be retrieved.
    let sent = retrieval_q("Some Long Artist Name - Album - The Subtitle");
    assert_eq!(
        normalized(&sent),
        "album the subtitle",
        "retrieval q={sent:?}"
    );
}

#[test]
fn extracted_retrieval_text_is_a_valid_mb_query() {
    for query in [
        LONG_ARTIST_GO,
        "Pink Floyd - Animals",
        "Artist - Album (Live)",
        "Metallica - Metallica",
        "A - Go",
    ] {
        let sent = retrieval_q(query);
        assert!(
            validate_match_query(&sent).is_some(),
            "query {query:?}: retrieval q={sent:?} must satisfy the MB \
             2..=100-char / no-control contract"
        );
    }
}

#[test]
fn one_character_album_title_is_never_sent_as_an_invalid_query() {
    // "X" alone is under the MB 2-char floor: extraction must not
    // produce a retrieval text the MB worker would reject with 400.
    // Falling back to the (caller-validated) full identity is the
    // honest outcome; sending "X" is not.
    let sent = retrieval_q("The International Noise Conspiracy Orchestra Collective - X");
    assert!(
        validate_match_query(&sent).is_some(),
        "retrieval q={sent:?} must not be an invalid extracted query"
    );
    assert_ne!(
        normalized(&sent),
        "x",
        "retrieval q={sent:?}: the bare one-character album must not be sent"
    );
}

#[test]
fn extracted_album_retrieval_stays_percent_encoded() {
    // Album side carrying query-syntax characters: whatever album
    // text is extracted, it must reach the URL encoded — no raw
    // space, '&', '?' or '#' may leak into the q= value.
    let url = mb_search_url("Long Artist Name Here - Rock & Roll? #1");
    let encoded = url.rsplit("q=").next().expect("url carries q=");
    for raw in [' ', '&', '?', '#'] {
        assert!(
            !encoded.contains(raw),
            "q= value {encoded:?} in {url} must not contain a raw {raw:?}"
        );
    }
    let sent = retrieval_q("Long Artist Name Here - Rock & Roll? #1");
    assert_eq!(
        normalized(&sent),
        "rock roll 1",
        "retrieval q={sent:?} must be the album side even when it needs encoding"
    );
}

// ---------------------------------------------------------------------
// mb_search_url: plain titles are unchanged (guards)
// ---------------------------------------------------------------------

#[test]
fn plain_title_search_is_sent_unchanged() {
    for query in ["Animals", "dark side & more?", "Spider-Man", "21"] {
        assert_eq!(
            retrieval_q(query),
            query,
            "plain-title query {query:?} has no Artist - Album shape to extract"
        );
    }
    // The long-standing exact encoding for a plain title still holds.
    assert!(
        mb_search_url("dark side & more?").ends_with("dark%20side%20%26%20more%3F"),
        "plain-title percent-encoding must not change"
    );
}

// ---------------------------------------------------------------------
// Fake title-only retrieval + pick on the original identity
// ---------------------------------------------------------------------

#[test]
fn artist_heavy_query_retrieves_the_short_album_and_verifies_the_artist() {
    let catalogue = vec![
        hit(CORRECT, "Go", Some(LONG_ARTIST), 0.0),
        hit(WRONG_ARTIST, "Go", Some("Someone Else Entirely"), 0.0),
        hit(STUDIO, "Something Longer", Some(LONG_ARTIST), 0.0),
    ];
    let (hits, picked) = retrieve_then_pick(LONG_ARTIST_GO, &catalogue);
    assert!(
        retrieved_mbids(&hits).contains(&CORRECT),
        "the correct short album exists in the mirror and must be \
         retrieved; got {hits:?}"
    );
    assert_eq!(
        picked.as_deref(),
        Some(CORRECT),
        "with the candidate retrieved, the original Artist - Album \
         verification must pick it (and not the wrong-artist namesake)"
    );
}

#[test]
fn retrieved_wrong_artist_namesake_still_stays_unmatched() {
    let catalogue = vec![hit(WRONG_ARTIST, "Go", Some("Someone Else Entirely"), 0.0)];
    let (hits, picked) = retrieve_then_pick(LONG_ARTIST_GO, &catalogue);
    assert!(
        retrieved_mbids(&hits).contains(&WRONG_ARTIST),
        "title-only retrieval should surface the namesake; got {hits:?}"
    );
    assert_eq!(
        picked, None,
        "a title match by the wrong artist must never be accepted: \
         retrieval widened, verification did not"
    );
}

#[test]
fn retrieved_hit_without_an_artist_still_stays_unmatched() {
    let catalogue = vec![hit(CORRECT, "Go", None, 0.0)];
    let (hits, picked) = retrieve_then_pick(LONG_ARTIST_GO, &catalogue);
    assert!(
        retrieved_mbids(&hits).contains(&CORRECT),
        "title-only retrieval should surface the hit; got {hits:?}"
    );
    assert_eq!(
        picked, None,
        "a hit with no artist to verify cannot be accepted on title alone"
    );
}

#[test]
fn two_distinct_releases_for_the_identity_still_stay_unmatched() {
    let catalogue = vec![
        hit(CORRECT, "Go", Some(LONG_ARTIST), 0.0),
        hit(CORRECT_ALT, "Go", Some(LONG_ARTIST), 0.0),
        hit(WRONG_ARTIST, "Go", Some("Someone Else Entirely"), 0.0),
    ];
    let (hits, picked) = retrieve_then_pick(LONG_ARTIST_GO, &catalogue);
    let ids = retrieved_mbids(&hits);
    assert!(
        ids.contains(&CORRECT) && ids.contains(&CORRECT_ALT),
        "both same-artist releases should be retrieved; got {hits:?}"
    );
    assert_eq!(
        picked, None,
        "several distinct releases match the full identity: ambiguous \
         stays unmatched rather than guessing by title score"
    );
}

#[test]
fn edition_identity_retrieves_and_picks_the_edition_not_the_studio_release() {
    let original = "The International Noise Conspiracy Orchestra Collective - Go (Live)";
    let catalogue = vec![
        hit(STUDIO, "Go", Some(LONG_ARTIST), 0.0),
        hit(LIVE, "Go (Live)", Some(LONG_ARTIST), 0.0),
    ];
    let (hits, picked) = retrieve_then_pick(original, &catalogue);
    assert!(
        retrieved_mbids(&hits).contains(&LIVE),
        "the live release must be retrieved for a live identity; got {hits:?}"
    );
    assert_eq!(
        picked.as_deref(),
        Some(LIVE),
        "edition-preserving retrieval + full-identity verification must \
         land on the live release, never the studio one"
    );
}

#[test]
fn upload_subject_identity_retrieves_by_album_and_verifies_by_artist() {
    // The upload path end to end: the NZB subject derives the stored
    // Artist - Album identity (unchanged), that identity's album side
    // retrieves, and the full identity verifies.
    let subjects = vec![format!(
        r#"{LONG_ARTIST_GO} [01/05] - "01.flac" yEnc (1/3)"#
    )];
    let original = derive_upload_query(&subjects).expect("upload subject must derive an identity");
    assert_eq!(
        original, LONG_ARTIST_GO,
        "the persisted identity itself must not be rewritten to the album alone"
    );
    let catalogue = vec![
        hit(CORRECT, "Go", Some(LONG_ARTIST), 0.0),
        hit(WRONG_ARTIST, "Go", Some("Someone Else Entirely"), 0.0),
    ];
    let (hits, picked) = retrieve_then_pick(&original, &catalogue);
    assert!(
        retrieved_mbids(&hits).contains(&CORRECT),
        "the uploaded album must be retrieved; got {hits:?}"
    );
    assert_eq!(picked.as_deref(), Some(CORRECT));
}

#[test]
fn plain_title_retrieval_and_pick_are_unchanged() {
    let catalogue = vec![
        hit(CORRECT, "Animals", Some("Pink Floyd"), 0.0),
        hit(STUDIO, "Something Longer", None, 0.0),
    ];
    let (hits, picked) = retrieve_then_pick("Animals", &catalogue);
    assert!(retrieved_mbids(&hits).contains(&CORRECT), "got {hits:?}");
    assert_eq!(
        picked.as_deref(),
        Some(CORRECT),
        "the scored plain-title path must behave exactly as before"
    );
}
