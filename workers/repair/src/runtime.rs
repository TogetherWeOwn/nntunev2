//! Workers runtime (wasm32): the no-route repair Worker and its
//! per-job Container Durable Object.
//!
//! - The Worker's `fetch` exists only for the ingest service binding
//!   (`workers_dev = false`, no routes): it validates the request and
//!   forwards it to the Durable Object named by `job_id`, so one job's
//!   rounds serialise behind one object.
//! - The Durable Object owns the Container for that job. It streams
//!   staged inputs from `INGEST_SCRATCH` into the Container, runs one
//!   repair, streams outputs back, and writes `out/result.json` last
//!   (the idempotency marker). The Container itself holds no
//!   credentials and is started with internet access disabled.

use std::time::Duration;

use futures_util::StreamExt;
use worker::wasm_bindgen::JsValue;
use worker::{
    console_log, durable_object, event, Bucket, ContainerStartupOptions, Context, Delay,
    DurableObject, Env, Fetcher, FixedLengthStream, Headers, Method, Request, RequestInit,
    Response, ResponseBody, Result as WorkerResult, State,
};

use crate::contract::{
    classify_container_reply, container_repair_body, container_unavailable_fault, in_key,
    ingest_reply_body, manifest_digest, manifest_key, out_key, parse_manifest, parse_request, plan,
    push_chunk_capped, repair_unavailable_fault, result_key, validate_repaired_output_set,
    ApiFault, ContainerOutcome, FileRole, Plan, RepairRequest, RepairStatus, MAX_REQUEST_BYTES,
    MAX_RESULT_BYTES,
};

/// Durable Object binding + class fronting the Container.
pub const DO_BINDING: &str = "REPAIR_JOBS";
/// R2 binding for the per-environment ingest scratch bucket.
pub const SCRATCH_BINDING: &str = "INGEST_SCRATCH";
/// The Container server's port (`containers/repair`).
const CONTAINER_PORT: u16 = 8080;
/// Container cold-start budget: poll `/health` every 500 ms.
const HEALTH_ATTEMPTS: u32 = 60;

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
/// rejects non-stream bodies. Callers map the unit error to their own
/// fault.
async fn read_response_capped(response: &mut Response, cap: usize) -> Result<Vec<u8>, ()> {
    if matches!(response.body(), ResponseBody::Empty) {
        return Ok(Vec::new());
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
/// ingest `REPAIR_CONTAINER` service binding.
#[event(fetch)]
async fn fetch(mut req: Request, env: Env, _ctx: Context) -> WorkerResult<Response> {
    if req.method() != Method::Post || req.path() != "/repair" {
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
            console_log!("repair DO unavailable: {:?}", error);
            return fault_response(&repair_unavailable_fault());
        }
    };
    let headers = Headers::new();
    let _ = headers.set("content-type", "application/json");
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(&String::from_utf8_lossy(&body))));
    let request = match Request::new_with_init("https://repair-do/repair", &init) {
        Ok(request) => request,
        Err(_) => return fault_response(&repair_unavailable_fault()),
    };
    let mut response = match stub.fetch_with_request(request).await {
        Ok(response) => response,
        Err(error) => {
            console_log!("repair DO call failed job={}: {:?}", parsed.job_id, error);
            return fault_response(&repair_unavailable_fault());
        }
    };
    let status = response.status_code();
    match read_response_capped(&mut response, MAX_RESULT_BYTES).await {
        Ok(bytes) => json_response(status, bytes),
        Err(()) => fault_response(&ApiFault::new(503, "repair_unavailable")),
    }
}

/// One Container per ingest job. Rounds for the job serialise here.
#[durable_object(fetch)]
pub struct RepairContainer {
    state: State,
    env: Env,
}

