use super::*;
use crate::jobs::{advance_checkpoint, complete_admin_parse, resume_archive, MAX_ATTEMPTS};

const RELEASE: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_RELEASE: &str = "22222222-2222-4222-8222-222222222222";

fn checkpoint() -> JobCheckpoint {
    let msg =
        crate::dispatch::build_upload_dispatch("gate_user", crate::fixtures::SMALL_NZB).unwrap();
    let mut cp = crate::checkpoint::initial(&msg);
    cp.source = JobSource::AdminUpload;
    cp.state = JobState::Parsing;
    let parsed = nntune_core::nzb::parse(crate::fixtures::SMALL_NZB.as_bytes())
        .unwrap_or_else(|_| panic!("synthetic parse rejected"));
    assert!(complete_admin_parse(&mut cp, &parsed));
    cp
}

fn request(cp: &JobCheckpoint, batch_index: usize) -> ProbeRequest {
    // Reuse existing synthetic identities in memory only, never in assertions
    // that dump requests, persisted checkpoints, logs or new fixtures.
    ProbeRequest {
        job_id: cp.job_id.clone(),
        user_id: cp.user_id.clone(),
        idempotency_key: cp.idempotency_key.clone(),
        batch_index,
        segment_ids: vec![crate::fixtures::SMALL_NZB_SEGMENTS[0].to_owned()],
        segment_bytes: vec![None],
    }
}

fn answer(req: &ProbeRequest, missing: bool) -> ProbeOutcome {
    ProbeOutcome {
        batch_index: req.batch_index,
        present: if missing {
            vec![]
        } else {
            req.segment_ids.clone()
        },
        missing: if missing {
            req.segment_ids.clone()
        } else {
            vec![]
        },
    }
}

fn reload(cp: &JobCheckpoint) -> JobCheckpoint {
    serde_json::from_str(&serde_json::to_string(cp).unwrap()).unwrap()
}

#[test]
fn selected_match_and_every_clean_batch_survive_resume_before_writing() {
    let mut cp = checkpoint();
    let identity = (cp.job_id.clone(), cp.idempotency_key.clone());
    assert!(!probe_ready(&cp));
    assert!(!publish_ready(&cp));
    assert!(complete_match(&mut cp, Some(RELEASE), 2));
    assert_eq!(cp.state, JobState::Probing);
    assert_eq!(cp.admin_release_mbid.as_deref(), Some(RELEASE));
    assert!(probe_ready(&cp));
    assert_eq!(advance_checkpoint(&mut cp, false), JobState::Probing);
    let req = request(&cp, 0);
    assert!(record_probe(&mut cp, &req, &answer(&req, false)));
    cp = reload(&cp);
    assert_eq!(cp.state, JobState::Probing);
    assert!(probe_ready(&cp));
    assert!(!publish_ready(&cp));
    let before = cp.clone();
    assert!(!record_probe(&mut cp, &req, &answer(&req, true)));
    assert_eq!(cp, before);
    let req = request(&cp, 1);
    assert!(record_probe(&mut cp, &req, &answer(&req, false)));
    cp = reload(&cp);
    assert!(publish_ready(&cp));
    assert!(!probe_ready(&cp));
    // The retained probe projection is numeric, not a segment manifest.
    let stored = serde_json::to_string(&cp).unwrap();
    assert!(req.segment_ids.iter().all(|id| !stored.contains(id)));
    assert_eq!(advance_checkpoint(&mut cp, false), JobState::Done);
    assert_eq!((cp.job_id, cp.idempotency_key), identity);
}

