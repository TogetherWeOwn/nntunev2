//! Retry-budget + DLQ acceptance.
//!
//! An always-failing ("poison") job must burn its bounded retry budget with
//! backoff, then land in the DLQ carrying reason body fields (error code,
//! attempts) — while valid jobs in the same batch keep flowing. A redriven
//! entry must return to the queue with a fresh budget.

use nntune_ingest::dispatch::build_upload_dispatch;
use nntune_ingest::fixtures::SMALL_NZB;
use nntune_ingest::jobs::{JobCheckpoint, JobState, MAX_ATTEMPTS};
use nntune_ingest::queue::QueueMsg;
use nntune_ingest::retry::{
    decide_checkpoint, drive_batch, fail_checkpoint, redrive, redrive_checkpoint, redrive_raw,
    Disposition, JobError, ERR_PARSE_FAILED, ERR_PROBE_FAILED, MAX_DLQ_DETAIL_CHARS,
};

/// The poison fixture: a well-formed job whose processing *always* fails
/// (every segment 430s, no par2 set) — it can never succeed no matter how
/// often it is redelivered.
fn poison_msg() -> QueueMsg {
    build_upload_dispatch("poison_user", SMALL_NZB).unwrap()
}

fn valid_msg(user: &str) -> QueueMsg {
    build_upload_dispatch(user, SMALL_NZB).unwrap()
}

fn always_fails(_: &QueueMsg) -> Result<(), JobError> {
    Err(JobError::transient(
        ERR_PROBE_FAILED,
        "synthetic 430 on every segment, no par2 set",
    ))
}

fn checkpoint_for(msg: &QueueMsg) -> JobCheckpoint {
    JobCheckpoint {
        job_id: msg.job_id.clone(),
        user_id: msg.user_id.clone(),
        idempotency_key: msg.idempotency_key.clone(),
        source: msg.source,
        state: msg.state,
        attempt: 1,
        repair_rounds: msg.repair_rounds,
        probe_tally: None,
        par2_set_id: None,
        missing_blocks: 0,
        failure_code: None,
        failure_reason: None,
        provider_id: msg.provider_id.clone(),
    }
}

#[test]
fn poison_job_burns_durable_budget_on_unchanged_queue_body() {
    let msg = poison_msg();
    let original_body = serde_json::to_vec(&msg).unwrap();
    let mut checkpoint = checkpoint_for(&msg);
    let mut last = None;
    for delivery in 1..=MAX_ATTEMPTS {
        assert_eq!(checkpoint.attempt, delivery);
        assert_eq!(serde_json::to_vec(&msg).unwrap(), original_body);
        match decide_checkpoint(&msg, &mut checkpoint, always_fails(&msg)).unwrap() {
            Disposition::Retry { delay_secs } => {
                assert!(delivery < MAX_ATTEMPTS, "spent budget must quarantine");
                assert!(delay_secs >= 5, "redelivery must back off, not hot-loop");
                // The handler persists checkpoint.attempt before calling
                // retry_with_options; the Queue body is unchanged on delivery.
                assert_eq!(checkpoint.attempt, delivery + 1);
            }
            Disposition::Quarantine(entry) => {
                last = Some(entry);
                break;
            }
            Disposition::Ack => panic!("an always-failing job must never ack"),
            Disposition::Continue => {
                panic!("an always-failing job made no progress to continue on")
            }
        }
    }
    let entry = last.expect("poison must end in the DLQ after the budget");
    assert_eq!(entry.attempts, MAX_ATTEMPTS);
    assert_eq!(checkpoint.state, JobState::Failed);
    assert_eq!(checkpoint.failure_code.as_deref(), Some(ERR_PROBE_FAILED));
    assert_eq!(
        checkpoint.failure_reason.as_deref(),
        Some("synthetic 430 on every segment, no par2 set")
    );
    assert_eq!(entry.error_code, ERR_PROBE_FAILED);
    assert_eq!(entry.message.job_id, msg.job_id);
    let body = serde_json::to_value(&entry).unwrap();
    assert_eq!(body["error_code"], ERR_PROBE_FAILED);
    assert_eq!(body["attempts"], MAX_ATTEMPTS);
    assert!(entry.reason.contains(ERR_PROBE_FAILED));
}

#[test]
fn failed_checkpoint_redelivery_keeps_failure_code_in_dlq_entry() {
    let msg = poison_msg();
    let mut checkpoint = checkpoint_for(&msg);
    checkpoint.attempt = MAX_ATTEMPTS;
    fail_checkpoint(&mut checkpoint, ERR_PARSE_FAILED, "persisted parse failure");

    let report = drive_batch(&[msg], &mut [checkpoint], |_| {
        panic!("a failed checkpoint must not rerun processing")
    })
    .unwrap();

    assert_eq!(report.acked, 0);
    assert_eq!(report.retried, 0);
    assert_eq!(report.quarantined.len(), 1);
    let entry = &report.quarantined[0];
    assert_eq!(entry.error_code, ERR_PARSE_FAILED);
    assert_eq!(entry.attempts, MAX_ATTEMPTS);
    assert_eq!(entry.message.attempt, MAX_ATTEMPTS);
    assert!(entry.reason.contains("persisted parse failure"));
}

#[test]
fn quarantine_redacts_and_bounds_untrusted_error_details() {
    let msg = poison_msg();
    let mut checkpoint = checkpoint_for(&msg);
    let detail = format!(
        "transport failed https://indexer.invalid/api?apikey=secret&x=1 {}",
        "x".repeat(2_000)
    );
    let disposition = decide_checkpoint(
        &msg,
        &mut checkpoint,
        Err(JobError::permanent(ERR_PROBE_FAILED, detail)),
    )
    .unwrap();
    let Disposition::Quarantine(entry) = disposition else {
        panic!("permanent failure must quarantine");
    };
    assert!(!entry.reason.contains("secret"));
    assert!(entry.reason.chars().count() <= MAX_DLQ_DETAIL_CHARS + 64);
    let failure_reason = checkpoint.failure_reason.as_ref().unwrap();
    assert!(failure_reason.chars().count() <= MAX_DLQ_DETAIL_CHARS + 1);
    assert!(!failure_reason.contains("secret"));
}

