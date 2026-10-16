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
use worker::{console_log, Env, Request, Response, Result, RouteContext, Router};

pub mod api_error;
pub mod dispatch;
pub mod fixtures;
pub mod jobs;
pub mod library;
pub mod metering;
pub mod newznab;
pub mod nzb_intake;
pub mod parse;
pub mod pool_probe;
pub mod probe;
pub mod queue;
pub mod rate_limit;
pub mod repair;

use api_error::ApiError;
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

fn json_error(err: ApiError) -> Result<Response> {
    let status = err.status;
    Response::from_body(worker::ResponseBody::Body(err.body_json().into_bytes())).map(|r| {
        r.with_status(status)
            .with_headers(headers_json().unwrap_or_default())
    })
}

fn headers_json() -> Result<worker::Headers> {
    let headers = worker::Headers::new();
    headers.append("content-type", "application/json; charset=utf-8")?;
    Ok(headers)
}

fn accepted(body: serde_json::Value) -> Result<Response> {
    Ok(Response::from_json(&body)?.with_status(202))
}

fn service_unavailable(message: &str) -> Result<Response> {
    let headers = headers_json().unwrap_or_default();
    headers.append("retry-after", "5")?;
    let body = ApiError::service_unavailable(message).body_json();
    Response::from_body(worker::ResponseBody::Body(body.into_bytes()))
        .map(|r| r.with_status(503).with_headers(headers))
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
async fn read_capped_text(req: &mut Request, cap: usize) -> core::result::Result<String, ApiError> {
    if let Err(message) = check_declared_len(content_length(req), cap) {
        return Err(ApiError::payload_too_large(message));
    }
    let stream = req
        .stream()
        .map_err(|_| ApiError::invalid_request("failed to read request body"))?;
    let mut stream = Box::pin(stream);
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ApiError::invalid_request("failed to read request body"))?;
        push_chunk(&mut buf, &chunk, cap).map_err(ApiError::payload_too_large)?;
    }
    String::from_utf8(buf).map_err(|_| ApiError::invalid_request("request body must be UTF-8"))
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

/// Build the status + JSON body for an intake refusal. Pure so the 4xx
/// taxonomy (registry `code`, stable `intake_code`, measured value + cap fields
/// the client renders) is unit-tested without a Worker runtime.
fn intake_rejection_parts(rejection: &IntakeRejection) -> (u16, serde_json::Value) {
    let error = ApiError::from(*rejection);
    let status = error.status;
    let mut body = serde_json::json!({
        "code": error.code,
        "message": error.message,
        "retryable": error.retryable,
        "error": rejection.message(),
    });
    body["intake_code"] = serde_json::Value::String(rejection.code().into());
    body["intake_config_version"] = nzb_intake::INTAKE_CONFIG_VERSION.into();
    match *rejection {
        IntakeRejection::TooLarge {
            size_bytes,
            max_bytes,
        } => {
            body["size_bytes"] = size_bytes.into();
            body["max_bytes"] = max_bytes.into();
        }
        IntakeRejection::TooManyFiles { count, max } => {
            body["file_count"] = count.into();
            body["max_files"] = max.into();
        }
        IntakeRejection::TooManySegments { count, max } => {
            body["segment_count"] = count.into();
            body["max_segments"] = max.into();
        }
        IntakeRejection::DeclaredBytesOverCap { declared, max } => {
            body["declared_bytes"] = declared.into();
            body["max_declared_bytes"] = max.into();
        }
        IntakeRejection::Empty | IntakeRejection::NotNzb | IntakeRejection::Malformed => {}
    }
    (status, body)
}

/// Answer an intake refusal with its stable code + reason fields.
fn intake_rejection_response(rejection: &IntakeRejection) -> Result<Response> {
    let (status, body) = intake_rejection_parts(rejection);
    Response::from_json(&body).map(|r| {
        r.with_status(status)
            .with_headers(headers_json().unwrap_or_default())
    })
}

