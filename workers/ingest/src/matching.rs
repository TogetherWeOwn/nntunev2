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

/// Minimum trigram similarity accepted for a plain-title query (one with
/// no "Artist - Album" shape). This is deliberately well above the
/// `pg_trgm` default `similarity_threshold` (0.3) that the MB worker's
/// `%` predicate applies server-side. It never applies to
/// "Artist - Album" queries: there the server score is
/// `similarity(title, whole query)`, so the real album can score 0.333
/// while a self-titled artist release scores 0.667 — no threshold
/// separates those, which is why that shape is verified against the
/// hit's artist instead (see [`pick_release_mbid`]).
pub const MIN_MATCH_SCORE: f32 = 0.6;

/// Minimum lead the top hit must hold over the runner-up for a
/// plain-title query. Two releases with the same title (by any artist)
/// score identically under the title-only search, so a tie — or a
/// near-tie within this margin — is ambiguous and yields no match
/// instead of an arbitrary MBID pick.
pub const MIN_MATCH_MARGIN: f32 = 0.1;

/// One MB release-search hit (`GET /v1/mb/search?type=release`).
/// `artist_mbid`/`artist_name` come from the release's joined
/// `mb_artist` row and are null when the release has no artist (or when
/// an older cached response predates the fields).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MbSearchHit {
    pub mbid: String,
    #[serde(default)]
    pub title: String,
    pub score: f32,
    #[serde(default)]
    pub artist_mbid: Option<String>,
    #[serde(default)]
    pub artist_name: Option<String>,
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

