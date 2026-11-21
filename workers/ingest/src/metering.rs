//! Analytics Engine metering: one event per stage transition.
//!
//! Billing truth lives with the metering card; these events are the ingest
//! pipeline's usage signal (stage entered, outcome, attempt). The worker
//! writes them through the `INGEST_METER` dataset binding (see
//! `wrangler.toml`; the runtime write is `EdgeMeterSink` in
//! `lib.rs`) while this module owns the event shape.

use serde::{Deserialize, Serialize};

use crate::queue::{QueueMsg, WORKFLOW_STEPS};

/// Analytics Engine binding name: must match `[analytics_engine_datasets]`
/// in `wrangler.toml`.
pub const METER_DATASET_BINDING: &str = "INGEST_METER";
/// Analytics Engine dataset receiving ingest usage events.
pub const METER_DATASET: &str = "nntune_ingest_meter";

/// What happened when the job entered the stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeterOutcome {
    Ok,
    Retry,
    Failed,
}

/// One usage event: which job, which stage, what happened, which attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeterEvent {
    pub job_id: String,
    pub user_id: String,
    pub stage: String,
    pub outcome: MeterOutcome,
    pub attempt: u32,
}

/// Build the event for a stage transition. `stage` must be one of
/// [`WORKFLOW_STEPS`] so metering can never invent a stage the pipeline
/// does not have.
pub fn meter_event(
    msg: &QueueMsg,
    stage: &str,
    outcome: MeterOutcome,
) -> Result<MeterEvent, MeterWriteError> {
    if !WORKFLOW_STEPS.contains(&stage) {
        return Err(MeterWriteError::UnknownStage);
    }
    Ok(MeterEvent {
        job_id: msg.job_id.clone(),
        user_id: msg.user_id.clone(),
        stage: stage.to_string(),
        outcome,
        attempt: msg.attempt,
    })
}

/// Exact wire body for one meter event.
///
/// The body round-trips back to the same [`MeterEvent`] and carries exactly
/// the shape [`meter_event`] builds: this module owns the event shape, and
/// every writer (edge binding call, test fake) sends this body unchanged.
pub fn meter_write_body(event: &MeterEvent) -> serde_json::Value {
    serde_json::to_value(event).expect("MeterEvent serializes")
}

/// Failures before a dataset status exists. Sinks cannot return upstream text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterTransportReason {
    BindingMissing,
    WriteFailed,
}

impl MeterTransportReason {
    /// Classify an upstream failure without inspecting or formatting it.
    pub(crate) fn redact<T, E>(self, result: Result<T, E>) -> Result<T, Self> {
        result.map_err(|_| self)
    }
}

/// How metered admission failed. Retryable failures map to 503 + `Retry-After`;
/// an unknown stage is a caller bug and maps to 400.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeterWriteError {
    /// `stage` was not in [`WORKFLOW_STEPS`]; no write was attempted.
    UnknownStage,
    Transport(MeterTransportReason),
    /// The dataset endpoint answered with a non-2xx HTTP-equivalent status.
    Rejected {
        status: u16,
    },
    AdmissionLedgerUnavailable,
}

impl MeterWriteError {
    /// Fixed diagnostic code; neither upstream errors nor stage input appear here.
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::UnknownStage => "meter_unknown_stage",
            Self::Transport(MeterTransportReason::BindingMissing) => "meter_binding_missing",
            Self::Transport(MeterTransportReason::WriteFailed) => "meter_write_failed",
            Self::Rejected { .. } => "meter_write_rejected",
            Self::AdmissionLedgerUnavailable => "admission_ledger_unavailable",
        }
    }

    pub fn is_retryable(&self) -> bool {
        !matches!(self, Self::UnknownStage)
    }

    pub fn http_status(&self) -> u16 {
        if self.is_retryable() {
            503
        } else {
            400
        }
    }
}

impl core::fmt::Display for MeterWriteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.reason_code())
    }
}

impl std::error::Error for MeterWriteError {}

/// One meter-event delivery to the named dataset.
///
/// The `u16` is the HTTP-equivalent status of the dataset write and `Err`
/// means the binding/transport failed before any status existed. At the edge
/// this is the `INGEST_METER` binding call; a native binding success maps to
/// 2xx.
pub trait MeterSink {
    fn send(&mut self, dataset: &str, body: serde_json::Value)
        -> Result<u16, MeterTransportReason>;
}

