//! Official-media selection: OMV/ATV gating for the YouTube backend.
//!
//! Host research (2026-10-21) selection rules, encoded as pure decision
//! logic (no network; the Worker entry does the I/O):
//!
//! - Key each recording by MBID + ISRC; resolve the ISRC first to the
//!   canonical artist, title and duration.
//! - Search YouTube Music `songs` and `videos`, keeping only
//!   `musicVideoType` OMV (official music video) and ATV (the official
//!   audio track), read from
//!   `watchEndpointMusicConfig.musicVideoType`.
//! - Gate by a `channelId` allowlist plus the official-artist/verified
//!   badges. NEVER "Vevo"/"Topic" substring matching.
//! - Prefer the ATV's `/next` counterpart as the OMV; otherwise take the
//!   top OMV passing duration (≈±5 s) and denylist checks.
//! - MusicBrainz video-recording links and Wikidata P1651 count only
//!   after a liveness check plus OMV re-verification. IMVDb is a
//!   candidate source and is always re-verified.
//! - Audio: the exact-recording ATV at the best available Opus (expect
//!   itag 251 for non-premium). Never take audio from the OMV. YouTube
//!   never beats NNTP FLAC; use it only where FLAC is missing.
//! - Caching: MBID→selection about 30 days, videoId→metadata about 7
//!   days. Revalidate on MusicBrainz edits or dead IDs.

use crate::player::StreamFormat;

/// Duration match tolerance: ±5 s against the canonical recording
/// duration. Anything further out is a different recording (or edit).
pub const DURATION_TOLERANCE_SECS: u64 = 5;

/// MBID→selection cache TTL: about 30 days.
pub const SELECTION_TTL_SECS: u64 = 30 * 24 * 3600;

/// videoId→metadata cache TTL: about 7 days.
pub const METADATA_TTL_SECS: u64 = 7 * 24 * 3600;

/// Preferred audio itag: Opus, expected for non-premium extractions.
pub const PREFERRED_AUDIO_ITAG: i64 = 251;

/// YouTube Music official-artist badge icon type.
const OFFICIAL_ARTIST_BADGE: &str = "OFFICIAL_ARTIST_BADGE";

/// Verified-channel badge icon type on music shelves. Badge shapes vary
/// by shelf; confirm against live YTM search responses during the
/// measurement run.
const VERIFIED_BADGE: &str = "CHECK_CIRCLE_THICK";

/// `watchEndpointMusicConfig.musicVideoType` classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MusicVideoType {
    /// Official music video.
    Omv,
    /// Official audio track (auto-generated exact recording).
    Atv,
    /// Any other declared type (UGC, live, ...), kept for logging.
    Other(String),
    /// Missing or unparseable.
    Unknown,
}

/// Parse `musicVideoType` out of a `watchEndpointMusicConfig` object.
pub fn parse_music_video_type(config: &serde_json::Value) -> MusicVideoType {
    match config
        .get("musicVideoType")
        .and_then(serde_json::Value::as_str)
    {
        Some("MUSIC_VIDEO_TYPE_OMV") => MusicVideoType::Omv,
        Some("MUSIC_VIDEO_TYPE_ATV") => MusicVideoType::Atv,
        Some(other) => MusicVideoType::Other(other.to_string()),
        None => MusicVideoType::Unknown,
    }
}

/// One YouTube Music `songs`/`videos` search result, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCandidate {
    /// `watchEndpoint.videoId` (falling back to `playlistItemData`).
    pub video_id: String,
    /// First `UC*` browse ID in the subtitle runs, when present.
    pub channel_id: Option<String>,
    /// OMV/ATV/other from `watchEndpointMusicConfig`.
    pub music_video_type: MusicVideoType,
    /// Carries the official-artist badge.
    pub official_artist: bool,
    /// Carries a verified-channel badge.
    pub verified: bool,
    /// Shelf duration in seconds, when the shelf shows one.
    pub duration_secs: Option<u64>,
}

