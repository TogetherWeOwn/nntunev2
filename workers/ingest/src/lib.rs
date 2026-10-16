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
    let headers = worker::Headers::new();
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
    let headers = headers_json().unwrap_or_default();
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
fn try_meter(
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
        status => json_error("unknown meter stage", status),
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
/// 1. `record_stage_once` inserts (`job_id`, `stage`) as `pending`, or
///    returns the existing mark. A storage failure answers 503
///    (fail-closed): nothing was queued or metered.
/// 2. An existing `queued` mark means a previous attempt finished the
///    whole outbox (Queue accepted AND meter written): replay 202 without
///    re-queueing or re-metering. This is the idempotency that closes the
///    retry race above.
/// 3. Otherwise the Queue send runs first, so the meter outcome below is
///    truthful: a failed send answers 503 with no meter event at all (the
///    ledger stays `pending`; the retry resumes here and still owes
///    exactly one `Ok`).
/// 4. On a successful send exactly one `Ok` meter event is written. A
///    meter failure answers 503 with the ledger left `pending`: the Queue
///    already holds one message (an orphan), so the retry re-queues and
///    re-meters. The duplicate Queue message is harmless by construction
///    (the consumer converges on `idempotency_key` — `library.rs`
///    `UNIQUE(idempotency_key)` — plus `jobs::JobCheckpoint` resume and
///    Queue redelivery), while the meter count stays exact. Marking first
///    and metering second would risk a silent drop instead; this order
///    risks at most a wasted queue slot, never a lost event.
/// 5. `mark_queued` commits the outbox; only then is 202 answered.
///    Crash-window honesty: the dataset write and the ledger commit are
///    two systems with no shared transaction, so an isolate that dies
///    between them can leave `pending` behind a written event; reload
///    recovery re-drives `pending` (at most a duplicate `Ok` in that
///    narrow window, never a silent drop — this slice forbids drops).
///
/// Production backing is the `metered_admission` table in the existing
/// `LIBRARY_DB` Hyperdrive database (`migrations/0004_*`): `PRIMARY KEY
/// (job_id, stage)` makes concurrent `record_stage_once` inserts converge
/// (one row wins; every loser reads the same mark), which a process-local
/// set or an eventually consistent marker cannot do.
///
/// Outbox mark for one (`job_id`, `stage`) admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum AdmissionMark {
    /// Intent recorded; the Queue has not observably accepted the job and
    /// no `Ok` meter event is owed yet.
    Pending,
    /// Queue accepted AND the entry `Ok` meter event was written: the
    /// admission is complete and replays as 202 with no further writes.
    Queued,
}

/// Authoritative durable ledger keyed by (`job_id`, `stage`).
///
/// Production: [`PgAdmissionLedger`] over Hyperdrive Postgres, where the
/// primary key is the concurrency authority. Tests drive the same seam
/// with in-memory and file-backed doubles (see `mod tests`).
trait AdmissionLedger {
    /// Insert (`job_id`, `stage`) as `pending`, or return the existing
    /// mark when a previous attempt already recorded it. A storage
    /// failure maps to a retryable [`MeterWriteError`] (fail-closed).
    async fn record_stage_once(
        &mut self,
        job_id: &str,
        stage: &str,
    ) -> Result<AdmissionMark, MeterWriteError>;
    /// Commit a `pending` admission to `queued` after the Queue send was
    /// accepted and the `Ok` meter event was written.
    async fn mark_queued(&mut self, job_id: &str, stage: &str) -> Result<(), MeterWriteError>;
}

/// Parameterized statements owned here (execution lives with the ledger
/// impls); see `migrations/0004_metered_admission.sql` for the table.
/// `$1` is the job id, `$2` is the stage, everywhere.
fn admission_record_sql() -> &'static str {
    "INSERT INTO metered_admission (job_id, stage, state) VALUES ($1, $2, 'pending') \
     ON CONFLICT (job_id, stage) DO NOTHING"
}

fn admission_read_mark_sql() -> &'static str {
    "SELECT state FROM metered_admission WHERE job_id = $1 AND stage = $2"
}

fn admission_mark_queued_sql() -> &'static str {
    "UPDATE metered_admission SET state = 'queued' WHERE job_id = $1 AND stage = $2"
}

