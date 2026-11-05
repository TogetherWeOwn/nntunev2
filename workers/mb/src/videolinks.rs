//! Video-link curation pipeline (N-videolinks).
//!
//! Harvest candidate YouTube IDs from MusicBrainz URL relations (W6b
//! mirror), Wikidata P1651, ListenBrainz-observed links and moderated
//! user submissions; batch-validate them with `videos.list`; expire
//! API-returned data after 30 days; and report coverage weighted by
//! **played** tracks.
//!
//! Guardrails (AGENTS.md clean-room + YouTube + MetaBrainz checklist):
//! - Play time never calls `search.list`. Harvest and ahead-of-time
//!   `videos.list` validation are the only YouTube Data API shapes in
//!   this module; there is no search client here at all.
//! - Thumbnails are never proxied, cached or stored. [`ValidatedVideo`]
//!   carries no thumbnail field and the response parser ignores any
//!   thumbnail payload the API returns.
//! - Only video IDs are stored long-term. Every other API-returned field
//!   is TTL'd (30 days) and must be refreshed or deleted past expiry.
//! - The mirror stays CC0-only: user submissions and ListenBrainz
//!   observations live in the app-side curation tables
//!   (`migrations/0035_videolinks_curation.sql`), never in `mb_*`.
//!
//! Parsing and policy helpers live here; [`validation`] adds a bounded
//! runner and app-side PostgreSQL adapter. No runtime route, schedule or
//! credential-bound HTTP adapter activates them yet. The query builder
//! omits the API key; credential installation and live validation are not
//! established by fixture-based tests. See `docs/videolinks-pipeline.md`
//! for the remaining integration work.

pub mod validation;

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// How many IDs fit in one `videos.list` call (`id` takes at most 50).
pub const VIDEOS_LIST_MAX_IDS: usize = 50;

/// TTL for API-returned validation data: 30 days, in seconds.
pub const VALIDATION_TTL_S: i64 = 30 * 24 * 60 * 60;

/// Length of a YouTube video ID (11 URL-safe base64 characters).
pub const VIDEO_ID_LEN: usize = 11;

/// Where a candidate video ID came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoSource {
    /// MusicBrainz URL relationship from the W6b CC0 mirror.
    MbUrl,
    /// Wikidata `P1651` (YouTube video ID) statement.
    Wikidata,
    /// Link observed in ListenBrainz data (CC0 dumps only).
    ListenBrainz,
    /// Moderated user submission (approved only).
    UserSubmission,
}

impl VideoSource {
    /// Harvest priority, lowest wins: curated mirror first, user
    /// submissions last (they still need approval + validation).
    pub fn priority(self) -> u8 {
        match self {
            VideoSource::MbUrl => 0,
            VideoSource::Wikidata => 1,
            VideoSource::ListenBrainz => 2,
            VideoSource::UserSubmission => 3,
        }
    }

    /// Stable string for the app-side curation tables.
    pub fn as_str(self) -> &'static str {
        match self {
            VideoSource::MbUrl => "mb_url",
            VideoSource::Wikidata => "wikidata",
            VideoSource::ListenBrainz => "listenbrainz",
            VideoSource::UserSubmission => "user_submission",
        }
    }
}

/// One harvested candidate: a recording paired with a video ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub recording_mbid: String,
    pub video_id: String,
    pub source: VideoSource,
}

/// True when `id` is an 11-character YouTube video ID.
pub fn is_valid_video_id(id: &str) -> bool {
    id.len() == VIDEO_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Extract the video ID from a YouTube watch / share / embed URL.
///
/// Accepts `youtube.com/watch?v=`, `youtu.be/`, `/embed/`, `/shorts/`,
/// `/live/` and `music.youtube.com` hosts. Returns `None` for
/// non-YouTube hosts, playlists without an ID, and malformed IDs.
pub fn extract_video_id(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, rest) = split_host(url)?;
    if !is_youtube_host(host) {
        return None;
    }
    if host == "youtu.be" {
        let id = rest
            .trim_start_matches('/')
            .split(['?', '#', '/'])
            .next()
            .unwrap_or("");
        return valid_id(id);
    }
    // Path forms: /embed/<id>, /shorts/<id>, /live/<id>.
    for prefix in ["/embed/", "/shorts/", "/live/"] {
        if let Some(after) = rest
            .split(['?', '#'])
            .next()
            .and_then(|p| p.strip_prefix(prefix))
        {
            let id = after.split('/').next().unwrap_or("");
            return valid_id(id);
        }
    }
    // Query form: ?v=<id> or &v=<id> (watch, attribution, music).
    for part in rest.split(['?', '#', '&']) {
        if let Some(v) = part.strip_prefix("v=") {
            let id = v.split(['/', ';']).next().unwrap_or("");
            return valid_id(id);
        }
    }
    None
}