/// Parse one YTM search result (a bare renderer or one wrapped in
/// `musicResponsiveListItemRenderer` / `musicTwoRowItemRenderer`).
/// Returns `None` when no video ID survives; every other field degrades
/// to unknown and fails closed in [`gate_candidate`].
pub fn parse_search_candidate(item: &serde_json::Value) -> Option<SearchCandidate> {
    let renderer = item
        .get("musicResponsiveListItemRenderer")
        .or_else(|| item.get("musicTwoRowItemRenderer"))
        .unwrap_or(item);
    let watch = renderer.get("navigationEndpoint")?.get("watchEndpoint")?;
    let video_id = watch
        .get("videoId")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            renderer
                .get("playlistItemData")
                .and_then(|data| data.get("videoId"))
                .and_then(serde_json::Value::as_str)
        })?;
    let music_video_type = watch
        .get("watchEndpointMusicConfig")
        .map(parse_music_video_type)
        .unwrap_or(MusicVideoType::Unknown);
    let badges = badge_icon_types(renderer);
    Some(SearchCandidate {
        video_id: video_id.to_string(),
        channel_id: channel_from_runs(renderer),
        music_video_type,
        official_artist: badges.contains(&OFFICIAL_ARTIST_BADGE),
        verified: badges.contains(&VERIFIED_BADGE),
        duration_secs: duration_from_renderer(renderer),
    })
}

/// First `UC*` channel browse ID in the flex-column subtitle runs.
fn channel_from_runs(renderer: &serde_json::Value) -> Option<String> {
    renderer
        .get("flexColumns")?
        .as_array()?
        .iter()
        .filter_map(|column| {
            column
                .get("musicResponsiveListItemFlexColumnRenderer")?
                .get("text")?
                .get("runs")?
                .as_array()
        })
        .flat_map(|runs| runs.iter())
        .filter_map(|run| {
            run.get("navigationEndpoint")?
                .get("browseEndpoint")?
                .get("browseId")?
                .as_str()
        })
        .find(|id| id.starts_with("UC"))
        .map(str::to_string)
}

/// Badge icon types from `badges`, `subtitleBadges` (inline music
/// badges) and `ownerBadges` (metadata badges).
fn badge_icon_types(renderer: &serde_json::Value) -> Vec<&str> {
    let mut icons = Vec::new();
    for key in ["badges", "subtitleBadges"] {
        if let Some(list) = renderer.get(key).and_then(serde_json::Value::as_array) {
            icons.extend(list.iter().filter_map(|badge| {
                badge
                    .get("musicInlineBadgeRenderer")?
                    .get("icon")?
                    .get("iconType")?
                    .as_str()
            }));
        }
    }
    if let Some(list) = renderer
        .get("ownerBadges")
        .and_then(serde_json::Value::as_array)
    {
        icons.extend(list.iter().filter_map(|badge| {
            badge
                .get("metadataBadgeRenderer")?
                .get("icon")?
                .get("iconType")?
                .as_str()
        }));
    }
    icons
}

/// All run texts in one column kind, in shelf order.
fn column_texts<'a>(
    renderer: &'a serde_json::Value,
    columns_key: &str,
    column_renderer_key: &str,
) -> Vec<&'a str> {
    let mut texts = Vec::new();
    if let Some(columns) = renderer
        .get(columns_key)
        .and_then(serde_json::Value::as_array)
    {
        for column in columns {
            if let Some(runs) = column
                .get(column_renderer_key)
                .and_then(|column| column.get("text"))
                .and_then(|text| text.get("runs"))
                .and_then(serde_json::Value::as_array)
            {
                texts.extend(
                    runs.iter()
                        .filter_map(|run| run.get("text").and_then(serde_json::Value::as_str)),
                );
            }
        }
    }
    texts
}

/// Shelf duration: the last duration-shaped run in the fixed columns,
/// falling back to the flex columns only when the fixed columns yield
/// nothing. Fixed-first matters because flex subtitle runs carry bare
/// numbers (years, album metadata) that parse as durations and would
/// otherwise shadow the real shelf duration.
fn duration_from_renderer(renderer: &serde_json::Value) -> Option<u64> {
    let fixed = column_texts(
        renderer,
        "fixedColumns",
        "musicResponsiveListItemFixedColumnRenderer",
    );
    for text in fixed.iter().rev() {
        if let Some(duration) = parse_duration(text) {
            return Some(duration);
        }
    }
    let flex = column_texts(
        renderer,
        "flexColumns",
        "musicResponsiveListItemFlexColumnRenderer",
    );
    for text in flex.iter().rev() {
        if let Some(duration) = parse_duration(text) {
            return Some(duration);
        }
    }
    None
}

