//! Admin NZB ingest: per-file upload validation, the encrypted
//! no-expiry original, and the redacted job views (migration 0034).
//!
//! Pure decisions plus the v2 field envelope live here, mirroring the
//! `track_sources` `nzb_ref_*` envelope shapes from migration 0022
//! (fresh per-row DEK, 12-byte nonces, 48-byte wrapped DEK, envelope
//! version 2 literal). The store traits stay in [`crate::handlers`]
//! and the Postgres statements in [`crate::worker_entry`]; both sides
//! share this module's caps, vocabulary, identity derivation and row
//! types, so the two stores cannot drift apart.
//!
//! Validation order mirrors the pipeline intake
//! (`workers/ingest/src/nzb_intake.rs`, which this worker must not
//! depend on): empty, too-large, not-NZB heuristic, malformed, then
//! the per-dimension caps. The parse itself runs on the real
//! `nntune-core` NZB parser with the admin limits — never a forked
//! predicate — and every rejection maps to one fixed-vocabulary
//! reason. Parser internals (message IDs, subjects, file names) never
//! leave this module: responses carry the reason string only.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::vault::VaultError;

/// Raw NZB cap for `POST /v1/admin/ingest`: 8 MiB. Own constant plus a
/// `max_body_bytes` entry — the user 2 MiB intake gate and
/// `workers/ingest` stay untouched. Both the entry's collection bound
/// and the in-handler gate call `max_body_bytes`, so the two caps
/// cannot drift apart.
pub const ADMIN_INGEST_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;
/// Validated `<file>` cap: the core compiled-in maximum.
pub const ADMIN_MAX_FILES: usize = 1024;
/// Validated `<segment>`-per-file cap: the core compiled-in maximum.
pub const ADMIN_MAX_SEGMENTS_PER_FILE: usize = 4096;
/// Validated total-segment cap across the document: the core parser
/// bounds segments per file only, so the cross-file total is checked
/// after the parse.
pub const ADMIN_MAX_TOTAL_SEGMENTS: usize = 60_000;
/// Validated total declared-bytes cap: the core compiled-in maximum.
pub const ADMIN_MAX_DECLARED_BYTES: i64 = 64 << 30;

/// Audit action for a guarded admin ingest attempt (actor in
/// `user_id`, job and outcome in `details`, subject always null:
/// writers reference the job by id, as before).
pub const INGEST_UPLOAD_AUDIT_ACTION: &str = "ingest.uploaded";

/// Value bound into `ingest_originals.nzb_ref_envelope_version`: the
/// v2 field envelope. A SQL literal, never a caller parameter (the
/// schema `CHECK (nzb_ref_envelope_version = 2)` backstops it).
pub const ENVELOPE_VERSION: i16 = 2;

/// Value bound into `ingest_originals.nzb_ref_payload_format`: raw NZB
/// bytes. A SQL literal, never a caller parameter.
pub const PAYLOAD_FORMAT_RAW_NZB_V1: i16 = 1;

/// GCM nonce length (schema `CHECK (octet_length(..) = 12)`).
pub const NONCE_LEN: usize = 12;
/// Wrapped-DEK length: 32-byte DEK + 16-byte GCM tag (schema `CHECK
/// (octet_length(nzb_ref_dek_wrapped) = 48)`).
pub const WRAPPED_DEK_LEN: usize = 48;

/// AAD domain pinning the v2 envelope family (ADR 0002 rule 1).
const AAD_DOMAIN: &str = "nntune-envelope-v2";
/// AAD table/field pins for this envelope.
const AAD_TABLE: &str = "ingest_originals";
const AAD_FIELD: &str = "nzb_body";

/// Fixed failure vocabulary (migration 0034 `failure_reason` CHECK).
/// The wire `code` is the registry code for the HTTP status; the
/// `reason` is this vocabulary word. Parser errors are never echoed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    Empty,
    Malformed,
    NotNzb,
    TooLarge,
    TooManyFiles,
    TooManySegments,
    DeclaredBytesOverCap,
}

