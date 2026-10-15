//! Queue retry budget + DLQ quarantine for poison messages.
//!
//! One poison NZB must never wedge `nntune-ingest-jobs` forever. Every
//! delivery settles **per message** (ack on success, delayed retry while
//! budget remains, DLQ quarantine on exhaustion), so a failing job ahead of
//! valid ones costs retries, never head-of-line blocking.
//!
//! Three budgets agree by construction (see the `const` asserts below):
//! - [`crate::jobs::MAX_ATTEMPTS`] (5): deliveries per job, gated by
//!   [`crate::jobs::can_retry`]. `QueueMsg.attempt` is 1-based; the consumer
//!   bumps it on each redelivery via the `INGEST_STATE` checkpoint.
//! - [`CONSUMER_MAX_RETRIES`] (4): `[[queues.consumers]] max_retries` in
//!   `wrangler.toml.example` — the native redelivery slots.
//! - [`crate::dispatch::PAYLOAD_RETRY_ATTEMPTS`] (4): payload-fetch retries,
//!   so every redelivery has a KV-read slot.
//!
//! Backoff ([`retry_delay_secs`]) mirrors the payload-fetch schedule
//! (5/15/30/60 s): redeliveries stay spaced across the documented ~60 s KV
//! read lag instead of hot-looping. Exhaustion sends a [`DlqEntry`] — the
//! original message plus machine reason headers (error code, attempts) — to
//! [`DLQ_QUEUE_NAME`]; the operator redrives with [`redrive`] (see README).

use serde::{Deserialize, Serialize};

use crate::dispatch::PAYLOAD_RETRY_ATTEMPTS;
use crate::jobs::{can_retry, JobSource, JobState, MAX_ATTEMPTS};
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

