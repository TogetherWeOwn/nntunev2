//! Dispatch payload TTL and retry-schedule agreement pins (tests-only).
//!
//! Contract under test (`workers/ingest/src/dispatch.rs:35,41,46,51,61-67`):
//!
//! - `PAYLOAD_TTL_SECS == 2_592_000` (30 days, `30 * 24 * 3_600`);
//! - `payload_retry_delay_ms(0..4) == [5_000, 15_000, 30_000, 60_000]`;
//! - saturation: any attempt beyond 3 maps to `MAX_PAYLOAD_RETRY_DELAY_MS`;
//! - `MAX_PAYLOAD_RETRY_DELAY_MS == PAYLOAD_KV_READ_LAG_MS` (60 s agreement,
//!   so the tail retry lands after the cached miss has expired);
//! - cumulative delay across `PAYLOAD_RETRY_ATTEMPTS` covers the documented
//!   `PAYLOAD_KV_READ_LAG_MS` (5 + 15 + 30 + 60 = 110 s >= 60 s).
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - changing the TTL expression (e.g. 7 days) fails
//!   `payload_ttl_secs_is_thirty_days` — the existing in-module pin at
//!   `dispatch.rs:358` only checks `>= 7 days`, so a 7-day regression passes
//!   there and dies here;
//! - changing any schedule entry (e.g. 10 s first wait) fails
//!   `payload_retry_schedule_is_5_15_30_60`;
//! - dropping the saturating tail (e.g. returning 0 past attempt 3) fails
//!   `retry_delay_saturates_at_ceiling_beyond_attempt_3`;
//! - raising/lowering either side of the 60 s agreement fails
//!   `max_delay_agrees_with_documented_kv_lag`;
//! - shrinking the schedule or the attempt budget below the lag fails
//!   `cumulative_delay_covers_documented_kv_lag`.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test dispatch_payload_policy_acceptance
//! ```

use nntune_ingest::dispatch::{
    payload_retry_delay_ms, MAX_PAYLOAD_RETRY_DELAY_MS, PAYLOAD_KV_READ_LAG_MS,
    PAYLOAD_RETRY_ATTEMPTS, PAYLOAD_TTL_SECS,
};

#[test]
fn payload_ttl_secs_is_thirty_days() {
    assert_eq!(
        PAYLOAD_TTL_SECS, 2_592_000,
        "payload KV TTL must stay exactly 30 days (30 * 24 * 3_600)"
    );
}

#[test]
fn payload_retry_schedule_is_5_15_30_60() {
    assert_eq!(
        [
            payload_retry_delay_ms(0),
            payload_retry_delay_ms(1),
            payload_retry_delay_ms(2),
            payload_retry_delay_ms(3),
        ],
        [5_000, 15_000, 30_000, 60_000],
        "payload retry schedule must stay exactly 5s/15s/30s/60s"
    );
}

#[test]
fn retry_delay_saturates_at_ceiling_beyond_attempt_3() {
    for attempt in 3..32 {
        assert_eq!(
            payload_retry_delay_ms(attempt),
            MAX_PAYLOAD_RETRY_DELAY_MS,
            "attempt {attempt} must saturate at the ceiling"
        );
    }
    assert_eq!(
        payload_retry_delay_ms(u32::MAX),
        MAX_PAYLOAD_RETRY_DELAY_MS,
        "a hostile attempt magnitude must clamp to the ceiling, not escape or wrap"
    );
    assert_eq!(
        payload_retry_delay_ms(u32::MAX),
        60_000,
        "the saturated tail must stay exactly 60s"
    );
}

#[test]
fn max_delay_agrees_with_documented_kv_lag() {
    assert_eq!(
        MAX_PAYLOAD_RETRY_DELAY_MS, PAYLOAD_KV_READ_LAG_MS,
        "tail-retry ceiling must agree with the documented KV read lag"
    );
    assert_eq!(
        MAX_PAYLOAD_RETRY_DELAY_MS, 60_000,
        "the agreed ceiling/lag value must stay exactly 60s"
    );
}

#[test]
fn cumulative_delay_covers_documented_kv_lag() {
    let total: u64 = (0..PAYLOAD_RETRY_ATTEMPTS)
        .map(payload_retry_delay_ms)
        .sum();
    assert_eq!(
        total, 110_000,
        "cumulative payload retry window must stay exactly 5 + 15 + 30 + 60 = 110s"
    );
    assert!(
        total >= PAYLOAD_KV_READ_LAG_MS,
        "payload retry window {total}ms must cover KV lag {PAYLOAD_KV_READ_LAG_MS}ms"
    );
}
