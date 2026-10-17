//! NNTune v2 ingest pipeline worker.
//!
//! Routes (all JSON):
//! - `POST /v1/ingest/search` — Newznab search with the user's own indexer
//!   (`indexer_ref` vault handle; raw keys are never accepted here).
//! - `POST /v1/ingest/nzb` — NZB upload; guarded by intake caps, then
//!   parsed by `nntune-core` in the next slice.
//! - `GET /v1/ingest/jobs/:id` — job checkpoint status (idempotent resume).
//!
//! Pipeline: search/upload → parse (`nntune-core`) → segment availability
//! probe through the user's pool DO → par2 verify (Container batch repair
//! when needed) → tag/fingerprint + MusicBrainz match (`workers/mb`) →
//! library rows. Routes validate the contract, persist upload bytes to the
//! payload KV, and enqueue to `nntune-ingest-jobs`; the Queue consumer
//! (Workflow steps in `queue::WORKFLOW_STEPS`) runs parse → probe → verify →
//! repair → match → write once its handler lands (the consumer/workflow
//! entries stay commented out in `wrangler.toml.example` until then).
//! See `README.md`.

use futures_util::StreamExt;
use nntune_logging::{Level, LogRecord, Worker as LogWorker};
use worker::{console_log, Env, Request, Response, Result, RouteContext, Router};

pub mod dispatch;
pub mod fixtures;
pub mod jobs;
pub mod library;
pub mod metering;
pub mod newznab;
pub mod nzb_intake;
pub mod parse;
pub mod probe;
pub mod queue;
pub mod rate_limit;
pub mod repair;

use dispatch::validate_user_id;
use jobs::validate_job_id;
use newznab::{MAX_CATEGORIES, MAX_LIMIT, MAX_PAGE};
use nzb_intake::{estimate_nzb, IntakeRejection};
use queue::{validate_indexer_ref, NzbUploadRequest, QueueMsg, SearchRequest};

/// Largest accepted `POST /v1/ingest/search` body: the request is a query plus
/// a short category list, never content. Enforced while streaming
/// ([`read_capped_text`]): at most `cap` bytes are ever buffered, whether or
/// not the caller sent `Content-Length`.
pub const MAX_SEARCH_BODY_BYTES: usize = 16 * 1024;
/// Largest accepted `POST /v1/ingest/nzb` JSON body. The decoded NZB is capped
/// at [`MAX_NZB_BYTES`], but JSON escaping can inflate hostile input ~6x on
/// the wire, so the wire cap accounts for that before buffering.
pub const MAX_UPLOAD_BODY_BYTES: usize = 13 * 1024 * 1024;

#[derive(Debug, serde::Serialize)]
struct ApiError {
    error: String,
}

/// Resolve the request trace from the inbound `x-trace-id` header: a valid
/// 128-bit ID is kept, anything else (absent, malformed, untrusted) is
/// replaced with a fresh random ID. The invalid value is never logged.
fn inbound_trace_id(req: &Request) -> String {
    let header = req
        .headers()
        .get(nntune_logging::TRACE_HEADER)
        .ok()
        .flatten();
    nntune_logging::resolve_trace_id(header.as_deref())
}

/// Emit one structured log line for this request (`docs/logging-standard.md`).
/// Field validation runs at construction: a banned key or secret-like value
/// drops the line instead of leaking it. All call-site fields are constants
/// plus opaque IDs and counters, so the drop branch only fires on a code bug.
fn emit(level: Level, trace_id: &str, event: &str, fields: serde_json::Value) {
    let ts_millis = worker::Date::now().as_millis();
    let Ok(field_map) = nntune_logging::fields_from_json(fields) else {
        return;
    };
    let Ok(record) = LogRecord::new(
        LogWorker::Ingest,
        level,
        trace_id,
        event,
        field_map,
        ts_millis,
    ) else {
        return;
    };
    console_log!("{}", record.to_json_line());
}

/// Echo the request trace on a response so callers can correlate.
/// Best-effort: a header failure must not fail the request itself.
fn with_trace(mut response: Response, trace_id: &str) -> Response {
    let _ = response
        .headers_mut()
        .append(nntune_logging::TRACE_HEADER, trace_id);
    response
}

