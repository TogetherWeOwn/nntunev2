//! Workers runtime (wasm32): the no-route transcode Worker and its
//! per-job Container Durable Object.
//!
//! - The Worker's `fetch` exists only for the ingest service binding
//!   (`workers_dev = false`, no routes): it validates the request and
//!   forwards it to the Durable Object named by `job_id`, so one job's
//!   encodes serialise behind one object.
//! - The Durable Object owns the Container for that job. It streams
//!   the staged input from `INGEST_SCRATCH` into the Container, runs
//!   one encode, fetches the artifact back, verifies it against the
//!   Container's reply, writes `out/artifact.ogg` plus `out/result.json`
//!   last (the idempotency marker). The Container itself holds no
//!   credentials and is started with internet access disabled.

use std::time::Duration;

use futures_util::StreamExt;
use worker::wasm_bindgen::JsValue;
use worker::{
    console_log, durable_object, event, Bucket, ContainerStartupOptions, Context, Delay,
    DurableObject, Env, Fetcher, Headers, Method, Request, RequestInit, Response, ResponseBody,
    Result as WorkerResult, State,
};

use crate::contract::{
    artifact_key, classify_container_reply, container_transcode_body, container_unavailable_fault,
    in_key, ingest_reply_body, parse_request, plan, push_chunk_capped, result_key, settle_do_reply,
    transcode_unavailable_fault, validate_transcoded_result, verify_artifact, ApiFault,
    ContainerOutcome, Plan, TranscodeRequest, TranscodeStatus, ARTIFACT_NAME, MAX_ARTIFACT_BYTES,
    MAX_REQUEST_BYTES, MAX_RESULT_BYTES, MAX_TRANSCODE_INPUT_BYTES,
};

/// Durable Object binding + class fronting the Container.
pub const DO_BINDING: &str = "TRANSCODE_JOBS";
/// R2 binding for the per-environment ingest scratch bucket.
pub const SCRATCH_BINDING: &str = "INGEST_SCRATCH";
/// The Container server's port (`containers/transcode`).
const CONTAINER_PORT: u16 = 8080;
/// Container cold-start budget: poll `/health` every 500 ms.
const HEALTH_ATTEMPTS: u32 = 60;
/// Longest one Container fetch may take, headers plus body sharing one
/// cancellable deadline. `stage_input` and the `/health` + `/cleanup`
/// exchanges arm from this constant (size-scaled atop the base where a
/// request or reply body streams); `/transcode` runs ffmpeg
/// synchronously under the Container's 300 s budget so it arms from
/// `CONTAINER_TRANSCODE_DEADLINE_MS` instead (same `*_MS: u64` shape as
/// the sibling ingest MB deadline).
const CONTAINER_FETCH_DEADLINE_MS: u64 = 10_000;
/// `/transcode` deadline: the Container defaults to 300 s
/// (`containers/transcode/src/lib.rs`) and the Worker never sets
/// `timeout_secs`, so a legitimate encode may take up to 300 s before
/// headers arrive; 30 s covers the small result-body read and scheduling
/// margin while still failing a hung Container.
const CONTAINER_TRANSCODE_DEADLINE_MS: u64 = 330_000;
/// Total front-to-DO cutoff: 330 s for the child encode exchange plus
/// 270 s shared by result reads, health, input staging, artifact
/// relay, result-marker write and cleanup. This is a cutoff, not the
/// sum of all per-file allowances: slow/large resource-permitted
/// encodes can exceed it. Ingest waits 660 s (below its enclosing
/// 900 s queue invocation contract), leaving 60 s to forward this
/// hop's verdict. Serial batch admission remains a separate
/// consumer-activation gate. See README.md for staging/health bounds.
const DO_TRANSCODE_DEADLINE_MS: u64 = 600_000;

fn json_response(status: u16, body: Vec<u8>) -> WorkerResult<Response> {
    let headers = Headers::new();
    let _ = headers.set("content-type", "application/json");
    Ok(Response::from_bytes(body)?
        .with_status(status)
        .with_headers(headers))
}

