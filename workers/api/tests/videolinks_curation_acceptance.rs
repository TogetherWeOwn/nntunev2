//! Acceptance for the N-videolinks curation pipeline: harvest the W6b
//! mirror URLs into candidate video IDs, batch `videos.list`
//! validation, 30-day TTL on API-returned data, the user-submission
//! moderation queue, and played-weighted coverage.
//!
//! What this file pins, beyond the in-module unit suite:
//!
//! - harvest accepts the URL shapes the W6b read join can return
//!   (watch, youtu.be, embed) and drops non-YouTube rows;
//! - validation batches never exceed 50 IDs and the request builder only
//!   addresses the `videos` endpoint (never search at play time);
//! - API-returned data expires after exactly 30 days;
//! - moderation decisions are terminal; submissions accept IDs or URLs;
//! - coverage is reported raw and played-weighted, and the played share
//!   is absent (not zero) until N-plays data exists;
//! - the pipeline stores IDs only: no thumbnail storage, no extraction
//!   shapes.
//!
//! Standalone integration target: no existing suite is edited.

use nntune_api::videolinks::{
    compute_coverage, decide_submission, dedupe_candidates, expires_at, extract_video_id, is_fresh,
    is_playable, is_valid_video_id, normalise_submission, parse_videos_list_response,
    validation_batches, videos_list_query, Candidate, ModerationError, SubmissionStatus,
    VideoSource, VALIDATION_TTL_S, VIDEOS_LIST_MAX_IDS,
};
use std::collections::{HashMap, HashSet};

const MODULE_RS: &str = include_str!("../src/videolinks.rs");
const MIGRATION_SQL: &str = include_str!("../../../migrations/0035_videolinks_curation.sql");

/// Harvest the URL shapes the recording-videos join returns.
#[test]
fn harvest_accepts_mirror_url_shapes() {
    let urls = [
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        "https://youtu.be/dQw4w9WgXcQ",
        "https://www.youtube.com/embed/dQw4w9WgXcQ",
        "https://music.youtube.com/watch?v=9bZkp7q19f0",
        "https://example.com/video",
    ];
    let candidates: Vec<Candidate> = urls
        .iter()
        .filter_map(|u| {
            extract_video_id(u).map(|video_id| Candidate {
                recording_mbid: "123e4567-e89b-12d3-a456-426614174000".into(),
                video_id,
                source: VideoSource::MbUrl,
            })
        })
        .collect();
    assert_eq!(candidates.len(), 4, "non-YouTube rows must not harvest");
}

/// Cross-source duplicates collapse to the curated-mirror candidate.
#[test]
fn harvest_dedupe_prefers_curated_sources() {
    let mbid = "123e4567-e89b-12d3-a456-426614174000".to_string();
    let out = dedupe_candidates(vec![
        Candidate {
            recording_mbid: mbid.clone(),
            video_id: "dQw4w9WgXcQ".into(),
            source: VideoSource::UserSubmission,
        },
        Candidate {
            recording_mbid: mbid.clone(),
            video_id: "dQw4w9WgXcQ".into(),
            source: VideoSource::Wikidata,
        },
        Candidate {
            recording_mbid: mbid.clone(),
            video_id: "dQw4w9WgXcQ".into(),
            source: VideoSource::MbUrl,
        },
    ]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].source, VideoSource::MbUrl);
}

/// Batches cap at 50 and the request only addresses `videos`.
#[test]
fn validation_batches_and_request_shape() {
    assert_eq!(VIDEOS_LIST_MAX_IDS, 50);
    let ids: Vec<String> = (0..51).map(|i| format!("v{i:010}")).collect();
    assert!(ids.iter().all(|id| is_valid_video_id(id)));
    let batches = validation_batches(&ids);
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), 50);
    assert_eq!(batches[1].len(), 1);

    let query = videos_list_query(&batches[0]);
    assert!(query.contains("id="), "batch ids must be sent");
    assert!(
        !query.contains("search"),
        "validation must never address search"
    );
}

/// API-returned data expires after exactly 30 days.
#[test]
fn validation_ttl_is_thirty_days() {
    assert_eq!(VALIDATION_TTL_S, 30 * 24 * 60 * 60);
    let validated_at = 1_700_000_000i64;
    assert!(is_fresh(validated_at, validated_at + VALIDATION_TTL_S - 1));
    assert!(!is_fresh(validated_at, validated_at + VALIDATION_TTL_S));
    assert_eq!(expires_at(validated_at), validated_at + VALIDATION_TTL_S);
}

/// Only public/unlisted embeddable videos are playable.
#[test]
fn playability_gate() {
    let body = r#"{"items": [
        {"id": "dQw4w9WgXcQ", "snippet": {"title": "ok", "channelId": "ch"},
         "status": {"embeddable": true, "privacyStatus": "public"},
         "contentDetails": {"duration": "PT3M30S"}},
        {"id": "9bZkp7q19f0", "snippet": {"title": "noembed", "channelId": "ch"},
         "status": {"embeddable": false, "privacyStatus": "public"},
         "contentDetails": {"duration": "PT4M00S"}},
        {"id": "eY52Zsg9R3Q", "snippet": {"title": "priv", "channelId": "ch"},
         "status": {"embeddable": true, "privacyStatus": "private"},
         "contentDetails": {"duration": "PT4M00S"}}]}"#;
    let videos = parse_videos_list_response(body).expect("valid body parses");
    assert_eq!(videos.len(), 3);
    assert!(is_playable(&videos[0]));
    assert!(!is_playable(&videos[1]), "non-embeddable is not playable");
    assert!(!is_playable(&videos[2]), "private is not playable");
}

