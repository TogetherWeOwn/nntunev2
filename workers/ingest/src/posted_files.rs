//! `posted_files` writer: pure statements (migrations 0035 + 0036).
//!
//! Pipeline position: the post flow writes one content-global row per
//! tagged plaintext sha256 after the poster seals the file (encrypt with
//! `nntune_core::encrypt`, NZB via `nntune_core::nzb_writer`, article plan
//! via `plan_articles`) and the API-side mint wraps the per-file key and
//! the manifest in the existing AES-GCM envelope v2. This module is pure
//! and order-agnostic: it pins the full-row insert, the sha re-select, the
//! verify/retention touches, the parameter orders, and the zero-row
//! classification the wire-up uses. Zero execution here — the
//! writers-wiring follow-up owns the DB calls.
//!
//! Table split: migration 0035 (TWO-1768 two-spool ledger) owns copy state
//! — the backbone pair, counts, spool states — and migration 0036
//! (TWO-1761) owns key custody and geometry — sizes, header nonce, the two
//! envelopes, verify/retention timestamps. One table, one writer module:
//! the insert below binds the FULL row (both halves), because every new
//! column is `NOT NULL`-complete for poster-written rows (the 0036
//! `posted_files_crypto_complete` guard). The state machine on top
//! (`SpoolCopyState`, dedupe reuse) stays in [`crate::two_spool`]; reuse
//! logic lands on the verify card, not here.
//!
//! Envelope boundary (KEK never in ingest): this module pins the
//! `file_key_*`/`manifest_*` companion contract — envelope v2 as SQL
//! literals, lengths, kek versions, payload formats — and validates bound
//! values, but it never mints. Minting lives with the providers-minting
//! sibling, which generalizes the canonical v2 AAD construction
//! (`workers/api/src/providers.rs::aad_v2`, ADR 0002: length-prefixed
//! frames, never delimiter joins) with the tuple pinned below — NOT the v1
//! delimiter AAD (`v1|vault|<user_id>`) of
//! `workers/api/src/vault.rs::encrypt_vault`, which carries no
//! table/field/row binding and must never mint these columns. Each envelope
//! gets a fresh DEK (32-byte DEK + 16-byte tag = the 48-byte
//! `*_dek_wrapped` the schema CHECKs). The sibling seals:
//! - file key envelope (payload format 1): the raw 32-byte per-file key.
//! - manifest envelope (payload format 1): the `posted_manifest_v1` JSON
//!   document — `nzb_xml_base64` (standard-base64 NZB 1.1), one
//!   `[start, end_exclusive]` plaintext range and one `[offset, len]`
//!   ciphertext offset per posted article in NZB segment order,
//!   `flac_seek_index` (object or null), `par2_message_ids` (bare IDs),
//!   `posted_via` (backbone labels). Message-IDs, subjects, file names and
//!   segment lists exist ONLY inside this sealed manifest — never as
//!   plaintext columns, files, URLs or log lines.
//!
//! Envelope v2 AAD binding (ADR 0002: `domain, purpose, table, field,
//! owner_scope, row_key, envelope_version, payload_format`, plus
//! `kek_version` on the DEK-wrap AAD only; payload AAD excludes it so an
//! authorized rewrap never touches the payload):
//! - file key: `domain = "nntune-envelope-v2"`, `purpose = "payload"` (or
//!   `"dek"` for the wrap), [`ENVELOPE_AAD_TABLE`], [`FILE_KEY_AAD_FIELD`],
//!   [`ENVELOPE_AAD_OWNER_SCOPE`], `row_key = plaintext_sha256`,
//!   `envelope_version = 2`, `payload_format = 1`.
//! - manifest: the same tuple with [`MANIFEST_AAD_FIELD`].
//!
//! `row_key` is the tagged plaintext sha256 because it is the only row
//! identity that exists at mint time (the DB `id` is assigned on write and
//! re-selected, so it cannot be minted into). The two envelopes share
//! payload format 1, so file-key↔manifest separation rests entirely on the
//! AAD `field` element: cross-envelope and cross-row substitution fail
//! closed at decrypt time.
//!
//! The conflict target is `ON CONFLICT (plaintext_sha256) DO NOTHING`
//! (first writer wins): rows are content-global, and a divergent later
//! write is a pipeline bug that must not flap a shared row. Every post
//! mints a fresh file key, header nonce, DEKs and GCM nonces, so envelope
//! bytes are NOT stable across posts — existence (and reuse of the stored
//! key/manifest) is decided *before* posting, never re-sealed on replay;
//! the reuse check itself lands on the verify card, not here. A zero-row
//! write is either a benign replay or a lost write; the consumer separates
//! them with the sha re-select plus [`classify_posted_file_zero_rows`],
//! never by assuming.

