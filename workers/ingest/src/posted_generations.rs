//! Short PostgreSQL transactions for immutable posted-file custody.
//!
//! Preparation only: migration 0037 retains the unique hash constraint, so
//! replacement is refused until the legacy writer/publication rollout is
//! complete. No queue, NNTP, KV or pointer caller is wired here. An admitted
//! bundle is custody, NOT verification, registry promotion or serving authority.
//! No transaction may span a network POST. Commit errors are ambiguous and
//! require operation-identity recovery, never a remint or optimistic POST.
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

/// Persisted attempt authority, minted only by `acquire_attempt`. Queue-carried
/// attempt numbers do not construct this handle and are never sufficient.
#[derive(Clone)]
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
#[derive(Clone)]
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
            header_nonce: r.try_get("header_nonce").map_err(database_error)?,
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
/// expired attempt it observed. Job status/owner are locked through commit.
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

async fn lock_attempt(tx: &Transaction<'_>, a: &AttemptHandle) -> Result<bool, StoreError> {
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

/// Atomic absent-hash admission. The preparation schema deliberately refuses
/// terminal-predecessor replacement until the complete rollout fence lands.
/// A zero-row insert is never interpreted as admission of the caller's mint.
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
    tx.execute(
        "INSERT INTO posted_file_registry (plaintext_sha256, selected_generation) \
         SELECT $1, (SELECT id FROM posted_files WHERE plaintext_sha256 = $1) \
         ON CONFLICT (plaintext_sha256) DO NOTHING",
        &[&expected.hash],
    )
    .await
    .map_err(database_error)?;
    let registry = tx
        .query_one(
            "SELECT selected_generation, revision, pending_operation FROM posted_file_registry \
             WHERE plaintext_sha256 = $1 FOR UPDATE",
            &[&expected.hash],
        )
        .await
        .map_err(database_error)?;
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
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
        return Ok(AdmissionOutcome::ResumedSameCandidate(handle, bundle));
    }
    let selected: Option<String> = registry.get("selected_generation");
    if registry.get::<_, i64>("revision") != expected.revision || selected != expected.selected {
        return Ok(AdmissionOutcome::StaleOrDiverged);
    }
    if let Some(id) = selected {
        let Some(health) = tx
            .query_opt(
                "SELECT state, revision FROM posted_file_health WHERE generation_id = $1 FOR UPDATE",
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
        return Ok(match health.get::<_, &str>("state") {
            // The complete reuse/publication lease is not implemented in this
            // preparation slice. Do not return an unfenced reusable bundle.
            "usable" | "terminal_failed" => AdmissionOutcome::InconsistentOrMissing,
            _ => AdmissionOutcome::ClaimBusy,
        });
    }
    if expected.health_revision.is_some() {
        return Ok(AdmissionOutcome::InconsistentOrMissing);
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
             manifest_dek_wrapped, manifest_payload_nonce, manifest_payload_format) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,2,$13,$14,$15,$16,$17,$18,2,$19,$20,$21,$22,$23) \
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
                &p.header_nonce,
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
             attempt, attempt_token, claim_token, lease_until, registry_revision) \
             SELECT $1,$2,$3,a.job_id,a.attempt,a.token,gen_random_uuid()::text,a.lease_until,$7 \
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
             WHERE plaintext_sha256 = $1 AND revision = $4 AND selected_generation IS NULL \
             AND pending_operation IS NULL",
            &[
                &expected.hash,
                &mint.operation_id,
                &revision,
                &expected.revision,
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
        "SELECT p.article_count, h.verified_at, h.state, h.round \
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

/// Start a distinct verification evidence round. Old evidence is retained but
/// cannot satisfy the new round. This does not reset repost accounting or
/// checkpoint budgets, promote a candidate, or grant retention/serving proof.
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
             DO UPDATE SET confirmed = TRUE WHERE NOT posted_file_article_progress.confirmed",
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

/// Durable evidence for one slot on one configured backbone. POST success is
/// accounting only: it never sets `confirmed` or supplies content-integrity proof.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct SlotProgress {
    pub confirmed: bool,
    pub repost_attempts: u32,
}

/// Current-round checkpoint bound to the stored generation/plan. Missing rows
/// mean unconfirmed/zero attempts, not a new local article set. This is a read
/// snapshot, not POST authority, and must not be used to overwrite durable state
/// from a job-keyed KV blob. Recheck scheduling/negative-health policy stays with
/// the verifier; this preparation does not yet persist that scheduler state.
pub struct GenerationCheckpoint {
    generation: String,
    plan: String,
    round: i64,
    slots: Vec<[SlotProgress; 2]>,
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
    Ok(SlotProgress {
        confirmed: row.get("confirmed"),
        repost_attempts,
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
    let count = current.get::<_, i32>("article_count");
    if !(1..=MAX_ARTICLE_COUNT).contains(&count) {
        return Err(StoreError::Inconsistent);
    }
    let bundle = stored(&tx, &handle.generation).await?;
    let mut slots = vec![[SlotProgress::default(); 2]; count as usize];
    for row in tx
        .query(
            "SELECT slot, side, confirmed, repost_attempts FROM posted_file_article_progress \
             WHERE generation_id = $1 AND round = $2",
            &[&handle.generation, &handle.round],
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
        generation: handle.generation.clone(),
        plan: bundle.plan_sha256().to_owned(),
        round: handle.round,
        slots,
    };
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(Some(checkpoint))
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
            "SELECT confirmed, repost_attempts FROM posted_file_article_progress \
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
/// Posted/confirmed-duplicate outcomes do NOT confirm presence or content.
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
        "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
        &[&handle.generation],
    )
    .await
    .map_err(database_error)?;
    commit_mutation(tx, handle).await
}

/// Lost admission acknowledgment: reread by durable operation identity before
/// any POST. This never inserts, changes a plan, rotates a token or resets a
/// counter. An expired handle needs explicit fenced takeover, not this method.
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