/// Send one dispatch message. Returns true when the Queue accepted it; a
/// missing or failing binding returns false and the route answers 503 (see
/// [`queued_envelope`]), so the job is retried instead of silently lost.
async fn try_enqueue(env: &Env, msg: &QueueMsg) -> bool {
    match env.queue(dispatch::INGEST_QUEUE_BINDING) {
        Ok(queue) => match queue.send(msg.clone()).await {
            Ok(()) => true,
            Err(e) => {
                console_log!("ingest enqueue failed: {:?}", e);
                false
            }
        },
        Err(e) => {
            console_log!("ingest queue binding missing: {:?}", e);
            false
        }
    }
}

/// Answer an enqueue result. Success is 202 `queued`; a missing or failing
/// Queue binding is 503 with `Retry-After` — never 202 — so clients, proxies,
/// and retry middleware do not mistake a lost job for an accepted one.
/// `status: queued` is only ever sent after a successful `send`.
/// Pure body builder behind [`queued_envelope`]: the 503 `!queued` path must
/// always carry the shared `{code, message, retryable}` envelope fields
/// alongside the job correlation fields (see docs/api-errors.md). Split out
/// so the envelope fields are unit-tested without a `worker::Response`.
fn queued_body(
    msg: &QueueMsg,
    source: &str,
    queued: bool,
    extra: serde_json::Value,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "stage": "queued",
        "source": source,
        "status": if queued { "queued" } else { "retry" },
        "job_id": msg.job_id,
        "idempotency_key": msg.idempotency_key,
    });
    if !queued {
        // Error path: the shared envelope fields ride alongside the job
        // correlation fields above (extra fields are allowed, see
        // docs/api-errors.md).
        body["code"] = serde_json::Value::String("service_unavailable".into());
        body["message"] = serde_json::Value::String(
            "Queue unavailable; nothing was accepted, retry shortly.".into(),
        );
        body["retryable"] = serde_json::Value::Bool(true);
        body["hint"] = body["message"].clone();
    }
    if let serde_json::Value::Object(map) = extra {
        if let serde_json::Value::Object(ref mut body_map) = body {
            body_map.extend(map);
        }
    }
    body
}