#[test]
fn no_match_parks_and_manual_approval_or_retry_requires_fresh_probes() {
    let mut cp = checkpoint();
    cp.attempt = MAX_ATTEMPTS;
    assert!(complete_match(&mut cp, None, 0));
    assert_eq!(cp.state, JobState::NeedsReview);
    assert!(!probe_ready(&cp));
    assert_eq!(advance_checkpoint(&mut cp, false), JobState::NeedsReview);
    assert!(approve_review(&mut cp, RELEASE, 1));
    assert_eq!(cp.attempt, 1);
    let req = request(&cp, 0);
    assert!(record_probe(&mut cp, &req, &answer(&req, true)));
    assert_eq!(cp.state, JobState::NeedsReview);
    assert!(!publish_ready(&cp));
    cp.failure_code = Some("probe_failed".into());
    cp.failure_reason = Some("redacted".into());
    assert!(approve_review(&mut cp, OTHER_RELEASE, 1));
    assert_eq!(cp.admin_release_mbid.as_deref(), Some(OTHER_RELEASE));
    assert!(cp.probe_tally.as_ref().unwrap().missing_by_batch.is_empty());
    assert!(cp.failure_code.is_none());
    assert!(cp.failure_reason.is_none());
    assert!(!publish_ready(&cp));
    assert_eq!(advance_checkpoint(&mut cp, false), JobState::Probing);
    let req = request(&cp, 0);
    assert!(record_probe(&mut cp, &req, &answer(&req, true)));
    assert!(retry_review(&mut cp));
    assert_eq!(cp.state, JobState::Matching);
    assert!(cp.admin_release_mbid.is_none());
    assert!(cp.probe_tally.is_none());
    assert!(!probe_ready(&cp));
    assert!(complete_match(&mut cp, Some(RELEASE), 1));
    let req = request(&cp, 0);
    assert!(record_probe(&mut cp, &req, &answer(&req, false)));
    assert!(publish_ready(&cp));
}

#[test]
fn gap_in_any_batch_parks_even_when_last_answer_is_clean() {
    let mut cp = checkpoint();
    assert!(complete_match(&mut cp, Some(RELEASE), 3));
    for (batch, missing, expected) in [
        (1, true, JobState::Probing),
        (2, false, JobState::Probing),
        (0, false, JobState::NeedsReview),
    ] {
        let req = request(&cp, batch);
        assert!(record_probe(&mut cp, &req, &answer(&req, missing)));
        cp = reload(&cp);
        assert_eq!(cp.state, expected);
        assert!(!publish_ready(&cp));
    }
    assert_eq!(cp.probe_tally.as_ref().unwrap().missing_segments(), 1);
}

#[test]
fn malformed_or_foreign_batch_answers_never_change_the_checkpoint() {
    let mut cp = checkpoint();
    assert!(complete_match(&mut cp, Some(RELEASE), 2));
    for mutation in 0..12 {
        let mut req = request(&cp, 0);
        let mut outcome = answer(&req, false);
        match mutation {
            0 => req.job_id.push('x'),
            1 => req.user_id.push('x'),
            2 => req.idempotency_key.push('x'),
            3 => outcome.batch_index = 1,
            4 => outcome.present.clear(),
            5 => outcome.missing = outcome.present.clone(),
            6 => {
                req.batch_index = 2;
                outcome.batch_index = 2;
            }
            7 => {
                req.segment_ids.clear();
                outcome.present.clear();
            }
            8 => req.segment_bytes.clear(),
            9 => req.segment_bytes[0] = Some(0),
            10 => req.segment_ids[0].push('\n'),
            11 => {
                req.segment_ids
                    .resize(MAX_PROBE_SEGMENTS + 1, req.segment_ids[0].clone());
                outcome.present = req.segment_ids.clone();
            }
            _ => unreachable!(),
        }
        let before = cp.clone();
        assert!(!record_probe(&mut cp, &req, &outcome));
        assert_eq!(cp, before);
    }
}

