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
/// a short category list, never content. Checked on `Content-Length` before a
/// byte is buffered and again after the read (missing header must not widen
/// the hole).
pub const MAX_SEARCH_BODY_BYTES: usize = 16 * 1024;
/// Largest accepted `POST /v1/ingest/nzb` JSON body. The decoded NZB is capped
/// at [`MAX_NZB_BYTES`], but JSON escaping can inflate hostile input ~6x on
/// the wire, so the wire cap accounts for that before buffering.
pub const MAX_UPLOAD_BODY_BYTES: usize = 13 * 1024 * 1024;

#[derive(Debug, serde::Serialize)]
struct ApiError {
    error: String,
}

fn json_error(message: &str, status: u16) -> Result<Response> {
    let body = serde_json::to_string(&ApiError {
        error: message.to_string(),
    })
    .unwrap_or_else(|_| r#"{"error":"internal"}"#.to_string());
    Response::from_body(worker::ResponseBody::Body(body.into_bytes())).map(|r| {
        r.with_status(status)
            .with_headers(headers_json().unwrap_or_default())
    })
}

fn headers_json() -> Result<worker::Headers> {
    let mut headers = worker::Headers::new();
    headers.append("content-type", "application/json; charset=utf-8")?;
    Ok(headers)
}

fn accepted(body: serde_json::Value) -> Result<Response> {
    Ok(Response::from_json(&body)?.with_status(202))
}

fn service_unavailable(message: &str) -> Result<Response> {
    let body = serde_json::to_string(&ApiError {
        error: message.to_string(),
    })
    .unwrap_or_else(|_| r#"{"error":"internal"}"#.to_string());
    let mut headers = headers_json().unwrap_or_default();
    headers.append("retry-after", "5")?;
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

/// Enforce a wire cap both before buffering (declared length) and after the
/// read (lying or missing header). Pure so the policy is unit-tested.
fn check_body_cap(
    text_len: usize,
    declared_len: Option<usize>,
    cap: usize,
) -> core::result::Result<(), &'static str> {
    if declared_len.is_some_and(|len| len > cap) {
        return Err("request body too large");
    }
    if text_len > cap {
        return Err("request body too large");
    }
    Ok(())
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
fn queued_envelope(
    msg: &QueueMsg,
    source: &str,
    queued: bool,
    extra: serde_json::Value,
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
        let mut headers = headers_json().unwrap_or_default();
        headers.append("retry-after", "5")?;
        return Response::from_json(&body).map(|r| r.with_status(503).with_headers(headers));
    }
    accepted(body)
}

/// Persist upload bytes under their content-addressed key before enqueue, so
/// the Queue consumer can fetch the exact bytes the intake guard approved.
/// `false` means the payload is not durably stored: the route answers 503
/// and never enqueues.
async fn store_upload_payload(env: &Env, payload_ref: &str, nzb_xml: &str) -> bool {
    match env.kv(dispatch::PAYLOAD_KV_BINDING) {
        Ok(store) => match store.put(payload_ref, nzb_xml.to_string()) {
            Ok(put) => match put.execute().await {
                Ok(()) => true,
                Err(e) => {
                    console_log!("ingest payload write failed: {:?}", e);
                    false
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
    let declared = content_length(req);
    if declared.is_some_and(|len| len > MAX_SEARCH_BODY_BYTES) {
        return json_error("request body too large", 413);
    }
    let text = match req.text().await {
        Ok(text) => text,
        Err(_) => return json_error("failed to read request body", 400),
    };
    if let Err(message) = check_body_cap(text.len(), declared, MAX_SEARCH_BODY_BYTES) {
        return json_error(message, 413);
    }
    let parsed = match parse_search(&text) {
        Ok(parsed) => parsed,
        Err(message) => return json_error(message, 400),
    };
    if let Err(message) = check_search(&parsed) {
        return json_error(message, 400);
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
        Err(message) => return json_error(message, 400),
    };
    console_log!("ingest search q_len={} job={}", q.len(), msg.job_id);
    let queued = try_enqueue(&ctx.env, &msg).await;
    queued_envelope(&msg, "indexer_search", queued, serde_json::json!({}))
}

/// Validate an NZB upload and enqueue it; the consumer parses with
/// `nntune-core`, probes the pool DO, and verifies before matching.
async fn upload_nzb(req: &mut Request, ctx: &RouteContext<()>) -> Result<Response> {
    let declared = content_length(req);
    if declared.is_some_and(|len| len > MAX_UPLOAD_BODY_BYTES) {
        return json_error("request body too large", 413);
    }
    let text = match req.text().await {
        Ok(text) => text,
        Err(_) => return json_error("failed to read request body", 400),
    };
    if let Err(message) = check_body_cap(text.len(), declared, MAX_UPLOAD_BODY_BYTES) {
        return json_error(message, 413);
    }
    let parsed = match parse_upload(&text) {
        Ok(parsed) => parsed,
        Err(message) => return json_error(message, 400),
    };
    if let Err(message) = check_upload(&parsed) {
        return json_error(message, 400);
    }
    match estimate_nzb(&parsed.nzb_xml) {
        Err(IntakeRejection::Empty) => json_error("nzb_xml must not be empty", 400),
        Err(IntakeRejection::TooLarge) => json_error("nzb_xml exceeds 2 MiB", 413),
        Err(IntakeRejection::NotNzb) => json_error("nzb_xml is not an NZB document", 400),
        Err(IntakeRejection::TooManyFiles) => json_error("nzb has too many files", 413),
        Err(IntakeRejection::TooManySegments) => json_error("nzb has too many segments", 413),
        Ok(est) => {
            let msg = match dispatch::build_upload_dispatch(&parsed.user_id, &parsed.nzb_xml) {
                Ok(msg) => msg,
                Err(message) => return json_error(message, 400),
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
        return json_error("unknown job id", 404);
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
    fn body_caps_reject_declared_and_actual_overruns() {
        assert!(check_body_cap(10, Some(10), 16).is_ok());
        assert!(check_body_cap(10, Some(17), 16).is_err());
        assert!(check_body_cap(17, None, 16).is_err());
        assert!(check_body_cap(17, Some(10), 16).is_err());
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
        .or_else_any_method_async("/*catchall", |_, _| async { json_error("not found", 404) })
        .run(req, env)
        .await
}
