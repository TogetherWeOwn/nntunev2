//! Durable service-failure settlement, not a live queue/timer or schema test.
use super::*;
use nntune_ingest::admin_execution::ServiceFailureOutcome;

struct FailedService(StageError);

impl StageRuntime for FailedService {
    async fn match_release(&mut self, _: &Document) -> Result<Option<String>, StageError> {
        assert_eq!(self.0, StageError::Match);
        Err(self.0)
    }

    async fn probe(&mut self, _: &ProbeRequest) -> Result<ProbeOutcome, StageError> {
        assert_eq!(self.0, StageError::Probe);
        Err(self.0)
    }

    async fn persist(&mut self, _: &JobCheckpoint, _: &JobCheckpoint) -> Result<bool, StageError> {
        panic!("service failure reached ordinary stage persistence")
    }
}

async fn advance_to(
    app: &mut tokio_postgres::Client,
    original: &OriginalBody,
    snapshot: Snapshot,
    state: JobState,
) -> Snapshot {
    let mut cp = snapshot.checkpoint().clone();
    let mut runtime = DatabaseRuntime {
        app,
        original,
        snapshot,
        services: Runtime(cp.clone()),
    };
    for _ in 0..4 {
        if cp.state == state {
            return runtime.snapshot;
        }
        run_stage(
            &mut runtime,
            &mut cp,
            original.job_id(),
            original.idempotency_key(),
            original.parsed(),
        )
        .await
        .unwrap();
    }
    assert_eq!(cp.state, state);
    runtime.snapshot
}

