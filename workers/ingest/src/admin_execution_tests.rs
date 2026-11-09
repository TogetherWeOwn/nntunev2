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

#[test]
fn service_failures_reuse_bounded_backoff_preserve_probe_progress_and_park_exhaustion() {
    let (mut cp, mut parsed, _) = fixture();
    let segment = parsed.files[0].segments[0].clone();
    parsed.files[0]
        .segments
        .resize(MAX_PROBE_SEGMENTS + 1, segment);
    cp.state = JobState::Matching;
    assert!(admin_gates::complete_match(&mut cp, Some(RELEASE), 2));
    cp.probe_tally
        .as_mut()
        .unwrap()
        .missing_by_batch
        .insert(0, 0);
    let proof = cp.probe_tally.clone();
    for (spent, delay) in [(1, 5), (2, 15), (3, 30), (4, 60)] {
        let (next, outcome) = service_failure_checkpoint(
            &cp,
            StageError::Probe,
            None,
            &cp.job_id,
            &cp.idempotency_key,
            &parsed,
        )
        .unwrap();
        assert_eq!(outcome, ServiceFailureOutcome::Retry { delay_secs: delay });
        assert_eq!((next.state, next.attempt), (JobState::Probing, spent + 1));
        assert_eq!(next.probe_tally, proof);
        assert_eq!(next.admin_release_mbid.as_deref(), Some(RELEASE));
        assert!(next.failure_code.is_none() && next.failure_reason.is_none());
        assert_eq!(
            validate_stage_transition(&cp, &next, &cp.job_id, &cp.idempotency_key, &parsed),
            Err(StageError::Invalid)
        );
        cp = next;
    }
    let (next, outcome) = service_failure_checkpoint(
        &cp,
        StageError::Probe,
        None,
        &cp.job_id,
        &cp.idempotency_key,
        &parsed,
    )
    .unwrap();
    assert_eq!(outcome, ServiceFailureOutcome::Parked);
    assert_eq!(
        (next.state, next.attempt),
        (JobState::NeedsReview, jobs::MAX_ATTEMPTS)
    );
    assert_eq!(next.probe_tally, proof);
    assert_eq!(
        next.failure_code.as_deref(),
        Some(crate::retry::ERR_PROBE_FAILED)
    );
    assert_eq!(
        next.failure_reason.as_deref(),
        Some("retry budget exhausted")
    );
    let retried = controlled_checkpoint(
        &next,
        Control::RetryReview,
        &cp.job_id,
        &cp.idempotency_key,
        &parsed,
    )
    .unwrap();
    assert_eq!((retried.state, retried.attempt), (JobState::Matching, 1));
    assert!(retried.probe_tally.is_none() && retried.admin_release_mbid.is_none());
    assert!(retried.failure_code.is_none() && retried.failure_reason.is_none());
}

#[test]
fn service_failure_classification_bounds_hints_and_refuses_non_service_or_wrong_stage_errors() {
    let (mut cp, mut parsed, _) = fixture();
    cp.state = JobState::Matching;
    for (hint, delay) in [(0, 5), (1, 5), (30, 30), (u32::MAX, 300)] {
        let (next, outcome) = service_failure_checkpoint(
            &cp,
            StageError::Match,
            Some(hint),
            &cp.job_id,
            &cp.idempotency_key,
            &parsed,
        )
        .unwrap();
        assert_eq!(outcome, ServiceFailureOutcome::Retry { delay_secs: delay });
        assert_eq!(next.attempt, 2);
        assert_eq!(cp.attempt, 1);
    }
    for error in [
        StageError::Invalid,
        StageError::Probe,
        StageError::Store,
        StageError::Stale,
    ] {
        assert_eq!(
            service_failure_checkpoint(&cp, error, None, &cp.job_id, &cp.idempotency_key, &parsed)
                .err(),
            Some(StageError::Invalid)
        );
    }
    for state in [
        JobState::Queued,
        JobState::Parsing,
        JobState::Searching,
        JobState::Writing,
        JobState::NeedsReview,
        JobState::NeedsArchiveRead,
        JobState::Done,
        JobState::Rejected,
        JobState::Failed,
    ] {
        cp.state = state;
        assert_eq!(
            service_failure_checkpoint(
                &cp,
                StageError::Match,
                None,
                &cp.job_id,
                &cp.idempotency_key,
                &parsed
            )
            .err(),
            Some(StageError::Invalid)
        );
    }
    cp.state = JobState::Matching;
    for attempt in [0, jobs::MAX_ATTEMPTS + 1] {
        cp.attempt = attempt;
        assert_eq!(
            service_failure_checkpoint(
                &cp,
                StageError::Match,
                None,
                &cp.job_id,
                &cp.idempotency_key,
                &parsed
            )
            .err(),
            Some(StageError::Invalid)
        );
    }
    cp.attempt = 1;
    assert!(admin_gates::complete_match(&mut cp, Some(RELEASE), 1));
    cp.probe_tally
        .as_mut()
        .unwrap()
        .missing_by_batch
        .insert(0, 0);
    assert_eq!(
        service_failure_checkpoint(
            &cp,
            StageError::Probe,
            None,
            &cp.job_id,
            &cp.idempotency_key,
            &parsed
        )
        .err(),
        Some(StageError::Invalid)
    );
    cp.probe_tally.as_mut().unwrap().missing_by_batch.clear();
    cp.probe_tally.as_mut().unwrap().total_batches = 2;
    assert_eq!(
        service_failure_checkpoint(
            &cp,
            StageError::Probe,
            None,
            &cp.job_id,
            &cp.idempotency_key,
            &parsed
        )
        .err(),
        Some(StageError::Invalid)
    );
    cp.state = JobState::Matching;
    parsed.files[0].role = "archive".into();
    assert_eq!(
        service_failure_checkpoint(
            &cp,
            StageError::Match,
            None,
            &cp.job_id,
            &cp.idempotency_key,
            &parsed
        )
        .err(),
        Some(StageError::Invalid)
    );
}

