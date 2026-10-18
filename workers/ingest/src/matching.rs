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
//! redelivery), and permanent (non-404 4xx) failures write the matched
//! release with an empty list — the library upsert converges
//! (`ON CONFLICT DO UPDATE`), so a later replay can still fill them in.
//! A 404 is transient on both routes: the canonical `not_found` body is
//! shared by route misses and genuinely missing releases, so it retries
//! rather than DLQ-ing the match or wiping recordings. See
//! [`recording_mbids_or_fallback`].

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

/// Upper bound on `recording_mbids` carried into a library write. Mirrors
/// the 10k validation cap in `library::validate_library_write`:
/// truncating here keeps an oversized listing from failing validation
/// and losing the whole write. The MB route caps at 1000 rows, so this
/// only bites if that contract grows past the library cap.
pub const MAX_RECORDING_MBIDS: usize = 10_000;

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

/// One entry of `GET /v1/mb/release/:mbid/recordings`. Only the fields
/// the library write needs are decoded; extra fields are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MbRecording {
    pub mbid: String,
    #[serde(default)]
    pub track_no: Option<i16>,
    /// Medium (disc) position within the release. Absent in responses
    /// cached before the field existed, hence the default.
    #[serde(default)]
    pub medium_no: Option<i16>,
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
    /// The MB worker rejected the request itself (a 4xx other than 404).
    /// The query is pre-validated against the MB contract
    /// ([`validate_match_query`]), so such a 4xx is a contract bug, not
    /// a retry. A 404 is `Transient` instead: the canonical
    /// `not_found` body is shared by route misses and genuinely missing
    /// releases, so it proves neither.
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
///
/// A 404 is `Transient`, never a no-match: the legitimate no-match is a
/// 200 with an empty `results` list, so a search 404 can only be a route
/// miss / version skew (e.g. a rolling MB deploy with no search route
/// yet). It retries with backoff; every other 4xx stays `Permanent`.
pub fn parse_mb_search_response(status: u16, body: &str) -> Result<Vec<MbSearchHit>, MatchFailure> {
    match status {
        200 => serde_json::from_str::<MbSearchBody>(body)
            .map(|parsed| parsed.results)
            .map_err(|_| MatchFailure::Transient),
        404 => Err(MatchFailure::Transient),
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
/// album order. The MB worker orders server-side (medium, then
/// `track_no`, then MBID), but — like [`pick_release_mbid`] — this does
/// not trust wire order: entries are re-sorted the same way, malformed
/// MBIDs are dropped, duplicates keep their first position, and the
/// list is capped at [`MAX_RECORDING_MBIDS`]. An empty recordings list
/// is a successful empty result. Status classification mirrors
/// [`parse_mb_search_response`]: a 404 is `Transient`, retrying instead
/// of writing a zero-recording success. The MB worker renders the same
/// `{"code":"not_found",...}` body for a route miss and for a genuinely
/// missing release, so no 404 body proves the release has no recordings;
/// a genuinely missing release therefore retries until the normal retry
/// budget is exhausted rather than converging to an empty write.
/// [`recording_mbids_or_fallback`] decides what that means for the
/// write.
pub fn parse_mb_recordings_response(status: u16, body: &str) -> Result<Vec<String>, MatchFailure> {
    match status {
        200 => serde_json::from_str::<MbRecordingsBody>(body)
            .map(|parsed| recording_mbids_in_track_order(&parsed.recordings))
            .map_err(|_| MatchFailure::Transient),
        404 => Err(MatchFailure::Transient),
        400..=499 => Err(MatchFailure::Permanent),
        _ => Err(MatchFailure::Transient),
    }
}

/// Recording MBIDs ordered by medium (disc) number, then track number
/// within the medium (missing numbers last), ties broken by the
/// lexicographically smallest MBID. Sorting by track number alone
/// would interleave the discs of a multi-medium release (disc 1
/// track 1, disc 2 track 1, disc 1 track 2, ...). A response cached
/// before `medium_no` existed has it missing on every entry, which
/// ties and falls back to track order.
fn recording_mbids_in_track_order(recordings: &[MbRecording]) -> Vec<String> {
    let mut valid: Vec<&MbRecording> = recordings
        .iter()
        .filter(|recording| validate_mbid(&recording.mbid))
        .collect();
    valid.sort_by(|a, b| {
        a.medium_no
            .unwrap_or(i16::MAX)
            .cmp(&b.medium_no.unwrap_or(i16::MAX))
            .then_with(|| {
                a.track_no
                    .unwrap_or(i16::MAX)
                    .cmp(&b.track_no.unwrap_or(i16::MAX))
            })
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

/// A recognized numeric part counter is exactly `digits/digits` with no
/// extra characters: the documented `[01/05]` / `(1/3)` scene forms.
/// Anything else (`[aa/bb]`, `[01/]`, `[1/2/3]`, `[01/05 Live]`,
/// `(Live)`, `(2024)`, `(Disc 1)`, format/bitrate tags such as `[FLAC]`,
/// `(MP3)` or `[320]`) is release identity and is preserved. This is
/// deliberately conservative: non-counter trailing groups fail closed to
/// unmatched (see [`derive_upload_query`]) rather than risk a wrong
/// studio-release pick.
fn is_part_counter_content(inner: &str) -> bool {
    let Some((numerator, denominator)) = inner.split_once('/') else {
        return false;
    };
    if denominator.contains('/') {
        return false;
    }
    !numerator.is_empty()
        && !denominator.is_empty()
        && numerator.bytes().all(|b| b.is_ascii_digit())
        && denominator.bytes().all(|b| b.is_ascii_digit())
}

/// Index of the bracket that matches the trailing close bracket,
/// scanning backwards so nested same-form pairs (`[[01/05]]`) resolve
/// to the outer opener. Other bracket forms are ignored at this level.
fn find_matching_open(text: &str, open: u8, close: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0;
    for (index, &byte) in bytes.iter().enumerate().rev() {
        if byte == close {
            depth += 1;
        } else if byte == open {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
            if depth < 0 {
                return None;
            }
        }
    }
    None
}

/// Strip one trailing recognized counter (`[01/05]`, `(1/3)`, or one of
/// those wrapped in a single extra bracket pair such as `[(1/3)]` or
/// `[[01/05]]`). Returns the text before the counter, or `None` when
/// the trailing bracketed group is identity and stripping must stop.
fn strip_trailing_counter(trimmed: &str) -> Option<&str> {
    let (open, close) = if trimmed.ends_with(']') {
        (b'[', b']')
    } else if trimmed.ends_with(')') {
        (b'(', b')')
    } else {
        return None;
    };
    let open_index = find_matching_open(trimmed, open, close)?;
    let inner = &trimmed[open_index + 1..trimmed.len() - 1];
    if is_part_counter_content(inner) {
        return Some(&trimmed[..open_index]);
    }
    // One extra wrapping pair: the outer content is itself a bracketed
    // counter in either form.
    if inner.len() >= 2
        && ((inner.starts_with('[') && inner.ends_with(']'))
            || (inner.starts_with('(') && inner.ends_with(')')))
    {
        let wrapped = &inner[1..inner.len() - 1];
        if is_part_counter_content(wrapped) {
            return Some(&trimmed[..open_index]);
        }
    }
    None
}

/// Derive a candidate release query from NZB file subjects.
///
/// Scene-style subjects look like
/// `Artist - Album [01/05] - "01 track.flac" yEnc (1/3)`: the release
/// title is the text before the quoted file name, minus trailing numeric
/// part counters. Only recognized `digits/digits` counters (repeated,
/// stacked across bracket forms, or wrapped in one extra bracket pair)
/// and a trailing `yEnc` token strip; edition suffixes are preserved.
/// Dots/underscores become spaces and one trailing file-type
/// token is dropped for bare-filename subjects. Anything that does not
/// survive [`validate_match_query`] yields `None` (the job is then written
/// unmatched rather than guessed at).
///
/// Intended narrowing: every other trailing bracket
/// group is kept as identity, not just editions — format/bitrate tags
/// (`[FLAC]`, `(MP3)`, `[320]`), years (`(2024)`), disc markers and
/// malformed counter look-alikes all survive, so such subjects fail
/// closed to unmatched (`None`) where the old strip-everything behavior
/// matched the studio release. That is the accepted tradeoff: an
/// unmatched write converges cleanly on replay (the library upsert is
/// `DO UPDATE` on the idempotency key and re-resolves deterministically),
/// while a wrong studio MBID would persist as a false match.
pub fn derive_upload_query(subjects: &[String]) -> Option<String> {
    let first = subjects.first()?;
    let before_quote = first.split('"').next().unwrap_or(first);
    let spaced: String = before_quote
        .chars()
        .map(|c| if c == '.' || c == '_' { ' ' } else { c })
        .collect();
    let mut candidate = spaced.trim().trim_end_matches('-').trim().to_string();
    // Drop trailing recognized counters and "yEnc", repeatedly, stopping
    // at the first trailing bracketed group that is not a counter.
    loop {
        let trimmed = candidate.trim_end().to_string();
        let stripped: Option<String> = if let Some(head) = strip_trailing_counter(&trimmed) {
            Some(head.to_string())
        } else if trimmed
            .rsplit_once(char::is_whitespace)
            .is_some_and(|(_, tail)| tail.eq_ignore_ascii_case("yenc"))
        {
            trimmed
                .rsplit_once(char::is_whitespace)
                .map(|(head, _)| head.to_string())
        } else {
            None
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
        // A search 404 is a route miss / version skew, never a
        // no-match (the no-match is a 200 with empty results): retry.
        assert_eq!(
            parse_mb_search_response(404, "{}"),
            Err(MatchFailure::Transient)
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
    fn upload_query_preserves_format_and_year_tags_fail_closed() {
        // Pinned tradeoff: non-counter trailing groups are identity, so
        // these subjects keep the tag and fail closed to unmatched rather
        // than matching the studio release as strip-everything did.
        for (subject, expected) in [
            (
                r#"Artist - Album [FLAC] [01/05] - "01.flac" yEnc (1/3)"#,
                "Artist - Album [FLAC]",
            ),
            (
                r#"Artist - Album (2024) [01/05] - "01.flac""#,
                "Artist - Album (2024)",
            ),
            (
                r#"Artist - Album [320] [01/05] - "01.flac""#,
                "Artist - Album [320]",
            ),
            (
                r#"Artist - Album (MP3) [01/05] - "01.flac""#,
                "Artist - Album (MP3)",
            ),
            (
                r#"Artist - Album [01/05] [FLAC] - "01.flac""#,
                "Artist - Album [01/05] [FLAC]",
            ),
        ] {
            let query = derive_upload_query(&[subject.to_string()])
                .expect("tagged subject must still derive its tagged query");
            assert_eq!(query.as_str(), expected, "subject {subject:?}");
            let studio_only = vec![hit_by(MBID_A, "Album", Some("Artist"), 0.99)];
            assert_eq!(
                pick_release_mbid(&query, &studio_only),
                None,
                "query {query:?} must stay unmatched, never pick studio"
            );
        }
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
            format!("https://mb-api.invalid/v1/mb/release/{MBID_A}/recordings")
        );
    }

    #[test]
    fn recordings_response_is_sorted_into_track_order() {
        // Wire order is not trusted: track 2 arrives before track 1, an
        // untracked entry and a malformed MBID are mixed in, and track 1
        // is duplicated. Disc 2 track 1 must land after all of disc 1,
        // not interleaved with it.
        const REC1: &str = "11111111-1111-4111-8111-111111111111";
        const REC2: &str = "22222222-2222-4222-8222-222222222222";
        const REC_UNTRACKED: &str = "33333333-3333-4333-8333-333333333333";
        const REC_D2T1: &str = "44444444-4444-4444-8444-444444444444";
        let body = format!(
            r#"{{"release_mbid":"{MBID_A}","recordings":[
                {{"mbid":"{REC2}","title":"Second","track_no":2,"medium_no":1}},
                {{"mbid":"{REC_UNTRACKED}","title":"Bonus","track_no":null,"medium_no":null}},
                {{"mbid":"{REC_D2T1}","title":"Disc two opener","track_no":1,"medium_no":2}},
                {{"mbid":"not-an-mbid","title":"Bogus","track_no":1,"medium_no":1}},
                {{"mbid":"{REC1}","title":"First","track_no":1,"medium_no":1}},
                {{"mbid":"{REC1}","title":"First again","track_no":1,"medium_no":1}}
            ]}}"#
        );
        let mbids = parse_mb_recordings_response(200, &body).unwrap();
        assert_eq!(mbids, vec![REC1, REC2, REC_D2T1, REC_UNTRACKED]);
    }

    #[test]
    fn recordings_response_classification_mirrors_search() {
        assert!(parse_mb_recordings_response(
            200,
            &format!(r#"{{"release_mbid":"{MBID_A}","recordings":[]}}"#)
        )
        .unwrap()
        .is_empty());
        // A 404 retries: the canonical `not_found` body is shared by
        // route misses and genuinely missing releases, so it proves
        // neither. Every other 4xx stays permanent.
        assert_eq!(
            parse_mb_recordings_response(404, "{}"),
            Err(MatchFailure::Transient)
        );
        assert_eq!(
            parse_mb_recordings_response(
                404,
                r#"{"code":"not_found","message":"not found","retryable":false}"#
            ),
            Err(MatchFailure::Transient)
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