fn split_host(url: &str) -> Option<(&str, &str)> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    match after_scheme.find('/') {
        Some(i) => Some((&after_scheme[..i], &after_scheme[i..])),
        None => Some((after_scheme, "/")),
    }
}

fn is_youtube_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "youtube.com"
        || host == "www.youtube.com"
        || host == "m.youtube.com"
        || host == "music.youtube.com"
        || host == "youtu.be"
        || host == "youtube-nocookie.com"
        || host == "www.youtube-nocookie.com"
}

fn valid_id(id: &str) -> Option<String> {
    if is_valid_video_id(id) {
        Some(id.to_string())
    } else {
        None
    }
}

/// Dedupe candidates on `(recording_mbid, video_id)`, keeping the
/// highest-priority source (see [`VideoSource::priority`]).
pub fn dedupe_candidates(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut best: HashMap<(String, String), Candidate> = HashMap::new();
    for c in candidates {
        let key = (c.recording_mbid.clone(), c.video_id.clone());
        match best.get(&key) {
            Some(existing) if existing.source.priority() <= c.source.priority() => {}
            _ => {
                best.insert(key, c);
            }
        }
    }
    let mut out: Vec<Candidate> = best.into_values().collect();
    out.sort_by(|a, b| {
        a.recording_mbid
            .cmp(&b.recording_mbid)
            .then(a.video_id.cmp(&b.video_id))
    });
    out
}

/// Split IDs into `videos.list`-sized batches (at most 50 per call).
pub fn validation_batches(ids: &[String]) -> Vec<Vec<String>> {
    ids.chunks(VIDEOS_LIST_MAX_IDS)
        .map(|chunk| chunk.to_vec())
        .collect()
}

/// Query parameters for one `videos.list` batch call. The API key is not
/// included here; callers append it from runtime config at the edge.
/// Only validation parts are requested (`status`, `contentDetails`,
/// `snippet` for the moderation display). No search endpoint is ever
/// addressed from this module.
pub fn videos_list_query(ids: &[String]) -> String {
    format!(
        "part=status%2CcontentDetails%2Csnippet&id={}&maxResults=50",
        ids.join("%2C")
    )
}

/// A validated video: the minimum needed to serve the embed. There is
/// deliberately no thumbnail field — thumbnails are never proxied,
/// cached or stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedVideo {
    pub video_id: String,
    pub title: String,
    pub channel_id: String,
    pub duration_secs: u32,
    pub embeddable: bool,
    pub privacy_status: String,
}

/// Usable on a video page: public (or unlisted) and embeddable.
pub fn is_playable(v: &ValidatedVideo) -> bool {
    v.embeddable && (v.privacy_status == "public" || v.privacy_status == "unlisted")
}

