//! Wire contract between ingest, this Worker, the scratch bucket and
//! the Container. Pure and native-testable: every IO step in the
//! runtime is preceded by validation here.
//!
//! Shapes mirror the ingest `TranscodeRequest`
//! (`workers/ingest/src/transcode.rs`) and the Container server
//! (`containers/transcode`); field names are pinned by tests on all
//! three sides.

use nntune_core::par2::scratch::{sha256_hex, validate_staging_id};
use nntune_core::transcode::{
    canonical_sha256_hex, validate_transcode_input_key, validate_transcode_prefix,
};
use serde::{Deserialize, Serialize};

/// Body caps for the JSON documents this Worker reads.
pub const MAX_REQUEST_BYTES: usize = 64 << 10;
pub const MAX_RESULT_BYTES: usize = 1 << 20;
/// Largest artifact fetched back from the Container: the profile
/// ceiling (64 kbps over a 60-minute track is ~28.8 MB plus Ogg
/// framing). Ingest owns the ceiling
/// (`workers/ingest/src/transcode.rs::MAX_ARTIFACT_BYTES`); the
/// mirror here keeps a forged or buggy Container from making the
/// front buffer an unbounded reply.
pub const MAX_ARTIFACT_BYTES: usize = 32 << 20;
/// Largest staged input: verify assembles single objects up to 32
/// MiB, so anything larger is not this pipeline's verified audio.
/// The Container enforces the same ceiling independently.
pub const MAX_TRANSCODE_INPUT_BYTES: u64 = 64 << 20;
/// Longest ingest idempotency key this front accepts. Ingest owns the
/// shape (`ingest:{user}:{sha}:{ref}`, at most 768 bytes); the mirror
/// here keeps a forged caller from stuffing unbounded bytes into a
/// request the front would otherwise forward.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 768;
/// Fixed artifact object name under `out/`, on R2 and in the Container.
pub const ARTIFACT_NAME: &str = "artifact.ogg";

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

/// The ingest transcode request, mirrored field-for-field. Field names
/// and order match `workers/ingest/src/transcode.rs::TranscodeRequest`
/// (pinned there by `request_json_shape_is_pinned_for_the_front`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeRequest {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub input_key: String,
    pub source_sha256: String,
    pub scratch_prefix: String,
    pub staging_id: String,
}

fn validate_job_id(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn validate_user_id(raw: &str) -> bool {
    !raw.is_empty() && raw.len() <= 128 && !raw.contains(':') && !raw.contains('\0')
}

fn validate_idempotency_key(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= MAX_IDEMPOTENCY_KEY_BYTES
        && raw.starts_with("ingest:")
        && !raw.contains('\0')
}

/// Parse and validate an ingest transcode request. The rules mirror
/// ingest's `decide_transcode` exactly: a request ingest built must
/// parse here, and anything else must not.
pub fn parse_request(body: &[u8]) -> Result<TranscodeRequest, ApiFault> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(ApiFault::new(413, "payload_too_large"));
    }
    let req: TranscodeRequest = serde_json::from_slice(body).map_err(|_| ApiFault::invalid())?;
    if !validate_job_id(&req.job_id)
        || !validate_user_id(&req.user_id)
        || !validate_idempotency_key(&req.idempotency_key)
        || !validate_transcode_input_key(&req.input_key)
        || canonical_sha256_hex(&req.source_sha256).is_err()
        || !validate_transcode_prefix(&req.scratch_prefix, &req.job_id)
        || !validate_staging_id(&req.staging_id)
    {
        return Err(ApiFault::invalid());
    }
    Ok(req)
}

/// Terminal transcode status, stored in `out/result.json` and replied
/// to ingest. Wire spellings match the ingest classifier
/// (`classify_transcode_reply`): only `transcoded` with an
/// in-contract hash and size can complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscodeStatus {
    Transcoded,
    Untranscodable,
}

impl TranscodeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transcoded => "transcoded",
            Self::Untranscodable => "untranscodable",
        }
    }
}

/// One staging's terminal result, persisted as `out/result.json`. The
/// Container's own output lacks `staging_id`; this Worker stamps it
/// before persisting (serde defaults it to empty on parse).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeResult {
    pub status: TranscodeStatus,
    pub exit_code: Option<i32>,
    pub artifact_sha256: Option<String>,
    pub byte_size: Option<u64>,
    /// The request's `staging_id`: idempotency is keyed on the
    /// staging, not just `job_id`, so the previous input's verdict
    /// (same job, other staging) never replays for this input.
    #[serde(default)]
    pub staging_id: String,
}

