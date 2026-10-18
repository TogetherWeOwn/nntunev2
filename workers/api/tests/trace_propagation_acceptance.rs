//! Acceptance tests: api worker structured logging + trace-ID propagation.
//!
//! Pins the runtime contract from `docs/logging-standard.md`: the api emitter
//! renders the exact six-key envelope through the real constructor, inbound
//! `x-trace-id` values are kept only when valid (otherwise a fresh 128-bit ID
//! is generated and the invalid value is never trusted or logged), and
//! credential-shaped fields never become log lines.
//!
//! These tests link the shared emitter through the api crate's `logging`
//! re-export, so they fail without the worker-logging runtime and pass with it.
//! Static fixtures only: no live Cloudflare tail, no network.

use nntune_api::logging::{
    fields_from_json, generate_trace_id, is_valid_trace_id, resolve_trace_id, Level, LogRecord,
    Worker, TRACE_HEADER,
};

const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";
/// The fixture instant from the logging standard: `2026-10-15T20:00:00.123Z`.
const FIXTURE_MILLIS: u64 = 1_792_094_400_123;

fn line_fields(line: &str) -> serde_json::Map<String, serde_json::Value> {
    let parsed: serde_json::Value =
        serde_json::from_str(line).expect("log line must be valid JSON");
    parsed
        .as_object()
        .expect("log line must be a JSON object")
        .clone()
}

#[test]
fn api_emitter_renders_exact_six_key_envelope() {
    let record = LogRecord::new(
        Worker::Api,
        Level::Info,
        TRACE,
        "api.auth.login",
        fields_from_json(serde_json::json!({"status_code": 200, "duration_ms": 12})).unwrap(),
        FIXTURE_MILLIS,
    )
    .unwrap();
    let object = line_fields(&record.to_json_line());
    let keys: Vec<&str> = {
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    };
    assert_eq!(
        keys,
        ["event", "fields", "level", "trace_id", "ts", "worker"],
        "api records carry exactly the six shared fields"
    );
    assert_eq!(object["worker"], "api");
    assert_eq!(object["trace_id"], TRACE);
    assert_eq!(object["event"], "api.auth.login");
    assert_eq!(object["level"], "info");
    assert_eq!(object["ts"], "2026-10-15T20:00:00.123Z");
    assert_eq!(object["fields"]["status_code"], 200);
    assert_eq!(record.trace_id(), TRACE);
    assert_eq!(record.event(), "api.auth.login");
}

#[test]
fn inbound_trace_resolution_keeps_valid_ids_and_regenerates_the_rest() {
    assert_eq!(resolve_trace_id(Some(TRACE)), TRACE);
    for header in [
        None,
        Some(""),
        Some("too-short"),
        Some("9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c0"),
        Some("9F3A4C2E1B5D4F6A8C7E9D0B1A2F4E6C"),
        Some("Bearer abc123"),
    ] {
        let resolved = resolve_trace_id(header);
        assert!(
            is_valid_trace_id(&resolved),
            "must resolve {header:?} to a valid 128-bit ID"
        );
        assert_ne!(
            resolved.as_str(),
            header.unwrap_or(""),
            "must never echo an invalid value"
        );
    }
}

#[test]
fn generated_trace_ids_are_valid_128bit_and_unique() {
    let first = generate_trace_id().expect("entropy must be available in tests");
    let second = generate_trace_id().expect("entropy must be available in tests");
    assert!(is_valid_trace_id(&first));
    assert!(is_valid_trace_id(&second));
    assert_ne!(first, second);
}

#[test]
fn api_emitter_rejects_cross_worker_events_bad_traces_and_secrets() {
    let ok_fields = || fields_from_json(serde_json::json!({"status_code": 200})).unwrap();
    assert!(
        LogRecord::new(
            Worker::Api,
            Level::Info,
            TRACE,
            "ingest.job.queued",
            ok_fields(),
            0
        )
        .is_err(),
        "api emitter accepted another worker's event namespace"
    );
    assert!(
        LogRecord::new(
            Worker::Api,
            Level::Info,
            "short",
            "api.auth.login",
            ok_fields(),
            0
        )
        .is_err(),
        "api emitter accepted an invalid trace ID"
    );
    for value in [
        serde_json::json!({"session_id": "s"}),
        serde_json::json!({"password": "hunter2"}),
        serde_json::json!({"contact": "a@example.com"}),
        serde_json::json!({"host": "192.0.2.1"}),
    ] {
        assert!(
            fields_from_json(value).is_err(),
            "api emitter accepted a credential-shaped field"
        );
    }
}

#[test]
fn trace_header_name_is_the_stable_propagation_contract() {
    assert_eq!(TRACE_HEADER, "x-trace-id");
}
