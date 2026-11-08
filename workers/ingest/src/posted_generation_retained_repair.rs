//! Bounded selected-retained repair using the existing schedule and exact stored
//! bundle. The reference/selection/attempt/round fence is checked before IO and
//! again on its original callback. No transaction spans transport work. Neither
//! presence completion nor schedule exhaustion is affirmative content proof.

use super::*;
use crate::two_spool::{
    recheck_delay_ms, StatVerdict, MAX_RECHECKS, PROPAGATION_DELAY_MS, STAT_BATCH_SIZE,
};

#[derive(Debug)]
pub enum RetainedRepairStep {
    Progress(Box<RetainedCheckPermit>),
    Waiting {
        not_before: i64,
    },
    /// Presence only: no usable/retention timestamp or publication authority.
    PresenceComplete,
    KeepOriginal,
    RetentionFailed,
    StaleOrDiverged,
}

// Unlike historical negative reads, repair is authoritative only for the still
// selected generation with no pending replacement. Selection ABA retires work.
async fn lock_repair(
    tx: &Transaction<'_>,
    permit: &RetainedCheckPermit,
    exact_revision: bool,
) -> Result<bool, StoreError> {
    let Some(fence) = lock_retained(
        tx,
        &permit.attempt,
        &permit.generation,
        permit.reference_kind,
        &permit.reference_owner,
    )
    .await?
    else {
        return Ok(false);
    };
    Ok(fence.reference_token == permit.reference_token
        && fence.registry_revision == permit.registry_revision
        && fence.selected_generation.as_deref() == Some(permit.generation.as_str())
        && permit.selected_generation == fence.selected_generation
        && fence.pending_operation.is_none()
        && fence.health.get::<_, &str>("state") == "unknown"
        && fence.health.get::<_, i64>("round") == permit.round
        && fence.health.get::<_, &str>("round_token") == permit.round_token
        && (!exact_revision || fence.health.get::<_, i64>("revision") == permit.revision)
        && fence.health.get::<_, Option<i64>>("verified_at").is_none()
        && fence
            .health
            .get::<_, Option<i64>>("retention_checked_at")
            .is_none()
        && permit.round > 0)
}

/// Start/reload repair only after a selected negative withdrawal in this exact
/// reference incarnation and registry revision. Existing histories are NEVER
/// reset, including untracked/exhausted schedules and answered repost counts.
/// Legacy null-plan rows and candidates refuse. This is not a new health round.
pub async fn start_retained_repair(
    client: &mut Client,
    permit: &RetainedCheckPermit,
) -> Result<bool, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_repair(&tx, permit, true).await?
        || failure::negative_origin(&tx, permit).await?.is_none()
    {
        return Ok(false);
    }
    let bundle = stored(&tx, &permit.generation).await?;
    if bundle.row.get::<_, Option<String>>("plan_sha256").is_none()
        || bundle
            .row
            .get::<_, Option<String>>("generation_operation")
            .is_none()
    {
        return Ok(false);
    }
    bundle.custody()?;
    tx.execute(
        "INSERT INTO posted_file_recheck_schedule (generation_id, round, state, not_before) \
         VALUES ($1, $2, 'waiting', ceil(extract(epoch FROM clock_timestamp()))::bigint + $3) \
         ON CONFLICT (generation_id, round) DO NOTHING",
        &[
            &permit.generation,
            &permit.round,
            &((PROPAGATION_DELAY_MS / 1000) as i64),
        ],
    )
    .await
    .map_err(database_error)?;
    if !lock_attempt(&tx, &permit.attempt).await? {
        return Ok(false);
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(true)
}

/// Read-only restart recovery for an existing repair context, outside any IO
/// callback. Only the health revision may advance; original attempt, reference,
/// selection and round/token must survive. Never attach an old answer to this
/// refreshed permit. A newer attempt must explicitly prepare its own check.
pub async fn recover_retained_repair(
    client: &mut Client,
    original: &RetainedCheckPermit,
) -> Result<Option<RetainedCheckPermit>, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_repair(&tx, original, false).await?
        || failure::negative_origin(&tx, original).await?.is_none()
    {
        return Ok(None);
    }
    let exists = tx
        .query_opt(
            "SELECT 1 FROM posted_file_recheck_schedule WHERE generation_id = $1 AND round = $2",
            &[&original.generation, &original.round],
        )
        .await
        .map_err(database_error)?
        .is_some();
    if !exists {
        return Ok(None);
    }
    let revision = tx
        .query_one(
            "SELECT revision FROM posted_file_health WHERE generation_id = $1",
            &[&original.generation],
        )
        .await
        .map_err(database_error)?
        .get("revision");
    if !lock_attempt(&tx, &original.attempt).await? {
        return Ok(None);
    }
    let mut permit = original.clone();
    permit.revision = revision;
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(Some(permit))
}