use crate::track_writers::{ENVELOPE_NONCE_BYTES, ENVELOPE_WRAPPED_DEK_BYTES};
use crate::two_spool::{validate_backbone_label, validate_plaintext_sha256, SpoolCopyState};

/// Public rclone header nonce length (bytes). Posted in article 0 in the
/// clear; mirrors the 0036 `CHECK` (`octet_length(header_nonce) = 24`).
pub const HEADER_NONCE_BYTES: usize = 24;

/// `file_key_*` payload format: the raw 32-byte per-file key. Card-fixed
/// (0036 `CHECK`s `= 1`); the mint assigns nothing else here.
pub const FILE_KEY_PAYLOAD_FORMAT: i16 = 1;

/// `manifest_*` payload format: the `posted_manifest_v1` JSON document
/// described above. Card-fixed (0036 `CHECK`s `= 1`).
pub const MANIFEST_PAYLOAD_FORMAT: i16 = 1;

/// Envelope v2 AAD domain for both posted-file envelopes (ADR 0002).
pub const ENVELOPE_AAD_DOMAIN: &str = "nntune-envelope-v2";

/// Envelope v2 AAD table pin: both envelopes bind `posted_files`.
pub const ENVELOPE_AAD_TABLE: &str = "posted_files";

/// Envelope v2 AAD field pins. File key and manifest are distinct fields,
/// so cross-envelope substitution fails closed at decrypt time even though
/// both envelopes share payload format 1.
pub const FILE_KEY_AAD_FIELD: &str = "file_key";
pub const MANIFEST_AAD_FIELD: &str = "manifest";

/// Envelope v2 AAD owner scope: posted-file rows are content-global (no
/// user exists), so both envelopes bind this explicit tagged global scope —
/// never a user id, never an empty string (ADR 0002).
pub const ENVELOPE_AAD_OWNER_SCOPE: &str = "global:posted_files";

/// Format 1 seals exactly the raw 32-byte file key: 32 bytes of key plus
/// the 16-byte GCM tag. The manifest envelope is variable-length (a JSON
/// document), so only the file-key side pins its body length here.
pub const FILE_KEY_ENVELOPE_BODY_BYTES: usize = 48;

/// Latest representable epoch second (0022/0035 ceiling, year 9999).
pub const MAX_EPOCH: i64 = 253402300799;

/// Largest article set one row tracks (0035 `CHECK`, `<= 1000000`).
pub const MAX_ARTICLE_COUNT: i32 = 1_000_000;

/// Database wire string for a spool copy state: the 0035 `IN` list, which
/// is also the `snake_case` serde alphabet of [`SpoolCopyState`].
pub fn spool_copy_state_db_str(state: SpoolCopyState) -> &'static str {
    match state {
        SpoolCopyState::Unknown => "unknown",
        SpoolCopyState::Present => "present",
        SpoolCopyState::Missing => "missing",
        SpoolCopyState::Reposted => "reposted",
        SpoolCopyState::Verified => "verified",
    }
}

/// Parses a database copy-state string back into [`SpoolCopyState`].
pub fn parse_spool_copy_state(raw: &str) -> Result<SpoolCopyState, &'static str> {
    match raw {
        "unknown" => Ok(SpoolCopyState::Unknown),
        "present" => Ok(SpoolCopyState::Present),
        "missing" => Ok(SpoolCopyState::Missing),
        "reposted" => Ok(SpoolCopyState::Reposted),
        "verified" => Ok(SpoolCopyState::Verified),
        _ => Err("unknown spool copy state"),
    }
}

