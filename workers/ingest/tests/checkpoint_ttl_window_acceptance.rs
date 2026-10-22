//! Checkpoint TTL and same-key write-window pins (tests-only).
//!
//! Contract under test (`workers/ingest/src/checkpoint.rs:32,34,166`):
//!
//! - `CHECKPOINT_TTL_SECS == 2_592_000` (30 days), applied as the
//!   `expiration_ttl` on every KV write behind the checkpoint writer contract;
//! - `KV_SAME_KEY_WRITE_WINDOW_MS == 1_000` (Workers KV's 1 write per second
//!   per key), waited out by the consumer before its one planned retry.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - lowering `CHECKPOINT_TTL_SECS` shortens retention, failing
//!   `checkpoint_ttl_is_30_days`;
//! - lowering `KV_SAME_KEY_WRITE_WINDOW_MS` narrows the write discipline,
//!   failing `same_key_write_window_is_one_second`.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test checkpoint_ttl_window_acceptance
//! ```

use nntune_ingest::checkpoint::{CHECKPOINT_TTL_SECS, KV_SAME_KEY_WRITE_WINDOW_MS};

#[test]
fn checkpoint_ttl_is_30_days() {
    assert_eq!(
        CHECKPOINT_TTL_SECS, 2_592_000,
        "checkpoint TTL must stay exactly 30 days (2_592_000s)"
    );
}

#[test]
fn same_key_write_window_is_one_second() {
    assert_eq!(
        KV_SAME_KEY_WRITE_WINDOW_MS, 1_000,
        "same-key write window must stay exactly 1s (1_000ms)"
    );
}