/// Parse a shelf duration (`m:ss`, `h:mm:ss` or bare seconds).
pub fn parse_duration(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut total = 0u64;
    for part in &parts {
        let unit: u64 = part.parse().ok()?;
        total = total.checked_mul(60)?.checked_add(unit)?;
    }
    Some(total)
}

/// The allow/deny inputs to [`gate_candidate`]. Slices borrow the
/// Worker's KV-backed config; the spike hard-codes a small allowlist.
#[derive(Debug, Clone, Copy)]
pub struct SelectionPolicy<'a> {
    /// Channel IDs eligible for official selection.
    pub allowed_channels: &'a [&'a str],
    /// Video IDs that must never be selected.
    pub denylisted_videos: &'a [&'a str],
    /// Channel IDs that must never be selected.
    pub denylisted_channels: &'a [&'a str],
}

/// Why a candidate was rejected. Order is deliberate: type first (the
/// NEVER-substring-match rule means type is the only membership proof),
/// then denylist, channel, badges, duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Not OMV/ATV. Title/author substrings ("VEVO", "- Topic") never
    /// count; only `musicVideoType` admits a candidate.
    WrongType,
    /// Channel missing or outside the allowlist.
    ChannelNotAllowed,
    /// Neither the official-artist nor a verified badge.
    MissingBadge,
    /// Duration unknown or further than ±5 s from canonical.
    DurationMismatch,
    /// On the video or channel denylist.
    Denylisted,
}

/// Gate one candidate against the selection rules. `expected_duration_secs`
/// is the ISRC-resolved canonical duration; unknown durations fail closed.
pub fn gate_candidate(
    candidate: &SearchCandidate,
    expected_duration_secs: Option<u64>,
    policy: &SelectionPolicy,
) -> Result<(), Rejection> {
    if !matches!(
        candidate.music_video_type,
        MusicVideoType::Omv | MusicVideoType::Atv
    ) {
        return Err(Rejection::WrongType);
    }
    if policy
        .denylisted_videos
        .contains(&candidate.video_id.as_str())
    {
        return Err(Rejection::Denylisted);
    }
    if let Some(channel) = candidate.channel_id.as_deref() {
        // Denylist first: a denylisted channel reports Denylisted even
        // when it is outside the allowlist, so the rejection label stays
        // truthful for the per-layer hit/escape telemetry.
        if policy.denylisted_channels.contains(&channel) {
            return Err(Rejection::Denylisted);
        }
        if !policy.allowed_channels.contains(&channel) {
            return Err(Rejection::ChannelNotAllowed);
        }
    } else {
        return Err(Rejection::ChannelNotAllowed);
    }
    if !(candidate.official_artist || candidate.verified) {
        return Err(Rejection::MissingBadge);
    }
    match (candidate.duration_secs, expected_duration_secs) {
        (Some(have), Some(want)) if have.abs_diff(want) <= DURATION_TOLERANCE_SECS => Ok(()),
        _ => Err(Rejection::DurationMismatch),
    }
}

/// Pick the OMV for a recording: the ATV's `/next` counterpart when it
/// re-verifies as an OMV, else the top-ranked OMV passing the gate.
/// `candidates` must be in shelf rank order.
pub fn select_omv<'a>(
    counterpart: Option<&'a SearchCandidate>,
    candidates: &'a [SearchCandidate],
    expected_duration_secs: Option<u64>,
    policy: &SelectionPolicy,
) -> Option<&'a SearchCandidate> {
    if let Some(verified) = counterpart.filter(|c| {
        c.music_video_type == MusicVideoType::Omv
            && gate_candidate(c, expected_duration_secs, policy).is_ok()
    }) {
        return Some(verified);
    }
    candidates.iter().find(|c| {
        c.music_video_type == MusicVideoType::Omv
            && gate_candidate(c, expected_duration_secs, policy).is_ok()
    })
}

/// Provenance of an out-of-band video hint. Every source re-verifies;
/// there is no trusted shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalHintSource {
    /// MusicBrainz recording→video URL relationship.
    MusicBrainzLink,
    /// Wikidata P1651 (music-video role) statement.
    WikidataP1651,
    /// IMVDb match (key required, beta).
    Imvdb,
}

