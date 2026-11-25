//! Public counter projection for the off-route admin matcher.

use serde::Serialize;

use super::StageOutcome;
use crate::{catalogue_publication::PublicationError, matching::admin_publish_match::NeedsReview};

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

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Staged,
    NeedsReview,
    Refused,
    Indeterminate,
}

/// Project one completed attempt onto a bounded set of public counter labels.
/// `staged` counts successful staging attempts, including replays, before
/// outer commit. The projection does not send the counter to a collector.
pub fn matcher_counter(result: &Result<StageOutcome, PublicationError>) -> MatcherCounter {
    let (outcome, reason) = match result {
        Ok(StageOutcome::Staged(_)) => (Outcome::Staged, None),
        Ok(StageOutcome::NeedsReview(reason)) => (Outcome::NeedsReview, Some(*reason)),
        Err(PublicationError::Refused) => (Outcome::Refused, None),
        Err(PublicationError::Indeterminate) => (Outcome::Indeterminate, None),
    };
    MatcherCounter {
        name: "nntune_ingest_admin_match_attempts_total",
        labels: Labels { outcome, reason },
        increment: 1,
    }
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
