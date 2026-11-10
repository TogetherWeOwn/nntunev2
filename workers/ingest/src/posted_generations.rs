//! Short PostgreSQL transactions for immutable posted-file custody.
//!
//! Preparation only: migration 0054 replaces the 0037 global hash UNIQUE
//! with a legacy-only partial index, so this adapter admits at most one
//! pending generation per hash. A usable selected winner is reused
//! read-only; a terminally failed winner is recorded as the operation
//! predecessor for one replacement candidate. Full promotion must still
//! pass [`confirm_retention`]/[`promote_candidate`]; the legacy
//! writer/publication rollout stays separate. Migration 0042 rejects
//! unfenced health changes on registered
//! legacy custody; 0043 serializes new registration with existing legacy-row
//! writes, including stale transaction snapshots. Migration 0045 prepares an
//! OFF-by-default, monotonic database retirement gate for all legacy inserts
//! and changed health writes. Activation is a separately authorized rollout
//! operation; installation or even database retirement is NOT a worker/network
//! drain, compatible-consumer rollout proof or replacement authority.
//! The dispatch submodule supplies a disabled preparation/outbox/recovery adapter;
//! no live Worker Queue, NNTP, KV or pointer consumer is enabled. An admitted
//! bundle is custody, NOT verification, registry promotion or serving authority.
//! No transaction may span a network POST. Commit errors are ambiguous and
//! require operation-identity recovery, never a remint or optimistic POST.
//! Every attempt acquisition/recovery and fenced candidate/retained transaction
//! takes the owner deletion writer fence before job/attempt/registry locks. A
//! pre-deletion receipt or permit cannot restore authority after scheduling.
//! This transaction fence is not cancellation of already in-flight network IO.
//!
//! The sealed manifest is the durable article/checkpoint plan. `plan_sha256`
//! identifies its canonical required slot order, including recovery articles;
//! plaintext article identities never reach this store. Existing posted_files
//! envelopes and their hash-bound AAD remain unchanged.
//!
//! Presence confirmations and `verified_at` below are NOT readable/authenticated
//! content proof. R2 safety-net custody must stay until exact-generation content
//! integrity is established on two independent backbones and rechecked against
//! concurrent health/repair. A timestamp, successful POST, STAT or elapsed time
//! never authorizes cleanup; this preparation adapter grants no deletion permit.

use crate::posted_files::{validate_posted_file_params, PostedFileParams, MAX_ARTICLE_COUNT};
use crate::two_spool::{PostVerdict, SpoolCopyState, MAX_REPOST_ATTEMPTS_PER_ARTICLE};
use std::fmt;
use tokio_postgres::{Client, Row, Transaction};

#[path = "posted_generation_attempts.rs"]
mod attempts;
pub use attempts::{
    acquire_prepared_attempt, prepare_attempt_acquisition, prepare_durable_attempt_acquisition,
    recover_attempt_acquisition, recover_durable_attempt_acquisition,
    start_and_acquire_durable_attempt, start_durable_attempt_acquisition, AttemptAcquisition,
    DurableAttemptRequest,
};
#[path = "posted_generation_dispatch.rs"]
pub mod dispatch;
#[path = "posted_generation_driver.rs"]
mod driver;
pub use driver::{
    drive_durable_recheck, drive_pending_recheck, drive_recheck, GenerationTransport, RecheckStep,
};
#[path = "posted_generation_content.rs"]
mod content;
pub use content::{
    drive_content_read, prepare_content_check, record_content_check, ContentCheckPermit,
    ContentCheckReport, ContentCheckResult, ContentCheckState, ContentReadStep,
    GenerationContentTransport,
};
#[path = "posted_generation_failure.rs"]
mod failure;
pub use failure::{fail_candidate_verification, VerificationFailureOutcome};
#[path = "posted_generation_presence.rs"]
mod presence;
pub use presence::{prepare_presence_check, record_presence_check, PresenceCheckPermit};
#[path = "posted_generation_rechecks.rs"]
mod rechecks;
pub use rechecks::{
    complete_recheck, prepare_recheck, RecheckPermit, RecheckPreparation, RecheckSchedule,
};

#[path = "posted_generation_retained.rs"]
mod retained;
pub use retained::{
    conclude_retained_failure, drive_retained_content_read, drive_retained_repair,
    invalidate_retained_check, prepare_retained_check, prepare_retained_content_check,
    record_retained_content_check, recover_retained_repair, start_retained_repair,
    RetainedCheckPermit, RetainedContentPermit, RetainedCustody, RetainedFailureOutcome,
    RetainedInvalidation, RetainedReferenceKind, RetainedRepairStep,
};

const LEASE_SECONDS: i64 = 300;

/// Static, redacted errors. Never surface PostgreSQL errors: their detail may
/// include a failing row containing encrypted custody or pool configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    InvalidInput,
    Database,
    CommitUnknown,
    Inconsistent,
}

fn database_error(_: tokio_postgres::Error) -> StoreError {
    StoreError::Database
}

fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Persisted attempt authority, returned only after database acquisition or
/// exact receipt recovery. Queue-carried attempt numbers do not construct this
/// handle and are never sufficient.
#[derive(Clone, PartialEq, Eq)]
pub struct AttemptHandle {
    job_id: String,
    owner_id: String,
    attempt: i64,
    token: String,
}

impl fmt::Debug for AttemptHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AttemptHandle([REDACTED])")
    }
}

#[derive(Debug)]
pub enum AttemptOutcome {
    Acquired(AttemptHandle),
    Busy,
    StaleOrMissing,
}

/// A read snapshot is only an expected CAS value, never posting authority.
#[derive(Clone)]
pub struct Observation {
    hash: String,
    revision: i64,
    selected: Option<String>,
    health_revision: Option<i64>,
}

impl fmt::Debug for Observation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Observation([REDACTED])")
    }
}

/// The one current admission fence. Fields stay private so only a committed
/// adapter result can construct a handle. No Debug/Display exposes tokens.
#[derive(Clone, PartialEq, Eq)]
pub struct GenerationHandle {
    attempt: AttemptHandle,
    operation: String,
    generation: String,
    claim_token: String,
    registry_revision: i64,
    predecessor: Option<String>,
    predecessor_health_revision: Option<i64>,
    health_revision: i64,
    round: i64,
    round_token: String,
}

impl fmt::Debug for GenerationHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GenerationHandle([REDACTED])")
    }
}

impl GenerationHandle {
    pub fn generation_id(&self) -> &str {
        &self.generation
    }

    pub fn operation_id(&self) -> &str {
        &self.operation
    }
}

/// Exact stored custody, not a locally minted key paired with a hash lookup.
/// The row is deliberately private and has no derived Debug implementation.
pub struct StoredGeneration {
    row: Row,
}

impl fmt::Debug for StoredGeneration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoredGeneration([REDACTED])")
    }
}

impl StoredGeneration {
    pub fn generation_id(&self) -> &str {
        self.row.get("id")
    }

    pub fn plan_sha256(&self) -> &str {
        self.row.get("plan_sha256")
    }

    pub fn custody(&self) -> Result<PostedFileParams<'_>, StoreError> {
        let r = &self.row;
        let result = PostedFileParams {
            plaintext_sha256: r.try_get("plaintext_sha256").map_err(database_error)?,
            backbone_post: r.try_get("backbone_post").map_err(database_error)?,
            backbone_verify: r.try_get("backbone_verify").map_err(database_error)?,
            article_count: r.try_get("article_count").map_err(database_error)?,
            verified_count: r.try_get("verified_count").map_err(database_error)?,
            original_nzb_sha256: r.try_get("original_nzb_sha256").map_err(database_error)?,
            post_copy: crate::posted_files::parse_spool_copy_state(
                r.try_get("post_copy").map_err(database_error)?,
            )
            .map_err(|_| StoreError::Inconsistent)?,
            verify_copy: crate::posted_files::parse_spool_copy_state(
                r.try_get("verify_copy").map_err(database_error)?,
            )
            .map_err(|_| StoreError::Inconsistent)?,
            two_spool_verified: r.try_get("two_spool_verified").map_err(database_error)?,
            repost_exhausted: r.try_get("repost_exhausted").map_err(database_error)?,
            plaintext_size: r.try_get("plaintext_size").map_err(database_error)?,
            article_payload_size: r.try_get("article_payload_size").map_err(database_error)?,
            media: crate::posted_files::PostedFileMedia::from_record(
                r.try_get("media_format").map_err(database_error)?,
                r.try_get("header_nonce").map_err(database_error)?,
                r.try_get("media_key_mode").map_err(database_error)?,
                r.try_get("media_header").map_err(database_error)?,
            )
            .map_err(|_| StoreError::Inconsistent)?,
            file_key_encrypted: r.try_get("file_key_encrypted").map_err(database_error)?,
            file_key_kek_version: r.try_get("file_key_kek_version").map_err(database_error)?,
            file_key_dek_wrapped_nonce: r
                .try_get("file_key_dek_wrapped_nonce")
                .map_err(database_error)?,
            file_key_dek_wrapped: r.try_get("file_key_dek_wrapped").map_err(database_error)?,
            file_key_payload_nonce: r
                .try_get("file_key_payload_nonce")
                .map_err(database_error)?,
            file_key_payload_format: r
                .try_get("file_key_payload_format")
                .map_err(database_error)?,
            manifest_encrypted: r.try_get("manifest_encrypted").map_err(database_error)?,
            manifest_kek_version: r.try_get("manifest_kek_version").map_err(database_error)?,
            manifest_dek_wrapped_nonce: r
                .try_get("manifest_dek_wrapped_nonce")
                .map_err(database_error)?,
            manifest_dek_wrapped: r.try_get("manifest_dek_wrapped").map_err(database_error)?,
            manifest_payload_nonce: r
                .try_get("manifest_payload_nonce")
                .map_err(database_error)?,
            manifest_payload_format: r
                .try_get("manifest_payload_format")
                .map_err(database_error)?,
        };
        validate_posted_file_params(&result).map_err(|_| StoreError::Inconsistent)?;
        Ok(result)
    }
}