fn json_error(message: &str, status: u16, trace_id: &str) -> Result<Response> {
    let body = serde_json::to_string(&ApiError {
        error: message.to_string(),
    })
    .unwrap_or_else(|_| r#"{"error":"internal"}"#.to_string());
    Response::from_body(worker::ResponseBody::Body(body.into_bytes())).map(|r| {
        with_trace(
            r.with_status(status)
                .with_headers(headers_json().unwrap_or_default()),
            trace_id,
        )
    })
}

fn headers_json() -> Result<worker::Headers> {
    let headers = worker::Headers::new();
    headers.append("content-type", "application/json; charset=utf-8")?;
    Ok(headers)
}

fn accepted(body: serde_json::Value, trace_id: &str) -> Result<Response> {
    Ok(with_trace(
        Response::from_json(&body)?.with_status(202),
        trace_id,
    ))
}

fn service_unavailable(message: &str, trace_id: &str) -> Result<Response> {
    let body = serde_json::to_string(&ApiError {
        error: message.to_string(),
    })
    .unwrap_or_else(|_| r#"{"error":"internal"}"#.to_string());
    let headers = headers_json().unwrap_or_default();
    headers.append("retry-after", "5")?;
    Response::from_body(worker::ResponseBody::Body(body.into_bytes()))
        .map(|r| with_trace(r.with_status(503).with_headers(headers), trace_id))
}

/// Declared `Content-Length`, if the caller sent one.
fn content_length(req: &Request) -> Option<usize> {
    req.headers()
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
}

/// Cheap fast path: reject an over-cap declared `Content-Length` before any
/// byte is buffered. Pure so the policy is unit-tested.
fn check_declared_len(
    declared_len: Option<usize>,
    cap: usize,
) -> core::result::Result<(), &'static str> {
    if declared_len.is_some_and(|len| len > cap) {
        return Err("request body too large");
    }
    Ok(())
}

/// Fold one more body chunk into the buffer, enforcing the wire cap on the
/// running total. Pure so the streaming policy is unit-tested without a
/// `Request`: a missing or lying `Content-Length` can never buffer more than
/// `cap` bytes, because the 413 fires on the chunk that would cross it.
fn push_chunk(
    buf: &mut Vec<u8>,
    chunk: &[u8],
    cap: usize,
) -> core::result::Result<(), &'static str> {
    if buf.len().saturating_add(chunk.len()) > cap {
        return Err("request body too large");
    }
    buf.extend_from_slice(chunk);
    Ok(())
}

/// Read a request body as text without ever buffering more than `cap` bytes.
///
/// `Request::text()` is deliberately not used: it buffers the entire body
/// into a JS string before any cap applies, so a request without
/// `Content-Length` (chunked HTTP/1.1, or HTTP/2 and HTTP/3 where the header
/// is optional) could push the isolate toward its memory limit from an
/// unauthenticated route. This reads through `Request::stream()` and bails
/// with 413 on the chunk that would cross the cap; non-UTF-8 bodies are a
/// 400, as is an unreadable stream.
async fn read_capped_text(
    req: &mut Request,
    cap: usize,
) -> core::result::Result<String, (String, u16)> {
    if let Err(message) = check_declared_len(content_length(req), cap) {
        return Err((message.to_string(), 413));
    }
    let stream = req
        .stream()
        .map_err(|_| ("failed to read request body".to_string(), 400))?;
    let mut stream = Box::pin(stream);
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ("failed to read request body".to_string(), 400))?;
        push_chunk(&mut buf, &chunk, cap).map_err(|m| (m.to_string(), 413))?;
    }
    String::from_utf8(buf).map_err(|_| ("request body must be UTF-8".to_string(), 400))
}

/// Parse a search body: malformed JSON is a client error (400), never a 500.
/// (Without this, `Router::run` turns the handler `Err` into a 500 plus a
/// noisy `console_error!` for every client typo.)
fn parse_search(text: &str) -> core::result::Result<SearchRequest, &'static str> {
    serde_json::from_str(text).map_err(|_| "body must be SearchRequest JSON")
}

