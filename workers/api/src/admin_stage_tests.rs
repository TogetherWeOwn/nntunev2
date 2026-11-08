//! Cross-crate conformance with the real authenticated original reader. Fake
//! catalogue/probe/persistence services are not live dispatch or SQL acceptance.
use super::*;
use nntune_ingest::{
    admin_execution::{run_stage, StageError, StageOutcome, StageRuntime},
    jobs::{JobCheckpoint, JobSource, JobState},
    probe::{ProbeOutcome, ProbeRequest},
};

struct Runtime {
    stored: JobCheckpoint,
    events: Vec<&'static str>,
    lose_write: bool,
}

impl StageRuntime for Runtime {
    async fn match_release(
        &mut self,
        parsed: &nntune_core::nzb::Document,
    ) -> Result<Option<String>, StageError> {
        assert!(!parsed.files.is_empty());
        assert_eq!(self.stored.state, JobState::Matching);
        self.events.push("match");
        Ok(Some("11111111-1111-4111-8111-111111111111".into()))
    }

    async fn probe(&mut self, req: &ProbeRequest) -> Result<ProbeOutcome, StageError> {
        assert_eq!(self.stored.state, JobState::Probing);
        assert_eq!(self.stored.job_id, req.job_id);
        assert_eq!(self.stored.idempotency_key, req.idempotency_key);
        assert!(req.segment_bytes.iter().all(Option::is_some));
        self.events.push("probe");
        Ok(ProbeOutcome {
            batch_index: req.batch_index,
            present: req.segment_ids.clone(),
            missing: vec![],
        })
    }

    async fn persist(
        &mut self,
        expected: &JobCheckpoint,
        next: &JobCheckpoint,
    ) -> Result<bool, StageError> {
        self.events.push("persist");
        if self.lose_write || expected != &self.stored {
            return Ok(false);
        }
        self.stored = serde_json::from_str(&serde_json::to_string(next).unwrap()).unwrap();
        Ok(true)
    }
}

fn checkpoint(original: &OriginalBody) -> JobCheckpoint {
    let msg = nntune_ingest::dispatch::build_upload_dispatch(
        "stage_user",
        nntune_ingest::fixtures::SMALL_NZB,
    )
    .unwrap();
    let mut cp = nntune_ingest::checkpoint::initial(&msg);
    cp.source = JobSource::AdminUpload;
    cp.job_id = original.job_id().into();
    cp.idempotency_key = original.idempotency_key().into();
    cp.user_id.clear();
    cp
}

#[tokio::test]
async fn authenticated_retained_original_runs_persisted_parse_match_probe_without_publishing() {
    for (suffix, expected) in [
        (".flac", StageOutcome::ReadyToPublish),
        (".rar", StageOutcome::Parked),
        (".7z", StageOutcome::Parked),
    ] {
        let (_, job, mut repo, keys) = fixture_for(
            nntune_ingest::fixtures::SMALL_NZB
                .replace(".flac", suffix)
                .into_bytes(),
        );
        let original = read_original(&Authorization(true), &mut repo, &keys, &job)
            .await
            .unwrap();
        assert_eq!(repo.events, ["metadata", "envelope", "running"]);
        let mut cp = checkpoint(&original);
        let mut runtime = Runtime {
            stored: cp.clone(),
            events: vec![],
            lose_write: false,
        };
        for _ in 0..4 {
            cp = runtime.stored.clone();
            let outcome = run_stage(
                &mut runtime,
                &mut cp,
                original.job_id(),
                original.idempotency_key(),
                original.parsed(),
            )
            .await
            .unwrap();
            if outcome != StageOutcome::Continue {
                assert_eq!(outcome, expected);
                break;
            }
        }
        if expected == StageOutcome::ReadyToPublish {
            assert_eq!(cp.state, JobState::Writing);
            assert_eq!(
                runtime.events,
                ["persist", "persist", "match", "persist", "probe", "persist"]
            );
            assert!(nntune_ingest::admin_gates::publish_ready(&cp));
        } else {
            assert_eq!(cp.state, JobState::NeedsArchiveRead);
            assert_eq!(runtime.events, ["persist", "persist"]);
        }
        // Neither service requests nor the original's sensitive fields appear
        // in the persisted projection. Never dump those values on assertion failure.
        let stored = serde_json::to_string(&runtime.stored).unwrap();
        for file in &original.parsed().files {
            assert!(!stored.contains(&file.subject));
            for segment in &file.segments {
                assert!(!stored.contains(&segment.message_id));
            }
        }
    }
}

#[tokio::test]
async fn authenticated_probe_replays_a_lost_write_but_not_a_saved_batch() {
    let (_, job, mut repo, keys) =
        fixture_for(nntune_ingest::fixtures::SMALL_NZB.as_bytes().to_vec());
    let original = read_original(&Authorization(true), &mut repo, &keys, &job)
        .await
        .unwrap();
    let mut cp = checkpoint(&original);
    let mut runtime = Runtime {
        stored: cp.clone(),
        events: vec![],
        lose_write: false,
    };
    for _ in 0..3 {
        assert_eq!(
            run_stage(
                &mut runtime,
                &mut cp,
                original.job_id(),
                original.idempotency_key(),
                original.parsed()
            )
            .await,
            Ok(StageOutcome::Continue)
        );
    }
    let before = cp.clone();
    runtime.lose_write = true;
    assert_eq!(
        run_stage(
            &mut runtime,
            &mut cp,
            original.job_id(),
            original.idempotency_key(),
            original.parsed()
        )
        .await,
        Err(StageError::Stale)
    );
    assert_eq!(cp, before);
    assert_eq!(runtime.stored, before);
    runtime.lose_write = false;
    cp = runtime.stored.clone();
    assert_eq!(
        run_stage(
            &mut runtime,
            &mut cp,
            original.job_id(),
            original.idempotency_key(),
            original.parsed()
        )
        .await,
        Ok(StageOutcome::ReadyToPublish)
    );
    let count = runtime.events.len();
    assert_eq!(
        run_stage(
            &mut runtime,
            &mut cp,
            original.job_id(),
            original.idempotency_key(),
            original.parsed()
        )
        .await,
        Ok(StageOutcome::ReadyToPublish)
    );
    assert_eq!(runtime.events.len(), count);
    assert_eq!(runtime.events.iter().filter(|&&e| e == "probe").count(), 2);
    assert_eq!(cp.state, JobState::Writing);
}
