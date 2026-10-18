//! Acceptance: MB 404s retry instead of DLQ-ing matches / wiping recordings.
//!
//! Protected Test Author surface — the paired builder must not edit this
//! file. Synthetic statuses and bodies only; no MB network, database, or
//! provider connection.
//!
//! ```sh
//! cargo test --locked --manifest-path workers/ingest/Cargo.toml --test mb_404_transient
//! ```
//!
//! Why 404 is special on this wire (spec, refill-33 M3):
//!
//! - Search: a legitimate no-match is `200` with an empty `results` list,
//!   never a 404. A search 404 can only be a route miss / version skew
//!   (e.g. a rolling MB deploy that has no search route yet), so it must
//!   classify `Transient` (retry with backoff) — never `Permanent`, which
//!   the queue consumer maps to an immediate DLQ with zero retries.
//! - Recordings: the MB worker renders the *same* body
//!   `{"code":"not_found","message":"not found","retryable":false}` for a
//!   route miss and for a genuinely missing release, so a 404 recordings
//!   answer does not prove the release has no recordings. It must retry:
//!   feeding it through `recording_mbids_or_fallback` must never produce
//!   the zero-recording success (`Ok(vec![])`) that writes the matched
//!   release with its recordings wiped and no replay ever converging it.
//!   A builder may instead retry until the normal retry budget is
//!   exhausted; what is forbidden is the silent empty write.
//! - Every other classification is unchanged: non-404 4xx stay
//!   `Permanent` (and the fallback still maps `Permanent` to an empty
//!   list), 5xx and unreadable 200 bodies stay `Transient`, and genuine
//!   `200` successes — including empty results/recordings — still parse
//!   as successes. Those controls pass on the unpatched head and must
//!   keep passing; only the 404 cases are RED.
//!
//! Deliberately unpinned here: `429` and 3xx statuses (out of slice
//! scope), and — for recordings only — the pool-worker's
//! `{"error":"missing"}` body shape, which the MB worker never emits
//! today. A body-shape-aware builder may define proven-missing semantics
//! for a shape like that, but the canonical MB `not_found` body pinned
//! below is emitted by route misses too, so it can never serve as that
//! proof.

use nntune_ingest::matching::{
    parse_mb_recordings_response, parse_mb_search_response, recording_mbids_or_fallback,
    MatchFailure,
};

const MBID_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const MBID_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";

/// The exact body the MB worker renders for both a route miss and a
/// genuinely missing release.
const CANONICAL_NOT_FOUND: &str = r#"{"code":"not_found","message":"not found","retryable":false}"#;

/// Bodies a 404 may realistically carry across deploys and intermediaries:
/// canonical, abbreviated, retry-flag variant, empty, bare object,
/// non-JSON, and an HTML error page. None of them proves "no such
/// release / no recordings", so none may change the 404 verdict.
const AMBIGUOUS_404_BODIES: &[&str] = &[
    CANONICAL_NOT_FOUND,
    r#"{"code":"not_found"}"#,
    r#"{"code":"not_found","message":"not found","retryable":true}"#,
    "",
    "{}",
    "not json",
    "<html><body>404 Not Found</body></html>",
];

// ---------------------------------------------------------------------------
// Search: 404 retries, never DLQs
// ---------------------------------------------------------------------------

#[test]
fn search_404_canonical_not_found_body_is_transient() {
    assert_eq!(
        parse_mb_search_response(404, CANONICAL_NOT_FOUND),
        Err(MatchFailure::Transient),
        "a search 404 is a route miss / version skew, never a legitimate \
         no-match: it must retry instead of DLQ-ing the match job"
    );
}

