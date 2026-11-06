//! Posting-disabled preparation/outbox adapter for the existing admin pipeline.
//!
//! Migration 0047 commits a final-file checkpoint and dispatch intent as ONE row.
//! The producer supplies a stable operation key, never a transport delivery id.
//! Owner/admission/original provenance comes from the DB. Explicit producer
//! startup acquires the exact durable receipt before sending a pointer; Queue
//! recovery cannot start or take over work. No transaction spans object/Queue IO.
//!
//! Opaque object/version references must resolve immutable storage: callers store
//! final tagged bytes and the sealed plan before preparation, and readers return
//! the corresponding final plaintext/canonical plan bytes. The adapter hashes
//! those bytes, not upload digests, ETags or caller-reported digest strings.
//! A stored request is not candidate custody or POST authority. This module has
//! no article transport and is not wired into the old search/upload consumer.
//! Default mode is disabled. Live admin/M2 binding, rollout and actual drain,
//! schema/crypto review, staging/restore and release remain separate gates.

use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::future::Future;

#[path = "posted_generation_storage.rs"]
pub mod storage;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DispatchMode {
    #[default]
    Disabled,
    PreparationOnly,
}

/// Only these two fields cross the Queue boundary. Possession is not authority;
/// only the trusted internal DB/Queue binding may invoke the recovery adapter.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct DispatchPointer {
    version: u8,
    id: String,
}

impl<'de> Deserialize<'de> for DispatchPointer {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PointerVisitor;

        impl<'de> serde::de::Visitor<'de> for PointerVisitor {
            type Value = DispatchPointer;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a dispatch pointer object")
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                map: M,
            ) -> Result<Self::Value, M::Error> {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Fields {
                    version: u8,
                    id: String,
                }

                let fields =
                    Fields::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(DispatchPointer {
                    version: fields.version,
                    id: fields.id,
                })
            }
        }

        // Derived struct deserialization also accepts positional sequences.
        // The Queue wire contract requires a map with the two named fields.
        deserializer.deserialize_map(PointerVisitor)
    }
}

impl fmt::Debug for DispatchPointer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DispatchPointer([REDACTED])")
    }
}

/// Versioned opaque storage identity, not a filename or media URL.
#[derive(Clone, PartialEq, Eq)]
pub struct DispatchObject {
    id: String,
    version: String,
}

impl DispatchObject {
    pub fn new(id: &str, version: &str) -> Result<Self, StoreError> {
        if !identity(id) || !identity(version) {
            return Err(StoreError::InvalidInput);
        }
        Ok(Self {
            id: id.to_owned(),
            version: version.to_owned(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn version(&self) -> &str {
        &self.version
    }
}

impl fmt::Debug for DispatchObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DispatchObject([REDACTED])")
    }
}

/// Frozen format-1 input. Construct from actual final bytes, not their sidecar's
/// claimed digest. Plan bytes are canonical plaintext resolved from sealed storage;
/// they are hashed here, never persisted by this adapter, serialized or logged.
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedDispatchFile {
    file_index: i32,
    object: DispatchObject,
    plaintext_bytes: i64,
    plaintext_sha256: String,
    plan: DispatchObject,
    plan_bytes: i64,
    plan_sha256: String,
}

impl PreparedDispatchFile {
    pub fn from_bytes(
        file_index: i32,
        object: DispatchObject,
        tagged_bytes: &[u8],
        plan: DispatchObject,
        canonical_plan: &[u8],
    ) -> Result<Self, StoreError> {
        if file_index < 0 || tagged_bytes.is_empty() || canonical_plan.is_empty() {
            return Err(StoreError::InvalidInput);
        }
        Ok(Self {
            file_index,
            object,
            plan,
            plaintext_bytes: i64::try_from(tagged_bytes.len())
                .map_err(|_| StoreError::InvalidInput)?,
            plaintext_sha256: hex::encode(Sha256::digest(tagged_bytes)),
            plan_bytes: i64::try_from(canonical_plan.len())
                .map_err(|_| StoreError::InvalidInput)?,
            plan_sha256: hex::encode(Sha256::digest(canonical_plan)),
        })
    }

    fn from_row(row: &Row) -> Self {
        Self {
            file_index: row.get("file_index"),
            object: DispatchObject {
                id: row.get("object_id"),
                version: row.get("object_version"),
            },
            plaintext_bytes: row.get("plaintext_bytes"),
            plaintext_sha256: row.get("plaintext_sha256"),
            plan: DispatchObject {
                id: row.get("plan_id"),
                version: row.get("plan_version"),
            },
            plan_bytes: row.get("plan_bytes"),
            plan_sha256: row.get("plan_sha256"),
        }
    }
}

impl fmt::Debug for PreparedDispatchFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedDispatchFile([REDACTED])")
    }
}

