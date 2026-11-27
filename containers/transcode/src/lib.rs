//! Native HTTP server for the `opus-64` transcode Container.
//!
//! The Container holds no vault, NNTP or R2 credentials and has no
//! outbound network: the transcode Worker stages one verified audio
//! object in (`PUT /jobs/{job}/t/{staging}/in/{key}`), asks for one
//! bounded encode (`POST /transcode`, executed with the pinned argv
//! from [`nntune_core::transcode::ffmpeg_argv`] against the
//! image-pinned `ffmpeg` binary, then normalised with
//! [`nntune_core::transcode::normalize_ogg_serial`] so the bytes are
//! identical whichever path produced them), and streams the artifact
//! back (`GET /jobs/{job}/t/{staging}/out/artifact.ogg`). A repeated
//! `(job_id, staging_id)` request returns the stored result without
//! running `ffmpeg` again; the same input under a new `staging_id`
//! is a new generation and reruns on the re-staged bytes; a different
//! input for the same staging is a conflict (the Worker cleans the
//! staging first when it means to replace it). Errors are stable
//! machine-readable codes; logs carry job ids and counts only, never
//! file names.
//!
//! Shape mirrors the repair Container server
//! (`containers/repair/src/lib.rs`): std-only HTTP/1.1, one job at a
//! time behind a gate, atomic result writes. Differences are the
//! transcode domain: single-input requests namespaced by content
//! `staging_id` instead of multi-file repair rounds, an in-process
//! wall-clock encode budget instead of the par2 resource runner
//! (inputs are verified-intact audio under
//! [`MAX_TRANSCODE_INPUT_BYTES`], not hostile damage, so a wall
//! timeout plus the instance memory bound the cost), and
//! content-hash verification of the staged input before every run.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use nntune_core::par2::scratch::{sha256_hex, validate_staging_id};
use nntune_core::transcode::{
    canonical_sha256_hex, ffmpeg_argv, normalize_ogg_serial, validate_transcode_input_key,
    OGG_FIXED_SERIAL,
};
use serde::{Deserialize, Serialize};

/// Port the Container listens on (the transcode Worker fetches it
/// through the Durable Object's TCP port handle).
pub const SERVER_PORT: u16 = 8080;
/// Hard cap on a JSON request body.
const MAX_JSON_BYTES: u64 = 1 << 20;
/// Hard cap on the HTTP header block.
const MAX_HEADER_BYTES: usize = 16 << 10;
/// Default encode wall-clock budget. Opus encodes far faster than
/// realtime; the budget covers a slow Container start and a long
/// track, not a slow codec.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Maximum encode wall-clock budget a request may ask for.
const MAX_TIMEOUT_SECS: u64 = 600;
/// Largest staged input: verify assembles single objects up to 32
/// MiB, so anything larger is not this pipeline's verified audio and
/// is refused rather than encoded.
const MAX_TRANSCODE_INPUT_BYTES: u64 = 64 << 20;
/// Aggregate staged bytes per staging (one input plus its artifact):
/// the PUT stream is refused past what is already staged plus this.
const MAX_TRANSCODE_TOTAL_BYTES: u64 = 128 << 20;
/// Largest publishable artifact: 64 kbps over a 60-minute track is
/// ~28.8 MB plus Ogg framing. An encode emitting more is not this
/// profile's output and is untranscodable, never published.
const MAX_ARTIFACT_BYTES: u64 = 32 << 20;
/// Fixed output object name under `out/`.
const ARTIFACT_NAME: &str = "artifact.ogg";
/// Encode-process poll interval while waiting out the wall budget.
const ENCODE_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Bound teardown of requests rejected before their bodies were consumed.
const CLOSE_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_CLOSE_DRAIN_BYTES: usize = 1 << 20;

/// Server configuration: where staged bytes live and which pinned
/// binary encodes them. Both come from the image, never from requests.
pub struct ServerConfig {
    pub scratch_root: PathBuf,
    pub ffmpeg_bin: PathBuf,
}

