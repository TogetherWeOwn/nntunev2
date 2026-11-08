//! Post-commit continuation only. No queue event registration or recovery scanner.
use super::*;

impl DeliverySender for worker::Queue {
    type Error = worker::Error;

    async fn send(&mut self, encoded: String, delay_secs: u32) -> Result<(), Self::Error> {
        // JSON string body: the eventual consumer must decode that string with
        // Delivery::decode, not deserialize it as the legacy QueueMsg.
        worker::Queue::send(
            self,
            worker::MessageBuilder::new(encoded)
                .delay_seconds(delay_secs)
                .build(),
        )
        .await
    }
}

/// SQL has committed even if continuation is Err(Send). The eventual broker
/// caller must inspect this field before acknowledging its incoming message.
/// Retry continue_delivery with this SAME outcome/delivery; do not rerun a
/// provider stage just because its continuation send was lost or uncertain.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryCompletion {
    pub outcome: DeliveryStageOutcome,
    pub continuation: Result<bool, DeliveryError>,
}

/// Schedule only after the caller has a committed run_delivery_stage outcome.
/// Delays come from the existing bounded service-failure policy, never a queue
/// body. Reload before every send/recovery and retain the captured generation.
/// The send runs outside a transaction; concurrent controls may still invalidate
/// an accepted hint, which the receiving authority reload will refuse.
///
/// This is not durable retry-due storage: recovering a lost scheduling outcome
/// after a process crash still needs the eventual broker/recovery protocol.
/// No timer, live consumer, new binding or publication is enabled here.
pub async fn continue_delivery(
    client: &Client,
    original: &StageOriginal<'_>,
    delivery: &Delivery,
    outcome: DeliveryStageOutcome,
    sender: &mut impl DeliverySender,
) -> Result<bool, DeliveryError> {
    let delay_secs = match outcome {
        DeliveryStageOutcome::Advanced(StageOutcome::Continue) => 0,
        DeliveryStageOutcome::ServiceFailure(ServiceFailureOutcome::Retry { delay_secs })
            if (1..=crate::retry::MAX_HINTED_RETRY_DELAY_SECS).contains(&delay_secs) =>
        {
            delay_secs
        }
        DeliveryStageOutcome::ServiceFailure(ServiceFailureOutcome::Retry { .. }) => {
            return Err(DeliveryError::Authority(StoreError::Invalid));
        }
        _ => return Ok(false),
    };
    let Some(snapshot) = load_delivery(client, original, delivery)
        .await
        .map_err(DeliveryError::Authority)?
    else {
        return Ok(false);
    };
    // Writing is handed to the separately gated publication path. Never create
    // an endless wake loop if another worker already completed the last probe.
    if snapshot.checkpoint().state == JobState::Writing {
        return Ok(false);
    }
    let encoded = delivery.encode().map_err(DeliveryError::Authority)?;
    sender
        .send(encoded, delay_secs)
        .await
        .map_err(|_| DeliveryError::Send)?;
    Ok(true)
}

/// Execute one stage, commit, then request its continuation. Store errors,
/// including uncertain COMMIT, return before any send. A continuation failure
/// does not undo the committed stage or consume another service attempt.
/// Caller authenticates/opens the original and bounds this entire operation.
pub async fn run_delivery_and_enqueue(
    client: &mut Client,
    original: &StageOriginal<'_>,
    delivery: &Delivery,
    services: &mut impl StageServices,
    sender: &mut impl DeliverySender,
) -> Result<DeliveryCompletion, StoreError> {
    let outcome = run_delivery_stage(client, original, delivery, services).await?;
    let continuation = continue_delivery(client, original, delivery, outcome, sender).await;
    Ok(DeliveryCompletion {
        outcome,
        continuation,
    })
}

/// The control and its audit committed even if enqueue failed. Recover with
/// enqueue_delivery using this snapshot's generation, never by repeating the
/// control/reconciliation or adopting the latest generation. No Debug: the
/// committed snapshot retains the same safe-checkpoint boundary as Snapshot.
#[must_use]
pub struct ControlCompletion {
    pub snapshot: Snapshot,
    pub continuation: Result<bool, DeliveryError>,
}

/// Compose the canonical actor-fenced control with its post-commit wake hint.
/// Caller still authenticates/opens the original, gates archive resume and bounds
/// the whole operation. Database errors (including uncertain COMMIT) never send;
/// they require independent outcome resolution before scheduling. Cancellation
/// or a process crash can lose the returned outcome: this is not durable recovery.
pub async fn apply_control_and_enqueue(
    client: &mut Client,
    actor: ControlActor<'_>,
    original: &StageOriginal<'_>,
    expected: &Snapshot,
    control: Control<'_>,
    sender: &mut impl DeliverySender,
) -> Result<ControlCompletion, StoreError> {
    let snapshot = apply_control(client, actor, original, expected, control).await?;
    let continuation = enqueue_delivery(client, original, snapshot.generation(), sender).await;
    Ok(ControlCompletion {
        snapshot,
        continuation,
    })
}

/// Explicit SQL-NULL reconciliation only, with the same caller duties and
/// uncertain-outcome rules as apply_control_and_enqueue. No automatic repair,
/// live route, scanner or capability enablement. The fresh generation comes
/// exclusively from the committed reconciliation, not a subsequent latest read.
pub async fn reconcile_null_and_enqueue(
    client: &mut Client,
    actor: ControlActor<'_>,
    original: &StageOriginal<'_>,
    expected_status: &str,
    expected_revision: i64,
    sender: &mut impl DeliverySender,
) -> Result<ControlCompletion, StoreError> {
    let snapshot =
        reconcile_null(client, actor, original, expected_status, expected_revision).await?;
    let continuation = enqueue_delivery(client, original, snapshot.generation(), sender).await;
    Ok(ControlCompletion {
        snapshot,
        continuation,
    })
}