fn fault_response(fault: &ApiFault) -> WorkerResult<Response> {
    json_response(
        fault.status,
        serde_json::to_vec(&serde_json::json!({"error": fault.code})).unwrap_or_default(),
    )
}

/// Read a request body with a hard cap (streamed, never unbounded).
async fn read_capped(req: &mut Request, cap: usize) -> Result<Vec<u8>, ApiFault> {
    let mut stream = req
        .stream()
        .map_err(|_| ApiFault::new(400, "invalid_request"))?;
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ApiFault::new(400, "invalid_request"))?;
        body.extend_from_slice(&chunk);
        if body.len() > cap {
            return Err(ApiFault::new(413, "payload_too_large"));
        }
    }
    Ok(body)
}

/// Read a response body with a hard cap (streamed, never unbounded).
/// Each chunk goes through `push_chunk_capped`, so the chunk that
/// would cross the cap aborts the read with the accumulated body
/// untouched: at most `cap` bytes are ever materialised and the
/// excess is never retained. A bodyless response (`ResponseBody::Empty`,
/// e.g. a bare 200 from `/health`) reads as empty — the same value the
/// previous whole-body `bytes()` path returned for it, since `stream()`
/// rejects non-stream bodies. An already-materialised
/// (`ResponseBody::Body`) reply is length-checked before cloning, so it
/// never buffers past the cap either. Callers map the unit error to
/// their own fault.
async fn read_response_capped(response: &mut Response, cap: usize) -> Result<Vec<u8>, ()> {
    match response.body() {
        ResponseBody::Empty => return Ok(Vec::new()),
        ResponseBody::Body(body) if body.len() > cap => return Err(()),
        ResponseBody::Body(body) => return Ok(body.clone()),
        ResponseBody::Stream(_) => {}
    }
    let mut stream = response.stream().map_err(|_| ())?;
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ())?;
        if !push_chunk_capped(&mut body, &chunk, cap) {
            return Err(());
        }
    }
    Ok(body)
}

/// The Worker's only route. Reachable in production solely through the
/// ingest `TRANSCODE_CONTAINER` service binding.
#[event(fetch)]
async fn fetch(mut req: Request, env: Env, _ctx: Context) -> WorkerResult<Response> {
    if req.method() != Method::Post || req.path() != "/transcode" {
        return fault_response(&ApiFault::new(404, "not_found"));
    }
    let body = match read_capped(&mut req, MAX_REQUEST_BYTES).await {
        Ok(body) => body,
        Err(fault) => return fault_response(&fault),
    };
    let parsed = match parse_request(&body) {
        Ok(parsed) => parsed,
        Err(fault) => return fault_response(&fault),
    };
    let stub = match env
        .durable_object(DO_BINDING)
        .and_then(|ns| ns.id_from_name(&parsed.job_id)?.get_stub())
    {
        Ok(stub) => stub,
        Err(error) => {
            console_log!("transcode DO unavailable: {:?}", error);
            return fault_response(&transcode_unavailable_fault());
        }
    };
    let headers = Headers::new();
    let _ = headers.set("content-type", "application/json");
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(&String::from_utf8_lossy(&body))));
    let request = match Request::new_with_init("https://transcode-do/transcode", &init) {
        Ok(request) => request,
        Err(_) => return fault_response(&transcode_unavailable_fault()),
    };
    // The settlement polls (registers) Delay before dispatching the lazy
    // fetch and owns it through body completion, releasing it on every exit.
    let deadline = Box::pin(Delay::from(Duration::from_millis(DO_TRANSCODE_DEADLINE_MS)));
    let fetch = Box::pin(async {
        let mut response = stub.fetch_with_request(request).await.map_err(|error| {
            console_log!(
                "transcode DO call failed job={}: {:?}",
                parsed.job_id,
                error
            );
        })?;
        let status = response.status_code();
        // The shared reader preserves Empty and materialised bodies, checking
        // the length BEFORE cloning. It runs inside this timed fetch future.
        // Streamed bodies instead drain chunk-by-chunk in the settlement.
        let stream = match response.body() {
            ResponseBody::Empty | ResponseBody::Body(_) => {
                let read = read_response_capped(&mut response, MAX_RESULT_BYTES);
                let body = read.await?;
                futures_util::stream::iter([Ok(body)]).boxed_local()
            }
            ResponseBody::Stream(_) => response
                .stream()
                .map_err(|_| ())?
                .map(|chunk| chunk.map_err(|_| ()))
                .boxed_local(),
        };
        Ok((status, stream))
    });
    match settle_do_reply(fetch, deadline).await {
        Ok((status, bytes)) => json_response(status, bytes),
        Err(_) => fault_response(&ApiFault::new(503, "transcode_unavailable")),
    }
}