/// `POST /transcode` body: one verified audio object to encode.
/// `input_key` is the verify-staged object name the Worker PUT first;
/// `source_sha256` is the verified bytes' content hash, re-checked
/// against the staged bytes before every run so a redrive that
/// re-staged different bytes under the same names cannot replay the
/// previous generation's artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeJobRequest {
    pub job_id: String,
    /// Content-derived token for this staging: idempotency binds to
    /// it, so the same input re-staged with different bytes (new id)
    /// reruns instead of replaying the old staging's result.
    pub staging_id: String,
    pub input_key: String,
    pub source_sha256: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Terminal transcode outcome, also stored as `result.json`.
/// `artifact_sha256`/`byte_size` are `Some` exactly when `status` is
/// `Transcoded`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeJobResult {
    pub status: TranscodeStatus,
    pub exit_code: Option<i32>,
    pub artifact_sha256: Option<String>,
    pub byte_size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscodeStatus {
    Transcoded,
    Untranscodable,
}

/// Stable API error: HTTP status plus a machine-readable code. Response
/// bodies are `{"error": code}`; no paths or file names leak.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
}

impl ApiError {
    fn new(status: u16, code: &'static str) -> Self {
        Self { status, code }
    }
    fn invalid() -> Self {
        Self::new(400, "invalid_request")
    }
    fn not_found() -> Self {
        Self::new(404, "not_found")
    }
    fn request_body(err: io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => Self::invalid(),
            _ => Self::from(err),
        }
    }
}

impl From<io::Error> for ApiError {
    fn from(_: io::Error) -> Self {
        Self::new(500, "transcode_unavailable")
    }
}

