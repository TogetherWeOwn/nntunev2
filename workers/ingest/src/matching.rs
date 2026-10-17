//! MusicBrainz matching: turn a job's match query into a library write.
//!
//! The query is the indexer-search text for `IndexerSearch` jobs, or a
//! candidate release title derived from the NZB file subjects for
//! `NzbUpload` jobs ([`derive_upload_query`]). The lookup itself is the
//! `workers/mb` read API (`GET /v1/mb/search?type=release`, verified on
//! `main` in `workers/mb/src/lib.rs`), reached at the edge through the
//! `MB_API` service binding; this module owns the URL shape, the response
//! classification, and the deterministic pick so all of it is unit-tested
//! without a binding.
//!
//! After a release pick, the matcher also fetches the release's
//! recordings (`GET /v1/mb/release/:mbid/recordings`) to fill
//! `recording_mbids`. A recordings-fetch failure never loses the release
//! match: transient failures retry the job (the match is re-resolved on
//! redelivery), and permanent failures write the matched release with an
//! empty list — the library upsert converges (`ON CONFLICT DO UPDATE`),
//! so a later replay can still fill them in. See
//! [`recording_mbids_or_fallback`].

use serde::Deserialize;

use crate::queue::{validate_mbid, LibraryWrite, QueueMsg};

/// Service binding name for the `workers/mb` read API: must match
/// `[[services]]` in `wrangler.toml.example`.
pub const MB_API_BINDING: &str = "MB_API";

/// Host used for service-binding fetches. The binding routes by service,
/// not by host; the name only needs to be a valid absolute URL.
const MB_SERVICE_HOST: &str = "https://mb-api.invalid";

/// Minimum trigram similarity accepted as a release match. This mirrors
/// the `pg_trgm` default `similarity_threshold` (0.3) that the MB worker's
/// `%` predicate already applies server-side; re-checking it here means a
/// future MB-side contract change cannot silently start accepting weaker
/// hits as library matches.
pub const MIN_MATCH_SCORE: f32 = 0.3;

/// Upper bound on `recording_mbids` carried into a library write. Mirrors
/// the 10k validation cap in `library::validate_library_write`:
/// truncating here keeps an oversized listing from failing validation
/// and losing the whole write. The MB route caps at 1000 rows, so this
/// only bites if that contract grows past the library cap.
pub const MAX_RECORDING_MBIDS: usize = 10_000;

/// One MB release-search hit (`GET /v1/mb/search?type=release`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MbSearchHit {
    pub mbid: String,
    #[serde(default)]
    pub title: String,
    pub score: f32,
}

#[derive(Debug, Deserialize)]
struct MbSearchBody {
    results: Vec<MbSearchHit>,
}

/// One entry of `GET /v1/mb/release/:mbid/recordings`. Only the fields
/// the library write needs are decoded; extra fields are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MbRecording {
    pub mbid: String,
    #[serde(default)]
    pub track_no: Option<i16>,
}

#[derive(Debug, Deserialize)]
struct MbRecordingsBody {
    recordings: Vec<MbRecording>,
}

/// Why an MB lookup failed, in retry-budget terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchFailure {
    /// Binding/transport failure, a 5xx, or an unreadable body: the same
    /// lookup may succeed on redelivery.
    Transient,
    /// The MB worker rejected the request itself (4xx). The query is
    /// pre-validated against the MB contract ([`validate_match_query`]),
    /// so a 4xx is a contract bug, not a retry.
    Permanent,
}

/// Validate a query against the MB search contract before spending the
/// call: 2..=100 trimmed chars, no control characters (the MB handler
/// rejects all three with 400). Returns the trimmed query to send.
pub fn validate_match_query(raw: &str) -> Option<String> {
    let q = raw.trim();
    let chars = q.chars().count();
    if !(2..=100).contains(&chars) || q.chars().any(char::is_control) {
        return None;
    }
    Some(q.to_string())
}