/// Normalize text for exact match comparison: lowercased, every
/// non-alphanumeric run collapsed to single spaces. "Don't Stop!" and
/// "dont stop" compare equal; different words never do.
fn normalize_match_text(raw: &str) -> String {
    raw.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split an "Artist - Album" query into its normalized parts, when it
/// has that shape (the shape [`derive_upload_query`] produces).
/// Typographic dashes (en/em dash, minus sign — as pasted from the web)
/// count as the separator hyphen. A query without " - ", or with an
/// empty side, has no artist part.
fn split_artist_album(query: &str) -> Option<(String, String)> {
    let folded: String = query
        .chars()
        .map(|c| match c {
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            _ => c,
        })
        .collect();
    let (artist, album) = folded.split_once(" - ")?;
    let artist = normalize_match_text(artist);
    let album = normalize_match_text(album);
    if artist.is_empty() || album.is_empty() {
        return None;
    }
    Some((artist, album))
}

/// Deterministic, ambiguity-rejecting release pick. What it can and
/// cannot disambiguate:
///
/// - "Artist - Album" queries are verified, not scored: a hit counts
///   only when its `artist_name` and `title` are exactly the query's
///   parts after [`normalize_match_text`]. The trigram score is ignored
///   there on purpose — it is `similarity(title, whole query)`, under
///   which a self-titled artist release ("Metallica", 0.667) beats the
///   real album ("Load", 0.333), so no threshold or margin can tell
///   them apart. Exactly one distinct MBID may survive; zero or several
///   (same artist, same title, different releases) is unmatched. Hits
///   without an artist cannot be verified and never count here.
/// - Plain-title queries keep the scored pick, tightened to hits whose
///   normalized title is exactly the normalized query: the highest
///   score at or above [`MIN_MATCH_SCORE`], leading the runner-up by
///   more than [`MIN_MATCH_MARGIN`]. Fuzzy near-misses write unmatched.
///
/// Anything else — extra words in the query, edition suffixes, a
/// featured-artist credit — yields `None` (unmatched) rather than a
/// guessed MBID, because the library upsert would persist the guess
/// and replays would keep overwriting with it.
pub fn pick_release_mbid(query: &str, hits: &[MbSearchHit]) -> Option<String> {
    let valid = hits.iter().filter(|hit| validate_mbid(&hit.mbid));
    if let Some((artist, album)) = split_artist_album(query) {
        let mut matches: Vec<&str> = valid
            .filter(|hit| {
                hit.artist_name
                    .as_deref()
                    .map(normalize_match_text)
                    .is_some_and(|name| name == artist)
                    && normalize_match_text(&hit.title) == album
            })
            .map(|hit| hit.mbid.as_str())
            .collect();
        matches.sort_unstable();
        matches.dedup();
        return match matches.as_slice() {
            [only] => Some((*only).to_string()),
            _ => None,
        };
    }
    let normalized_query = normalize_match_text(query);
    if normalized_query.is_empty() {
        return None;
    }
    let mut candidates: Vec<&MbSearchHit> = valid
        .filter(|hit| {
            hit.score >= MIN_MATCH_SCORE && normalize_match_text(&hit.title) == normalized_query
        })
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
            artist_mbid: None,
            artist_name: None,
        }
    }

    fn hit_by(mbid: &str, title: &str, artist: Option<&str>, score: f32) -> MbSearchHit {
        MbSearchHit {
            mbid: mbid.to_string(),
            title: title.to_string(),
            score,
            artist_mbid: artist.map(|_| "cccccccc-3333-4333-8333-333333333333".to_string()),
            artist_name: artist.map(str::to_string),
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
        assert_eq!(pick_release_mbid("Album", &[]), None);
        // A clear winner above the bar wins regardless of wire order.
        let hits = vec![hit(MBID_B, 0.65), hit(MBID_A, 0.9)];
        assert_eq!(pick_release_mbid("Album", &hits).as_deref(), Some(MBID_A));
        let hits_rev = vec![hit(MBID_A, 0.9), hit(MBID_B, 0.65)];
        assert_eq!(
            pick_release_mbid("Album", &hits_rev).as_deref(),
            Some(MBID_A)
        );
        // A lone strong hit matches; case/punctuation differences in the
        // query do not defeat the normalized-title requirement.
        assert_eq!(
            pick_release_mbid("album!", &[hit(MBID_A, 0.9)]).as_deref(),
            Some(MBID_A)
        );
        // Below the raised bar or malformed MBIDs never match — including
        // scores the server-side 0.3 `%` floor would have returned.
        assert_eq!(pick_release_mbid("Album", &[hit(MBID_A, 0.59)]), None);
        assert_eq!(pick_release_mbid("Album", &[hit(MBID_A, 0.35)]), None);
        assert_eq!(pick_release_mbid("Album", &[hit("not-an-mbid", 1.0)]), None);
        // A strong score for a *different* title never matches a plain
        // query: only a normalized-identical title is persisted.
        assert_eq!(
            pick_release_mbid("Something Else", &[hit(MBID_A, 0.9)]),
            None
        );
    }

    #[test]
    fn pick_rejects_ambiguous_same_title_hits() {
        // Two same-title releases tie exactly: title-only search cannot
        // tell them apart, so the pick is unmatched in either wire order
        // rather than the lexicographically smallest MBID.
        let ties = vec![hit(MBID_B, 0.9), hit(MBID_A, 0.9)];
        assert_eq!(pick_release_mbid("Album", &ties), None);
        let ties_rev = vec![hit(MBID_A, 0.9), hit(MBID_B, 0.9)];
        assert_eq!(pick_release_mbid("Album", &ties_rev), None);
        // A near-tie within the margin is likewise ambiguous.
        let near_tie = vec![hit(MBID_A, 0.9), hit(MBID_B, 0.85)];
        assert_eq!(pick_release_mbid("Album", &near_tie), None);
        // Same artist, same title, two releases: the artist-aware path
        // cannot tell them apart either, so it writes unmatched.
        let dupes = vec![
            hit_by(MBID_A, "Load", Some("Metallica"), 0.333),
            hit_by(MBID_B, "Load", Some("Metallica"), 0.333),
        ];
        assert_eq!(pick_release_mbid("Metallica - Load", &dupes), None);
    }

    #[test]
    fn pick_artist_album_queries_verify_artist_not_score() {
        // Regression (review on 1369e15): under title-only trigram
        // scoring of "Metallica - Load", the self-titled "Metallica"
        // release scores 0.667 and the real album "Load" scores 0.333.
        // The pick must attach "Load" (verified artist + title), never
        // the higher-scored self-titled release.
        let hits = vec![
            hit_by(MBID_B, "Metallica", Some("Metallica"), 0.667),
            hit_by(MBID_A, "Load", Some("Metallica"), 0.333),
        ];
        assert_eq!(
            pick_release_mbid("Metallica - Load", &hits).as_deref(),
            Some(MBID_A)
        );
        // A typographic dash as the separator splits the same way.
        assert_eq!(
            pick_release_mbid("Metallica \u{2013} Load", &hits).as_deref(),
            Some(MBID_A)
        );
        // Same shape, other numbers from the review: "Adele - 21".
        let adele = vec![
            hit_by(MBID_B, "Adele", Some("Adele"), 0.667),
            hit_by(MBID_A, "21", Some("Adele"), 0.333),
        ];
        assert_eq!(
            pick_release_mbid("Adele - 21", &adele).as_deref(),
            Some(MBID_A)
        );
        // A genuinely self-titled album still matches: title and artist
        // both equal the query's parts.
        let self_titled = vec![hit_by(MBID_A, "Metallica", Some("Metallica"), 0.5)];
        assert_eq!(
            pick_release_mbid("Metallica - Metallica", &self_titled).as_deref(),
            Some(MBID_A)
        );
        // Without artist data on the hits (older cached responses), an
        // "Artist - Album" query cannot be verified: unmatched, even
        // for the 0.667 hit.
        let unverified = vec![
            hit_by(MBID_B, "Metallica", None, 0.667),
            hit_by(MBID_A, "Load", None, 0.333),
        ];
        assert_eq!(pick_release_mbid("Metallica - Load", &unverified), None);
        // Right title, wrong artist: unmatched.
        let wrong_artist = vec![hit_by(MBID_A, "Load", Some("Someone Else"), 0.333)];
        assert_eq!(pick_release_mbid("Metallica - Load", &wrong_artist), None);
        // The earlier "Pink Floyd - Animals" case (0.579 vs 0.421) now
        // resolves to the real album instead of merely dodging the bar.
        let floyd = vec![
            hit_by(MBID_B, "Pink Floyd", Some("Pink Floyd"), 0.579),
            hit_by(MBID_A, "Animals", Some("Pink Floyd"), 0.421),
        ];
        assert_eq!(
            pick_release_mbid("Pink Floyd - Animals", &floyd).as_deref(),
            Some(MBID_A)
        );
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
