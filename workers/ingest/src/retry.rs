//! Queue retry budget + DLQ quarantine for poison messages.
//!
//! One poison NZB must never wedge `nntune-ingest-jobs` forever. Every
//! delivery settles **per message** (ack on success, delayed retry while
//! budget remains, DLQ quarantine on exhaustion), so a failing job ahead of
//! valid ones costs retries, never head-of-line blocking.
//!
//! Three budgets agree by construction (see the `const` asserts below):
//! - [`crate::jobs::MAX_ATTEMPTS`] (5): deliveries per job, gated by
//!   [`crate::jobs::can_retry`]. The 1-based attempt in `INGEST_STATE` is
//!   authoritative; Cloudflare redelivers the Queue body unchanged.
//! - [`CONSUMER_MAX_RETRIES`] (4): `[[queues.consumers]] max_retries` in
//!   `wrangler.toml.example` — the native redelivery slots.
//! - [`crate::dispatch::PAYLOAD_RETRY_ATTEMPTS`] (4): payload-fetch retries,
//!   so every redelivery has a KV-read slot.
//!
//! Backoff ([`retry_delay_secs`]) mirrors the payload-fetch schedule
//! (5/15/30/60 s): redeliveries stay spaced across the documented ~60 s KV
//! read lag instead of hot-looping. Exhaustion sends a [`DlqEntry`] — the
//! original message plus machine-readable `error_code` and `attempts` body
//! fields — to [`DLQ_QUEUE_NAME`]; the operator redrives with [`redrive`]
//! (see README).

use serde::{Deserialize, Serialize};

use crate::dispatch::{payload_retry_delay_ms, PAYLOAD_RETRY_ATTEMPTS};
use crate::jobs::{can_retry, is_terminal, JobCheckpoint, JobSource, JobState, MAX_ATTEMPTS};
use crate::newznab::redact_api_key;
use crate::queue::QueueMsg;

/// Queue binding the consumer uses to send quarantine entries (see
/// `[[queues.producers]]` in `wrangler.toml.example`).
pub const DLQ_QUEUE_BINDING: &str = "INGEST_DLQ";
/// Quarantine queue: exhausted and permanently-failed messages land here.
pub const DLQ_QUEUE_NAME: &str = "nntune-ingest-dlq";
/// Native consumer redeliveries per message. Must equal `[[queues.consumers]]
/// max_retries` in `wrangler.toml.example` and `MAX_ATTEMPTS - 1`, so the
/// job-level budget and the Queue-level slots describe the same 1 + 4
/// deliveries.
pub const CONSUMER_MAX_RETRIES: u32 = 4;
/// Ceiling for [`retry_delay_secs`]: the longest single wait between
/// redeliveries.
pub const MAX_RETRY_DELAY_SECS: u32 = 60;

/// Deterministic step failures, classified so the consumer knows whether a
/// redelivery could ever succeed.
pub const ERR_PAYLOAD_MISSING: &str = "payload_missing";
pub const ERR_PARSE_FAILED: &str = "parse_failed";
pub const ERR_PROBE_FAILED: &str = "probe_failed";
pub const ERR_VERIFY_FAILED: &str = "verify_failed";
pub const ERR_REPAIR_FAILED: &str = "repair_failed";
pub const ERR_MATCH_FAILED: &str = "match_failed";
pub const ERR_WRITE_FAILED: &str = "write_failed";

// Compile-time: the three budgets describe the same 1 + 4 deliveries (see
// module docs). Breaks the build if MAX_ATTEMPTS or the payload budget moves
// without updating CONSUMER_MAX_RETRIES and wrangler's max_retries together.
const _: () = assert!(CONSUMER_MAX_RETRIES == MAX_ATTEMPTS - 1);
const _: () = assert!(CONSUMER_MAX_RETRIES == PAYLOAD_RETRY_ATTEMPTS);