/// Percent-encode one query value (unreserved bytes pass through).
fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Absolute MB release-search URL for the `MB_API` service binding.
pub fn mb_search_url(query: &str) -> String {
    format!(
        "{MB_SERVICE_HOST}/v1/mb/search?type=release&q={}",
        encode_query_value(query)
    )
}

/// Classify one MB search response. Only a 200 with the documented body
/// shape is a result; an empty result list is a successful no-match.
pub fn parse_mb_search_response(status: u16, body: &str) -> Result<Vec<MbSearchHit>, MatchFailure> {
    match status {
        200 => serde_json::from_str::<MbSearchBody>(body)
            .map(|parsed| parsed.results)
            .map_err(|_| MatchFailure::Transient),
        400..=499 => Err(MatchFailure::Permanent),
        _ => Err(MatchFailure::Transient),
    }
}

/// Absolute MB release-recordings URL for the `MB_API` service binding.
/// Callers pass a picked release MBID, which [`pick_release_mbid`] has
/// already validated as canonical hex-and-hyphens, so it needs no
/// percent-encoding.
pub fn mb_release_recordings_url(release_mbid: &str) -> String {
    format!("{MB_SERVICE_HOST}/v1/mb/release/{release_mbid}/recordings")
}

/// Classify one MB release-recordings response into recording MBIDs in
/// track order. The MB worker orders server-side (`track_no` nulls
/// last, then MBID), but — like [`pick_release_mbid`] — this does not
/// trust wire order: entries are re-sorted the same way, malformed
/// MBIDs are dropped, duplicates keep their first position, and the
/// list is capped at [`MAX_RECORDING_MBIDS`]. An empty recordings list
/// is a successful empty result. Status classification mirrors
/// [`parse_mb_search_response`]; note a 404 here is `Permanent` (the
/// release vanished between search and this fetch, or a contract bug),
/// and [`recording_mbids_or_fallback`] decides what that means for the
/// write.
pub fn parse_mb_recordings_response(status: u16, body: &str) -> Result<Vec<String>, MatchFailure> {
    match status {
        200 => serde_json::from_str::<MbRecordingsBody>(body)
            .map(|parsed| recording_mbids_in_track_order(&parsed.recordings))
            .map_err(|_| MatchFailure::Transient),
        400..=499 => Err(MatchFailure::Permanent),
        _ => Err(MatchFailure::Transient),
    }
}

/// Recording MBIDs ordered by track number (missing numbers last),
/// ties broken by the lexicographically smallest MBID.
fn recording_mbids_in_track_order(recordings: &[MbRecording]) -> Vec<String> {
    let mut valid: Vec<&MbRecording> = recordings
        .iter()
        .filter(|recording| validate_mbid(&recording.mbid))
        .collect();
    valid.sort_by(|a, b| {
        a.track_no
            .unwrap_or(i16::MAX)
            .cmp(&b.track_no.unwrap_or(i16::MAX))
            .then_with(|| a.mbid.cmp(&b.mbid))
    });
    let mut seen = std::collections::HashSet::with_capacity(valid.len());
    let mut mbids: Vec<String> = Vec::with_capacity(valid.len().min(MAX_RECORDING_MBIDS));
    for recording in valid {
        if seen.insert(recording.mbid.as_str()) {
            mbids.push(recording.mbid.clone());
        }
        if mbids.len() == MAX_RECORDING_MBIDS {
            break;
        }
    }
    mbids
}

/// Decide the `recording_mbids` for a matched write from the recordings
/// fetch outcome, without ever losing the release match itself:
///
/// - success → the fetched MBIDs;
/// - `Permanent` → an empty list. Retrying cannot fix a 4xx, so the
///   matched release is still written (with no recordings) rather than
///   failing the job; a later replay converges the row once fixed.
/// - `Transient` → propagate, so the caller retries the job and the
///   match is re-resolved — recordings included — on redelivery.
pub fn recording_mbids_or_fallback(
    fetch: Result<Vec<String>, MatchFailure>,
) -> Result<Vec<String>, MatchFailure> {
    match fetch {
        Ok(mbids) => Ok(mbids),
        Err(MatchFailure::Permanent) => Ok(Vec::new()),
        Err(transient) => Err(transient),
    }
}

