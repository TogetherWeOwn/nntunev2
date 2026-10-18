use nntune_ingest::jobs::{JobSource, JobState};
use nntune_ingest::pool_body::{
    build_body_request, classify_body_reply, BodyFetch, PoolBodyError, DEFAULT_RETRY_SECS,
    MAX_BODY_REPLY_BYTES, POOL_BODY_URL,
};
use nntune_ingest::queue::QueueMsg;

fn msg() -> QueueMsg {
    QueueMsg {
        job_id: "job-1".to_owned(),
        user_id: "user-1".to_owned(),
        idempotency_key: "key-1".to_owned(),
        source: JobSource::NzbUpload,
        state: JobState::Queued,
        attempt: 1,
        repair_rounds: 0,
        query: None,
        indexer_ref: None,
        categories: Vec::new(),
        limit: None,
        page: None,
        payload_ref: Some("nzb:sha256:abc".to_owned()),
        trace_id: None,
    }
}

#[test]
fn body_request_carries_job_identity_and_validates_the_id() {
    let req = build_body_request(&msg(), "seg-one@synthetic.invalid").expect("valid id");
    assert_eq!(req.job_id, "job-1");
    assert_eq!(req.user_id, "user-1");
    assert_eq!(req.idempotency_key, "key-1");
    assert_eq!(req.segment_id, "seg-one@synthetic.invalid");
    // The pool interpolates the id into `BODY <id>`: injection shapes must
    // never leave this crate.
    assert!(build_body_request(&msg(), "x@y>\r\nQUIT\r\n<z").is_err());
    assert_eq!(POOL_BODY_URL, "https://nntp-pool/body");
}

#[test]
fn successful_reply_returns_the_raw_bytes() {
    assert_eq!(
        classify_body_reply(200, None, b"=ybegin part=1 line=128 size=3 name=x\nabc"),
        Ok(BodyFetch::Bytes(
            b"=ybegin part=1 line=128 size=3 name=x\nabc".to_vec()
        ))
    );
}

#[test]
fn missing_article_reply_is_an_outcome_not_an_error() {
    assert_eq!(
        classify_body_reply(404, None, br#"{"error":"missing"}"#),
        Ok(BodyFetch::Missing)
    );
}

#[test]
fn route_miss_404_is_not_classified_as_missing() {
    // A stream deployment without the `/body` route answers 404 with the
    // plain `not found` body: that is version skew, not a provider 430,
    // and must retry instead of counting the article as gone.
    assert_eq!(
        classify_body_reply(404, None, b"not found"),
        Err(PoolBodyError::Retry {
            after_secs: DEFAULT_RETRY_SECS,
        })
    );
}

#[test]
fn busy_and_provider_replies_retry_with_the_hint() {
    assert_eq!(
        classify_body_reply(503, None, br#"{"error":"provider_unavailable"}"#),
        Err(PoolBodyError::Retry {
            after_secs: DEFAULT_RETRY_SECS,
        })
    );
    assert_eq!(
        classify_body_reply(503, Some("17"), br#"{"error":"busy"}"#),
        Err(PoolBodyError::Retry { after_secs: 17 })
    );
    assert_eq!(
        classify_body_reply(502, None, br#"{"error":"provider_error"}"#),
        Err(PoolBodyError::Retry {
            after_secs: DEFAULT_RETRY_SECS,
        })
    );
}

#[test]
fn refused_and_oversize_replies_are_rejected() {
    assert_eq!(
        classify_body_reply(400, None, br#"{"error":"bad_request"}"#),
        Err(PoolBodyError::Rejected)
    );
    // `413` is per-article data (the segment is judged absent), never
    // a rejected fetch that would permanently fail the whole job.
    assert_eq!(
        classify_body_reply(413, None, br#"{"error":"too_large"}"#),
        Ok(BodyFetch::TooLarge)
    );
    let oversize = vec![0u8; MAX_BODY_REPLY_BYTES + 1];
    assert_eq!(
        classify_body_reply(200, None, &oversize),
        Err(PoolBodyError::Rejected)
    );
}