#[test]
fn redrive_resets_checkpoint_budgets_and_terminal_fields() {
    let msg = poison_msg();
    let mut checkpoint = checkpoint_for(&msg);
    checkpoint.state = JobState::Failed;
    checkpoint.attempt = MAX_ATTEMPTS;
    checkpoint.repair_rounds = nntune_ingest::jobs::MAX_REPAIR_ROUNDS;
    checkpoint.failure_code = Some(ERR_PROBE_FAILED.into());
    checkpoint.failure_reason = Some("previous failure".into());
    checkpoint.probe_tally = Some(nntune_ingest::probe::ProbeTally::new(2));
    let identity = (
        checkpoint.job_id.clone(),
        checkpoint.user_id.clone(),
        checkpoint.idempotency_key.clone(),
    );
    redrive_checkpoint(&mut checkpoint);
    assert_eq!(checkpoint.state, JobState::Queued);
    assert_eq!(checkpoint.attempt, 1);
    assert_eq!(checkpoint.repair_rounds, 0);
    assert!(checkpoint.failure_code.is_none());
    assert!(checkpoint.failure_reason.is_none());
    assert!(checkpoint.probe_tally.is_none());
    assert_eq!(
        identity,
        (
            checkpoint.job_id.clone(),
            checkpoint.user_id.clone(),
            checkpoint.idempotency_key.clone()
        )
    );
}

#[test]
fn poison_does_not_block_valid_jobs_behind_it() {
    // Poison sits at the head of the batch; valid jobs behind it still ack.
    // First delivery: poison retries, valids ack — nothing is quarantined yet
    // and no batch-level throw wedges the queue.
    let msgs = vec![poison_msg(), valid_msg("user_a"), valid_msg("user_b")];
    let mut checkpoints: Vec<_> = msgs.iter().map(checkpoint_for).collect();
    let report = drive_batch(&msgs, &mut checkpoints, |m| {
        if m.user_id == "poison_user" {
            always_fails(m)
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(report.acked, 2);
    assert_eq!(report.retried, 1);
    assert!(report.quarantined.is_empty());

    // Last delivery: the poisoned attempt quarantines; the valids still ack.
    let mut last_round = vec![valid_msg("user_a"), valid_msg("user_b")];
    let poison = poison_msg();
    last_round.insert(0, poison);
    let mut checkpoints: Vec<_> = last_round.iter().map(checkpoint_for).collect();
    checkpoints[0].attempt = MAX_ATTEMPTS;
    let report = drive_batch(&last_round, &mut checkpoints, |m| {
        if m.user_id == "poison_user" {
            always_fails(m)
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(report.acked, 2);
    assert_eq!(report.retried, 0);
    assert_eq!(report.quarantined.len(), 1);
    assert_eq!(report.quarantined[0].error_code, ERR_PROBE_FAILED);
}

#[test]
fn redrive_returns_a_quarantined_job_to_the_queue_with_fresh_budget() {
    let poison = poison_msg();
    let mut checkpoint = checkpoint_for(&poison);
    checkpoint.attempt = MAX_ATTEMPTS;
    checkpoint.repair_rounds = nntune_ingest::jobs::MAX_REPAIR_ROUNDS;
    let entry = match decide_checkpoint(&poison, &mut checkpoint, always_fails(&poison)).unwrap() {
        Disposition::Quarantine(entry) => entry,
        d => panic!("expected quarantine, got {d:?}"),
    };
    // Operator redrives after fixing the cause; reset and persist the
    // checkpoint before enqueuing the matching message.
    let again = redrive(&entry, &mut checkpoint).unwrap();
    assert_eq!(again.attempt, 1);
    assert_eq!(again.state, nntune_ingest::jobs::JobState::Queued);
    assert_eq!(checkpoint.attempt, again.attempt);
    assert_eq!(checkpoint.state, again.state);
    assert_eq!(checkpoint.repair_rounds, again.repair_rounds);
    assert_eq!(checkpoint.failure_reason, None);
    assert_eq!(again.job_id, poison.job_id);
    assert_eq!(again.idempotency_key, poison.idempotency_key);
    assert_eq!(again.payload_ref, poison.payload_ref);
    assert!(matches!(
        decide_checkpoint(&again, &mut checkpoint, Ok(())).unwrap(),
        Disposition::Ack
    ));

    // Native-DLQ fallback shape (raw message, no reason envelope) redrives too.
    let mut raw_checkpoint = checkpoint_for(&poison);
    raw_checkpoint.attempt = MAX_ATTEMPTS;
    raw_checkpoint.repair_rounds = nntune_ingest::jobs::MAX_REPAIR_ROUNDS;
    raw_checkpoint.state = JobState::Failed;
    raw_checkpoint.failure_reason = Some("native failure".into());
    let raw = redrive_raw(&poison, &mut raw_checkpoint).unwrap();
    assert_eq!(raw.attempt, 1);
    assert_eq!(raw.repair_rounds, 0);
    assert_eq!(raw_checkpoint.attempt, raw.attempt);
    assert_eq!(raw_checkpoint.repair_rounds, raw.repair_rounds);
    assert_eq!(raw_checkpoint.state, raw.state);
    assert_eq!(raw_checkpoint.failure_reason, None);
    assert_eq!(raw.job_id, poison.job_id);
}