/// One full posted-file row: every bound column of the 0035 + 0036 insert.
/// `id` and `created_at` are DB-owned and never bound;
/// `post_verified_at`/`last_retention_check` start NULL and are stamped by
/// the touch statements below.
#[derive(Clone, PartialEq, Eq)]
pub struct PostedFileParams<'a> {
    /// Tagged plaintext sha256, lowercase hex (dedupe key).
    pub plaintext_sha256: &'a str,
    /// First-spool backbone label (pool-config vocabulary, never a host).
    pub backbone_post: &'a str,
    /// Second-spool backbone label; must differ from `backbone_post`.
    pub backbone_verify: &'a str,
    /// Posted article count, 1..=MAX_ARTICLE_COUNT.
    pub article_count: i32,
    /// Articles verified on both backbones yet (starts 0).
    pub verified_count: i32,
    /// Content sha256 of the original NZB bytes, if the poster kept one
    /// for the exhausted-repost path (hash only, never the NZB).
    pub original_nzb_sha256: Option<&'a str>,
    /// First-spool copy state at insert (a fresh post is `Present`).
    pub post_copy: SpoolCopyState,
    /// Second-spool copy state at insert (unchecked: `Unknown`).
    pub verify_copy: SpoolCopyState,
    /// Two-spool verification complete (starts false; verify card flips).
    pub two_spool_verified: bool,
    /// Re-post budget exhausted keeping the original NZB (starts false).
    pub repost_exhausted: bool,
    /// Tagged plaintext bytes (> 0).
    pub plaintext_size: i64,
    /// Cipher bytes per full article, header excluded (> 0).
    pub article_payload_size: i64,
    /// Public 24-byte rclone header nonce.
    pub header_nonce: &'a [u8],
    /// Sealed file-key envelope body (opaque AES-GCM ciphertext).
    pub file_key_encrypted: &'a [u8],
    /// KEK generation that wrapped the file-key DEK (>= 1).
    pub file_key_kek_version: i32,
    /// Nonces/tag bundle of the file-key envelope (exact lengths).
    pub file_key_dek_wrapped_nonce: &'a [u8],
    pub file_key_dek_wrapped: &'a [u8],
    pub file_key_payload_nonce: &'a [u8],
    /// Must be [`FILE_KEY_PAYLOAD_FORMAT`].
    pub file_key_payload_format: i16,
    /// Sealed manifest envelope body (opaque AES-GCM ciphertext).
    pub manifest_encrypted: &'a [u8],
    /// KEK generation that wrapped the manifest DEK (>= 1).
    pub manifest_kek_version: i32,
    /// Nonces/tag bundle of the manifest envelope (exact lengths).
    pub manifest_dek_wrapped_nonce: &'a [u8],
    pub manifest_dek_wrapped: &'a [u8],
    pub manifest_payload_nonce: &'a [u8],
    /// Must be [`MANIFEST_PAYLOAD_FORMAT`].
    pub manifest_payload_format: i16,
}

/// Redacted [`std::fmt::Debug`]: identities, counts, states and byte
/// lengths only. Envelope ciphertext and nonces never render — a debug log
/// must not carry key material or article references.
impl std::fmt::Debug for PostedFileParams<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostedFileParams")
            .field("plaintext_sha256", &self.plaintext_sha256)
            .field("backbone_post", &self.backbone_post)
            .field("backbone_verify", &self.backbone_verify)
            .field("article_count", &self.article_count)
            .field("verified_count", &self.verified_count)
            .field("original_nzb_sha256", &self.original_nzb_sha256)
            .field("post_copy", &spool_copy_state_db_str(self.post_copy))
            .field("verify_copy", &spool_copy_state_db_str(self.verify_copy))
            .field("two_spool_verified", &self.two_spool_verified)
            .field("repost_exhausted", &self.repost_exhausted)
            .field("plaintext_size", &self.plaintext_size)
            .field("article_payload_size", &self.article_payload_size)
            .field("header_nonce_len", &self.header_nonce.len())
            .field("file_key_encrypted_len", &self.file_key_encrypted.len())
            .field("file_key_kek_version", &self.file_key_kek_version)
            .field(
                "file_key_dek_wrapped_nonce_len",
                &self.file_key_dek_wrapped_nonce.len(),
            )
            .field("file_key_dek_wrapped_len", &self.file_key_dek_wrapped.len())
            .field(
                "file_key_payload_nonce_len",
                &self.file_key_payload_nonce.len(),
            )
            .field("file_key_payload_format", &self.file_key_payload_format)
            .field("manifest_encrypted_len", &self.manifest_encrypted.len())
            .field("manifest_kek_version", &self.manifest_kek_version)
            .field(
                "manifest_dek_wrapped_nonce_len",
                &self.manifest_dek_wrapped_nonce.len(),
            )
            .field("manifest_dek_wrapped_len", &self.manifest_dek_wrapped.len())
            .field(
                "manifest_payload_nonce_len",
                &self.manifest_payload_nonce.len(),
            )
            .field("manifest_payload_format", &self.manifest_payload_format)
            .finish()
    }
}

