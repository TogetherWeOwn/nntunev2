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
//! Recording MBIDs stay empty for now: the MB worker has no
//! release-to-recordings route yet (only per-recording lookup/search), so
//! there is no honest way to populate them. The library upsert converges
//! (`ON CONFLICT DO UPDATE`), so a later replay can fill them in once that
//! route exists.

use serde::Deserialize;

use crate::queue::{validate_mbid, LibraryWrite, QueueMsg};

/// Service binding name for the `workers/mb` read API: must match
/// `[[services]]` in `wrangler.toml.example`.
pub const MB_API_BINDING: &str = "MB_API";

/// Host used for service-binding fetches. The binding routes by service,
/// not by host; the name only needs to be a valid absolute URL.
const MB_SERVICE_HOST: &str = "https://mb-api.invalid";

/// Minimum trigram similarity accepted as a release match. This is
/// deliberately well above the `pg_trgm` default `similarity_threshold`
/// (0.3) that the MB worker's `%` predicate applies server-side: that
/// search ranks on release title only, so for an "Artist - Album" query a
/// release merely titled "Artist" can outscore the real album at the 0.3
/// floor. Until the MB lookup is artist-aware, only a strong title hit is
/// accepted (see also [`MIN_MATCH_MARGIN`]); anything weaker is written
/// unmatched rather than guessed at.
pub const MIN_MATCH_SCORE: f32 = 0.6;

/// Minimum lead the top hit must hold over the runner-up. Two releases
/// with the same title (by any artist) score identically under the
/// title-only search, so a tie — or a near-tie within this margin — is
/// ambiguous and yields no match instead of an arbitrary MBID pick.
pub const MIN_MATCH_MARGIN: f32 = 0.1;

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

/// Deterministic, ambiguity-rejecting release pick: the highest score at or
/// above [`MIN_MATCH_SCORE`] with a canonical MBID, accepted only when it
/// leads the runner-up by more than [`MIN_MATCH_MARGIN`]. An exact tie or
/// a near-tie returns `None` (unmatched) — the title-only search cannot
/// distinguish same-title releases, and persisting an arbitrary one of
/// them would be a guessed match. The MB worker already orders hits by
/// score, but the pick does not trust wire order.
pub fn pick_release_mbid(hits: &[MbSearchHit]) -> Option<String> {
    let mut candidates: Vec<&MbSearchHit> = hits
        .iter()
        .filter(|hit| hit.score >= MIN_MATCH_SCORE && validate_mbid(&hit.mbid))
        .collect();
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(core::cmp::Ordering::Equal)
            .then_with(|| a.mbid.cmp(&b.mbid))
    });
    let top = candidates.first()?;
    if let Some(runner_up) = candidates.get(1) {
        if top.score - runner_up.score <= MIN_MATCH_MARGIN {
            return None;
        }
    }
    Some(top.mbid.clone())
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

/// The library write for a job and its match outcome. `None` is a
/// legitimate unmatched write (no query, or no hit passed the pick), not
/// a failure: the row records the job and converges if a later replay
/// refines the match.
pub fn library_write_for(msg: &QueueMsg, release_mbid: Option<String>) -> LibraryWrite {
    LibraryWrite {
        job_id: msg.job_id.clone(),
        user_id: msg.user_id.clone(),
        idempotency_key: msg.idempotency_key.clone(),
        release_mbid,
        recording_mbids: Vec::new(),
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
        // A clear winner above the bar wins regardless of wire order.
        let hits = vec![hit(MBID_B, 0.65), hit(MBID_A, 0.9)];
        assert_eq!(pick_release_mbid(&hits).as_deref(), Some(MBID_A));
        let hits_rev = vec![hit(MBID_A, 0.9), hit(MBID_B, 0.65)];
        assert_eq!(pick_release_mbid(&hits_rev).as_deref(), Some(MBID_A));
        // A lone strong hit matches.
        assert_eq!(
            pick_release_mbid(&[hit(MBID_A, 0.9)]).as_deref(),
            Some(MBID_A)
        );
        // Below the raised bar or malformed MBIDs never match — including
        // scores the server-side 0.3 `%` floor would have returned.
        assert_eq!(pick_release_mbid(&[hit(MBID_A, 0.59)]), None);
        assert_eq!(pick_release_mbid(&[hit(MBID_A, 0.35)]), None);
        assert_eq!(pick_release_mbid(&[hit("not-an-mbid", 1.0)]), None);
    }

    #[test]
    fn pick_rejects_ambiguous_same_title_and_artist_named_hits() {
        // Two same-title releases tie exactly: title-only search cannot
        // tell them apart, so the pick is unmatched in either wire order
        // rather than the lexicographically smallest MBID.
        let ties = vec![hit(MBID_B, 0.9), hit(MBID_A, 0.9)];
        assert_eq!(pick_release_mbid(&ties), None);
        let ties_rev = vec![hit(MBID_A, 0.9), hit(MBID_B, 0.9)];
        assert_eq!(pick_release_mbid(&ties_rev), None);
        // A near-tie within the margin is likewise ambiguous.
        let near_tie = vec![hit(MBID_A, 0.9), hit(MBID_B, 0.85)];
        assert_eq!(pick_release_mbid(&near_tie), None);
        // "Artist - Album" query under title-only trigram scoring: a
        // release titled just "Pink Floyd" (0.579) outscores the real
        // album "Animals" (0.421). Both sit below the raised bar, so the
        // artist-named release is not attached by mistake.
        let artist_named = vec![
            MbSearchHit {
                mbid: MBID_B.to_string(),
                title: "Pink Floyd".to_string(),
                score: 0.579,
            },
            MbSearchHit {
                mbid: MBID_A.to_string(),
                title: "Animals".to_string(),
                score: 0.421,
            },
        ];
        assert_eq!(pick_release_mbid(&artist_named), None);
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
    fn library_write_carries_job_identity_and_no_recordings_yet() {
        let msg =
            build_search_dispatch("user_01", "animals", "vault_idx_01", &[3000], 25, 0).unwrap();
        let write = library_write_for(&msg, Some(MBID_A.to_string()));
        assert_eq!(write.job_id, msg.job_id);
        assert_eq!(write.user_id, msg.user_id);
        assert_eq!(write.idempotency_key, msg.idempotency_key);
        assert_eq!(write.release_mbid.as_deref(), Some(MBID_A));
        assert!(write.recording_mbids.is_empty());
        assert!(crate::library::validate_library_write(&write).is_ok());
    }
}