/// Emit one stage-transition meter event through the sink.
///
/// Builds the event with [`meter_event`] (an unknown stage returns
/// [`MeterWriteError::UnknownStage`] BEFORE touching the sink), delivers
/// exactly one body shaped like the [`MeterEvent`], and classifies the
/// result: 2xx is `Ok`, anything else (transport `Err` or non-2xx) is a
/// retryable `Err`. Never `Ok` on failure: a dropped event must never look
/// accepted.
pub fn emit_meter<S: MeterSink>(
    sink: &mut S,
    msg: &QueueMsg,
    stage: &str,
    outcome: MeterOutcome,
) -> Result<(), MeterWriteError> {
    let event = meter_event(msg, stage, outcome)?;
    let status = sink
        .send(METER_DATASET, meter_write_body(&event))
        .map_err(MeterWriteError::Transport)?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(MeterWriteError::Rejected { status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::build_upload_dispatch;
    use crate::fixtures::SMALL_NZB;

    #[test]
    fn meter_event_roundtrips_with_stage_and_outcome() {
        let msg = build_upload_dispatch("user_01", SMALL_NZB).unwrap();
        let ev = meter_event(&msg, "probe", MeterOutcome::Ok).unwrap();
        let back: MeterEvent = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(ev, back);
        assert_eq!(ev.stage, "probe");
        assert_eq!(ev.attempt, 1);
    }

    #[test]
    fn meter_event_rejects_unknown_stages() {
        let msg = build_upload_dispatch("user_01", SMALL_NZB).unwrap();
        assert!(meter_event(&msg, "teleport", MeterOutcome::Ok).is_err());
        // Every workflow step is a legal stage, including the repair branch.
        for step in crate::queue::WORKFLOW_STEPS {
            assert!(meter_event(&msg, step, MeterOutcome::Retry).is_ok());
        }
    }

    struct RecordingSink {
        status: u16,
        writes: Vec<(String, serde_json::Value)>,
    }

    impl MeterSink for RecordingSink {
        fn send(
            &mut self,
            dataset: &str,
            body: serde_json::Value,
        ) -> Result<u16, MeterTransportReason> {
            self.writes.push((dataset.to_owned(), body));
            Ok(self.status)
        }
    }

    fn synthetic_message() -> QueueMsg {
        let mut msg = crate::dispatch::build_search_dispatch(
            "user_01",
            "canary-9f1c",
            "vault_idx_01-ab",
            &[3000],
            10,
            0,
        )
        .unwrap();
        msg.attempt = 3;
        msg
    }

    #[test]
    fn existing_stages_emit_one_exact_event_for_each_supported_outcome() {
        let msg = synthetic_message();
        // This exercises the meter contract, not which outcomes each caller emits.
        for stage in WORKFLOW_STEPS {
            for (outcome, wire_outcome) in [
                (MeterOutcome::Ok, "ok"),
                (MeterOutcome::Retry, "retry"),
                (MeterOutcome::Failed, "failed"),
            ] {
                let mut sink = RecordingSink {
                    status: 204,
                    writes: Vec::new(),
                };
                assert_eq!(emit_meter(&mut sink, &msg, stage, outcome), Ok(()));
                assert_eq!(
                    sink.writes,
                    [(
                        METER_DATASET.to_owned(),
                        serde_json::json!({
                            "job_id": msg.job_id,
                            "user_id": msg.user_id,
                            "stage": stage,
                            "outcome": wire_outcome,
                            "attempt": 3,
                        })
                    )],
                    "{stage}/{wire_outcome} must emit one write with only the meter fields",
                );
            }
        }
    }

    #[test]
    fn meter_accepts_only_2xx_at_status_boundaries() {
        let msg = synthetic_message();
        for (status, expected) in [
            (199, Err(MeterWriteError::Rejected { status: 199 })),
            (200, Ok(())),
            (299, Ok(())),
            (300, Err(MeterWriteError::Rejected { status: 300 })),
        ] {
            let mut sink = RecordingSink {
                status,
                writes: Vec::new(),
            };
            assert_eq!(
                emit_meter(&mut sink, &msg, "probe", MeterOutcome::Ok),
                expected
            );
            assert_eq!(sink.writes.len(), 1, "no inline retry for {status}");
        }
    }

    #[test]
    fn dataset_names_match_wrangler_example() {
        assert_eq!(METER_DATASET_BINDING, "INGEST_METER");
        assert_eq!(METER_DATASET, "nntune_ingest_meter");
    }
}
