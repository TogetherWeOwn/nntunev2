//! Native HTTP server for the par2 repair Container.
//!
//! The Container holds no vault, NNTP or R2 credentials and has no
//! outbound network: the repair Worker streams staged scratch objects in
//! (`PUT /jobs/{job}/r{round}/in/{key}`), asks for one bounded repair
//! (`POST /repair`, executed by
//! [`nntune_core::par2::repair::repair_with_limits`] against the
//! image-pinned `par2` binary), and streams outputs back
//! (`GET /jobs/{job}/r{round}/out/{key}`). A repeated `(job_id, round)`
//! request with the same manifest returns the stored result without
//! running `par2` again; a different manifest for the same round is a
//! conflict. Errors are stable machine-readable codes; logs carry job
//! ids and counts only, never file names.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use nntune_core::error::Error as CoreError;
use nntune_core::par2::repair::{repair_with_limits, RepairLimits};
use nntune_core::par2::scratch::{
    object_key_for_file_name, validate_file_name, validate_object_key, MAX_SCRATCH_FILES,
    MAX_SCRATCH_TOTAL_BYTES,
};
use serde::{Deserialize, Serialize};

/// Port the Container listens on (the repair Worker fetches it through
/// the Durable Object's TCP port handle).
pub const SERVER_PORT: u16 = 8080;
/// Hard cap on a JSON request body.
const MAX_JSON_BYTES: u64 = 1 << 20;
/// Hard cap on the HTTP header block.
const MAX_HEADER_BYTES: usize = 16 << 10;
/// Default and maximum repair wall-clock budget (the core ceiling).
const MAX_TIMEOUT_SECS: u64 = 120;

/// Server configuration: where staged bytes live and which pinned
/// binary repairs them. Both come from the image, never from requests.
pub struct ServerConfig {
    pub scratch_root: PathBuf,
    pub par2_bin: PathBuf,
}

/// One staged file in a repair manifest. `key` must equal
/// `sha256(file_name)` (the scratch object key); `size` is the staged
/// byte count, or 0 for a data file that is missing entirely and must
/// be recreated from recovery blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScratchFile {
    pub file_name: String,
    pub key: String,
    pub size: u64,
    pub role: FileRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    Data,
    Par2,
}

/// `POST /repair` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairJobRequest {
    pub job_id: String,
    pub round: u32,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    pub files: Vec<ScratchFile>,
}

/// One repaired output file, retrievable at `out/{key}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputFile {
    pub file_name: String,
    pub key: String,
    pub size: u64,
}

/// Terminal repair outcome, also stored as `result.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairJobResult {
    pub status: RepairStatus,
    pub exit_code: Option<i32>,
    pub files: Vec<OutputFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairStatus {
    Repaired,
    Unrepairable,
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
}

impl From<io::Error> for ApiError {
    fn from(_: io::Error) -> Self {
        Self::new(500, "repair_unavailable")
    }
}

/// Validate a repair request against the scratch contract. Every field
/// is untrusted NZB-derived metadata until it passes here.
pub fn validate_request(req: &RepairJobRequest) -> Result<(), ApiError> {
    // The canonical-prefix builder doubles as the job/round validator.
    if nntune_core::par2::scratch::scratch_prefix(&req.job_id, req.round).is_none() {
        return Err(ApiError::invalid());
    }
    if req.files.is_empty() || req.files.len() > MAX_SCRATCH_FILES {
        return Err(ApiError::invalid());
    }
    if let Some(timeout) = req.timeout_secs {
        if timeout == 0 || timeout > MAX_TIMEOUT_SECS {
            return Err(ApiError::invalid());
        }
    }
    let mut names = HashSet::new();
    let mut keys = HashSet::new();
    let mut total: u64 = 0;
    let mut has_par2 = false;
    for file in &req.files {
        if !validate_file_name(&file.file_name) || !validate_object_key(&file.key) {
            return Err(ApiError::invalid());
        }
        if file.key != object_key_for_file_name(&file.file_name) {
            return Err(ApiError::invalid());
        }
        if !names.insert(&file.file_name) || !keys.insert(&file.key) {
            return Err(ApiError::invalid());
        }
        if file.role == FileRole::Par2 {
            has_par2 = true;
            if file.size == 0 {
                return Err(ApiError::invalid());
            }
        }
        total = total.checked_add(file.size).ok_or_else(ApiError::invalid)?;
    }
    if !has_par2 || total > MAX_SCRATCH_TOTAL_BYTES {
        return Err(ApiError::invalid());
    }
    Ok(())
}

