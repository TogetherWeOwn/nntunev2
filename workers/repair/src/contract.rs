//! Wire contract between ingest, this Worker, the scratch bucket and
//! the Container. Pure and native-testable: every IO step in the
//! runtime is preceded by validation here.
//!
//! Shapes mirror the ingest `RepairRequest`
//! (`workers/ingest/src/queue.rs`) and the Container server
//! (`containers/repair`); field names are pinned by tests on all
//! three sides.

use std::collections::HashSet;

use nntune_core::par2::scratch::{
    object_key_for_file_name, validate_file_name, validate_object_key, validate_scratch_prefix,
    MAX_SCRATCH_FILES, MAX_SCRATCH_TOTAL_BYTES,
};
use serde::{Deserialize, Serialize};

/// Paid repair rounds per job. Ingest owns the budget
/// (`jobs::MAX_REPAIR_ROUNDS`); the mirror here keeps a forged or
/// buggy caller from starting round 4 against the Container.
pub const MAX_REPAIR_ROUNDS: u32 = 3;
/// Body caps for the JSON documents this Worker reads.
pub const MAX_REQUEST_BYTES: usize = 64 << 10;
pub const MAX_MANIFEST_BYTES: usize = 1 << 20;
pub const MAX_RESULT_BYTES: usize = 1 << 20;

/// Stable API fault: HTTP status plus a machine-readable code. Ingest
/// maps statuses (400/404/409/413 permanent, 5xx transient); codes are
/// for logs and operators, so they carry no user data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiFault {
    pub status: u16,
    pub code: &'static str,
}

impl ApiFault {
    pub fn new(status: u16, code: &'static str) -> Self {
        Self { status, code }
    }
    pub fn invalid() -> Self {
        Self::new(400, "invalid_request")
    }
}

/// The ingest repair request, mirrored field-for-field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairRequest {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub par2_set_id: String,
    pub missing_blocks: u32,
    #[serde(default)]
    pub round: u32,
    #[serde(default)]
    pub scratch_prefix: String,
}

fn bounded_text(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !value.bytes().any(|b| b == 0 || b.is_ascii_control())
}

/// Parse and validate an ingest repair request. The scratch prefix is
/// rebuilt from `(job_id, round)` and compared — a caller can only
/// aim repair at the one canonical prefix for its own job and round.
pub fn parse_request(body: &[u8]) -> Result<RepairRequest, ApiFault> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(ApiFault::new(413, "payload_too_large"));
    }
    let req: RepairRequest = serde_json::from_slice(body).map_err(|_| ApiFault::invalid())?;
    if !bounded_text(&req.user_id, 128)
        || !bounded_text(&req.idempotency_key, 512)
        || !bounded_text(&req.par2_set_id, 256)
        || req.missing_blocks == 0
        || req.round == 0
        || req.round > MAX_REPAIR_ROUNDS
        || !validate_scratch_prefix(&req.scratch_prefix, &req.job_id, req.round)
    {
        return Err(ApiFault::invalid());
    }
    Ok(req)
}

/// One staged file in `manifest.json`.
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

/// `in/manifest.json`: everything verify staged for this repair round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScratchManifest {
    pub files: Vec<ScratchFile>,
}

/// Parse and validate a scratch manifest with the same rules the
/// Container re-checks (defence in depth at both ends of the stream).
pub fn parse_manifest(body: &[u8]) -> Result<ScratchManifest, ApiFault> {
    if body.len() > MAX_MANIFEST_BYTES {
        return Err(ApiFault::new(413, "payload_too_large"));
    }
    let manifest: ScratchManifest =
        serde_json::from_slice(body).map_err(|_| ApiFault::new(400, "invalid_manifest"))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub fn validate_manifest(manifest: &ScratchManifest) -> Result<(), ApiFault> {
    let invalid = || ApiFault::new(400, "invalid_manifest");
    if manifest.files.is_empty() || manifest.files.len() > MAX_SCRATCH_FILES {
        return Err(invalid());
    }
    let mut names = HashSet::new();
    let mut keys = HashSet::new();
    let mut total: u64 = 0;
    let mut has_par2 = false;
    for file in &manifest.files {
        if !validate_file_name(&file.file_name)
            || !validate_object_key(&file.key)
            || file.key != object_key_for_file_name(&file.file_name)
            || !names.insert(&file.file_name)
            || !keys.insert(&file.key)
        {
            return Err(invalid());
        }
        if file.role == FileRole::Par2 {
            has_par2 = true;
            if file.size == 0 {
                return Err(invalid());
            }
        }
        total = total.checked_add(file.size).ok_or_else(invalid)?;
    }
    if !has_par2 || total > MAX_SCRATCH_TOTAL_BYTES {
        return Err(invalid());
    }
    Ok(())
}

/// Terminal repair status, stored in `out/result.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairStatus {
    Repaired,
    Unrepairable,
}