#[derive(Debug)]
pub enum AdmissionOutcome {
    Admitted(GenerationHandle, StoredGeneration),
    ResumedSameCandidate(GenerationHandle, StoredGeneration),
    ReusedUsableWinner(StoredGeneration),
    ClaimBusy,
    StaleOrDiverged,
    InconsistentOrMissing,
}

/// Caller-minted, complete sealed custody. Replay uses `recover`, not a new
/// mint. The two envelopes still use the existing v2 AAD contract.
pub struct CandidateMint<'a> {
    pub operation_id: &'a str,
    pub generation_id: &'a str,
    pub plan_sha256: &'a str,
    pub custody: PostedFileParams<'a>,
}

impl fmt::Debug for CandidateMint<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CandidateMint([REDACTED])")
    }
}

impl CandidateMint<'_> {
    fn validate(&self) -> Result<(), StoreError> {
        if !identity(self.operation_id)
            || !identity(self.generation_id)
            || !crate::two_spool::validate_plaintext_sha256(self.plan_sha256)
        {
            return Err(StoreError::InvalidInput);
        }
        validate_posted_file_params(&self.custody).map_err(|_| StoreError::InvalidInput)?;
        if self.custody.verified_count != 0
            || self.custody.post_copy != SpoolCopyState::Unknown
            || self.custody.verify_copy != SpoolCopyState::Unknown
            || self.custody.two_spool_verified
            || self.custody.repost_exhausted
            || self.custody.manifest_encrypted.len() < 16
        {
            return Err(StoreError::InvalidInput);
        }
        Ok(())
    }
}

