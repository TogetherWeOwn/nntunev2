//! Segment probe: availability checks through the user's pool DO.
//!
//! The wire contract (pool routes, auth, per-credential caps) is owned by
//! the pool-DO card; this module owns the ingest side: the probe envelope
//! and how its outcome moves the checkpoint. Live calls stay behind the
//! checkpoint resume: a crash mid-`Probing` redelivers the same envelope,
//! and the mapping below converges on one `Verifying` entry.

use serde::{Deserialize, Serialize};

use crate::jobs::JobState;
use crate::queue::QueueMsg;

/// Upper bound on message-ids per probe call: keeps one pool round-trip
/// small and the checkpoint resume cheap.
pub const MAX_PROBE_SEGMENTS: usize = 500;

/// Availability check for one batch of segments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRequest {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub segment_ids: Vec<String>,
}

/// Pool answer: which segments answered ARTICLE, which answered 430.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeOutcome {
    pub present: Vec<String>,
    pub missing: Vec<String>,
}

/// Build one probe envelope from the job and its parsed segment list.
pub fn build_probe_request(
    msg: &QueueMsg,
    segment_ids: &[&str],
) -> Result<ProbeRequest, &'static str> {
    if segment_ids.is_empty() || segment_ids.len() > MAX_PROBE_SEGMENTS {
        return Err("probe batch must be 1..=500 segments");
    }
    if segment_ids.iter().any(|s| s.is_empty() || s.len() > 512) {
        return Err("segment ids must be 1..=512 chars");
    }
    Ok(ProbeRequest {
        job_id: msg.job_id.clone(),
        user_id: msg.user_id.clone(),
        idempotency_key: msg.idempotency_key.clone(),
        segment_ids: segment_ids.iter().map(|s| s.to_string()).collect(),
    })
}

/// Move the checkpoint on a probe answer. Only `Probing` advances; every
/// other state passes through so replayed outcomes cannot drag a job
/// backwards. Returns the next state plus the missing-block count the
/// `Verifying` step feeds to repair dispatch.
pub fn apply_probe(state: JobState, outcome: &ProbeOutcome) -> (JobState, u32) {
    if state != JobState::Probing {
        return (state, 0);
    }
    let missing = outcome.missing.len() as u32;
    if missing == 0 {
        (JobState::Verifying, 0)
    } else {
        (JobState::Probing, missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::build_upload_dispatch;
    use crate::fixtures::{GAPPED_PAR2_SET, SMALL_NZB, SMALL_NZB_SEGMENTS};
    use crate::jobs::JobState;

    #[test]
    fn probe_request_carries_idempotency_and_caps_segments() {
        let msg = build_upload_dispatch("user_01", SMALL_NZB).unwrap();
        let req = build_probe_request(&msg, &SMALL_NZB_SEGMENTS).unwrap();
        assert_eq!(req.idempotency_key, msg.idempotency_key);
        assert_eq!(req.segment_ids.len(), 2);
        // Redelivery converges: same inputs, same envelope.
        assert_eq!(req, build_probe_request(&msg, &SMALL_NZB_SEGMENTS).unwrap());
    }

    #[test]
    fn probe_request_refuses_empty_and_oversize_batches() {
        let msg = build_upload_dispatch("user_01", SMALL_NZB).unwrap();
        assert!(build_probe_request(&msg, &[]).is_err());
        let many = vec!["seg@synthetic.invalid"; MAX_PROBE_SEGMENTS + 1];
        assert!(build_probe_request(&msg, &many).is_err());
        assert!(build_probe_request(&msg, &[""]).is_err());
        let _ = GAPPED_PAR2_SET;
    }

    #[test]
    fn full_availability_advances_to_verify() {
        let outcome = ProbeOutcome {
            present: SMALL_NZB_SEGMENTS.iter().map(|s| s.to_string()).collect(),
            missing: vec![],
        };
        let (next, missing) = apply_probe(JobState::Probing, &outcome);
        assert_eq!(next, JobState::Verifying);
        assert_eq!(missing, 0);
    }

    #[test]
    fn gaps_hold_probing_with_a_missing_count() {
        let outcome = ProbeOutcome {
            present: vec!["gap-one@synthetic.invalid".into()],
            missing: vec!["gap-missing@synthetic.invalid".into()],
        };
        let (next, missing) = apply_probe(JobState::Probing, &outcome);
        assert_eq!(next, JobState::Probing);
        assert_eq!(missing, 1);
    }

    #[test]
    fn probe_mapping_leaves_other_states_alone() {
        let outcome = ProbeOutcome {
            present: vec![],
            missing: vec![],
        };
        let (next, _) = apply_probe(JobState::Verifying, &outcome);
        assert_eq!(next, JobState::Verifying);
    }
}