/// One Container per ingest job. Encodes for the job serialise here.
#[durable_object(fetch)]
pub struct TranscodeContainer {
    state: State,
    env: Env,
}

impl DurableObject for TranscodeContainer {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, mut req: Request) -> WorkerResult<Response> {
        if req.method() != Method::Post || req.path() != "/transcode" {
            return fault_response(&ApiFault::new(404, "not_found"));
        }
        let body = match read_capped(&mut req, MAX_REQUEST_BYTES).await {
            Ok(body) => body,
            Err(fault) => return fault_response(&fault),
        };
        let parsed = match parse_request(&body) {
            Ok(parsed) => parsed,
            Err(fault) => return fault_response(&fault),
        };
        // The object is addressed by job_id; refuse envelopes for any
        // other job so one job can never drive another's Container.
        if self.state.id().name().as_deref() != Some(parsed.job_id.as_str()) {
            return fault_response(&ApiFault::invalid());
        }
        match self.run(&parsed).await {
            Ok(result_body) => json_response(200, result_body),
            Err(fault) => fault_response(&fault),
        }
    }
}

impl TranscodeContainer {
    async fn run(&self, req: &TranscodeRequest) -> Result<Vec<u8>, ApiFault> {
        let bucket = self
            .env
            .bucket(SCRATCH_BINDING)
            .map_err(|_| ApiFault::new(503, "scratch_unavailable"))?;
        let prefix = &req.scratch_prefix;

        // Read the stored result first: idempotency is keyed on the
        // request's staging id. A stored result only short-circuits
        // when it was produced for exactly this staging (a redelivery
        // replays it; the previous input's verdict never does).
        let existing = read_object_capped(&bucket, &result_key(prefix), MAX_RESULT_BYTES).await?;
        let stale_generation = match plan(req, existing.as_deref(), &req.staging_id)? {
            Plan::AlreadyDone(result) => return Ok(ingest_reply_body(&result)),
            Plan::Rerun => true,
            Plan::Run => false,
        };

        let container = self.container_port().await?;
        if stale_generation {
            // Order matters: clean the Container, then delete the
            // stale R2 result, then stage and run. The cleanup targets
            // this request's staging (a no-op when a previous attempt
            // already cleaned it) and clears any crash leftovers from
            // an attempt that died mid-run. Deleting the stale R2
            // marker right after means a retry inside this new
            // generation plans as `Run` (no stored result) and the
            // Container replays its own finished result instead of
            // being cleaned again and paid for twice.
            let cleanup = serde_json::to_vec(
                &serde_json::json!({"job_id": req.job_id, "staging_id": req.staging_id}),
            )
            .unwrap_or_default();
            let (status, _) =
                container_json(&container, Method::Post, "/cleanup", Some(cleanup)).await?;
            if status != 200 {
                return Err(ApiFault::new(502, "container_unavailable"));
            }
            bucket
                .delete(result_key(prefix))
                .await
                .map_err(|_| ApiFault::new(503, "scratch_unavailable"))?;
        }

        self.stage_input(&bucket, &container, req).await?;

        let body = serde_json::to_vec(&container_transcode_body(req))
            .map_err(|_| ApiFault::new(500, "transcode_unavailable"))?;
        let (status, reply) =
            container_json(&container, Method::Post, "/transcode", Some(body)).await?;
        let mut result = match classify_container_reply(status, &reply) {
            ContainerOutcome::Done(result) => result,
            ContainerOutcome::Forward(status, code) => {
                return Err(ApiFault::new(status, leak_code(code)));
            }
        };
        validate_transcoded_result(&result)?;
        // Stamp the inputs this verdict belongs to before it becomes
        // the idempotency marker in `out/result.json`.
        result.staging_id = req.staging_id.clone();

        if result.status == TranscodeStatus::Transcoded {
            self.fetch_artifact(&bucket, &container, req, &result)
                .await?;
        }

        // The result marker lands only after the artifact is durable.
        // The full document stays in scratch; ingest gets the small
        // status reply (see `ingest_reply_body`).
        let result_body =
            serde_json::to_vec(&result).map_err(|_| ApiFault::new(500, "transcode_unavailable"))?;
        bucket
            .put(result_key(prefix), result_body)
            .execute()
            .await
            .map_err(|_| ApiFault::new(503, "scratch_unavailable"))?;
        let reply_body = ingest_reply_body(&result);

        // Free the Container's disk for the next input; failure here is
        // not fatal (the Container is per-job and ephemeral, and the
        // durable copies already live in R2).
        let cleanup = serde_json::to_vec(
            &serde_json::json!({"job_id": req.job_id, "staging_id": req.staging_id}),
        )
        .unwrap_or_default();
        if container_json(&container, Method::Post, "/cleanup", Some(cleanup))
            .await
            .is_err()
        {
            console_log!("transcode cleanup failed job={}", req.job_id);
        }
        console_log!(
            "transcode done job={} status={} bytes={}",
            req.job_id,
            result.status.as_str(),
            result.byte_size.unwrap_or(0)
        );
        Ok(reply_body)
    }