/// Acquire a new DB-clock-bounded attempt. A caller may only advance the exact
/// expired attempt it observed. The owner deletion fence precedes job/attempt
/// locks and remains held through commit.
pub async fn acquire_attempt(
    client: &mut Client,
    job_id: &str,
    owner_id: &str,
    expected_attempt: i64,
) -> Result<AttemptOutcome, StoreError> {
    if !identity(job_id) || !identity(owner_id) || expected_attempt < 0 {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_owner(&tx, owner_id).await? {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    if tx
        .query_opt(
            "SELECT id FROM ingest_jobs WHERE id = $1 AND user_id = $2 AND status = 'running' FOR UPDATE",
            &[&job_id, &owner_id],
        )
        .await
        .map_err(database_error)?
        .is_none()
    {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    let old = tx
        .query_opt(
            "SELECT attempt, lease_until > extract(epoch FROM clock_timestamp())::bigint AS active \
             FROM posted_file_job_attempts WHERE job_id = $1 FOR UPDATE",
            &[&job_id],
        )
        .await
        .map_err(database_error)?;
    if let Some(row) = old {
        if row.get::<_, i64>("attempt") != expected_attempt {
            return Ok(AttemptOutcome::StaleOrMissing);
        }
        if row.get::<_, bool>("active") {
            return Ok(AttemptOutcome::Busy);
        }
    } else if expected_attempt != 0 {
        return Ok(AttemptOutcome::StaleOrMissing);
    }
    let next = expected_attempt
        .checked_add(1)
        .ok_or(StoreError::InvalidInput)?;
    let row = tx
        .query_one(
            "INSERT INTO posted_file_job_attempts (job_id, attempt, token, lease_until) \
             VALUES ($1, $2, gen_random_uuid()::text, \
                     extract(epoch FROM clock_timestamp())::bigint + $3) \
             ON CONFLICT (job_id) DO UPDATE SET attempt = EXCLUDED.attempt, \
                 token = EXCLUDED.token, lease_until = EXCLUDED.lease_until RETURNING token",
            &[&job_id, &next, &LEASE_SECONDS],
        )
        .await
        .map_err(database_error)?;
    let handle = AttemptHandle {
        job_id: job_id.to_owned(),
        owner_id: owner_id.to_owned(),
        attempt: next,
        token: row.get("token"),
    };
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(AttemptOutcome::Acquired(handle))
}

// Match admission/terminal writers: always take the deletion fence before any
// job, attempt, registry or health lock. Repeated calls retain the same row lock.
async fn lock_owner(tx: &Transaction<'_>, owner_id: &str) -> Result<bool, StoreError> {
    Ok(tx
        .query_opt(
            "SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE",
            &[&owner_id],
        )
        .await
        .map_err(database_error)?
        .is_some())
}

async fn lock_attempt(tx: &Transaction<'_>, a: &AttemptHandle) -> Result<bool, StoreError> {
    if !lock_owner(tx, &a.owner_id).await? {
        return Ok(false);
    }
    Ok(tx
        .query_opt(
            "SELECT j.id FROM ingest_jobs j JOIN posted_file_job_attempts a ON a.job_id = j.id \
             WHERE j.id = $1 AND j.user_id = $2 AND j.status = 'running' \
             AND a.attempt = $3 AND a.token = $4 \
             AND a.lease_until > extract(epoch FROM clock_timestamp())::bigint FOR UPDATE OF j, a",
            &[&a.job_id, &a.owner_id, &a.attempt, &a.token],
        )
        .await
        .map_err(database_error)?
        .is_some())
}

pub async fn observe(client: &Client, hash: &str) -> Result<Observation, StoreError> {
    if !crate::two_spool::validate_plaintext_sha256(hash) {
        return Err(StoreError::InvalidInput);
    }
    let row = client
        .query_opt(
            "SELECT r.revision, r.selected_generation, h.revision AS health_revision \
             FROM posted_file_registry r LEFT JOIN posted_file_health h \
             ON h.generation_id = r.selected_generation WHERE r.plaintext_sha256 = $1",
            &[&hash],
        )
        .await
        .map_err(database_error)?;
    Ok(match row {
        None => Observation {
            hash: hash.to_owned(),
            revision: 0,
            selected: None,
            health_revision: None,
        },
        Some(r) => Observation {
            hash: hash.to_owned(),
            revision: r.get("revision"),
            selected: r.get("selected_generation"),
            health_revision: r.get("health_revision"),
        },
    })
}

async fn stored(tx: &Transaction<'_>, id: &str) -> Result<StoredGeneration, StoreError> {
    let row = tx
        .query_opt("SELECT * FROM posted_files WHERE id = $1", &[&id])
        .await
        .map_err(database_error)?
        .ok_or(StoreError::Inconsistent)?;
    let value = StoredGeneration { row };
    value.custody()?;
    let plan: Option<&str> = value.row.get("plan_sha256");
    if plan.is_none() {
        return Err(StoreError::Inconsistent);
    }
    Ok(value)
}

async fn read_handle(
    tx: &Transaction<'_>,
    attempt: &AttemptHandle,
    operation: &str,
) -> Result<Option<GenerationHandle>, StoreError> {
    let row = tx
        .query_opt(
            "SELECT o.generation_id, o.claim_token, o.registry_revision, o.predecessor, \
             o.predecessor_health_revision, h.revision, h.round, h.round_token \
             FROM posted_file_operations o \
             JOIN posted_file_registry r ON r.plaintext_sha256 = o.plaintext_sha256 \
             JOIN posted_file_health h ON h.generation_id = o.generation_id \
             WHERE o.id = $1 AND o.job_id = $2 AND o.attempt = $3 AND o.attempt_token = $4 \
             AND o.state = 'pending' AND r.pending_operation = o.id \
             AND r.revision = o.registry_revision \
             AND r.selected_generation IS NOT DISTINCT FROM o.predecessor \
             AND o.lease_until > extract(epoch FROM clock_timestamp())::bigint",
            &[
                &operation,
                &attempt.job_id,
                &attempt.attempt,
                &attempt.token,
            ],
        )
        .await
        .map_err(database_error)?;
    Ok(row.map(|r| GenerationHandle {
        attempt: attempt.clone(),
        operation: operation.to_owned(),
        generation: r.get("generation_id"),
        claim_token: r.get("claim_token"),
        registry_revision: r.get("registry_revision"),
        predecessor: r.get("predecessor"),
        predecessor_health_revision: r.get("predecessor_health_revision"),
        health_revision: r.get("revision"),
        round: r.get("round"),
        round_token: r.get("round_token"),
    }))
}

/// Atomic admission. The registry alone selects physical custody;
/// missing legacy registration is an error, never an implicit hash-row
/// winner. A usable selected winner is returned read-only
/// ([`AdmissionOutcome::ReusedUsableWinner`] after the pinned fence); a
/// terminally failed winner is recorded as the predecessor for one
/// replacement candidate (G1). Unknown/verifying winners are busy.
/// A zero-row insert is never interpreted as admission of the caller's
/// mint.
pub async fn admit(
    client: &mut Client,
    attempt: &AttemptHandle,
    expected: &Observation,
    mint: &CandidateMint<'_>,
) -> Result<AdmissionOutcome, StoreError> {
    mint.validate()?;
    if expected.hash != mint.custody.plaintext_sha256 {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_attempt(&tx, attempt).await? {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    }
    // Bootstrap/backfill owns legacy associations while physical hashes are
    // unique. Never evaluate a scalar physical-row lookup, even on a conflicting
    // registry insert: multiple retained generations must not choose a winner.
    tx.execute(
        "INSERT INTO posted_file_registry (plaintext_sha256) \
         SELECT $1 WHERE NOT EXISTS \
         (SELECT 1 FROM posted_files WHERE plaintext_sha256 = $1) \
         ON CONFLICT (plaintext_sha256) DO NOTHING",
        &[&expected.hash],
    )
    .await
    .map_err(database_error)?;
    let Some(registry) = tx
        .query_opt(
            "SELECT selected_generation, revision, pending_operation FROM posted_file_registry \
             WHERE plaintext_sha256 = $1 FOR UPDATE",
            &[&expected.hash],
        )
        .await
        .map_err(database_error)?
    else {
        return Ok(AdmissionOutcome::InconsistentOrMissing);
    };
    if let Some(operation) = registry.get::<_, Option<String>>("pending_operation") {
        if operation != mint.operation_id {
            return Ok(AdmissionOutcome::ClaimBusy);
        }
        let Some(handle) = read_handle(&tx, attempt, &operation).await? else {
            return Ok(AdmissionOutcome::StaleOrDiverged);
        };
        let bundle = stored(&tx, &handle.generation).await?;
        if bundle.generation_id() != mint.generation_id
            || bundle.plan_sha256() != mint.plan_sha256
            || bundle.custody()? != mint.custody
        {
            return Ok(AdmissionOutcome::StaleOrDiverged);
        }
        if lock_candidate(&tx, &handle).await?.is_none() {
            return Ok(AdmissionOutcome::StaleOrDiverged);
        }
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
        return Ok(AdmissionOutcome::ResumedSameCandidate(handle, bundle));
    }
    let selected: Option<String> = registry.get("selected_generation");
    if registry.get::<_, i64>("revision") != expected.revision || selected != expected.selected {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    }
    // The terminally failed winner (if any) becomes this operation's
    // pinned predecessor. Usable winners reuse without a candidate;
    // unknown/verifying winners are busy.
    let mut predecessor: Option<String> = None;
    if let Some(id) = selected {
        let Some(health) = tx
            .query_opt(
                "SELECT state, revision, verified_at, retention_checked_at \
                 FROM posted_file_health WHERE generation_id = $1 FOR UPDATE",
                &[&id],
            )
            .await
            .map_err(database_error)?
        else {
            return Ok(AdmissionOutcome::InconsistentOrMissing);
        };
        if Some(health.get::<_, i64>("revision")) != expected.health_revision {
            return Ok(AdmissionOutcome::StaleOrDiverged);
        }
        match health.get::<_, &str>("state") {
            "usable" => {
                // Fenced reuse: both evidence stamps pinned by the
                // usable CHECK, custody validated, and a managed winner
                // must already be promoted. No candidate is created.
                if health.get::<_, Option<i64>>("verified_at").is_none()
                    || health
                        .get::<_, Option<i64>>("retention_checked_at")
                        .is_none()
                {
                    return Ok(AdmissionOutcome::InconsistentOrMissing);
                }
                let row = tx
                    .query_opt("SELECT * FROM posted_files WHERE id = $1", &[&id])
                    .await
                    .map_err(database_error)?
                    .ok_or(StoreError::Inconsistent)?;
                let bundle = StoredGeneration { row };
                if bundle.custody()?.plaintext_sha256 != expected.hash.as_str() {
                    return Ok(AdmissionOutcome::InconsistentOrMissing);
                }
                if let Some(op_id) = bundle.row.get::<_, Option<String>>("generation_operation") {
                    let promoted = tx
                        .query_opt(
                            "SELECT state FROM posted_file_operations WHERE id = $1",
                            &[&op_id],
                        )
                        .await
                        .map_err(database_error)?;
                    if promoted.is_none_or(|r| r.get::<_, &str>("state") != "promoted") {
                        return Ok(AdmissionOutcome::InconsistentOrMissing);
                    }
                }
                tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
                return Ok(AdmissionOutcome::ReusedUsableWinner(bundle));
            }
            "terminal_failed" => {
                predecessor = Some(id);
            }
            _ => {
                // unknown/verifying is busy only while a live handle
                // holds it. An abandoned candidate is durably
                // authority-less: never busy, and never replacement
                // eligible (no failure decision follows abandonment).
                let abandoned = tx
                    .query_opt(
                        "SELECT 1 FROM posted_file_operations \
                         WHERE generation_id = $1 AND state = 'abandoned'",
                        &[&id],
                    )
                    .await
                    .map_err(database_error)?
                    .is_some();
                if abandoned {
                    return Ok(AdmissionOutcome::InconsistentOrMissing);
                }
                return Ok(AdmissionOutcome::ClaimBusy);
            }
        }
    } else {
        if expected.health_revision.is_some() {
            return Ok(AdmissionOutcome::InconsistentOrMissing);
        }
        // No selected winner: an abandoned operation on this hash is
        // durably authority-less and never replacement eligible
        // (observe() joins health on the selected generation, so it
        // cannot see the abandoned candidate). Refuse instead of
        // minting a fresh candidate over abandoned custody.
        let abandoned = tx
            .query_opt(
                "SELECT 1 FROM posted_file_operations \
                 WHERE plaintext_sha256 = $1 AND state = 'abandoned'",
                &[&expected.hash],
            )
            .await
            .map_err(database_error)?
            .is_some();
        if abandoned {
            return Ok(AdmissionOutcome::InconsistentOrMissing);
        }
    }
    let p = &mint.custody;
    let inserted = tx
        .execute(
            "INSERT INTO posted_files (id, generation_operation, plan_sha256, plaintext_sha256, \
             backbone_post, backbone_verify, article_count, original_nzb_sha256, plaintext_size, \
             article_payload_size, header_nonce, file_key_encrypted, file_key_envelope_version, \
             file_key_kek_version, file_key_dek_wrapped_nonce, file_key_dek_wrapped, \
             file_key_payload_nonce, file_key_payload_format, manifest_encrypted, \
             manifest_envelope_version, manifest_kek_version, manifest_dek_wrapped_nonce, \
             manifest_dek_wrapped, manifest_payload_nonce, manifest_payload_format, \
             media_format, media_key_mode, media_header) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,2,$13,$14,$15,$16,$17,$18,2,$19,$20,$21,$22,$23,$24,$25,$26) \
             ON CONFLICT DO NOTHING",
            &[
                &mint.generation_id,
                &mint.operation_id,
                &mint.plan_sha256,
                &p.plaintext_sha256,
                &p.backbone_post,
                &p.backbone_verify,
                &p.article_count,
                &p.original_nzb_sha256,
                &p.plaintext_size,
                &p.article_payload_size,
                &p.media.legacy_nonce(),
                &p.file_key_encrypted,
                &p.file_key_kek_version,
                &p.file_key_dek_wrapped_nonce,
                &p.file_key_dek_wrapped,
                &p.file_key_payload_nonce,
                &p.file_key_payload_format,
                &p.manifest_encrypted,
                &p.manifest_kek_version,
                &p.manifest_dek_wrapped_nonce,
                &p.manifest_dek_wrapped,
                &p.manifest_payload_nonce,
                &p.manifest_payload_format,
                &p.media.format(),
                &p.media.key_mode(),
                &p.media.header(),
            ],
        )
        .await
        .map_err(database_error)?;
    if inserted != 1 {
        return Ok(AdmissionOutcome::InconsistentOrMissing);
    }
    let revision = expected
        .revision
        .checked_add(1)
        .ok_or(StoreError::Inconsistent)?;
    let inserted = tx
        .execute(
            "INSERT INTO posted_file_operations (id, plaintext_sha256, generation_id, job_id, \
             attempt, attempt_token, claim_token, lease_until, registry_revision, \
             predecessor, predecessor_health_revision) \
             SELECT $1,$2,$3,a.job_id,a.attempt,a.token,gen_random_uuid()::text,a.lease_until,$7,$8,$9 \
             FROM posted_file_job_attempts a WHERE a.job_id = $4 AND a.attempt = $5 AND a.token = $6 \
             AND a.lease_until > extract(epoch FROM clock_timestamp())::bigint ON CONFLICT DO NOTHING",
            &[
                &mint.operation_id,
                &p.plaintext_sha256,
                &mint.generation_id,
                &attempt.job_id,
                &attempt.attempt,
                &attempt.token,
                &revision,
                &predecessor,
                &expected.health_revision,
            ],
        )
        .await
        .map_err(database_error)?;
    if inserted != 1 {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    }
    tx.execute(
        "INSERT INTO posted_file_health (generation_id) VALUES ($1)",
        &[&mint.generation_id],
    )
    .await
    .map_err(database_error)?;
    tx.execute(
        "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
         VALUES ('in_flight', $1, $2, $3)",
        &[&mint.operation_id, &mint.generation_id, &p.plaintext_sha256],
    )
    .await
    .map_err(database_error)?;
    let updated = tx
        .execute(
            "UPDATE posted_file_registry SET pending_operation = $2, revision = $3 \
             WHERE plaintext_sha256 = $1 AND revision = $4 \
             AND selected_generation IS NOT DISTINCT FROM $5::text \
             AND pending_operation IS NULL",
            &[
                &expected.hash,
                &mint.operation_id,
                &revision,
                &expected.revision,
                &predecessor,
            ],
        )
        .await
        .map_err(database_error)?;
    if updated != 1 {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    }
    let handle = read_handle(&tx, attempt, mint.operation_id)
        .await?
        .ok_or(StoreError::Inconsistent)?;
    let bundle = stored(&tx, mint.generation_id).await?;
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(AdmissionOutcome::Admitted(handle, bundle))
}

/// Resume an expired claim under a newer persisted attempt of the SAME job.
/// This never accepts a new mint, changes the article plan, starts a new round
/// or resets evidence/budgets. An ambiguous acknowledgment is recovered by
/// operation identity; replay with the current attempt keeps the claim token.
/// Registry identity is fenced, but no predecessor-health decision, selection,
/// promotion or content-integrity/cleanup authority is granted by takeover.
// All paths use job/attempt -> registry -> operation/health lock order.
pub async fn take_over(
    client: &mut Client,
    attempt: &AttemptHandle,
    expected: &Observation,
    operation: &str,
) -> Result<AdmissionOutcome, StoreError> {
    if !identity(operation) {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_attempt(&tx, attempt).await? {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    }
    if tx
        .query_opt(
            "SELECT revision FROM posted_file_registry WHERE plaintext_sha256 = $1 \
             AND revision = $2 AND selected_generation IS NOT DISTINCT FROM $3::text \
             AND pending_operation = $4 FOR UPDATE",
            &[
                &expected.hash,
                &expected.revision,
                &expected.selected,
                &operation,
            ],
        )
        .await
        .map_err(database_error)?
        .is_none()
    {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    }
    let Some(old) = tx
        .query_opt(
            "SELECT o.generation_id, o.attempt, o.attempt_token FROM posted_file_operations o \
             JOIN posted_file_health h ON h.generation_id = o.generation_id \
             WHERE o.id = $1 AND o.plaintext_sha256 = $2 AND o.job_id = $3 \
             AND o.registry_revision = $4 AND o.predecessor IS NOT DISTINCT FROM $5::text \
             AND o.state = 'pending' AND h.state IN ('unknown', 'verifying') \
             FOR UPDATE OF o, h",
            &[
                &operation,
                &expected.hash,
                &attempt.job_id,
                &expected.revision,
                &expected.selected,
            ],
        )
        .await
        .map_err(database_error)?
    else {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    };
    // Re-evaluate both leases with the DB clock AFTER all lock waits. The
    // attempt/job rows remain locked, so no other acquisition can race commit.
    let Some(leases) = tx
        .query_opt(
            "SELECT o.lease_until > extract(epoch FROM clock_timestamp())::bigint AS active \
             FROM posted_file_operations o JOIN posted_file_job_attempts a ON a.job_id = o.job_id \
             WHERE o.id = $1 AND a.attempt = $2 AND a.token = $3 \
             AND a.lease_until > extract(epoch FROM clock_timestamp())::bigint",
            &[&operation, &attempt.attempt, &attempt.token],
        )
        .await
        .map_err(database_error)?
    else {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    };
    let old_attempt: i64 = old.get("attempt");
    let same_attempt =
        old_attempt == attempt.attempt && old.get::<_, &str>("attempt_token") == attempt.token;
    if !same_attempt {
        if old_attempt >= attempt.attempt {
            return Ok(AdmissionOutcome::StaleOrDiverged);
        }
        if leases.get::<_, bool>("active") {
            return Ok(AdmissionOutcome::ClaimBusy);
        }
        let changed = tx
            .execute(
                "UPDATE posted_file_operations o SET attempt = a.attempt, attempt_token = a.token, \
                 claim_token = gen_random_uuid()::text, lease_until = a.lease_until \
                 FROM posted_file_job_attempts a WHERE o.id = $1 AND a.job_id = o.job_id \
                 AND a.attempt = $2 AND a.token = $3 AND o.attempt < a.attempt \
                 AND o.lease_until <= extract(epoch FROM clock_timestamp())::bigint \
                 AND a.lease_until > extract(epoch FROM clock_timestamp())::bigint",
                &[&operation, &attempt.attempt, &attempt.token],
            )
            .await
            .map_err(database_error)?;
        if changed != 1 {
            return Ok(AdmissionOutcome::StaleOrDiverged);
        }
    }
    let Some(handle) = read_handle(&tx, attempt, operation).await? else {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    };
    let bundle = stored(&tx, old.get("generation_id")).await?;
    if bundle.custody()?.plaintext_sha256 != expected.hash {
        return Err(StoreError::Inconsistent);
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(AdmissionOutcome::ResumedSameCandidate(handle, bundle))
}

/// The two distinct configured backbones in the immutable custody bundle.
/// Slot numbers resolve through that bundle's sealed plan, never a new local
/// article list. This adapter stores no article identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArticleSide {
    Post,
    Verify,
}

impl ArticleSide {
    fn db_value(self) -> i16 {
        match self {
            Self::Post => 0,
            Self::Verify => 1,
        }
    }
}

/// A successful mutation returns the refreshed durable fence. A stale caller
/// must recover/reselect rather than locally increment a revision. Incomplete
/// evidence is not terminal failure and never authorizes replacement.
#[derive(Debug)]
pub enum MutationOutcome {
    Applied(Box<GenerationHandle>),
    Incomplete,
    StaleOrDiverged,
}

// All candidate mutations use the same lock order: owning job/attempt, registry,
// then operation/health. Custody is immutable. The last read evaluates the DB
// clock AFTER any lock waits, so waiting cannot extend either lease.
async fn lock_candidate(
    tx: &Transaction<'_>,
    handle: &GenerationHandle,
) -> Result<Option<Row>, StoreError> {
    if !lock_attempt(tx, &handle.attempt).await? {
        return Ok(None);
    }
    let registry = tx
        .query_opt(
            "SELECT r.revision FROM posted_file_registry r
             JOIN posted_file_operations o ON o.plaintext_sha256 = r.plaintext_sha256
             WHERE o.id = $1 AND r.pending_operation = o.id AND r.revision = $2
             AND r.selected_generation IS NOT DISTINCT FROM $3::text FOR UPDATE OF r",
            &[
                &handle.operation,
                &handle.registry_revision,
                &handle.predecessor,
            ],
        )
        .await
        .map_err(database_error)?;
    if registry.is_none() {
        return Ok(None);
    }
    let current = tx
        .query_opt(
            "SELECT o.id FROM posted_file_operations o
             JOIN posted_file_health h ON h.generation_id = o.generation_id
             WHERE o.id = $1 AND o.generation_id = $2 AND o.claim_token = $3
             AND o.job_id = $4 AND o.attempt = $5 AND o.attempt_token = $6
             AND o.registry_revision = $7 AND o.predecessor IS NOT DISTINCT FROM $8::text
             AND o.predecessor_health_revision IS NOT DISTINCT FROM $9::bigint
             AND h.revision = $10 AND h.round = $11 AND h.round_token = $12
             AND h.state IN ('unknown', 'verifying') AND o.state = 'pending'
             AND o.lease_until > extract(epoch FROM clock_timestamp())::bigint FOR UPDATE OF o, h",
            &[
                &handle.operation,
                &handle.generation,
                &handle.claim_token,
                &handle.attempt.job_id,
                &handle.attempt.attempt,
                &handle.attempt.token,
                &handle.registry_revision,
                &handle.predecessor,
                &handle.predecessor_health_revision,
                &handle.health_revision,
                &handle.round,
                &handle.round_token,
            ],
        )
        .await
        .map_err(database_error)?
        .is_some();
    if !current {
        return Ok(None);
    }
    tx.query_opt(
        "SELECT p.article_count, h.verified_at, h.retention_checked_at, h.state, h.round \
         FROM posted_file_operations o JOIN posted_file_job_attempts a ON a.job_id = o.job_id \
         JOIN posted_file_health h ON h.generation_id = o.generation_id \
         JOIN posted_files p ON p.id = o.generation_id AND p.generation_operation = o.id \
         WHERE o.id = $1 AND a.attempt = o.attempt AND a.token = o.attempt_token \
         AND o.lease_until > extract(epoch FROM clock_timestamp())::bigint \
         AND a.lease_until > extract(epoch FROM clock_timestamp())::bigint",
        &[&handle.operation],
    )
    .await
    .map_err(database_error)
}

/// Extend both live leases for the exact current candidate, using the database
/// clock after all lock waits. An expired attempt or operation is never revived:
/// acquire/take over through the existing recovery path instead. No token,
/// revision, round, checkpoint, budget or custody changes. A lost commit response
/// requires recovery before more network work; renewal is not posting proof.
/// This is a delivery-boundary heartbeat, not a lock held across a request or a
/// timer that keeps a request alive after its original callback fence expires.
pub async fn renew_candidate_lease(
    client: &mut Client,
    handle: &GenerationHandle,
) -> Result<bool, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    if lock_candidate(&tx, handle).await?.is_none() {
        return Ok(false);
    }
    let renewed = tx
        .query_opt(
            "WITH db_clock AS MATERIALIZED (
                 SELECT extract(epoch FROM clock_timestamp())::bigint AS now
             ), deadline AS MATERIALIZED (
                 SELECT GREATEST(a.lease_until, o.lease_until, c.now + $3) AS until
                 FROM posted_file_job_attempts a
                 JOIN posted_file_operations o ON o.job_id = a.job_id
                 CROSS JOIN db_clock c
                 WHERE a.job_id = $1 AND o.id = $2
                 AND a.lease_until > c.now AND o.lease_until > c.now
             ), renewed_attempt AS (
                 UPDATE posted_file_job_attempts a SET lease_until = d.until
                 FROM deadline d WHERE a.job_id = $1 RETURNING a.lease_until
             )
             UPDATE posted_file_operations o SET lease_until = a.lease_until
             FROM renewed_attempt a WHERE o.id = $2 RETURNING o.id",
            &[&handle.attempt.job_id, &handle.operation, &LEASE_SECONDS],
        )
        .await
        .map_err(database_error)?
        .is_some();
    if !renewed {
        return Ok(false);
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(true)
}

/// Revalidate immediately before a network request, without holding locks
/// across that request. A request may complete externally after expiry; this
/// check does not authorize its later callback to bypass the same fence.
pub async fn posting_fence_is_current(
    client: &mut Client,
    handle: &GenerationHandle,
) -> Result<bool, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    let current = lock_candidate(&tx, handle)
        .await?
        .is_some_and(|r| r.get::<_, Option<i64>>("verified_at").is_none());
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(current)
}

async fn commit_mutation(
    tx: Transaction<'_>,
    handle: &GenerationHandle,
) -> Result<MutationOutcome, StoreError> {
    let refreshed = read_handle(&tx, &handle.attempt, &handle.operation)
        .await?
        .ok_or(StoreError::Inconsistent)?;
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(MutationOutcome::Applied(Box::new(refreshed)))
}

/// Start a distinct verification evidence round and its propagation schedule.
/// Old evidence, schedule and repost accounting are retained, not reused as a
/// new round's proof. Takeover/recovery never calls this to reset a budget.
/// This does not promote a candidate or grant retention/serving proof.
/// Ambiguous acknowledgments recover the committed round by operation identity.
pub async fn begin_verification_round(
    client: &mut Client,
    handle: &GenerationHandle,
) -> Result<MutationOutcome, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    if lock_candidate(&tx, handle).await?.is_none() {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    tx.execute(
        "UPDATE posted_file_health SET round = round + 1, round_token = gen_random_uuid()::text, \
         revision = revision + 1, state = 'verifying', failure_kind = NULL, \
         verified_at = NULL, retention_checked_at = NULL WHERE generation_id = $1",
        &[&handle.generation],
    )
    .await
    .map_err(database_error)?;
    rechecks::start_round(&tx, handle).await?;
    commit_mutation(tx, handle).await
}

/// Record confirmed presence for one required slot on one configured backbone.
/// A transport failure/unknown is deliberately not a confirmation or terminal
/// failure. Repeated current-round evidence is idempotent; stale fences write
/// nothing. The verifier still owns same-ID repair and budget policy.
pub async fn confirm_article(
    client: &mut Client,
    handle: &GenerationHandle,
    slot: i32,
    side: ArticleSide,
) -> Result<MutationOutcome, StoreError> {
    if slot < 0 {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(MutationOutcome::StaleOrDiverged);
    };
    if current.get::<_, &str>("state") != "verifying"
        || handle.round == 0
        || current.get::<_, Option<i64>>("verified_at").is_some()
    {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    if slot >= current.get::<_, i32>("article_count") {
        return Err(StoreError::InvalidInput);
    }
    let changed = tx
        .execute(
            "INSERT INTO posted_file_article_progress (generation_id, round, slot, side, confirmed) \
             VALUES ($1, $2, $3, $4, TRUE) ON CONFLICT (generation_id, round, slot, side) \
             DO UPDATE SET confirmed = TRUE, presence_verdict = NULL, \
             presence_repost_attempts = NULL WHERE NOT posted_file_article_progress.confirmed",
            &[&handle.generation, &handle.round, &slot, &side.db_value()],
        )
        .await
        .map_err(database_error)?;
    if changed == 1 {
        tx.execute(
            "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
            &[&handle.generation],
        )
        .await
        .map_err(database_error)?;
    }
    commit_mutation(tx, handle).await
}

/// Mark verification complete only for the entire distinct required slot set
/// on BOTH backbones, including recovery slots in the sealed plan. Verification
/// alone does not establish retention, select a winner or publish a pointer.
pub async fn finish_verification(
    client: &mut Client,
    handle: &GenerationHandle,
) -> Result<MutationOutcome, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(MutationOutcome::StaleOrDiverged);
    };
    if current.get::<_, &str>("state") != "verifying"
        || handle.round == 0
        || current.get::<_, Option<i64>>("verified_at").is_some()
    {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    let complete: bool = tx
        .query_one(
            "SELECT count(*) = $3::integer::bigint * 2 \
             AND COALESCE(bool_and(confirmed AND slot >= 0 AND slot < $3::integer), FALSE) \
             FROM posted_file_article_progress WHERE generation_id = $1 AND round = $2",
            &[
                &handle.generation,
                &handle.round,
                &current.get::<_, i32>("article_count"),
            ],
        )
        .await
        .map_err(database_error)?
        .get(0);
    if !complete {
        return Ok(MutationOutcome::Incomplete);
    }
    tx.execute(
        "UPDATE posted_file_health SET verified_at = extract(epoch FROM clock_timestamp())::bigint, \
         revision = revision + 1 WHERE generation_id = $1",
        &[&handle.generation],
    )
    .await
    .map_err(database_error)?;
    commit_mutation(tx, handle).await
}

/// Confirm retention for a verified candidate: stamps
/// `retention_checked_at` from the database clock and moves health to
/// `usable`. Requires a current handle on a verifying round whose
/// `verified_at` is present and whose retention stamp is still absent;
/// anything else is incomplete evidence, never a stamp. This is the only
/// writer of the `usable` state alongside [`promote_candidate`]'s fence;
/// it selects nothing and changes no custody, references or budgets.
/// After success the candidate is promotable but no longer mutable
/// through the pending-candidate path. On CommitUnknown, retry this
/// original handle: a fresh handle re-observes durable state.
pub async fn confirm_retention(
    client: &mut Client,
    handle: &GenerationHandle,
) -> Result<MutationOutcome, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(MutationOutcome::StaleOrDiverged);
    };
    if current.get::<_, &str>("state") != "verifying"
        || handle.round == 0
        || current.get::<_, Option<i64>>("verified_at").is_none()
        || current
            .get::<_, Option<i64>>("retention_checked_at")
            .is_some()
    {
        return Ok(MutationOutcome::Incomplete);
    }
    tx.execute(
        "UPDATE posted_file_health SET retention_checked_at = \
         extract(epoch FROM clock_timestamp())::bigint, state = 'usable', \
         revision = revision + 1 WHERE generation_id = $1",
        &[&handle.generation],
    )
    .await
    .map_err(database_error)?;
    commit_mutation(tx, handle).await
}

/// Result of the atomic publication step. `AlreadyPromoted` is a durable
/// receipt for this original handle only; it grants no posting authority.
/// `NotReady` means the handle fence is current but the candidate is not
/// yet `usable` — confirm retention first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionOutcome {
    Promoted,
    AlreadyPromoted,
    NotReady,
    StaleOrDiverged,
}

