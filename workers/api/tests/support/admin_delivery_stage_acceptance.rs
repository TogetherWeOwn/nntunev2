//! Canonical delivery/runner/SQL composition, not live broker or schema acceptance.
#[path = "admin_continuation_acceptance.rs"]
mod continuation;
use super::*;
use nntune_ingest::{
    admin_checkpoint_store::{DeliveryStageOutcome as Outcome, ServiceError, StageServices},
    admin_execution::{ServiceFailureOutcome, StageOutcome},
};

#[derive(Default)]
struct Services {
    calls: Vec<&'static str>,
    fail: Option<ServiceError>,
    no_match: bool,
    malformed: bool,
    pending: bool,
}

impl StageServices for Services {
    async fn match_release(&mut self, _: &Document) -> Result<Option<String>, ServiceError> {
        self.calls.push("match");
        if self.pending {
            std::future::pending::<()>().await;
        }
        if let Some(error) = self.fail {
            return Err(error);
        }
        Ok((!self.no_match).then(|| RELEASE.into()))
    }

    async fn probe(&mut self, req: &ProbeRequest) -> Result<ProbeOutcome, ServiceError> {
        self.calls.push("probe");
        if self.pending {
            std::future::pending::<()>().await;
        }
        if let Some(error) = self.fail {
            return Err(error);
        }
        Ok(ProbeOutcome {
            batch_index: req.batch_index,
            present: if self.malformed {
                vec![]
            } else {
                req.segment_ids.clone()
            },
            missing: vec![],
        })
    }
}

async fn hint(app: &tokio_postgres::Client, original: &OriginalBody) -> store::Delivery {
    store::prepare_delivery(app, &input(original), 1)
        .await
        .unwrap()
        .unwrap()
}

async fn advance(
    app: &mut tokio_postgres::Client,
    original: &OriginalBody,
    delivery: &store::Delivery,
    services: &mut Services,
    state: JobState,
) -> Snapshot {
    for _ in 0..5 {
        let saved = store::load(app, &input(original), delivery.generation())
            .await
            .unwrap();
        if saved.checkpoint().state == state {
            return saved;
        }
        assert!(matches!(
            store::run_delivery_stage(app, &input(original), delivery, services)
                .await
                .unwrap(),
            Outcome::Advanced(StageOutcome::Continue | StageOutcome::ReadyToPublish)
        ));
    }
    panic!("stage did not advance within its bounded fixture plan")
}

#[tokio::test]
async fn delivery_stage_reloads_committed_progress_matches_before_probe_and_never_publishes() {
    let (mut app, original, _) = seeded().await;
    let delivery = hint(&app, &original).await;
    let mut services = Services::default();
    // Replay the same initial hint through every stage, not a caller checkpoint.
    let saved = advance(
        &mut app,
        &original,
        &delivery,
        &mut services,
        JobState::Writing,
    )
    .await;
    assert_eq!(services.calls, ["match", "probe"]);
    assert_eq!(
        (
            saved.revision(),
            saved.generation(),
            saved.checkpoint().attempt
        ),
        (5, 1, 1)
    );
    let before = authority_row(&app).await;
    for _ in 0..2 {
        assert_eq!(
            store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services).await,
            Ok(Outcome::Advanced(StageOutcome::ReadyToPublish))
        );
        assert_eq!(authority_row(&app).await, before);
    }
    assert_eq!(services.calls, ["match", "probe"]);
    assert_eq!(counts(&app).await, (0, 0, 0));
    store::apply_control(
        &mut app,
        actor(),
        &input(&original),
        &saved,
        Control::Reject,
    )
    .await
    .unwrap();
    assert_eq!(
        store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services).await,
        Err(StoreError::Stale)
    );
    assert_eq!(services.calls, ["match", "probe"]);
}

