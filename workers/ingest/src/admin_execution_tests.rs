use super::*;

const RELEASE: &str = "11111111-1111-4111-8111-111111111111";

struct Runtime {
    stored: JobCheckpoint,
    events: Vec<&'static str>,
    batches: Vec<usize>,
    release: Option<String>,
    missing: bool,
    malformed: bool,
    service_error: Option<StageError>,
    store_error: bool,
    stale: bool,
    pending: bool,
}

impl StageRuntime for Runtime {
    async fn match_release(&mut self, _: &Document) -> Result<Option<String>, StageError> {
        self.events.push("match");
        if let Some(error) = self.service_error {
            return Err(error);
        }
        Ok(self.release.clone())
    }

    async fn probe(&mut self, request: &ProbeRequest) -> Result<ProbeOutcome, StageError> {
        self.events.push("probe");
        self.batches.push(request.batch_index);
        if let Some(error) = self.service_error {
            return Err(error);
        }
        assert_eq!(self.stored.state, JobState::Probing);
        assert!(admin_gates::probe_ready(&self.stored));
        let mut answer = ProbeOutcome {
            batch_index: request.batch_index,
            present: request.segment_ids.clone(),
            missing: vec![],
        };
        if self.missing {
            answer.missing.push(answer.present.pop().unwrap());
        }
        if self.malformed {
            answer.present.clear();
            answer.missing.clear();
        }
        Ok(answer)
    }

    async fn persist(
        &mut self,
        expected: &JobCheckpoint,
        next: &JobCheckpoint,
    ) -> Result<bool, StageError> {
        self.events.push("persist");
        if self.pending {
            std::future::pending::<()>().await;
        }
        if self.store_error {
            return Err(StageError::Store);
        }
        if self.stale || expected != &self.stored {
            return Ok(false);
        }
        self.stored = serde_json::from_str(&serde_json::to_string(next).unwrap()).unwrap();
        Ok(true)
    }
}

fn fixture() -> (JobCheckpoint, Document, Runtime) {
    let msg =
        crate::dispatch::build_upload_dispatch("stage_user", crate::fixtures::SMALL_NZB).unwrap();
    let mut cp = crate::checkpoint::initial(&msg);
    cp.source = JobSource::AdminUpload;
    cp.user_id.clear();
    let parsed = nntune_core::nzb::parse(crate::fixtures::SMALL_NZB.as_bytes())
        .unwrap_or_else(|_| panic!("synthetic parse rejected"));
    let runtime = Runtime {
        stored: cp.clone(),
        events: vec![],
        batches: vec![],
        release: Some(RELEASE.into()),
        missing: false,
        malformed: false,
        service_error: None,
        store_error: false,
        stale: false,
        pending: false,
    };
    (cp, parsed, runtime)
}

async fn step(
    runtime: &mut Runtime,
    cp: &mut JobCheckpoint,
    parsed: &Document,
) -> Result<StageOutcome, StageError> {
    let job = cp.job_id.clone();
    let key = cp.idempotency_key.clone();
    run_stage(runtime, cp, &job, &key, parsed).await
}

fn at_state(runtime: &mut Runtime, cp: &mut JobCheckpoint, state: JobState) {
    cp.state = state;
    runtime.stored = cp.clone();
}