/// Backoff before the next redelivery, keyed on the attempt just spent
/// (1-based). Uses the payload-fetch schedule's matching zero-based index.
pub fn retry_delay_secs(attempt: u32) -> u32 {
    let retry_index = attempt.saturating_sub(1);
    (payload_retry_delay_ms(retry_index) / 1000) as u32
}

/// Character limit for untrusted failure detail stored in the DLQ.
pub const MAX_DLQ_DETAIL_CHARS: usize = 512;

fn bounded_redacted_detail(detail: &str) -> String {
    let redacted = redact_api_key(detail);
    let mut chars = redacted.chars();
    let bounded: String = chars.by_ref().take(MAX_DLQ_DETAIL_CHARS).collect();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

/// One failed step run. `transient` failures may succeed on redelivery (pool
/// 430s, KV lag, Container 500s); permanent ones never will (bytes that fail
/// the intake guard, an unparseable NZB), so they skip the budget and go
/// straight to the DLQ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobError {
    pub code: &'static str,
    pub transient: bool,
    pub detail: String,
}

impl JobError {
    pub fn transient(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            transient: true,
            detail: detail.into(),
        }
    }

    pub fn permanent(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            transient: false,
            detail: detail.into(),
        }
    }
}

/// What the consumer must do with one delivered message. The live
/// `#[worker::event(queue)]` handler maps these onto `ack()`,
/// `retry_with_options(delay)`, and `INGEST_DLQ.send(entry)` + `ack()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    Ack,
    Retry {
        delay_secs: u32,
    },
    /// Boxed: the entry carries the whole original message, ~64x the other
    /// variants (see `large_enum_variant`).
    Quarantine(Box<DlqEntry>),
}

/// A quarantined message: the original [`QueueMsg`] (with `attempt` = the
/// spent delivery count) plus the machine reason for quarantine. Serialized
/// as the DLQ queue body, so redrive needs no second fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DlqEntry {
    pub message: QueueMsg,
    pub attempts: u32,
    pub error_code: String,
    pub reason: String,
}

/// Record the next Queue redelivery in the durable checkpoint before invoking
/// the platform retry operation. Persist the returned checkpoint to
/// `INGEST_STATE` atomically with the job id before returning Retry.
pub fn record_redelivery(cp: &mut JobCheckpoint) -> Option<u32> {
    if !can_retry(cp.attempt) || is_terminal(cp.state) {
        return None;
    }
    let spent_attempt = cp.attempt;
    cp.attempt += 1;
    Some(retry_delay_secs(spent_attempt))
}

/// Mark a quarantined checkpoint terminal before acknowledging the source
/// message. The handler persists this value to `INGEST_STATE` before sending
/// the DLQ entry; a failed checkpoint write must leave the source unacked.
pub fn fail_checkpoint(cp: &mut JobCheckpoint, code: &str, reason: &str) {
    if !is_terminal(cp.state) {
        cp.state = JobState::Failed;
        cp.failure_code = Some(code.to_string());
        cp.failure_reason = Some(bounded_redacted_detail(reason));
    }
}

/// Reset a checkpoint before re-enqueuing a manually redriven job. Preserve
/// its identity, source, and all input references; persist the returned state
/// under the same job id before sending the redriven QueueMsg.
pub fn redrive_checkpoint(cp: &mut JobCheckpoint) {
    cp.state = JobState::Queued;
    cp.attempt = 1;
    cp.repair_rounds = 0;
    cp.failure_code = None;
    cp.failure_reason = None;
    cp.probe_tally = None;
}

