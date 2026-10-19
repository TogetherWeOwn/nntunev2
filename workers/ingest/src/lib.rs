//! NNTune v2 ingest pipeline worker.
//!
//! Routes (all JSON):
//! - `GET /healthz` — unauthenticated Worker health check; no bindings.
//! - `POST /v1/ingest/search` — Newznab search with the user's own indexer
//!   (`indexer_ref` vault handle; raw keys are never accepted here).
//! - `POST /v1/ingest/nzb` — NZB upload; guarded by intake caps and parsed
//!   with the bounded `nntune-core` parser before enqueue.
//! - `GET /v1/ingest/jobs/:id` — job checkpoint status (idempotent resume).
//!
//! Pipeline: search/upload → parse (`nntune-core`) → segment availability
//! probe through the user's pool DO → par2 verify (Container batch repair
//! when needed) → tag/fingerprint + MusicBrainz match (`workers/mb`) →
//! library rows. Routes validate the contract, persist upload bytes to the
//! payload KV, enqueue to `nntune-ingest-jobs`, and write the initial job
//! checkpoint for every real admission. The Queue consumer
//! (`src/queue_consumer.rs`; Workflow steps in `queue::WORKFLOW_STEPS`)
//! validates messages and checkpoints and executes the upload
//! parse + pool-probe + verify steps, the repair step (through the
//! `REPAIR_CONTAINER` service binding, fed by verify's staged inputs)
//! and match/write, but its search boundary is not executable yet, so
//! the consumer/workflow entries stay commented out in
//! `wrangler.toml.example` until it lands.
//! See `README.md`.

use futures_util::StreamExt;
use nntune_logging::{Level, LogRecord, Worker as LogWorker};
use worker::{console_log, Env, Request, Response, Result, RouteContext, Router};

pub mod api_error;
pub mod checkpoint;
pub mod dispatch;
pub mod fixtures;
pub mod indexer_fetch;
pub mod jobs;
pub mod library;
pub mod matching;
pub mod metering;
pub mod newznab;
pub mod nzb_fetch;
pub mod nzb_intake;
pub mod parse;
pub mod pool_body;
pub mod pool_deadline;
pub mod pool_probe;
pub mod probe;
pub mod queue;
pub mod queue_consumer;
pub mod rate_limit;
pub mod repair;
pub mod retry;
pub mod verify;

use api_error::ApiError;
use dispatch::validate_user_id;
use jobs::{validate_job_id, JobSource};
use metering::{
    emit_meter, MeterOutcome, MeterSink, MeterWriteError, METER_DATASET, METER_DATASET_BINDING,
};
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

fn health_response() -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .header(http::header::CACHE_CONTROL, "no-store")
        .body(r#"{"ok":true}"#.as_bytes().to_vec())
        .expect("static health response is valid")
}

struct HealthResponseParts {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn health_response_parts() -> HealthResponseParts {
    let response = health_response();
    HealthResponseParts {
        status: response.status().as_u16(),
        headers: response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value
                        .to_str()
                        .expect("static health header is valid")
                        .to_owned(),
                )
            })
            .collect(),
        body: response.into_body(),
    }
}

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

