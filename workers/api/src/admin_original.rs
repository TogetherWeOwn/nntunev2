//! Internal durable admin-original reader, not a public route or a queue
//! admission authority. The API/keyring opens the existing envelope; neither
//! KEKs nor DEKs cross this seam. Callers must bound the whole operation with
//! their database deadline. Live dispatch remains disabled until publish gates
//! and the ingest-job-fenced writers are integrated.

use std::future::Future;
use zeroize::Zeroizing;

use crate::{admin_ingest as intake, rotation::KekSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginalError {
    Denied,
    Missing,
    Invalid,
    UnknownKey,
    Crypto,
    Store,
}

/// No payload-bearing field is selected until these bounded lengths and the
/// active admin-job identity have been checked. No Debug/Serialize on metadata
/// or plaintext: they must not become a response or diagnostic dump.
#[derive(Clone)]
pub struct OriginalMetadata {
    pub job_id: String,
    pub content_sha256: String,
    pub nzb_bytes: i64,
    pub envelope_version: i16,
    pub payload_format: i16,
    pub kek_version: i32,
    pub ciphertext_len: i32,
    pub wrap_nonce_len: i32,
    pub wrapped_dek_len: i32,
    pub payload_nonce_len: i32,
}

impl OriginalMetadata {
    fn validate(&self, job_id: &str) -> Result<u32, OriginalError> {
        if self.job_id != job_id
            || job_id.len() != 47
            || !job_id.starts_with("adm_")
            || !job_id[4..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || self.content_sha256.len() != 64
            || !self
                .content_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !(1..=intake::ADMIN_INGEST_BODY_LIMIT_BYTES as i64).contains(&self.nzb_bytes)
            || self.ciphertext_len as i64 != self.nzb_bytes + 16
            || self.envelope_version != intake::ENVELOPE_VERSION
            || self.payload_format != intake::PAYLOAD_FORMAT_RAW_NZB_V1
            || self.wrap_nonce_len != 12
            || self.wrapped_dek_len != 48
            || self.payload_nonce_len != 12
        {
            return Err(OriginalError::Invalid);
        }
        u32::try_from(self.kek_version).map_err(|_| OriginalError::Invalid)
    }
}

pub fn original_metadata_sql() -> &'static str {
    "SELECT o.job_id, o.content_sha256, o.nzb_bytes, o.nzb_ref_envelope_version, o.nzb_ref_payload_format, o.nzb_ref_kek_version, octet_length(o.nzb_ref_encrypted) AS ciphertext_len, octet_length(o.nzb_ref_dek_wrapped_nonce) AS wrap_nonce_len, octet_length(o.nzb_ref_dek_wrapped) AS wrapped_dek_len, octet_length(o.nzb_ref_payload_nonce) AS payload_nonce_len FROM ingest_originals o JOIN ingest_jobs j ON j.id = o.job_id WHERE j.id = $1 AND j.source = 'admin' AND j.status IN ('queued', 'running') AND j.content_sha256 = o.content_sha256 AND j.idempotency_key = 'admin:' || o.content_sha256 AND j.nzb_bytes = o.nzb_bytes AND octet_length(o.content_sha256) = 64 LIMIT 2"
}

/// Bind job, content hash, stored KEK version, and plaintext byte count. The
/// predicates are repeated at byte loading and running persistence: deletion,
/// settlement or rotation between operations fails closed, not onto a new row.
pub fn original_envelope_sql() -> &'static str {
    "SELECT o.nzb_ref_dek_wrapped_nonce, o.nzb_ref_dek_wrapped, o.nzb_ref_payload_nonce, o.nzb_ref_encrypted FROM ingest_originals o JOIN ingest_jobs j ON j.id = o.job_id WHERE j.id = $1 AND j.source = 'admin' AND j.status IN ('queued', 'running') AND j.content_sha256 = $2 AND j.idempotency_key = 'admin:' || $2 AND j.nzb_bytes = $4 AND o.content_sha256 = $2 AND o.nzb_ref_kek_version = $3 AND o.nzb_bytes = $4 AND o.nzb_ref_envelope_version = 2 AND o.nzb_ref_payload_format = 1 AND octet_length(o.nzb_ref_encrypted) = $4 + 16 AND octet_length(o.nzb_ref_encrypted) BETWEEN 17 AND 8388624 AND octet_length(o.nzb_ref_dek_wrapped_nonce) = 12 AND octet_length(o.nzb_ref_dek_wrapped) = 48 AND octet_length(o.nzb_ref_payload_nonce) = 12 LIMIT 1"
}

