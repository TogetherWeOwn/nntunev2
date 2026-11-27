//! Public counter and structured diagnostic projections for the off-route admin matcher.

use nntune_logging::{
    fields_from_json, parse_build_sha, resolve_trace_id, Level, LogError, LogRecord,
    Worker as LogWorker,
};
use serde::Serialize;
use serde_json::Value;

use super::StageOutcome;
use crate::{catalogue_publication::PublicationError, matching::admin_publish_match::NeedsReview};

pub const MATCHER_COUNTER_NAME: &str = "nntune_ingest_admin_match_attempts_total";
pub const MATCHER_DIAGNOSTIC_EVENT: &str = "ingest.admin_match.attempted";

#[derive(Serialize)]
pub struct MatcherCounter {
    name: &'static str,
    labels: Labels,
    increment: u8,
}

#[derive(Serialize)]
struct Labels {
    outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<NeedsReview>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Staged,
    NeedsReview,
    Refused,
    Indeterminate,
}

impl Outcome {
    fn diagnostic_level(self) -> Level {
        match self {
            Self::Staged => Level::Info,
            Self::NeedsReview | Self::Refused => Level::Warn,
            Self::Indeterminate => Level::Error,
        }
    }
}

fn classify(result: &Result<StageOutcome, PublicationError>) -> Labels {
    let (outcome, reason) = match result {
        Ok(StageOutcome::Staged(_)) => (Outcome::Staged, None),
        Ok(StageOutcome::NeedsReview(reason)) => (Outcome::NeedsReview, Some(*reason)),
        Err(PublicationError::Refused) => (Outcome::Refused, None),
        Err(PublicationError::Indeterminate) => (Outcome::Indeterminate, None),
    };
    Labels { outcome, reason }
}

/// Project one completed attempt onto a bounded set of public counter labels.
/// `staged` counts successful staging attempts, including replays, before
/// outer commit. The projection does not send the counter to a collector.
pub fn matcher_counter(result: &Result<StageOutcome, PublicationError>) -> MatcherCounter {
    MatcherCounter {
        name: MATCHER_COUNTER_NAME,
        labels: classify(result),
        increment: 1,
    }
}

/// Project one completed attempt onto a bounded, redacted structured log
/// record (`docs/logging-standard.md`). `trace_id` and `build_sha` are
/// normalized through `nntune_logging`, discarding invalid or hostile inputs.
/// The projection does not write to the console or emit to a live sink.
pub fn matcher_diagnostic(
    result: &Result<StageOutcome, PublicationError>,
    trace_id: Option<&str>,
    build_sha: Option<&str>,
    ts_millis: u64,
) -> Result<LogRecord, LogError> {
    let labels = classify(result);
    let mut map = match serde_json::to_value(&labels) {
        Ok(Value::Object(map)) => map,
        _ => return Err(LogError::FieldsNotAnObject),
    };
    map.insert(
        "build_sha".to_string(),
        Value::String(parse_build_sha(build_sha)),
    );
    let fields = fields_from_json(Value::Object(map))?;
    let trace_id = resolve_trace_id(trace_id);
    LogRecord::new(
        LogWorker::Ingest,
        labels.outcome.diagnostic_level(),
        &trace_id,
        MATCHER_DIAGNOSTIC_EVENT,
        fields,
        ts_millis,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matcher_counter_has_one_fixed_series_per_outcome_and_review_reason() {
        for (result, outcome) in [
            (
                Ok(StageOutcome::Staged(vec!["synthetic-track-id-a".into()])),
                "staged",
            ),
            (
                Ok(StageOutcome::Staged(vec!["synthetic-track-id-b".into()])),
                "staged",
            ),
            (Err(PublicationError::Refused), "refused"),
            (Err(PublicationError::Indeterminate), "indeterminate"),
        ] {
            assert_eq!(
                serde_json::to_value(matcher_counter(&result)).unwrap(),
                json!({
                    "name": "nntune_ingest_admin_match_attempts_total",
                    "labels": {"outcome": outcome},
                    "increment": 1,
                })
            );
        }

        for (reason, label) in [
            (NeedsReview::InvalidQuery, "invalid_query"),
            (NeedsReview::SearchEvidence, "search_evidence"),
            (NeedsReview::ReleaseUnresolved, "release_unresolved"),
            (NeedsReview::RecordingEvidence, "recording_evidence"),
            (NeedsReview::CountMismatch, "count_mismatch"),
            (NeedsReview::FileNumbering, "file_numbering"),
            (NeedsReview::PositionMismatch, "position_mismatch"),
        ] {
            let counter = matcher_counter(&Ok(StageOutcome::NeedsReview(reason)));
            assert_eq!(
                serde_json::to_value(counter).unwrap(),
                json!({
                    "name": "nntune_ingest_admin_match_attempts_total",
                    "labels": {"outcome": "needs_review", "reason": label},
                    "increment": 1,
                })
            );
        }
    }
}