/// Job-id alphabet: the pipeline shape (same rule the ingest job
/// machine accepts) — non-empty, at most 64 chars, URL/key-safe.
/// Duplicated from the core scratch contract, whose validator is
/// private to it; the staging-id and input-key rules are shared
/// instead of duplicated.
fn validate_job_id(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Validate a transcode request. Every field is untrusted
/// pipeline-derived metadata until it passes here.
pub fn validate_request(req: &TranscodeJobRequest) -> Result<(), ApiError> {
    if !validate_job_id(&req.job_id) {
        return Err(ApiError::invalid());
    }
    if !validate_staging_id(&req.staging_id) {
        return Err(ApiError::invalid());
    }
    if !validate_transcode_input_key(&req.input_key) {
        return Err(ApiError::invalid());
    }
    if canonical_sha256_hex(&req.source_sha256).is_err() {
        return Err(ApiError::invalid());
    }
    if let Some(timeout) = req.timeout_secs {
        if timeout == 0 || timeout > MAX_TIMEOUT_SECS {
            return Err(ApiError::invalid());
        }
    }
    Ok(())
}

fn staging_dir(config: &ServerConfig, job_id: &str, staging_id: &str) -> PathBuf {
    config.scratch_root.join(job_id).join("t").join(staging_id)
}

/// Run `ffmpeg_bin` with `argv`, killing it past `timeout`. A process
/// that exits by itself reports its status; a kill reports a timeout.
/// Stderr is discarded (`-v error` keeps it diagnostic-only, and
/// captured diagnostics would risk echoing staged paths into logs).
fn run_bounded(ffmpeg_bin: &Path, argv: &[String], timeout: Duration) -> Result<ExitStatus, ()> {
    let mut child = Command::new(ffmpeg_bin)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().map_err(|_| ())? {
            Some(status) => return Ok(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
            None => std::thread::sleep(ENCODE_POLL_INTERVAL),
        }
    }
}

/// Execute one transcode request against the already-staged input.
/// Terminal results are persisted under the staging directory so a
/// repeated request is idempotent; transient failures (timeout,
/// resource ceilings) persist nothing and may be retried.
pub fn run_transcode(
    config: &ServerConfig,
    req: &TranscodeJobRequest,
) -> Result<TranscodeJobResult, ApiError> {
    validate_request(req)?;
    let dir = staging_dir(config, &req.job_id, &req.staging_id);
    let request_path = dir.join("request.json");
    let result_path = dir.join("result.json");

    if let Ok(stored) = fs::read(&request_path) {
        let stored_req: TranscodeJobRequest = serde_json::from_slice(&stored)
            .map_err(|_| ApiError::new(500, "transcode_unavailable"))?;
        if stored_req == *req {
            if let Ok(stored_result) = fs::read(&result_path) {
                let result: TranscodeJobResult = serde_json::from_slice(&stored_result)
                    .map_err(|_| ApiError::new(500, "transcode_unavailable"))?;
                return Ok(result);
            }
            // Same staging, no terminal result yet: fall through and
            // (re)run on the re-staged input.
        } else if stored_req.input_key != req.input_key
            || stored_req.source_sha256 != req.source_sha256
        {
            // A different input for the same staging is a conflict:
            // replacing the input goes through the Worker's explicit
            // cleanup. (A new staging id is a new directory, so it
            // never reaches this branch.)
            return Err(ApiError::new(409, "transcode_input_conflict"));
        } else {
            // Same input, only the timeout differs: re-run; the old
            // staging's verdict and outputs must not replay for a
            // request that changed the budget.
            let _ = fs::remove_file(&result_path);
            let _ = fs::remove_dir_all(dir.join("out"));
            write_atomic(&request_path, &serde_json::to_vec(req).unwrap())?;
        }
    } else {
        fs::create_dir_all(dir.join("in"))?;
        write_atomic(&request_path, &serde_json::to_vec(req).unwrap())?;
    }

    // The staged input must exist at the declared size and hash: the
    // Worker re-stages from R2 before each call, and a redrive that
    // re-staged different bytes under the same names must fail here,
    // never encode (or replay) the wrong bytes.
    let staged = dir.join("in").join(&req.input_key);
    let input_len = fs::metadata(&staged)
        .map_err(|_| ApiError::new(400, "scratch_input_mismatch"))?
        .len();
    if input_len == 0 || input_len > MAX_TRANSCODE_INPUT_BYTES {
        return Err(ApiError::new(400, "scratch_input_mismatch"));
    }
    let input_bytes = fs::read(&staged)?;
    if sha256_hex(&input_bytes) != canonical_sha256_hex(&req.source_sha256).unwrap() {
        return Err(ApiError::new(400, "scratch_input_mismatch"));
    }

    let timeout = Duration::from_secs(req.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));
    let raw_out = dir.join("encode.ogg");
    let _ = fs::remove_file(&raw_out);
    let argv = ffmpeg_argv(
        staged.to_str().ok_or_else(ApiError::invalid)?,
        raw_out.to_str().ok_or_else(ApiError::invalid)?,
    );
    let status = match run_bounded(&config.ffmpeg_bin, &argv, timeout) {
        Ok(status) => status,
        Err(()) => {
            let _ = fs::remove_file(&raw_out);
            return Err(ApiError::new(503, "transcode_timeout"));
        }
    };
    let exit_code = status.code();
    // A signal kill (OOM, limit) is retryable: nothing is persisted.
    if exit_code.is_none() {
        let _ = fs::remove_file(&raw_out);
        return Err(ApiError::new(503, "transcode_resource"));
    }
    let untranscodable = |exit_code: Option<i32>| TranscodeJobResult {
        status: TranscodeStatus::Untranscodable,
        exit_code,
        artifact_sha256: None,
        byte_size: None,
    };
    if !status.success() {
        // Deterministic for these bytes (same input encodes
        // identically), so a nonzero exit is terminal for the file,
        // never a retry.
        let _ = fs::remove_file(&raw_out);
        let result = untranscodable(exit_code);
        write_atomic(&result_path, &serde_json::to_vec(&result).unwrap())?;
        return Ok(result);
    }
    let raw_len = fs::metadata(&raw_out)
        .map_err(|_| ApiError::new(500, "transcode_output_missing"))?
        .len();
    if raw_len == 0 || raw_len > MAX_ARTIFACT_BYTES {
        let _ = fs::remove_file(&raw_out);
        let result = untranscodable(exit_code);
        write_atomic(&result_path, &serde_json::to_vec(&result).unwrap())?;
        return Ok(result);
    }
    let raw = fs::read(&raw_out)?;
    let _ = fs::remove_file(&raw_out);
    let artifact = match normalize_ogg_serial(&raw, OGG_FIXED_SERIAL) {
        Ok(artifact) => artifact,
        Err(_) => {
            let result = untranscodable(exit_code);
            write_atomic(&result_path, &serde_json::to_vec(&result).unwrap())?;
            return Ok(result);
        }
    };
    let artifact_sha256 = sha256_hex(&artifact);
    let byte_size = artifact.len() as u64;
    let out_dir = dir.join("out");
    fs::create_dir_all(&out_dir)?;
    write_atomic(&out_dir.join(ARTIFACT_NAME), &artifact)?;
    let result = TranscodeJobResult {
        status: TranscodeStatus::Transcoded,
        exit_code,
        artifact_sha256: Some(artifact_sha256),
        byte_size: Some(byte_size),
    };
    write_atomic(&result_path, &serde_json::to_vec(&result).unwrap())?;
    eprintln!(
        "transcode job={} status={:?} input_bytes={} artifact_bytes={}",
        req.job_id, result.status, input_len, byte_size
    );
    Ok(result)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ApiError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Stage one input object from a request body reader. The aggregate
/// cap is enforced against what is already staged for the staging.
fn store_input(
    config: &ServerConfig,
    job_id: &str,
    staging_id: &str,
    key: &str,
    body: &mut impl Read,
    declared_len: Option<u64>,
) -> Result<u64, ApiError> {
    if !validate_job_id(job_id)
        || !validate_staging_id(staging_id)
        || !validate_transcode_input_key(key)
    {
        return Err(ApiError::invalid());
    }
    if let Some(len) = declared_len {
        if len > MAX_TRANSCODE_INPUT_BYTES {
            return Err(ApiError::new(413, "payload_too_large"));
        }
    }
    let dir = staging_dir(config, job_id, staging_id).join("in");
    fs::create_dir_all(&dir)?;
    let mut staged_total: u64 = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy() != key {
            staged_total = staged_total
                .checked_add(entry.metadata()?.len())
                .ok_or_else(|| ApiError::new(413, "payload_too_large"))?;
        }
    }
    let tmp = dir.join(format!("{key}.tmp"));
    let mut file = fs::File::create(&tmp)?;
    let mut written: u64 = 0;
    let mut buf = [0u8; 64 << 10];
    loop {
        let n = body.read(&mut buf).map_err(|err| {
            let _ = fs::remove_file(&tmp);
            ApiError::request_body(err)
        })?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > MAX_TRANSCODE_INPUT_BYTES || staged_total + written > MAX_TRANSCODE_TOTAL_BYTES
        {
            let _ = fs::remove_file(&tmp);
            return Err(ApiError::new(413, "payload_too_large"));
        }
        file.write_all(&buf[..n])?;
    }
    if let Some(len) = declared_len {
        if written != len {
            let _ = fs::remove_file(&tmp);
            return Err(ApiError::invalid());
        }
    }
    fs::rename(&tmp, dir.join(key))?;
    Ok(written)
}

