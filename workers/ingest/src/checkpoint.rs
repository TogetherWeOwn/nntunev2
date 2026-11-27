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

use crate::jobs::{validate_job_id, JobCheckpoint, JobState, MAX_ATTEMPTS, MAX_REPAIR_ROUNDS};
use crate::nzb_intake::{MAX_DECLARED_BYTES, MAX_SEGMENTS};
use crate::probe::MAX_PROBE_SEGMENTS;
use crate::queue::QueueMsg;
use crate::retry::redrive_checkpoint;
use crate::verify::{VerifyProgress, MAX_VERIFY_FILES};

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
        par2_set_id: None,
        missing_blocks: 0,
        failure_code: None,
        failure_reason: None,
        // Attribution starts here and lives on the checkpoint after:
        // redelivered Queue bodies are immutable, so the message copy
        // must never be read back over this field.
        provider_id: msg.provider_id.clone(),
        // Transcode inputs record at the first verify `Complete`; the
        // cursor starts at the first input.
        transcode_inputs: Vec::new(),
        transcode_next: 0,
        accepted_audio_manifest_v1: None,
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

fn validate_checkpoint(cp: &JobCheckpoint, job_id: &str) -> Result<()> {
    if cp.job_id != job_id || cp.validate_audio_manifest().is_err() {
        return Err(worker::Error::RustError(
            "checkpoint identity mismatch".into(),
        ));
    }
    // The exact end is a valid completed cursor. Anything beyond it must
    // fail here instead of advancing the consumer past unperformed work.
    if !(1..=MAX_ATTEMPTS).contains(&cp.attempt)
        || cp.repair_rounds > MAX_REPAIR_ROUNDS
        || cp.transcode_inputs.len() > MAX_VERIFY_FILES
        || cp.transcode_next > cp.transcode_inputs.len()
        || cp.probe_tally.as_ref().is_some_and(|tally| {
            !(1..=MAX_SEGMENTS.div_ceil(MAX_PROBE_SEGMENTS)).contains(&tally.total_batches)
                || tally.missing_by_batch.iter().any(|(&index, &missing)| {
                    index >= tally.total_batches || missing as usize > MAX_PROBE_SEGMENTS
                })
        })
    {
        return Err(worker::Error::RustError(
            "invalid checkpoint progress".into(),
        ));
    }
    Ok(())
}

pub async fn get(env: &Env, job_id: &str) -> Result<Option<JobCheckpoint>> {
    let key =
        checkpoint_key(job_id).ok_or_else(|| worker::Error::RustError("invalid job id".into()))?;
    let loaded = get_json(env, &key).await?;
    if let Some(cp) = &loaded {
        validate_checkpoint(cp, job_id)?;
    }
    Ok(loaded)
}

async fn get_json<T: serde::de::DeserializeOwned>(env: &Env, key: &str) -> Result<Option<T>> {
    let text = env
        .kv(STATE_KV_BINDING)?
        .get(key)
        .text()
        .await
        .map_err(|_| worker::Error::RustError("checkpoint unavailable".into()))?;
    decode_json(text)
}

fn decode_json<T: serde::de::DeserializeOwned>(text: Option<String>) -> Result<Option<T>> {
    // KV's JS JSON decoder passes object keys as strings even for Rust maps
    // keyed by integers. Read using the same JSON codec as the writer.
    text.as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| worker::Error::RustError("invalid stored checkpoint".into()))
}

pub async fn put(env: &Env, checkpoint: &JobCheckpoint) -> Result<()> {
    let key = checkpoint_key(&checkpoint.job_id)
        .ok_or_else(|| worker::Error::RustError("invalid job id".into()))?;
    validate_checkpoint(checkpoint, &checkpoint.job_id)?;
    put_json(
        env,
        key,
        &serde_json::to_string(checkpoint)?,
        retention_ttl_secs(checkpoint),
    )
    .await
}

/// A frozen selection must survive job replay; expiring it would permit
/// regeneration from changed tags. Legacy/verify checkpoints keep their TTL.
/// Explicit retention cleanup belongs to the future catalogue lifecycle.
pub fn retention_ttl_secs(checkpoint: &JobCheckpoint) -> Option<u64> {
    checkpoint
        .audio_manifest()
        .is_none()
        .then_some(CHECKPOINT_TTL_SECS)
}

