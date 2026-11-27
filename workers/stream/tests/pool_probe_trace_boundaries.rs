use std::{cell::RefCell, io};

use nntune_core::{nntp::NntpClient, transport::Transport};
use nntune_stream::{
    delivered::DeliveredInterval,
    meter_sink::{batch_body, parse_ack, MeterBook, MAX_BATCH_BODY_BYTES},
    observability::{CacheObserveEvent, CacheStatus},
    pool::{CircuitState, Lane, Pool, CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_OPEN_MS},
    probe::{run_probe, ProbeError, ProbeRequest, MAX_PROBE_SEGMENTS},
    trace::{LogFields, REDACTED},
};

#[test]
fn foreign_release_and_superseded_probe_cannot_open_admission() {
    let mut pool = Pool::new(3);
    let stale = pool.try_acquire(Lane::Playback).unwrap();
    let mut other = Pool::<()>::new(1);
    pool.release(other.try_acquire(Lane::Playback).unwrap(), Some(()));
    assert_eq!((pool.in_use(), pool.idle(), other.in_use()), (1, 0, 0));

    for _ in 0..CIRCUIT_FAILURE_THRESHOLD {
        pool.record_provider_error();
    }
    pool.advance(CIRCUIT_OPEN_MS);
    let replaced = pool.try_acquire(Lane::Ingest).unwrap();
    pool.record_provider_error_for(stale.id);
    assert!(!pool.record_probe_success_for(replaced.id));
    assert_eq!(pool.circuit_state(), CircuitState::Open);
    pool.advance(CIRCUIT_OPEN_MS);
    let probe = pool.try_acquire(Lane::Ingest).unwrap();
    drop(replaced);
    assert!(!pool.can_acquire(Lane::Playback));
    assert!(pool.record_probe_success_for(probe.id));
}

struct TimeoutAfterHandshake(bool);

impl Transport for TimeoutAfterHandshake {
    async fn read(&mut self, mut buf: &mut [u8]) -> io::Result<usize> {
        if std::mem::replace(&mut self.0, false) {
            io::Write::write(&mut buf, b"200 ready\r\n200 reader mode\r\n")
        } else {
            Err(io::ErrorKind::TimedOut.into())
        }
    }
    async fn write_all(&mut self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
    async fn shutdown(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn typed_probe_refuses_bad_batches_and_never_redials_a_timeout() {
    let pool = RefCell::new(Pool::new(2));
    let client = NntpClient::connect(TimeoutAfterHandshake(true), &Default::default())
        .await
        .unwrap();
    let lease = pool.borrow_mut().try_acquire(Lane::Ingest).unwrap();
    pool.borrow_mut().release(lease, Some(client));
    let mut req: ProbeRequest = serde_json::from_str(
        r#"{"job_id":"job","user_id":"user","idempotency_key":"key","batch_index":0,"segment_ids":[]}"#,
    ).unwrap();
    let dial = || async { panic!("this probe must not dial") };
    for batch in [
        vec![],
        vec!["id@example.test".into(); MAX_PROBE_SEGMENTS + 1],
        vec!["bad id".into()],
    ] {
        req.segment_ids = batch;
        assert!(matches!(
            run_probe(&pool, dial, |_| {}, &req).await,
            Err(ProbeError::BadRequest(_))
        ));
        assert_eq!(pool.borrow().idle(), 1);
    }
    req.segment_ids = vec!["id@example.test".into()];
    assert_eq!(
        run_probe(&pool, dial, |_| {}, &req).await,
        Err(ProbeError::Provider)
    );
    assert_eq!((pool.borrow().idle(), pool.borrow().in_use()), (0, 0));
}

#[test]
fn meter_refuses_invalid_acks_and_oversized_windows() {
    assert!(parse_ack(br#"{"accepted":["p"],"rejected":["p"]}"#).is_none());
    let mut book = MeterBook::new("user");
    let mut interval: DeliveredInterval = serde_json::from_str(
        r#"{"user_id":"user","play_id":"p","track_id":"t","start":0,"end":10,"file_bytes":9}"#,
    )
    .unwrap();
    assert!(!book.record(&interval, 1));
    interval.file_bytes = Some(10);
    assert!(book.record(&interval, 1));
    let mut sent = book.pending_windows();
    sent[0].started_at = 0;
    book.mark_flushed(&sent, &["p".into()]);
    assert!(book.has_pending());
    sent[0].track_id = "x".repeat(MAX_BATCH_BODY_BYTES);
    assert!(batch_body(&sent).is_none());
}

#[test]
fn trace_rejects_sensitive_and_unbounded_fields() {
    let mut fields = LogFields::new();
    for value in [
        "[::ffff:192.0.2.1]:563",
        "proxy Bearer sample",
        &"x".repeat(1025),
    ] {
        assert!(fields.insert_str("detail", value).is_err());
    }
    assert!(!format!("{:?}", fields.insert_uint("password=sample", 1)).contains("sample"));
    assert!(fields.is_empty());
}

#[test]
fn observation_projects_only_bounded_request_ids_hashes_and_known_codes() {
    let pii = "person@example.test";
    let mut event = CacheObserveEvent::new(pii, "user", CacheStatus::Miss, 0, 0, None);
    assert_eq!(event.request_id, REDACTED);
    event.request_id = "x".repeat(129);
    event.user_hash = pii.into();
    event.pool_error_code = Some("sample".into());
    assert_eq!(event.analytics_blobs(), ["miss", REDACTED, REDACTED, ""]);
    assert!(!format!("{event:?}").contains("sample"));
    assert!(!event.to_json_log().contains("@example.test"));
}