#[test]
fn missing_or_corrupt_persisted_gate_evidence_never_advances_to_publication() {
    let corruptions: Vec<fn(&mut JobCheckpoint)> = vec![
        |cp| cp.admin_release_mbid = None,
        |cp| cp.admin_release_mbid = Some("invalid".into()),
        |cp| cp.probe_tally = None,
        |cp| cp.probe_tally.as_mut().unwrap().total_batches = 0,
        |cp| cp.probe_tally.as_mut().unwrap().total_batches = MAX_BATCHES + 1,
        |cp| {
            cp.probe_tally
                .as_mut()
                .unwrap()
                .missing_by_batch
                .insert(1, 0);
        },
        |cp| {
            cp.probe_tally
                .as_mut()
                .unwrap()
                .missing_by_batch
                .insert(0, u32::MAX);
        },
    ];
    for corrupt in corruptions {
        let mut cp = checkpoint();
        assert!(complete_match(&mut cp, Some(RELEASE), 1));
        corrupt(&mut cp);
        cp = reload(&cp);
        assert!(!probe_ready(&cp));
        assert_eq!(advance_checkpoint(&mut cp, false), JobState::NeedsReview);
        cp.state = JobState::Writing;
        assert!(!publish_ready(&cp));
        assert_eq!(advance_checkpoint(&mut cp, false), JobState::NeedsReview);
    }
    let mut cp = checkpoint();
    assert!(complete_match(&mut cp, Some(RELEASE), 1));
    let mut old = serde_json::to_value(&cp).unwrap();
    old.as_object_mut().unwrap().remove("admin_release_mbid");
    let mut cp: JobCheckpoint = serde_json::from_value(old).unwrap();
    assert!(cp.admin_release_mbid.is_none());
    assert_eq!(advance_checkpoint(&mut cp, false), JobState::NeedsReview);
}

#[test]
fn invalid_match_plans_and_manual_actions_are_no_ops_outside_their_states() {
    for (release, batches) in [("invalid", 1), (RELEASE, 0), (RELEASE, MAX_BATCHES + 1)] {
        let mut cp = checkpoint();
        let before = cp.clone();
        assert!(!complete_match(&mut cp, Some(release), batches));
        assert_eq!(cp, before);
        cp.state = JobState::NeedsReview;
        let before = cp.clone();
        assert!(!approve_review(&mut cp, release, batches));
        assert_eq!(cp, before);
    }
    for source in [
        JobSource::NzbUpload,
        JobSource::IndexerSearch,
        JobSource::AdminUpload,
    ] {
        for state in [
            JobState::Queued,
            JobState::Parsing,
            JobState::Matching,
            JobState::Probing,
            JobState::Writing,
            JobState::NeedsReview,
            JobState::NeedsArchiveRead,
            JobState::Done,
            JobState::Rejected,
            JobState::Failed,
        ] {
            let mut cp = checkpoint();
            cp.source = source;
            cp.state = state;
            let before = cp.clone();
            if source != JobSource::AdminUpload || state != JobState::Matching {
                assert!(!complete_match(&mut cp, Some(RELEASE), 1));
                assert_eq!(cp, before);
            }
            if source != JobSource::AdminUpload || state != JobState::NeedsReview {
                assert!(!approve_review(&mut cp, RELEASE, 1));
                assert!(!retry_review(&mut cp));
                assert_eq!(cp, before);
            }
        }
    }
    let mut cp = checkpoint();
    assert_eq!(advance_checkpoint(&mut cp, false), JobState::NeedsReview);
}

#[test]
fn archive_resume_and_redrive_drop_old_match_and_probe_evidence() {
    for archive in [true, false] {
        let mut cp = checkpoint();
        assert!(complete_match(&mut cp, Some(RELEASE), 1));
        let req = request(&cp, 0);
        assert!(record_probe(&mut cp, &req, &answer(&req, false)));
        assert!(publish_ready(&cp));
        if archive {
            cp.state = JobState::NeedsArchiveRead;
            assert!(resume_archive(&mut cp));
            assert_eq!(cp.state, JobState::Parsing);
        } else {
            crate::retry::redrive_checkpoint(&mut cp);
            assert_eq!(cp.state, JobState::Queued);
        }
        assert!(cp.admin_release_mbid.is_none());
        assert!(cp.probe_tally.is_none());
        assert!(!publish_ready(&cp));
    }
}
