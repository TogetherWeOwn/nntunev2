//! Pool retry-after default and ceiling pins (tests-only).
//!
//! Contract under test (`workers/ingest/src/pool_probe.rs:23,26` +
//! `classify_reply`; `workers/ingest/src/pool_body.rs:28,31` +
//! `classify_body_reply`):
//!
//! - `DEFAULT_RETRY_SECS == 5` and `MAX_RETRY_SECS == 300` on BOTH the probe
//!   and body paths (the two modules each declare their own pair);
//! - a missing or unparsable `Retry-After` yields exactly 5s;
//! - an excessive value (e.g. `3600`) clamps to exactly 300s;
//! - `0` clamps to exactly 1s.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - changing either `DEFAULT_RETRY_SECS` (e.g. `5` -> `6`) fails the literal
//!   pin and every missing/garbage case on that path;
//! - changing either `MAX_RETRY_SECS` (e.g. `300` -> `301`) fails the literal
//!   pin and the excessive-value clamp on that path;
//! - dropping the lower clamp (`clamp(1, ..)`) lets `0` yield a `0`-second
//!   retry instead of `1`, failing the zero-floor cases;
//! - dropping the upper clamp lets `3600` escape `300`, failing the
//!   excessive-value cases.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked -p nntune-ingest --test pool_retry_clamp_acceptance
//! ```

use nntune_ingest::pool_body::{
    classify_body_reply, PoolBodyError as BodyError, DEFAULT_RETRY_SECS as BODY_DEFAULT,
    MAX_RETRY_SECS as BODY_MAX,
};
use nntune_ingest::pool_probe::{
    classify_reply, PoolProbeError as ProbeError, DEFAULT_RETRY_SECS as PROBE_DEFAULT,
    MAX_RETRY_SECS as PROBE_MAX,
};
use nntune_ingest::probe::ProbeRequest;

fn request() -> ProbeRequest {
    ProbeRequest {
        job_id: "job-1".to_owned(),
        user_id: "user-1".to_owned(),
        idempotency_key: "key-1".to_owned(),
        batch_index: 3,
        segment_ids: vec![
            "segment-a@synthetic.invalid".to_owned(),
            "segment-b@synthetic.invalid".to_owned(),
        ],
        segment_bytes: vec![None, None],
    }
}

fn probe_after_secs(retry_after: Option<&str>) -> u32 {
    match classify_reply(&request(), 503, retry_after, r#"{"error":"busy"}"#) {
        Err(ProbeError::Retry { after_secs }) => after_secs,
        other => panic!("503 with Retry-After {retry_after:?} must retry, got {other:?}"),
    }
}

fn body_after_secs(retry_after: Option<&str>) -> u32 {
    match classify_body_reply(503, retry_after, br#"{"error":"busy"}"#) {
        Err(BodyError::Retry { after_secs }) => after_secs,
        other => panic!("503 with Retry-After {retry_after:?} must retry, got {other:?}"),
    }
}

#[test]
fn default_and_ceiling_literals_are_5_and_300_on_both_paths() {
    assert_eq!(PROBE_DEFAULT, 5, "probe default must stay exactly 5s");
    assert_eq!(PROBE_MAX, 300, "probe ceiling must stay exactly 300s");
    assert_eq!(BODY_DEFAULT, 5, "body default must stay exactly 5s");
    assert_eq!(BODY_MAX, 300, "body ceiling must stay exactly 300s");
}

#[test]
fn probe_missing_or_garbage_retry_after_yields_default() {
    // Missing, empty, non-numeric, negative and trailing-junk headers all
    // fail the `u32` parse and must fall back to exactly 5s.
    for header in [
        None,
        Some(""),
        Some("   "),
        Some("soon"),
        Some("-1"),
        Some("12x"),
    ] {
        assert_eq!(
            probe_after_secs(header),
            5,
            "probe Retry-After {header:?} must yield exactly 5s"
        );
    }
}

#[test]
fn body_missing_or_garbage_retry_after_yields_default() {
    for header in [
        None,
        Some(""),
        Some("   "),
        Some("soon"),
        Some("-1"),
        Some("12x"),
    ] {
        assert_eq!(
            body_after_secs(header),
            5,
            "body Retry-After {header:?} must yield exactly 5s"
        );
    }
}

#[test]
fn probe_excessive_clamps_to_ceiling_and_zero_floors_to_one() {
    // In-range control: the clamp passes ordinary values through untouched.
    assert_eq!(
        probe_after_secs(Some("17")),
        17,
        "in-range hint must pass through"
    );
    assert_eq!(
        probe_after_secs(Some("3600")),
        300,
        "excessive probe Retry-After must clamp to exactly 300s"
    );
    assert_eq!(
        probe_after_secs(Some("0")),
        1,
        "zero probe Retry-After must floor to exactly 1s"
    );
}

#[test]
fn body_excessive_clamps_to_ceiling_and_zero_floors_to_one() {
    assert_eq!(
        body_after_secs(Some("17")),
        17,
        "in-range hint must pass through"
    );
    assert_eq!(
        body_after_secs(Some("3600")),
        300,
        "excessive body Retry-After must clamp to exactly 300s"
    );
    assert_eq!(
        body_after_secs(Some("0")),
        1,
        "zero body Retry-After must floor to exactly 1s"
    );
}