/// The one KV write helper behind every key this module stores.
async fn put_json(env: &Env, key: String, json: &str, ttl: Option<u64>) -> Result<()> {
    let store = env.kv(STATE_KV_BINDING)?;
    let mut write = store.put(&key, json.to_string())?;
    if let Some(ttl) = ttl {
        write = write.expiration_ttl(ttl);
    }
    write
        .execute()
        .await
        .map_err(|e| worker::Error::RustError(e.to_string()))
}

fn verify_key(job_id: &str) -> Option<String> {
    validate_job_id(job_id).then(|| format!("ingest:verify:{job_id}"))
}

fn validate_verify_progress(progress: &VerifyProgress) -> Result<()> {
    // The consumer sums spool lengths and judged positions before the
    // verifier checks them against the current NZB. Bound those sums on
    // read and permit the final verification round after all repairs.
    if !(1..=MAX_REPAIR_ROUNDS + 1).contains(&progress.round)
        || progress
            .staged
            .iter()
            .any(|(&index, staged)| index >= MAX_VERIFY_FILES || staged.spool.len() > MAX_SEGMENTS)
        || progress
            .spool
            .iter()
            .any(|(&index, spool)| index >= MAX_VERIFY_FILES || spool.len() > MAX_SEGMENTS)
        || progress.judging.iter().any(|(&index, cursor)| {
            index >= MAX_VERIFY_FILES
                || cursor.segment > MAX_SEGMENTS
                || cursor.pos > MAX_DECLARED_BYTES
        })
    {
        return Err(worker::Error::RustError("invalid verify progress".into()));
    }
    Ok(())
}

fn resume_verify(loaded: Result<Option<VerifyProgress>>) -> Result<Option<VerifyProgress>> {
    // Invalid cached progress can be rebuilt from scratch object metadata.
    // An unavailable KV read still propagates so the consumer retries it.
    Ok(loaded?.filter(|progress| validate_verify_progress(progress).is_ok()))
}

/// Load one job's valid Verifying resume state (see the module docs).
pub async fn get_verify(env: &Env, job_id: &str) -> Result<Option<VerifyProgress>> {
    let key =
        verify_key(job_id).ok_or_else(|| worker::Error::RustError("invalid job id".into()))?;
    resume_verify(get_json(env, &key).await)
}