#[test]
fn manual_approval_derives_full_original_plan_and_refuses_invalid_inputs_without_mutation() {
    let (mut cp, mut parsed, _) = fixture();
    cp.state = JobState::NeedsReview;
    let segment = parsed.files[0].segments[0].clone();
    parsed.files[0]
        .segments
        .resize(MAX_PROBE_SEGMENTS + 1, segment);
    let next = controlled_checkpoint(
        &cp,
        Control::ApproveReview { release: RELEASE },
        &cp.job_id,
        &cp.idempotency_key,
        &parsed,
    )
    .unwrap();
    assert_eq!(next.state, JobState::Probing);
    assert_eq!(next.probe_tally.as_ref().unwrap().total_batches, 2);
    assert!(next
        .probe_tally
        .as_ref()
        .unwrap()
        .missing_by_batch
        .is_empty());
    let before = cp.clone();
    for release in ["", "invalid", "11111111-1111-4111-8111-111111111111 "] {
        assert_eq!(
            controlled_checkpoint(
                &cp,
                Control::ApproveReview { release },
                &cp.job_id,
                &cp.idempotency_key,
                &parsed
            ),
            Err(StageError::Invalid)
        );
    }
    assert_eq!(
        controlled_checkpoint(
            &cp,
            Control::Reject,
            "different_job",
            &cp.idempotency_key,
            &parsed
        ),
        Err(StageError::Invalid)
    );
    parsed.files[0].segments.clear();
    for action in [
        Control::ApproveReview { release: RELEASE },
        Control::RetryReview,
    ] {
        assert_eq!(
            controlled_checkpoint(&cp, action, &cp.job_id, &cp.idempotency_key, &parsed),
            Err(StageError::Invalid)
        );
    }
    assert_eq!(cp, before);
}

#[test]
fn archive_controls_cannot_approve_or_retry_past_parse_and_resume_keeps_original_identity() {
    for suffix in [".rar", ".7z"] {
        let (mut cp, _, _) = fixture();
        let body = crate::fixtures::SMALL_NZB.replace(".flac", suffix);
        let parsed = nntune_core::nzb::parse(body.as_bytes())
            .unwrap_or_else(|_| panic!("synthetic parse rejected"));
        cp.state = JobState::NeedsReview;
        for action in [
            Control::ApproveReview { release: RELEASE },
            Control::RetryReview,
        ] {
            assert_eq!(
                controlled_checkpoint(&cp, action, &cp.job_id, &cp.idempotency_key, &parsed),
                Err(StageError::Invalid)
            );
        }
        cp.state = JobState::NeedsArchiveRead;
        let before = cp.clone();
        let mut resumed = controlled_checkpoint(
            &cp,
            Control::ResumeArchive,
            &cp.job_id,
            &cp.idempotency_key,
            &parsed,
        )
        .unwrap();
        assert_eq!(resumed.state, JobState::Parsing);
        assert_eq!(
            (&resumed.job_id, &resumed.user_id, &resumed.idempotency_key),
            (&cp.job_id, &cp.user_id, &cp.idempotency_key)
        );
        assert!(resumed.admin_release_mbid.is_none() && resumed.probe_tally.is_none());
        // No archive capability is enabled by this helper: unchanged parse parks again.
        assert!(jobs::complete_admin_parse(&mut resumed, &parsed));
        assert_eq!(resumed.state, JobState::NeedsArchiveRead);
        assert_eq!(cp, before);
    }
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
