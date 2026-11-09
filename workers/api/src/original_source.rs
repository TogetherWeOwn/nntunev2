//! Internal original-source service seam, not an HTTP handler. The selected
//! shared track row supplies expected AAD; only this API/keyring authority
//! seals/opens. No KEK or DEK crosses the ingest/stream seam.

use nntune_core::original_source::{
    canonical_track_id, valid_source_job, OriginalSource, MAX_CIPHERTEXT_BYTES, PAYLOAD_FORMAT,
};
use zeroize::Zeroizing;

pub use crate::field_envelope::FieldEnvelope;

use crate::{
    field_envelope::{self, Context},
    rotation::KekSource,
};

pub const ENVELOPE_VERSION: i16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceServiceError {
    Denied,
    Missing,
    Invalid,
    UnknownKey,
    Store,
    Crypto,
}

fn context(track_id: &str) -> Context<'_> {
    Context {
        table: "track_sources",
        field: "nzb_ref",
        scope: "shared:track-source",
        row: track_id,
        version: 2,
        format: PAYLOAD_FORMAT as u16,
    }
}

/// Safe bounded projection, with no ciphertext. Store adapters must never
/// fetch BYTEA (or base64/JSON encode it) until this shape passes validation.
/// INTEGER and octet_length(BYTEA) are PostgreSQL int4 (Rust i32), including
/// the selected KEK/length parameters bound by source_envelope_sql.
#[derive(Clone)]
pub struct SourceMetadata {
    pub track_id: String,
    pub source_job_id: String,
    pub envelope_version: i16,
    pub payload_format: i16,
    pub kek_version: i32,
    pub ciphertext_len: i32,
    pub wrap_nonce_len: i32,
    pub wrapped_dek_len: i32,
    pub payload_nonce_len: i32,
}

impl SourceMetadata {
    fn validate(&self, track_id: &str) -> Result<u32, SourceServiceError> {
        if self.track_id != track_id
            || !canonical_track_id(track_id)
            || !valid_source_job(&self.source_job_id)
            || self.envelope_version != ENVELOPE_VERSION
            || self.payload_format != PAYLOAD_FORMAT
            || !(17..=MAX_CIPHERTEXT_BYTES as i32).contains(&self.ciphertext_len)
            || self.wrap_nonce_len != 12
            || self.wrapped_dek_len != 48
            || self.payload_nonce_len != 12
        {
            return Err(SourceServiceError::Invalid);
        }
        u32::try_from(self.kek_version).map_err(|_| SourceServiceError::Invalid)
    }
}

/// Production adapters use this shape before loading bytes. CASE bounds
/// text too, so even a corrupt row cannot return an oversized identity.
pub fn source_metadata_sql() -> &'static str {
    "SELECT track_id::text, CASE WHEN octet_length(source_job_id) <= 64 THEN source_job_id END AS source_job_id, nzb_ref_envelope_version, nzb_ref_payload_format, nzb_ref_kek_version, octet_length(nzb_ref_encrypted) AS ciphertext_len, octet_length(nzb_ref_dek_wrapped_nonce) AS wrap_nonce_len, octet_length(nzb_ref_dek_wrapped) AS wrapped_dek_len, octet_length(nzb_ref_payload_nonce) AS payload_nonce_len FROM track_sources WHERE track_id = $1::text::uuid LIMIT 2"
}

/// Parameters: track, selected job, exact KEK version, selected ciphertext
/// length. Every byte-bearing field is bounded in SQL before materializing.
/// Concurrent rotation/change loses this snapshot or fails authentication;
/// it never falls back to a different key or an unbounded full-row SELECT.
pub fn source_envelope_sql() -> &'static str {
    "SELECT nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped, nzb_ref_payload_nonce, nzb_ref_encrypted FROM track_sources WHERE track_id = $1::text::uuid AND source_job_id = $2 AND nzb_ref_kek_version = $3 AND nzb_ref_envelope_version = 2 AND nzb_ref_payload_format = 1 AND octet_length(nzb_ref_encrypted) = $4 AND octet_length(nzb_ref_encrypted) BETWEEN 17 AND 8388624 AND octet_length(nzb_ref_dek_wrapped_nonce) = 12 AND octet_length(nzb_ref_dek_wrapped) = 48 AND octet_length(nzb_ref_payload_nonce) = 12 LIMIT 1"
}

