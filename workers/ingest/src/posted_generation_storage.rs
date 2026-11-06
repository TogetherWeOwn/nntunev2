//! Final tagged-file storage in the existing ingest scratch bucket. This is
//! deliberately NOT a plan store: manifests remain encrypted at the API/keyring
//! boundary. No route, Queue handler or binding configuration is enabled here.
//!
//! A stable opaque operation key is create-only. Lost PUT replies reconcile by
//! hashing the stored bytes, never by trusting an ETag or custom metadata. R2
//! has no historical-version GET: every read checks the returned version and
//! refuses replacement rather than falling back to the current object. This is
//! fail-closed version pinning, not a bucket-wide immutability/retention policy.
//! No delete, expiry or cleanup operation exists in this module.

use super::{DispatchObject, DispatchObjectReader};
use crate::posted_generations::{identity, StoreError};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{fmt, future::Future};

#[cfg(test)]
#[path = "posted_generation_storage_tests.rs"]
mod tests;

const PREFIX: &str = "prepared-tagged/";
const READ_BYTES: usize = 65_536;
// workers-rs converts R2 range arguments to JavaScript numbers.
const MAX_EXACT_INTEGER: u64 = (1_u64 << 53) - 1;

/// Storage response, not authority or an integrity assertion. No Debug: the
/// object's identity and provider version must not appear in runtime logs.
#[derive(Clone)]
pub struct TaggedObjectMetadata {
    pub object: DispatchObject,
    pub size: u64,
}

/// Low-level create-only/range contract, implemented below by the Worker SDK.
/// Implementations must bound the body while streaming, not after array_buffer.
/// No metadata-supplied digest is accepted by the producer.
pub trait TaggedObjectStore {
    fn create<'a>(
        &'a mut self,
        id: &'a str,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<Option<TaggedObjectMetadata>, StoreError>> + 'a;

    fn head<'a>(
        &'a mut self,
        id: &'a str,
    ) -> impl Future<Output = Result<Option<TaggedObjectMetadata>, StoreError>> + 'a;

    fn read<'a>(
        &'a mut self,
        object: &'a DispatchObject,
        offset: u64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<u8>, StoreError>> + 'a;
}

/// Verified storage result. Only the byte-walking producer can construct it;
/// neither Queue fields nor R2 custom metadata can forge its digest/length.
pub struct FrozenTaggedFile {
    pub(super) object: DispatchObject,
    pub(super) size: i64,
    pub(super) sha256: String,
}

impl fmt::Debug for FrozenTaggedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FrozenTaggedFile([REDACTED])")
    }
}

/// Stable within the admitted job/file/operation. Frame identities to avoid
/// delimiter collisions; no filename, tags or article references enter the key.
pub fn tagged_object_id(
    job_id: &str,
    operation_key: &str,
    file_index: i32,
) -> Result<String, StoreError> {
    if !identity(job_id) || !identity(operation_key) || file_index < 0 {
        return Err(StoreError::InvalidInput);
    }
    let mut digest = Sha256::new();
    digest.update(b"nntune-tagged-object-v1");
    for value in [job_id, operation_key] {
        digest.update((value.len() as u32).to_be_bytes());
        digest.update(value.as_bytes());
    }
    digest.update(file_index.to_be_bytes());
    Ok(hex::encode(digest.finalize()))
}

/// Store actual final output, ignoring its public sidecar digest. Repeating
/// after an ambiguous PUT uses the same key and fully checks the existing
/// object; a divergent existing value is never overwritten or deleted.
pub async fn freeze_tagged_file<S: TaggedObjectStore>(
    store: &mut S,
    job_id: &str,
    operation_key: &str,
    file_index: i32,
    tagged: crate::flac::TaggedFlac,
) -> Result<FrozenTaggedFile, StoreError> {
    let id = tagged_object_id(job_id, operation_key, file_index)?;
    let size = i64::try_from(tagged.bytes.len()).map_err(|_| StoreError::InvalidInput)?;
    if size == 0 || size as u64 > MAX_EXACT_INTEGER {
        return Err(StoreError::InvalidInput);
    }
    let hash = hex::encode(Sha256::digest(&tagged.bytes));
    // Always reconcile after PUT, including a lost reply. A read failure or a
    // missing object remains an error, never a fresh dispatch or cleanup grant.
    let _ = store.create(&id, tagged.bytes).await;
    let selected = store.head(&id).await?.ok_or(StoreError::Inconsistent)?;
    if selected.object.id() != id || selected.size != size as u64 {
        return Err(StoreError::Inconsistent);
    }
    let object = selected.object.clone();
    let mut reader = TaggedFileReader { store, selected };
    if !super::object_matches(&mut reader, &object, size, &hash).await {
        return Err(StoreError::Inconsistent);
    }
    Ok(FrozenTaggedFile {
        object: reader.selected.object,
        size,
        sha256: hash,
    })
}

/// Reads ONLY one pinned final tagged object. A plan reference is not an alias
/// for this object: the composite dispatcher must resolve it at the API's
/// encrypted-plan boundary. Do not use this reader alone as a complete binding.
pub struct TaggedFileReader<'a, S: TaggedObjectStore> {
    store: &'a mut S,
    selected: TaggedObjectMetadata,
}