/// Store one job's Verifying resume state. The consumer's Verifying step
/// is the only writer, at most once per delivery, before the checkpoint
/// persist of the same delivery.
pub async fn put_verify(env: &Env, job_id: &str, progress: &VerifyProgress) -> Result<()> {
    let key =
        verify_key(job_id).ok_or_else(|| worker::Error::RustError("invalid job id".into()))?;
    validate_verify_progress(progress)?;
    put_json(
        env,
        key,
        &serde_json::to_string(progress)?,
        Some(CHECKPOINT_TTL_SECS),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::build_search_dispatch;
    use crate::probe::ProbeTally;
    use crate::transcode::TranscodeInput;
    use crate::verify::JudgeCursor;

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
    fn initial_projects_provider_attribution_onto_the_checkpoint() {
        // The checkpoint is the live attribution copy: whatever the
        // (validated) admission message carries starts here, and the
        // redelivered message never writes back over it.
        let plain = msg();
        assert!(plain.provider_id.is_none());
        assert!(initial(&plain).provider_id.is_none());
        let mut attributed = msg();
        attributed.provider_id = Some("11111111-1111-4111-8111-111111111111".to_string());
        assert_eq!(
            initial(&attributed).provider_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
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
    fn bounds_preserve_legacy_and_completed_transcode_cursors() {
        let msg = msg();
        let mut legacy = serde_json::to_value(initial(&msg)).unwrap();
        let fields = legacy.as_object_mut().unwrap();
        fields.remove("transcode_next");
        fields.remove("transcode_inputs");
        let mut cp: JobCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(validate_checkpoint(&cp, &msg.job_id).is_ok());
        cp.transcode_inputs.push(TranscodeInput {
            file_index: 0,
            key: VerifyProgress::file_key(&msg.job_id, 1, 0, "track.flac"),
            sha256: "a".repeat(64),
            size: 1,
            scratch_prefix: VerifyProgress::scratch_prefix(&msg.job_id, 1),
        });
        cp.state = JobState::Transcoding;
        cp.transcode_next = 1;
        assert!(validate_checkpoint(&cp, &msg.job_id).is_ok());
        cp.transcode_next = usize::MAX;
        assert!(validate_checkpoint(&cp, &msg.job_id).is_err());
        cp.transcode_next = 0;
        cp.transcode_inputs
            .resize(MAX_VERIFY_FILES + 1, cp.transcode_inputs[0].clone());
        assert!(validate_checkpoint(&cp, &msg.job_id).is_err());
    }

    #[test]
    fn bounds_reject_invalid_probe_and_retry_progress() {
        let msg = msg();
        let mut cp = initial(&msg);
        cp.attempt = MAX_ATTEMPTS;
        cp.repair_rounds = MAX_REPAIR_ROUNDS;
        let mut tally = ProbeTally::new(MAX_SEGMENTS.div_ceil(MAX_PROBE_SEGMENTS));
        tally
            .missing_by_batch
            .insert(tally.total_batches - 1, MAX_PROBE_SEGMENTS as u32);
        cp.probe_tally = Some(tally.clone());
        let stored = serde_json::to_string(&cp).unwrap();
        let decoded: JobCheckpoint = decode_json(Some(stored)).unwrap().unwrap();
        assert_eq!(decoded, cp);
        assert!(validate_checkpoint(&cp, &msg.job_id).is_ok());
        let mut corrupt = tally.clone();
        corrupt.total_batches += 1;
        cp.probe_tally = Some(corrupt);
        assert!(validate_checkpoint(&cp, &msg.job_id).is_err());
        for (index, missing) in [(tally.total_batches, 0), (0, u32::MAX)] {
            let mut corrupt = tally.clone();
            corrupt.missing_by_batch.insert(index, missing);
            cp.probe_tally = Some(corrupt);
            assert!(validate_checkpoint(&cp, &msg.job_id).is_err());
        }
        cp.probe_tally = Some(tally);
        cp.repair_rounds += 1;
        assert!(validate_checkpoint(&cp, &msg.job_id).is_err());
        cp.repair_rounds -= 1;
        cp.attempt = 0;
        assert!(validate_checkpoint(&cp, &msg.job_id).is_err());
    }

    #[test]
    fn bounds_verify_storage_recovers_from_invalid_progress() {
        let mut progress = VerifyProgress::new(MAX_REPAIR_ROUNDS + 1);
        let cursor = JudgeCursor {
            segment: 1,
            pos: MAX_DECLARED_BYTES,
            block_index: 1,
            in_block: 0,
            crc32: 0,
            bad: 0,
            fingerprint: "synthetic-fingerprint".into(),
        };
        let resume = |progress: &VerifyProgress| {
            resume_verify(decode_json(Some(serde_json::to_string(progress).unwrap())))
        };
        progress.judging.insert(0, cursor.clone());
        progress.judging.insert(1, cursor.clone());
        assert_eq!(resume(&progress).unwrap(), Some(progress.clone()));
        progress.judging.get_mut(&1).unwrap().pos = u64::MAX;
        assert!(resume(&progress).unwrap().is_none());
        progress.judging.insert(1, cursor);
        progress.spool.insert(MAX_VERIFY_FILES, Vec::new());
        assert!(validate_verify_progress(&progress).is_err());
        progress.spool.clear();
        progress.round = u32::MAX;
        assert!(resume(&progress).unwrap().is_none());
        assert!(resume_verify(decode_json(Some("malformed".into()))).is_err());
        assert!(resume_verify(Err(worker::Error::RustError(
            "synthetic read failure".into()
        )))
        .is_err());
    }

    #[test]
    fn bounds_storage_entry_points_call_the_progress_guards() {
        // Native tests exercise the validators without a Workers KV binding.
        let source = include_str!("checkpoint.rs");
        for (name, guard) in [
            ("get", "validate_checkpoint(cp, job_id)?"),
            ("put", "validate_checkpoint("),
            ("get_verify", "resume_verify(get_json(env, &key).await)"),
            ("put_verify", "validate_verify_progress(progress)?"),
        ] {
            let (_, signature_and_body) =
                source.split_once(&format!("pub async fn {name}(")).unwrap();
            let (function, _) = signature_and_body.split_once("\n}\n").unwrap();
            assert!(function.contains(guard), "{name} must call {guard}");
        }
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
        // The consumer plans exactly one persist per delivery. The call
        // carries the delivery trace alongside the checkpoint, so pin the
        // negated call head rather than the full argument list.
        let persist_call = ["!persist", "("].concat();
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
