//! Analytics Engine metering: one event per stage transition.
//!
//! Billing truth lives with the metering card; these events are the ingest
//! pipeline's usage signal (stage entered, outcome, attempt). The worker
//! writes them through the `INGEST_METER` dataset binding (see
//! `wrangler.toml.example`; the runtime write is `EdgeMeterSink` in
//! `lib.rs`) while this module owns the event shape.

use serde::{Deserialize, Serialize};

use crate::queue::{QueueMsg, WORKFLOW_STEPS};

/// Analytics Engine binding name: must match `[analytics_engine_datasets]`
/// in `wrangler.toml.example`.
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
) -> Result<MeterEvent, &'static str> {
    if !WORKFLOW_STEPS.contains(&stage) {
        return Err("stage must be a pipeline workflow step");
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

/// How a meter edge-write failed. A dropped event must never look accepted:
/// retryable failures map to HTTP 503 + `Retry-After` at the route, while an
/// unknown stage is a caller bug and maps to 400.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeterWriteError {
    /// `stage` was not in [`WORKFLOW_STEPS`]. Returned before any write is
    /// attempted, so the sink is never touched.
    UnknownStage(&'static str),
    /// The binding/transport failed before any dataset status existed
    /// (missing `INGEST_METER` binding, connection refused, timeout).
    Transport(String),
    /// The dataset endpoint answered with a non-2xx HTTP-equivalent status.
    Rejected { status: u16 },
}

impl MeterWriteError {
    /// Transport and rejection failures are retryable (never a silent drop);
    /// an unknown stage is a caller bug, not a retry.
    pub fn is_retryable(&self) -> bool {
        match self {
            MeterWriteError::UnknownStage(_) => false,
            MeterWriteError::Transport(_) | MeterWriteError::Rejected { .. } => true,
        }
    }

    /// HTTP status the route answers with: retryable failures surface 503 +
    /// `Retry-After` (never 202-and-dropped); unknown stages surface 400.
    pub fn http_status(&self) -> u16 {
        match self {
            MeterWriteError::UnknownStage(_) => 400,
            MeterWriteError::Transport(_) | MeterWriteError::Rejected { .. } => 503,
        }
    }
}

impl core::fmt::Display for MeterWriteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MeterWriteError::UnknownStage(stage) => write!(f, "unknown meter stage: {stage}"),
            MeterWriteError::Transport(detail) => {
                write!(f, "meter write transport failed: {detail}")
            }
            MeterWriteError::Rejected { status } => {
                write!(f, "meter write rejected with status {status}")
            }
        }
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
    fn send(&mut self, dataset: &str, body: serde_json::Value) -> Result<u16, String>;
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
    let event = match meter_event(msg, stage, outcome) {
        Ok(event) => event,
        Err(message) => return Err(MeterWriteError::UnknownStage(message)),
    };
    let body = meter_write_body(&event);
    match sink.send(METER_DATASET, body) {
        Ok(status) if (200..300).contains(&status) => Ok(()),
        Ok(status) => Err(MeterWriteError::Rejected { status }),
        Err(detail) => Err(MeterWriteError::Transport(detail)),
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

    #[test]
    fn dataset_names_match_wrangler_example() {
        assert_eq!(METER_DATASET_BINDING, "INGEST_METER");
        assert_eq!(METER_DATASET, "nntune_ingest_meter");
    }
}