#[tokio::test]
async fn service_failure_budget_commits_before_retry_and_exhausts_into_review_without_duplicates() {
    for (state, failure, code) in [
        (JobState::Matching, StageError::Match, "match_failed"),
        (JobState::Probing, StageError::Probe, "probe_failed"),
    ] {
        let (mut app, original, initial) = seeded().await;
        let mut snapshot = advance_to(&mut app, &original, initial, state).await;
        let hint = store::prepare_delivery(&app, &input(&original), snapshot.generation())
            .await
            .unwrap()
            .unwrap();
        let proof = snapshot.checkpoint().probe_tally.clone();
        let release = snapshot.checkpoint().admin_release_mbid.clone();
        for spent in 1..=nntune_ingest::jobs::MAX_ATTEMPTS {
            let expected = snapshot.clone();
            let mut cp = expected.checkpoint().clone();
            assert_eq!(
                run_stage(
                    &mut FailedService(failure),
                    &mut cp,
                    original.job_id(),
                    original.idempotency_key(),
                    original.parsed()
                )
                .await,
                Err(failure)
            );
            assert_eq!(&cp, expected.checkpoint());
            assert!(
                store::load(&app, &input(&original), hint.generation())
                    .await
                    .unwrap()
                    == expected
            );
            let (saved, outcome) = store::persist_service_failure(
                &mut app,
                &input(&original),
                &expected,
                failure,
                None,
            )
            .await
            .unwrap();
            assert_eq!(saved.revision(), expected.revision() + 1);
            assert_eq!(saved.generation(), hint.generation());
            assert_eq!(saved.checkpoint().probe_tally, proof);
            assert_eq!(saved.checkpoint().admin_release_mbid, release);
            assert!(
                store::load(&app, &input(&original), hint.generation())
                    .await
                    .unwrap()
                    == saved
            );
            // A late duplicate result cannot spend the next attempt's budget.
            let before = authority_row(&app).await;
            assert_eq!(
                store::persist_service_failure(
                    &mut app,
                    &input(&original),
                    &expected,
                    failure,
                    None
                )
                .await
                .err(),
                Some(StoreError::Stale)
            );
            assert_eq!(authority_row(&app).await, before);
            if spent < nntune_ingest::jobs::MAX_ATTEMPTS {
                assert_eq!(
                    outcome,
                    ServiceFailureOutcome::Retry {
                        delay_secs: nntune_ingest::retry::retry_delay_secs(spent)
                    }
                );
                assert_eq!(
                    (saved.checkpoint().state, saved.checkpoint().attempt),
                    (state, spent + 1)
                );
                assert!(
                    saved.checkpoint().failure_code.is_none()
                        && saved.checkpoint().failure_reason.is_none()
                );
                assert!(
                    store::load_delivery(&app, &input(&original), &hint)
                        .await
                        .unwrap()
                        .unwrap()
                        == saved
                );
            } else {
                assert_eq!(outcome, ServiceFailureOutcome::Parked);
                assert_eq!(
                    (
                        saved.status(),
                        saved.checkpoint().state,
                        saved.checkpoint().attempt
                    ),
                    ("needs_review", JobState::NeedsReview, spent)
                );
                assert_eq!(saved.checkpoint().failure_code.as_deref(), Some(code));
                assert_eq!(
                    saved.checkpoint().failure_reason.as_deref(),
                    Some("retry budget exhausted")
                );
                assert!(store::load_delivery(&app, &input(&original), &hint)
                    .await
                    .unwrap()
                    .is_none());
                let mut queue = DeliveryQueue::default();
                assert_eq!(
                    store::enqueue_delivery(&app, &input(&original), hint.generation(), &mut queue)
                        .await,
                    Ok(false)
                );
                assert_eq!(queue.calls, 0);
            }
            snapshot = saved;
        }
        let audit_count: i64 = app
            .query_one("SELECT COUNT(*) FROM audit_log", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(audit_count, 0);
        assert_eq!(counts(&app).await, (0, 0, 0));
        let retried = store::apply_control(
            &mut app,
            actor(),
            &input(&original),
            &snapshot,
            Control::RetryReview,
        )
        .await
        .unwrap();
        assert_eq!(
            (retried.checkpoint().state, retried.checkpoint().attempt),
            (JobState::Matching, 1)
        );
        assert!(retried.generation() > hint.generation());
        assert!(
            retried.checkpoint().probe_tally.is_none()
                && retried.checkpoint().admin_release_mbid.is_none()
        );
        assert!(
            retried.checkpoint().failure_code.is_none()
                && retried.checkpoint().failure_reason.is_none()
        );
        let before = authority_row(&app).await;
        assert_eq!(
            store::persist_service_failure(&mut app, &input(&original), &snapshot, failure, None)
                .await
                .err(),
            Some(StoreError::Invalid)
        );
        assert_eq!(
            store::load_delivery(&app, &input(&original), &hint)
                .await
                .err(),
            Some(StoreError::Stale)
        );
        assert_eq!(authority_row(&app).await, before);
    }
}

#[tokio::test]
async fn service_failure_refuses_wrong_errors_and_late_results_after_progress_or_rejection() {
    let (mut app, original, initial) = seeded().await;
    let matching = advance_to(&mut app, &original, initial, JobState::Matching).await;
    let before = authority_row(&app).await;
    for error in [
        StageError::Invalid,
        StageError::Probe,
        StageError::Store,
        StageError::Stale,
    ] {
        assert_eq!(
            store::persist_service_failure(&mut app, &input(&original), &matching, error, None)
                .await
                .err(),
            Some(StoreError::Invalid)
        );
        assert_eq!(authority_row(&app).await, before);
    }
    let probing = advance_to(&mut app, &original, matching.clone(), JobState::Probing).await;
    let before = authority_row(&app).await;
    assert_eq!(
        store::persist_service_failure(
            &mut app,
            &input(&original),
            &matching,
            StageError::Match,
            None
        )
        .await
        .err(),
        Some(StoreError::Stale)
    );
    assert_eq!(authority_row(&app).await, before);
    let mut changed = probing.checkpoint().clone();
    changed.attempt += 1;
    let forged = probing
        .successor(changed, false, &identity(&original))
        .unwrap();
    // Hold revision/generation equal: the whole checkpoint comparison must
    // independently reject this different attempt, not just a counter mismatch.
    let forged = Snapshot::decode(
        &forged.encode().unwrap(),
        probing.revision(),
        &identity(&original),
    )
    .unwrap();
    assert_eq!(
        store::persist_service_failure(
            &mut app,
            &input(&original),
            &forged,
            StageError::Probe,
            None
        )
        .await
        .err(),
        Some(StoreError::Stale)
    );
    assert_eq!(authority_row(&app).await, before);
    app.batch_execute("UPDATE ingest_originals SET nzb_bytes = nzb_bytes + 1")
        .await
        .unwrap();
    assert_eq!(
        store::persist_service_failure(
            &mut app,
            &input(&original),
            &probing,
            StageError::Probe,
            None
        )
        .await
        .err(),
        Some(StoreError::Stale)
    );
    assert_eq!(authority_row(&app).await, before);
    app.batch_execute("UPDATE ingest_originals SET nzb_bytes = nzb_bytes - 1")
        .await
        .unwrap();
    let rejected = store::apply_control(
        &mut app,
        actor(),
        &input(&original),
        &probing,
        Control::Reject,
    )
    .await
    .unwrap();
    let before = authority_row(&app).await;
    assert_eq!(
        store::persist_service_failure(
            &mut app,
            &input(&original),
            &probing,
            StageError::Probe,
            None
        )
        .await
        .err(),
        Some(StoreError::Stale)
    );
    assert_eq!(
        store::persist_service_failure(
            &mut app,
            &input(&original),
            &rejected,
            StageError::Probe,
            None
        )
        .await
        .err(),
        Some(StoreError::Invalid)
    );
    assert_eq!(authority_row(&app).await, before);
    assert_eq!(counts(&app).await, (0, 0, 0));
}

#[tokio::test]
async fn service_failure_sql_refusal_zero_row_and_revision_overflow_preserve_authority() {
    let (mut app, original, initial) = seeded().await;
    let matching = advance_to(&mut app, &original, initial, JobState::Matching).await;
    for (function, error) in [
        ("CREATE OR REPLACE FUNCTION pg_temp.refuse_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'refused'; END $$", StoreError::Database),
        ("CREATE OR REPLACE FUNCTION pg_temp.refuse_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$", StoreError::Stale),
    ] {
        app.batch_execute(function).await.unwrap();
        app.batch_execute("CREATE TRIGGER refuse_failure BEFORE UPDATE ON ingest_jobs FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_failure()").await.unwrap();
        let before = authority_row(&app).await;
        assert_eq!(store::persist_service_failure(&mut app, &input(&original), &matching, StageError::Match, None).await.err(), Some(error));
        assert_eq!(authority_row(&app).await, before);
        app.batch_execute("DROP TRIGGER refuse_failure ON ingest_jobs").await.unwrap();
    }
    // Restored control: the same expected authority can spend exactly once.
    let (saved, outcome) = store::persist_service_failure(
        &mut app,
        &input(&original),
        &matching,
        StageError::Match,
        Some(u32::MAX),
    )
    .await
    .unwrap();
    assert_eq!(outcome, ServiceFailureOutcome::Retry { delay_secs: 300 });
    assert_eq!(saved.checkpoint().attempt, 2);
    let overflow =
        Snapshot::decode(&saved.encode().unwrap(), i64::MAX, &identity(&original)).unwrap();
    seed(&app, &overflow).await;
    let before = authority_row(&app).await;
    assert_eq!(
        store::persist_service_failure(
            &mut app,
            &input(&original),
            &overflow,
            StageError::Match,
            None
        )
        .await
        .err(),
        Some(StoreError::Invalid)
    );
    assert_eq!(authority_row(&app).await, before);
    assert_eq!(counts(&app).await, (0, 0, 0));
}