async fn commit_progress(
    tx: Transaction<'_>,
    permit: &RetainedCheckPermit,
) -> Result<Option<RetainedCheckPermit>, StoreError> {
    // Evidence/schedule statements may wait on relation locks as well.
    if !lock_attempt(&tx, &permit.attempt).await? {
        return Ok(None);
    }
    let revision = tx
        .query_one(
            "UPDATE posted_file_health SET revision = revision + 1, verified_at = NULL, \
         retention_checked_at = NULL WHERE generation_id = $1 RETURNING revision",
            &[&permit.generation],
        )
        .await
        .map_err(database_error)?
        .get("revision");
    let mut next = permit.clone();
    next.revision = revision;
    // The UPDATE itself can wait for a stronger relation lock after the earlier
    // clock check. Roll back all evidence/accounting if that wait outlived us.
    if !lock_attempt(&tx, &permit.attempt).await? {
        return Ok(None);
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(Some(next))
}

async fn withdraw_side(
    tx: &Transaction<'_>,
    permit: &RetainedCheckPermit,
    side: ArticleSide,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE posted_file_content_checks SET state = 'unavailable', checked_articles = 0, \
         checked_at = floor(extract(epoch FROM clock_timestamp()))::bigint \
         WHERE generation_id = $1 AND round = $2 AND side = $3 AND state = 'intact'",
        &[&permit.generation, &permit.round, &side.db_value()],
    )
    .await
    .map_err(database_error)?;
    Ok(())
}

// A cursor is durable slot/side work, not authority. The original revision and
// pass ordinal fence every callback, including identical STAT answers.
async fn record_stat(
    client: &mut Client,
    permit: &RetainedCheckPermit,
    cursor: i32,
    pass: i32,
    verdict: StatVerdict,
) -> Result<Option<RetainedCheckPermit>, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_repair(&tx, permit, true).await? || !current_cursor(&tx, permit, cursor, pass).await? {
        return Ok(None);
    }
    content::load_generation_checks(&tx, &permit.generation, permit.round).await?;
    let side = if cursor % 2 == 0 {
        ArticleSide::Post
    } else {
        ArticleSide::Verify
    };
    let verdict_text = match verdict {
        StatVerdict::Present => "present",
        StatVerdict::Missing => "missing",
        StatVerdict::Transport => "unavailable",
    };
    tx.execute(
        "INSERT INTO posted_file_article_progress AS p \
         (generation_id, round, slot, side, confirmed, presence_verdict, presence_repost_attempts) \
         VALUES ($1, $2, $3, $4, $5, $6, 0) \
         ON CONFLICT (generation_id, round, slot, side) DO UPDATE \
         SET confirmed = EXCLUDED.confirmed, presence_verdict = EXCLUDED.presence_verdict, \
         presence_repost_attempts = p.repost_attempts",
        &[
            &permit.generation,
            &permit.round,
            &(cursor / 2),
            &side.db_value(),
            &(verdict == StatVerdict::Present),
            &verdict_text,
        ],
    )
    .await
    .map_err(database_error)?;
    if verdict != StatVerdict::Present {
        withdraw_side(&tx, permit, side).await?;
    }
    commit_progress(tx, permit).await
}

async fn current_cursor(
    tx: &Transaction<'_>,
    permit: &RetainedCheckPermit,
    cursor: i32,
    pass: i32,
) -> Result<bool, StoreError> {
    Ok(tx
        .query_opt(
            "SELECT 1 FROM posted_file_recheck_schedule WHERE generation_id = $1 AND round = $2 \
         AND state = 'waiting' AND work_cursor = $3 AND completed_passes = $4 \
         AND not_before <= floor(extract(epoch FROM clock_timestamp()))::bigint",
            &[&permit.generation, &permit.round, &cursor, &pass],
        )
        .await
        .map_err(database_error)?
        .is_some())
}