/// Read a deploy-time plain var, ignoring empty values. `BUILD_SHA` is
/// public build identity (see `wrangler.toml.example`): a secret binding
/// is tried first so an operator-supplied value wins, then a plain var,
/// mirroring the api worker's reader.
fn env_string(env: &Env, name: &str) -> Option<String> {
    env.secret(name)
        .ok()
        .map(|s| s.to_string())
        .or_else(|| env.var(name).ok().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
}

/// Build identity for log lines: the 40-char lowercase hex commit supplied
/// at deploy time by the deploy lane via the public `BUILD_SHA` var (no
/// workflow wires it for the ingest Worker yet), or the explicit
/// `"unknown"` marker when absent or invalid (dev, preview without env,
/// unwired lane). Normalized through the shared validator, never logged
/// raw; never fail-closed, so a missing identity degrades the line instead
/// of the request.
pub(crate) fn build_sha_from_env(env: &Env) -> String {
    nntune_logging::parse_build_sha(env_string(env, "BUILD_SHA").as_deref())
}

/// Emit one structured log line for this request (`docs/logging-standard.md`).
/// Field validation runs at construction: a banned key or secret-like value
/// drops the line instead of leaking it. All call-site fields are constants
/// plus opaque IDs and counters, so the drop branch only fires on a code bug.
pub(crate) fn emit(level: Level, trace_id: &str, event: &str, fields: serde_json::Value) {
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

// --- Search abuse guard (roadmap R6) -------------------------------------------

thread_local! {
    /// Per-isolate search buckets. Scope is deliberately honest: this
    /// bounds bursts per isolate, complementing the durable per-plan
    /// metering downstream (see `rate_limit::SearchThrottleStore`).
    static SEARCH_THROTTLE: std::cell::RefCell<rate_limit::SearchThrottleStore> =
        std::cell::RefCell::new(rate_limit::SearchThrottleStore::new());
}

/// Canonical source-IP scope for the search throttle: only a clean IP
/// literal from the Cloudflare-set header survives, else `unknown` (all
/// unidentified callers share one bucket — fail-closed, never a fresh
/// budget per forged header). IPv6 is keyed by its /64 (IPv4-mapped by the
/// embedded IPv4), mirroring the API worker's `rate_limit_ip_key`, so
/// rotation inside one prefix cannot mint fresh budgets. The value keys
/// buckets in memory only; it is never logged.
fn search_source_ip(raw: Option<String>) -> String {
    use std::net::IpAddr;
    match raw {
        Some(value) => match value.trim().parse::<IpAddr>() {
            Ok(IpAddr::V4(v4)) => v4.to_string(),
            Ok(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
                Some(v4) => v4.to_string(),
                None => {
                    let segments = v6.segments();
                    std::net::Ipv6Addr::new(
                        segments[0],
                        segments[1],
                        segments[2],
                        segments[3],
                        0,
                        0,
                        0,
                        0,
                    )
                    .to_string()
                }
            },
            Err(_) => "unknown".to_string(),
        },
        None => "unknown".to_string(),
    }
}

/// The [vars] names `SearchRateLimitConfig::from_vars` reads. One
/// const list so the glue test can pin every name against its field.
const SEARCH_RATE_LIMIT_VAR_NAMES: [&str; 2] =
    ["SEARCH_RATE_LIMIT_MAX", "SEARCH_RATE_LIMIT_WINDOW_SECS"];

/// Cloudflare-set connecting-IP header. One const so the glue test
/// pins the exact name the route reads.
const CONNECTING_IP_HEADER: &str = "cf-connecting-ip";

/// Search throttle tunables from a [vars] lookup; `from_vars` fails
/// safe to the documented defaults on anything missing or invalid.
/// Pure over the lookup closure (mirrors the api worker's
/// `rate_limits_from_lookup`) so a renamed key fails a native test.
fn search_throttle_vars_from_lookup(
    get: impl Fn(&str) -> Option<String>,
) -> std::collections::HashMap<String, String> {
    SEARCH_RATE_LIMIT_VAR_NAMES
        .iter()
        .filter_map(|name| get(name).map(|v| ((*name).to_string(), v)))
        .collect()
}

/// Read the connecting IP through a header lookup: asks for exactly
/// [`CONNECTING_IP_HEADER`], so a renamed header fails a native test
/// instead of silently collapsing every caller into `unknown`.
fn search_connecting_ip(get_header: impl Fn(&str) -> Option<String>) -> Option<String> {
    get_header(CONNECTING_IP_HEADER)
}

/// The search route's throttle decision, extracted over plain inputs
/// (connecting-IP header value, [vars], account, now) so native tests
/// drive the exact function the route calls — `Request`/`Env` need
/// wasm-bindgen and cannot be built in a unit test.
fn search_attempt_decision(
    store: &mut rate_limit::SearchThrottleStore,
    connecting_ip: Option<String>,
    vars: &std::collections::HashMap<String, String>,
    user_id: &str,
    now_s: u64,
) -> rate_limit::SearchDecision {
    let ip = search_source_ip(connecting_ip);
    let key = rate_limit::SearchThrottleKey::new(&ip, user_id);
    let cfg = rate_limit::SearchRateLimitConfig::from_vars(vars);
    store.check(&key, &cfg, now_s)
}

/// The full route glue over plain lookups: header name, [vars]
/// names and the store decision composed exactly as the route runs
/// them. A native test drives THIS fn end to end, so neutering any
/// link (unconditional Allow, renamed var, renamed header) fails.
fn check_search_attempt_in(
    store: &mut rate_limit::SearchThrottleStore,
    get_header: impl Fn(&str) -> Option<String>,
    get_var: impl Fn(&str) -> Option<String>,
    user_id: &str,
    now_s: u64,
) -> rate_limit::SearchDecision {
    let connecting_ip = search_connecting_ip(get_header);
    let vars = search_throttle_vars_from_lookup(get_var);
    search_attempt_decision(store, connecting_ip, &vars, user_id, now_s)
}

/// Run the dual (per-IP + per-account) search gate for one attempt.
/// Thin wasm wrapper: supplies the `Request`/`Env` lookups and the
/// per-isolate store to [`check_search_attempt_in`].
fn check_search_attempt(req: &Request, env: &Env, user_id: &str) -> rate_limit::SearchDecision {
    let now_s = worker::Date::now().as_millis() / 1000;
    SEARCH_THROTTLE.with(|store| {
        check_search_attempt_in(
            &mut store.borrow_mut(),
            |name| req.headers().get(name).ok().flatten(),
            |name| env.var(name).ok().map(|v| v.to_string()),
            user_id,
            now_s,
        )
    })
}

/// 429 for a throttled search: the standard `rate_limited` envelope as a
/// generic zero-arg body (no query, key or caller identity can leak into
/// it) plus `Retry-After`.
fn search_rate_limited_response(retry_after_secs: u64) -> Result<Response> {
    let headers = headers_json().unwrap_or_default();
    headers.append(
        rate_limit::RETRY_AFTER_HEADER,
        &rate_limit::search_retry_after_header(retry_after_secs),
    )?;
    Response::from_body(worker::ResponseBody::Body(
        rate_limit::search_rate_limited_body().into_bytes(),
    ))
    .map(|r| {
        r.with_status(rate_limit::SEARCH_RATE_LIMIT_STATUS)
            .with_headers(headers)
    })
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
    if q.is_empty() || q.chars().count() > 200 {
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

/// Entry workflow stage metered at enqueue, by job source.
///
/// Search jobs enter at `search`; upload jobs skip search and enter at
/// `parse` (see `jobs::advance` from `Queued`). Both are in
/// `queue::WORKFLOW_STEPS`, so the meter write never hits `UnknownStage` here.
fn meter_entry_stage(source: JobSource) -> &'static str {
    match source {
        JobSource::IndexerSearch => "search",
        JobSource::NzbUpload => "parse",
    }
}

/// [`MeterSink`] backed by the `INGEST_METER` Analytics Engine binding.
///
/// A native binding success maps to 200; a binding call failure maps to
/// `Err` (transport failure before any status existed). The event shape stays
/// owned by `metering.rs`: the full [`meter_write_body`] JSON rides in blob1,
/// with the stage as the single index and the attempt as double1 for queries.
struct EdgeMeterSink<'a> {
    dataset: &'a worker::AnalyticsEngineDataset,
}

impl MeterSink for EdgeMeterSink<'_> {
    fn send(&mut self, dataset: &str, body: serde_json::Value) -> Result<u16, String> {
        debug_assert_eq!(dataset, METER_DATASET);
        let stage = body.get("stage").and_then(|v| v.as_str()).unwrap_or("");
        let attempt = body.get("attempt").and_then(|v| v.as_u64()).unwrap_or(0) as f64;
        let job_id = body.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
        let user_id = body.get("user_id").and_then(|v| v.as_str()).unwrap_or("");
        let outcome = body.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
        let event_json = body.to_string();
        let point = worker::AnalyticsEngineDataPointBuilder::new()
            .indexes([stage])
            .blobs([event_json.as_str(), job_id, user_id, outcome])
            .doubles([attempt])
            .build();
        match self.dataset.write_data_point(&point) {
            Ok(()) => Ok(200),
            Err(e) => Err(format!("{e:?}")),
        }
    }
}

/// Write one stage-transition meter event through the `INGEST_METER` binding.
///
/// A missing binding or a failing write returns a retryable
/// [`MeterWriteError`] (the route answers 503 + `Retry-After`, never
/// 202-and-dropped). An unknown stage is returned before the binding is
/// touched. The unused `_body_hint` keeps the shape-ownership link explicit:
/// the wire body is always [`meter_write_body`], via [`emit_meter`].
pub(crate) fn try_meter(
    env: &Env,
    msg: &QueueMsg,
    stage: &str,
    outcome: MeterOutcome,
) -> Result<(), MeterWriteError> {
    let dataset = match env.analytics_engine(METER_DATASET_BINDING) {
        Ok(dataset) => dataset,
        Err(e) => {
            console_log!("ingest meter binding missing: {:?}", e);
            return Err(MeterWriteError::Transport(format!(
                "meter binding {METER_DATASET_BINDING} missing: {e:?}"
            )));
        }
    };
    let mut sink = EdgeMeterSink { dataset: &dataset };
    match emit_meter(&mut sink, msg, stage, outcome) {
        Ok(()) => Ok(()),
        Err(e) => {
            console_log!("ingest meter write failed: {}", e);
            Err(e)
        }
    }
}

/// Answer a meter-write failure. The status is derived from
/// [`MeterWriteError::http_status`] — the single source of truth — so the
/// route mapping can never drift from the tested error mapping: retryable
/// failures (missing/failing binding or ledger) surface 503 + `Retry-After`,
/// never 202-and-dropped; an unknown stage is a caller bug and surfaces 400.
fn meter_failure_response(err: &MeterWriteError) -> Result<Response> {
    match err.http_status() {
        503 => service_unavailable("meter unavailable; nothing was accepted, retry shortly"),
        _ => json_error(ApiError::invalid_request("unknown meter stage")),
    }
}

/// Durable metered admission: the (`job_id`, `stage`) ledger owns the
/// enqueue + meter outbox.
///
/// Background: the entry-stage meter used to be written before the Queue
/// send. When the send failed, the route answered 503 and the client
/// retried; the retry rebuilt the same deterministic `job_id`
/// (`dispatch::make_job_id`, `attempt` always 1) and wrote a second
/// identical `Ok` meter event. The dataset then held an event for a job
/// that was never accepted and overcounted the accepted stage, against
/// `metering.rs` ("one event per stage transition"). Analytics Engine is
/// an append-only projection with no query path in this repo, so it can
/// never be the authority behind the idempotency: the ledger below is.
///
/// Protocol per admission (`drive_metered_admission`):
/// 1. `record_stage_once` ensures the row exists (`pending`), then runs one
///    atomic `UPDATE ... WHERE (pending OR claimed-but-expired) RETURNING`
///    as the claim. Postgres serializes concurrent UPDATEs on the same row
///    (row lock + predicate re-check), so exactly one claimant wins; every
///    statement is single-transaction autocommit with no session state held
///    across awaits, which is what makes it Hyperdrive-safe (transaction
///    mode pools one origin connection per transaction; session advisory
///    locks cannot survive that). A loser gets `Busy` and answers 503 +
///    Retry-After without queue/meter side effects. A storage failure also
///    answers 503 (fail-closed). Lease bound, stated honestly: the claim is
///    single-flight only while the owner is live within its 30s lease. An
///    owner stalled past the lease (isolate stall, not just death) lets a
///    retry reclaim and re-drive concurrently, so a pathological >30s stall
///    can still double-drive queue + meter. A monotonically increasing claim
///    epoch fences stale commit/release writes, so only the current owner can
///    transition the row. It cannot retract an already-written meter event or
///    Queue send; those side effects remain part of the lease/crash-window
///    bound. Normal admissions complete in milliseconds, making the window
///    narrow by construction.
/// 2. An existing recent `queued` mark means a previous attempt finished the
///    whole outbox (Queue accepted AND meter written): replay 202 without
///    re-queueing or re-metering. This closes the ordinary retry race. After
///    15 days, queued rows can be claimed again: Cloudflare Queue retention is
///    at most 14 days, so any message from that admission has expired before
///    the replay window opens. Re-admission emits a fresh stage event; bounded
///    replay can therefore produce a later event for the same deterministic
///    job id, but prevents a permanently acknowledged message loss.
/// 3. Otherwise the Queue send runs first, so the meter outcome below is
///    truthful: a failed send answers 503 with no meter event at all (the
///    claim is released back to `pending`; the retry resumes here and still
///    owes exactly one `Ok`).
/// 4. On a successful send exactly one `Ok` meter event is written. A
///    meter failure answers 503 with the claim released to `pending`: the
///    Queue already holds one message (an orphan), so the retry re-queues
///    and re-meters. The duplicate Queue message is harmless by
///    construction (the consumer converges on `idempotency_key` —
///    `library.rs` `UNIQUE(idempotency_key)` — plus `jobs::JobCheckpoint`
///    resume and Queue redelivery), while the meter count stays exact.
///    Marking first and metering second would risk a silent drop instead;
///    this order risks at most a wasted queue slot, never a lost event.
/// 5. `mark_queued` commits `claimed -> queued`; only then is 202 answered.
///    Crash-window honesty: the dataset write and the ledger commit are
///    two systems with no shared transaction, so an isolate that dies
///    between them can leave `claimed` behind a written event; lease expiry
///    (30s) lets the retry reclaim and re-drive (at most a duplicate `Ok`
///    in that narrow window, never a silent drop — this slice forbids
///    drops).
///
/// Production uses the `metered_admission` table in the existing
/// `LIBRARY_DB` Hyperdrive database (`migrations/0009_*` through `0012_*`) for
/// durable marks. The cross-isolate atomic claim is the single-statement
/// `UPDATE ... RETURNING` in `admission_claim_sql`; no connection, lock,
/// transaction, or temp state is held across the queue/meter awaits.
///
/// Outbox mark for one (`job_id`, `stage`) admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum AdmissionMark {
    /// Intent recorded; the Queue has not observably accepted the job and
    /// no `Ok` meter event is owed yet.
    Pending,
    /// Another request currently owns the admission attempt; not persisted.
    Busy,
    /// Queue accepted AND the entry `Ok` meter event was written: the
    /// admission is complete and replays as 202 with no further writes.
    Queued,
}

/// Authoritative durable ledger keyed by (`job_id`, `stage`).
///
/// Production: [`PgAdmissionLedger`] over Hyperdrive Postgres. Atomic
/// ownership comes from the single-statement `UPDATE ... RETURNING` claim
/// plus the table primary key for durable identity — never from session
/// state. Tests drive the same seam with in-memory and file-backed
/// doubles (see `mod tests`).
trait AdmissionLedger {
    /// Atomically claim (`job_id`, `stage`) before reading or creating the
    /// durable mark. A competing request returns `Busy` without side effects.
    async fn record_stage_once(
        &mut self,
        job_id: &str,
        stage: &str,
    ) -> Result<AdmissionMark, MeterWriteError>;
    /// Commit a `claimed` admission to `queued` after the Queue send was
    /// accepted and the `Ok` meter event was written.
    async fn mark_queued(&mut self, job_id: &str, stage: &str) -> Result<(), MeterWriteError>;
    /// Release a `claimed` admission back to `pending` after a failed
    /// attempt; an expired lease also lets the next retry reclaim it, so a
    /// lost isolate never blocks admission forever. Best-effort: ignored
    /// errors still converge via lease expiry.
    async fn release_claim(&mut self, job_id: &str, stage: &str);
}

/// Seconds a `claimed` admission stays owned before the next retry may
/// reclaim it. Must match the `INTERVAL` in [`admission_claim_sql`]; the
/// SQL test below pins both to the same value.
const ADMISSION_CLAIM_LEASE_SECS: u64 = 30;

#[allow(dead_code)]
fn admission_claim_lease_secs() -> u64 {
    ADMISSION_CLAIM_LEASE_SECS
}

/// Parameterized statements owned here (execution lives with the ledger
/// impls); see migrations `0009`–`0012` for the table and replay window.
/// Every statement is single-transaction autocommit with `$1` = job id and
/// `$2` = stage: no `BEGIN`, no session lock, no temp state, so each one is
/// safe under Hyperdrive transaction-mode pooling.
fn admission_ensure_sql() -> &'static str {
    "INSERT INTO metered_admission (job_id, stage, state) VALUES ($1, $2, 'pending') \
     ON CONFLICT (job_id, stage) DO NOTHING"
}

