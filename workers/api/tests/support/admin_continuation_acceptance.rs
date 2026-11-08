//! Post-commit scheduling against the real SQL authority and a scripted sender.
//! Not live broker delivery, durable retry-due recovery or ordered schema proof.
use super::*;
use store::DeliveryError;

#[tokio::test]
async fn continuation_sends_only_committed_immediate_progress_and_stops_at_writing() {
    let (mut app, original, _) = seeded().await;
    let delivery = hint(&app, &original).await;
    let mut services = Services::default();
    let mut queue = DeliveryQueue::default();
    for state in [
        JobState::Parsing,
        JobState::Matching,
        JobState::Probing,
        JobState::Writing,
    ] {
        let result = store::run_delivery_and_enqueue(
            &mut app,
            &input(&original),
            &delivery,
            &mut services,
            &mut queue,
        )
        .await
        .unwrap();
        let saved = store::load(&app, &input(&original), 1).await.unwrap();
        assert_eq!(saved.checkpoint().state, state);
        assert_eq!(result.continuation, Ok(state != JobState::Writing));
        assert!(matches!(
            result.outcome,
            Outcome::Advanced(StageOutcome::Continue | StageOutcome::ReadyToPublish)
        ));
    }
    assert_eq!(services.calls, ["match", "probe"]);
    assert_eq!(queue.calls, 3);
    assert_eq!(queue.delays, [0, 0, 0]);
    for encoded in &queue.messages {
        let sent = store::Delivery::decode(encoded).unwrap();
        assert!(sent == delivery);
    }
    let before = authority_row(&app).await;
    let result = store::run_delivery_and_enqueue(
        &mut app,
        &input(&original),
        &delivery,
        &mut services,
        &mut queue,
    )
    .await
    .unwrap();
    assert_eq!(
        result.outcome,
        Outcome::Advanced(StageOutcome::ReadyToPublish)
    );
    assert_eq!(result.continuation, Ok(false));
    assert_eq!(queue.calls, 3);
    assert_eq!(authority_row(&app).await, before);
    assert_eq!(counts(&app).await, (0, 0, 0));
}

#[tokio::test]
async fn continuation_retries_lost_or_uncertain_sends_without_rerunning_or_spending_attempts() {
    for transient in [false, true] {
        for uncertain in [false, true] {
            let (mut app, original, _) = seeded().await;
            let delivery = hint(&app, &original).await;
            let mut services = Services::default();
            let prior = advance(
                &mut app,
                &original,
                &delivery,
                &mut services,
                JobState::Matching,
            )
            .await;
            services.calls.clear();
            if transient {
                services.fail = Some(ServiceError::Transient {
                    retry_after_secs: Some(u32::MAX),
                });
            }
            let mut queue = DeliveryQueue {
                reject: !uncertain,
                uncertain,
                ..Default::default()
            };
            let result = store::run_delivery_and_enqueue(
                &mut app,
                &input(&original),
                &delivery,
                &mut services,
                &mut queue,
            )
            .await
            .unwrap();
            assert_eq!(result.continuation, Err(DeliveryError::Send));
            let saved = store::load(&app, &input(&original), 1).await.unwrap();
            assert_eq!(saved.revision(), prior.revision() + 1);
            assert_eq!(saved.generation(), prior.generation());
            assert_eq!(saved.checkpoint().attempt, if transient { 2 } else { 1 });
            let delay = if transient { 300 } else { 0 };
            let expected_outcome = if transient {
                Outcome::ServiceFailure(ServiceFailureOutcome::Retry { delay_secs: delay })
            } else {
                Outcome::Advanced(StageOutcome::Continue)
            };
            assert_eq!(result.outcome, expected_outcome);
            assert_eq!(queue.delays, [delay]);
            assert_eq!(queue.messages.len(), usize::from(uncertain));
            let before = authority_row(&app).await;
            queue.reject = false;
            queue.uncertain = false;
            for _ in 0..2 {
                assert_eq!(
                    store::continue_delivery(
                        &app,
                        &input(&original),
                        &delivery,
                        result.outcome,
                        &mut queue,
                    )
                    .await,
                    Ok(true)
                );
                assert_eq!(authority_row(&app).await, before);
            }
            assert_eq!(services.calls, ["match"]);
            assert_eq!(queue.delays, [delay, delay, delay]);
            assert_eq!(counts(&app).await, (0, 0, 0));
        }
    }
}

#[tokio::test]
async fn continuation_cancelled_send_reloads_and_refuses_manual_generation_without_adoption() {
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
    services.fail = Some(ServiceError::Transient {
        retry_after_secs: None,
    });
    let outcome = store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services)
        .await
        .unwrap();
    let before = authority_row(&app).await;
    let mut queue = DeliveryQueue {
        pending: true,
        ..Default::default()
    };
    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(50),
        store::continue_delivery(&app, &input(&original), &delivery, outcome, &mut queue),
    )
    .await
    .is_err());
    assert_eq!(queue.calls, 1);
    assert_eq!(authority_row(&app).await, before);
    queue.pending = false;
    assert_eq!(
        store::continue_delivery(&app, &input(&original), &delivery, outcome, &mut queue).await,
        Ok(true)
    );
    assert_eq!(authority_row(&app).await, before);
    let saved = store::load(&app, &input(&original), 1).await.unwrap();
    let rejected = store::apply_control(
        &mut app,
        actor(),
        &input(&original),
        &saved,
        Control::Reject,
    )
    .await
    .unwrap();
    let winner = authority_row(&app).await;
    assert_eq!(
        store::continue_delivery(&app, &input(&original), &delivery, outcome, &mut queue).await,
        Err(DeliveryError::Authority(StoreError::Stale))
    );
    assert!(rejected.generation() > delivery.generation());
    assert_eq!(queue.calls, 2);
    assert_eq!(authority_row(&app).await, winner);
    assert_eq!(counts(&app).await, (0, 0, 0));
}

