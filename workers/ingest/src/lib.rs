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
//! library rows. Queue/Workflow bindings and DB wiring land in the next
//! slice; today each route validates its contract and returns the durable
//! envelope so the surface is reviewable first. See `README.md`.

use worker::{console_log, Env, Request, Response, Result, RouteContext, Router};

pub mod jobs;
pub mod newznab;
pub mod nzb_intake;
pub mod queue;
pub mod rate_limit;

use jobs::validate_job_id;
use newznab::MAX_LIMIT;
use nzb_intake::{estimate_nzb, IntakeRejection};
use queue::{validate_indexer_ref, NzbUploadRequest, SearchRequest};

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

/// Validate a search body; Queue enqueue lands in the next slice.
async fn search(req: &mut Request, _ctx: &RouteContext<()>) -> Result<Response> {
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
    console_log!("ingest search q_len={}", q.len());
    accepted(serde_json::json!({
        "stage": "queued",
        "source": "indexer_search",
        "status": "not_wired",
        "hint": "Queue enqueue lands in the next slice; contract is idempotent search-then-parse.",
    }))
}

/// Validate an NZB upload; pool probe + verify land in the next slice.
async fn upload_nzb(req: &mut Request, _ctx: &RouteContext<()>) -> Result<Response> {
    let text = req.text().await.unwrap_or_default();
    let parsed: NzbUploadRequest =
        serde_json::from_str(&text).map_err(|_| "body must be NzbUploadRequest JSON")?;
    if parsed.filename.is_empty() || parsed.filename.len() > 255 {
        return json_error("filename must be 1..=255 chars", 400);
    }
    match estimate_nzb(&parsed.nzb_xml) {
        Err(IntakeRejection::Empty) => json_error("nzb_xml must not be empty", 400),
        Err(IntakeRejection::TooLarge) => json_error("nzb_xml exceeds 2 MiB", 413),
        Err(IntakeRejection::NotNzb) => json_error("nzb_xml is not an NZB document", 400),
        Err(IntakeRejection::TooManyFiles) => json_error("nzb has too many files", 413),
        Err(IntakeRejection::TooManySegments) => json_error("nzb has too many segments", 413),
        Ok(est) => {
            console_log!(
                "ingest upload files={} segments={}",
                est.file_count,
                est.segment_count
            );
            accepted(serde_json::json!({
                "stage": "queued",
                "source": "nzb_upload",
                "file_count": est.file_count,
                "segment_count": est.segment_count,
                "status": "not_wired",
                "hint": "nntune-core parse + pool probe land in the next slice.",
            }))
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
        "hint": "Checkpoint reads land with the Queue/Workflow slice.",
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
