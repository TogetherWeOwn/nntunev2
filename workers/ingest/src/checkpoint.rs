//! Durable ingest checkpoint storage in the `INGEST_STATE` KV namespace.
//!
//! **Writers of `ingest:checkpoint:<job_id>`** (exactly two, both behind
//! [`put`]; pinned by `checkpoint_key_has_exactly_two_writers`):
//!
//! 1. the admission route (`lib::ensure_admission_checkpoint`): at most one
//!    write per real (non-replay) admission, per [`admission_plan`];
//! 2. the Queue consumer (`queue_consumer::persist`): at most one planned
//!    write per delivery.
//!
//! Workers KV allows 1 write per second to the same key, and writer 1 can
//! land less than a second before writer 2's first delivery, so the
//! consumer waits out [`KV_SAME_KEY_WRITE_WINDOW_MS`] and retries its one
//! write once in-invocation before giving the delivery back unpersisted.
//!
//! Verifying keeps its per-file resume state under a separate key
//! (`ingest:verify:<job_id>`, [`get_verify`]/[`put_verify`]) with a single
//! writer (the consumer's Verifying step). It is deliberately not a
//! checkpoint field: the scratch objects it indexes carry the same
//! judgement in their metadata, so a lost progress write only costs a
//! re-adoption scan, never correctness — while the checkpoint key keeps
//! its exactly-one-write-per-delivery discipline.

use worker::{Env, Result};

use crate::jobs::{validate_job_id, JobCheckpoint, JobState};
use crate::queue::QueueMsg;
use crate::retry::redrive_checkpoint;
use crate::verify::VerifyProgress;

pub const STATE_KV_BINDING: &str = "INGEST_STATE";
pub const CHECKPOINT_TTL_SECS: u64 = 60 * 60 * 24 * 30;
/// Workers KV same-key write limit: 1 write per second per key.
pub const KV_SAME_KEY_WRITE_WINDOW_MS: u64 = 1_000;

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
    admission_plan(existing, msg).write
}

/// One real admission's checkpoint actions, in the order the route must
/// perform them: write `write` (if any) first, then — only when
/// `reenqueue_after_write` — send one trailing copy of the message. The
/// trailing copy exists for the `Failed`-reset case: the reset lands after
/// the admission's own enqueue, so a delivery racing it could settle
/// against the stale `Failed` checkpoint; the copy sent after the reset is
/// persisted is guaranteed to observe `Queued`. Pure so both the write
/// choice and the reset-then-reenqueue ordering are unit-tested without a
/// KV or Queue binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionPlan {
    pub write: Option<JobCheckpoint>,
    pub reenqueue_after_write: bool,
}

pub fn admission_plan(existing: Option<&JobCheckpoint>, msg: &QueueMsg) -> AdmissionPlan {
    match existing {
        None => AdmissionPlan {
            write: Some(initial(msg)),
            reenqueue_after_write: false,
        },
        Some(cp) if is_failed_for(cp, msg) => {
            let mut reset = cp.clone();
            redrive_checkpoint(&mut reset);
            AdmissionPlan {
                write: Some(reset),
                reenqueue_after_write: true,
            }
        }
        Some(_) => AdmissionPlan {
            write: None,
            reenqueue_after_write: false,
        },
    }
}

/// How an `AlreadyQueued` admission replay (the ledger will not enqueue
/// again inside its replay window) must be answered for a loaded
/// checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayDecision {
    /// The earlier run terminally failed and will never run again in the
    /// window: refuse with 409 instead of claiming `queued`.
    Conflict,
    /// Answer the plain 202 `queued` replay.
    Queued,
}

pub fn replay_decision(existing: Option<&JobCheckpoint>, msg: &QueueMsg) -> ReplayDecision {
    match existing {
        Some(cp) if is_failed_for(cp, msg) => ReplayDecision::Conflict,
        _ => ReplayDecision::Queued,
    }
}

/// True when `cp` is the terminal `Failed` checkpoint of exactly this job
/// (same deterministic identity). An `AlreadyQueued` admission replay uses
/// this to refuse the re-post with 409 instead of answering 202 for a job
/// that will never run again inside the replay window.
pub fn is_failed_for(cp: &JobCheckpoint, msg: &QueueMsg) -> bool {
    cp.state == JobState::Failed
        && cp.job_id == msg.job_id
        && cp.user_id == msg.user_id
        && cp.idempotency_key == msg.idempotency_key
        && cp.source == msg.source
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
    put_json(env, key, &serde_json::to_string(checkpoint)?).await
}