/// Validate a parsed search request: bounds the query, the vault handle, the
/// page window, and the category list before anything is built or enqueued.
fn check_search(req: &SearchRequest) -> core::result::Result<(), &'static str> {
    let q = req.query.trim();
    if q.is_empty() || q.len() > 200 {
        return Err("query must be 1..=200 chars");
    }
    if !validate_indexer_ref(&req.indexer_ref) {
        return Err("indexer_ref must be a vault handle");
    }
    if req.limit == 0 || req.limit > MAX_LIMIT {
        return Err("limit must be 1..=100");
    }
    if req.page > MAX_PAGE {
        return Err("page out of range");
    }
    if req.categories.len() > MAX_CATEGORIES {
        return Err("too many categories (max 16)");
    }
    if !validate_user_id(&req.user_id) {
        return Err("user_id must be 1..=128 chars without ':'");
    }
    Ok(())
}

fn parse_upload(text: &str) -> core::result::Result<NzbUploadRequest, &'static str> {
    serde_json::from_str(text).map_err(|_| "body must be NzbUploadRequest JSON")
}

fn check_upload(req: &NzbUploadRequest) -> core::result::Result<(), &'static str> {
    if req.filename.is_empty() || req.filename.len() > 255 {
        return Err("filename must be 1..=255 chars");
    }
    if !validate_user_id(&req.user_id) {
        return Err("user_id must be 1..=128 chars without ':'");
    }
    Ok(())
}

/// Send one dispatch message. Returns true when the Queue accepted it; a
/// missing or failing binding returns false and the route answers 503 (see
/// [`queued_envelope`]), so the job is retried instead of silently lost.
/// Queue errors stay out of the log line (driver strings can carry binding
/// detail); the line carries the trace plus opaque routing facts only.
async fn try_enqueue(env: &Env, msg: &QueueMsg, trace_id: &str, source: &str) -> bool {
    // The message already carries `trace_id` (stamped by the route), so the
    // consumer and any downstream worker call share this request's trace.
    let accepted = match env.queue(dispatch::INGEST_QUEUE_BINDING) {
        Ok(queue) => queue.send(msg.clone()).await.is_ok(),
        Err(_) => false,
    };
    if !accepted {
        emit(
            Level::Error,
            trace_id,
            "ingest.queue.enqueue_failed",
            serde_json::json!({"job_id": msg.job_id, "source": source}),
        );
    }
    accepted
}

/// Answer an enqueue result. Success is 202 `queued`; a missing or failing
/// Queue binding is 503 with `Retry-After` — never 202 — so clients, proxies,
/// and retry middleware do not mistake a lost job for an accepted one.
/// `status: queued` is only ever sent after a successful `send`.
fn queued_envelope(
    msg: &QueueMsg,
    source: &str,
    queued: bool,
    extra: serde_json::Value,
    trace_id: &str,
) -> Result<Response> {
    let mut body = serde_json::json!({
        "stage": "queued",
        "source": source,
        "status": if queued { "queued" } else { "retry" },
        "job_id": msg.job_id,
        "idempotency_key": msg.idempotency_key,
    });
    if !queued {
        body["hint"] = serde_json::Value::String(
            "Queue unavailable; nothing was accepted, retry shortly.".into(),
        );
    }
    if let serde_json::Value::Object(map) = extra {
        if let serde_json::Value::Object(ref mut body_map) = body {
            body_map.extend(map);
        }
    }
    if !queued {
        let headers = headers_json().unwrap_or_default();
        headers.append("retry-after", "5")?;
        return Response::from_json(&body)
            .map(|r| with_trace(r.with_status(503).with_headers(headers), trace_id));
    }
    accepted(body, trace_id)
}