/// Deterministic release pick: the highest score at or above
/// [`MIN_MATCH_SCORE`] with a canonical MBID, ties broken by the
/// lexicographically smallest MBID. The MB worker already orders hits
/// this way, but the pick does not trust wire order.
pub fn pick_release_mbid(hits: &[MbSearchHit]) -> Option<String> {
    hits.iter()
        .filter(|hit| hit.score >= MIN_MATCH_SCORE && validate_mbid(&hit.mbid))
        .min_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| a.mbid.cmp(&b.mbid))
        })
        .map(|hit| hit.mbid.clone())
}

/// Derive a candidate release query from NZB file subjects.
///
/// Scene-style subjects look like
/// `Artist - Album [01/05] - "01 track.flac" yEnc (1/3)`: the release
/// title is the text before the quoted file name, minus trailing part
/// counters. Dots/underscores become spaces and one trailing file-type
/// token is dropped for bare-filename subjects. Anything that does not
/// survive [`validate_match_query`] yields `None` (the job is then written
/// unmatched rather than guessed at).
pub fn derive_upload_query(subjects: &[String]) -> Option<String> {
    let first = subjects.first()?;
    let before_quote = first.split('"').next().unwrap_or(first);
    let spaced: String = before_quote
        .chars()
        .map(|c| if c == '.' || c == '_' { ' ' } else { c })
        .collect();
    let mut candidate = spaced.trim().trim_end_matches('-').trim().to_string();
    // Drop trailing "[01/05]" / "(1/3)" / "yEnc" groups, repeatedly.
    loop {
        let trimmed = candidate.trim_end();
        let stripped = match trimmed.chars().last() {
            Some(']') => trimmed.rfind('[').map(|i| &trimmed[..i]),
            Some(')') => trimmed.rfind('(').map(|i| &trimmed[..i]),
            _ if trimmed
                .rsplit_once(char::is_whitespace)
                .is_some_and(|(_, tail)| tail.eq_ignore_ascii_case("yenc")) =>
            {
                trimmed
                    .rsplit_once(char::is_whitespace)
                    .map(|(head, _)| head)
            }
            _ => None,
        };
        let Some(head) = stripped else { break };
        let next = head.trim().trim_end_matches('-').trim().to_string();
        if next == candidate {
            break;
        }
        candidate = next;
        if candidate.is_empty() {
            break;
        }
    }
    // Bare "name ext" subjects: drop one trailing file-type token.
    const FILE_TYPES: [&str; 11] = [
        "flac", "mp3", "ogg", "opus", "m4a", "wav", "rar", "zip", "7z", "par2", "nfo",
    ];
    if let Some((head, tail)) = candidate.rsplit_once(char::is_whitespace) {
        if FILE_TYPES.contains(&tail.to_ascii_lowercase().as_str()) {
            candidate = head.trim().to_string();
        }
    }
    validate_match_query(&candidate)
}

