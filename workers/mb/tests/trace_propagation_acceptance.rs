//! Acceptance tests: mb worker structured logging + trace-ID propagation.
//!
//! Pins the runtime contract from `docs/logging-standard.md`: inbound
//! `x-trace-id` values resolve (keep valid, mint fresh otherwise), the mb
//! emitter renders the `mb.*`-namespaced six-key envelope for lookup/search
//! completions, the trace rides back out on the same `x-trace-id` header, and
//! the emitter's privacy backstop rejects personal-data and credential shapes
//! while operational labels still pass.
//!
//! These tests link the shared emitter directly, so they fail without the
//! worker-logging runtime and pass with it. Static fixtures only: no live
//! database, no network.

use nntune_logging::{
    fields_from_json, is_valid_trace_id, resolve_trace_id, Level, LogRecord, Worker, TRACE_HEADER,
};

const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";
/// The fixture instant from the logging standard: `2026-10-15T20:00:00.123Z`.
const FIXTURE_MILLIS: u64 = 1_792_094_400_123;

#[test]
fn inbound_trace_resolution_keeps_valid_ids_and_regenerates_the_rest() {
    assert_eq!(resolve_trace_id(Some(TRACE)), TRACE);
    for header in [
        None,
        Some(""),
        Some("short"),
        Some("9F3A4C2E1B5D4F6A8C7E9D0B1A2F4E6C"),
        Some("9f3a4c2e-1b5d-4f6a-8c7e-9d0b1a2f4e6c"),
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
fn mb_lookup_completion_renders_the_mb_envelope() {
    let record = LogRecord::new(
        Worker::Mb,
        Level::Info,
        TRACE,
        "mb.lookup.completed",
        fields_from_json(
            serde_json::json!({"kind": "artist", "status_code": 200, "cache": "edge"}),
        )
        .unwrap(),
        FIXTURE_MILLIS,
    )
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&record.to_json_line()).unwrap();
    let object = parsed.as_object().unwrap();
    assert_eq!(
        object.len(),
        6,
        "mb records carry exactly the six shared fields"
    );
    assert_eq!(object["worker"], "mb");
    assert_eq!(object["trace_id"], TRACE);
    assert_eq!(object["event"], "mb.lookup.completed");
    assert_eq!(object["ts"], "2026-10-15T20:00:00.123Z");
}

#[test]
fn mb_search_completion_carries_parsed_kind_and_status_only() {
    for (kind, status) in [("artist", 200), ("unknown", 404)] {
        let record = LogRecord::new(
            Worker::Mb,
            Level::Info,
            TRACE,
            "mb.search.completed",
            fields_from_json(serde_json::json!({"kind": kind, "status_code": status})).unwrap(),
            FIXTURE_MILLIS,
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&record.to_json_line()).unwrap();
        assert_eq!(parsed["fields"]["kind"], kind);
        assert_eq!(parsed["fields"]["status_code"], status);
    }
    assert!(
        LogRecord::new(
            Worker::Mb,
            Level::Info,
            TRACE,
            "stream.play.start",
            fields_from_json(serde_json::json!({"kind": "artist"})).unwrap(),
            0,
        )
        .is_err(),
        "mb emitter accepted another worker's event namespace"
    );
}

#[test]
fn mb_emitter_privacy_backstop_rejects_personal_data_and_secrets() {
    for key in [
        "home_address",
        "phone_number",
        "payment_method",
        "postal_code",
        "grant_token",
        "api_key",
    ] {
        assert!(
            fields_from_json(serde_json::json!({key: "safe"})).is_err(),
            "emitter accepted banned key {key}"
        );
    }
    for value in ["a@example.com", "::1", "2001:db8::1", "Bearer abc123"] {
        assert!(
            fields_from_json(serde_json::json!({"contact": value})).is_err(),
            "emitter accepted sensitive value {value}"
        );
    }
    for value in [
        "nzb:sha256:abc123",
        "12:00:00",
        "task-queued",
        "ingest:u1:abc:upload",
    ] {
        assert!(
            fields_from_json(serde_json::json!({"detail": value})).is_ok(),
            "emitter rejected operational value {value}"
        );
    }
}

#[test]
fn trace_echo_rides_the_same_header_name() {
    assert_eq!(TRACE_HEADER, "x-trace-id");
    // Pin the wasm-only adapter by source (Response/Headers need
    // wasm-bindgen, so it cannot run natively): `with_trace` must echo on
    // the shared header and `inbound_trace_id` must resolve the inbound
    // value, so renaming the echo or minting unconditionally fails here.
    let source = include_str!("../src/lib.rs");
    let production = source
        .split_once("#[cfg(test)]")
        .expect("test module must exist")
        .0;
    let echo = production
        .split_once("fn with_trace(")
        .expect("with_trace must exist")
        .1
        .split_once("\n    }\n")
        .expect("with_trace must close")
        .0;
    assert!(
        echo.contains(".append(nntune_logging::TRACE_HEADER, trace_id)"),
        "with_trace must echo the trace on the shared x-trace-id header"
    );
    let inbound = production
        .split_once("fn inbound_trace_id(")
        .expect("inbound_trace_id must exist")
        .1
        .split_once("\n    }\n")
        .expect("inbound_trace_id must close")
        .0;
    assert!(
        inbound.contains("resolve_trace_id(header.as_deref())"),
        "inbound_trace_id must resolve the inbound header value, not mint unconditionally"
    );
}