/// Implemented by the existing internal request/grant authority, never by
/// parameters decoded from a public raw-source request (no such route exists).
pub trait SourceReadAuthorization {
    fn authorize(&self, track_id: &str) -> Result<(), SourceServiceError>;
}

pub trait SourceRepository {
    /// At most two metadata rows, to detect an invalid duplicate projection.
    fn metadata(
        &mut self,
        track_id: &str,
    ) -> Result<[Option<SourceMetadata>; 2], SourceServiceError>;
    /// Only after metadata validation, using the SQL byte guards above.
    /// Nonce conversion happens after the length-guarded row is fetched.
    fn envelope(
        &mut self,
        selected: &SourceMetadata,
    ) -> Result<Option<FieldEnvelope>, SourceServiceError>;
}

pub fn read_original_source(
    authorization: &impl SourceReadAuthorization,
    repository: &mut impl SourceRepository,
    keks: &impl KekSource,
    track_id: &str,
) -> Result<OriginalSource, SourceServiceError> {
    authorization.authorize(track_id)?;
    if !canonical_track_id(track_id) {
        return Err(SourceServiceError::Invalid);
    }
    let [first, second] = repository.metadata(track_id)?;
    if second.is_some() {
        return Err(SourceServiceError::Invalid);
    }
    let selected = first.ok_or(SourceServiceError::Missing)?;
    let version = selected.validate(track_id)?;
    let kek = keks.kek(version).ok_or(SourceServiceError::UnknownKey)?;
    let envelope = repository
        .envelope(&selected)?
        .ok_or(SourceServiceError::Missing)?;
    if envelope.ciphertext.len() != selected.ciphertext_len as usize
        || envelope.dek_wrapped.len() != 48
    {
        return Err(SourceServiceError::Invalid);
    }
    let plaintext = Zeroizing::new(
        field_envelope::open(kek, version, &context(track_id), &envelope)
            .map_err(|_| SourceServiceError::Crypto)?,
    );
    OriginalSource::parse(
        selected.payload_format,
        &plaintext,
        track_id,
        &selected.source_job_id,
    )
    .map_err(|_| SourceServiceError::Invalid)
}

/// One complete source value for the existing album publish transaction.
/// A losing first-writer-wins insert must retain the previous entire value,
/// including its authenticated file binding and range hint.
pub struct SealedSource {
    pub metadata: SourceMetadata,
    pub envelope: FieldEnvelope,
}

pub fn seal_original_source(
    keks: &impl KekSource,
    kek_version: u32,
    source: &OriginalSource,
) -> Result<SealedSource, SourceServiceError> {
    let stored_kek_version = i32::try_from(kek_version).map_err(|_| SourceServiceError::Invalid)?;
    let kek = keks
        .kek(kek_version)
        .ok_or(SourceServiceError::UnknownKey)?;
    let plaintext = Zeroizing::new(source.encode().map_err(|_| SourceServiceError::Invalid)?);
    let envelope = field_envelope::seal(kek, kek_version, &context(source.track_id()), &plaintext)
        .map_err(|_| SourceServiceError::Crypto)?;
    let metadata = SourceMetadata {
        track_id: source.track_id().into(),
        source_job_id: source.source_job_id().into(),
        envelope_version: ENVELOPE_VERSION,
        payload_format: PAYLOAD_FORMAT,
        kek_version: stored_kek_version,
        ciphertext_len: i32::try_from(envelope.ciphertext.len())
            .map_err(|_| SourceServiceError::Invalid)?,
        wrap_nonce_len: 12,
        wrapped_dek_len: 48,
        payload_nonce_len: 12,
    };
    metadata.validate(source.track_id())?;
    Ok(SealedSource { metadata, envelope })
}

#[cfg(test)]
#[path = "original_source_tests.rs"]
mod tests;