/// Decide one message's fate from its durable checkpoint and step result.
/// The checkpoint, not the immutable Queue body, is authoritative for retry
/// budget. A job-id mismatch returns an error without mutation. The caller
/// persists mutations before retrying or acknowledging.
pub fn decide_checkpoint(
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
    result: Result<(), JobError>,
) -> Result<Disposition, RetryError> {
    if msg.job_id != cp.job_id {
        return Err(RetryError::CheckpointJobMismatch);
    }
    if cp.state == JobState::Done {
        return Ok(Disposition::Ack);
    }
    if cp.state == JobState::Failed {
        return Ok(Disposition::Quarantine(Box::new(
            quarantine_existing_failure(msg, cp),
        )));
    }
    Ok(match result {
        Ok(()) => Disposition::Ack,
        Err(e) if !e.transient => {
            fail_checkpoint(cp, e.code, &e.detail);
            Disposition::Quarantine(Box::new(quarantine(msg, cp.attempt, &e)))
        }
        Err(e) => match record_redelivery(cp) {
            Some(delay_secs) => Disposition::Retry { delay_secs },
            None => {
                fail_checkpoint(cp, e.code, &e.detail);
                Disposition::Quarantine(Box::new(quarantine(msg, cp.attempt, &e)))
            }
        },
    })
}

fn quarantine_existing_failure(msg: &QueueMsg, cp: &JobCheckpoint) -> DlqEntry {
    let attempts = cp.attempt;
    let error_code = cp
        .failure_code
        .clone()
        .unwrap_or_else(|| "job_failed".to_string());
    let mut message = msg.clone();
    message.attempt = attempts;
    message.state = JobState::Failed;
    DlqEntry {
        message,
        attempts,
        reason: format!(
            "{error_code} after {attempts} attempt(s): {}",
            bounded_redacted_detail(cp.failure_reason.as_deref().unwrap_or("previously failed"))
        ),
        error_code,
    }
}

fn quarantine(msg: &QueueMsg, attempts: u32, err: &JobError) -> DlqEntry {
    let reason = bounded_redacted_detail(&err.detail);
    let mut message = msg.clone();
    message.attempt = attempts;
    message.state = JobState::Failed;
    DlqEntry {
        message,
        attempts,
        error_code: err.code.to_string(),
        reason: format!("{} after {} attempt(s): {}", err.code, attempts, reason),
    }
}

/// Build a redrive message and reset its matching Failed checkpoint after the
/// cause is fixed. The job id must match; non-Failed checkpoints are rejected
/// to prevent resetting active work. Caller must persist the mutated checkpoint
/// to `INGEST_STATE` before enqueueing this message; if that write fails, do not
/// enqueue. A still-poison message burns the fresh budget and quarantines again.
pub fn redrive(entry: &DlqEntry, cp: &mut JobCheckpoint) -> Result<QueueMsg, RetryError> {
    if entry.message.job_id != cp.job_id {
        return Err(RetryError::CheckpointJobMismatch);
    }
    if cp.state != JobState::Failed {
        return Err(RetryError::CheckpointNotFailed);
    }
    redrive_checkpoint(cp);
    let mut msg = entry.message.clone();
    msg.state = cp.state;
    msg.attempt = cp.attempt;
    msg.repair_rounds = cp.repair_rounds;
    Ok(msg)
}

/// Error returned for mismatched message/checkpoint pairs, invalid batch
/// pairing, or redrive attempts from non-Failed state. Errors do not mutate a
/// checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryError {
    CheckpointCountMismatch,
    CheckpointJobMismatch,
    CheckpointNotFailed,
}

/// Redrive a raw [`QueueMsg`] that reached the native DLQ (handler panic or
/// timeout: Cloudflare moves the body without a reason envelope). Requires a
/// matching Failed checkpoint; persist the reset before enqueue. For handling
/// a nonterminal checkpoint, see README "Redrive".
pub fn redrive_raw(msg: &QueueMsg, cp: &mut JobCheckpoint) -> Result<QueueMsg, RetryError> {
    if msg.job_id != cp.job_id {
        return Err(RetryError::CheckpointJobMismatch);
    }
    if cp.state != JobState::Failed {
        return Err(RetryError::CheckpointNotFailed);
    }
    redrive_checkpoint(cp);
    let mut next = msg.clone();
    next.state = cp.state;
    next.attempt = cp.attempt;
    next.repair_rounds = cp.repair_rounds;
    Ok(next)
}

