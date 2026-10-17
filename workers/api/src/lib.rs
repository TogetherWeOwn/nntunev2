//! NNTune v2 accounts/auth/session/vault core.
//!
//! Native + wasm32 compatible. No Worker runtime imports here: the HTTP +
//! Hyperdrive wiring lives in the Worker entry (follow-up once staging
//! secrets are provisioned). This crate holds the security-critical logic
//! with unit + integration tests.

pub mod auth;
pub mod b64;
pub mod library;
pub mod session;
pub mod vault;

/// Structured log emitter and trace-ID propagation shared with the other
/// workers (`docs/logging-standard.md`). The api Worker's HTTP entry — the
/// fetch boundary that resolves inbound `x-trace-id`s, stamps this emitter,
/// and echoes the trace downstream — lands in a follow-up slice; until then
/// this re-export is the contract that entry programs against, exercised by
/// the tests below.
pub use nntune_logging as logging;

#[cfg(test)]
mod logging_tests {
    use super::logging::{fields_from_json, Level, LogRecord, Worker};

    const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";

    /// The api emitter renders the standard six-key envelope through the
    /// REAL constructor, not a hand-written fixture.
    #[test]
    fn api_emitter_renders_standard_record() {
        let record = LogRecord::new(
            Worker::Api,
            Level::Info,
            TRACE,
            "api.auth.login",
            fields_from_json(serde_json::json!({"status_code": 200, "duration_ms": 12})).unwrap(),
            1_792_094_400_123,
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&record.to_json_line()).unwrap();
        let object = parsed.as_object().unwrap();
        assert_eq!(object.len(), 6);
        assert_eq!(object["worker"], "api");
        assert_eq!(object["trace_id"], TRACE);
        assert_eq!(object["event"], "api.auth.login");
        assert_eq!(object["ts"], "2026-10-15T20:00:00.123Z");
        assert_eq!(record.trace_id(), TRACE);
        assert_eq!(record.event(), "api.auth.login");
    }

    /// Negative coverage: unscoped events, invalid traces, and credential
    /// shapes never become api log lines.
    #[test]
    fn api_emitter_rejects_unscoped_events_and_secrets() {
        let ok_fields = || fields_from_json(serde_json::json!({"status_code": 200})).unwrap();
        assert!(LogRecord::new(
            Worker::Api,
            Level::Info,
            TRACE,
            "ingest.job.queued",
            ok_fields(),
            0,
        )
        .is_err());
        assert!(LogRecord::new(
            Worker::Api,
            Level::Info,
            "short",
            "api.auth.login",
            ok_fields(),
            0
        )
        .is_err());
        for value in [
            serde_json::json!({"session_id": "s"}),
            serde_json::json!({"password": "hunter2"}),
            serde_json::json!({"email": "a@example.com"}),
        ] {
            assert!(
                fields_from_json(value).is_err(),
                "emitter accepted a credential shape"
            );
        }
    }
}
