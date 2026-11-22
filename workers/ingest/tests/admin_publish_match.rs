use nntune_ingest::matching::{
    admin_publish_match::{
        decide_publish_match, release_lookup, select_release, CompleteReleaseCandidates,
        FileNumbering as Numbers, NeedsReview, NeedsReview::*, PublishMatch, ReleaseCandidate,
    },
    derive_upload_query, MbSearchHit,
};
use serde_json::{json, Value};

const RELEASE: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const OTHER: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const TRACK: &str = "cccccccc-3333-4333-8333-333333333333";
const ALBUM: &str = "Tin Skies";

fn hit(id: &str, title: &str, score: f32) -> MbSearchHit {
    MbSearchHit {
        mbid: id.into(),
        title: title.into(),
        score,
        artist_name: Some("Paper Moons".into()),
        artist_mbid: None,
    }
}

fn select(query: &str, hits: Vec<MbSearchHit>) -> Result<ReleaseCandidate, NeedsReview> {
    let lookup = release_lookup(query)?;
    let evidence = CompleteReleaseCandidates {
        lookup: &lookup,
        hits,
    };
    select_release(query, Some(evidence))
}

fn listing(positions: &[(i16, i16)]) -> Value {
    let rows: Vec<_> = positions
        .iter()
        .map(|&(disc, track)| {
            json!({
                "release_mbid": RELEASE, "mbid": TRACK, "title": "Synthetic Signal",
                "medium_no": disc, "track_no": track
            })
        })
        .collect();
    json!({"release_mbid": RELEASE, "recordings": rows})
}

fn decide(
    files: &[(Option<&str>, &str)],
    count: usize,
    rows: Value,
) -> Result<PublishMatch, NeedsReview> {
    let release = select(ALBUM, vec![hit(RELEASE, ALBUM, 1.0)]).unwrap();
    let files: Vec<_> = files
        .iter()
        .map(|&(disc, track)| Numbers {
            disc,
            track: (!track.is_empty()).then_some(track),
        })
        .collect();
    decide_publish_match(release, count, &files, 200, &rows.to_string())
}

#[test]
fn exact_release_and_numbered_occurrences_are_publish_eligible() {
    let single = decide(&[(None, "01"), (None, "2")], 2, listing(&[(1, 1), (1, 2)])).unwrap();
    assert_eq!(single.release_mbid(), RELEASE);
    let files = [(Some("1"), "1"), (Some("1"), "2"), (Some("2"), "1")];
    let multi = decide(&files, 3, listing(&[(1, 1), (1, 2), (2, 1)])).unwrap();
    assert_eq!(multi.recordings().len(), 3);
    assert_eq!(multi.recordings()[2].medium_no, Some(2));
}

#[test]
fn release_pick_preserves_confidence_and_margin_boundaries() {
    // Inject f32 boundary scores at the matcher seam. These are distinct
    // spellings of the same normalized title.
    for (score, runner, matches) in [
        (f32::from_bits(0.6_f32.to_bits() - 1), None, false),
        (0.6, None, true),
        (0.6, Some(f32::from_bits(0.5_f32.to_bits() + 1)), false),
        (0.6, Some(0.5), true),
    ] {
        let mut hits = vec![hit(RELEASE, ALBUM, score)];
        hits.extend(runner.map(|score| hit(OTHER, "Tin Sk'ies", score)));
        let result = select(ALBUM, hits);
        let actual = result.as_ref().map(ReleaseCandidate::mbid).map_err(|e| *e);
        assert_eq!(actual, matches.then_some(RELEASE).ok_or(ReleaseUnresolved));
    }
}

