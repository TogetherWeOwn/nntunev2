use nntune_ingest::matching::{
    admin_publish_match::{
        decide_publish_match, select_release, FileNumbering as Numbers, NeedsReview,
        NeedsReview::*, PublishMatch,
    },
    derive_upload_query,
};
use serde_json::{json, Value};

const RELEASE: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const OTHER: &str = "bbbbbbbb-2222-4222-8222-222222222222";
const TRACK: &str = "cccccccc-3333-4333-8333-333333333333";
const ALBUM: &str = "Tin Skies";

fn hit(id: &str, title: &str, score: f32) -> Value {
    json!({"mbid": id, "title": title, "score": score, "artist_name": "Paper Moons"})
}

fn page(q: &str, hits: Vec<Value>) -> String {
    json!({"q": q, "type": "release", "limit": 25, "offset": 0, "results": hits}).to_string()
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
    let search = page("tin skies", vec![hit(RELEASE, ALBUM, 0.6)]);
    let release = select_release(ALBUM, 200, &search).unwrap();
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
    for (score, runner, expected) in [
        (
            f32::from_bits(0.6_f32.to_bits() - 1),
            None,
            Err(ReleaseUnresolved),
        ),
        (0.6, None, Ok(RELEASE)),
        (0.7, Some(0.6), Err(ReleaseUnresolved)),
        (
            0.6,
            Some(f32::from_bits(0.5_f32.to_bits() + 1)),
            Err(ReleaseUnresolved),
        ),
        (0.6, Some(0.5), Ok(RELEASE)),
    ] {
        let mut hits = vec![hit(RELEASE, ALBUM, score)];
        hits.extend(runner.map(|score| hit(OTHER, ALBUM, score)));
        let result = select_release(ALBUM, 200, &page("tin skies", hits));
        assert_eq!(result.as_ref().map(|r| r.mbid()).map_err(|e| *e), expected);
    }
}

#[test]
fn cleaned_subject_keeps_edition_and_requires_unique_exact_artist_album() {
    let subjects = [r#"Paper.Moons.-.Tin.Skies.(Live).[01/05].-."01.flac" yEnc (1/3)"#.to_owned()];
    let query = derive_upload_query(&subjects).unwrap();
    assert_eq!(query, "Paper Moons - Tin Skies (Live)");
    let base = hit(RELEASE, "Tin Skies (Live)", 0.3);
    let studio = hit(OTHER, ALBUM, 1.0);
    let edition = hit(OTHER, "Tin Skies (Live)", 1.0);
    for (hits, expected) in [
        (vec![studio.clone(), base.clone()], Ok(RELEASE)),
        (vec![studio], Err(ReleaseUnresolved)),
        (vec![base, edition], Err(ReleaseUnresolved)),
    ] {
        let result = select_release(&query, 200, &page("tin skies (live)", hits));
        assert_eq!(result.as_ref().map(|r| r.mbid()).map_err(|e| *e), expected);
    }
}

#[test]
fn inconsistent_search_evidence_returns_a_fixed_reason() {
    let h = hit(RELEASE, ALBUM, 0.9);
    for body in [
        page("source-subject-canary", vec![h.clone()]),
        page("tin skies", vec![h.clone(), hit(RELEASE, ALBUM, 0.8)]),
        page("tin skies", vec![h; 25]),
    ] {
        let reason = select_release(ALBUM, 200, &body).unwrap_err();
        assert_eq!(serde_json::to_value(reason).unwrap(), "search_evidence");
    }
}

#[test]
fn unproven_numbering_count_or_topology_requires_review() {
    let one = &[(None, "1")];
    let rows = listing(&[(1, 1)]);
    for track in ["", "1.0", "+1", "0", "32768", "１"] {
        let result = decide(&[(None, track)], 1, rows.clone());
        assert_eq!(result.unwrap_err(), FileNumbering);
    }
    assert_eq!(decide(one, 2, rows).unwrap_err(), CountMismatch);
    assert_eq!(decide(one, 1, listing(&[])).unwrap_err(), CountMismatch);
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
        assert_eq!(
            decide(&files, 2, listing(&positions)).unwrap_err(),
            PositionMismatch
        );
    }
}

#[test]
fn incomplete_or_foreign_recordings_require_review() {
    let rows = listing(&[(1, 1)]);
    for (pointer, value, reason) in [
        ("/release_mbid", json!(OTHER), RecordingEvidence),
        (
            "/recordings/0/release_mbid",
            json!(OTHER),
            RecordingEvidence,
        ),
        ("/recordings/0/mbid", json!("invalid"), RecordingEvidence),
        ("/recordings/0/medium_no", Value::Null, PositionMismatch),
        ("/recordings/0/track_no", json!("1"), RecordingEvidence),
    ] {
        let mut invalid = rows.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert_eq!(decide(&[(None, "1")], 1, invalid).unwrap_err(), reason);
    }
}
