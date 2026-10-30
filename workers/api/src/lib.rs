//! NNTune v2 accounts/auth/session/vault/library: security-critical core
//! (`auth`, `session`, `vault`, `webauthn`, `library`), the
//! framework-agnostic HTTP handlers (`handlers`, native-tested against
//! fake stores), the Worker + Hyperdrive entry (`worker_entry`), and the
//! Subsonic-compatible `/rest/*` surface (wasm32 only), which adapts to
//! the transport-free `nntune-subsonic` dispatcher.
//!
//! The production Subsonic `Library` (PlanetScale via Hyperdrive, R2
//! audio/artwork via the stream worker) plugs in behind the `Library`
//! seam. Until then the Worker uses `MemStore::empty()`, which holds no
//! credentials and rejects every auth attempt. Per-client `apiKey`
//! credentials will resolve against `subsonic_clients`
//! (`migrations/0023_subsonic_clients.sql`).

pub mod account_deletion;
pub mod api_error;
pub mod auth;
pub mod b64;
pub mod billing;
pub mod handlers;
pub mod ledger;
pub mod legacy_playlist_fixtures;
pub mod library;
mod magic_redemption;
pub mod native_rest;
pub mod plan_limits;
pub mod providers;
pub mod random;
pub mod rate_limit;
pub mod rotation;
pub mod session;
pub mod session_retry_store;
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

#[cfg(target_arch = "wasm32")]
use futures_util::StreamExt;
#[cfg(target_arch = "wasm32")]
use nntune_subsonic::{
    oversized_body_response, unreadable_body_response, BodyFault, FormBody, MemStore, SubsonicApp,
};
#[cfg(target_arch = "wasm32")]
use worker::*;

#[worker::event(fetch)]
pub async fn main(
    req: worker::Request,
    env: worker::Env,
    _ctx: worker::Context,
) -> worker::Result<worker::Response> {
    // The Subsonic `/rest/*` surface (wasm32 only) adapts to the
    // transport-free `nntune-subsonic` dispatcher. Every other path keeps
    // `worker_entry::dispatch` behavior; the path peek only borrows, so an
    // unclaimed request still moves into dispatch untouched.
    #[cfg(target_arch = "wasm32")]
    if is_subsonic_request(&req) {
        let mut req = req;
        return subsonic_response(&mut req).await;
    }
    worker_entry::dispatch(req, env).await
}

/// True when this request targets the Subsonic `/rest/*` surface.
#[cfg(target_arch = "wasm32")]
fn is_subsonic_request(req: &Request) -> bool {
    req.url().is_ok_and(|url| is_subsonic_path(url.path()))
}

/// Pure path predicate behind [`is_subsonic_request`]. Compiled natively
/// for tests so the routing boundary the wasm32 entry depends on is
/// pinned without a Worker runtime.
#[cfg(any(test, target_arch = "wasm32"))]
fn is_subsonic_path(path: &str) -> bool {
    path == "/rest" || path.starts_with("/rest/")
}

#[cfg(test)]
mod subsonic_routing_tests {
    use super::is_subsonic_path;

    #[test]
    fn rest_prefix_claims_subsonic_paths_only() {
        for claimed in ["/rest", "/rest/", "/rest/ping", "/rest/getArtists.view"] {
            assert!(is_subsonic_path(claimed), "{claimed} routes to Subsonic");
        }
        for unclaimed in [
            "/",
            "/healthz",
            "/v1/auth/magic/start",
            "/restless",
            "/api/rest/ping",
            "/REST/ping",
        ] {
            assert!(!is_subsonic_path(unclaimed), "{unclaimed} keeps dispatch");
        }
    }
}

/// Serve one Subsonic request: v1 reads params from query plus POST form
/// body (`$request->input()`), refusing bodies over 10 MB with error 10.
#[cfg(target_arch = "wasm32")]
async fn subsonic_response(req: &mut Request) -> Result<Response> {
    let url = req.url()?;
    let path = url.path().to_string();
    let query = url.query().unwrap_or("").to_string();
    let method = req.method().to_string();

    // v1 reads params from query plus POST form body (`$request->input()`).
    let content_type = req
        .headers()
        .get("content-type")
        .unwrap_or_default()
        .unwrap_or_default();
    let is_form_post = method.eq_ignore_ascii_case("POST")
        && content_type.contains("application/x-www-form-urlencoded");
    let declared = req
        .headers()
        .get("content-length")
        .unwrap_or_default()
        .and_then(|value| value.trim().parse::<usize>().ok());
    let fault_response = |fault: BodyFault| match fault {
        BodyFault::Oversized => oversized_body_response(&query),
        BodyFault::Unreadable => unreadable_body_response(&query),
    };
    let mut form = FormBody::new(declared);
    // v1 `MergeFormPostParams` refuses bodies over 10 MB on `Content-Length`
    // before buffering: answer error 10 without reading the body at all.
    if is_form_post {
        if let Err(fault) = form.check_declared() {
            return respond(&fault_response(fault));
        }
    }
    let body = if is_form_post
        // Request::stream() errors on a null JS body. That is a valid empty
        // form POST, distinct from a stream that exists but cannot be read.
        && req.inner().body().is_some()
    {
        let mut stream = match req.stream() {
            Ok(stream) => stream,
            Err(_) => return respond(&unreadable_body_response(&query)),
        };
        while let Some(chunk) = stream.next().await {
            let pushed = match chunk {
                Ok(chunk) => form.push(&chunk),
                Err(_) => Err(BodyFault::Unreadable),
            };
            if let Err(fault) = pushed {
                return respond(&fault_response(fault));
            }
        }
        match form.finish() {
            Ok(body) => body,
            Err(fault) => return respond(&fault_response(fault)),
        }
    } else {
        String::new()
    };

    let app = SubsonicApp::new(MemStore::empty());
    let res = app.handle(&method, &path, &query, &body);
    respond(&res)
}

#[cfg(target_arch = "wasm32")]
fn respond(res: &nntune_subsonic::HttpResponse) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", &res.content_type)?;
    let mut response = Response::from_bytes(res.body.clone())?;
    response = response.with_status(res.status);
    *response.headers_mut() = headers;
    Ok(response)
}
