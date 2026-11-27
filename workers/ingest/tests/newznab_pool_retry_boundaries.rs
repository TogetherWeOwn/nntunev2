use nntune_ingest::checkpoint;
use nntune_ingest::dispatch::build_upload_dispatch;
use nntune_ingest::fixtures::SMALL_NZB;
use nntune_ingest::jobs::{JobState, MAX_ATTEMPTS};
use nntune_ingest::newznab::{build_search_url, inject_api_key, SearchParams};
use nntune_ingest::pool_body::{
    build_body_request, call_pool_body_with_deadline, classify_body_reply, PoolBodyError,
    POOL_BODY_DEADLINE_MS,
};
use nntune_ingest::pool_deadline::bounded_deadline_ms;
use nntune_ingest::pool_probe::{
    call_pool_probe_with_deadline, classify_reply, PoolProbeError, POOL_PROBE_DEADLINE_MS,
};
use nntune_ingest::probe::{ProbeOutcome, ProbeRequest, MAX_PROBE_SEGMENTS, MAX_SEGMENT_BYTES};
use nntune_ingest::retry::{decide_checkpoint, Disposition, JobError, ERR_PROBE_FAILED};
use worker::wasm_bindgen::{JsCast, JsValue};

#[test]
fn newznab_rejects_malformed_authorities_and_ambiguous_key_targets() {
    for base_url in [
        "https://:443/api",
        "https://indexer.example:bogus/api",
        "https://%69ndexer.example/api",
        "https://indexer.example\\api",
        "https://indexer.example/api\0",
    ] {
        assert!(build_search_url(&SearchParams {
            base_url,
            query: "artist",
            categories: &[],
            limit: 25,
            offset: 0,
        })
        .is_err());
    }

    for url in [
        "http://indexer.example/api?q=x",
        "https://indexer.example/api",
        "https://indexer.example/api?q=x#fragment",
        "https://indexer.example/api?q=x&%41PIKEY=old",
    ] {
        assert!(inject_api_key(url, "synthetic&key").is_err(), "{url}");
    }
    assert_eq!(
        inject_api_key("https://indexer.example:8443/api?q=x", "synthetic&key"),
        Ok("https://indexer.example:8443/api?q=x&apikey=synthetic%26key".into())
    );
}

#[test]
fn body_status_alone_does_not_mark_an_article_unavailable() {
    let trailing = br#"{"error":"too_large"} trailing"#;
    for invalid in [b"".as_slice(), b"<html>too large</html>", trailing] {
        assert_eq!(
            classify_body_reply(413, None, invalid),
            Err(PoolBodyError::Retry { after_secs: 5 })
        );
    }
}

#[tokio::test]
async fn deadline_overrides_can_only_shorten_the_pool_budget() {
    for budget in [POOL_BODY_DEADLINE_MS, POOL_PROBE_DEADLINE_MS] {
        assert_eq!(bounded_deadline_ms(0, budget), None);
        assert_eq!(bounded_deadline_ms(1, budget), Some(1));
        assert_eq!(bounded_deadline_ms(budget, budget), Some(budget));
        assert_eq!(bounded_deadline_ms(u64::MAX, budget), Some(budget));
    }
    assert_eq!(
        bounded_deadline_ms(u64::MAX, u64::MAX),
        Some(i32::MAX as u64)
    );
    // Any attempt to use a Worker binding fails on this native test target.
    let bindings = JsValue::UNDEFINED;
    let env = bindings.unchecked_ref::<worker::Env>();
    let probe = probe_request(1, None);
    let msg = build_upload_dispatch("offline-user", SMALL_NZB).unwrap();
    let body = build_body_request(&msg, &probe.segment_ids[0]).unwrap();
    assert_eq!(
        call_pool_body_with_deadline(env, &body, 0).await,
        Err(PoolBodyError::Retry { after_secs: 5 })
    );
    assert_eq!(
        call_pool_probe_with_deadline(env, &probe, 0).await,
        Err(PoolProbeError::Retry { after_secs: 5 })
    );
}

fn probe_request(count: usize, bytes: Option<u64>) -> ProbeRequest {
    ProbeRequest {
        user_id: "offline-user".into(),
        job_id: "offline-job".into(),
        idempotency_key: "offline-key".into(),
        batch_index: 0,
        segment_ids: vec!["segment@synthetic.invalid".into(); count],
        segment_bytes: vec![bytes; count],
    }
}

#[test]
fn a_matching_probe_reply_cannot_validate_an_invalid_request() {
    let echo = |request: &ProbeRequest| ProbeOutcome {
        batch_index: request.batch_index,
        present: request.segment_ids.clone(),
        missing: vec![],
    };
    let mut unpaired = probe_request(1, None);
    unpaired.segment_bytes.clear();
    let mut invalid_id = probe_request(1, None);
    invalid_id.segment_ids[0] = "invalid id".into();
    for request in [
        probe_request(0, None),
        probe_request(MAX_PROBE_SEGMENTS + 1, None),
        probe_request(1, Some(0)),
        probe_request(1, Some(MAX_SEGMENT_BYTES + 1)),
        unpaired,
        invalid_id,
    ] {
        let body = serde_json::to_string(&echo(&request)).unwrap();
        assert_eq!(
            classify_reply(&request, 200, None, &body),
            Err(PoolProbeError::Rejected)
        );
    }
    let request = probe_request(MAX_PROBE_SEGMENTS, Some(MAX_SEGMENT_BYTES));
    let outcome = echo(&request);
    let body = serde_json::to_string(&outcome).unwrap();
    assert_eq!(classify_reply(&request, 200, None, &body), Ok(outcome));
}

#[test]
fn pool_retry_header_caps_overflow_and_rejects_malformed_deltas() {
    let request = probe_request(1, None);
    for (header, after_secs) in [
        ("18446744073709551616", 300),
        ("99999999999999999999x", 5),
        ("+17", 5),
        ("\t17 ", 17),
    ] {
        assert_eq!(
            classify_body_reply(503, Some(header), &[]),
            Err(PoolBodyError::Retry { after_secs })
        );
        assert_eq!(
            classify_reply(&request, 503, Some(header), ""),
            Err(PoolProbeError::Retry { after_secs })
        );
    }
}

#[test]
fn progress_can_continue_only_from_a_valid_attempt() {
    let msg = build_upload_dispatch("offline-user", SMALL_NZB).unwrap();
    for attempt in [0, MAX_ATTEMPTS + 1, u32::MAX] {
        let mut cp = checkpoint::initial(&msg);
        cp.attempt = attempt;
        let progress = JobError::progress(ERR_PROBE_FAILED, "offline progress");
        let Ok(Disposition::Quarantine(entry)) = decide_checkpoint(&msg, &mut cp, Err(progress))
        else {
            panic!("attempt {attempt} must quarantine");
        };
        assert_eq!(entry.attempts, attempt);
        assert_eq!(cp.state, JobState::Failed);
    }
}