/// Atomically publish a usable candidate as the registry selection:
/// operation `pending` → `promoted`, registry selection moves from the
/// pinned predecessor to this generation, the in-flight reference becomes
/// serving, and a predecessor serving reference becomes rollback
/// (legacy pins are kept). Custody, evidence and history are preserved;
/// the predecessor health revision is re-checked under lock so a drifted
/// replacement basis refuses. Lock order matches admission: owning
/// job/attempt, registry, operation/health, predecessor health,
/// references; both leases are re-checked after lock waits with the DB
/// clock. No transaction spans network work. On CommitUnknown, retry
/// this original handle: a matching promoted receipt is idempotent, a
/// fresh handle must be re-observed. This publishes the generation only;
/// track-pointer wire-up, rollout and drain remain separate operations.
pub async fn promote_candidate(
    client: &mut Client,
    handle: &GenerationHandle,
) -> Result<PromotionOutcome, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_attempt(&tx, &handle.attempt).await? {
        return Ok(PromotionOutcome::StaleOrDiverged);
    }
    // Receipt for an earlier promotion: the operation is promoted, the
    // registry already selects this generation and nothing is pending.
    let receipt = tx
        .query_opt(
            "SELECT o.plaintext_sha256, r.selected_generation, r.pending_operation \
             FROM posted_file_operations o \
             JOIN posted_file_registry r ON r.plaintext_sha256 = o.plaintext_sha256 \
             WHERE o.id = $1 AND o.generation_id = $2 AND o.claim_token = $3 \
             AND o.job_id = $4 AND o.attempt = $5 AND o.attempt_token = $6 \
             AND o.registry_revision = $7 AND o.predecessor IS NOT DISTINCT FROM $8::text \
             AND o.predecessor_health_revision IS NOT DISTINCT FROM $9::bigint \
             AND o.state = 'promoted' AND r.selected_generation = o.generation_id \
             AND r.pending_operation IS NULL FOR UPDATE OF r, o",
            &[
                &handle.operation,
                &handle.generation,
                &handle.claim_token,
                &handle.attempt.job_id,
                &handle.attempt.attempt,
                &handle.attempt.token,
                &handle.registry_revision,
                &handle.predecessor,
                &handle.predecessor_health_revision,
            ],
        )
        .await
        .map_err(database_error)?;
    if receipt.is_some() {
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
        return Ok(PromotionOutcome::AlreadyPromoted);
    }
    let Some(registry) = tx
        .query_opt(
            "SELECT revision, selected_generation, pending_operation FROM posted_file_registry r \
             JOIN posted_file_operations o ON o.plaintext_sha256 = r.plaintext_sha256 \
             WHERE o.id = $1 AND r.pending_operation = o.id AND r.revision = $2 \
             AND r.selected_generation IS NOT DISTINCT FROM $3::text FOR UPDATE OF r",
            &[
                &handle.operation,
                &handle.registry_revision,
                &handle.predecessor,
            ],
        )
        .await
        .map_err(database_error)?
    else {
        return Ok(PromotionOutcome::StaleOrDiverged);
    };
    let Some(current) = tx
        .query_opt(
            "SELECT o.id, o.plaintext_sha256, o.claim_token, o.lease_until, h.state, \
             h.verified_at, h.retention_checked_at, h.revision, h.round, h.round_token \
             FROM posted_file_operations o \
             JOIN posted_file_health h ON h.generation_id = o.generation_id \
             WHERE o.id = $1 AND o.generation_id = $2 AND o.claim_token = $3 \
             AND o.job_id = $4 AND o.attempt = $5 AND o.attempt_token = $6 \
             AND o.registry_revision = $7 AND o.predecessor IS NOT DISTINCT FROM $8::text \
             AND o.predecessor_health_revision IS NOT DISTINCT FROM $9::bigint \
             AND h.revision = $10 AND h.round = $11 AND h.round_token = $12 \
             AND o.state = 'pending' FOR UPDATE OF o, h",
            &[
                &handle.operation,
                &handle.generation,
                &handle.claim_token,
                &handle.attempt.job_id,
                &handle.attempt.attempt,
                &handle.attempt.token,
                &handle.registry_revision,
                &handle.predecessor,
                &handle.predecessor_health_revision,
                &handle.health_revision,
                &handle.round,
                &handle.round_token,
            ],
        )
        .await
        .map_err(database_error)?
    else {
        return Ok(PromotionOutcome::StaleOrDiverged);
    };
    if current.get::<_, &str>("state") != "usable"
        || current.get::<_, Option<i64>>("verified_at").is_none()
        || current
            .get::<_, Option<i64>>("retention_checked_at")
            .is_none()
    {
        return Ok(PromotionOutcome::NotReady);
    }
    // Pin the predecessor basis under lock: a drifted predecessor health
    // refuses the publication rather than selecting on stale evidence.
    if let Some(predecessor) = &handle.predecessor {
        let prior = tx
            .query_opt(
                "SELECT revision FROM posted_file_health WHERE generation_id = $1 FOR UPDATE",
                &[predecessor],
            )
            .await
            .map_err(database_error)?;
        if prior
            .is_none_or(|r| Some(r.get::<_, i64>("revision")) != handle.predecessor_health_revision)
        {
            return Ok(PromotionOutcome::StaleOrDiverged);
        }
    }
    // Re-evaluate both leases with the DB clock AFTER all lock waits.
    let leases = tx
        .query_opt(
            "SELECT o.lease_until > extract(epoch FROM clock_timestamp())::bigint AS active \
             FROM posted_file_operations o JOIN posted_file_job_attempts a ON a.job_id = o.job_id \
             WHERE o.id = $1 AND a.attempt = $2 AND a.token = $3 \
             AND a.lease_until > extract(epoch FROM clock_timestamp())::bigint",
            &[
                &handle.operation,
                &handle.attempt.attempt,
                &handle.attempt.token,
            ],
        )
        .await
        .map_err(database_error)?;
    if leases.is_none_or(|r| !r.get::<_, bool>("active")) {
        return Ok(PromotionOutcome::StaleOrDiverged);
    }
    let hash: String = current.get("plaintext_sha256");
    let registry_revision: i64 = registry.get("revision");
    let promoted = tx
        .execute(
            "UPDATE posted_file_operations SET state = 'promoted' WHERE id = $1 AND state = 'pending'",
            &[&handle.operation],
        )
        .await
        .map_err(database_error)?;
    if promoted != 1 {
        return Err(StoreError::Inconsistent);
    }
    let published = tx
        .execute(
            "UPDATE posted_file_registry SET selected_generation = $3, pending_operation = NULL, \
             revision = revision + 1 WHERE plaintext_sha256 = $1 AND revision = $2 \
             AND pending_operation = $4 AND selected_generation IS NOT DISTINCT FROM $5::text",
            &[
                &hash,
                &registry_revision,
                &handle.generation,
                &handle.operation,
                &handle.predecessor,
            ],
        )
        .await
        .map_err(database_error)?;
    if published != 1 {
        return Err(StoreError::Inconsistent);
    }
    let removed = tx
        .execute(
            "DELETE FROM posted_file_references WHERE kind = 'in_flight' \
             AND owner_id = $1 AND generation_id = $2",
            &[&handle.operation, &handle.generation],
        )
        .await
        .map_err(database_error)?;
    if removed != 1 {
        return Err(StoreError::Inconsistent);
    }
    tx.execute(
        "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
         VALUES ('serving', $1, $2, $3)",
        &[&handle.operation, &handle.generation, &hash],
    )
    .await
    .map_err(database_error)?;
    // A predecessor serving reference becomes rollback; legacy pins stay.
    if let Some(predecessor) = &handle.predecessor {
        tx.execute(
            "UPDATE posted_file_references SET kind = 'rollback' \
             WHERE generation_id = $1 AND kind = 'serving'",
            &[predecessor],
        )
        .await
        .map_err(database_error)?;
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(PromotionOutcome::Promoted)
}