/// Admit an external hint only after a liveness check plus OMV
/// re-verification. The `source` pins the call site's provenance for
/// logging; the rule is the same for all three.
pub fn admit_external_hint(
    _source: &ExternalHintSource,
    liveness_ok: bool,
    omv_reverified: bool,
) -> bool {
    liveness_ok && omv_reverified
}

/// Which video the audio formats were extracted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSource {
    /// The exact-recording ATV: audio allowed.
    Atv,
    /// The OMV: audio never taken from here.
    Omv,
}

/// Pick the audio format: the best available Opus from the ATV
/// (itag 251 first, else the highest-bitrate Opus format; a missing
/// bitrate sorts as 0 and ties keep shelf order). Cipher-only formats
/// (no plain URL) are skipped: they cannot be fetched. Returns `None`
/// for an OMV source or when no fetchable Opus format exists.
pub fn select_audio_format(formats: &[StreamFormat], source: AudioSource) -> Option<&StreamFormat> {
    if !matches!(source, AudioSource::Atv) {
        return None;
    }
    let mut best: Option<&StreamFormat> = None;
    let mut best_bitrate = 0u64;
    for format in formats {
        let opus = format
            .mime
            .as_deref()
            .is_some_and(|mime| mime.contains("opus"));
        if !opus || format.url.is_none() {
            continue;
        }
        if format.itag == PREFERRED_AUDIO_ITAG {
            return Some(format);
        }
        let bitrate = format.bitrate.unwrap_or(0);
        if best.is_none() || bitrate > best_bitrate {
            best = Some(format);
            best_bitrate = bitrate;
        }
    }
    best
}

/// MBID→selection cache key (TTL [`SELECTION_TTL_SECS`]).
pub fn selection_cache_key(mbid: &str, isrc: &str) -> String {
    format!("yt:sel:{mbid}:{isrc}")
}