#[tokio::test]
async fn storage_reconstructs_each_stage_and_refuses_skips_and_unrelated_mutations() {
    let (mut cp, parsed, mut runtime) = fixture();
    for _ in 0..4 {
        let expected = cp.clone();
        step(&mut runtime, &mut cp, &parsed).await.unwrap();
        let validate = |next: &JobCheckpoint| {
            validate_stage_transition(
                &expected,
                next,
                &expected.job_id,
                &expected.idempotency_key,
                &parsed,
            )
        };
        assert_eq!(validate(&cp), Ok(()));
        assert_eq!(validate(&expected), Err(StageError::Invalid));
        let mut bad = cp.clone();
        bad.attempt += 1;
        assert_eq!(validate(&bad), Err(StageError::Invalid));
        bad = cp.clone();
        bad.failure_code = Some("probe_failed".into());
        assert_eq!(validate(&bad), Err(StageError::Invalid));
        for state in [JobState::Done, JobState::Rejected, JobState::Searching] {
            bad = cp.clone();
            bad.state = state;
            assert_eq!(validate(&bad), Err(StageError::Invalid));
        }
    }
    let mut done = cp.clone();
    jobs::advance_checkpoint(&mut done, false);
    assert_eq!(done.state, JobState::Done);
    assert_eq!(
        validate_stage_transition(&cp, &done, &cp.job_id, &cp.idempotency_key, &parsed),
        Err(StageError::Invalid)
    );
}

#[tokio::test]
async fn storage_rechecks_probe_batch_order_geometry_and_unchanged_evidence() {
    let (mut cp, mut parsed, mut runtime) = fixture();
    let mut template = parsed.files[0].segments[0].clone();
    let seed = template.message_id.clone();
    parsed.files.truncate(1);
    parsed.files[0].segments.clear();
    for index in 0..(MAX_PROBE_SEGMENTS + 1) {
        template.message_id = format!("{index}{seed}");
        parsed.files[0].segments.push(template.clone());
    }
    at_state(&mut runtime, &mut cp, JobState::Matching);
    step(&mut runtime, &mut cp, &parsed).await.unwrap();
    let first = cp.clone();
    step(&mut runtime, &mut cp, &parsed).await.unwrap();
    let second = cp.clone();
    let validate = |expected: &JobCheckpoint, next: &JobCheckpoint| {
        validate_stage_transition(
            expected,
            next,
            &expected.job_id,
            &expected.idempotency_key,
            &parsed,
        )
    };
    assert_eq!(validate(&first, &second), Ok(()));
    let mut bad = second.clone();
    let tally = bad.probe_tally.as_mut().unwrap();
    tally.missing_by_batch.remove(&0);
    tally.missing_by_batch.insert(1, 0);
    assert_eq!(validate(&first, &bad), Err(StageError::Invalid));
    step(&mut runtime, &mut cp, &parsed).await.unwrap();
    assert_eq!(validate(&second, &cp), Ok(()));
    bad = cp.clone();
    bad.probe_tally
        .as_mut()
        .unwrap()
        .missing_by_batch
        .insert(1, 2);
    bad.state = JobState::NeedsReview;
    assert_eq!(validate(&second, &bad), Err(StageError::Invalid));
    bad = cp.clone();
    bad.probe_tally
        .as_mut()
        .unwrap()
        .missing_by_batch
        .insert(0, 1);
    assert_eq!(validate(&second, &bad), Err(StageError::Invalid));
    assert_eq!(validate(&first, &cp), Err(StageError::Invalid));
}

#[tokio::test]
async fn every_stage_persists_before_the_next_service_and_stops_before_publication() {
    let (mut cp, parsed, mut runtime) = fixture();
    for expected in [
        StageOutcome::Continue,
        StageOutcome::Continue,
        StageOutcome::Continue,
        StageOutcome::ReadyToPublish,
    ] {
        assert_eq!(step(&mut runtime, &mut cp, &parsed).await, Ok(expected));
        cp = runtime.stored.clone();
    }
    assert_eq!(
        runtime.events,
        ["persist", "persist", "match", "persist", "probe", "persist"]
    );
    assert_eq!(runtime.batches, [0]);
    assert!(admin_gates::publish_ready(&cp));
    assert_eq!(
        step(&mut runtime, &mut cp, &parsed).await,
        Ok(StageOutcome::ReadyToPublish)
    );
    assert_eq!(runtime.events.len(), 6);
    assert_eq!(cp.state, JobState::Writing);
    let stored = serde_json::to_string(&runtime.stored).unwrap();
    for file in &parsed.files {
        assert!(!stored.contains(&file.subject));
        for segment in &file.segments {
            assert!(!stored.contains(&segment.message_id));
        }
    }
}