/// Persist upload bytes under their content-addressed key before enqueue, so
/// the Queue consumer can fetch the exact bytes the intake guard approved.
/// `false` means the payload is not durably stored: the route answers 503
/// and never enqueues.
///
/// The write carries [`PAYLOAD_TTL_SECS`](dispatch::PAYLOAD_TTL_SECS) so a
/// payload stored before a failed enqueue, or for a job that later fails,
/// expires instead of accumulating forever. A failed write is confirmed
/// with a read before giving up: KV rate-limits same-key writes (1/s), and
/// the key is the content hash, so a client retry inside a second — or a
/// retry racing its own first write — can 429 even though the bytes are
/// already stored. Bytes present means the job can run.
async fn store_upload_payload(env: &Env, payload_ref: &str, nzb_xml: &str, trace_id: &str) -> bool {
    // KV/driver errors stay out of the line (they can carry binding detail);
    // the line carries the trace plus the fact of the failed stage only.
    let failed = || {
        emit(
            Level::Error,
            trace_id,
            "ingest.payload.store_failed",
            serde_json::json!({"source": "nzb_upload"}),
        );
    };
    match env.kv(dispatch::PAYLOAD_KV_BINDING) {
        Ok(store) => match store.put(payload_ref, nzb_xml.to_string()) {
            Ok(put) => match put
                .expiration_ttl(dispatch::PAYLOAD_TTL_SECS)
                .execute()
                .await
            {
                Ok(()) => true,
                Err(_) => {
                    let present = matches!(store.get(payload_ref).text().await, Ok(Some(_)));
                    if !present {
                        failed();
                    }
                    present
                }
            },
            Err(_) => {
                failed();
                false
            }
        },
        Err(_) => {
            failed();
            false
        }
    }
}

/// Validate a search body and enqueue it; the Queue consumer runs the
/// search → parse → probe → verify → match → write workflow. The resolved
/// trace travels in every log line, in the `x-trace-id` response header, and
/// in the queued message for the consumer.
async fn search(req: &mut Request, ctx: &RouteContext<()>) -> Result<Response> {
    let trace_id = inbound_trace_id(req);
    let text = match read_capped_text(req, MAX_SEARCH_BODY_BYTES).await {
        Ok(text) => text,
        Err((message, status)) => return json_error(&message, status, &trace_id),
    };
    let parsed = match parse_search(&text) {
        Ok(parsed) => parsed,
        Err(message) => return json_error(message, 400, &trace_id),
    };
    if let Err(message) = check_search(&parsed) {
        return json_error(message, 400, &trace_id);
    }
    let q = parsed.query.trim().to_string();
    let mut msg = match dispatch::build_search_dispatch(
        &parsed.user_id,
        &q,
        &parsed.indexer_ref,
        &parsed.categories,
        parsed.limit,
        parsed.page,
    ) {
        Ok(msg) => msg,
        Err(message) => return json_error(message, 400, &trace_id),
    };
    msg.trace_id = Some(trace_id.clone());
    // Operational facts only: opaque job ID plus the query length. The query
    // text, user handle, and indexer handle never enter a log line.
    emit(
        Level::Info,
        &trace_id,
        "ingest.search.received",
        serde_json::json!({"job_id": msg.job_id, "q_len": q.len()}),
    );
    let queued = try_enqueue(&ctx.env, &msg, &trace_id, "indexer_search").await;
    queued_envelope(
        &msg,
        "indexer_search",
        queued,
        serde_json::json!({}),
        &trace_id,
    )
}