/// Moderation: IDs or URLs in, terminal decisions, no re-decision.
#[test]
fn moderation_queue_rules() {
    assert_eq!(
        normalise_submission("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
        Ok("dQw4w9WgXcQ".to_string())
    );
    assert_eq!(
        normalise_submission("https://example.com/watch?v=dQw4w9WgXcQ"),
        Err(ModerationError::NotAYoutubeLink)
    );
    assert_eq!(
        decide_submission(SubmissionStatus::Pending, false),
        Ok(SubmissionStatus::Rejected)
    );
    assert_eq!(
        decide_submission(SubmissionStatus::Rejected, true),
        Err(ModerationError::IllegalTransition)
    );
}

/// Coverage: raw share plus played-weighted share; the played share is
/// absent until N-plays data exists.
#[test]
fn coverage_raw_and_played_weighted() {
    let linked: HashSet<String> = ["recording-a".to_string()].into_iter().collect();
    let report = compute_coverage(100, &linked, &HashMap::new());
    assert_eq!((report.coverage_ratio * 100.0).round(), 1.0);
    assert_eq!(
        report.played_coverage_ratio, None,
        "no plays yet: absent, not zero"
    );

    // 1.6% raw coverage can still dominate played coverage when the
    // linked tracks are the ones people actually play.
    let plays: HashMap<String, u64> = [
        ("recording-a".to_string(), 80),
        ("recording-b".to_string(), 20),
    ]
    .into_iter()
    .collect();
    let report = compute_coverage(100, &linked, &plays);
    assert_eq!(report.played_coverage_ratio, Some(0.8));
}

/// Individual counts fit u64, but the catalogue-wide totals need not.
#[test]
fn coverage_preserves_weights_when_play_totals_exceed_u64() {
    let plays: HashMap<String, u64> = [
        ("recording-a".to_string(), u64::MAX),
        ("recording-b".to_string(), u64::MAX),
        ("recording-c".to_string(), u64::MAX),
    ]
    .into_iter()
    .collect();
    for (ids, expected) in [
        (vec![], 0.0),
        (vec!["recording-a"], 1.0 / 3.0),
        (vec!["recording-a", "recording-b"], 2.0 / 3.0),
        (vec!["recording-a", "recording-b", "recording-c"], 1.0),
    ] {
        let linked = ids.into_iter().map(str::to_owned).collect();
        let report = compute_coverage(3, &linked, &plays);
        let ratio = report.played_coverage_ratio.expect("positive play counts");
        assert!(ratio.is_finite() && (0.0..=1.0).contains(&ratio));
        assert!((ratio - expected).abs() < 1e-15);
    }
}

#[test]
fn coverage_distinguishes_unplayed_from_played_but_unlinked() {
    let linked = HashSet::new();
    let mut plays = HashMap::from([("recording-a".to_string(), 0)]);
    assert_eq!(
        compute_coverage(1, &linked, &plays).played_coverage_ratio,
        None
    );
    plays.insert("recording-a".to_string(), u64::MAX);
    assert_eq!(
        compute_coverage(1, &linked, &plays).played_coverage_ratio,
        Some(0.0)
    );
}

/// Guardrail: the pipeline module never searches and never extracts —
/// harvest plus ahead-of-time `videos.list` validation only. Doc
/// comments may state the prohibition; code lines must not carry it.
#[test]
fn no_search_or_extraction_shapes_in_pipeline() {
    let code: Vec<&str> = MODULE_RS
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    let code_text = code.join("\n");
    assert!(
        !code_text.contains("search.list"),
        "play time must never call search.list"
    );
    // Assembled from fragments so this file never carries the banned
    // contiguous shapes itself (same technique as clean_room_lint.py):
    // direct-stream host, extractor path, and the extractor tool name.
    let direct_stream = ["google", "video"].concat();
    let extractor_path = ["/you", "tubei/"].concat();
    let extractor_tool = ["yt-", "dlp"].concat();
    let extractor_module = ["yt_", "dlp"].concat();
    for shape in [
        direct_stream,
        extractor_path,
        extractor_tool,
        extractor_module,
    ] {
        assert!(
            !code_text.to_lowercase().contains(&shape),
            "extraction shape banned outside the retired spike"
        );
    }
}

/// Guardrail: IDs are stored long-term; no thumbnail column exists —
/// SQL comments may state the prohibition; DDL must not carry it.
#[test]
fn migration_stores_ids_without_thumbnails() {
    let ddl: String = MIGRATION_SQL
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(ddl
        .trim_start()
        .starts_with("SET search_path = public, pg_catalog;"));
    for table in [
        "video_link_candidates",
        "video_user_submissions",
        "video_coverage_snapshots",
    ] {
        assert!(ddl.contains(&format!("CREATE TABLE IF NOT EXISTS public.{table}")));
    }
    assert!(
        !ddl.to_lowercase().contains("thumbnail"),
        "thumbnails are never proxied, cached or stored"
    );
    for table in [
        "video_link_candidates",
        "video_user_submissions",
        "video_coverage_snapshots",
    ] {
        assert!(
            MIGRATION_SQL.contains(table),
            "migration must create {table}"
        );
    }
    assert!(
        MIGRATION_SQL.contains("expires_at"),
        "API-returned data must carry its 30-day expiry"
    );
}