#[derive(Debug, Deserialize)]
struct VideosListResponse {
    items: Vec<VideosListItem>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct VideosListItem {
    id: String,
    #[serde(default)]
    snippet: Option<Snippet>,
    #[serde(default)]
    status: Option<Status>,
    #[serde(default)]
    #[serde(rename = "contentDetails")]
    content_details: Option<ContentDetails>,
}

#[derive(Debug, Deserialize)]
struct Snippet {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    #[serde(rename = "channelId")]
    channel_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Status {
    #[serde(default)]
    embeddable: Option<bool>,
    #[serde(default)]
    #[serde(rename = "privacyStatus")]
    privacy_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentDetails {
    #[serde(default)]
    duration: Option<String>,
}

/// Parse one `videos.list` JSON response. Unknown fields (including any
/// thumbnail payload) are ignored and never stored.
pub fn parse_videos_list_response(body: &str) -> Result<Vec<ValidatedVideo>, String> {
    let resp: VideosListResponse =
        serde_json::from_str(body).map_err(|_| "invalid videos.list body".to_string())?;
    if resp.error.is_some() || resp.items.len() > VIDEOS_LIST_MAX_IDS {
        return Err("invalid videos.list response".to_string());
    }
    resp.items
        .into_iter()
        .map(|item| {
            if !is_valid_video_id(&item.id) {
                return Err(format!("invalid video id in response: {}", item.id));
            }
            Ok(ValidatedVideo {
                video_id: item.id,
                title: item
                    .snippet
                    .as_ref()
                    .and_then(|s| s.title.clone())
                    .unwrap_or_default(),
                channel_id: item
                    .snippet
                    .as_ref()
                    .and_then(|s| s.channel_id.clone())
                    .unwrap_or_default(),
                duration_secs: item
                    .content_details
                    .as_ref()
                    .and_then(|c| c.duration.as_deref())
                    .map(parse_iso8601_duration)
                    .unwrap_or(0),
                embeddable: item
                    .status
                    .as_ref()
                    .and_then(|s| s.embeddable)
                    .unwrap_or(false),
                privacy_status: item
                    .status
                    .as_ref()
                    .and_then(|s| s.privacy_status.clone())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// Parse an ISO-8601 duration (`PT4M13S`) into seconds. Returns 0 for
/// missing or malformed input (validation treats it as unknown, not fatal).
pub fn parse_iso8601_duration(s: &str) -> u32 {
    let body = match s.strip_prefix("PT") {
        Some(b) => b,
        None => return 0,
    };
    let mut total: u32 = 0;
    let mut num = String::new();
    for ch in body.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
            continue;
        }
        let n: u32 = num.parse().unwrap_or(0);
        num.clear();
        match ch {
            'H' => total = total.saturating_add(n.saturating_mul(3600)),
            'M' => total = total.saturating_add(n.saturating_mul(60)),
            'S' => total = total.saturating_add(n),
            _ => return 0,
        }
    }
    if num.is_empty() {
        total
    } else {
        0
    }
}

/// True when API-returned data from `validated_at` is still within the
/// 30-day TTL at `now` (both unix seconds).
pub fn is_fresh(validated_at_unix: i64, now_unix: i64) -> bool {
    now_unix >= validated_at_unix && now_unix - validated_at_unix < VALIDATION_TTL_S
}

/// Expiry (unix seconds) for data validated at `validated_at_unix`.
pub fn expires_at(validated_at_unix: i64) -> i64 {
    validated_at_unix.saturating_add(VALIDATION_TTL_S)
}

/// User-submission moderation state. Terminal states are immutable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionStatus {
    Pending,
    Approved,
    Rejected,
}

impl SubmissionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SubmissionStatus::Pending => "pending",
            SubmissionStatus::Approved => "approved",
            SubmissionStatus::Rejected => "rejected",
        }
    }
}

/// Moderation error for a user submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModerationError {
    /// The text is neither a video ID nor a recognised YouTube URL.
    NotAYoutubeLink,
    /// The transition is not allowed (e.g. re-deciding a terminal state).
    IllegalTransition,
}

/// Accept a raw user submission: either an 11-character video ID or a
/// YouTube URL. Returns the normalised video ID.
pub fn normalise_submission(raw: &str) -> Result<String, ModerationError> {
    let raw = raw.trim();
    if is_valid_video_id(raw) {
        return Ok(raw.to_string());
    }
    extract_video_id(raw).ok_or(ModerationError::NotAYoutubeLink)
}

/// Decide a pending submission. Approved and rejected submissions are
/// terminal: re-deciding them is an error.
pub fn decide_submission(
    current: SubmissionStatus,
    approve: bool,
) -> Result<SubmissionStatus, ModerationError> {
    match current {
        SubmissionStatus::Pending => Ok(if approve {
            SubmissionStatus::Approved
        } else {
            SubmissionStatus::Rejected
        }),
        SubmissionStatus::Approved | SubmissionStatus::Rejected => {
            Err(ModerationError::IllegalTransition)
        }
    }
}

/// Coverage of the catalogue: share of recordings with at least one
/// fresh validated video link, plus the played-weighted share.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub total_recordings: u64,
    pub linked_recordings: u64,
    /// `linked / total` (0 when the catalogue is empty).
    pub coverage_ratio: f64,
    /// Play-weighted share, or `None` when no play data exists yet
    /// (the N-plays feed this pipeline's weighting depends on).
    pub played_coverage_ratio: Option<f64>,
}