/// Whether this request must run the Container or already finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// No stored result: run the Container.
    Run,
    /// A stored result exists but was produced for a different
    /// staging (the previous input's verdict): clear the Container's
    /// stale staging state, delete the stale stored result, and run
    /// again.
    Rerun,
    AlreadyDone(TranscodeResult),
}

/// Idempotency decision from the stored `result.json`, if any. Only a
/// result stamped with the *current* request's `staging_id`
/// short-circuits the paid run; an unstamped (legacy) or mismatched
/// result is another input's verdict and reruns. A corrupt stored
/// result is a server fault: silently re-running would spend a second
/// paid encode on a staging that may have completed.
pub fn plan(
    _req: &TranscodeRequest,
    existing_result: Option<&[u8]>,
    current_staging_id: &str,
) -> Result<Plan, ApiFault> {
    let Some(bytes) = existing_result else {
        return Ok(Plan::Run);
    };
    if bytes.len() > MAX_RESULT_BYTES {
        return Err(ApiFault::new(500, "transcode_unavailable"));
    }
    let result: TranscodeResult =
        serde_json::from_slice(bytes).map_err(|_| ApiFault::new(500, "transcode_unavailable"))?;
    if !current_staging_id.is_empty() && result.staging_id == current_staging_id {
        Ok(Plan::AlreadyDone(result))
    } else {
        Ok(Plan::Rerun)
    }
}

/// The status document replied to ingest. The artifact bytes stay in
/// scratch for ingest to fetch; the wire reply carries only the hash
/// and size, so it always fits ingest's reply cap. Field names match
/// the ingest classifier's reads (`status`, `artifact_sha256`,
/// `byte_size`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeReply {
    pub status: TranscodeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_size: Option<u64>,
}

/// Validate a `Transcoded` result's shape before any byte is fetched
/// or persisted. The wasm runtime calls this on every Container
/// `Done` outcome; rejections are the retryable 502
/// `container_unavailable` fault the runtime already uses for a
/// Container reply it cannot trust. An `Untranscodable` result
/// carries no artifact to validate.
pub fn validate_transcoded_result(result: &TranscodeResult) -> Result<(), ApiFault> {
    if result.status != TranscodeStatus::Transcoded {
        return Ok(());
    }
    let invalid = || ApiFault::new(502, "container_unavailable");
    let Some(sha) = result.artifact_sha256.as_deref() else {
        return Err(invalid());
    };
    if canonical_sha256_hex(sha).is_err() {
        return Err(invalid());
    }
    match result.byte_size {
        Some(size) if (1..=MAX_ARTIFACT_BYTES as u64).contains(&size) => Ok(()),
        _ => Err(invalid()),
    }
}

/// Verify fetched artifact bytes against a validated result's claims:
/// exact size plus exact SHA-256. The wasm runtime calls this after
/// fetching `out/artifact.ogg` from the Container and before writing
/// anything to R2, so a corrupt or substituted artifact fails here —
/// never as a published row. Mismatches are the retryable 502: a
/// redelivery replays the Container's stored result and re-fetches,
/// and a persistently corrupt Container quarantines through the
/// shared attempt budget rather than failing the job as
/// undecodable input.
pub fn verify_artifact(
    bytes: &[u8],
    artifact_sha256: &str,
    byte_size: u64,
) -> Result<(), ApiFault> {
    let invalid = || ApiFault::new(502, "container_unavailable");
    if bytes.len() as u64 != byte_size {
        return Err(invalid());
    }
    let canonical = canonical_sha256_hex(artifact_sha256).map_err(|_| invalid())?;
    if sha256_hex(bytes) != canonical {
        return Err(invalid());
    }
    Ok(())
}

/// Settle the front door's object headers and capped body under ONE owned
/// deadline. Like the ingest race, `select` polls its first argument first:
/// an expired deadline wins even when the operation is ready. Carrying the
/// same future into the body race prevents dribbles from extending it. Every
/// return drops the timer and unfinished local operation; this is not proof
/// that the remote object or Container stopped executing.
pub async fn settle_do_reply<F, S, D>(fetch: F, deadline: D) -> Result<(u16, Vec<u8>), ApiFault>
where
    F: std::future::Future<Output = Result<(u16, S), ()>> + Unpin,
    S: futures_util::Stream<Item = Result<Vec<u8>, ()>> + Unpin,
    D: std::future::Future<Output = ()> + Unpin,
{
    use futures_util::future::{select, Either};
    use futures_util::StreamExt;

    let (reply, deadline) = match select(deadline, fetch).await {
        Either::Left(_) => return Err(transcode_unavailable_fault()),
        Either::Right((reply, deadline)) => (reply, deadline),
    };
    let (status, mut stream) = reply.map_err(|()| transcode_unavailable_fault())?;
    let drain = Box::pin(async move {
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|()| transcode_unavailable_fault())?;
            if !push_chunk_capped(&mut body, &chunk, MAX_RESULT_BYTES) {
                return Err(transcode_unavailable_fault());
            }
        }
        Ok((status, body))
    });
    match select(deadline, drain).await {
        Either::Left(_) => Err(transcode_unavailable_fault()),
        Either::Right((reply, _deadline)) => reply,
    }
}