/// Per-message settlement report for one consumed batch. The consumer acks
/// successes, retries failures with backoff, and quarantines exhaustion —
/// each message on its own, never a batch-level throw, so one poison job
/// cannot wedge the valid jobs behind it.
#[derive(Debug, Default)]
pub struct BatchReport {
    pub acked: usize,
    pub retried: usize,
    pub quarantined: Vec<DlqEntry>,
}

/// Settle a batch using the matching durable checkpoint for each Queue body.
/// The caller persists each mutated checkpoint before applying the returned
/// settlement (retry, DLQ send, or ack).
pub fn drive_batch<F>(
    msgs: &[QueueMsg],
    checkpoints: &mut [JobCheckpoint],
    mut run: F,
) -> Result<BatchReport, RetryError>
where
    F: FnMut(&QueueMsg) -> Result<(), JobError>,
{
    if msgs.len() != checkpoints.len() {
        return Err(RetryError::CheckpointCountMismatch);
    }
    if msgs
        .iter()
        .zip(checkpoints.iter())
        .any(|(msg, cp)| msg.job_id != cp.job_id)
    {
        return Err(RetryError::CheckpointJobMismatch);
    }
    let mut report = BatchReport::default();
    for (msg, checkpoint) in msgs.iter().zip(checkpoints) {
        let result = if is_terminal(checkpoint.state) {
            Ok(())
        } else {
            run(msg)
        };
        let disposition = decide_checkpoint(msg, checkpoint, result)
            .expect("checkpoint pairing was validated before batch processing");
        match disposition {
            Disposition::Ack => report.acked += 1,
            Disposition::Retry { .. } => report.retried += 1,
            Disposition::Quarantine(entry) => report.quarantined.push(*entry),
        }
    }
    Ok(report)
}

/// A well-formed job whose processing always fails — the shared poison
/// fixture for retry/DLQ tests (and the future consumer E2E). Built through
/// the real upload dispatch so it carries a valid key and payload ref; only
/// its *processing* is poisoned, by the test's `run` closure.
pub fn poison_fixture(user_id: &str) -> QueueMsg {
    crate::dispatch::build_upload_dispatch(user_id, crate::fixtures::SMALL_NZB)
        .expect("poison fixture must pass the intake guard")
}

/// Source tag used by [`poison_fixture`] callers that share one batch.
pub const POISON_USER: &str = "poison_user";

