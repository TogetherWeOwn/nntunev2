use nntune_ingest::pool_probe::{classify_reply, PoolProbeError, DEFAULT_RETRY_SECS};
use nntune_ingest::probe::{ProbeOutcome, ProbeRequest};

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
    }
}

#[test]
fn successful_reply_preserves_batch_and_partitions_requested_ids() {
    let req = request();
    let body = serde_json::json!({
        "batch_index": req.batch_index,
        "present": [req.segment_ids[0]],
        "missing": [req.segment_ids[1]],
    })
    .to_string();

    let outcome = classify_reply(&req, 200, None, &body).expect("valid pool reply");

    assert_eq!(
        outcome,
        ProbeOutcome {
            batch_index: req.batch_index,
            present: vec![req.segment_ids[0].clone()],
            missing: vec![req.segment_ids[1].clone()],
        }
    );
}

#[test]
fn provider_unavailable_reply_retries_without_a_healthy_outcome() {
    let req = request();

    assert_eq!(
        classify_reply(&req, 503, None, r#"{"error":"provider_unavailable"}"#),
        Err(PoolProbeError::Retry {
            after_secs: DEFAULT_RETRY_SECS,
        })
    );
}

#[test]
fn provider_error_reply_retries_without_a_healthy_outcome() {
    let req = request();

    assert_eq!(
        classify_reply(&req, 502, None, r#"{"error":"provider_error"}"#),
        Err(PoolProbeError::Retry {
            after_secs: DEFAULT_RETRY_SECS,
        })
    );
}

#[test]
fn malformed_or_incomplete_success_reply_is_rejected() {
    let req = request();

    for body in [
        "not json",
        r#"{"batch_index":3,"present":[],"missing":[]}"#,
        r#"{"batch_index":3,"present":["segment-a@synthetic.invalid"],"missing":[]}"#,
    ] {
        assert_eq!(
            classify_reply(&req, 200, None, body),
            Err(PoolProbeError::Rejected),
            "unexpectedly accepted response: {body}"
        );
    }
}
