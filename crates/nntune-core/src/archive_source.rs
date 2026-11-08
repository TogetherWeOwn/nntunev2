//! Private archive metadata preparation, not an installed writer or miss path.
//! The API remains the envelope/key owner. Decode only after authenticated open
//! and bind against the exact selected catalogue/journal identity, never a
//! request field or today's new-write policy. Legacy OriginalSource stays v1:
//! imported FLAC article metadata is not a media.v1 ciphertext manifest.

use serde::{Deserialize, Serialize};

use crate::{
    archive::v1::{ObjectIdentity, ObjectRecord},
    media,
    original_source::{canonical_track_id, valid_source_job, SourceError, MAX_FILE_BYTES},
};

pub const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;

/// A versioned payload inside a future API-owned authenticated envelope. This
/// does not allocate a track_sources payload-format number or change its schema.
/// No public Deserialize or Debug: callers use the bounded, bound parser.
#[derive(Clone, Serialize)]
pub struct ArchiveSource {
    version: u16,
    track_id: String,
    source_job_id: String,
    object: ObjectRecord,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u16,
    track_id: String,
    source_job_id: String,
    object: ObjectRecord,
}

impl ArchiveSource {
    /// Trusted producer seam after exact ciphertext commit/readback. This stores
    /// coordinates only: it grants neither durability/eviction nor media-read
    /// authority. Keep safety bytes until the existing qualification gates pass.
    pub fn from_object(
        track_id: &str,
        source_job_id: &str,
        object: ObjectRecord,
    ) -> Result<Self, SourceError> {
        if !canonical_track_id(track_id) || !valid_source_job(source_job_id) {
            return Err(SourceError::Shape);
        }
        let source = Self {
            version: 1,
            track_id: track_id.into(),
            source_job_id: source_job_id.into(),
            object,
        };
        source.validate()?;
        Ok(source)
    }

    fn validate(&self) -> Result<(), SourceError> {
        if self.version != 1
            || !canonical_track_id(&self.track_id)
            || !valid_source_job(&self.source_job_id)
        {
            return Err(SourceError::Shape);
        }
        self.object.validate().map_err(|_| SourceError::Integrity)?;
        let identity = &self.object.identity;
        if identity.bytes < media::HEADER_SIZE + media::TAG_SIZE || identity.bytes > MAX_FILE_BYTES
        {
            return Err(SourceError::Bounds);
        }
        if identity.target.encryption.format.as_str() != media::FORMAT_LABEL
            || identity.target.encryption.mode != media::KeyMode::Random.as_byte()
        {
            return Err(SourceError::Binding);
        }
        Ok(())
    }

    pub fn object(&self) -> &ObjectRecord {
        &self.object
    }

    /// Internal cache coordinate only, never an ETag, entitlement or hit oracle.
    /// Publication/serving must still enforce current authority and media.v1
    /// commitment/chunk authentication on both archive backends and cache hits.
    pub fn cache_key(&self) -> [u8; 32] {
        self.object.identity.cache_key()
    }

    pub fn encode(&self) -> Result<Vec<u8>, SourceError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| SourceError::Shape)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(SourceError::Bounds);
        }
        Ok(bytes)
    }

    /// The caller bounds the authenticated envelope before opening it; this
    /// independent plaintext cap precedes every decoder allocation. Serde's
    /// recursion limit remains enabled. Diagnostics contain no private values.
    pub fn parse(
        bytes: &[u8],
        track_id: &str,
        source_job_id: &str,
        expected: &ObjectIdentity,
    ) -> Result<Self, SourceError> {
        if bytes.is_empty() || bytes.len() > MAX_RECORD_BYTES {
            return Err(SourceError::Bounds);
        }
        let record: Record = serde_json::from_slice(bytes).map_err(|_| SourceError::Shape)?;
        let source = Self {
            version: record.version,
            track_id: record.track_id,
            source_job_id: record.source_job_id,
            object: record.object,
        };
        source.validate()?;
        if source.track_id != track_id
            || source.source_job_id != source_job_id
            || &source.object.identity != expected
        {
            return Err(SourceError::Binding);
        }
        Ok(source)
    }
}