#[tokio::test]
async fn delivery_stage_commits_service_budget_before_retry_and_exhaustion_parks_without_work() {
    for state in [JobState::Matching, JobState::Probing] {
        let (mut app, original, _) = seeded().await;
        let delivery = hint(&app, &original).await;
        let mut services = Services::default();
        let expected = advance(&mut app, &original, &delivery, &mut services, state).await;
        services.calls.clear();
        services.fail = Some(ServiceError::Transient {
            retry_after_secs: Some(u32::MAX),
        });
        for spent in 1..=nntune_ingest::jobs::MAX_ATTEMPTS {
            let result =
                store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services)
                    .await;
            let saved = store::load(&app, &input(&original), delivery.generation())
                .await
                .unwrap();
            assert_eq!(saved.revision(), expected.revision() + i64::from(spent));
            assert_eq!(saved.generation(), expected.generation());
            assert_eq!(
                saved.checkpoint().admin_release_mbid,
                expected.checkpoint().admin_release_mbid
            );
            assert_eq!(
                saved.checkpoint().probe_tally,
                expected.checkpoint().probe_tally
            );
            if spent < nntune_ingest::jobs::MAX_ATTEMPTS {
                assert_eq!(
                    result,
                    Ok(Outcome::ServiceFailure(ServiceFailureOutcome::Retry {
                        delay_secs: 300
                    }))
                );
                assert_eq!(
                    (saved.checkpoint().state, saved.checkpoint().attempt),
                    (state, spent + 1)
                );
            } else {
                assert_eq!(
                    result,
                    Ok(Outcome::ServiceFailure(ServiceFailureOutcome::Parked))
                );
                assert_eq!(saved.checkpoint().state, JobState::NeedsReview);
            }
        }
        assert_eq!(
            services.calls.len(),
            nntune_ingest::jobs::MAX_ATTEMPTS as usize
        );
        let before = authority_row(&app).await;
        assert_eq!(
            store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services).await,
            Ok(Outcome::Inactive)
        );
        assert_eq!(
            services.calls.len(),
            nntune_ingest::jobs::MAX_ATTEMPTS as usize
        );
        assert_eq!(authority_row(&app).await, before);
        assert_eq!(counts(&app).await, (0, 0, 0));
    }
}

#[tokio::test]
async fn delivery_stage_invalid_results_and_cancelled_reads_do_not_spend_budget() {
    for state in [JobState::Matching, JobState::Probing] {
        let (mut app, original, _) = seeded().await;
        let delivery = hint(&app, &original).await;
        let mut services = Services::default();
        advance(&mut app, &original, &delivery, &mut services, state).await;
        let before = authority_row(&app).await;
        services.fail = Some(ServiceError::Invalid);
        assert_eq!(
            store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services).await,
            Err(StoreError::Invalid)
        );
        assert_eq!(authority_row(&app).await, before);
        services.fail = None;
        services.pending = true;
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services)
        )
        .await
        .is_err());
        assert_eq!(authority_row(&app).await, before);
        services.pending = false;
        services.malformed = true;
        if state == JobState::Probing {
            assert_eq!(
                store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services)
                    .await,
                Err(StoreError::Invalid)
            );
            assert_eq!(authority_row(&app).await, before);
        }
    }
    let (mut app, original, _) = seeded().await;
    let delivery = hint(&app, &original).await;
    let mut services = Services {
        no_match: true,
        ..Services::default()
    };
    advance(
        &mut app,
        &original,
        &delivery,
        &mut services,
        JobState::Matching,
    )
    .await;
    assert_eq!(
        store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services).await,
        Ok(Outcome::Advanced(StageOutcome::Parked))
    );
    assert_eq!(services.calls, ["match"]);
}

#[tokio::test]
async fn delivery_stage_sql_errors_never_become_service_retries_and_reload_refuses_bad_authority() {
    for transient in [false, true] {
        let (mut app, original, _) = seeded().await;
        let delivery = hint(&app, &original).await;
        let mut services = Services::default();
        advance(
            &mut app,
            &original,
            &delivery,
            &mut services,
            JobState::Matching,
        )
        .await;
        if transient {
            services.fail = Some(ServiceError::Transient {
                retry_after_secs: None,
            });
        }
        for (function, error) in [
            ("CREATE OR REPLACE FUNCTION pg_temp.refuse_delivery() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'refused'; END $$", StoreError::Database),
            ("CREATE OR REPLACE FUNCTION pg_temp.refuse_delivery() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$", StoreError::Stale),
        ] {
            app.batch_execute(function).await.unwrap();
            app.batch_execute("CREATE TRIGGER refuse_delivery BEFORE UPDATE ON ingest_jobs FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_delivery()").await.unwrap();
            let before = authority_row(&app).await;
            assert_eq!(
                store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services).await,
                Err(error)
            );
            assert_eq!(authority_row(&app).await, before);
            app.batch_execute("DROP TRIGGER refuse_delivery ON ingest_jobs").await.unwrap();
        }
        services.fail = None;
        services.calls.clear();
        let before = authority_row(&app).await;
        app.batch_execute("UPDATE ingest_originals SET nzb_bytes = nzb_bytes + 1")
            .await
            .unwrap();
        assert_eq!(
            store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services).await,
            Err(StoreError::Stale)
        );
        assert!(services.calls.is_empty());
        assert_eq!(authority_row(&app).await, before);
        app.batch_execute("UPDATE ingest_originals SET nzb_bytes = nzb_bytes - 1; UPDATE ingest_jobs SET admin_checkpoint = NULL").await.unwrap();
        assert_eq!(
            store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services).await,
            Err(StoreError::Invalid)
        );
        assert!(services.calls.is_empty());
        assert_eq!(counts(&app).await, (0, 0, 0));
    }
}