/// The library write for a job and its match outcome. A `None` release
/// is a legitimate unmatched write (no query, or no hit passed the
/// pick), not a failure: the row records the job and converges if a
/// later replay refines the match. Recordings only ever accompany a
/// matched release — a caller that passes recordings without one has
/// them dropped rather than writing recording MBIDs unattributable to
/// any release.
pub fn library_write_for(
    msg: &QueueMsg,
    release_mbid: Option<String>,
    recording_mbids: Vec<String>,
) -> LibraryWrite {
    let recording_mbids = if release_mbid.is_some() {
        recording_mbids
    } else {
        Vec::new()
    };
    LibraryWrite {
        job_id: msg.job_id.clone(),
        user_id: msg.user_id.clone(),
        idempotency_key: msg.idempotency_key.clone(),
        release_mbid,
        recording_mbids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::build_search_dispatch;

    const MBID_A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
    const MBID_B: &str = "bbbbbbbb-2222-4222-8222-222222222222";

    fn hit(mbid: &str, score: f32) -> MbSearchHit {
        MbSearchHit {
            mbid: mbid.to_string(),
            title: "Album".to_string(),
            score,
        }
    }

    #[test]
    fn query_validation_mirrors_mb_contract() {
        assert_eq!(validate_match_query("  ab "), Some("ab".to_string()));
        assert_eq!(validate_match_query("a"), None);
        assert_eq!(validate_match_query(&"a".repeat(101)), None);
        assert_eq!(validate_match_query("be\u{7}ll"), None);
    }

    #[test]
    fn search_url_encodes_query_and_pins_release_type() {
        let url = mb_search_url("dark side & more?");
        assert!(
            url.starts_with("https://mb-api.invalid/v1/mb/search?type=release&q="),
            "{url}"
        );
        assert!(url.ends_with("dark%20side%20%26%20more%3F"), "{url}");
    }

    #[test]
    fn response_classification_separates_retryable_from_permanent() {
        let ok = parse_mb_search_response(
            200,
            &format!(r#"{{"q":"x","type":"release","results":[{{"mbid":"{MBID_A}","title":"A","score":0.9}}]}}"#),
        )
        .unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].title, "A");
        // Empty success is a no-match, not an error.
        assert!(
            parse_mb_search_response(200, r#"{"q":"x","type":"release","results":[]}"#)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            parse_mb_search_response(400, "{}"),
            Err(MatchFailure::Permanent)
        );
        assert_eq!(
            parse_mb_search_response(500, "{}"),
            Err(MatchFailure::Transient)
        );
        assert_eq!(
            parse_mb_search_response(200, "not json"),
            Err(MatchFailure::Transient)
        );
    }

    #[test]
    fn pick_is_deterministic_and_thresholded() {
        assert_eq!(pick_release_mbid(&[]), None);
        // Highest score wins regardless of wire order.
        let hits = vec![hit(MBID_B, 0.5), hit(MBID_A, 0.9)];
        assert_eq!(pick_release_mbid(&hits).as_deref(), Some(MBID_A));
        // Ties go to the smallest MBID, in either order.
        let ties = vec![hit(MBID_B, 0.8), hit(MBID_A, 0.8)];
        assert_eq!(pick_release_mbid(&ties).as_deref(), Some(MBID_A));
        let ties_rev = vec![hit(MBID_A, 0.8), hit(MBID_B, 0.8)];
        assert_eq!(pick_release_mbid(&ties_rev).as_deref(), Some(MBID_A));
        // Below threshold or malformed MBIDs never match.
        assert_eq!(pick_release_mbid(&[hit(MBID_A, 0.29)]), None);
        assert_eq!(pick_release_mbid(&[hit("not-an-mbid", 1.0)]), None);
    }

    #[test]
    fn upload_query_comes_from_the_subject_prefix() {
        let subjects =
            vec![r#"Pink Floyd - Animals [01/05] - "01 pigs.flac" yEnc (1/3)"#.to_string()];
        assert_eq!(
            derive_upload_query(&subjects).as_deref(),
            Some("Pink Floyd - Animals")
        );
        let bare = vec!["track01.flac".to_string()];
        assert_eq!(derive_upload_query(&bare).as_deref(), Some("track01"));
        assert_eq!(derive_upload_query(&[]), None);
        assert_eq!(
            derive_upload_query(&["\"only.flac\" yEnc".to_string()]),
            None
        );
    }

    #[test]
    fn library_write_carries_job_identity() {
        let msg =
            build_search_dispatch("user_01", "animals", "vault_idx_01", &[3000], 25, 0).unwrap();
        let write = library_write_for(&msg, Some(MBID_A.to_string()), Vec::new());
        assert_eq!(write.job_id, msg.job_id);
        assert_eq!(write.user_id, msg.user_id);
        assert_eq!(write.idempotency_key, msg.idempotency_key);
        assert_eq!(write.release_mbid.as_deref(), Some(MBID_A));
        assert!(write.recording_mbids.is_empty());
        assert!(crate::library::validate_library_write(&write).is_ok());
    }

    #[test]
    fn recordings_url_targets_the_release_listing_route() {
        assert_eq!(
            mb_release_recordings_url(MBID_A),
            format!("https://mb.internal/v1/mb/release/{MBID_A}/recordings")
        );
    }

    #[test]
    fn recordings_response_is_sorted_into_track_order() {
        // Wire order is not trusted: track 2 arrives before track 1, an
        // untracked entry and a malformed MBID are mixed in, and track 1
        // is duplicated.
        const REC1: &str = "11111111-1111-4111-8111-111111111111";
        const REC2: &str = "22222222-2222-4222-8222-222222222222";
        const REC_UNTRACKED: &str = "33333333-3333-4333-8333-333333333333";
        let body = format!(
            r#"{{"release_mbid":"{MBID_A}","recordings":[
                {{"mbid":"{REC2}","title":"Second","track_no":2}},
                {{"mbid":"{REC_UNTRACKED}","title":"Bonus","track_no":null}},
                {{"mbid":"not-an-mbid","title":"Bogus","track_no":1}},
                {{"mbid":"{REC1}","title":"First","track_no":1}},
                {{"mbid":"{REC1}","title":"First again","track_no":1}}
            ]}}"#
        );
        let mbids = parse_mb_recordings_response(200, &body).unwrap();
        assert_eq!(mbids, vec![REC1, REC2, REC_UNTRACKED]);
    }

    #[test]
    fn recordings_response_classification_mirrors_search() {
        assert!(parse_mb_recordings_response(
            200,
            &format!(r#"{{"release_mbid":"{MBID_A}","recordings":[]}}"#)
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            parse_mb_recordings_response(404, "{}"),
            Err(MatchFailure::Permanent)
        );
        assert_eq!(
            parse_mb_recordings_response(400, "{}"),
            Err(MatchFailure::Permanent)
        );
        assert_eq!(
            parse_mb_recordings_response(500, "{}"),
            Err(MatchFailure::Transient)
        );
        assert_eq!(
            parse_mb_recordings_response(200, "not json"),
            Err(MatchFailure::Transient)
        );
    }

    #[test]
    fn recordings_fallback_never_loses_the_release_match() {
        let fetched = vec![MBID_B.to_string()];
        assert_eq!(
            recording_mbids_or_fallback(Ok(fetched.clone())).unwrap(),
            fetched
        );
        // Permanent: write the matched release with no recordings.
        assert!(recording_mbids_or_fallback(Err(MatchFailure::Permanent))
            .unwrap()
            .is_empty());
        // Transient: propagate so the job retries and re-resolves.
        assert_eq!(
            recording_mbids_or_fallback(Err(MatchFailure::Transient)),
            Err(MatchFailure::Transient)
        );
    }

    #[test]
    fn matched_write_carries_recording_mbids_in_track_order() {
        let msg =
            build_search_dispatch("user_01", "animals", "vault_idx_01", &[3000], 25, 0).unwrap();
        const REC1: &str = "11111111-1111-4111-8111-111111111111";
        const REC2: &str = "22222222-2222-4222-8222-222222222222";
        let body = format!(
            r#"{{"release_mbid":"{MBID_A}","recordings":[
                {{"mbid":"{REC2}","track_no":2}},
                {{"mbid":"{REC1}","track_no":1}}
            ]}}"#
        );
        let recording_mbids =
            recording_mbids_or_fallback(parse_mb_recordings_response(200, &body)).unwrap();
        let write = library_write_for(&msg, Some(MBID_A.to_string()), recording_mbids);
        assert_eq!(write.release_mbid.as_deref(), Some(MBID_A));
        assert_eq!(write.recording_mbids, vec![REC1, REC2]);
        assert!(crate::library::validate_library_write(&write).is_ok());
        // Recordings without a release are dropped, never written orphaned.
        let unmatched = library_write_for(&msg, None, vec![REC1.to_string()]);
        assert!(unmatched.recording_mbids.is_empty());
        assert!(crate::library::validate_library_write(&unmatched).is_ok());
    }
}