pub fn mark_running_sql() -> &'static str {
    "UPDATE ingest_jobs j SET status = 'running' WHERE j.id = $1 AND j.source = 'admin' AND j.status IN ('queued', 'running') AND j.content_sha256 = $2 AND j.idempotency_key = 'admin:' || $2 AND j.nzb_bytes = $4 AND EXISTS (SELECT 1 FROM ingest_originals o WHERE o.job_id = j.id AND o.content_sha256 = $2 AND o.nzb_ref_kek_version = $3 AND o.nzb_bytes = $4 AND o.nzb_ref_envelope_version = 2 AND o.nzb_ref_payload_format = 1 AND octet_length(o.nzb_ref_encrypted) = $4 + 16 AND octet_length(o.nzb_ref_encrypted) BETWEEN 17 AND 8388624 AND octet_length(o.nzb_ref_dek_wrapped_nonce) = 12 AND octet_length(o.nzb_ref_dek_wrapped) = 48 AND octet_length(o.nzb_ref_payload_nonce) = 12)"
}

/// Implemented by an authenticated internal execution authority, never by a
/// value decoded from a public request or from a self-asserted queue source.
pub trait OriginalAuthorization {
    fn authorize(&self, job_id: &str) -> Result<(), OriginalError>;
}

pub trait OriginalRepository {
    fn metadata(
        &mut self,
        job_id: &str,
    ) -> impl Future<Output = Result<Vec<OriginalMetadata>, OriginalError>>;
    fn envelope(
        &mut self,
        selected: &OriginalMetadata,
    ) -> impl Future<Output = Result<Option<intake::IngestOriginalEnvelope>, OriginalError>>;
    /// Returns true only when exactly one active, identity-matching admin row
    /// was updated. This is a status projection, not an exclusive job lease.
    fn mark_running(
        &mut self,
        selected: &OriginalMetadata,
    ) -> impl Future<Output = Result<bool, OriginalError>>;
}

/// Plaintext exists only in this non-debuggable, non-serializable zeroizing
/// holder. The next parser stage borrows it; no payload is checkpointed here.
pub struct OriginalBody(Zeroizing<Vec<u8>>);

impl OriginalBody {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

pub async fn read_original(
    authorization: &impl OriginalAuthorization,
    repository: &mut impl OriginalRepository,
    keks: &impl KekSource,
    job_id: &str,
) -> Result<OriginalBody, OriginalError> {
    authorization.authorize(job_id)?;
    // Reject unbounded request identities before querying the database.
    if job_id.len() != 47 || !job_id.starts_with("adm_") || !job_id.is_ascii() {
        return Err(OriginalError::Invalid);
    }
    let mut rows = repository.metadata(job_id).await?;
    if rows.len() > 1 {
        return Err(OriginalError::Invalid);
    }
    let selected = rows.pop().ok_or(OriginalError::Missing)?;
    let version = selected.validate(job_id)?;
    let kek = keks.kek(version).ok_or(OriginalError::UnknownKey)?;
    let envelope = repository
        .envelope(&selected)
        .await?
        .ok_or(OriginalError::Missing)?;
    if envelope.ciphertext.len() != selected.ciphertext_len as usize
        || envelope.dek_wrapped.len() != intake::WRAPPED_DEK_LEN
    {
        return Err(OriginalError::Invalid);
    }
    let body = Zeroizing::new(
        intake::decrypt_ingest_original(
            kek,
            &selected.content_sha256,
            job_id,
            selected.envelope_version,
            selected.payload_format,
            version,
            &envelope,
        )
        .map_err(|_| OriginalError::Crypto)?,
    );
    if body.len() as i64 != selected.nzb_bytes
        || intake::content_sha256_hex(&body) != selected.content_sha256
        || intake::derive_job_id(&body) != job_id
    {
        return Err(OriginalError::Invalid);
    }
    intake::classify_upload(&body).map_err(|_| OriginalError::Invalid)?;
    if !repository.mark_running(&selected).await? {
        return Err(OriginalError::Missing);
    }
    Ok(OriginalBody(body))
}

/// The caller lends its deadline-bounded application connection. No keyring,
/// connection string, envelope or driver error is logged by this adapter.
pub struct PostgresOriginalRepository<'a> {
    pub client: &'a tokio_postgres::Client,
}

