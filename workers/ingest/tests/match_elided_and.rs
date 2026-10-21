//! Elided `n` remains a separate word when apostrophes are normalized.

use nntune_ingest::matching::{pick_release_mbid, MbSearchHit};

const MBID_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const MBID_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";

const TITLES: &[&str] = &[
    "Rock'n'Roll",
    "Rock 'n' Roll",
    "Rock’n’Roll",
    "Rock ’n’ Roll",
    "Rock'n Roll",
    "Rock n'Roll",
    "Rock’n Roll",
    "Rock n’Roll",
    "Rock N Roll",
    "ROCK'N’ROLL",
    "Rock\t'n'\nRoll",
    "Rock-'n'-Roll",
];

fn hit(mbid: &str, title: &str, artist: Option<&str>) -> MbSearchHit {
    MbSearchHit {
        mbid: mbid.to_string(),
        title: title.to_string(),
        score: 0.99,
        artist_mbid: None,
        artist_name: artist.map(str::to_string),
    }
}

#[test]
fn elided_and_title_spellings_match_in_both_directions() {
    for query in TITLES {
        for title in TITLES {
            assert_eq!(
                pick_release_mbid(query, &[hit(MBID_A, title, None)]).as_deref(),
                Some(MBID_A),
                "query {query:?}, title {title:?}"
            );
        }
    }
}

#[test]
fn elided_and_artist_spellings_match_in_both_directions() {
    let artists = [
        "Guns'n'Roses",
        "Guns 'n' Roses",
        "Guns’n’Roses",
        "Guns ’n’ Roses",
        "Guns'n Roses",
        "Guns n'Roses",
        "Guns N’ Roses",
        "Guns N Roses",
        "GUNS’N'ROSES",
    ];
    for query in artists {
        for artist in artists {
            assert_eq!(
                pick_release_mbid(
                    &format!("{query} - Appetite"),
                    &[hit(MBID_A, "Appetite", Some(artist))],
                )
                .as_deref(),
                Some(MBID_A),
                "query artist {query:?}, hit artist {artist:?}"
            );
        }
    }
}

#[test]
fn elided_and_does_not_match_collapsed_words() {
    for spelling in TITLES {
        for (query, title) in [(*spelling, "RockNRoll"), ("RockNRoll", *spelling)] {
            assert_eq!(pick_release_mbid(query, &[hit(MBID_A, title, None)]), None);
            assert_eq!(
                pick_release_mbid(
                    &format!("{query} - Appetite"),
                    &[hit(MBID_A, "Appetite", Some(title))],
                ),
                None
            );
        }
    }
}

#[test]
fn elided_and_variants_remain_ambiguous_between_distinct_releases() {
    let hits = [
        hit(MBID_A, "Rock'n'Roll", Some("Guns'n'Roses")),
        hit(MBID_B, "Rock 'n' Roll", Some("Guns N’ Roses")),
    ];
    for query in ["Guns'n'Roses - Rock'n'Roll", "Guns N’ Roses - Rock N Roll"] {
        assert_eq!(pick_release_mbid(query, &hits), None);
        let reversed = [hits[1].clone(), hits[0].clone()];
        assert_eq!(pick_release_mbid(query, &reversed), None);
    }
}

#[test]
fn ordinary_apostrophes_still_join_contractions() {
    for (query, title) in [
        ("Don't Stop", "Dont Stop"),
        ("Can't Sing", "Cant Sing"),
        ("N'Djamena", "Ndjamena"),
        ("Rock'nroll", "Rocknroll"),
        ("Rockn'roll", "Rocknroll"),
        ("Élan’s", "Élans"),
    ] {
        assert_eq!(
            pick_release_mbid(query, &[hit(MBID_A, title, None)]).as_deref(),
            Some(MBID_A),
            "query {query:?}, title {title:?}"
        );
    }
}