#[tokio::test]
async fn resume_asks_only_one_unsaved_batch_and_rechecks_the_full_original_plan() {
    let (mut cp, mut parsed, mut runtime) = fixture();
    let mut template = parsed.files[0].segments[0].clone();
    let seed = template.message_id.clone();
    parsed.files.truncate(1);
    parsed.files[0].segments.clear();
    // Expand an existing synthetic identity in memory, not a payload fixture.
    for index in 0..(MAX_PROBE_SEGMENTS * 2 + 1) {
        template.message_id = format!("{index}{seed}");
        parsed.files[0].segments.push(template.clone());
    }
    at_state(&mut runtime, &mut cp, JobState::Matching);
    assert_eq!(
        step(&mut runtime, &mut cp, &parsed).await,
        Ok(StageOutcome::Continue)
    );
    for expected in [
        StageOutcome::Continue,
        StageOutcome::Continue,
        StageOutcome::ReadyToPublish,
    ] {
        cp = runtime.stored.clone();
        assert_eq!(step(&mut runtime, &mut cp, &parsed).await, Ok(expected));
    }
    assert_eq!(runtime.batches, [0, 1, 2]);
    assert_eq!(cp.probe_tally.as_ref().unwrap().total_batches, 3);
    let before = cp.clone();
    parsed.files[0].segments.truncate(1);
    assert_eq!(
        step(&mut runtime, &mut cp, &parsed).await,
        Err(StageError::Invalid)
    );
    assert_eq!(cp, before);
}

#[tokio::test]
async fn absent_match_archive_and_gap_park_without_unearned_provider_work() {
    let (mut cp, parsed, mut runtime) = fixture();
    at_state(&mut runtime, &mut cp, JobState::Matching);
    runtime.release = None;
    assert_eq!(
        step(&mut runtime, &mut cp, &parsed).await,
        Ok(StageOutcome::Parked)
    );
    assert_eq!(runtime.events, ["match", "persist"]);
    assert_eq!(
        step(&mut runtime, &mut cp, &parsed).await,
        Ok(StageOutcome::Parked)
    );
    assert_eq!(runtime.events.len(), 2);
    assert!(admin_gates::approve_review(&mut cp, RELEASE, 1));
    runtime.stored = cp.clone();
    runtime.missing = true;
    assert_eq!(
        step(&mut runtime, &mut cp, &parsed).await,
        Ok(StageOutcome::Parked)
    );
    assert_eq!(cp.state, JobState::NeedsReview);
    assert!(!admin_gates::publish_ready(&cp));

    for suffix in [".rar", ".7z"] {
        let (mut cp, _, mut runtime) = fixture();
        let body = crate::fixtures::SMALL_NZB.replace(".flac", suffix);
        let parsed = nntune_core::nzb::parse(body.as_bytes())
            .unwrap_or_else(|_| panic!("synthetic parse rejected"));
        at_state(&mut runtime, &mut cp, JobState::Parsing);
        assert_eq!(
            step(&mut runtime, &mut cp, &parsed).await,
            Ok(StageOutcome::Parked)
        );
        assert_eq!(runtime.events, ["persist"]);
        assert!(jobs::resume_archive(&mut cp));
        runtime.stored = cp.clone();
        assert_eq!(
            step(&mut runtime, &mut cp, &parsed).await,
            Ok(StageOutcome::Parked)
        );
        assert_eq!(runtime.events, ["persist", "persist"]);
        at_state(&mut runtime, &mut cp, JobState::Matching);
        assert_eq!(
            step(&mut runtime, &mut cp, &parsed).await,
            Err(StageError::Invalid)
        );
        assert_eq!(runtime.events.len(), 2);
    }
}