fn admission_claim_sql() -> &'static str {
    "UPDATE metered_admission SET state = 'claimed', claimed_at = now(), claim_epoch = claim_epoch + 1 \
     WHERE job_id = $1 AND stage = $2 \
     AND (state = 'pending' \
       OR (state = 'claimed' AND claimed_at < now() - INTERVAL '30 seconds') \
       OR (state = 'queued' AND queued_at < now() - INTERVAL '15 days')) \
     RETURNING claim_epoch"
}

fn admission_read_mark_sql() -> &'static str {
    // UPDATE ... RETURNING is not cacheable by Hyperdrive. A no-op assignment
    // gives a fresh conflict-state read after a failed claim.
    "UPDATE metered_admission SET state = state WHERE job_id = $1 AND stage = $2 RETURNING state"
}

fn admission_mark_queued_sql() -> &'static str {
    "UPDATE metered_admission SET state = 'queued', claimed_at = NULL, queued_at = now() \
     WHERE job_id = $1 AND stage = $2 AND state = 'claimed' AND claim_epoch = $3"
}

fn admission_release_sql() -> &'static str {
    "UPDATE metered_admission SET state = 'pending', claimed_at = NULL \
     WHERE job_id = $1 AND stage = $2 AND state = 'claimed' AND claim_epoch = $3"
}

/// [`AdmissionLedger`] over the `LIBRARY_DB` Hyperdrive database.
///
/// Every method opens a short-lived connection, runs one autocommit
/// statement batch, and drops it before returning — nothing is held across
/// the queue/meter awaits. A connection hiccup fails closed as 503, never
/// the isolate. Connection detail never reaches logs or errors.
struct PgAdmissionLedger {
    env: Env,
    claim_epoch: Option<i64>,
}

/// Open one short-lived client on the `LIBRARY_DB` Hyperdrive database.
///
/// Shared by the admission ledger and the consumer's library upsert: the
/// caller drops the client before awaiting anything else, so no session
/// state crosses an await. The error is deliberately detail-free —
/// connection strings, hosts, and driver messages never reach logs or
/// responses.
pub(crate) async fn library_db_client(env: &Env) -> Result<tokio_postgres::Client, ()> {
    let hyperdrive = env
        .hyperdrive(crate::library::LIBRARY_DB_BINDING)
        .map_err(|e| {
            console_log!("library db binding missing: {:?}", e);
        })?;
    let socket = worker::Socket::builder()
        .secure_transport(worker::SecureTransport::StartTls)
        .connect(hyperdrive.host(), hyperdrive.port())
        .map_err(|_| ())?;
    let config: tokio_postgres::Config = hyperdrive.connection_string().parse().map_err(|_| ())?;
    let (client, connection) = config
        .connect_raw(socket, worker::postgres_tls::PassthroughTls)
        .await
        .map_err(|_| ())?;
    wasm_bindgen_futures::spawn_local(async move {
        if connection.await.is_err() {
            console_log!("library db connection error");
        }
    });
    Ok(client)
}

impl PgAdmissionLedger {
    fn from_env(env: &Env) -> Self {
        Self {
            env: env.clone(),
            claim_epoch: None,
        }
    }

    async fn client(&self) -> Result<tokio_postgres::Client, MeterWriteError> {
        library_db_client(&self.env)
            .await
            .map_err(|()| MeterWriteError::Transport("admission ledger unavailable".to_string()))
    }
}

impl AdmissionLedger for PgAdmissionLedger {
    async fn record_stage_once(
        &mut self,
        job_id: &str,
        stage: &str,
    ) -> Result<AdmissionMark, MeterWriteError> {
        let unavailable = || MeterWriteError::Transport("admission ledger unavailable".to_string());
        // One short-lived connection for the whole claim sequence; it is
        // dropped before the queue/meter awaits, so no session state ever
        // crosses them. Atomicity comes from the single UPDATE claim below,
        // not from holding a connection.
        let client = self.client().await?;
        client
            .execute(admission_ensure_sql(), &[&job_id, &stage])
            .await
            .map_err(|_| unavailable())?;
        let claimed = client
            .query_opt(admission_claim_sql(), &[&job_id, &stage])
            .await
            .map_err(|_| unavailable())?;
        if let Some(row) = claimed {
            self.claim_epoch = Some(row.get(0));
            return Ok(AdmissionMark::Pending);
        }
        let row = client
            .query_opt(admission_read_mark_sql(), &[&job_id, &stage])
            .await
            .map_err(|_| unavailable())?;
        match row {
            Some(row) => {
                let state: String = row.get(0);
                if state == "queued" {
                    Ok(AdmissionMark::Queued)
                } else {
                    // `pending` here means our claim lost the race after the
                    // ensure; `claimed` means a live owner or an unexpired
                    // lease. Either way this request must not send or meter.
                    Ok(AdmissionMark::Busy)
                }
            }
            None => Err(unavailable()),
        }
    }

    async fn mark_queued(&mut self, job_id: &str, stage: &str) -> Result<(), MeterWriteError> {
        let claim_epoch = self.claim_epoch.ok_or_else(|| {
            MeterWriteError::Transport("admission ledger unavailable".to_string())
        })?;
        let client = self
            .client()
            .await
            .map_err(|_| MeterWriteError::Transport("admission ledger unavailable".to_string()))?;
        let updated = client
            .execute(
                admission_mark_queued_sql(),
                &[&job_id, &stage, &claim_epoch],
            )
            .await
            .map_err(|e| {
                console_log!("admission ledger commit failed: {:?}", e);
                MeterWriteError::Transport("admission ledger unavailable".to_string())
            })?;
        if updated != 1 {
            // Lost the claim (lease expired and someone else reclaimed) or
            // already committed: fail closed so the retry converges instead
            // of answering 202-and-unmarked.
            return Err(MeterWriteError::Transport(
                "admission ledger unavailable".to_string(),
            ));
        }
        Ok(())
    }

    async fn release_claim(&mut self, job_id: &str, stage: &str) {
        // Best-effort `claimed -> pending` so the immediate retry can
        // reclaim without waiting out the lease; ignored errors still
        // converge via lease expiry. Fresh short-lived connection: never
        // held across the queue/meter awaits.
        let Some(claim_epoch) = self.claim_epoch else {
            return;
        };
        let Ok(client) = self.client().await else {
            return;
        };
        let _ = client
            .execute(admission_release_sql(), &[&job_id, &stage, &claim_epoch])
            .await;
    }
}

/// Outcome of one `drive_metered_admission` pass. Rendering to HTTP lives
/// in `admit_metered_job`, so this stays free of `worker::Response` and
/// unit-testable on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AdmissionDecision {
    /// Ledger already `queued`: replay 202 with no queue/meter writes.
    AlreadyQueued,
    /// Another request owns the durable admission claim; retry shortly.
    Busy,
    /// Queue accepted, `Ok` metered, ledger committed: answer 202.
    Admitted,
    /// Queue unavailable: nothing queued or metered, ledger `pending`.
    QueueUnavailable,
    /// Ledger or meter unavailable (retryable), or caller bug (unknown
    /// stage): rendered by `meter_failure_response`.
    MeterUnavailable(MeterWriteError),
}

/// Run one durable metered admission against injected queue/meter
/// operations (the routes pass the real edge operations; tests pass
/// scripted fakes). See the ledger docs above for the protocol.
async fn drive_metered_admission<L, Q, QF, M, MF>(
    ledger: &mut L,
    msg: &QueueMsg,
    stage: &str,
    mut queue_send: Q,
    mut meter_emit: M,
) -> AdmissionDecision
where
    L: AdmissionLedger,
    Q: FnMut() -> QF,
    QF: core::future::Future<Output = bool>,
    M: FnMut(MeterOutcome) -> MF,
    MF: core::future::Future<Output = Result<(), MeterWriteError>>,
{
    // Caller bug first, before the ledger is touched (mirrors
    // `emit_meter`: an unknown stage never causes a write).
    if !crate::queue::WORKFLOW_STEPS.contains(&stage) {
        return AdmissionDecision::MeterUnavailable(MeterWriteError::UnknownStage(
            "stage must be a pipeline workflow step",
        ));
    }
    let mark = match ledger.record_stage_once(&msg.job_id, stage).await {
        Ok(mark) => mark,
        Err(e) => return AdmissionDecision::MeterUnavailable(e),
    };
    if mark == AdmissionMark::Queued {
        return AdmissionDecision::AlreadyQueued;
    }
    if mark == AdmissionMark::Busy {
        return AdmissionDecision::Busy;
    }
    if !queue_send().await {
        ledger.release_claim(&msg.job_id, stage).await;
        return AdmissionDecision::QueueUnavailable;
    }
    if let Err(e) = meter_emit(MeterOutcome::Ok).await {
        ledger.release_claim(&msg.job_id, stage).await;
        return AdmissionDecision::MeterUnavailable(e);
    }
    if let Err(e) = ledger.mark_queued(&msg.job_id, stage).await {
        ledger.release_claim(&msg.job_id, stage).await;
        return AdmissionDecision::MeterUnavailable(e);
    }
    AdmissionDecision::Admitted
}

/// Write the checkpoint for a real (non-replay) admission, per
/// [`checkpoint::admission_plan`]: a fresh job becomes queryable via
/// `GET /v1/ingest/jobs/:id` immediately, and a re-admitted job's stale
/// `Failed` checkpoint is reset so the new enqueue runs. Best-effort: the
/// job is already queued + metered, and the consumer creates the initial
/// checkpoint on first delivery if this write is lost — failing the 202
/// here would strand an accepted job, so errors are only logged. This is
/// one of the checkpoint key's two writers (see `checkpoint` docs); the
/// consumer waits out KV's 1 s same-key window if its first persist
/// collides with this write.
async fn ensure_admission_checkpoint(env: &Env, msg: &QueueMsg) {
    let existing = match checkpoint::get(env, &msg.job_id).await {
        Ok(existing) => existing,
        Err(e) => {
            console_log!("ingest admission checkpoint read failed: {:?}", e);
            return;
        }
    };
    let plan = checkpoint::admission_plan(existing.as_ref(), msg);
    if let Some(cp) = &plan.write {
        if let Err(e) = checkpoint::put(env, cp).await {
            console_log!("ingest admission checkpoint write failed: {:?}", e);
            return;
        }
    }
    if plan.reenqueue_after_write {
        // `Failed` reset (see `checkpoint::AdmissionPlan`): the reset is
        // now persisted, so this trailing copy is guaranteed to observe
        // `Queued`; duplicate Queue messages are harmless by construction
        // (the consumer resumes from the durable checkpoint).
        if !try_enqueue(env, msg).await {
            console_log!("ingest re-drive enqueue after checkpoint reset failed");
        }
    }
}