/// Validate an NZB upload and enqueue it; the consumer parses with
/// `nntune-core`, probes the pool DO, and verifies before matching. Same
/// trace contract as [`search`]: log lines, response header, queued message.
async fn upload_nzb(req: &mut Request, ctx: &RouteContext<()>) -> Result<Response> {
    let trace_id = inbound_trace_id(req);
    let text = match read_capped_text(req, MAX_UPLOAD_BODY_BYTES).await {
        Ok(text) => text,
        Err((message, status)) => return json_error(&message, status, &trace_id),
    };
    let parsed = match parse_upload(&text) {
        Ok(parsed) => parsed,
        Err(message) => return json_error(message, 400, &trace_id),
    };
    if let Err(message) = check_upload(&parsed) {
        return json_error(message, 400, &trace_id);
    }
    match estimate_nzb(&parsed.nzb_xml) {
        Err(IntakeRejection::Empty) => json_error("nzb_xml must not be empty", 400, &trace_id),
        Err(IntakeRejection::TooLarge) => json_error("nzb_xml exceeds 2 MiB", 413, &trace_id),
        Err(IntakeRejection::NotNzb) => {
            json_error("nzb_xml is not an NZB document", 400, &trace_id)
        }
        Err(IntakeRejection::TooManyFiles) => json_error("nzb has too many files", 413, &trace_id),
        Err(IntakeRejection::TooManySegments) => {
            json_error("nzb has too many segments", 413, &trace_id)
        }
        Ok(est) => {
            let mut msg = match dispatch::build_upload_dispatch(&parsed.user_id, &parsed.nzb_xml) {
                Ok(msg) => msg,
                Err(message) => return json_error(message, 400, &trace_id),
            };
            msg.trace_id = Some(trace_id.clone());
            // The bytes must be durably stored before the job exists: an
            // enqueued job without retrievable bytes can never run.
            let payload_ref = msg.payload_ref.clone().unwrap_or_default();
            if payload_ref.is_empty()
                || !store_upload_payload(&ctx.env, &payload_ref, &parsed.nzb_xml, &trace_id).await
            {
                return service_unavailable(
                    "payload store unavailable; nothing was accepted",
                    &trace_id,
                );
            }
            // Counts plus the opaque job ID; filenames and NZB bytes never log.
            emit(
                Level::Info,
                &trace_id,
                "ingest.upload.received",
                serde_json::json!({
                    "job_id": msg.job_id,
                    "file_count": est.file_count,
                    "segment_count": est.segment_count,
                }),
            );
            let queued = try_enqueue(&ctx.env, &msg, &trace_id, "nzb_upload").await;
            queued_envelope(
                &msg,
                "nzb_upload",
                queued,
                serde_json::json!({
                    "file_count": est.file_count,
                    "segment_count": est.segment_count,
                }),
                &trace_id,
            )
        }
    }
}

