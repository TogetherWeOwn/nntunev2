//! Admin match-before-probe gates on the existing job checkpoint. These are
//! execution transitions, not authorization or live dispatch. The caller must
//! load the authenticated original, derive its full probe plan, and persist
//! each updated checkpoint before any next provider/publication side effect.
//! Only a catalogue release ID and numeric probe counts survive persistence.

use crate::jobs::{JobCheckpoint, JobSource, JobState};
use crate::pool_probe::check_outcome;
use crate::probe::{
    validate_message_id, ProbeOutcome, ProbeRequest, ProbeTally, MAX_PROBE_SEGMENTS,
};
use crate::queue::validate_mbid;

const MAX_BATCHES: usize = crate::nzb_intake::MAX_SEGMENTS.div_ceil(MAX_PROBE_SEGMENTS);

/// Complete the mirror match. An absent/ambiguous match parks without NNTP.
/// A selected release starts a fresh full-document probe plan; old evidence
/// must not qualify a newly selected release. Invalid inputs do not mutate.
pub fn complete_match(cp: &mut JobCheckpoint, release: Option<&str>, total_batches: usize) -> bool {
    if cp.source != JobSource::AdminUpload || cp.state != JobState::Matching {
        return false;
    }
    if let Some(release) = release {
        if !validate_mbid(release) || !(1..=MAX_BATCHES).contains(&total_batches) {
            return false;
        }
    }
    cp.admin_release_mbid = release.map(str::to_owned);
    cp.probe_tally = release.map(|_| ProbeTally::new(total_batches));
    cp.state = if release.is_some() {
        JobState::Probing
    } else {
        JobState::NeedsReview
    };
    true
}

/// Explicit, authorized manual approval still goes through the probe gate.
/// This function grants no authority to invoke it and never publishes.
pub fn approve_review(cp: &mut JobCheckpoint, release: &str, total_batches: usize) -> bool {
    if cp.source != JobSource::AdminUpload
        || cp.state != JobState::NeedsReview
        || !validate_mbid(release)
        || !(1..=MAX_BATCHES).contains(&total_batches)
    {
        return false;
    }
    reset_review(cp);
    complete_match(cp, Some(release), total_batches)
}

/// Explicit retry re-matches before probing, retaining the original identity.
/// Ordinary Queue replay never calls this transition.
pub fn retry_review(cp: &mut JobCheckpoint) -> bool {
    if cp.source != JobSource::AdminUpload || cp.state != JobState::NeedsReview {
        return false;
    }
    reset_review(cp);
    true
}

fn reset_review(cp: &mut JobCheckpoint) {
    cp.state = JobState::Matching;
    cp.attempt = 1;
    cp.admin_release_mbid = None;
    cp.probe_tally = None;
    cp.failure_code = None;
    cp.failure_reason = None;
}

/// Safe to dispatch the next planned batch only with a retained valid match
/// and a well-formed, non-empty tally. Old/corrupt checkpoints fail closed.
pub fn probe_ready(cp: &JobCheckpoint) -> bool {
    cp.source == JobSource::AdminUpload
        && cp.state == JobState::Probing
        && valid_evidence(cp)
        && cp.probe_tally.as_ref().is_some_and(|t| !t.is_complete())
}

fn valid_evidence(cp: &JobCheckpoint) -> bool {
    cp.admin_release_mbid.as_deref().is_some_and(validate_mbid)
        && cp.probe_tally.as_ref().is_some_and(|t| {
            (1..=MAX_BATCHES).contains(&t.total_batches)
                && t.missing_by_batch.iter().all(|(&index, &missing)| {
                    index < t.total_batches && missing <= MAX_PROBE_SEGMENTS as u32
                })
        })
}

fn clean_complete_probe(cp: &JobCheckpoint) -> bool {
    valid_evidence(cp)
        && cp.probe_tally.as_ref().is_some_and(|t| {
            t.is_complete() && t.missing_by_batch.values().all(|&missing| missing == 0)
        })
}

/// Recheck immediately before the existing ingest-job-fenced transaction.
/// This is a necessary gate, not proof that the transaction has committed.
pub fn publish_ready(cp: &JobCheckpoint) -> bool {
    cp.source == JobSource::AdminUpload && cp.state == JobState::Writing && clean_complete_probe(cp)
}

pub(crate) fn advance_probe(cp: &mut JobCheckpoint) -> JobState {
    if !valid_evidence(cp) {
        cp.state = JobState::NeedsReview;
    } else if cp.probe_tally.as_ref().is_some_and(ProbeTally::is_complete) {
        cp.state = if clean_complete_probe(cp) {
            JobState::Writing
        } else {
            JobState::NeedsReview
        };
    }
    cp.state
}

/// Fold only a full answer to this job's actual requested batch. Replay and
/// malformed/foreign answers leave the checkpoint untouched. No segment IDs
/// from the request or answer enter the persisted tally.
pub fn record_probe(cp: &mut JobCheckpoint, request: &ProbeRequest, answer: &ProbeOutcome) -> bool {
    if !probe_ready(cp)
        || request.job_id != cp.job_id
        || request.user_id != cp.user_id
        || request.idempotency_key != cp.idempotency_key
        || request.segment_ids.is_empty()
        || request.segment_ids.len() > MAX_PROBE_SEGMENTS
        || request.segment_bytes.len() != request.segment_ids.len()
        || request
            .segment_ids
            .iter()
            .any(|id| !validate_message_id(id))
        || request
            .segment_bytes
            .iter()
            .any(|bytes| bytes.is_some_and(|n| n == 0 || n > crate::probe::MAX_SEGMENT_BYTES))
        || check_outcome(request, answer).is_err()
    {
        return false;
    }
    if !cp.probe_tally.as_mut().unwrap().record(answer) {
        return false;
    }
    advance_probe(cp);
    true
}

#[cfg(test)]
#[path = "admin_gates_tests.rs"]
mod tests;