/// A stopped candidate is not a conclusively failed generation. None of these
/// outcomes grants fresh-post, serving, selection or cleanup authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbandonmentOutcome {
    Applied,
    AlreadyAbandoned,
    StaleOrDiverged,
}

/// Explicitly stop this candidate after a caller's keep-original/cancellation
/// decision. This does not infer terminal loss from transport noise, scheduling
/// or repair exhaustion. It never starts a replacement or releases custody.
///
/// The current handle fences the entire transaction. All references (including
/// in-flight), sealed custody and historical progress/evidence/budgets survive.
/// Old callbacks/takeovers cannot resume an abandoned operation. After an
/// ambiguous commit, retry THIS original handle: AlreadyAbandoned is a durable
/// receipt only, not a refreshed posting handle. An expired attempt must stop.
pub async fn abandon_candidate(
    client: &mut Client,
    handle: &GenerationHandle,
) -> Result<AbandonmentOutcome, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    if lock_candidate(&tx, handle).await?.is_none() {
        let abandoned = stopped_candidate_receipt(&tx, handle, CandidateStop::Abandoned).await?;
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
        return Ok(if abandoned {
            AbandonmentOutcome::AlreadyAbandoned
        } else {
            AbandonmentOutcome::StaleOrDiverged
        });
    }
    stop_candidate(&tx, handle, CandidateStop::Abandoned).await?;
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(AbandonmentOutcome::Applied)
}

