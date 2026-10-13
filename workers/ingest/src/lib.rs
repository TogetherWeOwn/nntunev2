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
//! library rows. Routes validate the contract and enqueue to
//! `nntune-ingest-jobs`; the Queue consumer (Workflow steps in
//! `queue::WORKFLOW_STEPS`) runs parse → probe → verify → repair → match →
//! write. See `README.md`.

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
use newznab::MAX_LIMIT;
use nzb_intake::{estimate_nzb, IntakeRejection};
use queue::{validate_indexer_ref, NzbUploadRequest, QueueMsg, SearchRequest};

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

/// Send one dispatch message. Returns true when the Queue accepted it; a
/// missing or failing binding keeps the 202 contract but reports `not_wired`
/// so callers (and tail logs) distinguish accepted-from-validated.
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

fn queued_envelope(
    msg: &QueueMsg,
    source: &str,
    queued: bool,
    extra: serde_json::Value,
) -> Result<Response> {
    let mut body = serde_json::json!({
        "stage": "queued",
        "source": source,
        "status": if queued { "queued" } else { "not_wired" },
        "job_id": msg.job_id,
        "idempotency_key": msg.idempotency_key,
    });
    if !queued {
        body["hint"] = serde_json::Value::String(
            "Queue unavailable; contract is validated, retry shortly.".into(),
        );
    }
    if let serde_json::Value::Object(map) = extra {
        if let serde_json::Value::Object(ref mut body_map) = body {
            body_map.extend(map);
        }
    }
    accepted(body)
}

/// Validate a search body and enqueue it; the Queue consumer runs the
/// search → parse → probe → verify → match → write workflow.
async fn search(req: &mut Request, ctx: &RouteContext<()>) -> Result<Response> {
    let text = req.text().await.unwrap_or_default();
    let parsed: SearchRequest =
        serde_json::from_str(&text).map_err(|_| "body must be SearchRequest JSON")?;
    let q = parsed.query.trim();
    if q.is_empty() || q.len() > 200 {
        return json_error("query must be 1..=200 chars", 400);
    }
    if !validate_indexer_ref(&parsed.indexer_ref) {
        return json_error("indexer_ref must be a vault handle", 400);
    }
    if parsed.limit == 0 || parsed.limit > MAX_LIMIT {
        return json_error("limit must be 1..=100", 400);
    }
    if !validate_user_id(&parsed.user_id) {
        return json_error("user_id must be 1..=128 chars", 400);
    }
    let msg = dispatch::build_search_dispatch(&parsed.user_id, q, &parsed.indexer_ref)
        .map_err(|e| e.to_string());
    let msg = match msg {
        Ok(msg) => msg,
        Err(message) => return json_error(&message, 400),
    };
    console_log!("ingest search q_len={} job={}", q.len(), msg.job_id);
    let queued = try_enqueue(&ctx.env, &msg).await;
    queued_envelope(&msg, "indexer_search", queued, serde_json::json!({}))
}

/// Validate an NZB upload and enqueue it; the consumer parses with
/// `nntune-core`, probes the pool DO, and verifies before matching.
async fn upload_nzb(req: &mut Request, ctx: &RouteContext<()>) -> Result<Response> {
    let text = req.text().await.unwrap_or_default();
    let parsed: NzbUploadRequest =
        serde_json::from_str(&text).map_err(|_| "body must be NzbUploadRequest JSON")?;
    if parsed.filename.is_empty() || parsed.filename.len() > 255 {
        return json_error("filename must be 1..=255 chars", 400);
    }
    if !validate_user_id(&parsed.user_id) {
        return json_error("user_id must be 1..=128 chars", 400);
    }
    match estimate_nzb(&parsed.nzb_xml) {
        Err(IntakeRejection::Empty) => json_error("nzb_xml must not be empty", 400),
        Err(IntakeRejection::TooLarge) => json_error("nzb_xml exceeds 2 MiB", 413),
        Err(IntakeRejection::NotNzb) => json_error("nzb_xml is not an NZB document", 400),
        Err(IntakeRejection::TooManyFiles) => json_error("nzb has too many files", 413),
        Err(IntakeRejection::TooManySegments) => json_error("nzb has too many segments", 413),
        Ok(est) => {
            let msg = dispatch::build_upload_dispatch(&parsed.user_id, &parsed.nzb_xml)
                .map_err(|e| e.to_string());
            let msg = match msg {
                Ok(msg) => msg,
                Err(message) => return json_error(&message, 400),
            };
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