fn job_dir(config: &ServerConfig, job_id: &str, round: u32) -> PathBuf {
    config.scratch_root.join(job_id).join(format!("r{round}"))
}

/// Execute one repair request against already-staged inputs. Terminal
/// results are persisted under the job directory so a repeated request
/// is idempotent; transient failures (timeout, resource ceilings)
/// persist nothing and may be retried.
pub fn run_repair(
    config: &ServerConfig,
    req: &RepairJobRequest,
) -> Result<RepairJobResult, ApiError> {
    validate_request(req)?;
    let dir = job_dir(config, &req.job_id, req.round);
    let request_path = dir.join("request.json");
    let result_path = dir.join("result.json");

    if let Ok(stored) = fs::read(&request_path) {
        let stored_req: RepairJobRequest = serde_json::from_slice(&stored)
            .map_err(|_| ApiError::new(500, "repair_unavailable"))?;
        if stored_req != *req {
            return Err(ApiError::new(409, "repair_manifest_conflict"));
        }
        if let Ok(stored_result) = fs::read(&result_path) {
            let result: RepairJobResult = serde_json::from_slice(&stored_result)
                .map_err(|_| ApiError::new(500, "repair_unavailable"))?;
            return Ok(result);
        }
    } else {
        fs::create_dir_all(dir.join("in"))?;
        write_atomic(&request_path, &serde_json::to_vec(req).unwrap())?;
    }

    // Materialise staged inputs under their real names in a fresh work
    // dir. `fs::copy` (never a hardlink): the bounded runner rejects
    // multiply-linked files in its workspace accounting.
    let work = dir.join("work");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work)?;
    let mut par2_paths: Vec<PathBuf> = Vec::new();
    for file in &req.files {
        let staged = dir.join("in").join(&file.key);
        match fs::metadata(&staged) {
            Ok(meta) => {
                if !meta.is_file() || meta.len() != file.size {
                    return Err(ApiError::new(400, "scratch_input_mismatch"));
                }
                fs::copy(&staged, work.join(&file.file_name))?;
            }
            Err(_) if file.role == FileRole::Data && file.size == 0 => {
                // Missing target: par2 recreates it from recovery blocks.
            }
            Err(_) => return Err(ApiError::new(400, "scratch_input_mismatch")),
        }
        if file.role == FileRole::Par2 {
            par2_paths.push(PathBuf::from(&file.file_name));
        }
    }

    let limits = RepairLimits {
        timeout: Duration::from_secs(req.timeout_secs.unwrap_or(MAX_TIMEOUT_SECS)),
        ..RepairLimits::default()
    };
    let par2_refs: Vec<&Path> = par2_paths.iter().map(PathBuf::as_path).collect();
    let report = match repair_with_limits(&config.par2_bin, &work, &par2_refs, &limits) {
        Ok(report) => report,
        Err(CoreError::Io(e)) if e.kind() == io::ErrorKind::TimedOut => {
            return Err(ApiError::new(503, "repair_timeout"));
        }
        Err(CoreError::Io(e))
            if e.kind() == io::ErrorKind::InvalidData
                && e.to_string().contains("limit exceeded") =>
        {
            return Err(ApiError::new(503, "repair_resource"));
        }
        Err(_) => return Err(ApiError::new(500, "repair_unavailable")),
    };

    let result = if report.success {
        let out_dir = dir.join("out");
        fs::create_dir_all(&out_dir)?;
        let mut outputs = Vec::new();
        for file in req.files.iter().filter(|f| f.role == FileRole::Data) {
            let repaired = work.join(&file.file_name);
            let size = fs::metadata(&repaired)
                .map_err(|_| ApiError::new(500, "repair_output_missing"))?
                .len();
            let out_key = object_key_for_file_name(&file.file_name);
            fs::copy(&repaired, out_dir.join(&out_key))?;
            outputs.push(OutputFile {
                file_name: file.file_name.clone(),
                key: out_key,
                size,
            });
        }
        RepairJobResult {
            status: RepairStatus::Repaired,
            exit_code: report.exit_code,
            files: outputs,
        }
    } else {
        match report.exit_code {
            // Permanently unusable for this set (see RepairReport docs).
            Some(1 | 2 | 4 | 6) => RepairJobResult {
                status: RepairStatus::Unrepairable,
                exit_code: report.exit_code,
                files: Vec::new(),
            },
            // OOM or a kernel/limit kill: retryable, nothing persisted.
            Some(8) => return Err(ApiError::new(503, "repair_resource")),
            None => return Err(ApiError::new(503, "repair_resource")),
            _ => return Err(ApiError::new(503, "repair_failed")),
        }
    };

    write_atomic(&result_path, &serde_json::to_vec(&result).unwrap())?;
    eprintln!(
        "repair job={} round={} status={:?} outputs={}",
        req.job_id,
        req.round,
        result.status,
        result.files.len()
    );
    Ok(result)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ApiError> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Stage one input object from a request body reader. The aggregate
