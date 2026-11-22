//! Off-route MusicBrainz evidence for an admin publish decision.

use std::{
    collections::{hash_map::Entry, HashMap},
    fmt,
};

use serde::{Deserialize, Serialize};

use super::{
    admin_policy::{evaluate_positions, Position, PositionalEligibility},
    album_retrieval_query, normalize_match_text, pick_release_mbid, split_artist_album,
    validate_match_query, MbRecording, MbSearchHit,
};
use crate::queue::validate_mbid;

// The mirror defaults to one 25-hit search page and caps recording listings
// at 1000 occurrences. A full page/listing cannot establish completeness.
const SEARCH_LIMIT: usize = 25;
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

#[derive(Deserialize)]
struct SearchPage {
    q: String,
    #[serde(rename = "type")]
    kind: String,
    limit: usize,
    offset: usize,
    results: Vec<SearchRow>,
}

#[derive(Deserialize)]
struct SearchRow {
    #[serde(flatten)]
    hit: MbSearchHit,
}

#[derive(Debug)]
pub struct ReleaseCandidate(String);

impl ReleaseCandidate {
    pub fn mbid(&self) -> &str {
        &self.0
    }
}

/// Check the original response to the default release search for this
/// query. The caller obtains it from the trusted CC0 mirror.
pub fn select_release(
    query: &str,
    status: u16,
    body: &str,
) -> Result<ReleaseCandidate, NeedsReview> {
    let query = validate_match_query(query).ok_or(NeedsReview::InvalidQuery)?;
    let page: SearchPage = mirror_body(status, body).ok_or(NeedsReview::SearchEvidence)?;
    let retrieval = album_retrieval_query(&query);
    let expected_q = retrieval
        .as_deref()
        .unwrap_or(&query)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if page.q != expected_q
        || page.kind != "release"
        || page.limit != SEARCH_LIMIT
        || page.offset != 0
        || page.results.len() >= SEARCH_LIMIT
    {
        return Err(NeedsReview::SearchEvidence);
    }

    let artist_album = split_artist_album(&query);
    let mut hits = HashMap::new();
    for SearchRow { mut hit } in page.results {
        let title = normalize_match_text(&hit.title);
        let artist = hit.artist_name.as_deref().unwrap_or_default();
        let exact_album = matches!(&artist_album, Some((_, album)) if *album == title);
        if !validate_mbid(&hit.mbid)
            || title.is_empty()
            || hit.title.chars().any(char::is_control)
            || !(0.0..=1.0).contains(&hit.score)
            || matches!(&hit.artist_mbid, Some(id) if !validate_mbid(id))
            || artist.chars().any(char::is_control)
            || (exact_album && normalize_match_text(artist).is_empty())
        {
            return Err(NeedsReview::SearchEvidence);
        }
        hit.mbid.make_ascii_lowercase();
        match hits.entry(hit.mbid.clone()) {
            Entry::Occupied(prior) if prior.get() != &hit => {
                return Err(NeedsReview::SearchEvidence)
            }
            Entry::Occupied(_) => {}
            Entry::Vacant(slot) => {
                slot.insert(hit);
            }
        }
    }
    let hits: Vec<_> = hits.into_values().collect();
    pick_release_mbid(&query, &hits)
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

fn mirror_body<'a, T: Deserialize<'a>>(status: u16, body: &'a str) -> Option<T> {
    if status != 200 || body.len() > MAX_BODY_BYTES || !body.trim_start().starts_with('{') {
        return None;
    }
    serde_json::from_str(body).ok()
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

/// Exact release and recording occurrences in the verified files' pairing
/// order. The caller still owes a trusted, immutable input binding and an
/// authorized atomic publish transaction.
pub struct PublishMatch {
    release: ReleaseCandidate,
    recordings: Vec<MbRecording>,
}

impl fmt::Debug for PublishMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublishMatch").finish_non_exhaustive()
    }
}

impl PublishMatch {
    pub fn release_mbid(&self) -> &str {
        self.release.mbid()
    }

    pub fn recordings(&self) -> &[MbRecording] {
        &self.recordings
    }
}

/// Check the unmodified response for the selected release. Preserve all
/// occurrences in wire order so filtering, deduplication or sorting cannot
/// hide invalid evidence.
pub fn decide_publish_match(
    release: ReleaseCandidate,
    audio_file_count: usize,
    files: &[FileNumbering<'_>],
    status: u16,
    body: &str,
) -> Result<PublishMatch, NeedsReview> {
    let listing: ReleaseRecordings =
        mirror_body(status, body).ok_or(NeedsReview::RecordingEvidence)?;
    if !listing.release_mbid.eq_ignore_ascii_case(release.mbid())
        || listing.recordings.len() >= RECORDINGS_LIMIT
        || listing.recordings.iter().any(|row| {
            !row.release_mbid.eq_ignore_ascii_case(release.mbid())
                || !validate_mbid(&row.recording.mbid)
                || row.recording.title.trim().is_empty()
                || row.recording.title.chars().any(char::is_control)
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
