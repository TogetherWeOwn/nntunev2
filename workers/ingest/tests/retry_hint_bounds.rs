//! Retry-hint zero floor and hint-ceiling pins (tests-only).
//!
//! Contract under test (`workers/ingest/src/retry.rs:58,293-294`):
//!
//! - `MAX_HINTED_RETRY_DELAY_SECS == 300`;
//! - `decide_checkpoint` settles a transient failure as
//!   `delay_secs.max(hint.clamp(1, MAX_HINTED_RETRY_DELAY_SECS))`, where
//!   `delay_secs` is the computed backoff for the spent attempt and `hint`
//!   is the boundary-supplied `retry_after_secs` (e.g. the pool's
//!   `Retry-After`). A hint may only lengthen the backoff, never shorten it
//!   to zero or stretch it past the ceiling.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - dropping the lower clamp (`clamp(1, ..)`) lets hint `0` yield a
//!   `0`-second retry instead of the computed backoff, failing
//!   `zero_hint_yields_computed_backoff_never_zero`;
//! - lowering `MAX_HINTED_RETRY_DELAY_SECS` or dropping the upper clamp /
//!   the surrounding `max` lets a huge hint escape `300`, failing
//!   `huge_hint_clamps_to_ceiling_via_decide_checkpoint` and
//!   `hint_ceiling_literal_is_300`;
//! - replacing the `max` with the raw hint lets a short hint shorten the
//!   backoff, failing the short-hint control inside the zero-hint test.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-ingest --test retry_hint_bounds
//! ```

use nntune_ingest::dispatch::build_upload_dispatch;
use nntune_ingest::fixtures::SMALL_NZB;
use nntune_ingest::jobs::{JobCheckpoint, MAX_ATTEMPTS};
use nntune_ingest::queue::QueueMsg;
use nntune_ingest::retry::{
    decide_checkpoint, retry_delay_secs, Disposition, JobError, ERR_PROBE_FAILED,
    MAX_HINTED_RETRY_DELAY_SECS,
};

fn msg_at(attempt: u32) -> QueueMsg {
    let mut msg = build_upload_dispatch("hint_bounds_user", SMALL_NZB).unwrap();
    msg.attempt = attempt;
    msg
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
        par2_set_id: None,
        missing_blocks: 0,
        failure_code: None,
        failure_reason: None,
    }
}

fn retry_delay_for_hint(attempt: u32, hint: u32) -> u32 {
    let msg = msg_at(attempt);
    let mut cp = checkpoint_for(&msg);
    let err = JobError::transient_after(ERR_PROBE_FAILED, "pool busy", hint);
    match decide_checkpoint(&msg, &mut cp, Err(err)).unwrap() {
        Disposition::Retry { delay_secs } => delay_secs,
        d => panic!("attempt {attempt} hint {hint} must retry, got {d:?}"),
    }
}

#[test]
fn hint_ceiling_literal_is_300() {
    assert_eq!(
        MAX_HINTED_RETRY_DELAY_SECS, 300,
        "hint ceiling must stay exactly 300s"
    );
}

#[test]
fn zero_hint_yields_computed_backoff_never_zero() {
    // A `Retry-After: 0` (or a zeroed boundary hint) must floor to the
    // computed backoff, never a 0-second hot-loop retry.
    for attempt in 1..MAX_ATTEMPTS {
        let delay = retry_delay_for_hint(attempt, 0);
        assert_eq!(
            delay,
            retry_delay_secs(attempt),
            "zero hint at attempt {attempt} must yield the computed backoff"
        );
        assert!(
            delay > 0,
            "zero hint at attempt {attempt} must never yield a 0-second retry"
        );
    }
    // Short-hint control: a hint below the backoff likewise never shortens it.
    assert_eq!(
        retry_delay_for_hint(1, 2),
        retry_delay_secs(1),
        "short hint must not shorten the computed backoff"
    );
}

#[test]
fn huge_hint_clamps_to_ceiling_via_decide_checkpoint() {
    // Hostile magnitudes travel the public settlement path and clamp to the
    // ceiling instead of escaping it or wrapping.
    for hint in [301, 3_600, u32::MAX] {
        let delay = retry_delay_for_hint(1, hint);
        assert_eq!(
            delay, MAX_HINTED_RETRY_DELAY_SECS,
            "huge hint {hint} must clamp to the ceiling via decide_checkpoint"
        );
        assert_eq!(delay, 300, "huge hint {hint} must clamp to exactly 300s");
    }
}