// Revalidate immediately before POST, after the committed Missing callback.
async fn prepare_post(
    client: &mut Client,
    permit: &RetainedCheckPermit,
    cursor: i32,
    pass: i32,
) -> Result<Option<i32>, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_repair(&tx, permit, true).await? || !current_cursor(&tx, permit, cursor, pass).await? {
        return Ok(None);
    }
    let row = tx.query_one(
        "SELECT confirmed, repost_attempts, presence_verdict, presence_repost_attempts \
         FROM posted_file_article_progress WHERE generation_id = $1 AND round = $2 AND slot = $3 AND side = $4",
        &[&permit.generation, &permit.round, &(cursor / 2), &((cursor % 2) as i16)],
    ).await.map_err(database_error)?;
    let progress = slot_progress(&row)?;
    if progress.presence != Some(StatVerdict::Missing) {
        return Err(StoreError::Inconsistent);
    }
    let result = (progress.repost_attempts < MAX_REPOST_ATTEMPTS_PER_ARTICLE)
        .then_some(progress.repost_attempts as i32);
    if !lock_attempt(&tx, &permit.attempt).await? {
        return Ok(None);
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(result)
}

async fn record_post(
    client: &mut Client,
    permit: &RetainedCheckPermit,
    cursor: i32,
    pass: i32,
    count: i32,
    verdict: PostVerdict,
) -> Result<Option<RetainedCheckPermit>, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_repair(&tx, permit, true).await? || !current_cursor(&tx, permit, cursor, pass).await? {
        return Ok(None);
    }
    if !matches!(verdict, PostVerdict::Posted | PostVerdict::DuplicatePresent) {
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
        return Ok(Some(permit.clone()));
    }
    content::load_generation_checks(&tx, &permit.generation, permit.round).await?;
    let changed = tx
        .execute(
            "UPDATE posted_file_article_progress SET repost_attempts = repost_attempts + 1 \
         WHERE generation_id = $1 AND round = $2 AND slot = $3 AND side = $4 AND NOT confirmed \
         AND presence_verdict = 'missing' AND presence_repost_attempts = repost_attempts \
         AND repost_attempts = $5 AND repost_attempts < $6",
            &[
                &permit.generation,
                &permit.round,
                &(cursor / 2),
                &((cursor % 2) as i16),
                &count,
                &(MAX_REPOST_ATTEMPTS_PER_ARTICLE as i32),
            ],
        )
        .await
        .map_err(database_error)?;
    if changed != 1 {
        return Ok(None);
    }
    withdraw_side(
        &tx,
        permit,
        if cursor % 2 == 0 {
            ArticleSide::Post
        } else {
            ArticleSide::Verify
        },
    )
    .await?;
    commit_progress(tx, permit).await
}

async fn advance(
    client: &mut Client,
    permit: &RetainedCheckPermit,
    cursor: i32,
    pass: i32,
    total: i32,
) -> Result<Option<RetainedCheckPermit>, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_repair(&tx, permit, true).await? || !current_cursor(&tx, permit, cursor, pass).await? {
        return Ok(None);
    }
    if cursor + 1 == total {
        let next = pass + 1;
        tx.execute(
            "UPDATE posted_file_recheck_schedule SET work_cursor = 0, completed_passes = $3, \
             state = CASE WHEN $4 THEN 'exhausted' ELSE 'waiting' END, \
             not_before = CASE WHEN $4 THEN NULL ELSE ceil(extract(epoch FROM clock_timestamp()))::bigint + $5 END \
             WHERE generation_id = $1 AND round = $2",
            &[&permit.generation, &permit.round, &next, &(next == MAX_RECHECKS as i32), &((recheck_delay_ms(pass as u32) / 1000) as i64)],
        ).await.map_err(database_error)?;
    } else {
        tx.execute("UPDATE posted_file_recheck_schedule SET work_cursor = work_cursor + 1 WHERE generation_id = $1 AND round = $2", &[&permit.generation, &permit.round]).await.map_err(database_error)?;
    }
    commit_progress(tx, permit).await
}