/// cap is enforced against what is already staged for the round.
fn store_input(
    config: &ServerConfig,
    job_id: &str,
    round: u32,
    key: &str,
    body: &mut impl Read,
    declared_len: Option<u64>,
) -> Result<u64, ApiError> {
    if nntune_core::par2::scratch::scratch_prefix(job_id, round).is_none()
        || !validate_object_key(key)
    {
        return Err(ApiError::invalid());
    }
    if let Some(len) = declared_len {
        if len > MAX_SCRATCH_TOTAL_BYTES {
            return Err(ApiError::new(413, "payload_too_large"));
        }
    }
    let dir = job_dir(config, job_id, round).join("in");
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
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if staged_total + written > MAX_SCRATCH_TOTAL_BYTES {
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

/// Read the header block, leaving any already-buffered body bytes in
/// `leftover` for the body reader.
fn read_head(stream: &mut impl Read) -> Result<(Request, Vec<u8>), ApiError> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if buf.len() > MAX_HEADER_BYTES {
            return Err(ApiError::invalid());
        }
        let n = stream.read(&mut byte)?;
        if n == 0 {
            return Err(ApiError::invalid());
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.lines();
    let mut first = lines
        .next()
        .ok_or_else(ApiError::invalid)?
        .split_whitespace();
    let method = first.next().ok_or_else(ApiError::invalid)?.to_string();
    let path = first.next().ok_or_else(ApiError::invalid)?.to_string();
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse::<u64>().ok();
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.eq_ignore_ascii_case("chunked")
            {
                chunked = true;
            }
        }
    }
    // The head reader consumes byte-by-byte, so nothing past the header
    // terminator has been read yet.
    Ok((
        Request {
            method,
            path,
            content_length,
            chunked,
        },
        Vec::new(),
    ))
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
        let mut byte = [0u8; 1];
        loop {
            if line.len() > 32 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "chunk header"));
            }
            let n = self.stream.read(&mut byte)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "chunk header"));
            }
            line.push(byte[0]);
            if line.ends_with(b"\r\n") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&line);
        let size = text.trim().split(';').next().unwrap_or("");
        u64::from_str_radix(size.trim(), 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk size"))
    }
}