// --- HTTP/1.1 plumbing (std only; one job at a time per Container) ---

struct Request {
    method: String,
    path: String,
    content_length: Option<u64>,
    chunked: bool,
}

fn read_head(stream: &mut impl Read) -> Result<Request, ApiError> {
    let mut budget = MAX_HEADER_BYTES;
    let mut line = Vec::new();
    read_crlf_line(stream, &mut line, &mut budget).map_err(ApiError::request_body)?;
    let first = std::str::from_utf8(&line).map_err(|_| ApiError::invalid())?;
    let mut parts = first.split(' ');
    let (Some(method), Some(path), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ApiError::invalid());
    };
    if !is_token(method)
        || !path.starts_with('/')
        || !path.bytes().all(|b| b.is_ascii_graphic())
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
    {
        return Err(ApiError::invalid());
    }
    let (method, path, http_11) = (method.to_string(), path.to_string(), version == "HTTP/1.1");
    let mut content_length = None;
    let mut chunked = false;
    loop {
        read_crlf_line(stream, &mut line, &mut budget).map_err(ApiError::request_body)?;
        if line.is_empty() {
            break;
        }
        let field = String::from_utf8_lossy(&line);
        let (name, value) = split_field(&field).ok_or_else(ApiError::invalid)?;
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ApiError::invalid());
            }
            content_length = Some(value.parse().map_err(|_| ApiError::invalid())?);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            if !http_11 || chunked || !value.eq_ignore_ascii_case("chunked") {
                return Err(ApiError::invalid());
            }
            chunked = true;
        }
    }
    if chunked && content_length.is_some() {
        return Err(ApiError::invalid());
    }
    Ok(Request {
        method,
        path,
        content_length,
        chunked,
    })
}