#[derive(Clone, Copy)]
enum CandidateStop {
    Abandoned,
    VerificationFailed,
}

impl CandidateStop {
    fn health_state(self) -> &'static str {
        match self {
            Self::Abandoned => "unknown",
            Self::VerificationFailed => "terminal_failed",
        }
    }

    fn failure_kind(self) -> Option<&'static str> {
        match self {
            Self::Abandoned => None,
            Self::VerificationFailed => Some("verification"),
        }
    }
}

// A receipt is independent of later registry selection but must match the exact
// original operation/claim and health transition. It grants no posting handle.
async fn stopped_candidate_receipt(
    tx: &Transaction<'_>,
    handle: &GenerationHandle,
    stop: CandidateStop,
) -> Result<bool, StoreError> {
    if !lock_attempt(tx, &handle.attempt).await? {
        return Ok(false);
    }
    let applied_revision = handle
        .health_revision
        .checked_add(1)
        .ok_or(StoreError::Inconsistent)?;
    let applied_round = handle
        .round
        .checked_add(1)
        .ok_or(StoreError::Inconsistent)?;
    Ok(tx
        .query_opt(
            "SELECT o.id FROM posted_file_operations o \
             JOIN posted_files p ON p.id = o.generation_id AND p.generation_operation = o.id \
             JOIN posted_file_health h ON h.generation_id = o.generation_id \
             WHERE o.id = $1 AND o.generation_id = $2 AND o.claim_token = $3 \
             AND o.job_id = $4 AND o.attempt = $5 AND o.attempt_token = $6 \
             AND o.registry_revision = $7 AND o.predecessor IS NOT DISTINCT FROM $8::text \
             AND o.predecessor_health_revision IS NOT DISTINCT FROM $9::bigint \
             AND o.state = 'abandoned' AND h.state = $12 \
             AND h.failure_kind IS NOT DISTINCT FROM $13::text \
             AND h.verified_at IS NULL AND h.retention_checked_at IS NULL \
             AND h.revision = $10 AND h.round = $11",
            &[
                &handle.operation,
                &handle.generation,
                &handle.claim_token,
                &handle.attempt.job_id,
                &handle.attempt.attempt,
                &handle.attempt.token,
                &handle.registry_revision,
                &handle.predecessor,
                &handle.predecessor_health_revision,
                &applied_revision,
                &applied_round,
                &stop.health_state(),
                &stop.failure_kind(),
            ],
        )
        .await
        .map_err(database_error)?
        .is_some())
}