    /// Start the Container if needed (internet disabled: it needs no
    /// egress and must never become one) and wait for `/health`.
    async fn container_port(&self) -> Result<Fetcher, ApiFault> {
        let container = self
            .state
            .container()
            .ok_or_else(|| ApiFault::new(502, "container_unavailable"))?;
        if !container.running() {
            let mut options = ContainerStartupOptions::new();
            options.enable_internet(false);
            container
                .start(Some(options))
                .map_err(|_| ApiFault::new(502, "container_unavailable"))?;
        }
        let port = container
            .get_tcp_port(CONTAINER_PORT)
            .map_err(|_| ApiFault::new(502, "container_unavailable"))?;
        for _ in 0..HEALTH_ATTEMPTS {
            if let Ok((200, _)) = container_json(&port, Method::Get, "/health", None).await {
                return Ok(port);
            }
            Delay::from(Duration::from_millis(500)).await;
        }
        Err(ApiFault::new(502, "container_unavailable"))
    }

    /// Stream the staged R2 input into the Container. The body rides
    /// as the object's own readable stream with an explicit length;
    /// the Container accepts fixed-length and chunked bodies. The R2
    /// size is checked before streaming: an empty or oversize object
    /// is a corrupt staging, terminal for this input.
    async fn stage_input(
        &self,
        bucket: &Bucket,
        container: &Fetcher,
        req: &TranscodeRequest,
    ) -> Result<(), ApiFault> {
        let object = bucket
            .get(in_key(&req.scratch_prefix, &req.input_key))
            .execute()
            .await
            .map_err(|_| ApiFault::new(503, "scratch_unavailable"))?
            .ok_or_else(|| ApiFault::new(404, "scratch_missing"))?;
        if object.size() == 0 || object.size() > MAX_TRANSCODE_INPUT_BYTES {
            return Err(ApiFault::new(400, "scratch_input_mismatch"));
        }
        let size = object.size();
        let body = object
            .body()
            .ok_or_else(|| ApiFault::new(404, "scratch_missing"))?
            .response_body()
            .map_err(|_| ApiFault::new(503, "scratch_unavailable"))?;
        let stream = match body {
            ResponseBody::Stream(stream) => stream,
            _ => return Err(ApiFault::new(503, "scratch_unavailable")),
        };
        let headers = Headers::new();
        let _ = headers.set("content-length", &size.to_string());
        let mut init = RequestInit::new();
        init.with_method(Method::Put)
            .with_headers(headers)
            .with_body(Some(JsValue::from(stream)));
        let url = format!(
            "http://container/jobs/{}/t/{}/in/{}",
            req.job_id, req.staging_id, req.input_key
        );
        let request = Request::new_with_init(&url, &init)
            .map_err(|_| ApiFault::new(502, "container_unavailable"))?;
        // One cancellable deadline spans the upload headers plus the
        // Container's reply headers, armed before the fetch is issued so
        // slow headers count against the same budget as a stalled upload
        // (same shape as the ingest pool-stub deadline). The upload rides
        // inside `fetch_request`: the Container only answers 201 after
        // reading and storing the streamed R2 body, so the total scales
        // with the input size atop the 10 s base (~1 MB/s floor) instead of
        // expiring a large healthy transfer mid-upload; retries still
        // restart from byte 0 but no longer hit the same wall. `Delay`
        // only registers its timer on first poll, so racing both futures
        // below polls (arms) the deadline from the start. Dropping the
        // pinned `Delay` releases its timer, and every return below drops
        // it: expiry, transport failure, and success all release the timer.
        let budget_ms = CONTAINER_FETCH_DEADLINE_MS.saturating_add(size.saturating_div(1024));
        let mut deadline = Box::pin(Delay::from(Duration::from_millis(budget_ms)));
        let mut fetch = Box::pin(container.fetch_request(request));
        let response = std::future::poll_fn(|cx| {
            use std::task::Poll;
            // Deadline first: an already-fired deadline aborts even when
            // headers are also ready.
            if std::future::Future::poll(deadline.as_mut(), cx).is_ready() {
                return Poll::Ready(None);
            }
            match std::future::Future::poll(fetch.as_mut(), cx) {
                Poll::Ready(response) => Poll::Ready(Some(response)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
        let response = match response {
            Some(Ok(response)) => response,
            Some(Err(_)) | None => return Err(ApiFault::new(502, "container_unavailable")),
        };
        let status = response.status_code();
        // The PUT reply carries no verdict beyond its status: cancel the
        // body instead of collecting bytes that are never read, so a
        // misbehaving container cannot make us buffer an unbounded reply
        // or stall the input until a discarded body arrives. Expiry
        // already failed the fetch above as 502 container_unavailable;
        // the known statuses below keep their mapping (a stored 201 stays
        // success, 400/413 stay terminal) instead of becoming a
        // retryable 502.
        drop(deadline);
        if let ResponseBody::Stream(stream) = response.body() {
            let _ = stream.cancel();
        }
        match status {
            201 => Ok(()),
            400 | 413 => Err(ApiFault::new(status, "scratch_input_mismatch")),
            _ => Err(ApiFault::new(502, "container_unavailable")),
        }
    }

    /// Fetch the encoded artifact from the Container into a capped
    /// buffer, verify it against the Container reply's hash and size,
    /// and only then persist it to R2. Buffering (instead of
    /// streaming straight through) is the point: a corrupt or
    /// substituted artifact must fail here, never as a published
    /// cache row. The 32 MiB ceiling fits the Durable Object memory
    /// with one encode in flight per job.
    async fn fetch_artifact(
        &self,
        bucket: &Bucket,
        container: &Fetcher,
        req: &TranscodeRequest,
        result: &crate::contract::TranscodeResult,
    ) -> Result<(), ApiFault> {
        let (Some(expected_sha), Some(expected_size)) =
            (result.artifact_sha256.as_deref(), result.byte_size)
        else {
            return Err(ApiFault::new(502, "container_unavailable"));
        };
        let url = format!(
            "http://container/jobs/{}/t/{}/out/{ARTIFACT_NAME}",
            req.job_id, req.staging_id
        );
        // One cancellable deadline spans the artifact fetch plus the R2
        // put that persists it, armed before the fetch is issued (same
        // shape as `stage_input` above). The total scales with the
        // validated reply size atop the 10 s base (~1 MB/s floor) so a
        // large healthy artifact does not expire mid-stream. Dropping
        // the pinned `Delay` releases its timer, and every return below
        // drops it.
        let budget_ms =
            CONTAINER_FETCH_DEADLINE_MS.saturating_add(expected_size.saturating_div(1024));
        let mut deadline = Box::pin(Delay::from(Duration::from_millis(budget_ms)));
        let mut fetch = Box::pin(container.fetch(url, None));
        let response = std::future::poll_fn(|cx| {
            use std::task::Poll;
            // Deadline first: an already-fired deadline aborts even when
            // headers are also ready.
            if std::future::Future::poll(deadline.as_mut(), cx).is_ready() {
                return Poll::Ready(None);
            }
            match std::future::Future::poll(fetch.as_mut(), cx) {
                Poll::Ready(response) => Poll::Ready(Some(response)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
        let mut response = match response {
            Some(Ok(response)) => response,
            Some(Err(_)) | None => return Err(ApiFault::new(502, "container_unavailable")),
        };
        if response.status_code() != 200 {
            return Err(ApiFault::new(502, "container_unavailable"));
        }
        // Reuse the same deadline for the capped read: partial progress
        // never re-arms it, so headers plus body share one total
        // budget. The artifact ceiling (32 MiB) — not the 1 MiB result
        // cap — bounds this read.
        let mut read = Box::pin(read_response_capped(&mut response, MAX_ARTIFACT_BYTES));
        let bytes = std::future::poll_fn(|cx| {
            use std::task::Poll;
            if std::future::Future::poll(deadline.as_mut(), cx).is_ready() {
                return Poll::Ready(None);
            }
            match std::future::Future::poll(read.as_mut(), cx) {
                Poll::Ready(bytes) => Poll::Ready(Some(bytes)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
        let bytes = match bytes {
            Some(Ok(bytes)) => bytes,
            Some(Err(_)) | None => return Err(ApiFault::new(502, "container_unavailable")),
        };
        verify_artifact(&bytes, expected_sha, expected_size)?;
        // Reuse the same deadline for the put: partial progress never
        // re-arms it, so fetch plus put share one total budget. Expiry
        // fails as the existing 502 container_unavailable; an R2
        // transport error keeps the existing 503 scratch_unavailable.
        let mut put = Box::pin(
            bucket
                .put(artifact_key(&req.scratch_prefix), bytes)
                .execute(),
        );
        let stored = std::future::poll_fn(|cx| {
            use std::task::Poll;
            if std::future::Future::poll(deadline.as_mut(), cx).is_ready() {
                return Poll::Ready(None);
            }
            match std::future::Future::poll(put.as_mut(), cx) {
                Poll::Ready(stored) => Poll::Ready(Some(stored)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
        drop(deadline);
        match stored {
            Some(Ok(_)) => Ok(()),
            Some(Err(_)) => Err(ApiFault::new(503, "scratch_unavailable")),
            None => Err(ApiFault::new(502, "container_unavailable")),
        }
    }
}

/// Codes forwarded from the Container are validated lowercase
/// identifiers (see `classify_container_reply`); leaking them into the
/// `'static` fault keeps the reply allocation-free of owned strings.
/// The set is closed and tiny.
fn leak_code(code: String) -> &'static str {
    match code.as_str() {
        "invalid_request" => "invalid_request",
        "not_found" => "not_found",
        "payload_too_large" => "payload_too_large",
        "transcode_input_conflict" => "transcode_input_conflict",
        "transcode_timeout" => "transcode_timeout",
        "transcode_resource" => "transcode_resource",
        "transcode_failed" => "transcode_failed",
        "scratch_input_mismatch" => "scratch_input_mismatch",
        _ => "container_unavailable",
    }
}

/// One small JSON exchange with the Container.
async fn container_json(
    port: &Fetcher,
    method: Method,
    path: &str,
    body: Option<Vec<u8>>,
) -> Result<(u16, Vec<u8>), ApiFault> {
    let headers = Headers::new();
    let _ = headers.set("content-type", "application/json");
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if let Some(body) = body {
        init.with_body(Some(JsValue::from_str(&String::from_utf8_lossy(&body))));
    }
    let request = Request::new_with_init(&format!("http://container{path}"), &init)
        .map_err(|_| container_unavailable_fault())?;
    // One cancellable deadline spans headers plus the body read, armed
    // before the fetch is issued so slow headers count against the same
    // budget as a stalled body (same shape as `stage_input` above and
    // the ingest pool-stub deadline). `/transcode` runs ffmpeg
    // synchronously under the Container's 300 s budget, so it arms the
    // longer transcode deadline while `/health` and `/cleanup` keep the
    // 10 s base. Dropping the pinned `Delay` releases its timer, and
    // every return below drops it: expiry, transport failure, and
    // success all release the timer.
    let budget_ms = if path == "/transcode" {
        CONTAINER_TRANSCODE_DEADLINE_MS
    } else {
        CONTAINER_FETCH_DEADLINE_MS
    };
    let mut deadline = Box::pin(Delay::from(Duration::from_millis(budget_ms)));
    let mut fetch = Box::pin(port.fetch_request(request));
    let response = std::future::poll_fn(|cx| {
        use std::task::Poll;
        // Deadline first: an already-fired deadline aborts even when
        // headers are also ready.
        if std::future::Future::poll(deadline.as_mut(), cx).is_ready() {
            return Poll::Ready(None);
        }
        match std::future::Future::poll(fetch.as_mut(), cx) {
            Poll::Ready(response) => Poll::Ready(Some(response)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await;
    let mut response = match response {
        Some(Ok(response)) => response,
        Some(Err(_)) | None => return Err(ApiFault::new(502, "container_unavailable")),
    };
    let status = response.status_code();
    // Cap while streaming under the same deadline: the Container-chosen
    // reply to /health, /transcode or /cleanup must abort at
    // MAX_RESULT_BYTES instead of being buffered whole before rejection.
    // `read_response_capped` streams via `push_chunk_capped`
    // (check-before-extend, never retaining the excess) and handles the
    // already-materialised cases check-before-clone; partial progress
    // never re-arms the deadline, so headers plus body share one total
    // budget. A read still incomplete at expiry fails the whole exchange
    // as the existing 502 container_unavailable, and every return below
    // drops the timer.
    let mut read = Box::pin(read_response_capped(&mut response, MAX_RESULT_BYTES));
    let bytes = std::future::poll_fn(|cx| {
        use std::task::Poll;
        if std::future::Future::poll(deadline.as_mut(), cx).is_ready() {
            return Poll::Ready(None);
        }
        match std::future::Future::poll(read.as_mut(), cx) {
            Poll::Ready(bytes) => Poll::Ready(Some(bytes)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await;
    drop(deadline);
    let bytes = match bytes {
        Some(Ok(bytes)) => bytes,
        Some(Err(_)) | None => return Err(ApiFault::new(502, "container_unavailable")),
    };
    Ok((status, bytes))
}

/// Read one small scratch object (result marker) with a size cap
/// checked against R2 metadata before the body is materialised.
async fn read_object_capped(
    bucket: &Bucket,
    key: &str,
    cap: usize,
) -> Result<Option<Vec<u8>>, ApiFault> {
    let object = bucket
        .get(key)
        .execute()
        .await
        .map_err(|_| ApiFault::new(503, "scratch_unavailable"))?;
    let Some(object) = object else {
        return Ok(None);
    };
    if object.size() > cap as u64 {
        return Err(ApiFault::new(413, "payload_too_large"));
    }
    let body = object
        .body()
        .ok_or_else(|| ApiFault::new(503, "scratch_unavailable"))?
        .bytes()
        .await
        .map_err(|_| ApiFault::new(503, "scratch_unavailable"))?;
    Ok(Some(body))
}