#[tokio::test]
async fn failed_stale_or_cancelled_persistence_never_advances_the_local_checkpoint() {
    for failure in 0..3 {
        let (mut cp, parsed, mut runtime) = fixture();
        at_state(&mut runtime, &mut cp, JobState::Matching);
        let before = cp.clone();
        runtime.store_error = failure == 0;
        runtime.stale = failure == 1;
        runtime.pending = failure == 2;
        if runtime.pending {
            let future = step(&mut runtime, &mut cp, &parsed);
            futures_util::pin_mut!(future);
            assert!(futures_util::poll!(future).is_pending());
        } else {
            assert_eq!(
                step(&mut runtime, &mut cp, &parsed).await,
                Err(if runtime.stale {
                    StageError::Stale
                } else {
                    StageError::Store
                })
            );
        }
        assert_eq!(cp, before);
        assert_eq!(runtime.stored, before);
        assert_eq!(runtime.events, ["match", "persist"]);
        assert!(runtime.batches.is_empty());
        runtime.store_error = false;
        runtime.stale = false;
        runtime.pending = false;
        assert_eq!(
            step(&mut runtime, &mut cp, &parsed).await,
            Ok(StageOutcome::Continue)
        );
        assert_eq!(runtime.events, ["match", "persist", "match", "persist"]);
    }
}

#[tokio::test]
async fn failed_or_malformed_service_answers_do_not_persist_or_mutate() {
    for state in [JobState::Matching, JobState::Probing] {
        let (mut cp, parsed, mut runtime) = fixture();
        cp.state = JobState::Matching;
        if state == JobState::Probing {
            assert!(admin_gates::complete_match(&mut cp, Some(RELEASE), 1));
        }
        runtime.stored = cp.clone();
        let before = cp.clone();
        let error = if state == JobState::Matching {
            StageError::Match
        } else {
            StageError::Probe
        };
        runtime.service_error = Some(error);
        assert_eq!(step(&mut runtime, &mut cp, &parsed).await, Err(error));
        assert_eq!(cp, before);
        assert_eq!(runtime.events.len(), 1);
        runtime.service_error = None;
        runtime.malformed = true;
        runtime.release = Some("invalid".into());
        assert_eq!(
            step(&mut runtime, &mut cp, &parsed).await,
            Err(StageError::Invalid)
        );
        assert_eq!(cp, before);
        assert!(!runtime.events.contains(&"persist"));
    }
}

#[tokio::test]
async fn foreign_identity_legacy_stages_and_invalid_plans_stop_before_services() {
    let mutations: Vec<fn(&mut JobCheckpoint)> = vec![
        |cp| cp.source = JobSource::NzbUpload,
        |cp| cp.attempt = 0,
        |cp| cp.repair_rounds = 1,
        |cp| cp.provider_id = Some("old".into()),
        |cp| cp.state = JobState::Searching,
        |cp| cp.state = JobState::Verifying,
        |cp| cp.state = JobState::Repairing,
        |cp| cp.state = JobState::Transcoding,
    ];
    for mutate in mutations {
        let (mut cp, parsed, mut runtime) = fixture();
        mutate(&mut cp);
        let before = cp.clone();
        assert_eq!(
            step(&mut runtime, &mut cp, &parsed).await,
            Err(StageError::Invalid)
        );
        assert_eq!(cp, before);
        assert!(runtime.events.is_empty());
    }
    for mutation in 0..6 {
        let (mut cp, mut parsed, mut runtime) = fixture();
        let job = cp.job_id.clone();
        let key = cp.idempotency_key.clone();
        match mutation {
            0 => cp.job_id.push('x'),
            1 => cp.idempotency_key.push('x'),
            2 => parsed.files.clear(),
            3 => parsed.files[0].segments.clear(),
            4 => parsed.files[0].segments[0].encoded_bytes = 0,
            _ => parsed.files[0].segments[0].message_id.push('\n'),
        }
        let before = cp.clone();
        assert_eq!(
            run_stage(&mut runtime, &mut cp, &job, &key, &parsed).await,
            Err(StageError::Invalid)
        );
        assert_eq!(cp, before);
        assert!(runtime.events.is_empty());
    }
}