/// [`AdmissionLedger`] over the `LIBRARY_DB` Hyperdrive database.
///
/// A fresh connection per statement (Hyperdrive pools underneath): one
/// statement never shares a client across awaits, so a pooled-route
/// hiccup fails that statement (fail-closed 503), never the isolate.
/// Connection detail never reaches logs or errors: every failure maps to
/// a retryable transport error with no host/role strings.
struct PgAdmissionLedger {
    env: Env,
}

impl PgAdmissionLedger {
    fn from_env(env: &Env) -> Self {
        Self { env: env.clone() }
    }

    async fn client(&self) -> Result<tokio_postgres::Client, MeterWriteError> {
        let unavailable = || MeterWriteError::Transport("admission ledger unavailable".to_string());
        let hyperdrive = self
            .env
            .hyperdrive(crate::library::LIBRARY_DB_BINDING)
            .map_err(|e| {
                console_log!("admission ledger binding missing: {:?}", e);
                unavailable()
            })?;
        let socket = worker::Socket::builder()
            .secure_transport(worker::SecureTransport::StartTls)
            .connect(hyperdrive.host(), hyperdrive.port())
            .map_err(|_| unavailable())?;
        let config: tokio_postgres::Config = hyperdrive
            .connection_string()
            .parse()
            .map_err(|_| unavailable())?;
        let (client, connection) = config
            .connect_raw(socket, worker::postgres_tls::PassthroughTls)
            .await
            .map_err(|_| unavailable())?;
        wasm_bindgen_futures::spawn_local(async move {
            if connection.await.is_err() {
                console_log!("admission ledger connection error");
            }
        });
        Ok(client)
    }

    fn parse_mark(state: String) -> Result<AdmissionMark, MeterWriteError> {
        match state.as_str() {
            "pending" => Ok(AdmissionMark::Pending),
            "queued" => Ok(AdmissionMark::Queued),
            _ => Err(MeterWriteError::Transport(
                "admission ledger returned an unknown mark".to_string(),
            )),
        }
    }
}

impl AdmissionLedger for PgAdmissionLedger {
    async fn record_stage_once(
        &mut self,
        job_id: &str,
        stage: &str,
    ) -> Result<AdmissionMark, MeterWriteError> {
        let unavailable = || MeterWriteError::Transport("admission ledger unavailable".to_string());
        let client = self.client().await?;
        client
            .execute(admission_record_sql(), &[&job_id, &stage])
            .await
            .map_err(|e| {
                console_log!("admission ledger record failed: {:?}", e);
                unavailable()
            })?;
        let row = client
            .query_opt(admission_read_mark_sql(), &[&job_id, &stage])
            .await
            .map_err(|e| {
                console_log!("admission ledger read failed: {:?}", e);
                unavailable()
            })?;
        match row {
            Some(row) => {
                let state: String = row.get(0);
                Self::parse_mark(state)
            }
            None => Err(unavailable()),
        }
    }

    async fn mark_queued(&mut self, job_id: &str, stage: &str) -> Result<(), MeterWriteError> {
        let client = self.client().await?;
        client
            .execute(admission_mark_queued_sql(), &[&job_id, &stage])
            .await
            .map_err(|e| {
                console_log!("admission ledger commit failed: {:?}", e);
                MeterWriteError::Transport("admission ledger unavailable".to_string())
            })?;
        Ok(())
    }
}