/// Resolve exactly the immutable object/version through existing trusted storage.
/// Range reads return at most `limit` bytes; empty means EOF. Unavailable versions,
/// failed decryption or changed association must error, never fall back to latest.
pub trait DispatchObjectReader {
    fn read<'a>(
        &'a mut self,
        object: &'a DispatchObject,
        offset: u64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<u8>, StoreError>> + 'a;
}

/// Existing Queue binding, injected here so adapter acceptance can use a fake.
/// An error includes an ambiguous send result: retain the same pointer for retry.
pub trait DispatchQueue {
    fn send(&mut self, pointer: DispatchPointer) -> impl Future<Output = Result<(), StoreError>>;
}

/// Internal preparation input. The immutable sealed-plan reference and opened
/// canonical bytes MUST come from the API/keyring boundary (not yet wired).
/// This function never stores them in R2. No Debug or serialization: neither
/// tags nor plan plaintext may leak.
pub struct TaggedDispatchInput<'a> {
    pub file_index: i32,
    pub tagged: crate::flac::TaggedFlac,
    pub plan: DispatchObject,
    pub canonical_plan: &'a [u8],
}

/// Existing tag output -> create-only tagged R2 object -> transactional file
/// checkpoint/outbox. Revalidate admission after storage IO before committing
/// dispatch. Failed/ambiguous operations retain the stored object; no cleanup
/// or acquisition is inferred. Concrete sealed-plan resolution and selected
/// Queue recovery remain necessary before this is a complete live binding.
pub async fn prepare_tagged_file_dispatch<S: storage::TaggedObjectStore>(
    client: &mut Client,
    mode: DispatchMode,
    job_id: &str,
    operation_key: &str,
    expected_attempt: i64,
    input: TaggedDispatchInput<'_>,
    store: &mut S,
) -> Result<Option<DispatchPointer>, StoreError> {
    if mode == DispatchMode::Disabled {
        return Ok(None);
    }
    let object_id = storage::tagged_object_id(job_id, operation_key, input.file_index)?;
    if expected_attempt < 0
        || expected_attempt == i64::MAX
        || input.canonical_plan.is_empty()
        || input.plan.id() == object_id
    {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(job) = admission_locked(&tx, job_id, None).await? else {
        return Ok(None);
    };
    if input.file_index >= job.get::<_, i32>("file_count") {
        return Ok(None);
    }
    let provenance = AdmissionIdentity::from_row(&job);
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    let frozen =
        storage::freeze_tagged_file(store, job_id, operation_key, input.file_index, input.tagged)
            .await?;
    let file = PreparedDispatchFile {
        file_index: input.file_index,
        object: frozen.object,
        plaintext_bytes: frozen.size,
        plaintext_sha256: frozen.sha256,
        plan: input.plan,
        plan_bytes: i64::try_from(input.canonical_plan.len())
            .map_err(|_| StoreError::InvalidInput)?,
        plan_sha256: hex::encode(Sha256::digest(input.canonical_plan)),
    };
    prepare_file_dispatch_bound(
        client,
        mode,
        job_id,
        operation_key,
        expected_attempt,
        &file,
        Some(&provenance),
    )
    .await
}

#[derive(PartialEq, Eq)]
struct AdmissionIdentity {
    owner: String,
    admission: String,
    original: String,
    original_bytes: i64,
    file_count: i32,
}

impl AdmissionIdentity {
    fn from_row(row: &Row) -> Self {
        Self {
            owner: row.get("user_id"),
            admission: row.get("idempotency_key"),
            original: row.get("content_sha256"),
            original_bytes: row.get("nzb_bytes"),
            file_count: row.get("file_count"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchSend {
    Disabled,
    StaleOrMissing,
    ObjectsUnavailable,
    Pending,
    Acknowledged,
}

// Same serialization order as admin admission/deletion, then owner -> job ->
// original. The role/deletion fence remains held through startup or recovery.
async fn admission_locked(
    tx: &Transaction<'_>,
    job_id: &str,
    expected_owner: Option<&str>,
) -> Result<Option<Row>, StoreError> {
    tx.query_one(
        "SELECT pg_advisory_xact_lock(hashtext('admin_writes'))",
        &[],
    )
    .await
    .map_err(database_error)?;
    let Some(owner) = tx
        .query_opt("SELECT user_id FROM ingest_jobs WHERE id = $1", &[&job_id])
        .await
        .map_err(database_error)?
    else {
        return Ok(None);
    };
    let owner: String = owner.get(0);
    if expected_owner.is_some_and(|expected| expected != owner) {
        return Ok(None);
    }
    if tx.query_opt(
        "SELECT id FROM users WHERE id = $1 AND role = 'admin' AND deleting_at IS NULL FOR SHARE",
        &[&owner],
    ).await.map_err(database_error)?.is_none() { return Ok(None) }
    let Some(job) = tx.query_opt(
        "SELECT user_id, idempotency_key, content_sha256, nzb_bytes, file_count FROM ingest_jobs \
         WHERE id = $1 AND user_id = $2 AND source = 'admin' \
         AND status IN ('queued', 'running') AND failure_reason IS NULL FOR UPDATE",
        &[&job_id, &owner],
    ).await.map_err(database_error)? else { return Ok(None) };
    let hash: String = job.get("content_sha256");
    if job.get::<_, String>("idempotency_key") != format!("admin:{hash}") {
        return Ok(None);
    }
    if tx.query_opt(
        "SELECT job_id FROM ingest_originals WHERE job_id = $1 AND content_sha256 = $2 \
         AND nzb_bytes = $3 AND nzb_ref_envelope_version = 2 AND nzb_ref_payload_format = 1 FOR SHARE",
        &[&job_id, &hash, &job.get::<_, i64>("nzb_bytes")],
    ).await.map_err(database_error)?.is_none() { return Ok(None) }
    // A later job-row edit cannot rebind an earlier prepared operation or use a
    // new predecessor to escape its admitted owner/original relationship.
    let diverged: bool = tx
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM ingest_file_dispatches WHERE job_id = $1 \
         AND (owner_id <> $2 OR admission_key <> $3 OR original_sha256 <> $4))",
            &[
                &job_id,
                &owner,
                &job.get::<_, String>("idempotency_key"),
                &hash,
            ],
        )
        .await
        .map_err(database_error)?
        .get(0);
    if diverged {
        return Ok(None);
    }
    Ok(Some(job))
}

/// Trusted per-file producer checkpoint. Its stable operation key survives an
/// ambiguous commit. Same key + changed input is refusal, never an upsert. One
/// request per explicit JOB predecessor prevents implicit parallel file attempts.
/// Preparing a successor is not generation replacement/takeover authorization.
pub async fn prepare_file_dispatch(
    client: &mut Client,
    mode: DispatchMode,
    job_id: &str,
    operation_key: &str,
    expected_attempt: i64,
    file: &PreparedDispatchFile,
) -> Result<Option<DispatchPointer>, StoreError> {
    prepare_file_dispatch_bound(
        client,
        mode,
        job_id,
        operation_key,
        expected_attempt,
        file,
        None,
    )
    .await
}

async fn prepare_file_dispatch_bound(
    client: &mut Client,
    mode: DispatchMode,
    job_id: &str,
    operation_key: &str,
    expected_attempt: i64,
    file: &PreparedDispatchFile,
    expected_provenance: Option<&AdmissionIdentity>,
) -> Result<Option<DispatchPointer>, StoreError> {
    if mode == DispatchMode::Disabled {
        return Ok(None);
    }
    if !identity(job_id)
        || !identity(operation_key)
        || expected_attempt < 0
        || expected_attempt == i64::MAX
    {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(job) = admission_locked(&tx, job_id, None).await? else {
        return Ok(None);
    };
    if file.file_index >= job.get::<_, i32>("file_count")
        || expected_provenance
            .is_some_and(|expected| *expected != AdmissionIdentity::from_row(&job))
    {
        return Ok(None);
    }
    let owner: String = job.get("user_id");
    let admission: String = job.get("idempotency_key");
    let original: String = job.get("content_sha256");
    let existing = tx
        .query_opt(
            "SELECT * FROM ingest_file_dispatches WHERE job_id = $1 AND operation_key = $2",
            &[&job_id, &operation_key],
        )
        .await
        .map_err(database_error)?;
    let id = if let Some(row) = existing {
        if PreparedDispatchFile::from_row(&row) != *file
            || row.get::<_, i64>("expected_attempt") != expected_attempt
            || row.get::<_, String>("owner_id") != owner
            || row.get::<_, String>("admission_key") != admission
            || row.get::<_, String>("original_sha256") != original
        {
            return Ok(None);
        }
        row.get("id")
    } else {
        // Intent does not choose an arbitrary future predecessor or create
        // concurrent per-file attempts. Acquisition still checks lease expiry.
        let current = tx
            .query_opt(
                "SELECT attempt FROM posted_file_job_attempts WHERE job_id = $1 FOR UPDATE",
                &[&job_id],
            )
            .await
            .map_err(database_error)?;
        if current.as_ref().map_or(0, |row| row.get::<_, i64>(0)) != expected_attempt {
            return Ok(None);
        }
        let Some(row) = tx.query_opt(
            "INSERT INTO ingest_file_dispatches (job_id, operation_key, owner_id, admission_key, \
             original_sha256, file_index, expected_attempt, object_id, object_version, \
             plaintext_bytes, plaintext_sha256, plan_id, plan_version, plan_format, plan_bytes, plan_sha256) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,1,$14,$15) \
             ON CONFLICT (job_id, expected_attempt) DO NOTHING RETURNING id",
            &[&job_id, &operation_key, &owner, &admission, &original, &file.file_index,
              &expected_attempt, &file.object.id, &file.object.version, &file.plaintext_bytes,
              &file.plaintext_sha256, &file.plan.id, &file.plan.version, &file.plan_bytes, &file.plan_sha256],
        ).await.map_err(database_error)? else { return Ok(None) };
        row.get("id")
    };
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(Some(DispatchPointer { version: 1, id }))
}

async fn dispatch_locked(
    tx: &Transaction<'_>,
    pointer: &DispatchPointer,
) -> Result<Option<Row>, StoreError> {
    if pointer.version != 1 || !identity(&pointer.id) {
        return Err(StoreError::InvalidInput);
    }
    let Some(row) = tx
        .query_opt(
            "SELECT * FROM ingest_file_dispatches WHERE id = $1",
            &[&pointer.id],
        )
        .await
        .map_err(database_error)?
    else {
        return Ok(None);
    };
    let job_id: String = row.get("job_id");
    let owner: String = row.get("owner_id");
    let Some(job) = admission_locked(tx, &job_id, Some(&owner)).await? else {
        return Ok(None);
    };
    if row.get::<_, String>("admission_key") != job.get::<_, String>("idempotency_key")
        || row.get::<_, String>("original_sha256") != job.get::<_, String>("content_sha256")
        || row.get::<_, i32>("file_index") >= job.get::<_, i32>("file_count")
        || row.get::<_, i16>("plan_format") != 1
        || row.get::<_, String>("purpose") != "prepare"
    {
        return Ok(None);
    }
    Ok(Some(row))
}

async fn recover_pointer(
    client: &mut Client,
    pointer: &DispatchPointer,
) -> Result<Option<(AttemptHandle, PreparedDispatchFile)>, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(row) = dispatch_locked(&tx, pointer).await? else {
        return Ok(None);
    };
    let request = DurableAttemptRequest::from_admin_dispatch(&row)?;
    let file = PreparedDispatchFile::from_row(&row);
    match attempts::recover_durable_locked(tx, &request).await? {
        AttemptOutcome::Acquired(handle) => Ok(Some((handle, file))),
        AttemptOutcome::StaleOrMissing => Ok(None),
        AttemptOutcome::Busy => Err(StoreError::Inconsistent),
    }
}

async fn object_matches<R: DispatchObjectReader>(
    reader: &mut R,
    object: &DispatchObject,
    length: i64,
    hash: &str,
) -> bool {
    let Ok(length) = u64::try_from(length) else {
        return false;
    };
    let mut offset = 0_u64;
    let mut digest = Sha256::new();
    loop {
        let Ok(mut chunk) = reader.read(object, offset, 65536).await else {
            return false;
        };
        if chunk.is_empty() {
            break;
        }
        let Some(next) = offset.checked_add(chunk.len() as u64) else {
            return false;
        };
        if chunk.len() > 65536 || next > length {
            return false;
        }
        digest.update(&chunk);
        chunk.fill(0);
        offset = next;
    }
    offset == length && hex::encode(digest.finalize()) == hash
}

async fn objects_match<R: DispatchObjectReader>(
    reader: &mut R,
    file: &PreparedDispatchFile,
) -> bool {
    object_matches(
        reader,
        &file.object,
        file.plaintext_bytes,
        &file.plaintext_sha256,
    )
    .await
        && object_matches(reader, &file.plan, file.plan_bytes, &file.plan_sha256).await
}

/// Explicit producer continuation. The immutable row is the pending outbox;
/// acquisition is atomic with its existing receipt/running transition, not with
/// Queue IO. Lost startup replies repeat this SAME pointer/request. Send errors
/// retain pending state, including when Queue accepted but lost its reply.
pub async fn send_file_dispatch<R: DispatchObjectReader, Q: DispatchQueue>(
    client: &mut Client,
    mode: DispatchMode,
    pointer: &DispatchPointer,
    reader: &mut R,
    queue: &mut Q,
) -> Result<DispatchSend, StoreError> {
    if mode == DispatchMode::Disabled {
        return Ok(DispatchSend::Disabled);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(row) = dispatch_locked(&tx, pointer).await? else {
        return Ok(DispatchSend::StaleOrMissing);
    };
    let request = DurableAttemptRequest::from_admin_dispatch(&row)?;
    let file = PreparedDispatchFile::from_row(&row);
    if !matches!(
        attempts::start_and_acquire_locked(tx, &request).await?,
        AttemptOutcome::Acquired(_)
    ) {
        return Ok(DispatchSend::StaleOrMissing);
    }
    if !objects_match(reader, &file).await {
        return Ok(DispatchSend::ObjectsUnavailable);
    }
    if recover_pointer(client, pointer).await?.is_none() {
        return Ok(DispatchSend::StaleOrMissing);
    }
    if queue.send(pointer.clone()).await.is_err() {
        return Ok(DispatchSend::Pending);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    if dispatch_locked(&tx, pointer).await?.is_none() {
        return Ok(DispatchSend::StaleOrMissing);
    }
    tx.execute(
        "UPDATE ingest_file_dispatches SET send_acknowledged = TRUE WHERE id = $1",
        &[&pointer.id],
    )
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(DispatchSend::Acknowledged)
}

/// Intended internal Queue entry: exact read-only recovery only. It validates
/// immutable object versions/digests outside DB locks, then revalidates the
/// original acquisition/provenance after those awaits. No fallback startup,
/// renewal, candidate mint or network POST is possible through this entry.
pub async fn recover_file_dispatch<R: DispatchObjectReader>(
    client: &mut Client,
    mode: DispatchMode,
    pointer: &DispatchPointer,
    reader: &mut R,
) -> Result<AttemptOutcome, StoreError> {
    if mode == DispatchMode::Disabled {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    let Some((_, file)) = recover_pointer(client, pointer).await? else {
        return Ok(AttemptOutcome::StaleOrMissing);
    };
    if !objects_match(reader, &file).await {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    Ok(match recover_pointer(client, pointer).await? {
        Some((handle, _)) => AttemptOutcome::Acquired(handle),
        None => AttemptOutcome::StaleOrMissing,
    })
}

/// Existing job continuation can rediscover pending intents after process loss.
/// These pointers grant no authority; the explicit producer must still load and
/// revalidate each one. Queue consumers must never call the producer as fallback.
pub async fn pending_file_dispatches(
    client: &Client,
    job_id: &str,
) -> Result<Vec<DispatchPointer>, StoreError> {
    if !identity(job_id) {
        return Err(StoreError::InvalidInput);
    }
    let rows = client
        .query(
            "SELECT id FROM ingest_file_dispatches WHERE job_id = $1 AND NOT send_acknowledged \
         ORDER BY expected_attempt DESC LIMIT 32",
            &[&job_id],
        )
        .await
        .map_err(database_error)?;
    Ok(rows
        .into_iter()
        .map(|row| DispatchPointer {
            version: 1,
            id: row.get(0),
        })
        .collect())
}
