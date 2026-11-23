//! Off-route MusicBrainz evidence for an admin publish decision.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{
    admin_policy::{evaluate_positions, Position, PositionalEligibility},
    album_retrieval_query, normalize_match_text, pick_release_mbid, split_artist_album,
    validate_match_query, MbRecording, MbSearchHit,
};
use crate::queue::validate_mbid;

const MAX_RELEASE_CANDIDATES: usize = 25;
const MAX_LABEL_BYTES: usize = 4096;
// The mirror truncates listings at this length.
const RECORDINGS_LIMIT: usize = 1000;
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Fixed reasons suitable for a future needs_review outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedsReview {
    InvalidQuery,
    SearchEvidence,
    ReleaseUnresolved,
    RecordingEvidence,
    CountMismatch,
    FileNumbering,
    PositionMismatch,
}

/// Exact-title scope and scoring query for the trusted mirror lookup.
#[derive(PartialEq, Eq)]
pub struct ReleaseLookup {
    pub normalized_title: String,
    pub score_query: String,
}

pub fn release_lookup(query: &str) -> Result<ReleaseLookup, NeedsReview> {
    let query = validate_match_query(query).ok_or(NeedsReview::InvalidQuery)?;
    let normalized_title =
        split_artist_album(&query).map_or_else(|| normalize_match_text(&query), |(_, album)| album);
    if normalized_title.is_empty() {
        return Err(NeedsReview::InvalidQuery);
    }
    let retrieval = album_retrieval_query(&query).unwrap_or(query);
    let score_query = retrieval
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    Ok(ReleaseLookup {
        normalized_title,
        score_query,
    })
}

/// Every release with this normalized title from a trusted CC0 mirror lookup.
/// Pass no evidence for failed lookups or trigram pages, which can omit rivals.
pub struct CompleteReleaseCandidates<'a> {
    pub lookup: &'a ReleaseLookup,
    pub hits: Vec<MbSearchHit>,
}

#[derive(Debug)]
pub struct ReleaseCandidate(String);

impl ReleaseCandidate {
    pub fn mbid(&self) -> &str {
        &self.0
    }
}

/// Select a release from complete title evidence for this query.
pub fn select_release(
    query: &str,
    evidence: Option<CompleteReleaseCandidates<'_>>,
) -> Result<ReleaseCandidate, NeedsReview> {
    let lookup = release_lookup(query)?;
    let evidence = evidence.ok_or(NeedsReview::SearchEvidence)?;
    if evidence.lookup != &lookup || evidence.hits.len() > MAX_RELEASE_CANDIDATES {
        return Err(NeedsReview::SearchEvidence);
    }

    let has_artist = split_artist_album(query.trim()).is_some();
    let mut scores = HashMap::new();
    let mut seen = HashMap::new();
    let mut hits = evidence.hits;
    for hit in &mut hits {
        hit.mbid.make_ascii_lowercase();
        let hit = &*hit;
        let artist = hit.artist_name.as_deref().unwrap_or_default();
        if !validate_mbid(&hit.mbid)
            || hit.title.len().max(artist.len()) > MAX_LABEL_BYTES
            || normalize_match_text(&hit.title) != lookup.normalized_title
            || hit.title.chars().any(char::is_control)
            || !(0.0..=1.0).contains(&hit.score)
            || matches!(&hit.artist_mbid, Some(id) if !validate_mbid(id))
            || artist.chars().any(char::is_control)
            || (has_artist && normalize_match_text(artist).is_empty())
            || matches!(scores.insert(&hit.title, hit.score), Some(prior) if prior != hit.score)
            || matches!(seen.insert(&hit.mbid, hit), Some(prior) if prior != hit)
        {
            return Err(NeedsReview::SearchEvidence);
        }
    }
    pick_release_mbid(query.trim(), &hits)
        .map(ReleaseCandidate)
        .ok_or(NeedsReview::ReleaseUnresolved)
}

#[derive(Deserialize)]
struct ReleaseRecordings {
    release_mbid: String,
    recordings: Vec<RecordingRow>,
}

#[derive(Deserialize)]
struct RecordingRow {
    release_mbid: String,
    #[serde(flatten)]
    recording: MbRecording,
}

/// The caller passes the explicit track and optional disc number for each
/// verified audio file, in pairing order. Decimal digits with zero padding
/// are accepted. A missing disc must represent an absent claim.
pub struct FileNumbering<'a> {
    pub disc: Option<&'a str>,
    pub track: Option<&'a str>,
}

fn parse_number(raw: &str) -> Result<i16, NeedsReview> {
    raw.parse::<i16>()
        .ok()
        .filter(|&n| n > 0 && raw.bytes().all(|b| b.is_ascii_digit()))
        .ok_or(NeedsReview::FileNumbering)
}

/// Exact release and occurrences in pairing order. The publisher must bind
/// them to the immutable inputs in an authorized atomic transaction.
pub struct PublishMatch {
    release: ReleaseCandidate,
    recordings: Vec<MbRecording>,
}

impl PublishMatch {
    pub fn release_mbid(&self) -> &str {
        self.release.mbid()
    }

    pub fn recordings(&self) -> &[MbRecording] {
        &self.recordings
    }
}

/// Check the original recording rows in wire order for the selected release.
pub fn decide_publish_match(
    release: ReleaseCandidate,
    audio_file_count: usize,
    files: &[FileNumbering<'_>],
    status: u16,
    body: &str,
) -> Result<PublishMatch, NeedsReview> {
    if status != 200 || body.len() > MAX_BODY_BYTES || !body.trim_start().starts_with('{') {
        return Err(NeedsReview::RecordingEvidence);
    }
    let listing: ReleaseRecordings =
        serde_json::from_str(body).map_err(|_| NeedsReview::RecordingEvidence)?;
    let mut titles = HashMap::new();
    if !listing.release_mbid.eq_ignore_ascii_case(release.mbid())
        || listing.recordings.len() >= RECORDINGS_LIMIT
        || listing.recordings.iter().any(|row| {
            let MbRecording { mbid, title, .. } = &row.recording;
            !row.release_mbid.eq_ignore_ascii_case(release.mbid())
                || !validate_mbid(mbid)
                || title.trim().is_empty()
                || title.chars().any(char::is_control)
                || matches!(titles.insert(mbid.to_ascii_lowercase(), title), Some(prior) if prior != title)
        })
    {
        return Err(NeedsReview::RecordingEvidence);
    }
    if audio_file_count == 0
        || audio_file_count != files.len()
        || audio_file_count != listing.recordings.len()
    {
        return Err(NeedsReview::CountMismatch);
    }
    let files: Vec<_> = files
        .iter()
        .map(|file| {
            Ok(Position {
                medium_no: file.disc.map(parse_number).transpose()?,
                track_no: Some(parse_number(file.track.ok_or(NeedsReview::FileNumbering)?)?),
            })
        })
        .collect::<Result<_, NeedsReview>>()?;
    let positions = listing
        .recordings
        .iter()
        .map(|row| Position {
            medium_no: row.recording.medium_no,
            track_no: row.recording.track_no,
        })
        .collect::<Vec<_>>();
    if evaluate_positions(audio_file_count, &files, &positions) != PositionalEligibility::Eligible {
        return Err(NeedsReview::PositionMismatch);
    }
    Ok(PublishMatch {
        release,
        recordings: listing
            .recordings
            .into_iter()
            .map(|row| row.recording)
            .collect(),
    })
}
