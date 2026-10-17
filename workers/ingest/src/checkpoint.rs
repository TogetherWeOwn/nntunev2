//! Durable ingest checkpoint storage in the `INGEST_STATE` KV namespace.

use worker::{Env, Result};

use crate::jobs::{validate_job_id, JobCheckpoint, JobState};
use crate::queue::QueueMsg;
use crate::retry::redrive_checkpoint;

pub const STATE_KV_BINDING: &str = "INGEST_STATE";
pub const CHECKPOINT_TTL_SECS: u64 = 60 * 60 * 24 * 30;

fn checkpoint_key(job_id: &str) -> Option<String> {
    validate_job_id(job_id).then(|| format!("ingest:checkpoint:{job_id}"))
}

/// The checkpoint a freshly enqueued job starts from. Callers validate the
/// message first (`queue_consumer::validate_queue_msg` / the dispatch
/// builders); this only projects its identity fields.
pub fn initial(msg: &QueueMsg) -> JobCheckpoint {
    JobCheckpoint {
        job_id: msg.job_id.clone(),
        user_id: msg.user_id.clone(),
        idempotency_key: msg.idempotency_key.clone(),
        source: msg.source,
        state: JobState::Queued,
        attempt: 1,
        repair_rounds: 0,
        probe_tally: None,
        failure_code: None,
        failure_reason: None,
    }
}

/// Decide what a route that just enqueued `msg` (a real admission, not an
/// `AlreadyQueued` replay) must write so the accepted job is immediately
/// queryable and a re-admitted job is not stuck behind its old failure:
///
/// - no checkpoint yet -> write the initial `Queued` checkpoint;
/// - a matching `Failed` checkpoint -> write it reset to `Queued`
///   ([`redrive_checkpoint`]), so the new enqueue actually runs instead of
///   being acked/quarantined against the stale failure;
/// - anything else (active or `Done`, or an identity mismatch) -> write
///   nothing: never overwrite a checkpoint the consumer has advanced.
///
/// `None` as the return means "no write". Pure so the policy is unit-tested
/// without a KV binding; the caller performs at most this one write.
pub fn admission_write(existing: Option<&JobCheckpoint>, msg: &QueueMsg) -> Option<JobCheckpoint> {
    match existing {
        None => Some(initial(msg)),
        Some(cp)
            if cp.state == JobState::Failed
                && cp.job_id == msg.job_id
                && cp.user_id == msg.user_id
                && cp.idempotency_key == msg.idempotency_key
                && cp.source == msg.source =>
        {
            let mut reset = cp.clone();
            redrive_checkpoint(&mut reset);
            Some(reset)
        }
        Some(_) => None,
    }
}

pub async fn get(env: &Env, job_id: &str) -> Result<Option<JobCheckpoint>> {
    let key =
        checkpoint_key(job_id).ok_or_else(|| worker::Error::RustError("invalid job id".into()))?;
    let store = env.kv(STATE_KV_BINDING)?;
    store
        .get(&key)
        .json::<JobCheckpoint>()
        .await
        .map_err(|e| worker::Error::RustError(e.to_string()))
}

pub async fn put(env: &Env, checkpoint: &JobCheckpoint) -> Result<()> {
    let key = checkpoint_key(&checkpoint.job_id)
        .ok_or_else(|| worker::Error::RustError("invalid job id".into()))?;
    let store = env.kv(STATE_KV_BINDING)?;
    store
        .put(&key, serde_json::to_string(checkpoint)?)?
        .expiration_ttl(CHECKPOINT_TTL_SECS)
        .execute()
        .await
        .map_err(|e| worker::Error::RustError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::build_search_dispatch;

    fn msg() -> QueueMsg {
        build_search_dispatch("user_01", "dark side", "vault_idx_01", &[3000], 25, 0).unwrap()
    }

    #[test]
    fn admission_write_creates_initial_checkpoint_for_new_jobs() {
        let msg = msg();
        let cp = admission_write(None, &msg).expect("new job gets a checkpoint");
        assert_eq!(cp.state, JobState::Queued);
        assert_eq!(cp.attempt, 1);
        assert_eq!(cp.job_id, msg.job_id);
        assert_eq!(cp.idempotency_key, msg.idempotency_key);
    }

    #[test]
    fn admission_write_resets_a_matching_failed_checkpoint() {
        let msg = msg();
        let mut failed = initial(&msg);
        failed.state = JobState::Failed;
        failed.attempt = 5;
        failed.failure_code = Some("probe_failed".into());
        failed.failure_reason = Some("runtime unavailable".into());
        let reset = admission_write(Some(&failed), &msg).expect("failed job restarts");
        assert_eq!(reset.state, JobState::Queued);
        assert_eq!(reset.attempt, 1);
        assert!(reset.failure_code.is_none());
        assert!(reset.failure_reason.is_none());
    }

    #[test]
    fn admission_write_never_overwrites_active_done_or_foreign_checkpoints() {
        let msg = msg();
        let mut active = initial(&msg);
        active.state = JobState::Probing;
        assert!(admission_write(Some(&active), &msg).is_none());
        active.state = JobState::Done;
        assert!(admission_write(Some(&active), &msg).is_none());
        let mut foreign = initial(&msg);
        foreign.state = JobState::Failed;
        foreign.user_id = "other_user".into();
        assert!(admission_write(Some(&foreign), &msg).is_none());
    }

    #[test]
    fn checkpoint_keys_are_scoped_and_job_id_validated() {
        assert_eq!(
            checkpoint_key("job_01-a").as_deref(),
            Some("ingest:checkpoint:job_01-a")
        );
        assert!(checkpoint_key("../secret").is_none());
        assert!(checkpoint_key("").is_none());
    }
}