// Caller holds the complete candidate fence. All custody and references remain.
async fn stop_candidate(
    tx: &Transaction<'_>,
    handle: &GenerationHandle,
    stop: CandidateStop,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE posted_file_operations SET state = 'abandoned' WHERE id = $1",
        &[&handle.operation],
    )
    .await
    .map_err(database_error)?;
    tx.execute(
        "UPDATE posted_file_health SET state = $2, failure_kind = $3, \
         verified_at = NULL, retention_checked_at = NULL, revision = revision + 1, \
         round = round + 1, round_token = gen_random_uuid()::text WHERE generation_id = $1",
        &[
            &handle.generation,
            &stop.health_state(),
            &stop.failure_kind(),
        ],
    )
    .await
    .map_err(database_error)?;
    let changed = tx
        .execute(
            "UPDATE posted_file_registry SET pending_operation = NULL, revision = revision + 1 \
             WHERE pending_operation = $1 AND revision = $2 \
             AND selected_generation IS NOT DISTINCT FROM $3::text",
            &[
                &handle.operation,
                &handle.registry_revision,
                &handle.predecessor,
            ],
        )
        .await
        .map_err(database_error)?;
    if changed != 1 {
        return Err(StoreError::Inconsistent);
    }
    Ok(())
}

/// Durable evidence for one slot on one configured backbone. POST success is
/// accounting only: it never sets `confirmed` or supplies content-integrity proof.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct SlotProgress {
    pub confirmed: bool,
    pub repost_attempts: u32,
    /// Latest explicit STAT verdict, only if no answered repost has occurred
    /// since that observation. None is unknown, not Missing. Direct confirmation
    /// and pre-migration rows carry no STAT provenance. No verdict declares loss.
    pub presence: Option<crate::two_spool::StatVerdict>,
}

/// Current-round checkpoint bound to the stored generation/plan. Missing rows
/// mean unconfirmed/zero attempts, not a new local article set. This is a read
/// snapshot, not POST authority, and must not be used to overwrite durable state
/// from a job-keyed KV blob. The schedule persists propagation/backoff and pass
/// counts; policy/provenance and queue delivery remain with the verifier. A
/// stopped schedule is never terminal negative health.
pub struct GenerationCheckpoint {
    generation: String,
    plan: String,
    round: i64,
    slots: Vec<[SlotProgress; 2]>,
    schedule: Option<RecheckSchedule>,
    content_checks: [Option<ContentCheckState>; 2],
    presence_verified: bool,
}

impl fmt::Debug for GenerationCheckpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GenerationCheckpoint([REDACTED])")
    }
}

impl GenerationCheckpoint {
    pub fn generation_id(&self) -> &str {
        &self.generation
    }

    pub fn plan_sha256(&self) -> &str {
        &self.plan
    }

    pub fn round(&self) -> i64 {
        self.round
    }

    pub fn article_count(&self) -> usize {
        self.slots.len()
    }

    pub fn recheck_schedule(&self) -> Option<RecheckSchedule> {
        self.schedule
    }

    /// Current-round observations only, never a retention or cleanup permit.
    /// Absence, unavailability or failure must retain the R2 safety-net copy.
    pub fn content_check(&self, side: ArticleSide) -> Option<ContentCheckState> {
        self.content_checks[side.db_value() as usize]
    }

    pub fn slot(&self, slot: usize, side: ArticleSide) -> Option<SlotProgress> {
        self.slots.get(slot).map(|s| s[side.db_value() as usize])
    }
}

/// A single same-ID repost callback capability, prepared from durable custody
/// and accounting under the current fence. It deliberately retains the exact
/// health revision/round/claim: never reconstruct one from a new handle after an
/// ambiguous callback commit. Recover by operation identity and load checkpoint
/// instead. A repeated/stale callback cannot increment the next attempt.
#[derive(Clone)]
pub struct RepostPermit {
    handle: GenerationHandle,
    slot: i32,
    side: ArticleSide,
    expected_attempts: i32,
}

impl fmt::Debug for RepostPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RepostPermit([REDACTED])")
    }
}

#[derive(Debug)]
pub enum RepostPreparation {
    Ready(Box<RepostPermit>),
    AlreadyConfirmed,
    BudgetExhausted,
    StaleOrDiverged,
}

fn slot_progress(row: &Row) -> Result<SlotProgress, StoreError> {
    let attempts: i32 = row.get("repost_attempts");
    let repost_attempts = u32::try_from(attempts).map_err(|_| StoreError::Inconsistent)?;
    if repost_attempts > MAX_REPOST_ATTEMPTS_PER_ARTICLE {
        return Err(StoreError::Inconsistent);
    }
    let confirmed: bool = row.get("confirmed");
    let verdict: Option<&str> = row.get("presence_verdict");
    let observed_attempts: Option<i32> = row.get("presence_repost_attempts");
    let presence = match (verdict, observed_attempts) {
        (None, None) => None,
        (Some(verdict), Some(observed)) if (0..=attempts).contains(&observed) => {
            let verdict = match verdict {
                "present" if confirmed => crate::two_spool::StatVerdict::Present,
                "missing" if !confirmed => crate::two_spool::StatVerdict::Missing,
                "unavailable" if !confirmed => crate::two_spool::StatVerdict::Transport,
                _ => return Err(StoreError::Inconsistent),
            };
            (observed == attempts).then_some(verdict)
        }
        _ => return Err(StoreError::Inconsistent),
    };
    Ok(SlotProgress {
        confirmed,
        repost_attempts,
        presence,
    })
}

/// Read a coherent checkpoint under the same lock order and revision fence as
/// mutations. Both backbone lanes and all required slots (including recovery)
/// are addressed by the immutable sealed plan. No article identities are read.
/// `None` means stale, not an empty checkpoint or fresh-post permission.
pub async fn load_checkpoint(
    client: &mut Client,
    handle: &GenerationHandle,
) -> Result<Option<GenerationCheckpoint>, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(None);
    };
    let checkpoint = checkpoint_locked(&tx, handle, &current).await?;
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(Some(checkpoint))
}

async fn checkpoint_locked(
    tx: &Transaction<'_>,
    handle: &GenerationHandle,
    current: &Row,
) -> Result<GenerationCheckpoint, StoreError> {
    checkpoint_generation_locked(
        tx,
        &handle.generation,
        handle.round,
        current.get("article_count"),
        current.get::<_, Option<i64>>("verified_at").is_some(),
    )
    .await
}

async fn checkpoint_generation_locked(
    tx: &Transaction<'_>,
    generation: &str,
    round: i64,
    count: i32,
    presence_verified: bool,
) -> Result<GenerationCheckpoint, StoreError> {
    if !(1..=MAX_ARTICLE_COUNT).contains(&count) {
        return Err(StoreError::Inconsistent);
    }
    let bundle = stored(tx, generation).await?;
    let mut slots = vec![[SlotProgress::default(); 2]; count as usize];
    for row in tx
        .query(
            "SELECT slot, side, confirmed, repost_attempts, presence_verdict, \
             presence_repost_attempts FROM posted_file_article_progress \
             WHERE generation_id = $1 AND round = $2",
            &[&generation, &round],
        )
        .await
        .map_err(database_error)?
    {
        let slot: i32 = row.get("slot");
        let side: i16 = row.get("side");
        if !(0..count).contains(&slot) || !(0..=1).contains(&side) {
            return Err(StoreError::Inconsistent);
        }
        slots[slot as usize][side as usize] = slot_progress(&row)?;
    }
    let checkpoint = GenerationCheckpoint {
        generation: generation.to_owned(),
        plan: bundle.plan_sha256().to_owned(),
        round,
        slots,
        schedule: rechecks::load_generation_schedule(tx, generation, round).await?,
        content_checks: content::load_generation_checks(tx, generation, round).await?,
        presence_verified,
    };
    Ok(checkpoint)
}

/// Prepare immediately before same-ID network repair, with no transaction held
/// over that request. Caller policy must have established a missing slot and
/// must use the stored bundle's exact bytes/ID, including confirmed-duplicate
/// client handling. This permit is not a lease extension or a network exactly-
/// once guarantee; an in-flight request can complete after the fence expires.
/// Answered accounting is capped per slot/backbone/round by the existing policy.
/// Exhaustion keeps the original; it is NOT conclusive lost-content evidence.
pub async fn prepare_repost(
    client: &mut Client,
    handle: &GenerationHandle,
    slot: i32,
    side: ArticleSide,
) -> Result<RepostPreparation, StoreError> {
    if slot < 0 {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(RepostPreparation::StaleOrDiverged);
    };
    if slot >= current.get::<_, i32>("article_count") {
        return Err(StoreError::InvalidInput);
    }
    if current.get::<_, &str>("state") != "verifying"
        || handle.round == 0
        || current.get::<_, Option<i64>>("verified_at").is_some()
    {
        return Ok(RepostPreparation::StaleOrDiverged);
    }
    let progress = tx
        .query_opt(
            "SELECT confirmed, repost_attempts, presence_verdict, presence_repost_attempts \
             FROM posted_file_article_progress \
             WHERE generation_id = $1 AND round = $2 AND slot = $3 AND side = $4",
            &[&handle.generation, &handle.round, &slot, &side.db_value()],
        )
        .await
        .map_err(database_error)?
        .as_ref()
        .map(slot_progress)
        .transpose()?
        .unwrap_or_default();
    let result = if progress.confirmed {
        RepostPreparation::AlreadyConfirmed
    } else if progress.repost_attempts >= MAX_REPOST_ATTEMPTS_PER_ARTICLE {
        RepostPreparation::BudgetExhausted
    } else {
        RepostPreparation::Ready(Box::new(RepostPermit {
            handle: handle.clone(),
            slot,
            side,
            expected_attempts: progress.repost_attempts as i32,
        }))
    };
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(result)
}

