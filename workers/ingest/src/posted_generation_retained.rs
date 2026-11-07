//! Fenced health observations for an already retained generation.
//!
//! This is not candidate verification or a new health scheduler. Preparation is
//! read-only and captures the original job-attempt, reference and health fence
//! together with immutable custody. The reference token binds the original
//! incarnation, so removing and recreating an association cannot revive a read.
//! No transaction spans a reader. An explicit
//! unknown/failed read may withdraw eligibility through that exact permit only.
//! Complete-read observations do not establish independent-backbone provenance,
//! affirmative retention, terminal loss, reuse or cleanup authority. Negative
//! mutations atomically retain original attempt/reference/health and pre/post
//! registry context in an immutable observation ledger; no conclusive failure
//! decision is inferred from those observations.

use super::*;

#[path = "posted_generation_retained_content.rs"]
mod content_reads;
pub use content_reads::{
    drive_retained_content_read, prepare_retained_content_check, record_retained_content_check,
    RetainedContentPermit,
};

/// Existing durable reference kinds eligible for retained health work. Pending
/// candidates use the claim-bound verifier, never this path's weaker fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedReferenceKind {
    LegacyPin,
    Serving,
    Rollback,
}

impl RetainedReferenceKind {
    fn db_value(self) -> &'static str {
        match self {
            Self::LegacyPin => "legacy_pin",
            Self::Serving => "serving",
            Self::Rollback => "rollback",
        }
    }
}

/// Private original-read fence; never reconstructed from a hash or refreshed
/// after the read to make an old failure current. Clones are the same permit.
#[derive(Clone)]
pub struct RetainedCheckPermit {
    attempt: AttemptHandle,
    generation: String,
    reference_kind: RetainedReferenceKind,
    reference_owner: String,
    reference_token: String,
    registry_revision: i64,
    selected_generation: Option<String>,
    revision: i64,
    round: i64,
    round_token: String,
}

impl fmt::Debug for RetainedCheckPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RetainedCheckPermit([REDACTED])")
    }
}

/// Retained rows may predate the canonical plan hash. Expose custody only,
/// never the managed-generation plan accessor or an affirmative proof handle.
pub struct RetainedCustody {
    stored: StoredGeneration,
}

impl fmt::Debug for RetainedCustody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RetainedCustody([REDACTED])")
    }
}

impl RetainedCustody {
    pub fn custody(&self) -> Result<PostedFileParams<'_>, StoreError> {
        self.stored.custody()
    }
}

/// A committed retained observation/invalidation, never eligibility authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedInvalidation {
    Applied,
    StaleOrMissing,
}

struct RetainedFence {
    health: Row,
    reference_token: String,
    registry_revision: i64,
    selected_generation: Option<String>,
}

// Match candidate/publication lock order: owning job/attempt, hash registry,
// generation operation/health, then reference. Retained work need not target the
// currently selected winner: a superseded rollback/serving reference still owns
// its exact custody. Never mutate the registry through this path.
async fn lock_retained(
    tx: &Transaction<'_>,
    attempt: &AttemptHandle,
    generation: &str,
    kind: RetainedReferenceKind,
    owner: &str,
) -> Result<Option<RetainedFence>, StoreError> {
    if !lock_attempt(tx, attempt).await? {
        return Ok(None);
    }
    let Some(registry) = tx
        .query_opt(
            "SELECT r.revision, r.selected_generation FROM posted_file_registry r \
             JOIN posted_files p ON p.plaintext_sha256 = r.plaintext_sha256 \
             WHERE p.id = $1 FOR UPDATE OF r",
            &[&generation],
        )
        .await
        .map_err(database_error)?
    else {
        return Ok(None);
    };
    let operation = tx
        .query_opt(
            "SELECT o.state FROM posted_file_operations o \
             JOIN posted_files p ON p.generation_operation = o.id AND p.id = o.generation_id \
             WHERE p.id = $1 FOR UPDATE OF o",
            &[&generation],
        )
        .await
        .map_err(database_error)?;
    if operation.is_some_and(|r| r.get::<_, &str>("state") != "promoted") {
        return Ok(None);
    }
    let health = tx
        .query_opt(
            "SELECT revision, round, round_token FROM posted_file_health \
             WHERE generation_id = $1 AND state IN ('unknown', 'usable') FOR UPDATE",
            &[&generation],
        )
        .await
        .map_err(database_error)?;
    let Some(health) = health else {
        return Ok(None);
    };
    let Some(reference) = tx
        .query_opt(
            "SELECT reference_token FROM posted_file_references \
             WHERE generation_id = $1 AND kind = $2 AND owner_id = $3 FOR UPDATE",
            &[&generation, &kind.db_value(), &owner],
        )
        .await
        .map_err(database_error)?
    else {
        return Ok(None);
    };
    // The DB clock must be sampled again AFTER every possible lock wait.
    if !lock_attempt(tx, attempt).await? {
        return Ok(None);
    }
    Ok(Some(RetainedFence {
        health,
        reference_token: reference.get("reference_token"),
        registry_revision: registry.get("revision"),
        selected_generation: registry.get("selected_generation"),
    }))
}