/// Durable idempotent admission for both ingest routes: the (`job_id`,
/// `stage`) ledger owns the enqueue + meter outbox, fail-closed. A 503
/// here means nothing was accepted with its meter; the client retry
/// converges on exactly one `Ok` meter event and one accepted job.
async fn admit_metered_job(
    env: &Env,
    ledger: &mut impl AdmissionLedger,
    msg: &QueueMsg,
    stage: &str,
    source: &str,
    extra: serde_json::Value,
) -> Result<Response> {
    match drive_metered_admission(
        ledger,
        msg,
        stage,
        || try_enqueue(env, msg),
        |outcome| async move { try_meter(env, msg, stage, outcome) },
    )
    .await
    {
        AdmissionDecision::Admitted => {
            ensure_admission_checkpoint(env, msg).await;
            queued_envelope(msg, source, true, extra)
        }
        AdmissionDecision::AlreadyQueued => {
            // Replay window: the ledger will not enqueue again (see the
            // admission protocol docs). If that earlier run already
            // terminally failed, answering 202 `queued` would claim work
            // that will never run — refuse with 409 so the caller reads
            // the failure from the job status instead. A checkpoint-store
            // failure falls through to the plain replay (logged).
            match checkpoint::get(env, &msg.job_id).await {
                Ok(cp)
                    if checkpoint::replay_decision(cp.as_ref(), msg)
                        == checkpoint::ReplayDecision::Conflict =>
                {
                    json_error(ApiError::conflict(
                        "this job previously failed; check its status before re-posting",
                    ))
                }
                Ok(_) => queued_envelope(msg, source, true, extra),
                Err(e) => {
                    console_log!("ingest replay checkpoint read failed: {:?}", e);
                    queued_envelope(msg, source, true, extra)
                }
            }
        }
        AdmissionDecision::Busy => service_unavailable("admission is in progress; retry shortly"),
        AdmissionDecision::QueueUnavailable => queued_envelope(msg, source, false, extra),
        AdmissionDecision::MeterUnavailable(e) => meter_failure_response(&e),
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
/// search → parse → probe → verify → match → write workflow. The resolved
/// trace travels in every log line, in the `x-trace-id` response header, and
/// in the queued message for the consumer.
async fn search(req: &mut Request, ctx: &RouteContext<()>, trace_id: &str) -> Result<Response> {
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
    // Abuse guard before any dispatch is built or metered (roadmap R6):
    // per-IP AND per-account token buckets; a denied attempt consumes
    // nothing and answers the generic 429 + Retry-After.
    if let rate_limit::SearchDecision::Deny { retry_after_secs } =
        check_search_attempt(req, &ctx.env, &parsed.user_id)
    {
        return search_rate_limited_response(retry_after_secs);
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
        Err(message) => return json_error(ApiError::invalid_request(message)),
    };
    msg.trace_id = Some(trace_id.to_string());
    // Build identity rides every line (mirrors the api worker): the deploy
    // lane's public BUILD_SHA var, or explicit "unknown" when unwired.
    let build_sha = build_sha_from_env(&ctx.env);
    // Operational facts only: opaque job ID plus the query length. The query
    // text, user handle, and indexer handle never enter a log line.
    emit(
        Level::Info,
        trace_id,
        "ingest.search.received",
        serde_json::json!({"job_id": msg.job_id, "q_len": q.len(), "build_sha": build_sha}),
    );
    // Durable idempotent admission: the (job_id, stage) ledger owns the
    // enqueue + meter outbox, fail-closed. A 503 here means nothing was
    // accepted with its meter; the client retry converges on one `Ok`
    // meter event for one accepted job.
    let mut ledger = PgAdmissionLedger::from_env(&ctx.env);
    return admit_metered_job(
        &ctx.env,
        &mut ledger,
        &msg,
        meter_entry_stage(msg.source),
        "indexer_search",
        serde_json::json!({}),
    )
    .await;
}

/// Validate an NZB upload and enqueue it; the consumer parses with
/// `nntune-core`, probes the pool DO, and verifies before matching. Same
/// trace contract as [`search`]: log lines, response header, queued message.
async fn upload_nzb(req: &mut Request, ctx: &RouteContext<()>, trace_id: &str) -> Result<Response> {
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
            // Shared-parser validation before anything is stored or queued:
            // an NZB the bounded `nntune-core` parser rejects (or whose
            // counts disagree with the intake guard) never becomes a job.
            match crate::parse::parse_upload(parsed.nzb_xml.as_bytes(), &est) {
                Ok(_) => {}
                Err(crate::parse::ParseRejection::Invalid(message)) => {
                    return json_error(ApiError::invalid_request(message))
                }
                Err(crate::parse::ParseRejection::TooLarge) => {
                    return json_error(ApiError::payload_too_large(
                        "nzb exceeds shared parser limits",
                    ))
                }
            }
            let mut msg = match dispatch::build_upload_dispatch(&parsed.user_id, &parsed.nzb_xml) {
                Ok(msg) => msg,
                Err(message) => return json_error(ApiError::invalid_request(message)),
            };
            msg.trace_id = Some(trace_id.to_string());
            // Build identity rides every line (mirrors the api worker): the deploy
            // lane's public BUILD_SHA var, or explicit "unknown" when unwired.
            let build_sha = build_sha_from_env(&ctx.env);
            // The bytes must be durably stored before the job exists: an
            // enqueued job without retrievable bytes can never run.
            let payload_ref = msg.payload_ref.clone().unwrap_or_default();
            if payload_ref.is_empty()
                || !store_upload_payload(&ctx.env, &payload_ref, &parsed.nzb_xml).await
            {
                // KV/driver detail stays out of the line; the trace plus the
                // failed stage is enough to correlate.
                emit(
                    Level::Error,
                    trace_id,
                    "ingest.payload.store_failed",
                    serde_json::json!({"source": "nzb_upload", "build_sha": build_sha}),
                );
                return service_unavailable("payload store unavailable; nothing was accepted");
            }
            // Counts plus the opaque job ID; filenames and NZB bytes never log.
            emit(
                Level::Info,
                trace_id,
                "ingest.upload.received",
                serde_json::json!({
                    "job_id": msg.job_id,
                    "file_count": est.file_count,
                    "segment_count": est.segment_count,
                    "build_sha": build_sha,
                }),
            );
            // Durable idempotent admission after the payload store (bytes are
            // TTL-expired on failure): the (job_id, stage) ledger owns the
            // enqueue + meter outbox, fail-closed, so a retry converges on
            // one `Ok` meter event for one accepted job.
            let mut ledger = PgAdmissionLedger::from_env(&ctx.env);
            return admit_metered_job(
                &ctx.env,
                &mut ledger,
                &msg,
                meter_entry_stage(msg.source),
                "nzb_upload",
                serde_json::json!({
                    "file_count": est.file_count,
                    "segment_count": est.segment_count,
                }),
            )
            .await;
        }
    }
}

fn healthz() -> Result<Response> {
    let health = health_response_parts();
    let headers = worker::Headers::new();
    for (name, value) in health.headers {
        headers.append(&name, &value)?;
    }
    Response::from_body(worker::ResponseBody::Body(health.body))
        .map(|response| response.with_status(health.status).with_headers(headers))
}