fn validate_envelope_side(
    encrypted: &[u8],
    kek_version: i32,
    wrap_nonce: &[u8],
    wrapped: &[u8],
    payload_nonce: &[u8],
) -> Result<(), &'static str> {
    if encrypted.is_empty() {
        return Err("envelope body is empty");
    }
    if kek_version < 1 {
        return Err("kek version must be >= 1");
    }
    if wrap_nonce.len() != ENVELOPE_NONCE_BYTES {
        return Err("wrap nonce must be 12 bytes");
    }
    if wrapped.len() != ENVELOPE_WRAPPED_DEK_BYTES {
        return Err("wrapped DEK must be 48 bytes");
    }
    if payload_nonce.len() != ENVELOPE_NONCE_BYTES {
        return Err("payload nonce must be 12 bytes");
    }
    Ok(())
}

/// Validates every bound value against the 0035 + 0036 `CHECK`s before the
/// wire-up binds it. Static messages: safe to surface, nothing secret.
pub fn validate_posted_file_params(params: &PostedFileParams) -> Result<(), &'static str> {
    if !validate_plaintext_sha256(params.plaintext_sha256) {
        return Err("plaintext sha256 must be 64 lowercase hex chars");
    }
    if !validate_backbone_label(params.backbone_post)
        || !validate_backbone_label(params.backbone_verify)
    {
        return Err("backbone labels must be 1..=64 [a-z0-9-]");
    }
    if params.backbone_post == params.backbone_verify {
        return Err("verify backbone must differ from the posting backbone");
    }
    if params.article_count < 1 || params.article_count > MAX_ARTICLE_COUNT {
        return Err("article count out of range");
    }
    if params.verified_count < 0 || params.verified_count > MAX_ARTICLE_COUNT {
        return Err("verified count out of range");
    }
    if let Some(sha) = params.original_nzb_sha256 {
        if !validate_plaintext_sha256(sha) {
            return Err("original NZB sha256 must be 64 lowercase hex chars");
        }
    }
    if params.plaintext_size <= 0 {
        return Err("plaintext size must be positive");
    }
    if params.article_payload_size <= 0 {
        return Err("article payload size must be positive");
    }
    if params.header_nonce.len() != HEADER_NONCE_BYTES {
        return Err("header nonce must be 24 bytes");
    }
    validate_envelope_side(
        params.file_key_encrypted,
        params.file_key_kek_version,
        params.file_key_dek_wrapped_nonce,
        params.file_key_dek_wrapped,
        params.file_key_payload_nonce,
    )?;
    // Fail fast at bind time: a mint bug that seals anything but the raw
    // 32-byte key would otherwise surface only at playback.
    if params.file_key_encrypted.len() != FILE_KEY_ENVELOPE_BODY_BYTES {
        return Err("file key envelope body must be 48 bytes");
    }
    if params.file_key_payload_format != FILE_KEY_PAYLOAD_FORMAT {
        return Err("file key payload format must be 1");
    }
    validate_envelope_side(
        params.manifest_encrypted,
        params.manifest_kek_version,
        params.manifest_dek_wrapped_nonce,
        params.manifest_dek_wrapped,
        params.manifest_payload_nonce,
    )?;
    if params.manifest_payload_format != MANIFEST_PAYLOAD_FORMAT {
        return Err("manifest payload format must be 1");
    }
    Ok(())
}

/// Bind order for [`insert_posted_file_sql`]. Envelope versions bind as
/// the SQL literal `2`, so they take no parameter slot.
pub const POSTED_FILE_INSERT_PARAM_ORDER: [&str; 25] = [
    "plaintext_sha256",
    "backbone_post",
    "backbone_verify",
    "article_count",
    "verified_count",
    "original_nzb_sha256",
    "post_copy",
    "verify_copy",
    "two_spool_verified",
    "repost_exhausted",
    "plaintext_size",
    "article_payload_size",
    "header_nonce",
    "file_key_encrypted",
    "file_key_kek_version",
    "file_key_dek_wrapped_nonce",
    "file_key_dek_wrapped",
    "file_key_payload_nonce",
    "file_key_payload_format",
    "manifest_encrypted",
    "manifest_kek_version",
    "manifest_dek_wrapped_nonce",
    "manifest_dek_wrapped",
    "manifest_payload_nonce",
    "manifest_payload_format",
];