/// Outcome of one `drive_metered_admission` pass. Rendering to HTTP lives
/// in `admit_metered_job`, so this stays free of `worker::Response` and
/// unit-testable on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AdmissionDecision {
    /// Ledger already `queued`: replay 202 with no queue/meter writes.
    AlreadyQueued,
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
    if !queue_send().await {
        return AdmissionDecision::QueueUnavailable;
    }
    if let Err(e) = meter_emit(MeterOutcome::Ok).await {
        return AdmissionDecision::MeterUnavailable(e);
    }
    if let Err(e) = ledger.mark_queued(&msg.job_id, stage).await {
        return AdmissionDecision::MeterUnavailable(e);
    }
    AdmissionDecision::Admitted
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
        AdmissionDecision::AlreadyQueued | AdmissionDecision::Admitted => {
            queued_envelope(msg, source, true, extra)
        }
        AdmissionDecision::QueueUnavailable => queued_envelope(msg, source, false, extra),
        AdmissionDecision::MeterUnavailable(e) => meter_failure_response(&e),
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
        Err((message, status)) => return json_error(&message, status),
    };
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
/// `nntune-core`, probes the pool DO, and verifies before matching.
async fn upload_nzb(req: &mut Request, ctx: &RouteContext<()>) -> Result<Response> {
    let text = match read_capped_text(req, MAX_UPLOAD_BODY_BYTES).await {
        Ok(text) => text,
        Err((message, status)) => return json_error(&message, status),
    };
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

    // --- Durable metered admission seam ---------------------------------
    //
    // The production ledger is Postgres (`PgAdmissionLedger`, primary key
    // as the concurrency authority). These doubles prove the admission
    // state machine drives the `AdmissionLedger` seam correctly under
    // reload, concurrency, and failure — no Worker runtime needed.

    /// In-memory ledger double: the `Mutex` (or sole ownership) around it
    /// stands in for the Postgres primary key in production. Injected
    /// `fail_*` flags model ledger commit failures.
    struct MemLedger {
        marks: std::collections::BTreeMap<(String, String), AdmissionMark>,
        fail_record: bool,
        fail_commit: bool,
        record_calls: usize,
    }

    impl MemLedger {
        fn new() -> Self {
            Self {
                marks: std::collections::BTreeMap::new(),
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
            Ok(*self
                .marks
                .entry((job_id.to_string(), stage.to_string()))
                .or_insert(AdmissionMark::Pending))
        }

        async fn mark_queued(&mut self, job_id: &str, stage: &str) -> Result<(), MeterWriteError> {
            if self.fail_commit {
                return Err(MeterWriteError::Transport(
                    "injected commit_failure".to_string(),
                ));
            }
            self.marks.insert(
                (job_id.to_string(), stage.to_string()),
                AdmissionMark::Queued,
            );
            Ok(())
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
                return Ok(row.state);
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
            admission_record_sql(),
            admission_read_mark_sql(),
            admission_mark_queued_sql(),
        ] {
            assert!(
                sql.contains("job_id") && sql.contains("stage"),
                "ledger statements are keyed by (job_id, stage): {sql}"
            );
        }
        assert!(
            admission_record_sql().contains("ON CONFLICT (job_id, stage) DO NOTHING"),
            "concurrent records must converge on one row"
        );
        assert!(admission_record_sql().contains("pending"));
        assert!(admission_mark_queued_sql().contains("queued"));
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

    #[test]
    fn admission_competing_inserts_converge_on_one_meter_write() {
        // Eight concurrent admissions for the same job: the ledger serializes
        // the intent (production: the primary key), so exactly one attempt
        // meters and the rest replay without side effects.
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        };
        let ledger = Arc::new(Mutex::new(MemLedger::new()));
        let meter_calls = Arc::new(AtomicUsize::new(0));
        let admitted = Arc::new(AtomicUsize::new(0));
        let msg = admission_msg();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let ledger = Arc::clone(&ledger);
                let meter_calls = Arc::clone(&meter_calls);
                let admitted = Arc::clone(&admitted);
                let msg = msg.clone();
                std::thread::spawn(move || {
                    use futures_util::future::FutureExt as _;
                    let mut guard = ledger.lock().expect("ledger lock");
                    let decision = drive_metered_admission(
                        &mut *guard,
                        &msg,
                        "parse",
                        || async move { true },
                        |outcome| {
                            assert_eq!(outcome, MeterOutcome::Ok);
                            meter_calls.fetch_add(1, Ordering::SeqCst);
                            async move { Ok(()) }
                        },
                    )
                    .now_or_never()
                    .expect("scripted admission resolves without yielding");
                    if decision == AdmissionDecision::Admitted {
                        admitted.fetch_add(1, Ordering::SeqCst);
                    }
                    decision
                })
            })
            .collect();
        for handle in handles {
            let decision = handle.join().expect("admission thread");
            assert!(matches!(
                decision,
                AdmissionDecision::Admitted | AdmissionDecision::AlreadyQueued
            ));
        }
        assert_eq!(meter_calls.load(Ordering::SeqCst), 1);
        assert_eq!(admitted.load(Ordering::SeqCst), 1);
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
