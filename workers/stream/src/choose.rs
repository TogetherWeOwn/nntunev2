//! Pure profile-artifact choice for the opus-64 stream profile.
//!
//! [`choose_profile_artifact`] picks which already-published artifact to serve
//! for a requested [`StreamProfile`] from caller-supplied [`MediaRow`] values
//! mirroring `track_media(format, bitrate, content_hash)`. It is synchronous,
//! total and deterministic: the same slice always yields the same outcome, and
//! no lease, dial or IO happens before the choice.
//!
//! A chosen artifact grants no authority to serve bytes. Authorization stays
//! with the serve-time check outside this module, and publication, serving,
//! ranges and runtime adapters all live elsewhere. This module has no
//! production callers; the only permitted production reference is the
//! out-of-line `pub mod choose;` export.

/// One candidate row mirroring `track_media(format, bitrate, content_hash)`.
///
/// Plain data supplied by the caller in candidate order; the chooser scans
/// the slice and never reorders it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaRow<'a> {
    /// Artifact format text, e.g. `"opus"`. Compared case-sensitively.
    pub format: &'a str,
    /// Bitrate in kbps; must be positive to count as well-formed.
    pub bitrate: i32,
    /// Content hash of the published artifact.
    pub content_hash: &'a str,
}

/// Stream profile requested by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamProfile {
    /// Opus in Ogg at 64 kbps.
    Opus64,
}

/// How the winning row matched the requested profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchLevel {
    /// The row is exactly the requested profile.
    Exact,
    /// The row is in the profile family but not the exact profile.
    Fallback,
}

/// Winning candidate: echoes the row plus the match level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChosenArtifact<'a> {
    /// Content hash of the winning row.
    pub content_hash: &'a str,
    /// Format text of the winning row.
    pub format: &'a str,
    /// Bitrate of the winning row.
    pub bitrate: i32,
    /// How the winning row matched the requested profile.
    pub match_level: MatchLevel,
}

/// Typed miss: why no row could be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceMiss {
    /// The candidate set was empty: nothing exists to choose.
    EmptyCandidateSet,
    /// Well-formed rows exist but none is in the requested profile family.
    CrossProfileRefused,
    /// The set is non-empty but no row is well-formed.
    NoWellFormedCandidate,
}

/// Typed next step for a miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextAction {
    /// Produce the missing profile artifact, then retry the choice.
    TranscodeThenRetry,
    /// Discard the malformed rows and re-read the candidate set.
    RepairCandidates,
}

impl ChoiceMiss {
    /// Stable wire code for the miss.
    pub fn code(self) -> &'static str {
        match self {
            ChoiceMiss::EmptyCandidateSet => "profile_artifact_empty",
            ChoiceMiss::CrossProfileRefused => "profile_artifact_cross_profile",
            ChoiceMiss::NoWellFormedCandidate => "profile_artifact_malformed",
        }
    }

    /// Typed next step for the miss.
    pub fn next_action(self) -> NextAction {
        match self {
            ChoiceMiss::EmptyCandidateSet | ChoiceMiss::CrossProfileRefused => {
                NextAction::TranscodeThenRetry
            }
            ChoiceMiss::NoWellFormedCandidate => NextAction::RepairCandidates,
        }
    }
}

/// True when the row mirrors the storage constraints: non-empty format,
/// positive bitrate, and a 64-character lowercase hex content hash.
fn is_well_formed(row: &MediaRow) -> bool {
    if row.format.is_empty() || row.bitrate <= 0 {
        return false;
    }
    let hash = row.content_hash;
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Choose the artifact to serve for `profile` from caller-ordered `rows`.
///
/// Decision table for [`StreamProfile::Opus64`], evaluated in order:
///
/// 1. Empty slice: [`ChoiceMiss::EmptyCandidateSet`].
/// 2. First well-formed row with `format == "opus"` and `bitrate == 64`:
///    [`MatchLevel::Exact`]. Matching is case-sensitive.
/// 3. First well-formed row with `format == "opus"` (any positive bitrate):
///    [`MatchLevel::Fallback`]. Candidate order, not bitrate, breaks ties.
/// 4. Else, if any well-formed row exists (all outside the opus family):
///    [`ChoiceMiss::CrossProfileRefused`].
/// 5. Else (non-empty, none well-formed):
///    [`ChoiceMiss::NoWellFormedCandidate`].
///
/// A well-formed cross-profile row beats malformed opus-shaped rows (rule 4
/// before rule 5), so the actionable "source exists, produce the profile
/// artifact" verdict wins over "repair".
pub fn choose_profile_artifact<'a>(
    rows: &[MediaRow<'a>],
    profile: StreamProfile,
) -> Result<ChosenArtifact<'a>, ChoiceMiss> {
    match profile {
        StreamProfile::Opus64 => choose_opus64(rows),
    }
}

fn choose_opus64<'a>(rows: &[MediaRow<'a>]) -> Result<ChosenArtifact<'a>, ChoiceMiss> {
    if rows.is_empty() {
        return Err(ChoiceMiss::EmptyCandidateSet);
    }
    for row in rows {
        if is_well_formed(row) && row.format == "opus" && row.bitrate == 64 {
            return Ok(ChosenArtifact {
                content_hash: row.content_hash,
                format: row.format,
                bitrate: row.bitrate,
                match_level: MatchLevel::Exact,
            });
        }
    }
    for row in rows {
        if is_well_formed(row) && row.format == "opus" {
            return Ok(ChosenArtifact {
                content_hash: row.content_hash,
                format: row.format,
                bitrate: row.bitrate,
                match_level: MatchLevel::Fallback,
            });
        }
    }
    if rows.iter().any(is_well_formed) {
        return Err(ChoiceMiss::CrossProfileRefused);
    }
    Err(ChoiceMiss::NoWellFormedCandidate)
}