fn queued_envelope(
    msg: &QueueMsg,
    source: &str,
    queued: bool,
    extra: serde_json::Value,
) -> Result<Response> {
    let body = queued_body(msg, source, queued, extra);
    if !queued {
        let headers = headers_json().unwrap_or_default();
        headers.append("retry-after", "5")?;
        return Response::from_json(&body).map(|r| r.with_status(503).with_headers(headers));
    }
    accepted(body)
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
async fn store_upload_payload(env: &Env, payload_ref: &str, nzb_xml: &str) -> bool {
    match env.kv(dispatch::PAYLOAD_KV_BINDING) {
        Ok(store) => match store.put(payload_ref, nzb_xml.to_string()) {
            Ok(put) => match put
                .expiration_ttl(dispatch::PAYLOAD_TTL_SECS)
                .execute()
                .await
            {
                Ok(()) => true,
                Err(e) => {
                    console_log!("ingest payload write failed: {:?}", e);
                    matches!(store.get(payload_ref).text().await, Ok(Some(_)))
                }
            },
            Err(e) => {
                console_log!("ingest payload put failed: {:?}", e);
                false
            }
        },
        Err(e) => {
            console_log!("ingest payload KV missing: {:?}", e);
            false
        }
    }
}

/// Validate a search body and enqueue it; the Queue consumer runs the
/// search → parse → probe → verify → match → write workflow.
async fn search(req: &mut Request, ctx: &RouteContext<()>) -> Result<Response> {
    let text = match read_capped_text(req, MAX_SEARCH_BODY_BYTES).await {
        Ok(text) => text,
        Err(err) => return json_error(err),
    };
    let parsed = match parse_search(&text) {
        Ok(parsed) => parsed,
        Err(message) => return json_error(ApiError::invalid_request(message)),
    };
    if let Err(message) = check_search(&parsed) {
        return json_error(ApiError::invalid_request(message));
    }
    let q = parsed.query.trim().to_string();
    let msg = match dispatch::build_search_dispatch(
        &parsed.user_id,
        &q,
        &parsed.indexer_ref,
        &parsed.categories,
        parsed.limit,
        parsed.page,
    ) {
        Ok(msg) => msg,
        Err(message) => return json_error(ApiError::invalid_request(message)),
    };
    console_log!("ingest search q_len={} job={}", q.len(), msg.job_id);
    let queued = try_enqueue(&ctx.env, &msg).await;
    queued_envelope(&msg, "indexer_search", queued, serde_json::json!({}))
}

/// Validate an NZB upload and enqueue it; the consumer parses with
/// `nntune-core`, probes the pool DO, and verifies before matching.
async fn upload_nzb(req: &mut Request, ctx: &RouteContext<()>) -> Result<Response> {
    let text = match read_capped_text(req, MAX_UPLOAD_BODY_BYTES).await {
        Ok(text) => text,
        Err(err) => return json_error(err),
    };
    let parsed = match parse_upload(&text) {
        Ok(parsed) => parsed,
        Err(message) => return json_error(ApiError::invalid_request(message)),
    };
    if let Err(message) = check_upload(&parsed) {
        return json_error(ApiError::invalid_request(message));
    }
    match estimate_nzb(&parsed.nzb_xml) {
        Err(rejection) => intake_rejection_response(&rejection),
        Ok(est) => {
            let msg = match dispatch::build_upload_dispatch(&parsed.user_id, &parsed.nzb_xml) {
                Ok(msg) => msg,
                Err(message) => return json_error(ApiError::invalid_request(message)),
            };
            // The bytes must be durably stored before the job exists: an
            // enqueued job without retrievable bytes can never run.
            let payload_ref = msg.payload_ref.clone().unwrap_or_default();
            if payload_ref.is_empty()
                || !store_upload_payload(&ctx.env, &payload_ref, &parsed.nzb_xml).await
            {
                return service_unavailable("payload store unavailable; nothing was accepted");
            }
            console_log!(
                "ingest upload files={} segments={} job={}",
                est.file_count,
                est.segment_count,
                msg.job_id
            );
            let queued = try_enqueue(&ctx.env, &msg).await;
            queued_envelope(
                &msg,
                "nzb_upload",
                queued,
                serde_json::json!({
                    "file_count": est.file_count,
                    "segment_count": est.segment_count,
                }),
            )
        }
    }
}

/// Job checkpoint status: validates the id shape before DB wiring lands.
async fn job_status(_req: &Request, ctx: &RouteContext<()>) -> Result<Response> {
    let id = ctx.param("id").cloned().unwrap_or_default();
    if !validate_job_id(&id) {
        return json_error(ApiError::not_found("unknown job id"));
    }
    Response::from_json(&serde_json::json!({
        "job_id": id,
        "status": "not_wired",
        "hint": "Checkpoint reads land with the Queue consumer; enqueue is live.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nzb_intake::{MAX_FILES, MAX_NZB_BYTES};

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
    fn unqueued_body_carries_the_error_envelope_with_job_correlation() {
        // The 503 `!queued` path must always carry the shared
        // `{code, message, retryable}` envelope fields alongside `job_id`
        // (docs/api-errors.md): deleting any one of them must fail here.
        let msg =
            dispatch::build_search_dispatch("user_01", "dark side", "vault_idx_01-ab", &[], 25, 0)
                .expect("dispatch");
        let body = queued_body(&msg, "indexer_search", false, serde_json::json!({}));
        assert_eq!(
            body["code"],
            serde_json::Value::String("service_unavailable".into())
        );
        assert_eq!(
            body["message"],
            serde_json::Value::String(
                "Queue unavailable; nothing was accepted, retry shortly.".into()
            )
        );
        assert_eq!(body["retryable"], serde_json::Value::Bool(true));
        assert_eq!(
            body["job_id"],
            serde_json::Value::String(msg.job_id.clone())
        );
        assert_eq!(body["status"], serde_json::Value::String("retry".into()));

        let ok = queued_body(&msg, "indexer_search", true, serde_json::json!({}));
        assert_eq!(ok["status"], serde_json::Value::String("queued".into()));
        assert_eq!(ok["job_id"], serde_json::Value::String(msg.job_id.clone()));
    }

    #[test]
    fn intake_corpus_maps_to_registered_envelopes_with_reason_fields() {
        // (body, expected status, registry code, stable intake-specific code)
        let over_bytes = r#"<?xml version="1.0"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
<file poster="a@b" date="1" subject="big.iso">
<groups><group>alt.binaries.music</group></groups>
<segments>
<segment bytes="42949672960" number="1">big-one@news</segment>
<segment bytes="42949672960" number="2">big-two@news</segment>
</segments>
</file>
</nzb>"#;
        let mut file_bomb = String::from("<?xml?><nzb>");
        for _ in 0..(MAX_FILES + 1) {
            file_bomb.push_str("<file></file>");
        }
        file_bomb.push_str("</nzb>");
        let mut segment_bomb = String::from("<?xml?><nzb>");
        for _ in 0..(crate::nzb_intake::MAX_SEGMENTS + 1) {
            segment_bomb.push_str("<segment></segment>");
        }
        segment_bomb.push_str("</nzb>");
        assert!(segment_bomb.len() < MAX_NZB_BYTES);
        let cases: &[(&str, u16, &str, &str)] = &[
            ("", 400, "invalid_request", "NZB_EMPTY"),
            (
                "<html><body>hi</body></html>",
                400,
                "invalid_request",
                "NZB_NOT_NZB",
            ),
            (
                "<?xml?><nzb><file>",
                400,
                "invalid_request",
                "NZB_MALFORMED",
            ),
            (
                over_bytes,
                413,
                "payload_too_large",
                "NZB_DECLARED_BYTES_OVER_CAP",
            ),
            (&file_bomb, 413, "payload_too_large", "NZB_TOO_MANY_FILES"),
            (
                &segment_bomb,
                413,
                "payload_too_large",
                "NZB_TOO_MANY_SEGMENTS",
            ),
            (
                &"x".repeat(MAX_NZB_BYTES + 1),
                413,
                "payload_too_large",
                "NZB_TOO_LARGE",
            ),
        ];
        for (body, status, api_code, intake_code) in cases {
            let rejection = estimate_nzb(body).unwrap_err();
            let (actual_status, actual_body) = intake_rejection_parts(&rejection);
            assert_eq!(actual_status, *status, "status for {intake_code}");
            assert_eq!(actual_body["code"], *api_code, "API code for {intake_code}");
            assert_eq!(actual_body["intake_code"], *intake_code);
            assert_eq!(actual_body["message"], ApiError::from(rejection).message);
            assert_eq!(actual_body["retryable"], false);
            assert_eq!(actual_body["error"], actual_body["message"]);
            assert_eq!(actual_status, rejection.http_status());
            assert_eq!(
                actual_body["intake_config_version"],
                crate::nzb_intake::INTAKE_CONFIG_VERSION
            );
        }
        // Reason fields the client renders ride alongside the code.
        let (_, over_bytes_body) = intake_rejection_parts(&estimate_nzb(over_bytes).unwrap_err());
        assert!(over_bytes_body["declared_bytes"].as_u64().unwrap() > 0);
        assert!(over_bytes_body["max_declared_bytes"].as_u64().unwrap() > 0);
        let (_, too_large_body) =
            intake_rejection_parts(&estimate_nzb(&"x".repeat(MAX_NZB_BYTES + 1)).unwrap_err());
        assert_eq!(
            too_large_body["max_bytes"].as_u64().unwrap(),
            MAX_NZB_BYTES as u64
        );
        let (_, too_many_files_body) =
            intake_rejection_parts(&estimate_nzb(&file_bomb).unwrap_err());
        assert_eq!(
            too_many_files_body["file_count"].as_u64().unwrap(),
            (MAX_FILES + 1) as u64
        );
        assert_eq!(
            too_many_files_body["max_files"].as_u64().unwrap(),
            MAX_FILES as u64
        );
        let (_, too_many_segments_body) =
            intake_rejection_parts(&estimate_nzb(&segment_bomb).unwrap_err());
        assert_eq!(
            too_many_segments_body["segment_count"].as_u64().unwrap(),
            (crate::nzb_intake::MAX_SEGMENTS + 1) as u64
        );
        assert_eq!(
            too_many_segments_body["max_segments"].as_u64().unwrap(),
            crate::nzb_intake::MAX_SEGMENTS as u64
        );
        // A valid NZB has no rejection at all: it queues.
        assert!(estimate_nzb(crate::fixtures::SMALL_NZB).is_ok());
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
        .or_else_any_method_async("/*catchall", |_, _| async {
            json_error(ApiError::not_found("not found"))
        })
        .run(req, env)
        .await
}
