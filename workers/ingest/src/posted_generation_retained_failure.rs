//! Selected retained-generation conclusion. Negative reads alone are never a
//! failure verdict. The existing repair-first policy consumes durable current
//! round evidence; the decision and its immutable evidence snapshot commit
//! together. The bounded retained repair collector supplies observations; neither
//! the collector nor this conclusion enables a production rollout.

use super::super::failure::repair_failure_is_conclusive;
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainedFailureOutcome {
    Applied,
    /// Exact original decision receipt only, not renewed mutation authority.
    AlreadyFailed,
    InsufficientEvidence,
    StaleOrDiverged,
}

/// Conclude retention-policy failure for the STILL selected managed generation.
/// Use a permit prepared after negative withdrawal and after repair observations
/// settle. Original reference incarnation, attempt, registry selection/revision,
/// health revision/round/token and post-lock DB-clock lease must all remain live.
/// Historical reference reads may invalidate old health, but cannot conclude a
/// failure on behalf of a newer selection or across selection ABA.
///
/// Requires the original selected negative observation immediately preceding this
/// round, a tracked exhausted schedule, and complete settled-slot evidence under
/// the SAME repair-first policy as candidate failure. Missing after the answered
/// repost cap is necessary; unknown/transport/auth/integrity/timer alone refuse.
/// Same-side intact content vetoes contradictory absence. No lock spans IO.
///
/// Preserve custody, references, selection, operations, budgets and R2. This is
/// policy failure, NOT physical loss or replacement/cleanup authorization. The
/// compatible rollout/drain and multi-generation admission remain separate
/// unfinished operations; no caller boolean replaces durable repair observations.
/// Retry the original permit after CommitUnknown: a receipt never starts work.
pub async fn conclude_retained_failure(
    client: &mut Client,
    permit: &RetainedCheckPermit,
) -> Result<RetainedFailureOutcome, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_retained_with_terminal(
        &tx,
        &permit.attempt,
        &permit.generation,
        permit.reference_kind,
        &permit.reference_owner,
        true,
    )
    .await?
    else {
        return Ok(RetainedFailureOutcome::StaleOrDiverged);
    };
    if current.reference_token != permit.reference_token
        || current.registry_revision != permit.registry_revision
        || current.selected_generation.as_deref() != Some(permit.generation.as_str())
        || permit.selected_generation.as_deref() != Some(permit.generation.as_str())
        || current.pending_operation.is_some()
    {
        return Ok(RetainedFailureOutcome::StaleOrDiverged);
    }
    if current.health.get::<_, &str>("state") == "terminal_failed" {
        let receipt = matching_receipt(&tx, permit, &current).await?;
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
        return Ok(if receipt {
            RetainedFailureOutcome::AlreadyFailed
        } else {
            RetainedFailureOutcome::StaleOrDiverged
        });
    }
    if !matches_fence(&current, permit) {
        return Ok(RetainedFailureOutcome::StaleOrDiverged);
    }
    if permit.round == 0
        || current.health.get::<_, &str>("state") != "unknown"
        || current
            .health
            .get::<_, Option<i64>>("verified_at")
            .is_some()
        || current
            .health
            .get::<_, Option<i64>>("retention_checked_at")
            .is_some()
    {
        return Ok(RetainedFailureOutcome::InsufficientEvidence);
    }
    let bundle = stored(&tx, &permit.generation).await?;
    if bundle.row.get::<_, Option<String>>("plan_sha256").is_none()
        || bundle
            .row
            .get::<_, Option<String>>("generation_operation")
            .is_none()
    {
        return Ok(RetainedFailureOutcome::InsufficientEvidence);
    }
    // A pre-promotion candidate round, historical selection or recreated
    // reference cannot be relabelled as a retained repair round.
    let Some(origin) = negative_origin(&tx, permit).await? else {
        return Ok(RetainedFailureOutcome::InsufficientEvidence);
    };
    let custody = bundle.custody()?;
    // Negative withdrawal does not create a repair schedule. Missing/untracked
    // histories refuse rather than receiving a fresh or fabricated pass budget.
    let schedule_exists = tx
        .query_opt(
            "SELECT 1 FROM posted_file_recheck_schedule WHERE generation_id = $1 AND round = $2",
            &[&permit.generation, &permit.round],
        )
        .await
        .map_err(database_error)?
        .is_some();
    if !schedule_exists {
        return Ok(RetainedFailureOutcome::InsufficientEvidence);
    }
    let checkpoint = checkpoint_generation_locked(
        &tx,
        &permit.generation,
        permit.round,
        custody.article_count,
        false,
    )
    .await?;
    if !repair_failure_is_conclusive(&checkpoint, &custody)? {
        return Ok(RetainedFailureOutcome::InsufficientEvidence);
    }
    // Evidence reads may wait for relation locks. Recheck the DB clock after
    // collection as well as after the authority-row locks, before mutation.
    if !lock_attempt(&tx, &permit.attempt).await? {
        return Ok(RetainedFailureOutcome::StaleOrDiverged);
    }
    let outcome = tx
        .query_one(
            "UPDATE posted_file_health SET state = 'terminal_failed', failure_kind = 'retention', \
             verified_at = NULL, retention_checked_at = NULL, revision = revision + 1, \
             round = round + 1, round_token = gen_random_uuid()::text \
             WHERE generation_id = $1 RETURNING revision, round, round_token",
            &[&permit.generation],
        )
        .await
        .map_err(database_error)?;
    let inserted = tx.execute(
        "INSERT INTO posted_file_retained_failures \
         (generation_id, health_revision, round, round_token, origin_health_revision, \
          job_id, attempt, attempt_token, reference_kind, reference_owner, reference_token, \
          registry_revision, outcome_revision, outcome_round, outcome_token, repair_evidence) \
         SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, \
         jsonb_build_object( \
           'schedule', (SELECT to_jsonb(s) FROM posted_file_recheck_schedule s \
                        WHERE s.generation_id = $1 AND s.round = $3), \
           'slots', (SELECT jsonb_agg(to_jsonb(p) ORDER BY p.slot, p.side) \
                     FROM posted_file_article_progress p WHERE p.generation_id = $1 AND p.round = $3), \
           'content', (SELECT coalesce(jsonb_agg(to_jsonb(c) ORDER BY c.side), '[]'::jsonb) \
                       FROM posted_file_content_checks c WHERE c.generation_id = $1 AND c.round = $3))",
        &[
            &permit.generation, &permit.revision, &permit.round, &permit.round_token,
            &origin, &permit.attempt.job_id,
            &permit.attempt.attempt, &permit.attempt.token, &permit.reference_kind.db_value(),
            &permit.reference_owner, &permit.reference_token, &permit.registry_revision,
            &outcome.get::<_, i64>("revision"), &outcome.get::<_, i64>("round"),
            &outcome.get::<_, String>("round_token"),
        ],
    ).await.map_err(database_error)?;
    if inserted != 1 {
        return Err(StoreError::Inconsistent);
    }
    // Final health/receipt statements can themselves wait on relation locks.
    // Expiry rolls the complete conclusion and its snapshot back together.
    if !lock_attempt(&tx, &permit.attempt).await? {
        return Ok(RetainedFailureOutcome::StaleOrDiverged);
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(RetainedFailureOutcome::Applied)
}