pub fn ingest_reply_body(result: &TranscodeResult) -> Vec<u8> {
    let reply = TranscodeReply {
        status: result.status,
        artifact_sha256: result.artifact_sha256.clone(),
        byte_size: result.byte_size,
    };
    serde_json::to_vec(&reply).unwrap_or_default()
}

// Scratch object keys for one round dir. `prefix` has already been
// validated as this job's `ingest-scratch/{job}/.../` dir.
pub fn result_key(prefix: &str) -> String {
    format!("{prefix}out/result.json")
}
pub fn in_key(prefix: &str, key: &str) -> String {
    format!("{prefix}in/{key}")
}
pub fn artifact_key(prefix: &str) -> String {
    format!("{prefix}out/{ARTIFACT_NAME}")
}

/// The `POST /transcode` body for the Container: the input identity
/// and nothing else. No user ids, idempotency keys or credentials
/// cross into the Container — it sees staged bytes plus ids for
/// logging. The `staging_id` crosses so the Container's own replay
/// check binds to the same staging identity as this Worker's plan,
/// and `source_sha256` crosses so the Container re-checks the staged
/// bytes before every run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerTranscodeRequest {
    pub job_id: String,
    pub staging_id: String,
    pub input_key: String,
    pub source_sha256: String,
}

pub fn container_transcode_body(req: &TranscodeRequest) -> ContainerTranscodeRequest {
    ContainerTranscodeRequest {
        job_id: req.job_id.clone(),
        staging_id: req.staging_id.clone(),
        input_key: req.input_key.clone(),
        source_sha256: req.source_sha256.clone(),
    }
}

/// What a Container reply means for the reply to ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerOutcome {
    /// Terminal result: persist the artifact and `result.json`, reply 200.
    Done(TranscodeResult),
    /// Forward this status + stable code to ingest unchanged.
    Forward(u16, String),
}