/// The one KV write helper behind every key this module stores.
async fn put_json(env: &Env, key: String, json: &str) -> Result<()> {
    let store = env.kv(STATE_KV_BINDING)?;
    store
        .put(&key, json.to_string())?
        .expiration_ttl(CHECKPOINT_TTL_SECS)
        .execute()
        .await
        .map_err(|e| worker::Error::RustError(e.to_string()))
}

fn verify_key(job_id: &str) -> Option<String> {
    validate_job_id(job_id).then(|| format!("ingest:verify:{job_id}"))
}

/// Load one job's Verifying resume state (see the module docs).
pub async fn get_verify(env: &Env, job_id: &str) -> Result<Option<VerifyProgress>> {
    let key =
        verify_key(job_id).ok_or_else(|| worker::Error::RustError("invalid job id".into()))?;
    let store = env.kv(STATE_KV_BINDING)?;
    store
        .get(&key)
        .json::<VerifyProgress>()
        .await
        .map_err(|e| worker::Error::RustError(e.to_string()))
}

/// Store one job's Verifying resume state. The consumer's Verifying step
/// is the only writer, at most once per delivery, before the checkpoint
/// persist of the same delivery.
pub async fn put_verify(env: &Env, job_id: &str, progress: &VerifyProgress) -> Result<()> {
    let key =
        verify_key(job_id).ok_or_else(|| worker::Error::RustError("invalid job id".into()))?;
    put_json(env, key, &serde_json::to_string(progress)?).await
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
        assert!(!is_failed_for(&foreign, &msg));
        foreign.user_id = msg.user_id.clone();
        assert!(is_failed_for(&foreign, &msg));
        foreign.state = JobState::Done;
        assert!(!is_failed_for(&foreign, &msg));
    }

    #[test]
    fn admission_plan_only_reenqueues_after_a_failed_reset() {
        let msg = msg();
        let fresh = admission_plan(None, &msg);
        assert!(fresh.write.is_some());
        assert!(!fresh.reenqueue_after_write);

        let mut failed = initial(&msg);
        failed.state = JobState::Failed;
        let reset = admission_plan(Some(&failed), &msg);
        assert_eq!(
            reset.write.as_ref().map(|cp| cp.state),
            Some(JobState::Queued)
        );
        assert!(reset.reenqueue_after_write);

        let mut active = initial(&msg);
        active.state = JobState::Probing;
        let untouched = admission_plan(Some(&active), &msg);
        assert!(untouched.write.is_none());
        assert!(!untouched.reenqueue_after_write);
    }

    #[test]
    fn replay_decision_conflicts_only_on_a_matching_failed_checkpoint() {
        let msg = msg();
        assert_eq!(replay_decision(None, &msg), ReplayDecision::Queued);

        let mut cp = initial(&msg);
        assert_eq!(replay_decision(Some(&cp), &msg), ReplayDecision::Queued);
        cp.state = JobState::Failed;
        assert_eq!(replay_decision(Some(&cp), &msg), ReplayDecision::Conflict);
        cp.user_id = "other_user".into();
        assert_eq!(replay_decision(Some(&cp), &msg), ReplayDecision::Queued);
    }

    #[test]
    fn checkpoint_key_has_exactly_two_writers() {
        // Enumerate every writer of `ingest:checkpoint:<job_id>`: all KV
        // puts funnel through `put` below (one key put in this file), and
        // `put` has exactly two call sites — the admission route and the
        // consumer's per-delivery persist. A new writer must update this
        // test and the module docs together. Needles are assembled so
        // this test's own source is not counted.
        let put_call = ["checkpoint", "::put("].concat();
        let store_put = [".put", "(&key"].concat();
        assert_eq!(include_str!("checkpoint.rs").matches(&store_put).count(), 1);
        assert_eq!(include_str!("lib.rs").matches(&put_call).count(), 1);
        assert_eq!(
            include_str!("queue_consumer.rs").matches(&put_call).count(),
            1
        );
        // The consumer plans exactly one persist per delivery.
        let persist_call = ["!persist", "(env, &cp).await"].concat();
        assert_eq!(
            include_str!("queue_consumer.rs")
                .matches(&persist_call)
                .count(),
            1
        );
    }

    #[test]
    fn checkpoint_keys_are_scoped_and_job_id_validated() {
        assert_eq!(
            checkpoint_key("job_01-a").as_deref(),
            Some("ingest:checkpoint:job_01-a")
        );
        assert!(checkpoint_key("../secret").is_none());
        assert!(checkpoint_key("").is_none());
        assert_eq!(
            verify_key("job_01-a").as_deref(),
            Some("ingest:verify:job_01-a")
        );
        assert!(verify_key("../secret").is_none());
    }
}