pub(super) async fn negative_origin(
    tx: &Transaction<'_>,
    permit: &RetainedCheckPermit,
) -> Result<Option<i64>, StoreError> {
    Ok(tx
        .query_opt(
            "SELECT health_revision FROM posted_file_retained_observations \
         WHERE generation_id = $1 AND round = $2::bigint - 1 AND health_revision < $3 \
         AND reference_kind = $4 AND reference_owner = $5 AND reference_token = $6 \
         AND prepared_registry_revision = $7 AND recorded_registry_revision = $7 \
         AND prepared_selection = $1 AND recorded_selection = $1",
            &[
                &permit.generation,
                &permit.round,
                &permit.revision,
                &permit.reference_kind.db_value(),
                &permit.reference_owner,
                &permit.reference_token,
                &permit.registry_revision,
            ],
        )
        .await
        .map_err(database_error)?
        .map(|row| row.get("health_revision")))
}

async fn matching_receipt(
    tx: &Transaction<'_>,
    permit: &RetainedCheckPermit,
    current: &RetainedFence,
) -> Result<bool, StoreError> {
    if current.health.get::<_, Option<&str>>("failure_kind") != Some("retention") {
        return Ok(false);
    }
    Ok(tx
        .query_opt(
            "SELECT 1 FROM posted_file_retained_failures WHERE generation_id = $1 \
         AND health_revision = $2 AND round = $3 AND round_token = $4 \
         AND job_id = $5 AND attempt = $6 AND attempt_token = $7 \
         AND reference_kind = $8 AND reference_owner = $9 AND reference_token = $10 \
         AND registry_revision = $11 AND outcome_revision = $12 \
         AND outcome_round = $13 AND outcome_token = $14",
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
                &current.health.get::<_, i64>("revision"),
                &current.health.get::<_, i64>("round"),
                &current.health.get::<_, String>("round_token"),
            ],
        )
        .await
        .map_err(database_error)?
        .is_some())
}
