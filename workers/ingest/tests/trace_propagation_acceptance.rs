//! Acceptance tests: ingest trace-ID propagation through queued work.
//!
//! Pins the runtime contract from `docs/logging-standard.md`: the validated
//! inbound `x-trace-id` rides in the queue message, survives serialization,
//! and a consumer receiving a valid ID MUST NOT replace it; messages enqueued
//! before tracing existed still decode (as untraced) so the consumer can mint
//! a root trace instead of failing. The ingest emitter renders the
//! `ingest.*`-namespaced six-key envelope.
//!
//! These tests touch `QueueMsg::trace_id` and the shared emitter, so they fail
//! without the worker-logging runtime and pass with it. No live Queue, no
//! network: messages are serialized in memory.

use nntune_ingest::queue::QueueMsg;
use nntune_logging::{
    fields_from_json, is_valid_trace_id, resolve_trace_id, Level, LogRecord, Worker, TRACE_HEADER,
};

const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";

fn search_message(trace_id: Option<&str>) -> serde_json::Value {
    let mut message = serde_json::json!({
        "job_id": "job_1",
        "user_id": "u1",
        "idempotency_key": "ingest:u1:abc:search",
        "source": "indexer_search",
        "state": "queued",
        "attempt": 1,
        "query": "query",
    });
    if let Some(trace) = trace_id {
        message["trace_id"] = serde_json::Value::String(trace.to_string());
    }
    message
}

#[test]
fn queue_message_carries_trace_id_end_to_end() {
    let message: QueueMsg = serde_json::from_value(search_message(Some(TRACE))).unwrap();
    assert_eq!(message.trace_id.as_deref(), Some(TRACE));
    let wire = serde_json::to_string(&message).unwrap();
    assert!(
        wire.contains(TRACE),
        "trace ID must survive queue serialization"
    );
    let back: QueueMsg = serde_json::from_str(&wire).unwrap();
    assert_eq!(message, back);
    assert_eq!(back.trace_id.as_deref(), Some(TRACE));
}

#[test]
fn queue_message_without_trace_decodes_as_untraced() {
    let message: QueueMsg = serde_json::from_value(search_message(None)).unwrap();
    assert_eq!(message.trace_id, None);
}

#[test]
fn consumer_preserves_a_valid_inbound_trace_and_mints_otherwise() {
    assert_eq!(
        resolve_trace_id(Some(TRACE)),
        TRACE,
        "a consumer receiving a valid ID MUST NOT replace it"
    );
    for header in [None, Some(""), Some("too-short"), Some("Bearer abc123")] {
        let resolved = resolve_trace_id(header);
        assert!(is_valid_trace_id(&resolved));
        assert_ne!(resolved.as_str(), header.unwrap_or(""));
    }
    // Pin the consumer wiring itself, not just the shared resolver: the
    // wasm-only `process_raw_message` must keep the inbound ID and stamp it
    // back on the message, so `resolve_trace_id(None)` there fails here.
    // Slice the pre-test source so this test's own literals cannot satisfy it.
    let source = include_str!("../src/queue_consumer.rs");
    let production = source
        .split_once("#[cfg(test)]")
        .expect("test module must exist")
        .0;
    let consumer = production
        .split_once("async fn process_raw_message")
        .expect("process_raw_message must exist")
        .1
        .split_once("#[worker::event(queue)]")
        .expect("queue entrypoint must bound process_raw_message")
        .0;
    assert!(
        consumer.contains("resolve_trace_id(msg.trace_id.as_deref())"),
        "consumer must resolve the inbound message trace ID, not mint unconditionally"
    );
    assert!(
        consumer.contains("msg.trace_id = Some(trace_id.clone())"),
        "consumer must stamp the resolved trace ID back on the message"
    );
}

#[test]
fn ingest_emitter_envelope_uses_the_ingest_namespace() {
    let record = LogRecord::new(
        Worker::Ingest,
        Level::Info,
        TRACE,
        "ingest.job.queued",
        fields_from_json(serde_json::json!({"job_id": "job_1", "stage": "queued"})).unwrap(),
        1_792_094_400_123,
    )
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&record.to_json_line()).unwrap();
    assert_eq!(parsed.as_object().unwrap().len(), 6);
    assert_eq!(parsed["worker"], "ingest");
    assert_eq!(parsed["trace_id"], TRACE);
    assert_eq!(parsed["event"], "ingest.job.queued");
    assert!(
        LogRecord::new(
            Worker::Ingest,
            Level::Info,
            TRACE,
            "api.auth.login",
            fields_from_json(serde_json::json!({"stage": "queued"})).unwrap(),
            0,
        )
        .is_err(),
        "ingest emitter accepted another worker's event namespace"
    );
}

#[test]
fn trace_header_name_is_the_stable_propagation_contract() {
    assert_eq!(TRACE_HEADER, "x-trace-id");
}