/// Prepare one referenced-generation read with its original persisted fence.
/// Missing references, live candidates and terminal health fail closed. A null
/// legacy plan hash is not authenticated-plan evidence; this path grants none.
/// The caller must validate custody before reading and keep R2 on every error.
pub async fn prepare_retained_check(
    client: &mut Client,
    attempt: &AttemptHandle,
    generation: &str,
    kind: RetainedReferenceKind,
    owner: &str,
) -> Result<Option<(RetainedCheckPermit, RetainedCustody)>, StoreError> {
    if !identity(generation) || !identity(owner) {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(fence) = lock_retained(&tx, attempt, generation, kind, owner).await? else {
        return Ok(None);
    };
    let row = tx
        .query_one("SELECT * FROM posted_files WHERE id = $1", &[&generation])
        .await
        .map_err(database_error)?;
    let custody = RetainedCustody {
        stored: StoredGeneration { row },
    };
    custody.custody()?;
    let permit = RetainedCheckPermit {
        attempt: attempt.clone(),
        generation: generation.to_owned(),
        reference_kind: kind,
        reference_owner: owner.to_owned(),
        reference_token: fence.reference_token,
        registry_revision: fence.registry_revision,
        selected_generation: fence.selected_generation,
        revision: fence.health.get("revision"),
        round: fence.health.get("round"),
        round_token: fence.health.get("round_token"),
    };
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(Some((permit, custody)))
}

/// Withdraw retained eligibility after an explicit unknown/failed read. This
/// does NOT classify terminal failure or authorize a replacement. A fresh round
/// token fences every old observation; old evidence/budgets and all custody and
/// references remain stored. Replay of an applied permit returns stale, not a
/// second mutation. On CommitUnknown, re-observe; never remint a permit and apply
/// the old network result to it. Cancellation records nothing.
pub async fn invalidate_retained_check(
    client: &mut Client,
    permit: &RetainedCheckPermit,
) -> Result<RetainedInvalidation, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(health) = lock_retained(
        &tx,
        &permit.attempt,
        &permit.generation,
        permit.reference_kind,
        &permit.reference_owner,
    )
    .await?
    else {
        return Ok(RetainedInvalidation::StaleOrMissing);
    };
    if !matches_fence(&health, permit) {
        return Ok(RetainedInvalidation::StaleOrMissing);
    }
    store_negative_provenance(&tx, permit, &health, None, "unknown").await?;
    withdraw_health(&tx, &permit.generation).await?;
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(RetainedInvalidation::Applied)
}

fn matches_fence(fence: &RetainedFence, permit: &RetainedCheckPermit) -> bool {
    fence.reference_token == permit.reference_token
        && fence.health.get::<_, i64>("revision") == permit.revision
        && fence.health.get::<_, i64>("round") == permit.round
        && fence.health.get::<_, &str>("round_token") == permit.round_token
}

// Persist the ORIGINAL read and current registry context in the same commit as
// the invalidation. Historical serving/rollback reads remain valid after global
// selection moves, but cannot be mistaken for a check of the new selection.
// This ledger contains observations, never a terminal-failure decision. No FK
// targets a mutable attempt/reference incarnation, which must be free to advance.
async fn store_negative_provenance(
    tx: &Transaction<'_>,
    permit: &RetainedCheckPermit,
    current: &RetainedFence,
    side: Option<ArticleSide>,
    state: &str,
) -> Result<(), StoreError> {
    let side = side.map(ArticleSide::db_value);
    let changed = tx
        .execute(
            "INSERT INTO posted_file_retained_observations \
             (generation_id, plaintext_sha256, health_revision, round, round_token, \
              job_id, attempt, attempt_token, reference_kind, reference_owner, reference_token, \
              prepared_registry_revision, prepared_selection, recorded_registry_revision, \
              recorded_selection, side, state) \
             SELECT id, plaintext_sha256, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
                    $11, $12, $13, $14, $15, $16 FROM posted_files WHERE id = $1",
            &[
                &permit.generation,
                &permit.revision,
                &permit.round,
                &permit.round_token,
                &permit.attempt.job_id,
                &permit.attempt.attempt,
                &permit.attempt.token,
                &permit.reference_kind.db_value(),
                &permit.reference_owner,
                &permit.reference_token,
                &permit.registry_revision,
                &permit.selected_generation,
                &current.registry_revision,
                &current.selected_generation,
                &side,
                &state,
            ],
        )
        .await
        .map_err(database_error)?;
    if changed != 1 {
        return Err(StoreError::Inconsistent);
    }
    Ok(())
}

async fn withdraw_health(tx: &Transaction<'_>, generation: &str) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE posted_file_health SET state = 'unknown', failure_kind = NULL, \
         verified_at = NULL, retention_checked_at = NULL, revision = revision + 1, \
         round = round + 1, round_token = gen_random_uuid()::text WHERE generation_id = $1",
        &[&generation],
    )
    .await
    .map_err(database_error)?;
    Ok(())
}