#[test]
fn search_404_is_transient_regardless_of_body() {
    for body in AMBIGUOUS_404_BODIES
        .iter()
        .copied()
        .chain([r#"{"error":"missing"}"#])
    {
        assert_eq!(
            parse_mb_search_response(404, body),
            Err(MatchFailure::Transient),
            "search 404 with body {body:?} must be Transient"
        );
    }
}

#[test]
fn search_other_4xx_stay_permanent() {
    for status in [400u16, 401, 403, 422] {
        for body in ["{}", CANONICAL_NOT_FOUND] {
            assert_eq!(
                parse_mb_search_response(status, body),
                Err(MatchFailure::Permanent),
                "search {status} with body {body:?} keeps its current \
                 Permanent classification"
            );
        }
    }
}

#[test]
fn search_5xx_and_unreadable_200_stay_transient() {
    for status in [500u16, 502, 503, 504] {
        assert_eq!(
            parse_mb_search_response(status, "{}"),
            Err(MatchFailure::Transient),
            "search {status} stays Transient"
        );
    }
    assert_eq!(
        parse_mb_search_response(200, "not json"),
        Err(MatchFailure::Transient),
        "an unreadable 200 body stays Transient"
    );
}

#[test]
fn search_200_success_controls_still_parse() {
    let hits = parse_mb_search_response(
        200,
        &format!(
            r#"{{"q":"x","type":"release","results":[{{"mbid":"{MBID_A}","title":"A","score":0.9}}]}}"#
        ),
    )
    .expect("a valid 200 search body must parse");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].mbid, MBID_A);
    assert_eq!(hits[0].title, "A");

    // The legitimate no-match: 200 with an empty results list. This is
    // exactly why a 404 can never mean "no match".
    let empty = parse_mb_search_response(200, r#"{"q":"x","type":"release","results":[]}"#)
        .expect("an empty 200 search body must parse as a no-match success");
    assert!(empty.is_empty());
}

// ---------------------------------------------------------------------------
// Recordings: 404 retries, never a zero-recording success
// ---------------------------------------------------------------------------

#[test]
fn recordings_404_canonical_not_found_body_is_transient() {
    assert_eq!(
        parse_mb_recordings_response(404, CANONICAL_NOT_FOUND),
        Err(MatchFailure::Transient),
        "the canonical MB 404 body is shared by route misses, so it does \
         not prove a missing release: it must retry, not wipe recordings"
    );
}

#[test]
fn recordings_404_ambiguous_bodies_are_transient() {
    for body in AMBIGUOUS_404_BODIES {
        assert_eq!(
            parse_mb_recordings_response(404, body),
            Err(MatchFailure::Transient),
            "recordings 404 with body {body:?} must be Transient"
        );
    }
}

#[test]
fn recordings_404_never_becomes_a_zero_recording_success_via_fallback() {
    for body in AMBIGUOUS_404_BODIES {
        let outcome = recording_mbids_or_fallback(parse_mb_recordings_response(404, body));
        assert_ne!(
            outcome,
            Ok(Vec::new()),
            "recordings 404 with body {body:?} must not become the \
             zero-recording success that wipes the matched release"
        );
        assert_eq!(
            outcome,
            Err(MatchFailure::Transient),
            "recordings 404 with body {body:?} must propagate as a retry"
        );
    }
}

#[test]
fn recordings_other_4xx_stay_permanent_and_fall_back_to_empty() {
    for status in [400u16, 401, 403, 422] {
        for body in ["{}", CANONICAL_NOT_FOUND] {
            assert_eq!(
                parse_mb_recordings_response(status, body),
                Err(MatchFailure::Permanent),
                "recordings {status} with body {body:?} keeps its current \
                 Permanent classification"
            );
            assert_eq!(
                recording_mbids_or_fallback(parse_mb_recordings_response(status, body)),
                Ok(Vec::new()),
                "recordings {status} keeps the existing Permanent fallback: \
                 write the matched release with no recordings"
            );
        }
    }
}

#[test]
fn recordings_5xx_stay_transient_and_propagate_through_fallback() {
    for status in [500u16, 502, 503, 504] {
        assert_eq!(
            parse_mb_recordings_response(status, "{}"),
            Err(MatchFailure::Transient),
            "recordings {status} stays Transient"
        );
        assert_eq!(
            recording_mbids_or_fallback(parse_mb_recordings_response(status, "{}")),
            Err(MatchFailure::Transient),
            "recordings {status} still propagates through the fallback as a retry"
        );
    }
    assert_eq!(
        parse_mb_recordings_response(200, "not json"),
        Err(MatchFailure::Transient),
        "an unreadable 200 body stays Transient"
    );
}

#[test]
fn recordings_200_success_controls_still_parse() {
    let body = format!(
        r#"{{"release_mbid":"{MBID_A}","recordings":[{{"mbid":"{MBID_A}","track_no":1,"medium_no":1}},{{"mbid":"{MBID_B}","track_no":2,"medium_no":1}}]}}"#
    );
    assert_eq!(
        parse_mb_recordings_response(200, &body),
        Ok(vec![MBID_A.to_string(), MBID_B.to_string()]),
        "a valid 200 recordings body must parse to its MBIDs"
    );

    // A *successful* fetch that reports no recordings is a legitimate
    // empty list — unlike a 404, it may flow through as Ok(vec![]).
    let empty_body = format!(r#"{{"release_mbid":"{MBID_A}","recordings":[]}}"#);
    assert_eq!(
        parse_mb_recordings_response(200, &empty_body),
        Ok(Vec::new()),
        "an empty 200 recordings body is a legitimate empty success"
    );
    assert_eq!(
        recording_mbids_or_fallback(parse_mb_recordings_response(200, &empty_body)),
        Ok(Vec::new()),
        "a successful empty fetch passes through the fallback unchanged"
    );
}

#[test]
fn fallback_controls_in_isolation_are_unchanged() {
    let fetched = vec![MBID_A.to_string(), MBID_B.to_string()];
    assert_eq!(
        recording_mbids_or_fallback(Ok(fetched.clone())),
        Ok(fetched),
        "a successful fetch passes its MBIDs through"
    );
    assert_eq!(
        recording_mbids_or_fallback(Ok(Vec::new())),
        Ok(Vec::<String>::new()),
        "a successful empty fetch stays an empty success"
    );
    assert_eq!(
        recording_mbids_or_fallback(Err(MatchFailure::Transient)),
        Err(MatchFailure::Transient),
        "Transient still propagates so the job retries"
    );
    assert_eq!(
        recording_mbids_or_fallback(Err(MatchFailure::Permanent)),
        Ok(Vec::new()),
        "Permanent still falls back to writing the release with no \
         recordings — the 404 fix belongs in classification, not here"
    );
}