/// Body reader over a socket: fixed-length or chunked, always bounded
/// by the caller through `limit`.
struct BodyReader<'a> {
    stream: &'a mut dyn Read,
    mode: BodyMode,
    remaining: u64,
    chunk_left: u64,
    done: bool,
}

enum BodyMode {
    Fixed,
    Chunked,
}

impl<'a> BodyReader<'a> {
    fn new(stream: &'a mut dyn Read, req: &Request) -> Result<Self, ApiError> {
        if req.chunked {
            Ok(Self {
                stream,
                mode: BodyMode::Chunked,
                remaining: 0,
                chunk_left: 0,
                done: false,
            })
        } else if let Some(len) = req.content_length {
            Ok(Self {
                stream,
                mode: BodyMode::Fixed,
                remaining: len,
                chunk_left: 0,
                done: false,
            })
        } else {
            Err(ApiError::new(411, "length_required"))
        }
    }

    fn read_chunk_header(&mut self) -> io::Result<u64> {
        let mut line = Vec::new();
        let mut budget = 32;
        read_crlf_line(self.stream, &mut line, &mut budget)?;
        let text = String::from_utf8_lossy(&line);
        let size = text.split_once(';').map_or(text.as_ref(), |(size, _)| {
            size.trim_end_matches([' ', '\t'])
        });
        if !size.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid_framing());
        }
        u64::from_str_radix(size, 16).map_err(|_| invalid_framing())
    }
}

impl Read for BodyReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        match self.mode {
            BodyMode::Fixed => {
                if self.remaining == 0 {
                    return Ok(0);
                }
                let take = (out.len() as u64).min(self.remaining) as usize;
                let n = self.stream.read(&mut out[..take])?;
                if n == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "fixed body"));
                }
                self.remaining -= n as u64;
                Ok(n)
            }
            BodyMode::Chunked => {
                if self.done {
                    return Ok(0);
                }
                if self.chunk_left == 0 {
                    let size = self.read_chunk_header()?;
                    if size == 0 {
                        read_chunk_trailers(self.stream)?;
                        self.done = true;
                        return Ok(0);
                    }
                    self.chunk_left = size;
                }
                let take = (out.len() as u64).min(self.chunk_left) as usize;
                let n = self.stream.read(&mut out[..take])?;
                if n == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "chunk data"));
                }
                self.chunk_left -= n as u64;
                if self.chunk_left == 0 {
                    let mut crlf = [0u8; 2];
                    self.stream.read_exact(&mut crlf)?;
                    if crlf != *b"\r\n" {
                        return Err(invalid_framing());
                    }
                }
                Ok(n)
            }
        }
    }
}

fn read_chunk_trailers(stream: &mut dyn Read) -> io::Result<()> {
    let mut budget = MAX_HEADER_BYTES;
    let mut line = Vec::new();
    loop {
        read_crlf_line(stream, &mut line, &mut budget)?;
        if line.is_empty() {
            return Ok(());
        }
        let field = String::from_utf8_lossy(&line);
        let (name, _) = split_field(&field).ok_or_else(invalid_framing)?;
        if name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding")
        {
            return Err(invalid_framing());
        }
    }
}