/// Full-row insert converging on the sha. Copy states bind as their
/// [`spool_copy_state_db_str`] strings; envelope `*_envelope_version`
/// columns bind the literal `2`.
pub fn insert_posted_file_sql() -> &'static str {
    "INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, \
     article_count, verified_count, original_nzb_sha256, post_copy, verify_copy, \
     two_spool_verified, repost_exhausted, plaintext_size, article_payload_size, \
     header_nonce, file_key_encrypted, file_key_envelope_version, file_key_kek_version, \
     file_key_dek_wrapped_nonce, file_key_dek_wrapped, file_key_payload_nonce, \
     file_key_payload_format, manifest_encrypted, manifest_envelope_version, \
     manifest_kek_version, manifest_dek_wrapped_nonce, manifest_dek_wrapped, \
     manifest_payload_nonce, manifest_payload_format) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 2, $15, \
     $16, $17, $18, $19, $20, 2, $21, $22, $23, $24, $25) \
     ON CONFLICT (plaintext_sha256) DO NOTHING"
}

/// Re-selects the DB-owned id for a sha (dedupe classification).
pub fn select_posted_file_id_by_sha_sql() -> &'static str {
    "SELECT id FROM posted_files WHERE plaintext_sha256 = $1"
}

/// Full-row read for the verify/retention consumers, in documented order.
pub fn select_posted_file_row_sql() -> &'static str {
    "SELECT id, plaintext_sha256, backbone_post, backbone_verify, article_count, \
     verified_count, original_nzb_sha256, post_copy, verify_copy, two_spool_verified, \
     repost_exhausted, plaintext_size, article_payload_size, header_nonce, \
     file_key_encrypted, file_key_envelope_version, file_key_kek_version, \
     file_key_dek_wrapped_nonce, file_key_dek_wrapped, file_key_payload_nonce, \
     file_key_payload_format, manifest_encrypted, manifest_envelope_version, \
     manifest_kek_version, manifest_dek_wrapped_nonce, manifest_dek_wrapped, \
     manifest_payload_nonce, manifest_payload_format, post_verified_at, \
     last_retention_check, created_at \
     FROM posted_files WHERE id = $1"
}

/// Read column order for [`select_posted_file_row_sql`].
pub const POSTED_FILE_COLUMN_ORDER: [&str; 31] = [
    "id",
    "plaintext_sha256",
    "backbone_post",
    "backbone_verify",
    "article_count",
    "verified_count",
    "original_nzb_sha256",
    "post_copy",
    "verify_copy",
    "two_spool_verified",
    "repost_exhausted",
    "plaintext_size",
    "article_payload_size",
    "header_nonce",
    "file_key_encrypted",
    "file_key_envelope_version",
    "file_key_kek_version",
    "file_key_dek_wrapped_nonce",
    "file_key_dek_wrapped",
    "file_key_payload_nonce",
    "file_key_payload_format",
    "manifest_encrypted",
    "manifest_envelope_version",
    "manifest_kek_version",
    "manifest_dek_wrapped_nonce",
    "manifest_dek_wrapped",
    "manifest_payload_nonce",
    "manifest_payload_format",
    "post_verified_at",
    "last_retention_check",
    "created_at",
];

/// Stamps both verify timestamps at once: first verification wins for
/// `post_verified_at`, while `last_retention_check` always advances.
pub fn touch_post_verified_sql() -> &'static str {
    "UPDATE posted_files SET post_verified_at = COALESCE(post_verified_at, $2), \
     last_retention_check = $2 WHERE id = $1"
}

/// Advances the retention-monitor timestamp without touching verification.
pub fn touch_retention_check_sql() -> &'static str {
    "UPDATE posted_files SET last_retention_check = $2 WHERE id = $1"
}

/// Bind order for both touch statements.
pub const POSTED_FILE_TOUCH_PARAM_ORDER: [&str; 2] = ["id", "checked_at"];

/// What a zero-row insert means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostedFileWriteOutcome {
    /// The insert landed one row.
    Written,
    /// The sha already has a row (benign replay); the re-select found it.
    Duplicate,
    /// Neither landed nor found: fail closed, the wire-up must surface it.
    Inconsistent,
}