#[tokio::test]
async fn continuation_refuses_store_failures_invalid_delays_and_inactive_work_before_sending() {
    let (mut app, original, _) = seeded().await;
    let delivery = hint(&app, &original).await;
    let mut services = Services::default();
    let mut queue = DeliveryQueue::default();
    for (function, error) in [
        ("CREATE OR REPLACE FUNCTION pg_temp.refuse_continuation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'refused'; END $$", StoreError::Database),
        ("CREATE OR REPLACE FUNCTION pg_temp.refuse_continuation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$", StoreError::Stale),
    ] {
        app.batch_execute(function).await.unwrap();
        app.batch_execute("CREATE TRIGGER refuse_continuation BEFORE UPDATE ON ingest_jobs FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_continuation()").await.unwrap();
        let before = authority_row(&app).await;
        assert_eq!(
            store::run_delivery_and_enqueue(
                &mut app, &input(&original), &delivery, &mut services, &mut queue,
            ).await,
            Err(error)
        );
        assert_eq!(authority_row(&app).await, before);
        assert_eq!(queue.calls, 0);
        app.batch_execute("DROP TRIGGER refuse_continuation ON ingest_jobs").await.unwrap();
    }
    for delay_secs in [0, 301, u32::MAX] {
        assert_eq!(
            store::continue_delivery(
                &app,
                &input(&original),
                &delivery,
                Outcome::ServiceFailure(ServiceFailureOutcome::Retry { delay_secs }),
                &mut queue,
            )
            .await,
            Err(DeliveryError::Authority(StoreError::Invalid))
        );
    }
    services.no_match = true;
    advance(
        &mut app,
        &original,
        &delivery,
        &mut services,
        JobState::Matching,
    )
    .await;
    let parked = store::run_delivery_and_enqueue(
        &mut app,
        &input(&original),
        &delivery,
        &mut services,
        &mut queue,
    )
    .await
    .unwrap();
    assert_eq!(parked.outcome, Outcome::Advanced(StageOutcome::Parked));
    assert_eq!(parked.continuation, Ok(false));
    let inactive = store::run_delivery_and_enqueue(
        &mut app,
        &input(&original),
        &delivery,
        &mut services,
        &mut queue,
    )
    .await
    .unwrap();
    assert_eq!(inactive.outcome, Outcome::Inactive);
    assert_eq!(inactive.continuation, Ok(false));
    assert_eq!(queue.calls, 0);
    assert!(queue.delays.is_empty());
    assert_eq!(counts(&app).await, (0, 0, 0));
}

#[tokio::test]
async fn continuation_exhaustion_parks_without_send_and_recovery_checks_original_linkage() {
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
    services.fail = Some(ServiceError::Transient {
        retry_after_secs: Some(1),
    });
    let mut queue = DeliveryQueue::default();
    let retry = store::run_delivery_stage(&mut app, &input(&original), &delivery, &mut services)
        .await
        .unwrap();
    let before = authority_row(&app).await;
    app.batch_execute("UPDATE ingest_originals SET nzb_bytes = nzb_bytes + 1")
        .await
        .unwrap();
    assert_eq!(
        store::continue_delivery(&app, &input(&original), &delivery, retry, &mut queue).await,
        Err(DeliveryError::Authority(StoreError::Stale))
    );
    assert_eq!(queue.calls, 0);
    assert_eq!(authority_row(&app).await, before);
    app.batch_execute("UPDATE ingest_originals SET nzb_bytes = nzb_bytes - 1")
        .await
        .unwrap();
    for spent in 2..=nntune_ingest::jobs::MAX_ATTEMPTS {
        let result = store::run_delivery_and_enqueue(
            &mut app,
            &input(&original),
            &delivery,
            &mut services,
            &mut queue,
        )
        .await
        .unwrap();
        assert_eq!(
            result.continuation,
            Ok(spent < nntune_ingest::jobs::MAX_ATTEMPTS)
        );
        if spent == nntune_ingest::jobs::MAX_ATTEMPTS {
            assert_eq!(
                result.outcome,
                Outcome::ServiceFailure(ServiceFailureOutcome::Parked)
            );
        }
    }
    let saved = store::load(&app, &input(&original), 1).await.unwrap();
    assert_eq!(saved.checkpoint().state, JobState::NeedsReview);
    let calls = queue.calls;
    assert_eq!(calls, (nntune_ingest::jobs::MAX_ATTEMPTS - 2) as usize);
    assert!(queue.delays.iter().all(|delay| (1..=300).contains(delay)));
    assert_eq!(
        store::continue_delivery(&app, &input(&original), &delivery, retry, &mut queue).await,
        Ok(false)
    );
    assert_eq!(queue.calls, calls);
    assert_eq!(counts(&app).await, (0, 0, 0));
}