impl FailureReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureReason::Empty => "empty",
            FailureReason::Malformed => "malformed",
            FailureReason::NotNzb => "not_nzb",
            FailureReason::TooLarge => "too_large",
            FailureReason::TooManyFiles => "too_many_files",
            FailureReason::TooManySegments => "too_many_segments",
            FailureReason::DeclaredBytesOverCap => "declared_bytes_over_cap",
        }
    }

    /// Every vocabulary word parses back (exact match only); anything
    /// else — including case or whitespace variants — is not a reason.
    pub fn parse(raw: &str) -> Option<FailureReason> {
        match raw {
            "empty" => Some(FailureReason::Empty),
            "malformed" => Some(FailureReason::Malformed),
            "not_nzb" => Some(FailureReason::NotNzb),
            "too_large" => Some(FailureReason::TooLarge),
            "too_many_files" => Some(FailureReason::TooManyFiles),
            "too_many_segments" => Some(FailureReason::TooManySegments),
            "declared_bytes_over_cap" => Some(FailureReason::DeclaredBytesOverCap),
            _ => None,
        }
    }

    /// HTTP status: shape failures are 400, cap failures are 413.
    pub fn http_status(self) -> u16 {
        match self {
            FailureReason::Empty | FailureReason::Malformed | FailureReason::NotNzb => 400,
            FailureReason::TooLarge
            | FailureReason::TooManyFiles
            | FailureReason::TooManySegments
            | FailureReason::DeclaredBytesOverCap => 413,
        }
    }

    /// Registry wire code for the status: the api-owned 400 is the
    /// existing `invalid_request`; every 413 is `payload_too_large`.
    pub fn wire_code(self) -> &'static str {
        match self.http_status() {
            400 => "invalid_request",
            _ => "payload_too_large",
        }
    }
}

/// A validated upload: counts only — the document bytes stay with the
/// caller for envelope encryption. No message IDs, subjects, file
/// names or segment lists cross this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedNzb {
    pub file_count: u64,
    pub segment_count: u64,
    pub declared_bytes: u64,
}

/// Validate raw upload bytes against the admin caps, in intake order.
/// `Ok` carries counts; `Err` carries the fixed reason (never parser
/// detail). Empty and over-cap bodies fail before the parser runs.
pub fn classify_upload(body: &[u8]) -> Result<ValidatedNzb, FailureReason> {
    let document = parse_upload(body)?;
    Ok(ValidatedNzb {
        file_count: document.files.len() as u64,
        segment_count: document.files.iter().map(|f| f.segments.len() as u64).sum(),
        declared_bytes: document.encoded_bytes.max(0) as u64,
    })
}

/// Internal execution uses exactly the intake parser and caps, without a
/// second parse under the legacy user-upload limits. Parser content stays
/// in memory: only `classify_upload`'s counts may enter an intake response.
pub(crate) fn parse_upload(body: &[u8]) -> Result<nntune_core::nzb::Document, FailureReason> {
    if body.is_empty() {
        return Err(FailureReason::Empty);
    }
    if body.len() > ADMIN_INGEST_BODY_LIMIT_BYTES {
        return Err(FailureReason::TooLarge);
    }
    // The intake's not-NZB heuristic, on bytes (panic-free like the
    // intake's str-slice version: `windows` yields nothing when the
    // body is shorter than the needle, never panics).
    let head = &body[..body.len().min(1024)];
    let head_has_nzb = head.windows(4).any(|w| w == b"<nzb");
    let head_has_xml = head.windows(5).any(|w| w == b"<?xml");
    if !(head_has_nzb || (head_has_xml && body.windows(4).any(|w| w == b"<nzb"))) {
        return Err(FailureReason::NotNzb);
    }
    let limits = nntune_core::nzb::Limits {
        files: ADMIN_MAX_FILES,
        segments_per_file: ADMIN_MAX_SEGMENTS_PER_FILE,
        encoded_bytes: ADMIN_MAX_DECLARED_BYTES,
        document_bytes: ADMIN_INGEST_BODY_LIMIT_BYTES as u64,
    };
    let parser = nntune_core::nzb::Parser::new(limits).map_err(|_| FailureReason::Malformed)?;
    let document = parser.parse(body).map_err(|e| match e {
        nntune_core::Error::Bounds { bound, .. } => match bound {
            "document bytes" => FailureReason::TooLarge,
            "file count" => FailureReason::TooManyFiles,
            "segments in file" => FailureReason::TooManySegments,
            "declared encoded bytes" | "encoded bytes in one segment" => {
                FailureReason::DeclaredBytesOverCap
            }
            _ => FailureReason::Malformed,
        },
        _ => FailureReason::Malformed,
    })?;
    let mut segment_count: u64 = 0;
    for file in &document.files {
        segment_count = segment_count.saturating_add(file.segments.len() as u64);
    }
    if segment_count > ADMIN_MAX_TOTAL_SEGMENTS as u64 {
        return Err(FailureReason::TooManySegments);
    }
    Ok(document)
}

