//! Retry-budget DLQ identity and delay-ceiling pins (tests-only).
//!
//! Contract under test (`workers/ingest/src/retry.rs:46,51,54,83-86`):
//!
//! - `DLQ_QUEUE_NAME == "nntune-ingest-dlq"` (quarantine queue identity);
//! - `CONSUMER_MAX_RETRIES == 4` (native redelivery slots, 1 + 4 deliveries);
//! - `MAX_RETRY_DELAY_SECS == 60` (longest single wait between redeliveries);
//! - `retry_delay_secs(u32::MAX) == MAX_RETRY_DELAY_SECS` (ceiling holds
//!   through the public path, via the saturating payload-fetch schedule).
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - renaming `DLQ_QUEUE_NAME` misroutes quarantine entries, failing
//!   `dlq_queue_name_is_nntune_ingest_dlq`;
//! - raising/lowering `CONSUMER_MAX_RETRIES` changes the native slot count,
//!   failing `consumer_max_retries_is_four` (the compile-time asserts at
//!   `retry.rs:78-79` check consistency with `MAX_ATTEMPTS`, not the value 4);
//! - raising/lowering `MAX_RETRY_DELAY_SECS` changes the longest wait, failing
//!   `max_retry_delay_secs_is_sixty` and `retry_delay_ceiling_holds_at_u32_max`;
//! - dropping the saturating ceiling in `payload_retry_delay_ms` lets a huge
//!   attempt escape `60`, failing `retry_delay_ceiling_holds_at_u32_max`.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test retry_budget_literals_acceptance
//! ```

use nntune_ingest::retry::{
    retry_delay_secs, CONSUMER_MAX_RETRIES, DLQ_QUEUE_NAME, MAX_RETRY_DELAY_SECS,
};

#[test]
fn dlq_queue_name_is_nntune_ingest_dlq() {
    assert_eq!(
        DLQ_QUEUE_NAME, "nntune-ingest-dlq",
        "quarantine queue identity must stay exactly nntune-ingest-dlq"
    );
}

#[test]
fn consumer_max_retries_is_four() {
    assert_eq!(
        CONSUMER_MAX_RETRIES, 4,
        "native redelivery slots must stay exactly 4 (1 + 4 deliveries)"
    );
}

#[test]
fn max_retry_delay_secs_is_sixty() {
    assert_eq!(
        MAX_RETRY_DELAY_SECS, 60,
        "longest single wait between redeliveries must stay exactly 60s"
    );
}

#[test]
fn retry_delay_ceiling_holds_at_u32_max() {
    // Hostile magnitude travels the public path and clamps to the ceiling
    // instead of escaping it or wrapping.
    assert_eq!(
        retry_delay_secs(u32::MAX),
        MAX_RETRY_DELAY_SECS,
        "retry_delay_secs(u32::MAX) must clamp to the ceiling via the public path"
    );
    assert_eq!(
        retry_delay_secs(u32::MAX),
        60,
        "retry_delay_secs(u32::MAX) must clamp to exactly 60s"
    );
}
