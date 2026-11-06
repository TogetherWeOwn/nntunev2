//! Concrete API/keyring-owned sealed preparation-plan persistence (migration 0048).
//! This internal service has no HTTP route or Queue entry. Preparation remains
//! default-off; opaque input bytes must come from the trusted canonical planner,
//! not a public request or Queue payload. This does not implement that planner.
//!
//! Only encrypted DB envelopes are stored. A stable job/operation/file identity
//! reconciles a lost commit reply without reminting a surviving envelope. Reads
//! require the exact stored version and original admitted owner/input; there is
//! no latest-version fallback. Both paths hold admission/deletion locks, never
//! storage or Queue IO. Opened bytes wipe on drop and are never serialized.
//! Prior KEKs remain necessary until a separately reviewed wrap-only rotation
//! adapter exists; installing this table does not authorize key retirement.

use crate::{
    field_envelope::{self, Context, FieldEnvelope},
    rotation::KekSource,
};
use sha2::{Digest, Sha256};
use std::fmt;
use tokio_postgres::{Client, Row, Transaction};
use zeroize::Zeroizing;

pub const MAX_PREPARATION_PLAN_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlanMode {
    #[default]
    Disabled,
    PreparationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    Invalid,
    Store,
    CommitUnknown,
    UnknownKey,
    Crypto,
    Conflict,
}

/// Expected original admission, supplied by the trusted internal producer.
/// Shape validation is NOT authentication or a grant to start/acquire work.
/// No Debug/serialization: identities and plan metadata are not log material.
pub struct PlanRequest<'a> {
    pub owner_id: &'a str,
    pub job_id: &'a str,
    pub operation_key: &'a str,
    pub file_index: i32,
    pub original_sha256: &'a str,
    pub original_bytes: i64,
    pub file_count: i32,
}

impl PlanRequest<'_> {
    fn id(&self) -> Result<String, PlanError> {
        if !atom(self.owner_id)
            || !atom(self.job_id)
            || !atom(self.operation_key)
            || self.file_index < 0
            || self.file_index >= self.file_count
            || self.original_bytes <= 0
            || self.original_sha256.len() != 64
            || !self
                .original_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(PlanError::Invalid);
        }
        let mut hash = Sha256::new();
        hash.update(b"nntune-preparation-plan-v1");
        for value in [self.job_id, self.operation_key] {
            hash.update((value.len() as u32).to_be_bytes());
            hash.update(value.as_bytes());
        }
        hash.update(self.file_index.to_be_bytes());
        Ok(hex(&hash.finalize()))
    }
}

fn atom(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Exact immutable storage reference plus authenticated plaintext for the
/// dispatcher's byte-derived digest. No KEK/DEK escapes the API boundary.
pub struct OpenedPlan {
    id: String,
    version: String,
    bytes: Zeroizing<Vec<u8>>,
}

impl OpenedPlan {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for OpenedPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpenedPlan([REDACTED])")
    }
}

fn context<'a>(id: &'a str, owner: &'a str) -> Context<'a> {
    Context {
        table: "ingest_preparation_plans",
        field: "plan",
        scope: owner,
        row: id,
        version: 2,
        format: 1,
    }
}

// Same admission -> owner -> job -> original ordering as the dispatch adapter.
async fn admitted(tx: &Transaction<'_>, input: &PlanRequest<'_>) -> Result<bool, PlanError> {
    tx.query_one(
        "SELECT pg_advisory_xact_lock(hashtext('admin_writes'))",
        &[],
    )
    .await
    .map_err(|_| PlanError::Store)?;
    if tx.query_opt(
        "SELECT id FROM users WHERE id = $1 AND role = 'admin' AND deleting_at IS NULL FOR SHARE",
        &[&input.owner_id],
    ).await.map_err(|_| PlanError::Store)?.is_none() { return Ok(false) }
    let admission = format!("admin:{}", input.original_sha256);
    if tx
        .query_opt(
            "SELECT id FROM ingest_jobs WHERE id = $1 AND user_id = $2 AND source = 'admin' \
         AND idempotency_key = $3 AND content_sha256 = $4 AND nzb_bytes = $5 AND file_count = $6 \
         AND status IN ('queued', 'running') AND failure_reason IS NULL FOR UPDATE",
            &[
                &input.job_id,
                &input.owner_id,
                &admission,
                &input.original_sha256,
                &input.original_bytes,
                &input.file_count,
            ],
        )
        .await
        .map_err(|_| PlanError::Store)?
        .is_none()
    {
        return Ok(false);
    }
    Ok(tx.query_opt(
        "SELECT job_id FROM ingest_originals WHERE job_id = $1 AND content_sha256 = $2 \
         AND nzb_bytes = $3 AND nzb_ref_envelope_version = 2 AND nzb_ref_payload_format = 1 FOR SHARE",
        &[&input.job_id, &input.original_sha256, &input.original_bytes],
    ).await.map_err(|_| PlanError::Store)?.is_some())
}