/// Content identity: lowercase hex SHA-256 of the raw upload bytes.
pub fn content_sha256_hex(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Deterministic job id: `adm_` plus unpadded-base64url SHA-256 (47
/// chars, inside the `ingest_jobs` id alphabet). Content-derived only:
/// the uploader never participates, so two admins uploading the same
/// bytes converge on one job.
pub fn derive_job_id(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    format!("adm_{}", crate::b64::encode(&digest))
}

/// Source-namespaced idempotency key: the content hash under the
/// `admin:` namespace, never colliding with pipeline keys.
pub fn idempotency_key(content_hex: &str) -> String {
    format!("admin:{content_hex}")
}

/// Per-row data-encrypting key, cleared on drop (same zeroizing
/// discipline as the provider envelope in [`crate::providers`]: not a
/// second crypto design).
#[derive(Zeroize, ZeroizeOnDrop)]
struct Dek([u8; 32]);

/// v2 original envelope: raw bytes for the `BYTEA` columns. No `Debug`
/// derive: this struct carries key material adjacent to ciphertext, and
/// a derived dump would print both (threat-model §3: no log line may
/// contain credential material). `Clone` copies the stored-form bytes
/// only (wrapped DEK, nonces, ciphertext — exactly what the table
/// holds); the unwrapped DEK never lives in this struct, so cloning
/// cannot duplicate key material.
#[derive(Clone)]
pub struct IngestOriginalEnvelope {
    pub dek_wrapped_nonce: [u8; NONCE_LEN],
    pub dek_wrapped: Vec<u8>,
    pub payload_nonce: [u8; NONCE_LEN],
    pub ciphertext: Vec<u8>,
}

/// Frame one variable-length AAD segment: `u32` big-endian length +
/// bytes. Fixed field order + fixed int widths make the encoding
/// unambiguous (ADR 0002 rule 1: no delimiter joins).
fn frame(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Canonical v2 AAD: `(domain, purpose, table, field, content-hash,
/// job-id, envelope_version, payload_format)` plus `kek_version` for
/// the DEK wrap. Built from the row being written/read, never from a
/// stored unauthenticated claim. The owner slot is the content hash —
/// never the uploader — so a re-upload by a second admin converges on
/// the identical AAD. Payload AAD intentionally excludes `kek_version`:
/// an authorized rewrap changes only the wrap columns.
fn aad_v2(
    purpose: &str,
    content_sha256_hex: &str,
    job_id: &str,
    envelope_version: u16,
    payload_format: u16,
    kek_version: Option<u32>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(128 + content_sha256_hex.len() + job_id.len());
    frame(&mut out, AAD_DOMAIN);
    frame(&mut out, purpose);
    frame(&mut out, AAD_TABLE);
    frame(&mut out, AAD_FIELD);
    frame(&mut out, content_sha256_hex);
    frame(&mut out, job_id);
    out.extend_from_slice(&envelope_version.to_be_bytes());
    out.extend_from_slice(&payload_format.to_be_bytes());
    if let Some(v) = kek_version {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

/// Encrypt upload bytes into a v2 envelope bound to (content hash,
/// job id). Fail-closed: secure-randomness failure (DEK or either
/// nonce) returns [`VaultError::RandomnessUnavailable`] (HTTP 503).
pub fn encrypt_ingest_original(
    kek: &[u8; 32],
    kek_version: u32,
    content_sha256_hex: &str,
    job_id: &str,
    plaintext: &[u8],
) -> Result<IngestOriginalEnvelope, VaultError> {
    let mut dek_bytes = crate::random::secure_random_bytes::<32>()?;
    let dek = Dek(dek_bytes);
    // `Dek(dek_bytes)` copies the array: clear the source buffer so
    // the DEK lives only in the zeroizing holder (mirrors `vault.rs`).
    dek_bytes.zeroize();

    // Wrap the DEK with the KEK (AAD binds content + job + KEK version).
    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| VaultError::Crypto)?;
    let wrap_nonce = crate::random::secure_random_bytes::<NONCE_LEN>()?;
    let dek_wrapped = kek_cipher
        .encrypt(
            &Nonce::from(wrap_nonce),
            Payload {
                msg: &dek.0,
                aad: &aad_v2(
                    "dek",
                    content_sha256_hex,
                    job_id,
                    ENVELOPE_VERSION as u16,
                    PAYLOAD_FORMAT_RAW_NZB_V1 as u16,
                    Some(kek_version),
                ),
            },
        )
        .map_err(|_| VaultError::Crypto)?;

    // Encrypt the payload with the DEK (AAD binds content + job; no
    // KEK version, so an authorized rewrap never touches the payload).
    let dek_cipher = Aes256Gcm::new_from_slice(&dek.0).map_err(|_| VaultError::Crypto)?;
    let payload_nonce = crate::random::secure_random_bytes::<NONCE_LEN>()?;
    let ciphertext = dek_cipher
        .encrypt(
            &Nonce::from(payload_nonce),
            Payload {
                msg: plaintext,
                aad: &aad_v2(
                    "payload",
                    content_sha256_hex,
                    job_id,
                    ENVELOPE_VERSION as u16,
                    PAYLOAD_FORMAT_RAW_NZB_V1 as u16,
                    None,
                ),
            },
        )
        .map_err(|_| VaultError::Crypto)?;

    Ok(IngestOriginalEnvelope {
        dek_wrapped_nonce: wrap_nonce,
        dek_wrapped,
        payload_nonce,
        ciphertext,
    })
}

/// Decrypt a stored original. The expected AAD is built from the row
/// being read (ADR 0002 rule 1): the caller passes the row's stored
/// `envelope_version`, `payload_format` and `kek_version`, and the KEK
/// for that stored version (version-pinned decrypt — no fallback
/// loop). Stored-shape failures (negative versions, a short/long
/// wrapped DEK) are [`VaultError::CorruptStoredData`];
/// authentication failures are the generic [`VaultError::Crypto`].
/// No `Debug` on the envelope: nothing here may land in a log line.
pub fn decrypt_ingest_original(
    kek: &[u8; 32],
    content_sha256_hex: &str,
    job_id: &str,
    envelope_version: i16,
    payload_format: i16,
    kek_version: u32,
    envelope: &IngestOriginalEnvelope,
) -> Result<Vec<u8>, VaultError> {
    let envelope_version =
        u16::try_from(envelope_version).map_err(|_| VaultError::CorruptStoredData)?;
    let payload_format =
        u16::try_from(payload_format).map_err(|_| VaultError::CorruptStoredData)?;
    if envelope.dek_wrapped.len() != WRAPPED_DEK_LEN {
        return Err(VaultError::CorruptStoredData);
    }
    let kek_cipher = Aes256Gcm::new_from_slice(kek).map_err(|_| VaultError::Crypto)?;
    let mut dek_bytes = kek_cipher
        .decrypt(
            &Nonce::from(envelope.dek_wrapped_nonce),
            Payload {
                msg: &envelope.dek_wrapped,
                aad: &aad_v2(
                    "dek",
                    content_sha256_hex,
                    job_id,
                    envelope_version,
                    payload_format,
                    Some(kek_version),
                ),
            },
        )
        .map_err(|_| VaultError::Crypto)?;
    let dek_array: [u8; 32] = dek_bytes
        .as_slice()
        .try_into()
        .map_err(|_| VaultError::Crypto)?;
    dek_bytes.zeroize();
    let dek = Dek(dek_array);
    let dek_cipher = Aes256Gcm::new_from_slice(&dek.0).map_err(|_| VaultError::Crypto)?;
    dek_cipher
        .decrypt(
            &Nonce::from(envelope.payload_nonce),
            Payload {
                msg: &envelope.ciphertext,
                aad: &aad_v2(
                    "payload",
                    content_sha256_hex,
                    job_id,
                    envelope_version,
                    payload_format,
                    None,
                ),
            },
        )
        .map_err(|_| VaultError::Crypto)
}

/// One guarded upload attempt: everything the store transaction needs.
/// `failure`/`original` are coupled — a failed attempt carries the
/// reason and no original; an accepted attempt carries the envelope
/// and no reason — and the store persists them in one transaction
/// with the audit row.
pub struct AdminIngestAttempt {
    pub actor_user_id: String,
    pub job_id: String,
    pub idempotency_key: String,
    pub content_sha256_hex: String,
    pub nzb_bytes: u64,
    pub file_count: Option<u64>,
    pub segment_count: Option<u64>,
    pub failure: Option<FailureReason>,
    pub original: Option<IngestOriginalEnvelope>,
    /// KEK version the envelope was wrapped under (accepted attempts
    /// only; the readers select the key by this stored version).
    pub kek_version: u32,
    pub source_ip: String,
    pub now: u64,
}

/// A stored admin job row, redacted by construction: counts and the
/// failure reason only — never NZB bytes, message IDs, subjects, file
/// names or segment lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAdminIngestJob {
    pub job_id: String,
    pub db_status: String,
    pub accepted_at: u64,
    pub nzb_bytes: u64,
    pub file_count: Option<u64>,
    pub segment_count: Option<u64>,
    pub failure_reason: Option<String>,
    pub has_original: bool,
    pub uploader_user_id: String,
}

/// Outcome of one guarded upload transaction: the stored row plus
/// whether this attempt converged on a pre-existing row, the actor
/// re-check refusal (nothing written or audited — the handler answers
/// the plain 404, as for any non-admin caller), or the account-deletion
/// fence refusal (nothing written or audited — the handler answers the
/// typed `account_deleting` 409, like every other fenced writer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminIngestUploadOutcome {
    Stored {
        job: StoredAdminIngestJob,
        deduplicated: bool,
    },
    ActorNotAdmin,
    AccountDeleting,
}

/// Outcome of the store-level delete primitive (no route serves it):
/// the original goes first, then the job, under the actor re-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminIngestDeleteOutcome {
    Deleted,
    Missing,
    ActorNotAdmin,
}

/// Redacted view of the existing job machine's ledger projection. `done`
/// means published, never merely parsed or matched. Intake failures keep
/// their original `failed` view; an explicit admin rejection is distinct.
/// Unknown stored states are a store bug, never a client answer.
pub fn view_status(db_status: &str) -> Option<&'static str> {
    match db_status {
        "queued" => Some("accepted"),
        "running" => Some("running"),
        "needs_review" => Some("needs_review"),
        "needs_archive_read" => Some("needs_archive_read"),
        "rejected" => Some("rejected"),
        "done" => Some("published"),
        "failed" => Some("failed"),
        _ => None,
    }
}