impl Read for BodyReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self.mode {
            BodyMode::Fixed => {
                if self.remaining == 0 {
                    return Ok(0);
                }
                let take = (out.len() as u64).min(self.remaining) as usize;
                let n = self.stream.read(&mut out[..take])?;
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
                        // Trailer part ends with a blank line; consume it.
                        let mut trailer = Vec::new();
                        let mut byte = [0u8; 1];
                        loop {
                            let n = self.stream.read(&mut byte)?;
                            if n == 0 {
                                break;
                            }
                            trailer.push(byte[0]);
                            if trailer.ends_with(b"\r\n") {
                                break;
                            }
                            if trailer.len() > MAX_HEADER_BYTES {
                                break;
                            }
                        }
                        self.done = true;
                        return Ok(0);
                    }
                    self.chunk_left = size;
                }
                let take = (out.len() as u64).min(self.chunk_left) as usize;
                let n = self.stream.read(&mut out[..take])?;
                self.chunk_left -= n as u64;
                if self.chunk_left == 0 {
                    let mut crlf = [0u8; 2];
                    self.stream.read_exact(&mut crlf)?;
                }
                Ok(n)
            }
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
        let n = reader.read(&mut buf)?;
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

/// Parse `/jobs/{job}/r{round}/{in|out}/{key}` path segments.
fn parse_job_path(path: &str) -> Option<(String, u32, &'static str, String)> {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.len() != 5 || parts[0] != "jobs" {
        return None;
    }
    let round = parts[2].strip_prefix('r')?.parse::<u32>().ok()?;
    let lane = match parts[3] {
        "in" => "in",
        "out" => "out",
        _ => return None,
    };
    Some((parts[1].to_string(), round, lane, parts[4].to_string()))
}

fn handle(stream: &mut std::net::TcpStream, config: &ServerConfig, gate: &RwLock<()>) {
    let result = (|| -> Result<(), ApiError> {
        let (req, _leftover) = read_head(stream)?;
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/health") => {
                respond_json(stream, 200, br#"{"ok":true}"#)?;
            }
            ("POST", "/repair") => {
                let body = read_json_body(stream, &req)?;
                let job_req: RepairJobRequest =
                    serde_json::from_slice(&body).map_err(|_| ApiError::invalid())?;
                let _guard = gate.write().unwrap_or_else(|e| e.into_inner());
                let result = run_repair(config, &job_req)?;
                respond_json(stream, 200, &serde_json::to_vec(&result).unwrap())?;
            }
            ("POST", "/cleanup") => {
                let body = read_json_body(stream, &req)?;
                #[derive(Deserialize)]
                struct Cleanup {
                    job_id: String,
                    round: u32,
                }
                let cleanup: Cleanup =
                    serde_json::from_slice(&body).map_err(|_| ApiError::invalid())?;
                if nntune_core::par2::scratch::scratch_prefix(&cleanup.job_id, cleanup.round)
                    .is_none()
                {
                    return Err(ApiError::invalid());
                }
                let _guard = gate.write().unwrap_or_else(|e| e.into_inner());
                let dir = job_dir(config, &cleanup.job_id, cleanup.round);
                let _ = fs::remove_dir_all(&dir);
                // Remove the now-empty per-job parent too (best effort).
                let _ = fs::remove_dir(dir.parent().unwrap_or(&dir));
                respond_json(stream, 200, br#"{"ok":true}"#)?;
            }
            ("PUT", path) => {
                let (job, round, lane, key) =
                    parse_job_path(path).ok_or_else(ApiError::not_found)?;
                if lane != "in" {
                    return Err(ApiError::not_found());
                }
                let _guard = gate.write().unwrap_or_else(|e| e.into_inner());
                let size = {
                    let mut reader = BodyReader::new(&mut *stream, &req)?;
                    store_input(config, &job, round, &key, &mut reader, req.content_length)?
                };
                let body =
                    serde_json::to_vec(&serde_json::json!({"key": key, "size": size})).unwrap();
                respond_json(stream, 201, &body)?;
            }
            ("GET", path) => {
                let (job, round, lane, key) =
                    parse_job_path(path).ok_or_else(ApiError::not_found)?;
                if lane != "out" || !validate_object_key(&key) {
                    return Err(ApiError::not_found());
                }
                let _guard = gate.read().unwrap_or_else(|e| e.into_inner());
                let file_path = job_dir(config, &job, round).join("out").join(&key);
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