fn read_json_body(stream: &mut dyn Read, req: &Request) -> Result<Vec<u8>, ApiError> {
    if let Some(len) = req.content_length {
        if len > MAX_JSON_BYTES {
            return Err(ApiError::new(413, "payload_too_large"));
        }
    }
    let mut reader = BodyReader::new(stream, req)?;
    let mut body = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).map_err(ApiError::request_body)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() as u64 > MAX_JSON_BYTES {
            return Err(ApiError::new(413, "payload_too_large"));
        }
    }
    Ok(body)
}

fn respond_json(stream: &mut impl Write, status: u16, body: &[u8]) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "OK",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn respond_error(stream: &mut impl Write, err: &ApiError) -> io::Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({"error": err.code})).unwrap();
    respond_json(stream, err.status, &body)
}

/// Parse `/jobs/{job}/t/{staging}/{in|out}/{key}` path segments. The
/// `t/{staging}` segment namespaces one encode generation: the same
/// input re-staged under a new content id is a new directory and can
/// never replay the old staging's result or conflict with it.
fn parse_job_path(path: &str) -> Option<(String, String, &'static str, String)> {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.len() != 6 || parts[0] != "jobs" || parts[2] != "t" {
        return None;
    }
    let lane = match parts[4] {
        "in" => "in",
        "out" => "out",
        _ => return None,
    };
    Some((
        parts[1].to_string(),
        parts[3].to_string(),
        lane,
        parts[5].to_string(),
    ))
}