/// Inverse of `view_status`, shared by list filtering and its tests. Raw
/// machine stages are not public filters and cannot select another source.
pub fn db_status(view: &str) -> Option<&'static str> {
    match view {
        "accepted" => Some("queued"),
        "running" => Some("running"),
        "needs_review" => Some("needs_review"),
        "needs_archive_read" => Some("needs_archive_read"),
        "rejected" => Some("rejected"),
        "published" => Some("done"),
        "failed" => Some("failed"),
        _ => None,
    }
}

/// Status filter for the list reader: `None` lists every admin job;
/// `Some` admits exactly one view status. Anything else rejects
/// before any store read.
pub fn parse_status_filter(raw: Option<&str>) -> Option<Option<&'static str>> {
    match raw {
        None => Some(None),
        Some(raw) => db_status(raw).and_then(view_status).map(Some),
    }
}

/// Default list window: 50 rows.
pub const LIST_DEFAULT_LIMIT: u64 = 50;
/// Hard list window cap: the reader never serves more per call.
pub const LIST_MAX_LIMIT: u64 = 100;

/// Bounded list window: limit clamps to 1..=`LIST_MAX_LIMIT` (default
/// `LIST_DEFAULT_LIMIT`), offset saturates. Both arrive as raw query
/// text; unparsable is the default, never an error — the window only
/// narrows what is served.
pub fn parse_list_window(limit_raw: Option<&str>, offset_raw: Option<&str>) -> (u64, u64) {
    let limit = limit_raw
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|limit| *limit >= 1)
        .map(|limit| limit.min(LIST_MAX_LIMIT))
        .unwrap_or(LIST_DEFAULT_LIMIT);
    let offset = offset_raw
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(0);
    (limit, offset)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Minimal valid NZB: one file, two contiguous segments. Test
    /// fixtures only — never competitor content.
    pub(crate) fn minimal_nzb() -> Vec<u8> {
        b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n<head><meta type=\"title\">syntheticfixture</meta></head>\n<file poster=\"poster@example.invalid\" date=\"1700000000\" subject=\"&quot;synthetic.part1.rar&quot; yEnc (1/2)\">\n<groups><group>alt.binaries.synthetic</group></groups>\n<segments>\n<segment bytes=\"100\" number=\"1\">mid-one-part-0001@example.invalid</segment>\n<segment bytes=\"200\" number=\"2\">mid-one-part-0002@example.invalid</segment>\n</segments>\n</file>\n</nzb>\n".to_vec()
    }

    #[test]
    fn failure_vocabulary_round_trips_and_maps_status_and_code() {
        for (word, status, code) in [
            ("empty", 400, "invalid_request"),
            ("malformed", 400, "invalid_request"),
            ("not_nzb", 400, "invalid_request"),
            ("too_large", 413, "payload_too_large"),
            ("too_many_files", 413, "payload_too_large"),
            ("too_many_segments", 413, "payload_too_large"),
            ("declared_bytes_over_cap", 413, "payload_too_large"),
        ] {
            let reason = FailureReason::parse(word).expect("vocabulary parses");
            assert_eq!(reason.as_str(), word);
            assert_eq!(reason.http_status(), status);
            assert_eq!(reason.wire_code(), code);
        }
        assert_eq!(FailureReason::parse("Empty"), None);
        assert_eq!(FailureReason::parse(" empty"), None);
        assert_eq!(FailureReason::parse(""), None);
        assert_eq!(FailureReason::parse("too large"), None);
    }

    #[test]
    fn classify_accepts_a_minimal_document_with_counts() {
        let body = minimal_nzb();
        let valid = classify_upload(&body).expect("minimal NZB validates");
        assert_eq!(valid.file_count, 1);
        assert_eq!(valid.segment_count, 2);
        assert_eq!(valid.declared_bytes, 300);
    }

    #[test]
    fn classify_rejects_empty_oversize_and_non_nzb_before_parsing() {
        assert_eq!(classify_upload(&[]), Err(FailureReason::Empty));
        let oversize = vec![b'a'; ADMIN_INGEST_BODY_LIMIT_BYTES + 1];
        assert_eq!(classify_upload(&oversize), Err(FailureReason::TooLarge));
        assert_eq!(
            classify_upload(b"{\"not\": \"nzb\"}"),
            Err(FailureReason::NotNzb)
        );
        // Well-formed XML that is not an NZB document.
        assert_eq!(
            classify_upload(b"<?xml version=\"1.0\"?><feed><entry/></feed>"),
            Err(FailureReason::NotNzb)
        );
    }

    #[test]
    fn classify_maps_truncated_nzb_to_malformed_without_detail() {
        let mut body = minimal_nzb();
        body.truncate(body.len() / 2);
        assert_eq!(classify_upload(&body), Err(FailureReason::Malformed));
    }

    #[test]
    fn job_identity_is_content_derived_and_alphabet_safe() {
        let a = minimal_nzb();
        let mut b = minimal_nzb();
        b.push(b'\n');
        let id_a = derive_job_id(&a);
        assert_eq!(id_a, derive_job_id(&a));
        assert_ne!(id_a, derive_job_id(&b));
        assert!(id_a.starts_with("adm_"));
        assert_eq!(id_a.len(), 47);
        assert!(id_a
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'));
        let hex = content_sha256_hex(&a);
        assert_eq!(hex.len(), 64);
        assert!(hex
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(idempotency_key(&hex).starts_with("admin:"));
    }

    #[test]
    fn list_window_clamps_without_erroring() {
        assert_eq!(parse_list_window(None, None), (50, 0));
        assert_eq!(parse_list_window(Some("10"), Some("5")), (10, 5));
        assert_eq!(parse_list_window(Some("0"), None), (50, 0));
        assert_eq!(parse_list_window(Some("9999"), None), (100, 0));
        assert_eq!(parse_list_window(Some("junk"), Some("junk")), (50, 0));
        assert_eq!(parse_status_filter(None), Some(None));
        assert_eq!(
            parse_status_filter(Some("accepted")),
            Some(Some("accepted"))
        );
        assert_eq!(parse_status_filter(Some("failed")), Some(Some("failed")));
        assert_eq!(parse_status_filter(Some("queued")), None);
        assert_eq!(view_status("queued"), Some("accepted"));
        assert_eq!(view_status("failed"), Some("failed"));
        assert_eq!(view_status("done"), Some("published"));
    }

    #[test]
    fn execution_statuses_round_trip_without_exposing_machine_stages() {
        for (stored, view) in [
            ("queued", "accepted"),
            ("running", "running"),
            ("needs_review", "needs_review"),
            ("needs_archive_read", "needs_archive_read"),
            ("rejected", "rejected"),
            ("done", "published"),
            ("failed", "failed"),
        ] {
            assert_eq!(view_status(stored), Some(view));
            assert_eq!(db_status(view), Some(stored));
            assert_eq!(parse_status_filter(Some(view)), Some(Some(view)));
        }
        for raw in [
            "",
            "queued",
            "done",
            "matching",
            "probing",
            "RUNNING",
            "rejected ",
        ] {
            assert_eq!(db_status(raw), None);
            assert_eq!(parse_status_filter(Some(raw)), None);
        }
        for raw in [
            "",
            "accepted",
            "published",
            "matching",
            "probing",
            "RUNNING",
        ] {
            assert_eq!(view_status(raw), None);
        }
    }

    #[test]
    fn envelope_round_trips_and_tamper_fails_closed() {
        let kek = [7u8; 32];
        let body = minimal_nzb();
        let hex = content_sha256_hex(&body);
        let job_id = derive_job_id(&body);
        let envelope = encrypt_ingest_original(&kek, 1, &hex, &job_id, &body).expect("encrypts");
        assert_eq!(envelope.dek_wrapped.len(), WRAPPED_DEK_LEN);
        let open = decrypt_ingest_original(
            &kek,
            &hex,
            &job_id,
            ENVELOPE_VERSION,
            PAYLOAD_FORMAT_RAW_NZB_V1,
            1,
            &envelope,
        )
        .expect("decrypts");
        assert_eq!(open, body);
        // A flipped ciphertext byte fails authentication, never
        // partial plaintext.
        let mut tampered = IngestOriginalEnvelope {
            dek_wrapped_nonce: envelope.dek_wrapped_nonce,
            dek_wrapped: envelope.dek_wrapped.clone(),
            payload_nonce: envelope.payload_nonce,
            ciphertext: envelope.ciphertext.clone(),
        };
        tampered.ciphertext[0] ^= 1;
        assert!(decrypt_ingest_original(
            &kek,
            &hex,
            &job_id,
            ENVELOPE_VERSION,
            PAYLOAD_FORMAT_RAW_NZB_V1,
            1,
            &tampered
        )
        .is_err());
        // A different job id fails the AAD check: rows do not open
        // under each other's identity.
        assert!(decrypt_ingest_original(
            &kek,
            &hex,
            "adm_otheridentity0000000000000000000000000000",
            ENVELOPE_VERSION,
            PAYLOAD_FORMAT_RAW_NZB_V1,
            1,
            &envelope
        )
        .is_err());
        // A wrong KEK version's key fails the wrap check.
        assert!(decrypt_ingest_original(
            &[9u8; 32],
            &hex,
            &job_id,
            ENVELOPE_VERSION,
            PAYLOAD_FORMAT_RAW_NZB_V1,
            1,
            &envelope
        )
        .is_err());
    }
}
