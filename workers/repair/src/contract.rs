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
    object_key_for_file_name, sha256_hex, validate_file_name, validate_object_key,
    validate_scratch_prefix, validate_staging_id, MAX_SCRATCH_FILES, MAX_SCRATCH_TOTAL_BYTES,
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
    /// Content-derived id for this staging (verify derives it from
    /// every staged file's content hash, so identical bytes keep the
    /// id and any byte change yields a new one; see
    /// `nntune_core::par2::scratch`).
    /// Required: without it a stored result cannot be proven to
    /// belong to these inputs — names and sizes repeat across a
    /// redrive even when the bytes changed.
    pub staging_id: String,
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
    if !validate_staging_id(&manifest.staging_id) {
        return Err(invalid());
    }
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

/// One round's terminal result, persisted as `out/result.json`. The
/// Container's own output lacks `manifest_digest`; this Worker stamps
/// it before persisting (serde defaults it to empty on parse).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairResult {
    pub status: RepairStatus,
    pub exit_code: Option<i32>,
    pub files: Vec<OutputFile>,
    /// [`manifest_digest`] of the staged inputs this result was
    /// produced from. Idempotency is keyed on the staging, not just
    /// `(job_id, round)`: a redrive resets the round to 1, so a result
    /// from a previous generation's staging must not replay.
    #[serde(default)]
    pub manifest_digest: String,
}

/// Canonical digest of one staging: sha256 of the manifest's compact
/// JSON — `staging_id` plus the file list (struct field order and
/// list order are the canonical form, so two serialisations of the
/// same staging agree). The `staging_id` is what binds the digest to
/// one staging's bytes: a redrive re-stages the same file names at
/// the same sizes with different bytes, and only the content-derived
/// `staging_id` distinguishes it from the generation that produced a
/// stored result.
pub fn manifest_digest(manifest: &ScratchManifest) -> String {
    let canonical = serde_json::to_vec(manifest).unwrap_or_default();
    sha256_hex(&canonical)
}

/// Whether this request must run the Container or already finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// No stored result: run the Container.
    Run,
    /// A stored result exists but was produced from a different
    /// staging (a redriven job reusing this round): clear the
    /// Container's stale round state, delete the stale stored
    /// result, and run again.
    Rerun,
    AlreadyDone(RepairResult),
}

/// Idempotency decision from the stored `result.json`, if any. Only a
/// result stamped with the *current* manifest's digest short-circuits
/// the paid run; an unstamped (legacy) or mismatched result is a
/// previous generation's verdict and reruns. A corrupt stored result
/// is a server fault: silently re-running would spend a second paid
/// repair on a round that may have completed.
pub fn plan(
    _req: &RepairRequest,
    existing_result: Option<&[u8]>,
    current_manifest_digest: &str,
) -> Result<Plan, ApiFault> {
    let Some(bytes) = existing_result else {
        return Ok(Plan::Run);
    };
    if bytes.len() > MAX_RESULT_BYTES {
        return Err(ApiFault::new(500, "repair_unavailable"));
    }
    let result: RepairResult =
        serde_json::from_slice(bytes).map_err(|_| ApiFault::new(500, "repair_unavailable"))?;
    if !current_manifest_digest.is_empty() && result.manifest_digest == current_manifest_digest {
        Ok(Plan::AlreadyDone(result))
    } else {
        Ok(Plan::Rerun)
    }
}

/// The status document replied to ingest. The full `RepairResult`
/// (one entry per data file, up to `MAX_SCRATCH_FILES`) lives in
/// `out/result.json`; the wire reply is a few dozen bytes so it
/// always fits ingest's reply cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairReply {
    pub status: RepairStatus,
    pub exit_code: Option<i32>,
    pub file_count: usize,
}