/// Classifies a zero-row insert: `inserted` is the `ON CONFLICT DO NOTHING`
/// row count, `existing_id` the sha re-select result.
pub fn classify_posted_file_zero_rows(
    inserted: u64,
    existing_id: Option<&str>,
) -> PostedFileWriteOutcome {
    if inserted == 1 {
        return PostedFileWriteOutcome::Written;
    }
    match existing_id {
        Some(_) if inserted == 0 => PostedFileWriteOutcome::Duplicate,
        _ => PostedFileWriteOutcome::Inconsistent,
    }
}

/// Validates an epoch timestamp for the touch statements.
pub fn validate_checked_at(checked_at: i64) -> Result<(), &'static str> {
    if !(0..=MAX_EPOCH).contains(&checked_at) {
        return Err("checked_at out of epoch range");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_params() -> PostedFileParams<'static> {
        PostedFileParams {
            plaintext_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            backbone_post: "spool-post",
            backbone_verify: "spool-verify",
            article_count: 11,
            verified_count: 0,
            original_nzb_sha256: None,
            post_copy: SpoolCopyState::Present,
            verify_copy: SpoolCopyState::Unknown,
            two_spool_verified: false,
            repost_exhausted: false,
            plaintext_size: 720_896,
            article_payload_size: 721_072,
            header_nonce: &[0u8; HEADER_NONCE_BYTES],
            file_key_encrypted: &[1u8; FILE_KEY_ENVELOPE_BODY_BYTES],
            file_key_kek_version: 1,
            file_key_dek_wrapped_nonce: &[2u8; ENVELOPE_NONCE_BYTES],
            file_key_dek_wrapped: &[3u8; ENVELOPE_WRAPPED_DEK_BYTES],
            file_key_payload_nonce: &[4u8; ENVELOPE_NONCE_BYTES],
            file_key_payload_format: FILE_KEY_PAYLOAD_FORMAT,
            manifest_encrypted: &[5u8; 128],
            manifest_kek_version: 2,
            manifest_dek_wrapped_nonce: &[6u8; ENVELOPE_NONCE_BYTES],
            manifest_dek_wrapped: &[7u8; ENVELOPE_WRAPPED_DEK_BYTES],
            manifest_payload_nonce: &[8u8; ENVELOPE_NONCE_BYTES],
            manifest_payload_format: MANIFEST_PAYLOAD_FORMAT,
        }
    }

    #[test]
    fn valid_params_pass() {
        assert!(validate_posted_file_params(&valid_params()).is_ok());
    }

    #[test]
    fn rejects_bad_dedupe_key_and_backbones() {
        let mut p = valid_params();
        p.plaintext_sha256 = "AAAA";
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.backbone_post = "news.Example.com";
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.backbone_verify = "spool-post";
        assert!(validate_posted_file_params(&p).is_err());
    }

    #[test]
    fn rejects_bad_counts_and_sizes() {
        let mut p = valid_params();
        p.article_count = 0;
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.verified_count = -1;
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.plaintext_size = 0;
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.article_payload_size = -7;
        assert!(validate_posted_file_params(&p).is_err());
    }

    #[test]
    fn rejects_bad_envelope_material() {
        let mut p = valid_params();
        p.header_nonce = &[0u8; 12];
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.file_key_encrypted = &[];
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.file_key_kek_version = 0;
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.file_key_dek_wrapped = &[0u8; 16];
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.file_key_payload_format = 2;
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.manifest_payload_nonce = &[0u8; 16];
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.manifest_payload_format = 0;
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.original_nzb_sha256 = Some("short");
        assert!(validate_posted_file_params(&p).is_err());
    }

    #[test]
    fn envelope_aad_tuple_is_pinned() {
        // The minting sibling binds exactly this tuple (see the module
        // header); a rename here without a mint-side change breaks decrypt.
        assert_eq!(ENVELOPE_AAD_DOMAIN, "nntune-envelope-v2");
        assert_eq!(ENVELOPE_AAD_TABLE, "posted_files");
        assert_eq!(FILE_KEY_AAD_FIELD, "file_key");
        assert_eq!(MANIFEST_AAD_FIELD, "manifest");
        assert_ne!(FILE_KEY_AAD_FIELD, MANIFEST_AAD_FIELD);
        assert_eq!(ENVELOPE_AAD_OWNER_SCOPE, "global:posted_files");
        assert_eq!(FILE_KEY_ENVELOPE_BODY_BYTES, 48);
    }

    #[test]
    fn file_key_envelope_body_is_exactly_48_bytes() {
        let mut p = valid_params();
        p.file_key_encrypted = &[0u8; FILE_KEY_ENVELOPE_BODY_BYTES - 1];
        assert!(validate_posted_file_params(&p).is_err());
        let mut p = valid_params();
        p.file_key_encrypted = &[0u8; FILE_KEY_ENVELOPE_BODY_BYTES + 1];
        assert!(validate_posted_file_params(&p).is_err());
        // The manifest envelope carries a variable-length JSON document, so
        // any non-empty body validates; only the file-key side is exact.
        let mut p = valid_params();
        p.manifest_encrypted = &[0u8; 1];
        assert!(validate_posted_file_params(&p).is_ok());
    }

    #[test]
    fn copy_state_strings_match_the_schema_list() {
        assert_eq!(spool_copy_state_db_str(SpoolCopyState::Unknown), "unknown");
        assert_eq!(spool_copy_state_db_str(SpoolCopyState::Present), "present");
        assert_eq!(spool_copy_state_db_str(SpoolCopyState::Missing), "missing");
        assert_eq!(
            spool_copy_state_db_str(SpoolCopyState::Reposted),
            "reposted"
        );
        assert_eq!(
            spool_copy_state_db_str(SpoolCopyState::Verified),
            "verified"
        );
        for state in [
            SpoolCopyState::Unknown,
            SpoolCopyState::Present,
            SpoolCopyState::Missing,
            SpoolCopyState::Reposted,
            SpoolCopyState::Verified,
        ] {
            assert_eq!(
                parse_spool_copy_state(spool_copy_state_db_str(state)),
                Ok(state)
            );
        }
        assert!(parse_spool_copy_state("gone").is_err());
    }

    #[test]
    fn insert_sql_shape() {
        let sql = insert_posted_file_sql();
        assert!(sql.contains("ON CONFLICT (plaintext_sha256) DO NOTHING"));
        // Envelope versions bind as the literal 2 on both sides.
        assert!(sql.contains("$14, 2, $15"));
        assert!(sql.contains("$20, 2, $21"));
        assert_eq!(POSTED_FILE_INSERT_PARAM_ORDER.len(), 25);
        for (i, name) in POSTED_FILE_INSERT_PARAM_ORDER.iter().enumerate() {
            let slot = format!("${}", i + 1);
            assert!(sql.contains(&slot), "missing slot for {name}");
        }
    }

    #[test]
    fn row_select_lists_columns_in_order() {
        let sql = select_posted_file_row_sql();
        assert_eq!(POSTED_FILE_COLUMN_ORDER.len(), 31);
        let mut cursor = 0;
        for name in POSTED_FILE_COLUMN_ORDER {
            let rest = &sql[cursor..];
            let pos = rest
                .find(name)
                .unwrap_or_else(|| panic!("missing column {name}"));
            cursor += pos + name.len();
        }
    }

    #[test]
    fn touch_sql_and_epoch_validation() {
        assert!(touch_post_verified_sql().contains("COALESCE(post_verified_at, $2)"));
        assert!(touch_retention_check_sql().contains("last_retention_check = $2"));
        assert!(validate_checked_at(1_800_000_000).is_ok());
        assert!(validate_checked_at(-1).is_err());
        assert!(validate_checked_at(MAX_EPOCH + 1).is_err());
    }

    #[test]
    fn classifies_zero_rows_fail_closed() {
        assert_eq!(
            classify_posted_file_zero_rows(1, None),
            PostedFileWriteOutcome::Written
        );
        assert_eq!(
            classify_posted_file_zero_rows(0, Some("id")),
            PostedFileWriteOutcome::Duplicate
        );
        assert_eq!(
            classify_posted_file_zero_rows(0, None),
            PostedFileWriteOutcome::Inconsistent
        );
        assert_eq!(
            classify_posted_file_zero_rows(2, Some("id")),
            PostedFileWriteOutcome::Inconsistent
        );
    }

    #[test]
    fn debug_redacts_envelope_bytes() {
        // Marker material that must never appear in a debug log line.
        let marker = b"MARKER-message-id-like-secret-should-not-render";
        let mut p = valid_params();
        p.file_key_encrypted = marker;
        p.manifest_encrypted = marker;
        let rendered = format!("{:?}", p);
        assert!(!rendered.contains("MARKER"));
        assert!(rendered.contains("file_key_encrypted_len"));
        assert!(rendered.contains(p.plaintext_sha256));
    }
}