#[test]
fn cleaned_subject_keeps_edition_and_requires_unique_exact_artist_album() {
    let subjects = [r#"Paper.Moons.-.Tin.Skies.(Live).[01/05].-."01.flac" yEnc (1/3)"#.to_owned()];
    let query = derive_upload_query(&subjects).unwrap();
    assert_eq!(query, "Paper Moons - Tin Skies (Live)");
    let live = hit(RELEASE, "Tin Skies (Live)", 1.0);
    let rival = hit(OTHER, "Tin Sk'ies (Live)", 0.2);
    assert_eq!(select(&query, vec![live.clone()]).unwrap().mbid(), RELEASE);
    assert_eq!(
        select(&query, vec![live, rival]).unwrap_err(),
        ReleaseUnresolved
    );
}

#[test]
fn inconsistent_search_evidence_returns_a_fixed_reason() {
    let h = hit(RELEASE, ALBUM, 1.0);
    let lookup = release_lookup("source-subject-canary").unwrap();
    let wrong = CompleteReleaseCandidates {
        lookup: &lookup,
        hits: vec![h.clone()],
    };
    for evidence in [None, Some(wrong)] {
        let reason = select_release(ALBUM, evidence).unwrap_err();
        assert_eq!(serde_json::to_value(reason).unwrap(), "search_evidence");
    }
    for (score, reason) in [
        (1.0, ReleaseUnresolved),
        (0.8, SearchEvidence),
        (f32::NAN, SearchEvidence),
    ] {
        let rival = hit(OTHER, ALBUM, score);
        assert_eq!(select(ALBUM, vec![h.clone(), rival]).unwrap_err(), reason);
    }
}

#[test]
fn unproven_numbering_count_or_topology_requires_review() {
    let one = &[(None, "1")];
    let rows = listing(&[(1, 1)]);
    for track in ["", "1.0", "+1", "0", "32768", "１"] {
        let result = decide(&[(None, track)], 1, rows.clone());
        assert_eq!(result.err(), Some(FileNumbering));
    }
    assert_eq!(decide(one, 2, rows).err(), Some(CountMismatch));
    assert_eq!(decide(one, 1, listing(&[])).err(), Some(CountMismatch));
    let single = [(None, "1"), (None, "2")];
    let two_discs = [(Some("1"), "1"), (Some("2"), "1")];
    for (files, positions) in [
        ([(None, "1"), (None, "1")], [(1, 1), (1, 2)]),
        ([(None, "2"), (None, "1")], [(1, 1), (1, 2)]),
        (single, [(1, 1), (1, 1)]),
        (single, [(1, 1), (2, 1)]),
        (two_discs, [(1, 1), (3, 1)]),
        (two_discs, [(1, 1), (2, 2)]),
    ] {
        let result = decide(&files, 2, listing(&positions));
        assert_eq!(result.err(), Some(PositionMismatch));
    }
}

#[test]
fn incomplete_or_foreign_recordings_require_review() {
    let rows = listing(&[(1, 1)]);
    let foreign = json!(OTHER);
    for (pointer, value, reason) in [
        ("/release_mbid", foreign.clone(), RecordingEvidence),
        ("/recordings/0/release_mbid", foreign, RecordingEvidence),
        ("/recordings/0/mbid", json!("invalid"), RecordingEvidence),
        ("/recordings/0/medium_no", Value::Null, PositionMismatch),
        ("/recordings/0/track_no", json!("1"), RecordingEvidence),
    ] {
        let mut invalid = rows.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert_eq!(decide(&[(None, "1")], 1, invalid).err(), Some(reason));
    }
}

#[test]
fn saturated_recording_listing_fails_even_when_exact_counts_agree() {
    let numbers: Vec<_> = (1..=1000).map(|n| n.to_string()).collect();
    let files: Vec<_> = numbers.iter().map(|n| (None, n.as_str())).collect();
    let positions: Vec<_> = (1..=1000).map(|n| (1, n)).collect();
    assert!(decide(&files[..999], 999, listing(&positions[..999])).is_ok());
    let result = decide(&files, 1000, listing(&positions));
    assert_eq!(result.err(), Some(RecordingEvidence));
}
