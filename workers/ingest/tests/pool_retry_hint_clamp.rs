//! Pin of the pool `Retry-After` hint clamp bounds (refill-19, tests-only).
//!
//! Both `classify_reply` (probe) and `classify_body_reply` (body) clamp a
//! parsed hint into `[1, 300]` and fall back to the default (`5`) when the
//! hint is missing or unparsable. This file pins that behavior; the
//! implementation stays untouched.

use nntune_ingest::pool_body::{classify_body_reply, PoolBodyError};
use nntune_ingest::pool_body::{DEFAULT_RETRY_SECS as BODY_DEFAULT, MAX_RETRY_SECS as BODY_MAX};
use nntune_ingest::pool_probe::{
    classify_reply, PoolProbeError, DEFAULT_RETRY_SECS as PROBE_DEFAULT,
    MAX_RETRY_SECS as PROBE_MAX,
};
use nntune_ingest::probe::ProbeRequest;

fn probe_request() -> ProbeRequest {
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

#[test]
fn probe_zero_hint_clamps_to_one() {
    let req = probe_request();
    assert_eq!(
        classify_reply(&req, 503, Some("0"), ""),
        Err(PoolProbeError::Retry { after_secs: 1 })
    );
}

#[test]
fn probe_large_hints_clamp_to_max() {
    let req = probe_request();
    assert_eq!(PROBE_MAX, 300);
    for hint in ["3600", &u32::MAX.to_string()] {
        assert_eq!(
            classify_reply(&req, 503, Some(hint), ""),
            Err(PoolProbeError::Retry { after_secs: 300 }),
            "hint {hint:?} must clamp to 300"
        );
    }
}

#[test]
fn probe_garbage_hints_fall_back_to_default() {
    let req = probe_request();
    assert_eq!(PROBE_DEFAULT, 5);
    for hint in ["soon", "-5", "  "] {
        assert_eq!(
            classify_reply(&req, 503, Some(hint), ""),
            Err(PoolProbeError::Retry {
                after_secs: PROBE_DEFAULT,
            }),
            "hint {hint:?} must fall back to default 5"
        );
    }
}

#[test]
fn probe_control_hint_passes_through() {
    let req = probe_request();
    assert_eq!(
        classify_reply(&req, 503, Some("17"), ""),
        Err(PoolProbeError::Retry { after_secs: 17 })
    );
}

#[test]
fn body_zero_hint_clamps_to_one() {
    assert_eq!(
        classify_body_reply(503, Some("0"), b"busy"),
        Err(PoolBodyError::Retry { after_secs: 1 })
    );
}

#[test]
fn body_large_hints_clamp_to_max() {
    assert_eq!(BODY_MAX, 300);
    for hint in ["3600", &u32::MAX.to_string()] {
        assert_eq!(
            classify_body_reply(503, Some(hint), b"busy"),
            Err(PoolBodyError::Retry { after_secs: 300 }),
            "hint {hint:?} must clamp to 300"
        );
    }
}

#[test]
fn body_garbage_hints_fall_back_to_default() {
    assert_eq!(BODY_DEFAULT, 5);
    for hint in ["soon", "-5", "  "] {
        assert_eq!(
            classify_body_reply(503, Some(hint), b"busy"),
            Err(PoolBodyError::Retry {
                after_secs: BODY_DEFAULT,
            }),
            "hint {hint:?} must fall back to default 5"
        );
    }
}

#[test]
fn body_control_hint_passes_through() {
    assert_eq!(
        classify_body_reply(503, Some("17"), b"busy"),
        Err(PoolBodyError::Retry { after_secs: 17 })
    );
}