// A bounded projection first: corrupt stored BYTEA never reaches the client
// until every length and expected relationship has been checked in SQL/Rust.
async fn load(
    tx: &Transaction<'_>,
    keks: &impl KekSource,
    input: &PlanRequest<'_>,
    id: &str,
) -> Result<Option<OpenedPlan>, PlanError> {
    let Some(meta) = tx.query_opt(
        "SELECT CASE WHEN octet_length(version) = 32 THEN version END AS version, \
         plaintext_bytes, CASE WHEN octet_length(plaintext_sha256) = 64 THEN plaintext_sha256 END AS digest, \
         envelope_version, payload_format, kek_version, octet_length(encrypted) AS encrypted_len, \
         octet_length(dek_wrapped_nonce) AS wrap_nonce_len, octet_length(dek_wrapped) AS wrap_len, \
         octet_length(payload_nonce) AS payload_nonce_len FROM ingest_preparation_plans \
         WHERE id = $1 AND job_id = $2 AND operation_key = $3 AND file_index = $4 \
         AND owner_id = $5 AND admission_key = $6 AND original_sha256 = $7 \
         AND original_bytes = $8 AND file_count = $9 FOR SHARE",
        &[&id, &input.job_id, &input.operation_key, &input.file_index, &input.owner_id,
          &format!("admin:{}", input.original_sha256), &input.original_sha256,
          &input.original_bytes, &input.file_count],
    ).await.map_err(|_| PlanError::Store)? else { return Ok(None) };
    let version: Option<String> = meta.get("version");
    let version = version
        .filter(|s| {
            s.len() == 32
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or(PlanError::Invalid)?;
    let size: i32 = meta.get("plaintext_bytes");
    let stored_kek: i32 = meta.get("kek_version");
    let kek_version = u32::try_from(stored_kek).map_err(|_| PlanError::Invalid)?;
    if !(1..=MAX_PREPARATION_PLAN_BYTES as i32).contains(&size)
        || meta.get::<_, i16>("envelope_version") != 2
        || meta.get::<_, i16>("payload_format") != 1
        || meta.get::<_, i32>("encrypted_len") != size + 16
        || meta.get::<_, i32>("wrap_nonce_len") != 12
        || meta.get::<_, i32>("wrap_len") != 48
        || meta.get::<_, i32>("payload_nonce_len") != 12
    {
        return Err(PlanError::Invalid);
    }
    let digest: Option<String> = meta.get("digest");
    let digest = digest.ok_or(PlanError::Invalid)?;
    let kek = keks.kek(kek_version).ok_or(PlanError::UnknownKey)?;
    let row = tx.query_opt(
        "SELECT dek_wrapped_nonce, dek_wrapped, payload_nonce, encrypted FROM ingest_preparation_plans \
         WHERE id = $1 AND version = $2 AND kek_version = $3 AND envelope_version = 2 AND payload_format = 1 \
         AND octet_length(encrypted) = $4 AND octet_length(encrypted) BETWEEN 17 AND 1048592 \
         AND octet_length(dek_wrapped_nonce) = 12 AND octet_length(dek_wrapped) = 48 \
         AND octet_length(payload_nonce) = 12",
        &[&id, &version, &stored_kek, &(size + 16)],
    ).await.map_err(|_| PlanError::Store)?.ok_or(PlanError::Conflict)?;
    let envelope = envelope(&row)?;
    let bytes = Zeroizing::new(
        field_envelope::open(kek, kek_version, &context(id, input.owner_id), &envelope)
            .map_err(|_| PlanError::Crypto)?,
    );
    if bytes.len() != size as usize || hex(&Sha256::digest(bytes.as_slice())) != digest {
        return Err(PlanError::Invalid);
    }
    Ok(Some(OpenedPlan {
        id: id.to_owned(),
        version,
        bytes,
    }))
}

fn envelope(row: &Row) -> Result<FieldEnvelope, PlanError> {
    Ok(FieldEnvelope {
        dek_wrapped_nonce: row
            .get::<_, Vec<u8>>("dek_wrapped_nonce")
            .try_into()
            .map_err(|_| PlanError::Invalid)?,
        dek_wrapped: row.get("dek_wrapped"),
        payload_nonce: row
            .get::<_, Vec<u8>>("payload_nonce")
            .try_into()
            .map_err(|_| PlanError::Invalid)?,
        ciphertext: row.get("encrypted"),
    })
}

/// Encrypt and durably freeze one canonical plan. Exact replay opens the stored
/// envelope with its pinned KEK; it never overwrites, reseals or changes version.
/// A commit error is ambiguous: retry the same input/operation, never delete it.
pub async fn freeze_plan(
    client: &mut Client,
    mode: PlanMode,
    keks: &impl KekSource,
    kek_version: u32,
    input: &PlanRequest<'_>,
    canonical_plan: &[u8],
) -> Result<Option<OpenedPlan>, PlanError> {
    if mode == PlanMode::Disabled {
        return Ok(None);
    }
    let id = input.id()?;
    if canonical_plan.is_empty() || canonical_plan.len() > MAX_PREPARATION_PLAN_BYTES {
        return Err(PlanError::Invalid);
    }
    let tx = client.transaction().await.map_err(|_| PlanError::Store)?;
    if !admitted(&tx, input).await? {
        return Ok(None);
    }
    if let Some(existing) = load(&tx, keks, input, &id).await? {
        if existing.bytes() != canonical_plan {
            return Err(PlanError::Conflict);
        }
        tx.commit().await.map_err(|_| PlanError::CommitUnknown)?;
        return Ok(Some(existing));
    }
    let stored_kek = i32::try_from(kek_version).map_err(|_| PlanError::Invalid)?;
    let kek = keks.kek(kek_version).ok_or(PlanError::UnknownKey)?;
    let sealed = field_envelope::seal(
        kek,
        kek_version,
        &context(&id, input.owner_id),
        canonical_plan,
    )
    .map_err(|_| PlanError::Crypto)?;
    let version = hex(&crate::random::secure_random_bytes::<16>().map_err(|_| PlanError::Crypto)?);
    let size = canonical_plan.len() as i32;
    let digest = hex(&Sha256::digest(canonical_plan));
    let inserted = tx.execute(
        "INSERT INTO ingest_preparation_plans (id, version, job_id, operation_key, file_index, owner_id, \
         admission_key, original_sha256, original_bytes, file_count, plaintext_bytes, plaintext_sha256, \
         envelope_version, payload_format, kek_version, dek_wrapped_nonce, dek_wrapped, payload_nonce, encrypted) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,2,1,$13,$14,$15,$16,$17) ON CONFLICT DO NOTHING",
        &[&id, &version, &input.job_id, &input.operation_key, &input.file_index, &input.owner_id,
          &format!("admin:{}", input.original_sha256), &input.original_sha256, &input.original_bytes,
          &input.file_count, &size, &digest, &stored_kek, &&sealed.dek_wrapped_nonce[..],
          &sealed.dek_wrapped, &&sealed.payload_nonce[..], &sealed.ciphertext],
    ).await.map_err(|_| PlanError::Store)?;
    // Job/admission serialization excludes another conforming writer. A
    // conflicting row with different provenance is never silently adopted.
    if inserted != 1 {
        return Err(PlanError::Conflict);
    }
    let opened = load(&tx, keks, input, &id)
        .await?
        .ok_or(PlanError::Conflict)?;
    tx.commit().await.map_err(|_| PlanError::CommitUnknown)?;
    Ok(Some(opened))
}

#[cfg(test)]
#[path = "ingest_plan_tests.rs"]
mod tests;

/// Resolve only the exact immutable reference under the original admission.
/// This is not token/attempt recovery, startup, renewal or POST authority.
pub async fn read_plan(
    client: &mut Client,
    mode: PlanMode,
    keks: &impl KekSource,
    input: &PlanRequest<'_>,
    expected_id: &str,
    expected_version: &str,
) -> Result<Option<OpenedPlan>, PlanError> {
    if mode == PlanMode::Disabled {
        return Ok(None);
    }
    let id = input.id()?;
    if id != expected_id || !atom(expected_version) {
        return Err(PlanError::Invalid);
    }
    let tx = client.transaction().await.map_err(|_| PlanError::Store)?;
    if !admitted(&tx, input).await? {
        return Ok(None);
    }
    let Some(opened) = load(&tx, keks, input, &id).await? else {
        return Ok(None);
    };
    if opened.version() != expected_version {
        return Ok(None);
    }
    tx.commit().await.map_err(|_| PlanError::CommitUnknown)?;
    Ok(Some(opened))
}
