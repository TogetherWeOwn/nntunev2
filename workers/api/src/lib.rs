//! NNTune v2 accounts/auth/session/vault/library: security-critical core
//! (`auth`, `session`, `vault`, `webauthn`, `library`), the
//! framework-agnostic HTTP handlers (`handlers`, native-tested against
//! fake stores), and the Worker + Hyperdrive entry (`worker_entry`).

pub mod account_deletion;
pub mod api_error;
pub mod auth;
pub mod b64;
pub mod handlers;
pub mod legacy_playlist_fixtures;
pub mod library;
pub mod native_rest;
pub mod rate_limit;
pub mod session;
pub mod vault;
pub mod webauthn;
pub mod worker_entry;

/// Structured log emitter and trace-ID propagation shared with the other
/// workers (`docs/logging-standard.md`). The api Worker's HTTP entry
/// (`worker_entry::dispatch`) resolves inbound `x-trace-id`s through this
/// emitter and records every served request plus startup/database failures;
/// the contract tests below pin the envelope this entry programs against.
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

#[worker::event(fetch)]
pub async fn main(
    req: worker::Request,
    env: worker::Env,
    _ctx: worker::Context,
) -> worker::Result<worker::Response> {
    worker_entry::dispatch(req, env).await
}