fn close_connection(stream: &mut TcpStream) {
    // Deliver response EOF before draining. Waiting for a rejected request's
    // declared body first would deadlock clients already waiting for the reply.
    if stream.shutdown(Shutdown::Write).is_err() {
        return;
    }
    let deadline = Instant::now() + CLOSE_DRAIN_TIMEOUT;
    let mut remaining = MAX_CLOSE_DRAIN_BYTES;
    let mut buf = [0u8; 8192];
    while remaining > 0 {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() || stream.set_read_timeout(Some(timeout)).is_err() {
            break;
        }
        let take = remaining.min(buf.len());
        match stream.read(&mut buf[..take]) {
            Ok(0) => break,
            Ok(n) => remaining -= n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

fn handle(stream: &mut TcpStream, config: &ServerConfig, gate: &RwLock<()>) {
    let result = (|| -> Result<(), ApiError> {
        let req = read_head(stream)?;
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/health") => {
                respond_json(stream, 200, br#"{"ok":true}"#)?;
            }
            ("POST", "/transcode") => {
                let body = read_json_body(stream, &req)?;
                let job_req: TranscodeJobRequest =
                    serde_json::from_slice(&body).map_err(|_| ApiError::invalid())?;
                let _guard = gate.write().unwrap_or_else(|e| e.into_inner());
                let result = run_transcode(config, &job_req)?;
                respond_json(stream, 200, &serde_json::to_vec(&result).unwrap())?;
            }
            ("POST", "/cleanup") => {
                let body = read_json_body(stream, &req)?;
                #[derive(Deserialize)]
                struct Cleanup {
                    job_id: String,
                    staging_id: String,
                }
                let cleanup: Cleanup =
                    serde_json::from_slice(&body).map_err(|_| ApiError::invalid())?;
                if !validate_job_id(&cleanup.job_id) || !validate_staging_id(&cleanup.staging_id) {
                    return Err(ApiError::invalid());
                }
                let _guard = gate.write().unwrap_or_else(|e| e.into_inner());
                let dir = staging_dir(config, &cleanup.job_id, &cleanup.staging_id);
                if let Err(err) = fs::remove_dir_all(&dir) {
                    if err.kind() != io::ErrorKind::NotFound {
                        eprintln!("transcode cleanup failed job={}", cleanup.job_id);
                        return Err(err.into());
                    }
                }
                // Remove the now-empty parents too (best effort): the
                // `t/` lane dir, then the per-job dir.
                if let Some(lane) = dir.parent() {
                    let _ = fs::remove_dir(lane);
                    if let Some(job) = lane.parent() {
                        let _ = fs::remove_dir(job);
                    }
                }
                respond_json(stream, 200, br#"{"ok":true}"#)?;
            }
            ("PUT", path) => {
                let (job, staging, lane, key) =
                    parse_job_path(path).ok_or_else(ApiError::not_found)?;
                if lane != "in" {
                    return Err(ApiError::not_found());
                }
                let _guard = gate.write().unwrap_or_else(|e| e.into_inner());
                let size = {
                    let mut reader = BodyReader::new(&mut *stream, &req)?;
                    store_input(
                        config,
                        &job,
                        &staging,
                        &key,
                        &mut reader,
                        req.content_length,
                    )?
                };
                let body =
                    serde_json::to_vec(&serde_json::json!({"key": key, "size": size})).unwrap();
                respond_json(stream, 201, &body)?;
            }
            ("GET", path) => {
                let (job, staging, lane, key) =
                    parse_job_path(path).ok_or_else(ApiError::not_found)?;
                if lane != "out" || key != ARTIFACT_NAME {
                    return Err(ApiError::not_found());
                }
                if !validate_job_id(&job) || !validate_staging_id(&staging) {
                    return Err(ApiError::not_found());
                }
                let _guard = gate.read().unwrap_or_else(|e| e.into_inner());
                let file_path = staging_dir(config, &job, &staging)
                    .join("out")
                    .join(ARTIFACT_NAME);
                let mut file = fs::File::open(&file_path).map_err(|_| ApiError::not_found())?;
                let len = file.metadata()?.len();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
                )?;
                io::copy(&mut file, stream)?;
            }
            _ => return Err(ApiError::not_found()),
        }
        Ok(())
    })();
    if let Err(err) = result {
        let _ = respond_error(stream, &err);
    }
    // Route guards have dropped, so teardown never holds the mutation gate.
    close_connection(stream);
}

fn invalid_framing() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP framing")
}

/// Read one line without consuming body bytes. The shared budget includes CRLF.
fn read_crlf_line(stream: &mut dyn Read, line: &mut Vec<u8>, budget: &mut usize) -> io::Result<()> {
    line.clear();
    let mut byte = [0u8; 1];
    for _ in 0..*budget {
        stream.read_exact(&mut byte)?;
        match byte[0] {
            b'\r' if line.len() + 2 <= *budget => {
                stream.read_exact(&mut byte)?;
                if byte[0] != b'\n' {
                    return Err(invalid_framing());
                }
                *budget -= line.len() + 2;
                return Ok(());
            }
            b'\t' | b' '..=b'~' | 0x80..=0xff => line.push(byte[0]),
            _ => return Err(invalid_framing()),
        }
    }
    Err(invalid_framing())
}

fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn split_field(line: &str) -> Option<(&str, &str)> {
    let (name, value) = line.split_once(':')?;
    is_token(name).then(|| (name, value.trim_matches([' ', '\t'])))
}

/// Serve until the listener fails. Connections run on their own
/// threads; mutating routes serialise behind one gate because a
/// Container serves exactly one job at a time.
pub fn serve(listener: TcpListener, config: Arc<ServerConfig>) -> io::Result<()> {
    fs::create_dir_all(&config.scratch_root)?;
    let gate = Arc::new(RwLock::new(()));
    for stream in listener.incoming() {
        let mut stream = stream?;
        let config = Arc::clone(&config);
        let gate = Arc::clone(&gate);
        std::thread::spawn(move || handle(&mut stream, &config, &gate));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX64: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";

    fn valid_request() -> TranscodeJobRequest {
        TranscodeJobRequest {
            job_id: "job_0123456789abcdef".to_string(),
            staging_id: HEX64.to_string(),
            input_key: format!("0007-{HEX64}"),
            source_sha256: HEX64.to_string(),
            timeout_secs: None,
        }
    }

    #[test]
    fn empty_body_reader_buffer_returns_zero_without_reading_the_socket() {
        struct NoRead;
        impl Read for NoRead {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("an empty output buffer must not read the socket");
            }
        }
        for (mode, remaining, chunk_left, done) in [
            (BodyMode::Fixed, 5, 0, false),
            (BodyMode::Fixed, 0, 0, false),
            (BodyMode::Chunked, 0, 0, false),
            (BodyMode::Chunked, 0, 5, false),
            (BodyMode::Chunked, 0, 0, true),
        ] {
            let mut socket = NoRead;
            let mut reader = BodyReader {
                stream: &mut socket,
                mode,
                remaining,
                chunk_left,
                done,
            };
            assert_eq!(reader.read(&mut []).unwrap(), 0);
            assert_eq!(reader.remaining, remaining);
            assert_eq!(reader.chunk_left, chunk_left);
            assert_eq!(reader.done, done);
        }
    }

    #[test]
    fn incomplete_body_reader_returns_unexpected_eof_not_clean_eof() {
        for (mode, body) in [
            (BodyMode::Fixed, b"".as_slice()),
            (BodyMode::Fixed, b"he"),
            (BodyMode::Chunked, b"5\r\n"),
            (BodyMode::Chunked, b"5\r\nhe"),
        ] {
            let mut socket = io::Cursor::new(body);
            let mut reader = BodyReader {
                stream: &mut socket,
                mode,
                remaining: 5,
                chunk_left: 0,
                done: false,
            };
            let mut bytes = Vec::new();
            let error = reader
                .read_to_end(&mut bytes)
                .expect_err("body advertised five bytes");
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        }
    }

    #[test]
    fn valid_request_passes() {
        assert!(validate_request(&valid_request()).is_ok());
    }

    #[test]
    fn request_validation_rejects_bad_fields() {
        let mut req = valid_request();
        req.job_id = "../evil".to_string();
        assert!(validate_request(&req).is_err());
        let mut req = valid_request();
        req.job_id = String::new();
        assert!(validate_request(&req).is_err());
        let mut req = valid_request();
        req.staging_id = "short".to_string();
        assert!(validate_request(&req).is_err());
        let mut req = valid_request();
        req.input_key = HEX64.to_string();
        assert!(validate_request(&req).is_err());
        let mut req = valid_request();
        req.input_key = format!("0007-{HEX64}/");
        assert!(validate_request(&req).is_err());
        let mut req = valid_request();
        req.source_sha256 = "xyz".to_string();
        assert!(validate_request(&req).is_err());
        let mut req = valid_request();
        req.timeout_secs = Some(0);
        assert!(validate_request(&req).is_err());
        let mut req = valid_request();
        req.timeout_secs = Some(MAX_TIMEOUT_SECS + 1);
        assert!(validate_request(&req).is_err());
        let mut req = valid_request();
        req.timeout_secs = Some(MAX_TIMEOUT_SECS);
        assert!(validate_request(&req).is_ok());
    }

    #[test]
    fn job_path_parsing_pins_segments() {
        assert_eq!(
            parse_job_path("/jobs/job_01/t/staging_09/in/0007-abc"),
            Some((
                "job_01".to_string(),
                "staging_09".to_string(),
                "in",
                "0007-abc".to_string()
            ))
        );
        assert_eq!(
            parse_job_path("/jobs/job_01/t/staging_09/out/artifact.ogg"),
            Some((
                "job_01".to_string(),
                "staging_09".to_string(),
                "out",
                "artifact.ogg".to_string()
            ))
        );
        // Repair-shaped paths, missing segments and unknown lanes fail.
        assert_eq!(parse_job_path("/jobs/job_01/r1/in/abc"), None);
        assert_eq!(parse_job_path("/jobs/job_01/t/staging_09/in"), None);
        assert_eq!(parse_job_path("/jobs/job_01/t/staging_09/tmp/abc"), None);
        assert_eq!(parse_job_path("/transcode"), None);
    }
}