/// Collect at most max_slots original-permit STAT/same-ID repair results. The
/// existing transport resolves the immutable sealed plan; only Missing permits
/// POST, and only answered/confirmed-duplicate replies spend the existing cap.
/// Transport/refusal and timer exhaustion cannot manufacture terminal evidence.
/// On commit ambiguity STOP and recover outside the old callback; no remint.
/// Cancellation records no answer; a committed earlier phase may be replayed.
/// The caller owns live attempt renewal/reacquisition; this never renews leases.
pub async fn drive_retained_repair<T: GenerationTransport>(
    client: &mut Client,
    original: &RetainedCheckPermit,
    transport: &mut T,
    max_slots: usize,
) -> Result<RetainedRepairStep, StoreError> {
    if max_slots == 0 || max_slots > STAT_BATCH_SIZE {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    if !lock_repair(&tx, original, true).await? {
        // The terminal commit may have succeeded without acknowledgment. End
        // this transaction before the receipt-only conclusion takes its locks.
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
        return Ok(match conclude_retained_failure(client, original).await? {
            RetainedFailureOutcome::AlreadyFailed => RetainedRepairStep::RetentionFailed,
            _ => RetainedRepairStep::StaleOrDiverged,
        });
    }
    let schedule = tx.query_opt(
        "SELECT work_cursor, completed_passes FROM posted_file_recheck_schedule WHERE generation_id = $1 AND round = $2",
        &[&original.generation, &original.round],
    ).await.map_err(database_error)?;
    let Some(schedule) = schedule else {
        return Ok(RetainedRepairStep::KeepOriginal);
    };
    if failure::negative_origin(&tx, original).await?.is_none() {
        return Ok(RetainedRepairStep::StaleOrDiverged);
    }
    let bundle = stored(&tx, &original.generation).await?;
    let checkpoint = checkpoint_generation_locked(
        &tx,
        &original.generation,
        original.round,
        bundle.custody()?.article_count,
        false,
    )
    .await?;
    let cursor: i32 = schedule.get("work_cursor");
    let pass: i32 = schedule.get("completed_passes");
    let total = checkpoint.article_count() as i32 * 2;
    if !(0..total).contains(&cursor) {
        return Err(StoreError::Inconsistent);
    }
    let now: i64 = tx
        .query_one(
            "SELECT floor(extract(epoch FROM clock_timestamp()))::bigint",
            &[],
        )
        .await
        .map_err(database_error)?
        .get(0);
    if !lock_attempt(&tx, &original.attempt).await? {
        return Ok(RetainedRepairStep::StaleOrDiverged);
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    if checkpoint.slots.iter().flatten().all(|slot| slot.confirmed) {
        return Ok(RetainedRepairStep::PresenceComplete);
    }
    match checkpoint.schedule {
        Some(RecheckSchedule::Waiting { not_before, .. }) if now < not_before => {
            return Ok(RetainedRepairStep::Waiting { not_before })
        }
        Some(RecheckSchedule::Waiting { .. }) => {}
        Some(RecheckSchedule::Exhausted) => {
            return Ok(match conclude_retained_failure(client, original).await? {
                RetainedFailureOutcome::Applied | RetainedFailureOutcome::AlreadyFailed => {
                    RetainedRepairStep::RetentionFailed
                }
                RetainedFailureOutcome::InsufficientEvidence => RetainedRepairStep::KeepOriginal,
                RetainedFailureOutcome::StaleOrDiverged => RetainedRepairStep::StaleOrDiverged,
            })
        }
        _ => return Ok(RetainedRepairStep::KeepOriginal),
    }
    let mut permit = original.clone();
    for index in cursor..(cursor + max_slots as i32).min(total) {
        let side = if index % 2 == 0 {
            ArticleSide::Post
        } else {
            ArticleSide::Verify
        };
        if !checkpoint.slots[(index / 2) as usize][(index % 2) as usize].confirmed {
            // Recheck immediately before each IO, never after it to borrow a fence.
            let tx = client.transaction().await.map_err(database_error)?;
            if !lock_repair(&tx, &permit, true).await?
                || !current_cursor(&tx, &permit, index, pass).await?
            {
                return Ok(RetainedRepairStep::StaleOrDiverged);
            }
            if !lock_attempt(&tx, &permit.attempt).await? {
                return Ok(RetainedRepairStep::StaleOrDiverged);
            }
            tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
            let verdict = transport.stat(&bundle, index / 2, side).await;
            let Some(next) = record_stat(client, &permit, index, pass, verdict).await? else {
                return Ok(RetainedRepairStep::StaleOrDiverged);
            };
            permit = next;
            if verdict == StatVerdict::Missing {
                if let Some(count) = prepare_post(client, &permit, index, pass).await? {
                    let answer = transport.repost(&bundle, index / 2, side).await;
                    let Some(next) =
                        record_post(client, &permit, index, pass, count, answer).await?
                    else {
                        return Ok(RetainedRepairStep::StaleOrDiverged);
                    };
                    permit = next;
                }
            }
        }
        let Some(next) = advance(client, &permit, index, pass, total).await? else {
            return Ok(RetainedRepairStep::StaleOrDiverged);
        };
        permit = next;
    }
    Ok(RetainedRepairStep::Progress(Box::new(permit)))
}
