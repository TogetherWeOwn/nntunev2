//! Fenced health observations for an already retained generation.
//!
//! This is not candidate verification or a new health scheduler. Preparation is
//! read-only and captures the original job-attempt, reference and health fence
//! together with immutable custody. No transaction spans a reader. An explicit
//! unknown/failed read may withdraw eligibility through that exact permit only.
//! Complete-read observations do not establish independent-backbone provenance,
//! affirmative retention, terminal loss, reuse or cleanup authority.

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
) -> Result<Option<Row>, StoreError> {
    if !lock_attempt(tx, attempt).await? {
        return Ok(None);
    }
    if tx
        .query_opt(
            "SELECT r.plaintext_sha256 FROM posted_file_registry r \
             JOIN posted_files p ON p.plaintext_sha256 = r.plaintext_sha256 \
             WHERE p.id = $1 FOR UPDATE OF r",
            &[&generation],
        )
        .await
        .map_err(database_error)?
        .is_none()
    {
        return Ok(None);
    }
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
    if health.is_none()
        || tx
            .query_opt(
                "SELECT generation_id FROM posted_file_references \
                 WHERE generation_id = $1 AND kind = $2 AND owner_id = $3 FOR UPDATE",
                &[&generation, &kind.db_value(), &owner],
            )
            .await
            .map_err(database_error)?
            .is_none()
    {
        return Ok(None);
    }
    // The DB clock must be sampled again AFTER every possible lock wait.
    if !lock_attempt(tx, attempt).await? {
        return Ok(None);
    }
    Ok(health)
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
    let Some(health) = lock_retained(&tx, attempt, generation, kind, owner).await? else {
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
        revision: health.get("revision"),
        round: health.get("round"),
        round_token: health.get("round_token"),
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
    withdraw_health(&tx, &permit.generation).await?;
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(RetainedInvalidation::Applied)
}

fn matches_fence(health: &Row, permit: &RetainedCheckPermit) -> bool {
    health.get::<_, i64>("revision") == permit.revision
        && health.get::<_, i64>("round") == permit.round
        && health.get::<_, &str>("round_token") == permit.round_token
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