/// Compute coverage from totals. `linked_ids` holds recording MBIDs with
/// at least one fresh validated link; `plays` maps recording MBID to
/// play count (empty when N-plays data is unavailable).
pub fn compute_coverage(
    total_recordings: u64,
    linked_ids: &HashSet<String>,
    plays: &HashMap<String, u64>,
) -> CoverageReport {
    let linked_recordings = linked_ids.len() as u64;
    let coverage_ratio = if total_recordings == 0 {
        0.0
    } else {
        linked_recordings as f64 / total_recordings as f64
    };
    let played_coverage_ratio = if plays.is_empty() {
        None
    } else {
        let total_plays: u64 = plays.values().sum();
        if total_plays == 0 {
            None
        } else {
            let linked_plays: u64 = plays
                .iter()
                .filter(|(mbid, _)| linked_ids.contains(mbid.as_str()))
                .map(|(_, c)| c)
                .sum();
            Some(linked_plays as f64 / total_plays as f64)
        }
    };
    CoverageReport {
        total_recordings,
        linked_recordings,
        coverage_ratio,
        played_coverage_ratio,
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn extracts_watch_and_share_forms() {
        assert_eq!(
            extract_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".into())
        );
        assert_eq!(
            extract_video_id("https://youtu.be/dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".into())
        );
        assert_eq!(
            extract_video_id("https://www.youtube.com/embed/dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".into())
        );
        assert_eq!(
            extract_video_id("https://www.youtube.com/shorts/dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".into())
        );
        assert_eq!(
            extract_video_id("https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=RDAMVMx"),
            Some("dQw4w9WgXcQ".into())
        );
    }

    #[test]
    fn rejects_non_youtube_and_bad_ids() {
        assert_eq!(
            extract_video_id("https://example.com/watch?v=dQw4w9WgXcQ"),
            None
        );
        assert_eq!(
            extract_video_id("https://www.youtube.com/playlist?list=PL1234567890123456"),
            None
        );
        assert_eq!(
            extract_video_id("https://www.youtube.com/watch?v=short"),
            None
        );
        assert_eq!(extract_video_id("not a url"), None);
    }

    #[test]
    fn dedupe_keeps_highest_priority_source() {
        let out = dedupe_candidates(vec![
            Candidate {
                recording_mbid: "r1".into(),
                video_id: "dQw4w9WgXcQ".into(),
                source: VideoSource::ListenBrainz,
            },
            Candidate {
                recording_mbid: "r1".into(),
                video_id: "dQw4w9WgXcQ".into(),
                source: VideoSource::MbUrl,
            },
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, VideoSource::MbUrl);
    }

    #[test]
    fn batches_cap_at_fifty() {
        let ids: Vec<String> = (0..125).map(|i| format!("id{i:09}")).collect();
        let batches = validation_batches(&ids);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 50);
        assert_eq!(batches[2].len(), 25);
    }

    #[test]
    fn ttl_boundaries() {
        assert!(is_fresh(1_000_000, 1_000_000 + VALIDATION_TTL_S - 1));
        assert!(!is_fresh(1_000_000, 1_000_000 + VALIDATION_TTL_S));
        assert!(!is_fresh(2_000_000, 1_000_000));
        assert_eq!(expires_at(1_000_000), 1_000_000 + VALIDATION_TTL_S);
    }

    #[test]
    fn parses_videos_list_and_ignores_thumbnails() {
        let body = r#"{"items": [{"id": "dQw4w9WgXcQ",
            "snippet": {"title": "t", "channelId": "c",
              "thumbnails": {"default": {"url": "https://example.invalid/t.jpg"}}},
            "status": {"embeddable": true, "privacyStatus": "public"},
            "contentDetails": {"duration": "PT4M13S"}}]}"#;
        let videos = parse_videos_list_response(body).unwrap();
        assert_eq!(videos.len(), 1);
        assert_eq!(videos[0].duration_secs, 253);
        assert!(is_playable(&videos[0]));
    }

    #[test]
    fn moderation_flow() {
        assert_eq!(
            normalise_submission("dQw4w9WgXcQ"),
            Ok("dQw4w9WgXcQ".into())
        );
        assert_eq!(
            normalise_submission("https://youtu.be/dQw4w9WgXcQ"),
            Ok("dQw4w9WgXcQ".into())
        );
        assert_eq!(
            normalise_submission("https://example.com/x"),
            Err(ModerationError::NotAYoutubeLink)
        );
        assert_eq!(
            decide_submission(SubmissionStatus::Pending, true),
            Ok(SubmissionStatus::Approved)
        );
        assert_eq!(
            decide_submission(SubmissionStatus::Approved, false),
            Err(ModerationError::IllegalTransition)
        );
    }

    #[test]
    fn coverage_with_and_without_plays() {
        let linked: HashSet<String> = ["a".to_string()].into_iter().collect();
        let no_plays: HashMap<String, u64> = HashMap::new();
        let r = compute_coverage(4, &linked, &no_plays);
        assert_eq!(r.coverage_ratio, 0.25);
        assert_eq!(r.played_coverage_ratio, None);

        let plays: HashMap<String, u64> = [("a".to_string(), 90), ("b".to_string(), 10)]
            .into_iter()
            .collect();
        let r = compute_coverage(4, &linked, &plays);
        assert_eq!(r.played_coverage_ratio, Some(0.9));
    }
}
