//! User-scoped ingest job state machine: idempotent and resumable.
//!
//! A job moves `Queued -> Searching|Parsing -> Probing -> Verifying
//! -> (Repairing -> Verifying)? -> Matching -> Writing -> Done`.
//! Any step may move to `Failed` with a reason; `Done`/`Failed` are
//! terminal. Workers persist the [`JobCheckpoint`] after every step, so a
//! crashed or evicted run resumes from the last checkpoint instead of
//! restarting (Queue redelivery + Workflow step replay both converge here).

use serde::{Deserialize, Serialize};

/// How many times a single job may attempt a step before it is `Failed`.
pub const MAX_ATTEMPTS: u32 = 5;

/// All ingest stages. Serialized snake_case for Queue/Workflow payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Searching,
    Parsing,
    Probing,
    Verifying,
    Repairing,
    Matching,
    Writing,
    Done,
    Failed,
}

/// How the job's NZB entered the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobSource {
    IndexerSearch,
    NzbUpload,
}

/// Durable resume point written after every completed step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobCheckpoint {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub source: JobSource,
    pub state: JobState,
    pub attempt: u32,
    pub failure_reason: Option<String>,
}

/// Advance one step on success. `repair_needed` is only read in `Verifying`.
pub fn advance(state: JobState, source: JobSource, repair_needed: bool) -> JobState {
    match state {
        JobState::Queued => match source {
            JobSource::IndexerSearch => JobState::Searching,
            JobSource::NzbUpload => JobState::Parsing,
        },
        JobState::Searching => JobState::Parsing,
        JobState::Parsing => JobState::Probing,
        JobState::Probing => JobState::Verifying,
        JobState::Verifying => {
            if repair_needed {
                JobState::Repairing
            } else {
                JobState::Matching
            }
        }
        JobState::Repairing => JobState::Verifying,
        JobState::Matching => JobState::Writing,
        JobState::Writing => JobState::Done,
        JobState::Done => JobState::Done,
        JobState::Failed => JobState::Failed,
    }
}

/// Move to `Failed`, unless already terminal (terminal states stick).
pub fn fail(state: JobState) -> JobState {
    match state {
        JobState::Done | JobState::Failed => state,
        _ => JobState::Failed,
    }
}

/// Terminal states need no further work and free the user's job slot.
pub fn is_terminal(state: JobState) -> bool {
    matches!(state, JobState::Done | JobState::Failed)
}

/// True when another attempt is allowed (`attempt` is 1-based).
pub fn can_retry(attempt: u32) -> bool {
    (1..MAX_ATTEMPTS).contains(&attempt)
}

/// Build the idempotency key for a job: `ingest:{user}:{sha}:{ref}`.
///
/// - `content_sha256_hex`: sha256 of the canonical NZB bytes (upload) or of
///   the indexer's GUID URL (search), lowercase hex.
/// - `source_ref`: `"upload"` or the indexer's GUID, binding the key to the
///   exact release so a retry of the same content reuses the job row.
pub fn derive_idempotency_key(
    user_id: &str,
    content_sha256_hex: &str,
    source_ref: &str,
) -> Result<String, &'static str> {
    if user_id.is_empty() || user_id.len() > 128 {
        return Err("user id must be 1..=128 chars");
    }
    let hex_ok =
        content_sha256_hex.len() == 64 && content_sha256_hex.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex_ok {
        return Err("content sha256 must be 64 lowercase hex chars");
    }
    if source_ref.is_empty() || source_ref.len() > 512 {
        return Err("source ref must be 1..=512 chars");
    }
    Ok(format!(
        "ingest:{user_id}:{}:{source_ref}",
        content_sha256_hex.to_ascii_lowercase()
    ))
}

/// Job ids travel in URLs (`GET /v1/ingest/jobs/:id`): keep them narrow.
pub fn validate_job_id(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_job_skips_nothing_upload_skips_search() {
        assert_eq!(
            advance(JobState::Queued, JobSource::IndexerSearch, false),
            JobState::Searching
        );
        assert_eq!(
            advance(JobState::Queued, JobSource::NzbUpload, false),
            JobState::Parsing
        );
    }

    #[test]
    fn happy_path_reaches_done() {
        let mut s = advance(JobState::Queued, JobSource::IndexerSearch, false);
        for _ in 0..10 {
            s = advance(s, JobSource::IndexerSearch, false);
        }
        assert_eq!(s, JobState::Done);
        assert!(is_terminal(s));
    }

    #[test]
    fn repair_loops_back_to_verify_then_matches() {
        assert_eq!(
            advance(JobState::Verifying, JobSource::NzbUpload, true),
            JobState::Repairing
        );
        assert_eq!(
            advance(JobState::Repairing, JobSource::NzbUpload, false),
            JobState::Verifying
        );
        assert_eq!(
            advance(JobState::Verifying, JobSource::NzbUpload, false),
            JobState::Matching
        );
    }

    #[test]
    fn fail_sticks_terminal_states() {
        assert_eq!(fail(JobState::Probing), JobState::Failed);
        assert!(is_terminal(JobState::Failed));
        assert_eq!(fail(JobState::Done), JobState::Done);
        assert_eq!(fail(JobState::Failed), JobState::Failed);
    }

    #[test]
    fn retry_budget_is_bounded() {
        assert!(can_retry(1));
        assert!(can_retry(MAX_ATTEMPTS - 1));
        assert!(!can_retry(0));
        assert!(!can_retry(MAX_ATTEMPTS));
        assert!(!can_retry(MAX_ATTEMPTS + 1));
    }

    #[test]
    fn idempotency_key_binds_user_content_and_source() {
        let sha = "a".repeat(64);
        let k1 = derive_idempotency_key("u1", &sha, "upload").unwrap();
        let k2 = derive_idempotency_key("u1", &sha, "upload").unwrap();
        assert_eq!(k1, k2);
        assert!(k1.starts_with("ingest:u1:"));
        assert_ne!(k1, derive_idempotency_key("u2", &sha, "upload").unwrap());
        assert_ne!(k1, derive_idempotency_key("u1", &sha, "guid-9").unwrap());
    }

    #[test]
    fn idempotency_key_rejects_bad_inputs() {
        let sha = "b".repeat(64);
        assert!(derive_idempotency_key("", &sha, "upload").is_err());
        assert!(derive_idempotency_key("u", "xyz", "upload").is_err());
        assert!(derive_idempotency_key("u", &sha, "").is_err());
        assert!(derive_idempotency_key("u", &"A".repeat(64), "upload").is_ok());
    }

    #[test]
    fn job_id_validation_rejects_path_tricks() {
        assert!(validate_job_id("job_01-abc"));
        assert!(!validate_job_id(""));
        assert!(!validate_job_id("../../etc"));
        assert!(!validate_job_id("a/b"));
        assert!(!validate_job_id(&"x".repeat(65)));
    }
}