/// Validate the set of outputs a `Repaired` result claims against the
/// manifest. The wasm runtime calls this before copying any output or
/// writing the durable result marker; rejections are the retryable
/// 502 `container_unavailable` fault the runtime already uses for a
/// Container reply it cannot trust. A non-`Repaired` result carries
/// no output set to validate.
///
/// The rule is set equality with the manifest's Data files: exactly
/// one output per Data file, with unique file names and unique
/// canonical object keys. Sizes are deliberately not compared — a
/// recreated zero-length Data target carries its repaired on-disk
/// size, not the manifest's 0.
pub fn validate_repaired_output_set(
    manifest: &ScratchManifest,
    result: &RepairResult,
) -> Result<(), ApiFault> {
    if result.status != RepairStatus::Repaired {
        return Ok(());
    }
    let invalid = || ApiFault::new(502, "container_unavailable");
    let data_names: HashSet<&str> = manifest
        .files
        .iter()
        .filter(|f| f.role == FileRole::Data)
        .map(|f| f.file_name.as_str())
        .collect();
    if result.files.len() != data_names.len() {
        return Err(invalid());
    }
    let mut seen_names = HashSet::with_capacity(result.files.len());
    let mut seen_keys = HashSet::with_capacity(result.files.len());
    for output in &result.files {
        if !data_names.contains(output.file_name.as_str())
            || output.key != object_key_for_file_name(&output.file_name)
            || !seen_names.insert(output.file_name.as_str())
            || !seen_keys.insert(output.key.as_str())
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
mod output_set_guard_regression {
    //! Pin each guard of [`validate_repaired_output_set`] in
    //! isolation: removing any single check must turn its test red.
    //! These live next to the validator — not in the acceptance
    //! suite — because that file is Test Author-owned and frozen
    //! for this change.

    use super::*;

    fn data_file(file_name: &str, size: u64) -> ScratchFile {
        ScratchFile {
            file_name: file_name.to_string(),
            key: object_key_for_file_name(file_name),
            size,
            role: FileRole::Data,
        }
    }

    fn two_data_manifest() -> ScratchManifest {
        ScratchManifest {
            staging_id: "staging-0000000000000001".to_string(),
            files: vec![
                data_file("track01.flac", 7),
                data_file("track02.flac", 9),
                ScratchFile {
                    file_name: "set.par2".to_string(),
                    key: object_key_for_file_name("set.par2"),
                    size: 8,
                    role: FileRole::Par2,
                },
            ],
        }
    }

    fn output(file_name: &str, size: u64) -> OutputFile {
        OutputFile {
            file_name: file_name.to_string(),
            key: object_key_for_file_name(file_name),
            size,
        }
    }

    fn repaired(files: Vec<OutputFile>) -> RepairResult {
        RepairResult {
            status: RepairStatus::Repaired,
            exit_code: Some(0),
            files,
            manifest_digest: String::new(),
        }
    }

    fn assert_rejected_502(manifest: &ScratchManifest, result: &RepairResult, context: &str) {
        match validate_repaired_output_set(manifest, result) {
            Err(err) => assert_eq!(
                (err.status, err.code),
                (502, "container_unavailable"),
                "{context}: rejection must keep the retryable 502 contract"
            ),
            Ok(()) => panic!("{context}: invalid output set was accepted"),
        }
    }

    #[test]
    fn unique_but_noncanonical_key_is_rejected() {
        // Count matches, names are unique manifest members, keys are
        // unique — only the canonical-key guard can reject this. The
        // forged key is unique across the set on purpose: sharing
        // another file's key would let the duplicate-key guard fire
        // first and leave this guard unpinned.
        let manifest = two_data_manifest();
        let forged_key = "0".repeat(64);
        assert_ne!(forged_key, object_key_for_file_name("track01.flac"));
        assert_ne!(forged_key, object_key_for_file_name("track02.flac"));
        let forged = OutputFile {
            file_name: "track02.flac".to_string(),
            key: forged_key,
            size: 9,
        };
        let result = repaired(vec![output("track01.flac", 7), forged]);
        assert_rejected_502(&manifest, &result, "unique but noncanonical key");
    }

    #[test]
    fn same_count_swap_with_unknown_file_is_rejected() {
        // Count matches, names are unique, keys are unique and each
        // canonical for its own name — only the manifest-membership
        // guard can reject this. A third output would let the count
        // guard fire first and leave this guard unpinned.
        let manifest = two_data_manifest();
        assert_ne!(
            object_key_for_file_name("extra.flac"),
            object_key_for_file_name("track01.flac")
        );
        assert_ne!(
            object_key_for_file_name("extra.flac"),
            object_key_for_file_name("track02.flac")
        );
        let result = repaired(vec![output("track01.flac", 7), output("extra.flac", 3)]);
        assert_rejected_502(&manifest, &result, "same-count swap with unknown file");
    }
}

pub fn ingest_reply_body(result: &RepairResult) -> Vec<u8> {
    let reply = RepairReply {
        status: result.status,
        exit_code: result.exit_code,
        file_count: result.files.len(),
    };
    serde_json::to_vec(&reply).unwrap_or_default()
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
/// Container — it sees staged bytes plus ids for logging. The
/// `staging_id` crosses so the Container's own replay check binds to
/// the same staging identity as this Worker's digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerRepairRequest {
    pub job_id: String,
    pub round: u32,
    pub staging_id: String,
    pub files: Vec<ScratchFile>,
}

pub fn container_repair_body(
    req: &RepairRequest,
    manifest: &ScratchManifest,
) -> ContainerRepairRequest {
    ContainerRepairRequest {
        job_id: req.job_id.clone(),
        round: req.round,
        staging_id: manifest.staging_id.clone(),
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