/// Job checkpoint status: read the durable checkpoint written at
/// admission and advanced by the Queue consumer. Unknown ids are 404; a
/// checkpoint-store failure is 503 (never a fabricated status).
/// Malformed IDs answer 404 without a log line (scanner noise stays out of
/// the logs); valid IDs log one `status_checked` line under the trace.
async fn job_status(ctx: &RouteContext<()>, trace_id: &str) -> Result<Response> {
    let id = ctx.param("id").cloned().unwrap_or_default();
    if !validate_job_id(&id) {
        return json_error(ApiError::not_found("unknown job id"));
    }
    // Build identity rides every line (mirrors the api worker): the deploy
    // lane's public BUILD_SHA var, or explicit "unknown" when unwired.
    let build_sha = build_sha_from_env(&ctx.env);
    emit(
        Level::Info,
        trace_id,
        "ingest.job.status_checked",
        serde_json::json!({"job_id": id, "build_sha": build_sha}),
    );
    match checkpoint::get(&ctx.env, &id).await {
        Ok(Some(cp)) => Response::from_json(&cp),
        Ok(None) => json_error(ApiError::not_found("unknown job id")),
        Err(_) => service_unavailable("checkpoint store unavailable"),
    }
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
    fn health_response_is_uncached_json_without_dependencies() {
        let response = health_response_parts();
        assert_eq!(response.status, http::StatusCode::OK.as_u16());
        assert_eq!(response.headers.len(), 2);
        assert!(response
            .headers
            .contains(&("content-type".to_owned(), "application/json".to_owned())));
        assert!(response
            .headers
            .contains(&("cache-control".to_owned(), "no-store".to_owned())));
        assert_eq!(response.body, br#"{"ok":true}"#);
    }

    #[test]
    fn healthz_route_contract_precedes_the_catchall() {
        // Worker Response/Headers require wasm-bindgen, so pin the production
        // route and adapter source while testing the HTTP contract natively.
        let source = include_str!("lib.rs");
        let router = source
            .split_once("\n#[worker::event(fetch)]\npub async fn main")
            .expect("worker entrypoint must exist")
            .1;
        let route = router
            .find(".get_async(\"/healthz\", |_, _| async { healthz() })")
            .expect("GET /healthz must dispatch to its handler");
        let catchall = router
            .find(".or_else_any_method_async")
            .expect("router must retain its catch-all");
        assert!(
            route < catchall,
            "healthz must be registered before catch-all"
        );
        assert_eq!(router.matches("\"/healthz\"").count(), 1);

        let handler = source
            .split_once("fn healthz() -> Result<Response> {")
            .expect("healthz handler must exist")
            .1
            .split_once("\n}\n")
            .expect("healthz handler must close")
            .0;
        let compact = |text: &str| {
            text.chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>()
        };
        assert_eq!(
            compact(handler),
            compact(
                r#"let health = health_response_parts();
    let headers = worker::Headers::new();
    for (name, value) in health.headers {
        headers.append(&name, &value)?;
    }
    Response::from_body(worker::ResponseBody::Body(health.body))
        .map(|response| response.with_status(health.status).with_headers(headers))"#
            )
        );
    }

    #[test]
    fn search_source_ip_takes_only_a_clean_ip_literal() {
        assert_eq!(
            search_source_ip(Some("203.0.113.9".to_string())),
            "203.0.113.9"
        );
        assert_eq!(search_source_ip(None), "unknown");
        assert_eq!(
            search_source_ip(Some("203.0.113.9\nINJECTED".to_string())),
            "unknown"
        );
        assert_eq!(search_source_ip(Some("not-an-ip".to_string())), "unknown");
        // IPv6 scopes to the /64: rotating the interface id stays one budget.
        assert_eq!(
            search_source_ip(Some("2001:db8:1:2:3:4:5:6".to_string())),
            search_source_ip(Some("2001:db8:1:2:ffff:ffff:ffff:ffff".to_string()))
        );
        assert_ne!(
            search_source_ip(Some("2001:db8:1:2::1".to_string())),
            search_source_ip(Some("2001:db8:1:3::1".to_string()))
        );
    }

    #[test]
    fn search_attempt_decision_enforces_configured_dual_cap() {
        // Route-level pin over the exact decision fn `check_search_attempt`
        // runs: [vars] cap of 2 binds per-IP AND per-account, denial
        // carries a bounded retry-after, and a denied attempt by a second
        // account leaves that account's budget intact on a fresh IP.
        use rate_limit::{SearchDecision, SearchThrottleStore};
        let vars: std::collections::HashMap<String, String> = [
            ("SEARCH_RATE_LIMIT_MAX".to_string(), "2".to_string()),
            (
                "SEARCH_RATE_LIMIT_WINDOW_SECS".to_string(),
                "60".to_string(),
            ),
        ]
        .into_iter()
        .collect();
        let mut store = SearchThrottleStore::new();
        let ip1 = || Some("203.0.113.7".to_string());
        let ip2 = || Some("203.0.113.8".to_string());
        assert_eq!(
            search_attempt_decision(&mut store, ip1(), &vars, "user_a", 1_000),
            SearchDecision::Allow
        );
        assert_eq!(
            search_attempt_decision(&mut store, ip1(), &vars, "user_a", 1_000),
            SearchDecision::Allow
        );
        match search_attempt_decision(&mut store, ip1(), &vars, "user_a", 1_000) {
            SearchDecision::Deny { retry_after_secs } => {
                assert!((1..=60).contains(&retry_after_secs), "{retry_after_secs}");
            }
            SearchDecision::Allow => panic!("third attempt inside the cap must deny"),
        }
        // Per-IP: a different account behind the same IP is also denied…
        assert!(matches!(
            search_attempt_decision(&mut store, ip1(), &vars, "user_b", 1_000),
            SearchDecision::Deny { .. }
        ));
        // …but that denial consumed nothing from user_b's account budget.
        assert_eq!(
            search_attempt_decision(&mut store, ip2(), &vars, "user_b", 1_000),
            SearchDecision::Allow
        );
        // Per-account: user_a stays denied even from a fresh IP.
        assert!(matches!(
            search_attempt_decision(&mut store, ip2(), &vars, "user_a", 1_000),
            SearchDecision::Deny { .. }
        ));
    }

    #[test]
    fn search_gate_glue_enforces_configured_cap_end_to_end() {
        // Drive the exact glue `check_search_attempt` runs
        // (header name -> IP scope, [vars] names -> config, store
        // decision) with stub lookups that answer ONLY the correct
        // names. Each classic wiring mutant fails behaviorally:
        // a renamed var falls back to the default cap of 10 (third
        // attempt would Allow), a renamed window var changes the
        // retry-after, a renamed header collapses both IPs into the
        // shared `unknown` bucket (user_b could not be admitted on
        // the second IP), and an unconditional Allow never denies.
        use rate_limit::{SearchDecision, SearchRateLimitConfig, SearchThrottleStore};
        // Var name -> field pins, mirroring the api overlay test.
        let only = |name: &'static str, value: &'static str| {
            SearchRateLimitConfig::from_vars(&search_throttle_vars_from_lookup(|key| {
                (key == name).then(|| value.to_string())
            }))
        };
        let defaults = SearchRateLimitConfig::default();
        assert_eq!(
            SearchRateLimitConfig::from_vars(&search_throttle_vars_from_lookup(|_| None)),
            defaults
        );
        assert_eq!(
            only("SEARCH_RATE_LIMIT_MAX", "3"),
            SearchRateLimitConfig {
                max_attempts: 3,
                ..defaults
            }
        );
        assert_eq!(
            only("SEARCH_RATE_LIMIT_WINDOW_SECS", "120"),
            SearchRateLimitConfig {
                window_secs: 120,
                ..defaults
            }
        );
        assert_eq!(SEARCH_RATE_LIMIT_VAR_NAMES.len(), 2);
        // End to end through the route glue.
        let get_var = |key: &str| match key {
            "SEARCH_RATE_LIMIT_MAX" => Some("2".to_string()),
            "SEARCH_RATE_LIMIT_WINDOW_SECS" => Some("120".to_string()),
            _ => None,
        };
        let at_ip = |ip: &'static str| {
            move |name: &str| (name == "cf-connecting-ip").then(|| ip.to_string())
        };
        let mut store = SearchThrottleStore::new();
        let attempt = |store: &mut SearchThrottleStore, ip: &'static str, user: &str| {
            check_search_attempt_in(store, at_ip(ip), get_var, user, 1_000)
        };
        assert_eq!(
            attempt(&mut store, "203.0.113.7", "user_a"),
            SearchDecision::Allow
        );
        assert_eq!(
            attempt(&mut store, "203.0.113.7", "user_a"),
            SearchDecision::Allow
        );
        // Third attempt denies; retry-after 60 == one token at
        // 2-per-120s pins BOTH configured values (default window
        // would give 30, default cap would not deny at all).
        assert_eq!(
            attempt(&mut store, "203.0.113.7", "user_a"),
            SearchDecision::Deny {
                retry_after_secs: 60
            }
        );
        // Per-IP via the header: another account on the same IP denies…
        assert!(matches!(
            attempt(&mut store, "203.0.113.7", "user_b"),
            SearchDecision::Deny { .. }
        ));
        // …but a distinct header IP is a distinct budget (a renamed
        // header would share `unknown` and deny here too).
        assert_eq!(
            attempt(&mut store, "203.0.113.8", "user_b"),
            SearchDecision::Allow
        );
        // Per-account follows the account across IPs.
        assert!(matches!(
            attempt(&mut store, "203.0.113.8", "user_a"),
            SearchDecision::Deny { .. }
        ));
    }

    #[test]
    fn search_route_denial_short_circuits_before_dispatch() {
        // The route itself needs wasm-bindgen, so pin its shape in
        // source: the throttle denial must return before any dispatch
        // is built. Slice ONLY the route body out of the pre-test
        // source — searching the whole file could match this test's
        // own literals. (The glue BEHIND the call is pinned
        // behaviorally by `search_gate_glue_enforces_configured_cap_end_to_end`.)
        let source = include_str!("lib.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        let route = production
            .split_once("async fn search(req: &mut Request")
            .expect("search route must exist")
            .1
            .split_once("\nasync fn ")
            .expect("a following route must bound the body")
            .0;
        let gate = route
            .find("check_search_attempt(req, &ctx.env, &parsed.user_id)")
            .expect("search route must run the throttle gate");
        let denied = route
            .find("return search_rate_limited_response(retry_after_secs);")
            .expect("throttle denial must return the 429 response");
        let dispatch = route
            .find("build_search_dispatch(")
            .expect("search route must build a dispatch");
        assert!(gate < denied && denied < dispatch);
        // The wasm wrapper must delegate to the behaviorally-pinned
        // glue over the per-isolate store: replacing its body with an
        // unconditional Allow removes this call from its pre-test
        // body slice and fails here.
        let wrapper = production
            .split_once("fn check_search_attempt(req: &Request")
            .expect("check_search_attempt wrapper must exist")
            .1
            .split_once("\nfn ")
            .expect("a following fn must bound the wrapper body")
            .0;
        assert!(
            wrapper.contains("check_search_attempt_in("),
            "check_search_attempt must run the pinned glue"
        );
        assert!(
            wrapper.contains("SEARCH_THROTTLE.with("),
            "check_search_attempt must use the per-isolate store"
        );
        // An early `return …::Allow` ABOVE the glue call would leave
        // both substrings in place, so forbid a direct Allow in the
        // wrapper outright: the only decision it may produce is the
        // glue's.
        assert!(
            !wrapper.contains("SearchDecision::Allow"),
            "check_search_attempt must not short-circuit the glue with Allow"
        );
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
    fn search_validation_counts_query_scalars_not_utf8_bytes() {
        // The contract is `1..=200 chars`: U+00E9 is one Unicode scalar
        // but two UTF-8 bytes, so 200 of them (400 bytes) must pass and
        // 201 must fail. A byte-length check rejects the 200 case.
        let exact = "\u{e9}".repeat(200);
        let over = "\u{e9}".repeat(201);
        assert_eq!(exact.chars().count(), 200);
        assert_eq!(exact.len(), 400);
        assert_eq!(over.chars().count(), 201);
        let check = |q: &str| {
            check_search(&parse_search(&search_body(q, "vault_idx_01-ab", &[], 25, 0)).unwrap())
        };
        assert_eq!(check(&exact), Ok(()));
        assert_eq!(check(&over), Err("query must be 1..=200 chars"));
        // The ASCII boundary is unchanged (scalars == bytes).
        assert_eq!(check(&"x".repeat(200)), Ok(()));
        assert_eq!(check(&"x".repeat(201)), Err("query must be 1..=200 chars"));
        // Trimming happens before counting: padding does not count, and a
        // padded over-limit query still fails.
        assert_eq!(check(&format!("  {exact}  ")), Ok(()));
        assert_eq!(
            check(&format!("  {over}  ")),
            Err("query must be 1..=200 chars")
        );
        assert_eq!(check("   "), Err("query must be 1..=200 chars"));
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

    // --- Durable metered admission seam ---------------------------------
    //
    // Production uses single-statement autocommit UPDATE claims plus the
    // primary key for durable marks — no session state, so it stays correct
    // under Hyperdrive transaction-mode pooling. These doubles prove the
    // admission state machine at the `AdmissionLedger` seam under reload,
    // concurrency, and failure — no Worker runtime needed.

    /// In-memory ledger double for durable marks. Models the production
    /// pending/claimed/queued machine: `record_stage_once` claims (Busy on a
    /// live claim), `mark_queued` requires the claim, `release_claim` drops
    /// it back to pending. Injected `fail_*` flags model ledger outages.
    struct MemLedger {
        marks: std::collections::BTreeMap<(String, String), AdmissionMark>,
        claimed: std::collections::BTreeSet<(String, String)>,
        fail_record: bool,
        fail_commit: bool,
        record_calls: usize,
    }

    impl MemLedger {
        fn new() -> Self {
            Self {
                marks: std::collections::BTreeMap::new(),
                claimed: std::collections::BTreeSet::new(),
                fail_record: false,
                fail_commit: false,
                record_calls: 0,
            }
        }

        fn mark_of(&self, job_id: &str, stage: &str) -> Option<AdmissionMark> {
            self.marks
                .get(&(job_id.to_string(), stage.to_string()))
                .copied()
        }
    }

    impl AdmissionLedger for MemLedger {
        async fn record_stage_once(
            &mut self,
            job_id: &str,
            stage: &str,
        ) -> Result<AdmissionMark, MeterWriteError> {
            self.record_calls += 1;
            if self.fail_record {
                return Err(MeterWriteError::Transport(
                    "injected commit_failure".to_string(),
                ));
            }
            let key = (job_id.to_string(), stage.to_string());
            if self.marks.get(&key) == Some(&AdmissionMark::Queued) {
                return Ok(AdmissionMark::Queued);
            }
            if self.claimed.contains(&key) {
                return Ok(AdmissionMark::Busy);
            }
            self.claimed.insert(key.clone());
            self.marks.entry(key).or_insert(AdmissionMark::Pending);
            Ok(AdmissionMark::Pending)
        }

        async fn mark_queued(&mut self, job_id: &str, stage: &str) -> Result<(), MeterWriteError> {
            if self.fail_commit {
                return Err(MeterWriteError::Transport(
                    "injected commit_failure".to_string(),
                ));
            }
            let key = (job_id.to_string(), stage.to_string());
            if !self.claimed.remove(&key) {
                return Err(MeterWriteError::Transport(
                    "ledger commit without a claim".to_string(),
                ));
            }
            self.marks.insert(key, AdmissionMark::Queued);
            Ok(())
        }

        async fn release_claim(&mut self, job_id: &str, stage: &str) {
            self.claimed
                .remove(&(job_id.to_string(), stage.to_string()));
        }
    }

    /// File-backed ledger double: every operation reads and writes one
    /// JSON document, so dropping the handle and opening a fresh one on
    /// the same directory models a process restart (`reload`).
    struct FileLedger {
        dir: std::path::PathBuf,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct LedgerRow {
        job_id: String,
        stage: String,
        state: AdmissionMark,
    }

    impl FileLedger {
        fn new(dir: std::path::PathBuf) -> Self {
            Self { dir }
        }

        fn path(&self) -> std::path::PathBuf {
            self.dir.join("metered_admission.json")
        }

        fn load(&self) -> Vec<LedgerRow> {
            match std::fs::read_to_string(self.path()) {
                Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        }

        fn store(&self, rows: &[LedgerRow]) {
            std::fs::create_dir_all(&self.dir).expect("ledger dir");
            // Crash honesty: write temp + rename so a torn write never
            // leaves half a document behind.
            let tmp = self.dir.join("metered_admission.json.tmp");
            std::fs::write(
                &tmp,
                serde_json::to_string(rows).expect("ledger serializes"),
            )
            .expect("ledger write");
            std::fs::rename(&tmp, self.path()).expect("ledger commit");
        }
    }

    impl AdmissionLedger for FileLedger {
        async fn record_stage_once(
            &mut self,
            job_id: &str,
            stage: &str,
        ) -> Result<AdmissionMark, MeterWriteError> {
            let rows = self.load();
            if let Some(row) = rows
                .iter()
                .find(|row| row.job_id == job_id && row.stage == stage)
            {
                // File double is sequential (one handle drives one attempt):
                // a persisted `queued` replays, anything else is reclaimable
                // pending. Concurrent claims are covered by ClaimLedger.
                if row.state == AdmissionMark::Queued {
                    return Ok(AdmissionMark::Queued);
                }
                return Ok(AdmissionMark::Pending);
            }
            let mut rows = rows;
            rows.push(LedgerRow {
                job_id: job_id.to_string(),
                stage: stage.to_string(),
                state: AdmissionMark::Pending,
            });
            self.store(&rows);
            Ok(AdmissionMark::Pending)
        }

        async fn mark_queued(&mut self, job_id: &str, stage: &str) -> Result<(), MeterWriteError> {
            let mut rows = self.load();
            match rows
                .iter_mut()
                .find(|row| row.job_id == job_id && row.stage == stage)
            {
                Some(row) => {
                    row.state = AdmissionMark::Queued;
                    self.store(&rows);
                    Ok(())
                }
                None => Err(MeterWriteError::Transport(
                    "ledger commit without a recorded intent".to_string(),
                )),
            }
        }

        async fn release_claim(&mut self, _job_id: &str, _stage: &str) {}
    }

    fn admission_msg() -> QueueMsg {
        crate::dispatch::build_upload_dispatch("user_01", crate::fixtures::SMALL_NZB)
            .expect("fixture dispatch")
    }

    /// Drive one admission with scripted queue/meter operations. The
    /// fakes resolve without yielding, so the future is ready on first
    /// poll — no executor needed.
    fn drive_once(
        ledger: &mut impl AdmissionLedger,
        msg: &QueueMsg,
        stage: &str,
        queue_ok: bool,
        meter_fail: bool,
        queue_calls: &mut usize,
        meter_calls: &mut usize,
    ) -> AdmissionDecision {
        use futures_util::future::FutureExt as _;
        drive_metered_admission(
            ledger,
            msg,
            stage,
            || {
                *queue_calls += 1;
                async move { queue_ok }
            },
            |outcome| {
                assert_eq!(outcome, MeterOutcome::Ok);
                *meter_calls += 1;
                async move {
                    if meter_fail {
                        Err(MeterWriteError::Rejected { status: 500 })
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .now_or_never()
        .expect("scripted admission resolves without yielding")
    }

    #[test]
    fn meter_entry_stage_maps_sources_to_workflow_steps() {
        // The swap mutant (search<->parse) must fail here: each source maps
        // to its own entry step, and both steps are pipeline steps.
        assert_eq!(meter_entry_stage(JobSource::IndexerSearch), "search");
        assert_eq!(meter_entry_stage(JobSource::NzbUpload), "parse");
        for stage in [
            meter_entry_stage(JobSource::IndexerSearch),
            meter_entry_stage(JobSource::NzbUpload),
        ] {
            assert!(
                crate::queue::WORKFLOW_STEPS.contains(&stage),
                "{stage} must be a pipeline workflow step"
            );
        }
    }

    #[test]
    fn meter_failure_status_mapping_is_single_sourced() {
        // `meter_failure_response` derives from `http_status` (pinned by
        // the admission acceptance contract); the mapping itself:
        assert_eq!(
            MeterWriteError::Transport("down".to_string()).http_status(),
            503
        );
        assert_eq!(MeterWriteError::Rejected { status: 500 }.http_status(), 503);
        assert_eq!(MeterWriteError::UnknownStage("s").http_status(), 400);
        assert!(MeterWriteError::Transport("down".to_string()).is_retryable());
        assert!(MeterWriteError::Rejected { status: 500 }.is_retryable());
        assert!(!MeterWriteError::UnknownStage("s").is_retryable());
    }

    #[test]
    fn admission_sql_owns_concurrency_clauses() {
        for sql in [
            admission_ensure_sql(),
            admission_claim_sql(),
            admission_read_mark_sql(),
            admission_mark_queued_sql(),
            admission_release_sql(),
        ] {
            assert!(
                sql.contains("job_id") && sql.contains("stage"),
                "ledger statements are keyed by (job_id, stage): {sql}"
            );
        }
        // Hyperdrive transaction-mode pooling returns the origin connection
        // per transaction, so no statement may rely on session state held
        // across awaits: no advisory locks, no BEGIN, no temp state.
        for sql in [
            admission_ensure_sql(),
            admission_claim_sql(),
            admission_read_mark_sql(),
            admission_mark_queued_sql(),
            admission_release_sql(),
        ] {
            let lower = sql.to_lowercase();
            assert!(
                !lower.contains("pg_try_advisory")
                    && !lower.contains("pg_advisory")
                    && !lower.contains("begin")
                    && !lower.contains("savepoint"),
                "ledger statements must be single-statement autocommit: {sql}"
            );
        }
        assert!(
            admission_ensure_sql().contains("ON CONFLICT (job_id, stage) DO NOTHING"),
            "retries must converge on one durable row"
        );
        assert!(admission_ensure_sql().contains("pending"));
        // The atomic claim: one UPDATE whose WHERE admits pending rows,
        // expired claimed leases, or queued rows past the replay window.
        // RETURNING identifies the winner. Concurrent UPDATEs
        // on the same row serialize in Postgres (row lock + predicate
        // re-check), so exactly one claimant wins with no session lock.
        let claim = admission_claim_sql();
        assert!(claim.contains("UPDATE metered_admission"));
        assert!(claim.contains("SET state = 'claimed'"));
        assert!(claim.contains("state = 'pending'"));
        assert!(claim.contains("state = 'queued' AND queued_at"));
        assert!(claim.contains("15 days"));
        assert!(claim.contains("claimed_at"));
        assert!(claim.contains("claim_epoch = claim_epoch + 1"));
        assert!(claim.contains("RETURNING claim_epoch"));
        assert_eq!(admission_claim_lease_secs(), 30);
        assert!(claim.contains("30 seconds"));
        let read_mark = admission_read_mark_sql().to_lowercase();
        assert!(read_mark.starts_with("update metered_admission"));
        assert!(read_mark.contains("returning state"));
        assert!(admission_mark_queued_sql().contains("queued_at = now()"));
        assert!(admission_mark_queued_sql().contains("state = 'claimed'"));
        assert!(admission_mark_queued_sql().contains("claim_epoch = $3"));
        assert!(admission_mark_queued_sql().contains("'queued'"));
        assert!(admission_release_sql().contains("state = 'claimed'"));
        assert!(admission_release_sql().contains("claim_epoch = $3"));
        assert!(admission_release_sql().contains("'pending'"));
    }

    #[test]
    fn admission_queue_failure_leaves_pending_without_meter() {
        // The reported bug, scripted: the Queue send fails, so no `Ok`
        // meter event is owed yet and the ledger stays pending.
        let msg = admission_msg();
        let mut ledger = MemLedger::new();
        let (mut queue_calls, mut meter_calls) = (0usize, 0usize);
        let decision = drive_once(
            &mut ledger,
            &msg,
            "parse",
            false,
            false,
            &mut queue_calls,
            &mut meter_calls,
        );
        assert_eq!(decision, AdmissionDecision::QueueUnavailable);
        assert_eq!((queue_calls, meter_calls), (1, 0));
        assert_eq!(
            ledger.mark_of(&msg.job_id, "parse"),
            Some(AdmissionMark::Pending)
        );
        // The client retry resumes the same intent and owes exactly one `Ok`.
        let decision = drive_once(
            &mut ledger,
            &msg,
            "parse",
            true,
            false,
            &mut queue_calls,
            &mut meter_calls,
        );
        assert_eq!(decision, AdmissionDecision::Admitted);
        assert_eq!((queue_calls, meter_calls), (2, 1));
        assert_eq!(
            ledger.mark_of(&msg.job_id, "parse"),
            Some(AdmissionMark::Queued)
        );
    }

    #[test]
    fn admission_already_queued_replays_without_side_effects() {
        let msg = admission_msg();
        let mut ledger = MemLedger::new();
        let (mut queue_calls, mut meter_calls) = (0usize, 0usize);
        assert_eq!(
            drive_once(
                &mut ledger,
                &msg,
                "parse",
                true,
                false,
                &mut queue_calls,
                &mut meter_calls
            ),
            AdmissionDecision::Admitted
        );
        assert_eq!((queue_calls, meter_calls), (1, 1));
        // A duplicate delivery (redelivery, double-clicked retry) replays
        // 202 with no further queue or meter writes.
        assert_eq!(
            drive_once(
                &mut ledger,
                &msg,
                "parse",
                true,
                false,
                &mut queue_calls,
                &mut meter_calls
            ),
            AdmissionDecision::AlreadyQueued
        );
        assert_eq!((queue_calls, meter_calls), (1, 1));
    }

    #[test]
    fn admission_commit_failure_is_fail_closed_without_side_effects() {
        // The ledger itself is down: 503 before anything is queued or
        // metered, and the status the route answers is 503.
        let msg = admission_msg();
        let mut ledger = MemLedger::new();
        ledger.fail_record = true;
        let (mut queue_calls, mut meter_calls) = (0usize, 0usize);
        let decision = drive_once(
            &mut ledger,
            &msg,
            "parse",
            true,
            false,
            &mut queue_calls,
            &mut meter_calls,
        );
        match &decision {
            AdmissionDecision::MeterUnavailable(e) => {
                assert!(e.is_retryable());
                assert_eq!(e.http_status(), 503);
            }
            other => panic!("ledger outage must be fail-closed, got {other:?}"),
        }
        assert_eq!((queue_calls, meter_calls), (0, 0));
    }

    #[test]
    fn admission_mark_commit_failure_stays_retryable() {
        // Record + queue + meter succeed, but the commit fails: the route
        // answers 503 (never 202-and-unmarked) and the retry re-drives
        // from `pending`.
        let msg = admission_msg();
        let mut ledger = MemLedger::new();
        ledger.fail_commit = true;
        let (mut queue_calls, mut meter_calls) = (0usize, 0usize);
        let decision = drive_once(
            &mut ledger,
            &msg,
            "parse",
            true,
            false,
            &mut queue_calls,
            &mut meter_calls,
        );
        match &decision {
            AdmissionDecision::MeterUnavailable(e) => assert_eq!(e.http_status(), 503),
            other => panic!("commit failure must stay retryable, got {other:?}"),
        }
        // Meter ran before the commit, so the retry re-meters; the count
        // stays exact per completed admission, not per attempt.
        assert_eq!((queue_calls, meter_calls), (1, 1));
        assert_eq!(
            ledger.mark_of(&msg.job_id, "parse"),
            Some(AdmissionMark::Pending)
        );
    }

    #[test]
    fn admission_meter_failure_leaves_pending_for_retry() {
        // Queue accepted but the meter write failed: 503 with the ledger
        // still pending (the orphan queue message is deduplicated by the
        // consumer via idempotency_key); the retry meters exactly once.
        let msg = admission_msg();
        let mut ledger = MemLedger::new();
        let (mut queue_calls, mut meter_calls) = (0usize, 0usize);
        let decision = drive_once(
            &mut ledger,
            &msg,
            "parse",
            true,
            true,
            &mut queue_calls,
            &mut meter_calls,
        );
        match &decision {
            AdmissionDecision::MeterUnavailable(e) => assert_eq!(e.http_status(), 503),
            other => panic!("meter failure must stay retryable, got {other:?}"),
        }
        assert_eq!((queue_calls, meter_calls), (1, 1));
        assert_eq!(
            ledger.mark_of(&msg.job_id, "parse"),
            Some(AdmissionMark::Pending)
        );
    }

    #[test]
    fn admission_unknown_stage_never_touches_ledger_or_queue() {
        let msg = admission_msg();
        let mut ledger = MemLedger::new();
        let (mut queue_calls, mut meter_calls) = (0usize, 0usize);
        let decision = drive_once(
            &mut ledger,
            &msg,
            "teleport",
            true,
            false,
            &mut queue_calls,
            &mut meter_calls,
        );
        match &decision {
            AdmissionDecision::MeterUnavailable(MeterWriteError::UnknownStage(_)) => {}
            other => panic!("unknown stage is a caller bug, got {other:?}"),
        }
        assert_eq!(ledger.record_calls, 0);
        assert_eq!((queue_calls, meter_calls), (0, 0));
    }

    #[test]
    fn admission_reload_recovers_pending_without_double_meter() {
        // A `pending` intent survives the loss of the isolate: a fresh
        // handle on the same directory recovers it, and the retried
        // admission still owes exactly one `Ok` meter event.
        let dir =
            std::env::temp_dir().join(format!("ingest-admission-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let msg = admission_msg();
        let (mut queue_calls, mut meter_calls) = (0usize, 0usize);
        {
            let mut ledger = FileLedger::new(dir.clone());
            let decision = drive_once(
                &mut ledger,
                &msg,
                "parse",
                false,
                false,
                &mut queue_calls,
                &mut meter_calls,
            );
            assert_eq!(decision, AdmissionDecision::QueueUnavailable);
            assert_eq!(meter_calls, 0);
        } // Handle dropped: models the loss of the isolate.
        {
            let mut ledger = FileLedger::new(dir.clone());
            let decision = drive_once(
                &mut ledger,
                &msg,
                "parse",
                true,
                false,
                &mut queue_calls,
                &mut meter_calls,
            );
            assert_eq!(decision, AdmissionDecision::Admitted);
            assert_eq!(meter_calls, 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Lock-free claim double with Postgres UPDATE semantics: the claim is
    /// a single atomic compare-exchange (row predicate re-check), never a
    /// mutex held across the queue/meter awaits. Competitors interleave
    /// freely after the first claim — exactly one wins.
    struct ClaimLedger {
        /// 0 = pending, 1 = queued (persisted mark).
        mark: std::sync::Arc<std::sync::atomic::AtomicU8>,
        /// false = unclaimed, true = claimed (lease owner).
        claimed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        /// Monotonic generation shared by requests over this row.
        claim_epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
        /// Generation owned by this ledger handle.
        own_epoch: Option<u64>,
    }

    impl ClaimLedger {
        fn new_shared(
            mark: std::sync::Arc<std::sync::atomic::AtomicU8>,
            claimed: std::sync::Arc<std::sync::atomic::AtomicBool>,
            claim_epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
        ) -> Self {
            Self {
                mark,
                claimed,
                claim_epoch,
                own_epoch: None,
            }
        }
    }

    impl AdmissionLedger for ClaimLedger {
        async fn record_stage_once(
            &mut self,
            _job_id: &str,
            _stage: &str,
        ) -> Result<AdmissionMark, MeterWriteError> {
            use std::sync::atomic::Ordering;
            if self.mark.load(Ordering::SeqCst) == 1 {
                return Ok(AdmissionMark::Queued);
            }
            // Atomic claim: mirrors
            // `UPDATE ... WHERE (pending OR expired) RETURNING`. No lock is
            // held here — losers observe the winner's claim immediately.
            match self
                .claimed
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => {
                    self.own_epoch = Some(self.claim_epoch.fetch_add(1, Ordering::SeqCst) + 1);
                    Ok(AdmissionMark::Pending)
                }
                Err(_) => {
                    if self.mark.load(Ordering::SeqCst) == 1 {
                        Ok(AdmissionMark::Queued)
                    } else {
                        Ok(AdmissionMark::Busy)
                    }
                }
            }
        }

        async fn mark_queued(
            &mut self,
            _job_id: &str,
            _stage: &str,
        ) -> Result<(), MeterWriteError> {
            use std::sync::atomic::Ordering;
            // Commit requires the current generation, like
            // `WHERE state = 'claimed' AND claim_epoch = $3`.
            if self.own_epoch != Some(self.claim_epoch.load(Ordering::SeqCst))
                || !self.claimed.swap(false, Ordering::SeqCst)
            {
                return Err(MeterWriteError::Transport(
                    "ledger commit without the current claim".to_string(),
                ));
            }
            self.mark.store(1, Ordering::SeqCst);
            Ok(())
        }

        async fn release_claim(&mut self, _job_id: &str, _stage: &str) {
            use std::sync::atomic::Ordering;
            if self.own_epoch == Some(self.claim_epoch.load(Ordering::SeqCst)) {
                self.claimed.store(false, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn admission_competing_requests_claim_before_queue_or_meter() {
        // Each request has its own ledger handle over shared atomics, just
        // like production isolates over one row. No mutex is held across
        // the queue/meter awaits: the queue delay lets competitors
        // interleave after the first claim, and the atomic claim admits
        // exactly one sender+meter.
        use std::sync::{
            atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering},
            Arc, Barrier,
        };
        let mark = Arc::new(AtomicU8::new(0));
        let claimed = Arc::new(AtomicBool::new(false));
        let claim_epoch = Arc::new(AtomicU64::new(0));
        let start = Arc::new(Barrier::new(8));
        let meter_calls = Arc::new(AtomicUsize::new(0));
        let queued_calls = Arc::new(AtomicUsize::new(0));
        let msg = admission_msg();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let mark = Arc::clone(&mark);
                let claimed = Arc::clone(&claimed);
                let claim_epoch = Arc::clone(&claim_epoch);
                let start = Arc::clone(&start);
                let meter_calls = Arc::clone(&meter_calls);
                let queued_calls = Arc::clone(&queued_calls);
                let msg = msg.clone();
                std::thread::spawn(move || {
                    use futures_util::future::FutureExt as _;
                    let mut ledger = ClaimLedger::new_shared(mark, claimed, claim_epoch);
                    start.wait();
                    drive_metered_admission(
                        &mut ledger,
                        &msg,
                        "parse",
                        || {
                            queued_calls.fetch_add(1, Ordering::SeqCst);
                            async {
                                std::thread::sleep(std::time::Duration::from_millis(20));
                                true
                            }
                        },
                        |outcome| {
                            assert_eq!(outcome, MeterOutcome::Ok);
                            meter_calls.fetch_add(1, Ordering::SeqCst);
                            async move { Ok(()) }
                        },
                    )
                    .now_or_never()
                    .expect("scripted admission resolves without yielding")
                })
            })
            .collect();
        let decisions: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("admission thread"))
            .collect();
        assert_eq!(
            decisions
                .iter()
                .filter(|d| **d == AdmissionDecision::Admitted)
                .count(),
            1
        );
        assert_eq!(
            decisions
                .iter()
                .filter(|d| matches!(
                    d,
                    AdmissionDecision::Busy | AdmissionDecision::AlreadyQueued
                ))
                .count(),
            7
        );
        assert_eq!(queued_calls.load(Ordering::SeqCst), 1);
        assert_eq!(meter_calls.load(Ordering::SeqCst), 1);
        assert_eq!(mark.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stale_admission_claim_cannot_commit_or_release_newer_claim() {
        use futures_util::future::FutureExt as _;
        use std::sync::{
            atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
            Arc,
        };

        let mark = Arc::new(AtomicU8::new(0));
        let claimed = Arc::new(AtomicBool::new(false));
        let claim_epoch = Arc::new(AtomicU64::new(0));
        let mut stale = ClaimLedger::new_shared(
            Arc::clone(&mark),
            Arc::clone(&claimed),
            Arc::clone(&claim_epoch),
        );
        assert_eq!(
            stale
                .record_stage_once("job", "parse")
                .now_or_never()
                .unwrap()
                .unwrap(),
            AdmissionMark::Pending
        );

        // Model the first lease expiring, then a second request reclaiming it.
        claimed.store(false, Ordering::SeqCst);
        let mut current = ClaimLedger::new_shared(
            Arc::clone(&mark),
            Arc::clone(&claimed),
            Arc::clone(&claim_epoch),
        );
        assert_eq!(
            current
                .record_stage_once("job", "parse")
                .now_or_never()
                .unwrap()
                .unwrap(),
            AdmissionMark::Pending
        );

        assert!(stale
            .mark_queued("job", "parse")
            .now_or_never()
            .unwrap()
            .is_err());
        stale.release_claim("job", "parse").now_or_never().unwrap();

        let mut competitor = ClaimLedger::new_shared(
            Arc::clone(&mark),
            Arc::clone(&claimed),
            Arc::clone(&claim_epoch),
        );
        assert_eq!(
            competitor
                .record_stage_once("job", "parse")
                .now_or_never()
                .unwrap()
                .unwrap(),
            AdmissionMark::Busy
        );
        assert!(current
            .mark_queued("job", "parse")
            .now_or_never()
            .unwrap()
            .is_ok());
        assert_eq!(mark.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn admission_claim_without_busy_winner_releases_for_retry() {
        // Queue failure releases the atomic claim (production: claimed ->
        // pending or lease expiry), so the immediate retry reclaims and
        // still owes exactly one `Ok` — no Busy deadlock, no double meter.
        use std::sync::{
            atomic::{AtomicBool, AtomicU64, AtomicU8},
            Arc,
        };
        let msg = admission_msg();
        let mut ledger = ClaimLedger::new_shared(
            Arc::new(AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
        );
        let (mut queue_calls, mut meter_calls) = (0usize, 0usize);
        assert_eq!(
            drive_once(
                &mut ledger,
                &msg,
                "parse",
                false,
                false,
                &mut queue_calls,
                &mut meter_calls,
            ),
            AdmissionDecision::QueueUnavailable
        );
        assert_eq!((queue_calls, meter_calls), (1, 0));
        assert_eq!(
            drive_once(
                &mut ledger,
                &msg,
                "parse",
                true,
                false,
                &mut queue_calls,
                &mut meter_calls,
            ),
            AdmissionDecision::Admitted
        );
        assert_eq!((queue_calls, meter_calls), (2, 1));
    }
}

#[worker::event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    Router::new()
        .get_async("/healthz", |_, _| async { healthz() })
        .post_async("/v1/ingest/search", |mut req, ctx| async move {
            // Resolve once per request: the same trace rides the log lines,
            // the `x-trace-id` response echo, and the queued message. Every
            // response below passes through `with_trace`, so error shapes
            // from the admission/meter paths stay byte-identical.
            let trace_id = inbound_trace_id(&req);
            search(&mut req, &ctx, &trace_id)
                .await
                .map(|r| with_trace(r, &trace_id))
        })
        .post_async("/v1/ingest/nzb", |mut req, ctx| async move {
            let trace_id = inbound_trace_id(&req);
            upload_nzb(&mut req, &ctx, &trace_id)
                .await
                .map(|r| with_trace(r, &trace_id))
        })
        .get_async("/v1/ingest/jobs/:id", |req, ctx| async move {
            let trace_id = inbound_trace_id(&req);
            job_status(&ctx, &trace_id)
                .await
                .map(|r| with_trace(r, &trace_id))
        })
        .or_else_any_method_async("/*catchall", |req, _| async move {
            let trace_id = inbound_trace_id(&req);
            json_error(ApiError::not_found("not found")).map(|r| with_trace(r, &trace_id))
        })
        .run(req, env)
        .await
}

#[cfg(test)]
mod logging_trace_tests {
    use super::*;

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

#[cfg(test)]
mod build_sha_tests {
    use super::*;

    const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Every ingest line carries the normalized build identity through the
    /// REAL emitter (`nntune-logging`), not a hand-written fixture: a known
    /// SHA and the explicit unknown marker both survive validation and keep
    /// the six-key envelope.
    #[test]
    fn emitted_lines_carry_build_sha_known_or_unknown() {
        for sha in [SHA, nntune_logging::BUILD_SHA_UNKNOWN] {
            let normalized = nntune_logging::parse_build_sha(Some(sha));
            let record = LogRecord::new(
                LogWorker::Ingest,
                Level::Info,
                TRACE,
                "ingest.search.received",
                nntune_logging::fields_from_json(serde_json::json!({
                    "job_id": "job_01",
                    "q_len": 9,
                    "build_sha": normalized,
                }))
                .unwrap(),
                1_792_094_400_123,
            )
            .expect("ingest line with build_sha must render");
            let parsed: serde_json::Value = serde_json::from_str(&record.to_json_line())
                .expect("record must be one JSON object");
            let object = parsed.as_object().expect("record must be an object");
            assert_eq!(object.len(), 6, "exactly the six envelope keys");
            assert_eq!(object["fields"]["build_sha"], sha);
        }
    }

    #[test]
    fn every_emit_site_wires_build_sha() {
        // `build_sha_from_env` needs a wasm Env, so pin the call sites in
        // source the way the throttle-gate test pins the route shape: slice
        // the pre-test source so this test's own literals cannot satisfy it.
        // All four fetch lines plus the queue-consumer line must carry the
        // normalized key — never a missing key or a raw var.
        let source = include_str!("lib.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        assert!(
            production.contains("fn build_sha_from_env("),
            "the BUILD_SHA reader must exist"
        );
        assert!(
            production.contains("nntune_logging::parse_build_sha"),
            "the reader must normalize through the shared validator"
        );
        for event in [
            "ingest.search.received",
            "ingest.payload.store_failed",
            "ingest.upload.received",
            "ingest.job.status_checked",
        ] {
            assert!(production.contains(event), "fetch entry must emit {event}");
        }
        // Match the key literal inside the `json!` fields, not the bare
        // substring: `let build_sha = …` alone must never satisfy this.
        // Four fetch emit sites carry the key, so a dropped key at any one
        // of them drops the count below four.
        assert_eq!(
            production.matches("\"build_sha\": build_sha").count(),
            4,
            "all four fetch lines must carry the build_sha key"
        );
        let consumer = include_str!("queue_consumer.rs");
        let consumer_production = consumer
            .split_once("#[cfg(test)]")
            .expect("consumer test module must exist")
            .0;
        assert!(
            consumer_production.contains("ingest.job.consumed"),
            "consumer must emit ingest.job.consumed"
        );
        assert_eq!(
            consumer_production
                .matches("\"build_sha\": build_sha")
                .count(),
            1,
            "the consumed line must carry the build_sha key"
        );
    }
}