/// Commit one answered callback against the original permit, incrementing the
/// slot counter and health revision atomically. Duplicates, parallel callbacks,
/// takeover and old rounds fail the same revision fence, with no counter write.
/// Transport/auth-retryable and prohibited outcomes spend no budget and return
/// Incomplete; their retry/keep-original disposition belongs to verifier policy,
/// never a terminal health write or replacement authorization in this adapter.
/// Posted/confirmed-duplicate outcomes do NOT confirm presence or content. An
/// answered repair withdraws any earlier intact content observation on this
/// side/round in the SAME accounting transaction; it requires a fresh read.
/// Other sides/rounds and nonaffirmative observations remain unchanged.
pub async fn record_repost(
    client: &mut Client,
    permit: &RepostPermit,
    verdict: PostVerdict,
) -> Result<MutationOutcome, StoreError> {
    let handle = &permit.handle;
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(MutationOutcome::StaleOrDiverged);
    };
    if current.get::<_, &str>("state") != "verifying"
        || handle.round == 0
        || current.get::<_, Option<i64>>("verified_at").is_some()
    {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    if !matches!(verdict, PostVerdict::Posted | PostVerdict::DuplicatePresent) {
        return Ok(MutationOutcome::Incomplete);
    }
    // Refuse inconsistent stored evidence before accounting or invalidating it.
    content::load_checks(&tx, handle).await?;
    let changed = tx
        .execute(
            "INSERT INTO posted_file_article_progress AS p \
             (generation_id, round, slot, side, repost_attempts) \
             SELECT $1, $2, $3, $4, 1 WHERE $5::integer = 0 OR EXISTS \
             (SELECT 1 FROM posted_file_article_progress WHERE generation_id = $1 \
              AND round = $2 AND slot = $3 AND side = $4) \
             ON CONFLICT (generation_id, round, slot, side) DO UPDATE \
             SET repost_attempts = p.repost_attempts + 1 \
             WHERE NOT p.confirmed AND p.repost_attempts = $5 \
             AND p.repost_attempts < $6",
            &[
                &handle.generation,
                &handle.round,
                &permit.slot,
                &permit.side.db_value(),
                &permit.expected_attempts,
                &(MAX_REPOST_ATTEMPTS_PER_ARTICLE as i32),
            ],
        )
        .await
        .map_err(database_error)?;
    if changed != 1 {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    tx.execute(
        "UPDATE posted_file_content_checks SET state = 'unavailable', checked_articles = 0, \
         checked_at = floor(extract(epoch FROM clock_timestamp()))::bigint \
         WHERE generation_id = $1 AND round = $2 AND side = $3 AND state = 'intact'",
        &[&handle.generation, &handle.round, &permit.side.db_value()],
    )
    .await
    .map_err(database_error)?;
    tx.execute(
        "UPDATE posted_file_health SET revision = revision + 1, \
         verified_at = NULL, retention_checked_at = NULL WHERE generation_id = $1",
        &[&handle.generation],
    )
    .await
    .map_err(database_error)?;
    commit_mutation(tx, handle).await
}

/// Discover the pending operation from the hash registry, not a Queue/KV hint.
/// The first read is only discovery: `recover` revalidates the discovered exact
/// operation under the owning persisted attempt and all current candidate fences.
/// A concurrent change refuses rather than falling back to a different candidate.
/// Missing/stopped/foreign/expired work never authorizes a mint or implicit
/// takeover. No lease, round, cursor or budget is changed by discovery/recovery.
/// This does not recover a lost `acquire_attempt` acknowledgment or construct
/// attempt authority from queue-carried numbers.
pub async fn recover_pending(
    client: &mut Client,
    attempt: &AttemptHandle,
    hash: &str,
) -> Result<AdmissionOutcome, StoreError> {
    if !crate::two_spool::validate_plaintext_sha256(hash) {
        return Err(StoreError::InvalidInput);
    }
    let operation: Option<String> = client
        .query_opt(
            "SELECT pending_operation FROM posted_file_registry WHERE plaintext_sha256 = $1",
            &[&hash],
        )
        .await
        .map_err(database_error)?
        .and_then(|row| row.get("pending_operation"));
    let Some(operation) = operation else {
        return Ok(AdmissionOutcome::InconsistentOrMissing);
    };
    recover(client, attempt, hash, &operation).await
}

/// Lost admission acknowledgment: reread by durable operation identity before
/// any POST. Recheck the complete candidate fence and both leases after all lock
/// waits, as on exact-mint admission replay. This never renews a lease, inserts,
/// changes a plan, rotates a token or resets a counter. An expired handle needs
/// explicit fenced takeover, not this method. Revalidate again immediately before
/// each network request; this returned snapshot is not a lease on future work.
pub async fn recover(
    client: &mut Client,
    attempt: &AttemptHandle,
    hash: &str,
    operation: &str,
) -> Result<AdmissionOutcome, StoreError> {
    if !identity(operation) || !crate::two_spool::validate_plaintext_sha256(hash) {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_attempt(&tx, attempt).await? {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    }
    let exists = tx
        .query_opt(
            "SELECT revision FROM posted_file_registry WHERE plaintext_sha256 = $1 FOR UPDATE",
            &[&hash],
        )
        .await
        .map_err(database_error)?;
    if exists.is_none() {
        return Ok(AdmissionOutcome::InconsistentOrMissing);
    }
    let Some(handle) = read_handle(&tx, attempt, operation).await? else {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    };
    let bundle = stored(&tx, &handle.generation).await?;
    if bundle.custody()?.plaintext_sha256 != hash {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    }
    if lock_candidate(&tx, &handle).await?.is_none() {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(AdmissionOutcome::ResumedSameCandidate(handle, bundle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_bounded_and_has_no_control_characters() {
        assert!(identity("operation_1-2"));
        for invalid in ["", "a b", "a/b", "a\n", "é"] {
            assert!(!identity(invalid));
        }
        assert!(!identity(&"a".repeat(65)));
    }

    #[test]
    fn mutation_outcome_keeps_the_handle_out_of_line() {
        assert!(std::mem::size_of::<MutationOutcome>() <= 2 * std::mem::size_of::<usize>());
    }

    #[test]
    fn handles_never_debug_tokens_or_identities() {
        let attempt = AttemptHandle {
            job_id: "private-job".into(),
            owner_id: "private-owner".into(),
            attempt: 3,
            token: "private-attempt-token".into(),
        };
        let handle = GenerationHandle {
            attempt: attempt.clone(),
            operation: "private-operation".into(),
            generation: "private-generation".into(),
            claim_token: "private-claim-token".into(),
            registry_revision: 2,
            predecessor: None,
            predecessor_health_revision: None,
            health_revision: 0,
            round: 0,
            round_token: "private-round-token".into(),
        };
        let permit = RepostPermit {
            handle: handle.clone(),
            slot: 2,
            side: ArticleSide::Verify,
            expected_attempts: 1,
        };
        assert_eq!(format!("{permit:?}"), "RepostPermit([REDACTED])");
        assert_eq!(
            format!("{:?}", RepostPreparation::Ready(Box::new(permit))),
            "Ready(RepostPermit([REDACTED]))"
        );
        let checkpoint = GenerationCheckpoint {
            generation: "private-generation".into(),
            plan: "private-plan".into(),
            round: 7,
            slots: vec![[SlotProgress::default(); 2]; 3],
            schedule: Some(RecheckSchedule::Untracked),
            content_checks: [None, Some(ContentCheckState::Unavailable)],
            presence_verified: false,
        };
        assert_eq!(
            format!("{checkpoint:?}"),
            "GenerationCheckpoint([REDACTED])"
        );
        assert_eq!(checkpoint.article_count(), 3);
        assert!(checkpoint.slot(3, ArticleSide::Post).is_none());
        assert!(checkpoint.slot(usize::MAX, ArticleSide::Verify).is_none());
        assert_eq!(format!("{attempt:?}"), "AttemptHandle([REDACTED])");
        assert_eq!(format!("{handle:?}"), "GenerationHandle([REDACTED])");
        assert_eq!(
            format!("{:?}", RecheckStep::Progress(Box::new(handle.clone()))),
            "Progress(GenerationHandle([REDACTED]))"
        );
        assert_eq!(
            format!(
                "{:?}",
                RecheckStep::PresenceComplete(Box::new(handle.clone()))
            ),
            "PresenceComplete(GenerationHandle([REDACTED]))"
        );
        assert_eq!(
            format!("{:?}", MutationOutcome::Applied(Box::new(handle))),
            "Applied(GenerationHandle([REDACTED]))"
        );
        assert_eq!(ArticleSide::Post.db_value(), 0);
        assert_eq!(ArticleSide::Verify.db_value(), 1);
        assert_eq!(
            format!("{:?}", AttemptOutcome::Acquired(attempt)),
            "Acquired(AttemptHandle([REDACTED]))"
        );
    }
}