impl DurableObject for RepairContainer {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, mut req: Request) -> WorkerResult<Response> {
        if req.method() != Method::Post || req.path() != "/repair" {
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

impl RepairContainer {
    async fn run(&self, req: &RepairRequest) -> Result<Vec<u8>, ApiFault> {
        let bucket = self
            .env
            .bucket(SCRATCH_BINDING)
            .map_err(|_| ApiFault::new(503, "scratch_unavailable"))?;
        let prefix = &req.scratch_prefix;

        // Read the manifest first: idempotency is keyed on it. A
        // stored result only short-circuits when it was produced
        // from exactly this manifest (a redrive reuses the round
        // number with freshly staged inputs).
        let manifest_bytes = read_object_capped(
            &bucket,
            &manifest_key(prefix),
            crate::contract::MAX_MANIFEST_BYTES,
        )
        .await?
        .ok_or_else(|| ApiFault::new(404, "scratch_missing"))?;
        let manifest = parse_manifest(&manifest_bytes)?;
        let digest = manifest_digest(&manifest);
        let existing = read_object_capped(&bucket, &result_key(prefix), MAX_RESULT_BYTES).await?;
        let stale_generation = match plan(req, existing.as_deref(), &digest)? {
            Plan::AlreadyDone(result) => return Ok(ingest_reply_body(&result)),
            Plan::Rerun => true,
            Plan::Run => false,
        };

        let container = self.container_port().await?;
        if stale_generation {
            // Order matters: clean the Container, then delete the
            // stale R2 result, then stage and run. The Container still
            // holds the previous generation's request/result for this
            // (job, round); clear it before staging or it rightly
            // 409s the new manifest. Deleting the stale R2 marker
            // right after means a retry inside this new generation
            // plans as `Run` (no stored result) and the Container
            // replays its own finished result instead of being
            // cleaned again and paid for twice.
            let cleanup =
                serde_json::to_vec(&serde_json::json!({"job_id": req.job_id, "round": req.round}))
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

        // Stage inputs. Data files with size 0 are missing targets the
        // Container recreates from recovery blocks; they have no input.
        for file in &manifest.files {
            if file.role == FileRole::Data && file.size == 0 {
                continue;
            }
            self.stage_input(&bucket, &container, req, file).await?;
        }

        let body = serde_json::to_vec(&container_repair_body(req, &manifest))
            .map_err(|_| ApiFault::new(500, "repair_unavailable"))?;
        let (status, reply) =
            container_json(&container, Method::Post, "/repair", Some(body)).await?;
        let mut result = match classify_container_reply(status, &reply) {
            ContainerOutcome::Done(result) => result,
            ContainerOutcome::Forward(status, code) => {
                return Err(ApiFault::new(status, leak_code(code)));
            }
        };
        // Stamp the inputs this verdict belongs to before it becomes
        // the idempotency marker in `out/result.json`.
        result.manifest_digest = digest;

        if result.status == RepairStatus::Repaired {
            validate_repaired_output_set(&manifest, &result)?;
            for output in &result.files {
                self.fetch_output(&bucket, &container, req, output).await?;
            }
        }

        // The result marker lands only after every output is durable.
        // The full document stays in scratch; ingest gets the small
        // status reply (see `ingest_reply_body`).
        let result_body =
            serde_json::to_vec(&result).map_err(|_| ApiFault::new(500, "repair_unavailable"))?;
        bucket
            .put(result_key(prefix), result_body)
            .execute()
            .await
            .map_err(|_| ApiFault::new(503, "scratch_unavailable"))?;
        let reply_body = ingest_reply_body(&result);

        // Free the Container's disk for the next round; failure here is
        // not fatal (the Container is per-job and ephemeral).
        let cleanup =
            serde_json::to_vec(&serde_json::json!({"job_id": req.job_id, "round": req.round}))
                .unwrap_or_default();
        if container_json(&container, Method::Post, "/cleanup", Some(cleanup))
            .await
            .is_err()
        {
            console_log!("repair cleanup failed job={}", req.job_id);
        }
        console_log!(
            "repair done job={} round={} status={}",
            req.job_id,
            req.round,
            result.status.as_str()
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

    /// Stream one staged R2 object into the Container. The body rides
    /// as the object's own readable stream with an explicit length;
    /// the Container accepts fixed-length and chunked bodies.
    async fn stage_input(
        &self,
        bucket: &Bucket,
        container: &Fetcher,
        req: &RepairRequest,
        file: &crate::contract::ScratchFile,
    ) -> Result<(), ApiFault> {
        let object = bucket
            .get(in_key(&req.scratch_prefix, &file.key))
            .execute()
            .await
            .map_err(|_| ApiFault::new(503, "scratch_unavailable"))?
            .ok_or_else(|| ApiFault::new(404, "scratch_missing"))?;
        if object.size() != file.size {
            return Err(ApiFault::new(400, "scratch_input_mismatch"));
        }
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
        let _ = headers.set("content-length", &file.size.to_string());
        let mut init = RequestInit::new();
        init.with_method(Method::Put)
            .with_headers(headers)
            .with_body(Some(JsValue::from(stream)));
        let url = format!(
            "http://container/jobs/{}/r{}/in/{}",
            req.job_id, req.round, file.key
        );
        let request = Request::new_with_init(&url, &init)
            .map_err(|_| ApiFault::new(502, "container_unavailable"))?;
        let response = container
            .fetch_request(request)
            .await
            .map_err(|_| ApiFault::new(502, "container_unavailable"))?;
        let status = response.status_code();
        // The PUT reply carries no verdict beyond its status: cancel the
        // body instead of collecting bytes that are never read, so a
        // misbehaving container cannot make us buffer an unbounded reply.
        if let ResponseBody::Stream(stream) = response.body() {
            let _ = stream.cancel();
        }
        match status {
            201 => Ok(()),
            400 | 413 => Err(ApiFault::new(status, "scratch_input_mismatch")),
            _ => Err(ApiFault::new(502, "container_unavailable")),
        }
    }

    /// Stream one repaired output from the Container into R2 under a
    /// fixed-length stream, so a size lie fails the put instead of
    /// persisting a truncated file.
    async fn fetch_output(
        &self,
        bucket: &Bucket,
        container: &Fetcher,
        req: &RepairRequest,
        output: &crate::contract::OutputFile,
    ) -> Result<(), ApiFault> {
        let url = format!(
            "http://container/jobs/{}/r{}/out/{}",
            req.job_id, req.round, output.key
        );
        let mut response = container
            .fetch(url, None)
            .await
            .map_err(|_| ApiFault::new(502, "container_unavailable"))?;
        if response.status_code() != 200 {
            return Err(ApiFault::new(502, "container_unavailable"));
        }
        let stream = response
            .stream()
            .map_err(|_| ApiFault::new(502, "container_unavailable"))?;
        bucket
            .put(
                out_key(&req.scratch_prefix, &output.key),
                FixedLengthStream::wrap(stream, output.size),
            )
            .execute()
            .await
            .map_err(|_| ApiFault::new(503, "scratch_unavailable"))?;
        Ok(())
    }
}

/// Codes forwarded from the Container are validated lowercase
/// identifiers (see `classify_container_reply`); leaking them into the
/// `'static` fault keeps the reply allocation-free of owned strings.
/// The set is closed and tiny.
fn leak_code(code: String) -> &'static str {
    match code.as_str() {
        "invalid_request" => "invalid_request",
        "invalid_manifest" => "invalid_manifest",
        "not_found" => "not_found",
        "payload_too_large" => "payload_too_large",
        "repair_manifest_conflict" => "repair_manifest_conflict",
        "repair_timeout" => "repair_timeout",
        "repair_resource" => "repair_resource",
        "repair_failed" => "repair_failed",
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
    let mut response = port
        .fetch_request(request)
        .await
        .map_err(|_| container_unavailable_fault())?;
    let status = response.status_code();
    let bytes = read_response_capped(&mut response, MAX_RESULT_BYTES)
        .await
        .map_err(|_| ApiFault::new(502, "container_unavailable"))?;
    Ok((status, bytes))
}

/// Read one small scratch object (manifest/result) with a size cap
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