impl OriginalRepository for PostgresOriginalRepository<'_> {
    async fn metadata(&mut self, job_id: &str) -> Result<Vec<OriginalMetadata>, OriginalError> {
        self.client
            .query(original_metadata_sql(), &[&job_id])
            .await
            .map_err(|_| OriginalError::Store)?
            .iter()
            .map(|r| {
                Ok(OriginalMetadata {
                    job_id: r.try_get("job_id").map_err(|_| OriginalError::Invalid)?,
                    content_sha256: r
                        .try_get("content_sha256")
                        .map_err(|_| OriginalError::Invalid)?,
                    nzb_bytes: r.try_get("nzb_bytes").map_err(|_| OriginalError::Invalid)?,
                    envelope_version: r
                        .try_get("nzb_ref_envelope_version")
                        .map_err(|_| OriginalError::Invalid)?,
                    payload_format: r
                        .try_get("nzb_ref_payload_format")
                        .map_err(|_| OriginalError::Invalid)?,
                    kek_version: r
                        .try_get("nzb_ref_kek_version")
                        .map_err(|_| OriginalError::Invalid)?,
                    ciphertext_len: r
                        .try_get("ciphertext_len")
                        .map_err(|_| OriginalError::Invalid)?,
                    wrap_nonce_len: r
                        .try_get("wrap_nonce_len")
                        .map_err(|_| OriginalError::Invalid)?,
                    wrapped_dek_len: r
                        .try_get("wrapped_dek_len")
                        .map_err(|_| OriginalError::Invalid)?,
                    payload_nonce_len: r
                        .try_get("payload_nonce_len")
                        .map_err(|_| OriginalError::Invalid)?,
                })
            })
            .collect()
    }

    async fn envelope(
        &mut self,
        s: &OriginalMetadata,
    ) -> Result<Option<intake::IngestOriginalEnvelope>, OriginalError> {
        let rows = self
            .client
            .query(
                original_envelope_sql(),
                &[&s.job_id, &s.content_sha256, &s.kek_version, &s.nzb_bytes],
            )
            .await
            .map_err(|_| OriginalError::Store)?;
        let Some(r) = rows.first() else {
            return Ok(None);
        };
        let nonce = |column| -> Result<[u8; 12], OriginalError> {
            let bytes: Vec<u8> = r.try_get(column).map_err(|_| OriginalError::Invalid)?;
            bytes.try_into().map_err(|_| OriginalError::Invalid)
        };
        Ok(Some(intake::IngestOriginalEnvelope {
            dek_wrapped_nonce: nonce("nzb_ref_dek_wrapped_nonce")?,
            dek_wrapped: r
                .try_get("nzb_ref_dek_wrapped")
                .map_err(|_| OriginalError::Invalid)?,
            payload_nonce: nonce("nzb_ref_payload_nonce")?,
            ciphertext: r
                .try_get("nzb_ref_encrypted")
                .map_err(|_| OriginalError::Invalid)?,
        }))
    }

    async fn mark_running(&mut self, s: &OriginalMetadata) -> Result<bool, OriginalError> {
        self.client
            .execute(
                mark_running_sql(),
                &[&s.job_id, &s.content_sha256, &s.kek_version, &s.nzb_bytes],
            )
            .await
            .map(|n| n == 1)
            .map_err(|_| OriginalError::Store)
    }
}

#[cfg(test)]
#[path = "admin_original_tests.rs"]
mod tests;