/// Builds the `Queued` upload message behind [`poison_fixture`]; exposed so
/// tests assert the fixture passes intake (valid key, payload ref) rather
/// than smuggling in a malformed message.
pub fn poison_source() -> JobSource {
    JobSource::NzbUpload
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{payload_retry_delay_ms, PAYLOAD_KV_READ_LAG_MS};

    fn msg_at(attempt: u32) -> QueueMsg {
        let mut m = poison_fixture(POISON_USER);
        m.attempt = attempt;
        m
    }

    fn checkpoint_for(msg: &QueueMsg) -> JobCheckpoint {
        JobCheckpoint {
            job_id: msg.job_id.clone(),
            user_id: msg.user_id.clone(),
            idempotency_key: msg.idempotency_key.clone(),
            source: msg.source,
            state: msg.state,
            attempt: msg.attempt,
            repair_rounds: msg.repair_rounds,
            probe_tally: None,
            failure_code: None,
            failure_reason: None,
        }
    }

    fn boom() -> Result<(), JobError> {
        Err(JobError::transient(
            ERR_PROBE_FAILED,
            "430 on every segment",
        ))
    }

    #[test]
    fn retry_delay_is_bounded_increasing_and_covers_kv_lag() {
        for attempt in 1..=CONSUMER_MAX_RETRIES {
            assert_eq!(
                retry_delay_secs(attempt) as u64 * 1000,
                payload_retry_delay_ms(attempt - 1)
            );
        }
        assert_eq!(retry_delay_secs(0), 5);
        assert_eq!(retry_delay_secs(u32::MAX), MAX_RETRY_DELAY_SECS);
        assert!(retry_delay_secs(2) > retry_delay_secs(1));
        assert!(retry_delay_secs(3) > retry_delay_secs(2));
        // Spacing across the redelivery slots covers the documented KV read
        // lag, so retries are spaced past the edge-cached miss window instead
        // of hot-looping inside it.
        let total_ms: u64 = (1..=CONSUMER_MAX_RETRIES)
            .map(|a| retry_delay_secs(a) as u64 * 1000)
            .sum();
        assert!(
            total_ms >= PAYLOAD_KV_READ_LAG_MS,
            "redelivery spacing {total_ms}ms must cover KV lag {PAYLOAD_KV_READ_LAG_MS}ms"
        );
    }

    #[test]
    fn success_acks_whatever_the_attempt() {
        let msg = msg_at(1);
        assert!(matches!(
            decide_checkpoint(&msg, &mut checkpoint_for(&msg), Ok(())).unwrap(),
            Disposition::Ack
        ));
        let msg = msg_at(MAX_ATTEMPTS);
        assert!(matches!(
            decide_checkpoint(&msg, &mut checkpoint_for(&msg), Ok(())).unwrap(),
            Disposition::Ack
        ));
    }

    #[test]
    fn transient_failure_retries_with_backoff_until_the_budget_runs_out() {
        let msg = msg_at(1);
        let mut cp = checkpoint_for(&msg);
        for attempt in 1..MAX_ATTEMPTS {
            match decide_checkpoint(&msg, &mut cp, boom()).unwrap() {
                Disposition::Retry { delay_secs } => {
                    assert_eq!(delay_secs, retry_delay_secs(attempt));
                    assert_eq!(cp.attempt, attempt + 1);
                }
                d => panic!("attempt {attempt} must retry, got {d:?}"),
            }
        }
        match decide_checkpoint(&msg, &mut cp, boom()).unwrap() {
            Disposition::Quarantine(entry) => {
                assert_eq!(entry.attempts, MAX_ATTEMPTS);
                assert_eq!(entry.message.attempt, MAX_ATTEMPTS);
                assert_eq!(entry.error_code, ERR_PROBE_FAILED);
                assert_eq!(cp.state, JobState::Failed);
            }
            d => panic!("spent budget must quarantine, got {d:?}"),
        }
    }

    #[test]
    fn terminal_failed_checkpoint_redelivery_preserves_failure_for_dlq() {
        let msg = msg_at(1);
        let mut cp = checkpoint_for(&msg);
        fail_checkpoint(&mut cp, ERR_PROBE_FAILED, "first failure");
        let original_code = cp.failure_code.clone();
        let original_reason = cp.failure_reason.clone();
        let disposition = decide_checkpoint(&msg, &mut cp, boom()).unwrap();
        let Disposition::Quarantine(entry) = disposition else {
            panic!("failed checkpoint must remain recoverable to DLQ");
        };
        assert_eq!(entry.error_code, ERR_PROBE_FAILED);
        assert!(entry.reason.contains("first failure"));
        fail_checkpoint(&mut cp, ERR_PARSE_FAILED, "later failure");
        assert_eq!(cp.failure_code, original_code);
        assert_eq!(cp.failure_reason, original_reason);
    }

    #[test]
    fn permanent_failure_skips_the_budget() {
        let msg = msg_at(1);
        let mut cp = checkpoint_for(&msg);
        let err = JobError::permanent(ERR_PARSE_FAILED, "not an NZB document");
        match decide_checkpoint(&msg, &mut cp, Err(err)).unwrap() {
            Disposition::Quarantine(entry) => {
                assert_eq!(entry.attempts, 1);
                assert_eq!(entry.error_code, ERR_PARSE_FAILED);
            }
            d => panic!("permanent failure must quarantine at once, got {d:?}"),
        }
    }

    #[test]
    fn corrupt_zero_attempt_quarantines_instead_of_looping() {
        let msg = msg_at(0);
        let mut cp = checkpoint_for(&msg);
        match decide_checkpoint(&msg, &mut cp, boom()).unwrap() {
            Disposition::Quarantine(entry) => assert_eq!(entry.attempts, 0),
            d => panic!("attempt 0 must never retry, got {d:?}"),
        }
    }

    #[test]
    fn dlq_entry_roundtrips_with_reason_body_fields() {
        let msg = msg_at(MAX_ATTEMPTS);
        let mut cp = checkpoint_for(&msg);
        let entry = match decide_checkpoint(&msg, &mut cp, boom()).unwrap() {
            Disposition::Quarantine(entry) => entry,
            d => panic!("expected quarantine, got {d:?}"),
        };
        let back: DlqEntry = serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
        assert_eq!(back, *entry);
        let body = serde_json::to_value(&back).unwrap();
        assert_eq!(body["error_code"], ERR_PROBE_FAILED);
        assert_eq!(body["attempts"], MAX_ATTEMPTS);
    }

    #[test]
    fn redrive_restores_a_queued_job_with_fresh_budgets() {
        let mut spent = msg_at(MAX_ATTEMPTS);
        spent.repair_rounds = crate::jobs::MAX_REPAIR_ROUNDS;
        let mut cp = checkpoint_for(&spent);
        let entry = match decide_checkpoint(&spent, &mut cp, boom()).unwrap() {
            Disposition::Quarantine(entry) => entry,
            d => panic!("expected quarantine, got {d:?}"),
        };
        let mut cp = checkpoint_for(&spent);
        cp.state = JobState::Failed;
        cp.attempt = MAX_ATTEMPTS;
        cp.repair_rounds = crate::jobs::MAX_REPAIR_ROUNDS;
        cp.failure_code = Some(ERR_PROBE_FAILED.into());
        cp.failure_reason = Some("spent".into());
        let again = redrive(&entry, &mut cp).unwrap();
        assert_eq!(again.state, JobState::Queued);
        assert_eq!(cp.state, JobState::Queued);
        assert_eq!(cp.attempt, 1);
        assert_eq!(cp.repair_rounds, 0);
        assert!(cp.failure_code.is_none());
        assert!(cp.failure_reason.is_none());
        assert_eq!(again.attempt, 1);
        assert_eq!(again.repair_rounds, 0);
        assert_eq!(again.job_id, spent.job_id);
        assert_eq!(again.idempotency_key, spent.idempotency_key);
        cp.state = JobState::Failed;
        let raw = redrive_raw(&spent, &mut cp).unwrap();
        assert_eq!(raw.state, JobState::Queued);
        assert_eq!(raw.attempt, 1);
        assert_eq!(raw.repair_rounds, 0);
    }

    #[test]
    fn drive_batch_settles_per_message_and_never_throws() {
        let msgs = vec![
            poison_fixture(POISON_USER),
            poison_fixture("valid_a"),
            poison_fixture("valid_b"),
        ];
        let mut checkpoints: Vec<_> = msgs.iter().map(checkpoint_for).collect();
        checkpoints[0].attempt = MAX_ATTEMPTS;
        let report = drive_batch(&msgs, &mut checkpoints, |m| {
            if m.user_id == POISON_USER {
                boom()
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(report.acked, 2);
        assert_eq!(report.retried, 0);
        assert_eq!(report.quarantined.len(), 1);
        assert!(drive_batch(&[], &mut [], |_| Ok(()))
            .unwrap()
            .quarantined
            .is_empty());
    }

    #[test]
    fn drive_batch_skips_job_step_for_done_and_failed_checkpoints() {
        let msgs = vec![msg_at(1), msg_at(1)];
        let mut checkpoints: Vec<_> = msgs.iter().map(checkpoint_for).collect();
        checkpoints[0].state = JobState::Done;
        checkpoints[1].state = JobState::Failed;
        checkpoints[1].failure_code = Some(ERR_PROBE_FAILED.to_string());
        checkpoints[1].failure_reason = Some("previous failure".to_string());
        let mut run_calls = 0;

        let report = drive_batch(&msgs, &mut checkpoints, |_| {
            run_calls += 1;
            Ok(())
        })
        .unwrap();

        assert_eq!(run_calls, 0);
        assert_eq!(report.acked, 1);
        assert_eq!(report.retried, 0);
        assert_eq!(report.quarantined.len(), 1);
        assert_eq!(report.quarantined[0].error_code, ERR_PROBE_FAILED);
    }

    #[test]
    fn mismatched_checkpoint_pairs_return_errors_without_mutation() {
        let msg = msg_at(MAX_ATTEMPTS);
        let mut cp = checkpoint_for(&msg);
        let entry = match decide_checkpoint(&msg, &mut cp, boom()).unwrap() {
            Disposition::Quarantine(entry) => entry,
            d => panic!("expected quarantine, got {d:?}"),
        };

        let mut wrong_cp = cp.clone();
        wrong_cp.job_id = "different_job".to_string();
        let before = wrong_cp.clone();
        assert_eq!(
            redrive(&entry, &mut wrong_cp),
            Err(RetryError::CheckpointJobMismatch)
        );
        assert_eq!(wrong_cp, before);
        assert_eq!(
            redrive_raw(&msg, &mut wrong_cp),
            Err(RetryError::CheckpointJobMismatch)
        );
        assert_eq!(wrong_cp, before);
        assert_eq!(
            decide_checkpoint(&msg, &mut wrong_cp, boom()),
            Err(RetryError::CheckpointJobMismatch)
        );
        assert_eq!(wrong_cp, before);

        let mut active_cp = cp.clone();
        active_cp.state = JobState::Queued;
        let active_before = active_cp.clone();
        assert_eq!(
            redrive(&entry, &mut active_cp),
            Err(RetryError::CheckpointNotFailed)
        );
        assert_eq!(
            redrive_raw(&msg, &mut active_cp),
            Err(RetryError::CheckpointNotFailed)
        );
        assert_eq!(active_cp, active_before);

        assert_eq!(
            drive_batch(std::slice::from_ref(&msg), &mut [], |_| Ok(())).unwrap_err(),
            RetryError::CheckpointCountMismatch
        );
        assert_eq!(
            drive_batch(
                std::slice::from_ref(&msg),
                std::slice::from_mut(&mut wrong_cp),
                |_| Ok(())
            )
            .unwrap_err(),
            RetryError::CheckpointJobMismatch
        );
        assert_eq!(wrong_cp, before);
    }

    #[test]
    fn budgets_agree_across_job_queue_and_payload() {
        assert_eq!(CONSUMER_MAX_RETRIES, MAX_ATTEMPTS - 1);
        assert_eq!(CONSUMER_MAX_RETRIES, PAYLOAD_RETRY_ATTEMPTS);
        assert_eq!(DLQ_QUEUE_BINDING, "INGEST_DLQ");
        assert_eq!(DLQ_QUEUE_NAME, "nntune-ingest-dlq");
    }

    #[test]
    fn poison_fixture_is_a_valid_job_with_poisoned_processing() {
        let p = poison_fixture(POISON_USER);
        assert_eq!(p.source, poison_source());
        assert!(p.payload_ref.is_some());
        assert!(p.idempotency_key.starts_with("ingest:poison_user:"));
    }
}