/// Header carrying the machine error code on a [`DlqEntry`].
pub const DLQ_HEADER_ERROR_CODE: &str = "x-ingest-error-code";
/// Header carrying the spent delivery count on a [`DlqEntry`].
pub const DLQ_HEADER_ATTEMPTS: &str = "x-ingest-attempts";

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
/// (1-based): 5 s, 15 s, 30 s, then 60 s. Saturating, so any `u32` maps to a
/// sane delay and a corrupt attempt count can never hot-loop the queue.
pub fn retry_delay_secs(attempt: u32) -> u32 {
    match attempt {
        1 => 5,
        2 => 15,
        3 => 30,
        _ => MAX_RETRY_DELAY_SECS,
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

impl DlqEntry {
    /// Machine reason headers: error code + spent attempts. Asserted by the
    /// DLQ acceptance test; operators filter on these.
    pub fn reason_headers(&self) -> [(String, String); 2] {
        [
            (DLQ_HEADER_ERROR_CODE.into(), self.error_code.clone()),
            (DLQ_HEADER_ATTEMPTS.into(), self.attempts.to_string()),
        ]
    }
}

/// Decide one message's fate from its step result. Pure so the budget is
/// unit-tested, not mocked: success acks; a transient failure retries with
/// backoff while [`can_retry`] allows it and quarantines once the budget is
/// spent; a permanent failure (or a corrupt 0 attempt, which must never loop)
/// quarantines immediately.
pub fn decide(msg: &QueueMsg, result: Result<(), JobError>) -> Disposition {
    match result {
        Ok(()) => Disposition::Ack,
        Err(e) if !e.transient => Disposition::Quarantine(Box::new(quarantine(msg, &e))),
        Err(_) if can_retry(msg.attempt) => Disposition::Retry {
            delay_secs: retry_delay_secs(msg.attempt),
        },
        Err(e) => Disposition::Quarantine(Box::new(quarantine(msg, &e))),
    }
}

fn quarantine(msg: &QueueMsg, err: &JobError) -> DlqEntry {
    DlqEntry {
        message: msg.clone(),
        attempts: msg.attempt,
        error_code: err.code.to_string(),
        reason: format!(
            "{} after {} attempt(s): {}",
            err.code, msg.attempt, err.detail
        ),
    }
}

/// Re-enqueue a quarantined entry after the cause is fixed: same job, same
/// idempotency key, same input — fresh attempt and repair budgets, back at
/// `Queued`. A still-poison message burns the fresh budget and quarantines
/// again, so redrive is an operator action, not a loop.
pub fn redrive(entry: &DlqEntry) -> QueueMsg {
    let mut msg = entry.message.clone();
    msg.state = JobState::Queued;
    msg.attempt = 1;
    msg.repair_rounds = 0;
    msg
}

/// Redrive a raw [`QueueMsg`] that reached the DLQ through the native
/// dead-letter route (handler panic/timeout: Cloudflare moves the untouched
/// body with no reason envelope). Same fresh-budget reset as [`redrive`].
pub fn redrive_raw(msg: &QueueMsg) -> QueueMsg {
    let mut next = msg.clone();
    next.state = JobState::Queued;
    next.attempt = 1;
    next.repair_rounds = 0;
    next
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

/// Settle one batch without throwing: `run` verdicts each message and
/// [`decide`] maps the verdict to its disposition. The live handler mirrors
/// this loop with `ack()` / `retry_with_options()` / DLQ-send + `ack()`.
pub fn drive_batch<F>(msgs: &[QueueMsg], mut run: F) -> BatchReport
where
    F: FnMut(&QueueMsg) -> Result<(), JobError>,
{
    let mut report = BatchReport::default();
    for msg in msgs {
        match decide(msg, run(msg)) {
            Disposition::Ack => report.acked += 1,
            Disposition::Retry { .. } => report.retried += 1,
            Disposition::Quarantine(entry) => report.quarantined.push(*entry),
        }
    }
    report
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
    use crate::dispatch::PAYLOAD_KV_READ_LAG_MS;

    fn msg_at(attempt: u32) -> QueueMsg {
        let mut m = poison_fixture(POISON_USER);
        m.attempt = attempt;
        m
    }

    fn boom() -> Result<(), JobError> {
        Err(JobError::transient(
            ERR_PROBE_FAILED,
            "430 on every segment",
        ))
    }

    #[test]
    fn retry_delay_is_bounded_increasing_and_covers_kv_lag() {
        assert_eq!(retry_delay_secs(1), 5);
        assert_eq!(retry_delay_secs(2), 15);
        assert_eq!(retry_delay_secs(3), 30);
        assert_eq!(retry_delay_secs(4), MAX_RETRY_DELAY_SECS);
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
        assert!(matches!(decide(&msg_at(1), Ok(())), Disposition::Ack));
        assert!(matches!(
            decide(&msg_at(MAX_ATTEMPTS), Ok(())),
            Disposition::Ack
        ));
    }

    #[test]
    fn transient_failure_retries_with_backoff_until_the_budget_runs_out() {
        for attempt in 1..MAX_ATTEMPTS {
            match decide(&msg_at(attempt), boom()) {
                Disposition::Retry { delay_secs } => {
                    assert_eq!(delay_secs, retry_delay_secs(attempt))
                }
                d => panic!("attempt {attempt} must retry, got {d:?}"),
            }
        }
        match decide(&msg_at(MAX_ATTEMPTS), boom()) {
            Disposition::Quarantine(entry) => {
                assert_eq!(entry.attempts, MAX_ATTEMPTS);
                assert_eq!(entry.error_code, ERR_PROBE_FAILED);
            }
            d => panic!("spent budget must quarantine, got {d:?}"),
        }
    }

    #[test]
    fn permanent_failure_skips_the_budget() {
        let err = JobError::permanent(ERR_PARSE_FAILED, "not an NZB document");
        match decide(&msg_at(1), Err(err)) {
            Disposition::Quarantine(entry) => {
                assert_eq!(entry.attempts, 1);
                assert_eq!(entry.error_code, ERR_PARSE_FAILED);
            }
            d => panic!("permanent failure must quarantine at once, got {d:?}"),
        }
    }

    #[test]
    fn corrupt_zero_attempt_quarantines_instead_of_looping() {
        match decide(&msg_at(0), boom()) {
            Disposition::Quarantine(entry) => assert_eq!(entry.attempts, 0),
            d => panic!("attempt 0 must never retry, got {d:?}"),
        }
    }

    #[test]
    fn dlq_entry_roundtrips_with_reason_headers() {
        let entry = match decide(&msg_at(MAX_ATTEMPTS), boom()) {
            Disposition::Quarantine(entry) => entry,
            d => panic!("expected quarantine, got {d:?}"),
        };
        let back: DlqEntry = serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
        assert_eq!(back, *entry);
        let headers = back.reason_headers();
        assert_eq!(
            headers,
            [
                (
                    DLQ_HEADER_ERROR_CODE.to_string(),
                    ERR_PROBE_FAILED.to_string()
                ),
                (DLQ_HEADER_ATTEMPTS.to_string(), MAX_ATTEMPTS.to_string()),
            ]
        );
    }

    #[test]
    fn redrive_restores_a_queued_job_with_fresh_budgets() {
        let mut spent = msg_at(MAX_ATTEMPTS);
        spent.repair_rounds = crate::jobs::MAX_REPAIR_ROUNDS;
        let entry = match decide(&spent, boom()) {
            Disposition::Quarantine(entry) => entry,
            d => panic!("expected quarantine, got {d:?}"),
        };
        let again = redrive(&entry);
        assert_eq!(again.state, JobState::Queued);
        assert_eq!(again.attempt, 1);
        assert_eq!(again.repair_rounds, 0);
        assert_eq!(again.job_id, spent.job_id);
        assert_eq!(again.idempotency_key, spent.idempotency_key);
        let raw = redrive_raw(&spent);
        assert_eq!(raw.state, JobState::Queued);
        assert_eq!(raw.attempt, 1);
    }

    #[test]
    fn drive_batch_settles_per_message_and_never_throws() {
        let msgs = vec![msg_at(1), msg_at(1), msg_at(MAX_ATTEMPTS)];
        let report = drive_batch(&msgs, |m| {
            if m.attempt >= MAX_ATTEMPTS {
                boom()
            } else {
                Ok(())
            }
        });
        assert_eq!(report.acked, 2);
        assert_eq!(report.retried, 0);
        assert_eq!(report.quarantined.len(), 1);
        assert!(drive_batch(&[], |_| Ok(())).quarantined.is_empty());
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