/// Job checkpoint status: validates the id shape before DB wiring lands.
/// Malformed IDs answer 404 without a log line (scanner noise stays out of
/// the logs); valid IDs log one `status_checked` line under the trace.
async fn job_status(req: &Request, ctx: &RouteContext<()>) -> Result<Response> {
    let trace_id = inbound_trace_id(req);
    let id = ctx.param("id").cloned().unwrap_or_default();
    if !validate_job_id(&id) {
        return json_error("unknown job id", 404, &trace_id);
    }
    emit(
        Level::Info,
        &trace_id,
        "ingest.job.status_checked",
        serde_json::json!({"job_id": id}),
    );
    Ok(with_trace(
        Response::from_json(&serde_json::json!({
            "job_id": id,
            "status": "not_wired",
            "hint": "Checkpoint reads land with the Queue consumer; enqueue is live.",
        }))?,
        &trace_id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nzb_intake::MAX_NZB_BYTES;

    fn search_body(
        query: &str,
        indexer_ref: &str,
        categories: &[u32],
        limit: u32,
        page: u32,
    ) -> String {
        serde_json::json!({
            "user_id": "user_01",
            "query": query,
            "indexer_ref": indexer_ref,
            "categories": categories,
            "limit": limit,
            "page": page,
        })
        .to_string()
    }

    #[test]
    fn malformed_search_json_is_a_client_error() {
        assert!(parse_search("").is_err());
        assert!(parse_search("{not json").is_err());
        assert!(parse_search(r#"{"query":"x"}"#).is_err());
    }

    #[test]
    fn search_validation_bounds_query_handle_and_window() {
        let ok = search_body("dark side", "vault_idx_01-ab", &[3000], 25, 0);
        assert!(check_search(&parse_search(&ok).unwrap()).is_ok());
        let bad_query = search_body("", "vault_idx_01-ab", &[], 25, 0);
        assert!(check_search(&parse_search(&bad_query).unwrap()).is_err());
        let bad_handle = search_body("x", "https://i.example?apikey=k", &[], 25, 0);
        assert!(check_search(&parse_search(&bad_handle).unwrap()).is_err());
        let bad_limit = search_body("x", "vault_idx_01-ab", &[], 0, 0);
        assert!(check_search(&parse_search(&bad_limit).unwrap()).is_err());
        let bad_page = search_body("x", "vault_idx_01-ab", &[], 25, MAX_PAGE + 1);
        assert!(check_search(&parse_search(&bad_page).unwrap()).is_err());
        let many: Vec<u32> = (0..(MAX_CATEGORIES as u32 + 1)).collect();
        let bad_cats = search_body("x", "vault_idx_01-ab", &many, 25, 0);
        assert!(check_search(&parse_search(&bad_cats).unwrap()).is_err());
    }

    #[test]
    fn malformed_upload_json_is_a_client_error() {
        assert!(parse_upload("").is_err());
        assert!(parse_upload("[]").is_err());
        let ok: NzbUploadRequest =
            parse_upload(r#"{"user_id":"user_01","filename":"r.nzb","nzb_xml":"<nzb></nzb>"}"#)
                .unwrap();
        assert!(check_upload(&ok).is_ok());
        let bad_name: NzbUploadRequest =
            parse_upload(r#"{"user_id":"user_01","filename":"","nzb_xml":"<nzb></nzb>"}"#).unwrap();
        assert!(check_upload(&bad_name).is_err());
    }

    #[test]
    fn chunked_bodies_accumulate_under_a_running_cap() {
        // No declared length (chunked HTTP/1.1, or HTTP/2 and HTTP/3 where
        // the header is optional): chunks summing past the cap must fail
        // while only the capped prefix is buffered, never after a full read.
        let mut buf = Vec::new();
        assert!(push_chunk(&mut buf, b"hello", 16).is_ok());
        assert!(push_chunk(&mut buf, b" world!", 16).is_ok());
        assert_eq!(buf, b"hello world!");
        assert!(push_chunk(&mut buf, b"overflow", 16).is_err());
        // The failed push leaves the capped prefix intact for the 413 path.
        assert_eq!(buf, b"hello world!");
        // An exact fit is accepted; one byte more is not.
        let mut exact = Vec::new();
        assert!(push_chunk(&mut exact, &[0u8; 16], 16).is_ok());
        assert!(push_chunk(&mut exact, &[0u8; 1], 16).is_err());
    }

    #[test]
    fn declared_length_precheck_runs_before_any_read() {
        assert!(check_declared_len(Some(10), 16).is_ok());
        assert!(check_declared_len(None, 16).is_ok());
        assert!(check_declared_len(Some(17), 16).is_err());
    }

    #[test]
    fn body_caps_stay_small_and_fit_escaped_nzbs() {
        // Compile-time: the search cap stays small and the upload wire cap
        // always fits a maximally-escaped 2 MiB NZB.
        const {
            assert!(MAX_SEARCH_BODY_BYTES <= 16 * 1024);
            assert!(MAX_UPLOAD_BODY_BYTES >= MAX_NZB_BYTES);
        }
    }

    const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";

    /// The route's `received` line goes through the REAL emitter
    /// (`nntune-logging`), not a hand-written fixture: the same constructor
    /// and renderer production uses, checked against the six-key envelope.
    #[test]
    fn structured_received_lines_conform_to_the_standard() {
        for (event, value) in [
            (
                "ingest.search.received",
                serde_json::json!({"job_id": "job_01", "q_len": 9}),
            ),
            (
                "ingest.upload.received",
                serde_json::json!({"job_id": "job_01", "file_count": 1, "segment_count": 2}),
            ),
            (
                "ingest.job.status_checked",
                serde_json::json!({"job_id": "job_01"}),
            ),
        ] {
            let fields = nntune_logging::fields_from_json(value).unwrap();
            let record = LogRecord::new(
                LogWorker::Ingest,
                Level::Info,
                TRACE,
                event,
                fields,
                1_792_094_400_123,
            )
            .unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&record.to_json_line()).unwrap();
            let object = parsed.as_object().unwrap();
            assert_eq!(object.len(), 6);
            assert_eq!(object["worker"], "ingest");
            assert_eq!(object["trace_id"], TRACE);
            assert_eq!(object["event"], event);
            assert_eq!(object["ts"], "2026-10-15T20:00:00.123Z");
            assert_eq!(record.trace_id(), TRACE);
        }
    }

    /// The inbound gate is `resolve_trace_id`: valid IDs pass through for
    /// downstream propagation, everything else is replaced — never trusted.
    #[test]
    fn invalid_inbound_trace_is_replaced_never_trusted() {
        assert_eq!(
            nntune_logging::resolve_trace_id(Some(TRACE)),
            TRACE,
            "a valid inbound ID must survive unchanged"
        );
        for bad in [
            "",
            "short",
            "9F3A4C2E1B5D4F6A8C7E9D0B1A2F4E6C",
            "Bearer abc",
        ] {
            let resolved = nntune_logging::resolve_trace_id(Some(bad));
            assert!(nntune_logging::is_valid_trace_id(&resolved));
            assert_ne!(resolved, bad);
        }
    }

    /// Route-stamped traces ride the queue message to the consumer (which
    /// must not replace a valid ID); builders stay trace-free so dispatch
    /// math is unaffected by observability.
    #[test]
    fn queue_messages_carry_the_request_trace() {
        let mut msg =
            dispatch::build_search_dispatch("user_01", "dark side", "vault_idx_01-ab", &[], 25, 0)
                .unwrap();
        assert_eq!(msg.trace_id, None);
        msg.trace_id = Some(TRACE.to_string());
        let back: QueueMsg = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(back.trace_id.as_deref(), Some(TRACE));
    }

    /// Negative privacy coverage: the exact field shapes the routes emit
    /// carry only opaque IDs, counts, and fixed labels — no user handles,
    /// query text, credentials, or network identifiers. Secret-like shapes
    /// are hard-rejected by the emitter itself.
    #[test]
    fn route_field_shapes_carry_no_handles_or_secrets() {
        const ALLOWED: &[&str] = &[
            "job_id",
            "q_len",
            "file_count",
            "segment_count",
            "source",
            "status_code",
        ];
        for value in [
            serde_json::json!({"job_id": "job_aaa", "q_len": 9}),
            serde_json::json!({"job_id": "job_aaa", "file_count": 1, "segment_count": 2}),
            serde_json::json!({"job_id": "job_aaa"}),
            serde_json::json!({"job_id": "job_aaa", "source": "nzb_upload"}),
            serde_json::json!({"source": "nzb_upload"}),
        ] {
            let fields = nntune_logging::fields_from_json(value).unwrap();
            for key in fields.keys() {
                assert!(
                    ALLOWED.contains(&key.as_str()),
                    "unexpected route field {key}"
                );
            }
        }
        // Credential, identity, and network shapes are rejected outright by
        // the emitter. (`user_id` passes the key filter as an opaque handle,
        // but no route shape above attaches it — the allowlist proves it.)
        for value in [
            serde_json::json!({"authorization": "Bearer abc"}),
            serde_json::json!({"client_ip": "192.0.2.1"}),
            serde_json::json!({"api_key": "k"}),
            serde_json::json!({"contact_email": "a@example.com"}),
            serde_json::json!({"query": "dark side", "session_id": "s"}),
        ] {
            assert!(
                nntune_logging::fields_from_json(value).is_err(),
                "emitter accepted a banned shape"
            );
        }
    }
}

#[worker::event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    Router::new()
        .post_async("/v1/ingest/search", |mut req, ctx| async move {
            search(&mut req, &ctx).await
        })
        .post_async("/v1/ingest/nzb", |mut req, ctx| async move {
            upload_nzb(&mut req, &ctx).await
        })
        .get_async("/v1/ingest/jobs/:id", |req, ctx| async move {
            job_status(&req, &ctx).await
        })
        .or_else_any_method_async("/*catchall", |req, _| async move {
            let trace_id = inbound_trace_id(&req);
            json_error("not found", 404, &trace_id)
        })
        .run(req, env)
        .await
}