/// videoId→metadata cache key (TTL [`METADATA_TTL_SECS`]).
pub fn metadata_cache_key(video_id: &str) -> String {
    format!("yt:meta:{video_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::StreamFormat;
    use serde_json::json;

    /// A shape-faithful YouTube Music `songs`-shelf list item. Badge icon
    /// strings and the fixed-column duration are the live-shape guesses to
    /// confirm against real YTM search responses during measurement.
    fn omv_item() -> serde_json::Value {
        json!({
            "musicResponsiveListItemRenderer": {
                "playlistItemData": {"videoId": "abcdefghijk"},
                "flexColumns": [
                    {"musicResponsiveListItemFlexColumnRenderer": {
                        "text": {"runs": [{"text": "Song Title"}]}
                    }},
                    {"musicResponsiveListItemFlexColumnRenderer": {
                        "text": {"runs": [
                            {"text": "Artist Name",
                             "navigationEndpoint": {"browseEndpoint": {"browseId": "UCchannel12345678901234"}}},
                            {"text": " • "},
                            {"text": "Album Name"}
                        ]}
                    }}
                ],
                "fixedColumns": [
                    {"musicResponsiveListItemFixedColumnRenderer": {
                        "text": {"runs": [{"text": "3:45"}]}
                    }}
                ],
                "navigationEndpoint": {"watchEndpoint": {
                    "videoId": "abcdefghijk",
                    "watchEndpointMusicConfig": {"musicVideoType": "MUSIC_VIDEO_TYPE_OMV"}
                }},
                "subtitleBadges": [
                    {"musicInlineBadgeRenderer": {"icon": {"iconType": "OFFICIAL_ARTIST_BADGE"}}}
                ]
            }
        })
    }

    fn policy<'a>() -> SelectionPolicy<'a> {
        SelectionPolicy {
            allowed_channels: &["UCchannel12345678901234"],
            denylisted_videos: &[],
            denylisted_channels: &[],
        }
    }

    #[test]
    fn music_video_type_parses_omv_and_atv() {
        assert_eq!(
            parse_music_video_type(&json!({"musicVideoType": "MUSIC_VIDEO_TYPE_OMV"})),
            MusicVideoType::Omv
        );
        assert_eq!(
            parse_music_video_type(&json!({"musicVideoType": "MUSIC_VIDEO_TYPE_ATV"})),
            MusicVideoType::Atv
        );
        assert_eq!(
            parse_music_video_type(&json!({"musicVideoType": "MUSIC_VIDEO_TYPE_UGC"})),
            MusicVideoType::Other("MUSIC_VIDEO_TYPE_UGC".to_string())
        );
        assert_eq!(parse_music_video_type(&json!({})), MusicVideoType::Unknown);
    }

    #[test]
    fn candidate_parse_reads_video_channel_badges_duration() {
        let candidate = parse_search_candidate(&omv_item()).expect("parse");
        assert_eq!(candidate.video_id, "abcdefghijk");
        assert_eq!(
            candidate.channel_id.as_deref(),
            Some("UCchannel12345678901234")
        );
        assert_eq!(candidate.music_video_type, MusicVideoType::Omv);
        assert!(candidate.official_artist);
        assert_eq!(candidate.duration_secs, Some(225));
    }

    #[test]
    fn candidate_parse_missing_video_id_is_none() {
        let mut item = omv_item();
        item["musicResponsiveListItemRenderer"]["playlistItemData"] = json!({});
        item["musicResponsiveListItemRenderer"]["navigationEndpoint"]["watchEndpoint"]
            .as_object_mut()
            .expect("watch endpoint")
            .remove("videoId");
        assert_eq!(parse_search_candidate(&item), None);
    }

    #[test]
    fn duration_parses_minutes_seconds() {
        assert_eq!(parse_duration("3:45"), Some(225));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration("45"), Some(45));
        assert_eq!(parse_duration("live"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn fixed_duration_wins_over_numeric_flex_runs() {
        // A bare-number flex run (year metadata) must not shadow the
        // fixed-column shelf duration.
        let mut item = omv_item();
        item["musicResponsiveListItemRenderer"]["flexColumns"][1]
            ["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"]
            .as_array_mut()
            .expect("runs")
            .push(json!({"text": "2024"}));
        let candidate = parse_search_candidate(&item).expect("parse");
        assert_eq!(candidate.duration_secs, Some(225));
        assert_eq!(gate_candidate(&candidate, Some(225), &policy()), Ok(()));
    }

    #[test]
    fn duration_falls_back_to_flex_columns() {
        // TwoRow-style shelf items carry no fixed columns: the flex
        // duration is the shelf duration then.
        let mut item = omv_item();
        item["musicResponsiveListItemRenderer"]
            .as_object_mut()
            .expect("renderer")
            .remove("fixedColumns");
        item["musicResponsiveListItemRenderer"]["flexColumns"][0]
            ["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"] =
            json!([{"text": "4:05"}]);
        let candidate = parse_search_candidate(&item).expect("parse");
        assert_eq!(candidate.duration_secs, Some(245));
    }

    #[test]
    fn substring_matching_never_accepts() {
        // "VEVO" in the title with no OMV type: rejected.
        let vevo = SearchCandidate {
            video_id: "vevo1234567".to_string(),
            channel_id: Some("UCchannel12345678901234".to_string()),
            music_video_type: MusicVideoType::Other("MUSIC_VIDEO_TYPE_UGC".to_string()),
            official_artist: true,
            verified: false,
            duration_secs: Some(225),
        };
        assert_eq!(
            gate_candidate(&vevo, Some(225), &policy()),
            Err(Rejection::WrongType)
        );
        // "- Topic" author shape with no ATV type: rejected.
        let topic = SearchCandidate {
            video_id: "topic1234567".to_string(),
            channel_id: Some("UCchannel12345678901234".to_string()),
            music_video_type: MusicVideoType::Unknown,
            official_artist: false,
            verified: true,
            duration_secs: Some(225),
        };
        assert_eq!(
            gate_candidate(&topic, Some(225), &policy()),
            Err(Rejection::WrongType)
        );
    }

    #[test]
    fn allowlist_and_badges_gate() {
        let candidate = parse_search_candidate(&omv_item()).expect("parse");
        assert_eq!(gate_candidate(&candidate, Some(225), &policy()), Ok(()));

        let mut off_list = candidate.clone();
        off_list.channel_id = Some("UCstranger00000000000000".to_string());
        assert_eq!(
            gate_candidate(&off_list, Some(225), &policy()),
            Err(Rejection::ChannelNotAllowed)
        );

        let mut no_badges = candidate.clone();
        no_badges.official_artist = false;
        no_badges.verified = false;
        assert_eq!(
            gate_candidate(&no_badges, Some(225), &policy()),
            Err(Rejection::MissingBadge)
        );
    }

    #[test]
    fn duration_tolerance_is_plus_minus_five_seconds() {
        let candidate = parse_search_candidate(&omv_item()).expect("parse");
        assert_eq!(gate_candidate(&candidate, Some(220), &policy()), Ok(()));
        assert_eq!(gate_candidate(&candidate, Some(230), &policy()), Ok(()));
        assert_eq!(
            gate_candidate(&candidate, Some(219), &policy()),
            Err(Rejection::DurationMismatch)
        );
        assert_eq!(
            gate_candidate(&candidate, Some(231), &policy()),
            Err(Rejection::DurationMismatch)
        );
        // Unknown durations fail closed: cannot prove same recording.
        let mut unknown = candidate.clone();
        unknown.duration_secs = None;
        assert_eq!(
            gate_candidate(&unknown, Some(225), &policy()),
            Err(Rejection::DurationMismatch)
        );
    }

    #[test]
    fn denylisted_video_or_channel_rejected() {
        let candidate = parse_search_candidate(&omv_item()).expect("parse");
        let video_deny = SelectionPolicy {
            denylisted_videos: &["abcdefghijk"],
            ..policy()
        };
        assert_eq!(
            gate_candidate(&candidate, Some(225), &video_deny),
            Err(Rejection::Denylisted)
        );
        let channel_deny = SelectionPolicy {
            denylisted_channels: &["UCchannel12345678901234"],
            ..policy()
        };
        assert_eq!(
            gate_candidate(&candidate, Some(225), &channel_deny),
            Err(Rejection::Denylisted)
        );
    }

    #[test]
    fn denylisted_channel_reports_denylisted_off_allowlist() {
        // The denylist outranks the allowlist so the rejection label
        // stays truthful for the per-layer hit/escape telemetry.
        let candidate = parse_search_candidate(&omv_item()).expect("parse");
        let policy = SelectionPolicy {
            allowed_channels: &["UCsomeoneelse00000000000"],
            denylisted_videos: &[],
            denylisted_channels: &["UCchannel12345678901234"],
        };
        assert_eq!(
            gate_candidate(&candidate, Some(225), &policy),
            Err(Rejection::Denylisted)
        );
    }

    #[test]
    fn counterpart_preferred_when_verified() {
        let top = parse_search_candidate(&omv_item()).expect("parse");
        let mut counterpart = top.clone();
        counterpart.video_id = "counterpart1".to_string();
        let ranked = [top];
        let picked = select_omv(Some(&counterpart), &ranked, Some(225), &policy()).expect("select");
        assert_eq!(picked.video_id, "counterpart1");
    }

    #[test]
    fn unverified_counterpart_falls_back_to_top_omv() {
        let top = parse_search_candidate(&omv_item()).expect("parse");
        let mut bad_counterpart = top.clone();
        bad_counterpart.video_id = "counterpart1".to_string();
        bad_counterpart.music_video_type = MusicVideoType::Atv;
        let picked = select_omv(
            Some(&bad_counterpart),
            std::slice::from_ref(&top),
            Some(225),
            &policy(),
        )
        .expect("select");
        assert_eq!(picked.video_id, "abcdefghijk");
    }

    #[test]
    fn external_hints_need_liveness_and_reverification() {
        for source in [
            ExternalHintSource::MusicBrainzLink,
            ExternalHintSource::WikidataP1651,
            ExternalHintSource::Imvdb,
        ] {
            assert!(admit_external_hint(&source, true, true));
            assert!(!admit_external_hint(&source, false, true));
            assert!(!admit_external_hint(&source, true, false));
            assert!(!admit_external_hint(&source, false, false));
        }
    }

    fn opus_format(itag: i64, bitrate: Option<u64>) -> StreamFormat {
        StreamFormat {
            itag,
            bitrate,
            mime: Some("audio/webm; codecs=\"opus\"".to_string()),
            url: Some("https://host.test/videoplayback?ip=203.0.113.7".to_string()),
            has_signature_cipher: false,
            has_n_param: false,
            has_sabr: false,
            ip: Some("203.0.113.7".to_string()),
        }
    }

    #[test]
    fn audio_prefers_itag_251_opus() {
        let formats = vec![
            opus_format(249, Some(50_000)),
            opus_format(251, Some(160_000)),
            opus_format(250, Some(70_000)),
        ];
        let picked = select_audio_format(&formats, AudioSource::Atv).expect("audio");
        assert_eq!(picked.itag, 251);
    }

    #[test]
    fn audio_falls_back_to_highest_bitrate_opus() {
        // Shelf order carries no bitrate guarantee: the best wins, not
        // the first. A missing bitrate sorts as 0, below any present one.
        let formats = vec![
            opus_format(249, Some(50_000)),
            opus_format(250, None),
            opus_format(249, Some(70_000)),
        ];
        let picked = select_audio_format(&formats, AudioSource::Atv).expect("audio");
        assert_eq!(picked.itag, 249);
        assert_eq!(picked.bitrate, Some(70_000));
    }

    #[test]
    fn audio_never_from_omv() {
        let formats = vec![opus_format(251, Some(160_000))];
        assert_eq!(select_audio_format(&formats, AudioSource::Omv), None);
    }

    fn cipher_only_opus_format(itag: i64, bitrate: Option<u64>) -> StreamFormat {
        StreamFormat {
            itag,
            bitrate,
            mime: Some("audio/webm; codecs=\"opus\"".to_string()),
            url: None,
            has_signature_cipher: true,
            has_n_param: false,
            has_sabr: false,
            ip: None,
        }
    }

    #[test]
    fn audio_skips_cipher_only_251_for_fetchable_opus() {
        // A cipher-only 251 has no fetchable URL: it must not shadow a
        // usable format via the preferred-itag fast path.
        let formats = vec![
            cipher_only_opus_format(251, Some(160_000)),
            opus_format(249, Some(50_000)),
        ];
        let picked = select_audio_format(&formats, AudioSource::Atv).expect("audio");
        assert_eq!(picked.itag, 249);
        assert!(picked.url.is_some());
    }

    #[test]
    fn audio_cipher_only_high_bitrate_does_not_replace_usable_fallback() {
        // A cipher-only format must not win the highest-bitrate fallback
        // either, however large its advertised bitrate.
        let formats = vec![
            opus_format(249, Some(50_000)),
            cipher_only_opus_format(250, Some(999_999)),
        ];
        let picked = select_audio_format(&formats, AudioSource::Atv).expect("audio");
        assert_eq!(picked.itag, 249);
        assert!(picked.url.is_some());
    }

    #[test]
    fn audio_none_when_only_cipher_only_opus() {
        // Fail closed: cipher-only Opus with nothing fetchable yields no
        // audio, the same as no Opus at all.
        let formats = vec![cipher_only_opus_format(251, Some(160_000))];
        assert_eq!(select_audio_format(&formats, AudioSource::Atv), None);
    }

    #[test]
    fn audio_none_without_opus() {
        let formats = vec![StreamFormat {
            itag: 140,
            bitrate: Some(128_000),
            mime: Some("audio/mp4; codecs=\"mp4a.40.2\"".to_string()),
            url: Some("https://host.test/videoplayback?ip=203.0.113.7".to_string()),
            has_signature_cipher: false,
            has_n_param: false,
            has_sabr: false,
            ip: Some("203.0.113.7".to_string()),
        }];
        assert_eq!(select_audio_format(&formats, AudioSource::Atv), None);
    }

    #[test]
    fn cache_ttls_are_30d_and_7d() {
        assert_eq!(SELECTION_TTL_SECS, 30 * 24 * 3600);
        assert_eq!(METADATA_TTL_SECS, 7 * 24 * 3600);
        assert_eq!(
            selection_cache_key("mbid-1", "ISRC1"),
            "yt:sel:mbid-1:ISRC1"
        );
        assert_eq!(metadata_cache_key("abcdefghijk"), "yt:meta:abcdefghijk");
    }
}