impl<'a, S: TaggedObjectStore> TaggedFileReader<'a, S> {
    pub fn new(store: &'a mut S, object: DispatchObject, size: u64) -> Result<Self, StoreError> {
        if size == 0 || size > MAX_EXACT_INTEGER {
            return Err(StoreError::InvalidInput);
        }
        Ok(Self {
            store,
            selected: TaggedObjectMetadata { object, size },
        })
    }
}

impl<S: TaggedObjectStore> DispatchObjectReader for TaggedFileReader<'_, S> {
    async fn read(
        &mut self,
        object: &DispatchObject,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>, StoreError> {
        if object != &self.selected.object
            || limit == 0
            || limit > READ_BYTES
            || offset > self.selected.size
        {
            return Err(StoreError::InvalidInput);
        }
        // Check even EOF against the exact version, so a deleted/replaced
        // object does not masquerade as a successful empty range.
        let current = self
            .store
            .head(object.id())
            .await?
            .ok_or(StoreError::Inconsistent)?;
        if current.object != *object || current.size != self.selected.size {
            return Err(StoreError::Inconsistent);
        }
        if offset == current.size {
            return Ok(Vec::new());
        }
        let wanted = (current.size - offset).min(limit as u64) as usize;
        let bytes = self.store.read(object, offset, wanted).await?;
        if bytes.len() != wanted {
            return Err(StoreError::Inconsistent);
        }
        Ok(bytes)
    }
}

/// Concrete existing INGEST_SCRATCH binding; no KEK, plan, new binding or secret.
/// Construction is internal producer work, not a public endpoint or authority.
pub struct R2TaggedObjectStore {
    bucket: worker::Bucket,
}

impl R2TaggedObjectStore {
    pub fn from_env(env: &worker::Env) -> Result<Self, StoreError> {
        Ok(Self {
            bucket: env
                .bucket(crate::verify::SCRATCH_BINDING)
                .map_err(|_| StoreError::Inconsistent)?,
        })
    }
}

fn metadata(object: &worker::Object, id: &str) -> Result<TaggedObjectMetadata, StoreError> {
    if object.key() != format!("{PREFIX}{id}") || object.size() > MAX_EXACT_INTEGER {
        return Err(StoreError::Inconsistent);
    }
    Ok(TaggedObjectMetadata {
        object: DispatchObject::new(id, &object.version())?,
        size: object.size(),
    })
}

impl TaggedObjectStore for R2TaggedObjectStore {
    async fn create(
        &mut self,
        id: &str,
        bytes: Vec<u8>,
    ) -> Result<Option<TaggedObjectMetadata>, StoreError> {
        if !identity(id) {
            return Err(StoreError::InvalidInput);
        }
        self.bucket
            .put(format!("{PREFIX}{id}"), bytes)
            .only_if(worker::Conditional {
                etag_does_not_match: Some("*".to_owned()),
                ..Default::default()
            })
            .execute()
            .await
            .map_err(|_| StoreError::Database)?
            .as_ref()
            .map(|object| metadata(object, id))
            .transpose()
    }

    async fn head(&mut self, id: &str) -> Result<Option<TaggedObjectMetadata>, StoreError> {
        if !identity(id) {
            return Err(StoreError::InvalidInput);
        }
        self.bucket
            .head(format!("{PREFIX}{id}"))
            .await
            .map_err(|_| StoreError::Database)?
            .as_ref()
            .map(|object| metadata(object, id))
            .transpose()
    }

    async fn read(
        &mut self,
        expected: &DispatchObject,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>, StoreError> {
        if limit == 0
            || limit > READ_BYTES
            || offset > MAX_EXACT_INTEGER
            || offset
                .checked_add(limit as u64)
                .is_none_or(|end| end > MAX_EXACT_INTEGER)
        {
            return Err(StoreError::InvalidInput);
        }
        let object = self
            .bucket
            .get(format!("{PREFIX}{}", expected.id()))
            .range(worker::Range::OffsetWithLength {
                offset,
                length: limit as u64,
            })
            .execute()
            .await
            .map_err(|_| StoreError::Database)?
            .ok_or(StoreError::Inconsistent)?;
        if metadata(&object, expected.id())?.object != *expected
            || object.range().map_err(|_| StoreError::Inconsistent)?
                != (worker::Range::OffsetWithLength {
                    offset,
                    length: limit as u64,
                })
        {
            return Err(StoreError::Inconsistent);
        }
        let mut stream = object
            .body()
            .ok_or(StoreError::Inconsistent)?
            .stream()
            .map_err(|_| StoreError::Inconsistent)?;
        let mut bytes = Vec::with_capacity(limit);
        while let Some(chunk) = stream.next().await {
            let mut chunk = chunk.map_err(|_| StoreError::Database)?;
            if chunk.len() > limit - bytes.len() {
                chunk.fill(0);
                bytes.fill(0);
                return Err(StoreError::Inconsistent);
            }
            bytes.extend_from_slice(&chunk);
            chunk.fill(0);
        }
        if bytes.len() != limit {
            bytes.fill(0);
            return Err(StoreError::Inconsistent);
        }
        Ok(bytes)
    }
}
