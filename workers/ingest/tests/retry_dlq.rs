//! Retry-budget + DLQ acceptance (TOG-25372).
//!
//! An always-failing ("poison") job must burn its bounded retry budget with
//! backoff, then land in the DLQ carrying reason headers (error code,
//! attempts) — while valid jobs in the same batch keep flowing. A redriven
//! entry must return to the queue with a fresh budget.

use nntune_ingest::dispatch::build_upload_dispatch;
use nntune_ingest::fixtures::SMALL_NZB;
use nntune_ingest::jobs::MAX_ATTEMPTS;
use nntune_ingest::queue::QueueMsg;
use nntune_ingest::retry::{
    decide, drive_batch, redrive, redrive_raw, Disposition, JobError, ERR_PROBE_FAILED,
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

#[test]
fn poison_job_burns_the_budget_then_quarantines_with_reason_headers() {
    let mut msg = poison_msg();
    let mut last = None;
    for attempt in 1..=MAX_ATTEMPTS {
        assert_eq!(msg.attempt, attempt, "redelivery must bump the attempt");
        match decide(&msg, always_fails(&msg)) {
            Disposition::Retry { delay_secs } => {
                assert!(
                    (attempt as usize) < MAX_ATTEMPTS as usize,
                    "budget left means retry, never quarantine"
                );
                assert!(delay_secs >= 5, "redelivery must back off, not hot-loop");
                msg.attempt += 1;
            }
            Disposition::Quarantine(entry) => {
                last = Some(entry);
                break;
            }
            Disposition::Ack => panic!("an always-failing job must never ack"),
        }
    }
    let entry = last.expect("poison must end in the DLQ after the budget");
    assert_eq!(entry.attempts, MAX_ATTEMPTS);
    assert_eq!(entry.error_code, ERR_PROBE_FAILED);
    assert_eq!(entry.message.job_id, msg.job_id);
    let headers = entry.reason_headers();
    let code = headers
        .iter()
        .find(|(k, _)| k == "x-ingest-error-code")
        .expect("DLQ entry must carry the error-code header");
    assert_eq!(code.1, ERR_PROBE_FAILED);
    let attempts = headers
        .iter()
        .find(|(k, _)| k == "x-ingest-attempts")
        .expect("DLQ entry must carry the attempts header");
    assert_eq!(attempts.1, MAX_ATTEMPTS.to_string());
    assert!(
        entry.reason.contains(ERR_PROBE_FAILED),
        "human reason must name the code"
    );
}

#[test]
fn poison_does_not_block_valid_jobs_behind_it() {
    // Poison sits at the head of the batch; valid jobs behind it still ack.
    // First delivery: poison retries, valids ack — nothing is quarantined yet
    // and no batch-level throw wedges the queue.
    let msgs = vec![poison_msg(), valid_msg("user_a"), valid_msg("user_b")];
    let report = drive_batch(&msgs, |m| {
        if m.user_id == "poison_user" {
            always_fails(m)
        } else {
            Ok(())
        }
    });
    assert_eq!(report.acked, 2);
    assert_eq!(report.retried, 1);
    assert!(report.quarantined.is_empty());

    // Last delivery: the poisoned attempt quarantines; the valids still ack.
    let mut last_round = vec![valid_msg("user_a"), valid_msg("user_b")];
    let mut poison = poison_msg();
    poison.attempt = MAX_ATTEMPTS;
    last_round.insert(0, poison);
    let report = drive_batch(&last_round, |m| {
        if m.user_id == "poison_user" {
            always_fails(m)
        } else {
            Ok(())
        }
    });
    assert_eq!(report.acked, 2);
    assert_eq!(report.retried, 0);
    assert_eq!(report.quarantined.len(), 1);
    assert_eq!(report.quarantined[0].error_code, ERR_PROBE_FAILED);
}

#[test]
fn redrive_returns_a_quarantined_job_to_the_queue_with_fresh_budget() {
    let mut poison = poison_msg();
    poison.attempt = MAX_ATTEMPTS;
    let entry = match decide(&poison, always_fails(&poison)) {
        Disposition::Quarantine(entry) => entry,
        d => panic!("expected quarantine, got {d:?}"),
    };
    // Operator redrives after fixing the cause; the job resumes and succeeds.
    let again = redrive(&entry);
    assert_eq!(again.attempt, 1);
    assert_eq!(again.state, nntune_ingest::jobs::JobState::Queued);
    assert_eq!(again.job_id, poison.job_id);
    assert_eq!(again.idempotency_key, poison.idempotency_key);
    assert_eq!(again.payload_ref, poison.payload_ref);
    assert!(matches!(decide(&again, Ok(())), Disposition::Ack));

    // Native-DLQ fallback shape (raw message, no reason envelope) redrives too.
    let raw = redrive_raw(&poison);
    assert_eq!(raw.attempt, 1);
    assert_eq!(raw.job_id, poison.job_id);
}