impl RepairStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repaired => "repaired",
            Self::Unrepairable => "unrepairable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputFile {
    pub file_name: String,
    pub key: String,
    pub size: u64,
}

/// One round's terminal result (Container output == `out/result.json`
/// == the reply to ingest).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairResult {
    pub status: RepairStatus,
    pub exit_code: Option<i32>,
    pub files: Vec<OutputFile>,
}

/// Whether this request must run the Container or already finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Run,
    AlreadyDone(RepairResult),
}

/// Idempotency decision from the stored `result.json`, if any. A
/// corrupt stored result is a server fault: silently re-running would
/// spend a second paid repair on a round that may have completed.
pub fn plan(_req: &RepairRequest, existing_result: Option<&[u8]>) -> Result<Plan, ApiFault> {
    let Some(bytes) = existing_result else {
        return Ok(Plan::Run);
    };
    if bytes.len() > MAX_RESULT_BYTES {
        return Err(ApiFault::new(500, "repair_unavailable"));
    }
    let result: RepairResult =
        serde_json::from_slice(bytes).map_err(|_| ApiFault::new(500, "repair_unavailable"))?;
    Ok(Plan::AlreadyDone(result))
}

// Scratch object keys for one round. `prefix` has already been
// validated as the canonical `ingest-scratch/{job}/r{round}/`.
pub fn manifest_key(prefix: &str) -> String {
    format!("{prefix}in/manifest.json")
}
pub fn result_key(prefix: &str) -> String {
    format!("{prefix}out/result.json")
}
pub fn in_key(prefix: &str, key: &str) -> String {
    format!("{prefix}in/{key}")
}
pub fn out_key(prefix: &str, key: &str) -> String {
    format!("{prefix}out/{key}")
}

/// The `POST /repair` body for the Container: the manifest and nothing
/// else. No user ids, idempotency keys or credentials cross into the
/// Container — it sees staged bytes plus ids for logging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerRepairRequest {
    pub job_id: String,
    pub round: u32,
    pub files: Vec<ScratchFile>,
}

pub fn container_repair_body(
    req: &RepairRequest,
    manifest: &ScratchManifest,
) -> ContainerRepairRequest {
    ContainerRepairRequest {
        job_id: req.job_id.clone(),
        round: req.round,
        files: manifest.files.clone(),
    }
}

/// What a Container reply means for the reply to ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerOutcome {
    /// Terminal result: persist outputs and `result.json`, reply 200.
    Done(RepairResult),
    /// Forward this status + stable code to ingest unchanged.
    Forward(u16, String),
}

/// Classify a Container reply. Only a well-formed 200 result is
/// terminal; known 4xx/503 codes forward with their stable code;
/// anything else is a retryable `container_unavailable`.
pub fn classify_container_reply(status: u16, body: &[u8]) -> ContainerOutcome {
    if status == 200 {
        if body.len() <= MAX_RESULT_BYTES {
            if let Ok(result) = serde_json::from_slice::<RepairResult>(body) {
                return ContainerOutcome::Done(result);
            }
        }
        return ContainerOutcome::Forward(502, "container_unavailable".to_string());
    }
    let code = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .filter(|code| {
            code.len() <= 64
                && code
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        });
    match (status, code) {
        (400 | 404 | 409 | 413 | 503, Some(code)) => ContainerOutcome::Forward(status, code),
        _ => ContainerOutcome::Forward(502, "container_unavailable".to_string()),
    }
}