/// Classify a Container reply. Only a well-formed 200 result is
/// terminal; known 4xx/503 codes forward with their stable code;
/// anything else is a retryable `container_unavailable`.
pub fn classify_container_reply(status: u16, body: &[u8]) -> ContainerOutcome {
    if status == 200 {
        if body.len() <= MAX_RESULT_BYTES {
            if let Ok(result) = serde_json::from_slice::<TranscodeResult>(body) {
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

/// Append one response chunk only when it fits under `cap`.
///
/// Check-before-extend: a chunk that would push the accumulated body
/// over `cap` is rejected and the retained body is left untouched, so
/// an oversized reply is never materialised in full. Exactly-at-`cap`
/// stays admissible (`>` rejects, never `>=`). The wasm runtime
/// streams container replies and the artifact fetch through this.
pub fn push_chunk_capped(body: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    if body.len() + chunk.len() > cap {
        return false;
    }
    body.extend_from_slice(chunk);
    true
}

/// Fault for a Durable Object reply the front door cannot trust
/// (unreadable or over `MAX_RESULT_BYTES`): retryable 503, the same
/// mapping the front door already uses for its other failure arms.
pub fn transcode_unavailable_fault() -> ApiFault {
    ApiFault::new(503, "transcode_unavailable")
}

/// Fault for a Container reply `container_json` cannot trust
/// (unreadable or over `MAX_RESULT_BYTES`): retryable 502, the same
/// mapping `container_json` already uses for its other failure arms.
pub fn container_unavailable_fault() -> ApiFault {
    ApiFault::new(502, "container_unavailable")
}

#[cfg(test)]
mod artifact_guard_regression {
    //! Pin each guard of [`validate_transcoded_result`] and
    //! [`verify_artifact`] in isolation: removing any single check
    //! must turn its test red.

    use super::*;

    const HEX64: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";

    fn transcoded(sha: Option<&str>, size: Option<u64>) -> TranscodeResult {
        TranscodeResult {
            status: TranscodeStatus::Transcoded,
            exit_code: Some(0),
            artifact_sha256: sha.map(str::to_string),
            byte_size: size,
            staging_id: HEX64.to_string(),
        }
    }

    fn assert_rejected_502(result: &TranscodeResult, context: &str) {
        match validate_transcoded_result(result) {
            Err(err) => assert_eq!(
                (err.status, err.code),
                (502, "container_unavailable"),
                "{context}: rejection must keep the retryable 502 contract"
            ),
            Ok(()) => panic!("{context}: invalid result was accepted"),
        }
    }

    #[test]
    fn well_formed_transcoded_passes_and_untranscodable_skips() {
        assert!(validate_transcoded_result(&transcoded(Some(HEX64), Some(58))).is_ok());
        // Uppercase hash canonicalises at verify time, not here — but
        // the shape guard accepts either case like the classifier.
        assert!(validate_transcoded_result(&transcoded(
            Some(&HEX64.to_ascii_uppercase()),
            Some(1)
        ))
        .is_ok());
        let untranscodable = TranscodeResult {
            status: TranscodeStatus::Untranscodable,
            exit_code: Some(1),
            artifact_sha256: None,
            byte_size: None,
            staging_id: HEX64.to_string(),
        };
        assert!(validate_transcoded_result(&untranscodable).is_ok());
    }

    #[test]
    fn each_shape_guard_rejects_alone() {
        // Missing hash: every other field is valid.
        assert_rejected_502(&transcoded(None, Some(58)), "missing hash");
        // Non-hex hash, valid size.
        assert_rejected_502(&transcoded(Some("zzzz"), Some(58)), "non-hex hash");
        // Short hash, valid size.
        assert_rejected_502(&transcoded(Some("abcd"), Some(58)), "short hash");
        // Missing size, valid hash.
        assert_rejected_502(&transcoded(Some(HEX64), None), "missing size");
        // Zero size, valid hash.
        assert_rejected_502(&transcoded(Some(HEX64), Some(0)), "zero size");
        // Over-ceiling size, valid hash.
        assert_rejected_502(
            &transcoded(Some(HEX64), Some(MAX_ARTIFACT_BYTES as u64 + 1)),
            "over-ceiling size",
        );
        // Ceiling itself stays admissible.
        assert!(validate_transcoded_result(&transcoded(
            Some(HEX64),
            Some(MAX_ARTIFACT_BYTES as u64)
        ))
        .is_ok());
    }

    #[test]
    fn artifact_bytes_must_match_size_and_hash_exactly() {
        let bytes = b"artifact-bytes";
        let sha = sha256_hex(bytes);
        assert!(verify_artifact(bytes, &sha, bytes.len() as u64).is_ok());
        // Uppercase spelling of the same digest verifies.
        assert!(verify_artifact(bytes, &sha.to_ascii_uppercase(), bytes.len() as u64).is_ok());
        // Size lie alone fails.
        assert_eq!(
            verify_artifact(bytes, &sha, bytes.len() as u64 + 1),
            Err(ApiFault::new(502, "container_unavailable"))
        );
        // Hash lie alone fails (size exact).
        assert_eq!(
            verify_artifact(bytes, HEX64, bytes.len() as u64),
            Err(ApiFault::new(502, "container_unavailable"))
        );
        // Malformed hash fails instead of panicking the canonicaliser.
        assert_eq!(
            verify_artifact(bytes, "xyz", bytes.len() as u64),
            Err(ApiFault::new(502, "container_unavailable"))
        );
    }
}

#[cfg(test)]
mod capped_reply_read_tests {
    //! Pin the capped reply-read building blocks in isolation: the
    //! accumulator must check before extending (never retaining the
    //! excess) with exactly-at-cap admissible, and both reply-site
    //! faults must keep their existing status mapping.

    use super::*;

    /// `runtime.rs` source, read as text (NOT a crate import — the module is
    /// `cfg(target_arch = "wasm32")` and unreachable from native tests).
    const RUNTIME_RS: &str = include_str!("runtime.rs");

    /// Whitespace-normalised text of `read_response_capped`: from its
    /// signature up to (not including) the front-door `fetch`.
    fn capped_reader_helper() -> String {
        let start = RUNTIME_RS
            .find("async fn read_response_capped")
            .expect("runtime.rs no longer contains `read_response_capped`");
        let rest = &RUNTIME_RS[start..];
        let end = rest
            .find("async fn fetch")
            .expect("runtime.rs no longer contains `fetch` after `read_response_capped`");
        rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn capped_reader_forwards_cap_and_aborts_on_refusal() {
        let helper = capped_reader_helper();
        // The reader must forward its own `cap` to the accumulator — a
        // hardcoded or unbounded limit here would restore the unbounded
        // materialisation this change removes while every accumulator pin
        // above still passes.
        assert!(
            helper.contains("push_chunk_capped(&mut body, &chunk, cap)"),
            "read_response_capped must forward its cap: \
             `push_chunk_capped(&mut body, &chunk, cap)`"
        );
        // The accumulator's refusal must abort the read — ignoring the
        // boolean silently truncates an over-cap reply to a cap-length
        // prefix and returns it Ok, feeding a partial body to the
        // classifier (or the front door) as if complete.
        assert!(
            helper.contains("if !push_chunk_capped(&mut body, &chunk, cap) { return Err(()); }"),
            "read_response_capped must abort on refusal: \
             `if !push_chunk_capped(&mut body, &chunk, cap) {{ return Err(()); }}`"
        );
    }

    #[test]
    fn fast_path_arms_keep_bodyless_empty_and_materialised_boundary() {
        let helper = capped_reader_helper();
        // A bodyless reply (e.g. a bare 200 from `/health`) must keep
        // reading as empty — the value the previous whole-body `bytes()`
        // path returned for it. Turning this arm into an error would 502
        // the `container_port` health poll (502 in `container_json`,
        // 503 at the front door).
        assert!(
            helper.contains("ResponseBody::Empty => return Ok(Vec::new())"),
            "read_response_capped must keep reading a bodyless reply as empty: \
             `ResponseBody::Empty => return Ok(Vec::new())`"
        );
        // An already-materialised reply over the cap must be rejected
        // before cloning: the boundary is strict `>`, so exactly-at-cap
        // stays admissible, and neutering the guard would forward an
        // over-cap body verbatim (the front door has no second size gate).
        assert!(
            helper.contains("ResponseBody::Body(body) if body.len() > cap => return Err(())"),
            "read_response_capped must reject an over-cap materialised body \
             before cloning: `ResponseBody::Body(body) if body.len() > cap => return Err(())`"
        );
        // The admissible materialised body must still be returned as-is.
        assert!(
            helper.contains("ResponseBody::Body(body) => return Ok(body.clone())"),
            "read_response_capped must return an in-cap materialised body: \
             `ResponseBody::Body(body) => return Ok(body.clone())`"
        );
        // Order matters: the guarded arm must precede the plain clone arm,
        // otherwise the guard is dead code and every materialised body is
        // cloned without a cap check.
        let guarded = helper
            .find("ResponseBody::Body(body) if body.len() > cap")
            .expect("guarded Body arm must exist");
        let plain = helper
            .find("ResponseBody::Body(body) => return Ok(body.clone())")
            .expect("plain Body clone arm must exist");
        assert!(
            guarded < plain,
            "the over-cap guard must precede the plain clone arm"
        );
    }

    #[test]
    fn accepts_chunk_landing_exactly_at_cap() {
        let mut body = vec![0u8; MAX_RESULT_BYTES - 3];
        assert!(push_chunk_capped(&mut body, &[1, 2, 3], MAX_RESULT_BYTES));
        assert_eq!(body.len(), MAX_RESULT_BYTES);
    }

    #[test]
    fn rejects_chunk_crossing_cap_by_one_byte_without_retaining_it() {
        let mut body = vec![0u8; MAX_RESULT_BYTES - 3];
        assert!(!push_chunk_capped(
            &mut body,
            &[1, 2, 3, 4],
            MAX_RESULT_BYTES
        ));
        assert_eq!(body.len(), MAX_RESULT_BYTES - 3);
        assert!(body.iter().all(|&b| b == 0));
    }

    #[test]
    fn accumulates_sequence_up_to_cap_then_rejects() {
        let mut body = Vec::new();
        assert!(push_chunk_capped(
            &mut body,
            &vec![7u8; MAX_RESULT_BYTES - 1],
            MAX_RESULT_BYTES
        ));
        assert!(push_chunk_capped(&mut body, &[8], MAX_RESULT_BYTES));
        assert_eq!(body.len(), MAX_RESULT_BYTES);
        assert!(!push_chunk_capped(&mut body, &[9], MAX_RESULT_BYTES));
        assert_eq!(body.len(), MAX_RESULT_BYTES);
    }

    #[test]
    fn reply_site_faults_keep_existing_mapping() {
        assert_eq!(
            transcode_unavailable_fault(),
            ApiFault::new(503, "transcode_unavailable")
        );
        assert_eq!(
            container_unavailable_fault(),
            ApiFault::new(502, "container_unavailable")
        );
    }
}
