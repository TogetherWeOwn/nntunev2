//! Original-permit presence callbacks for the immutable generation plan.
//! Prepare before STAT and return the same permit after the network request;
//! never attach an old answer to a recovered/refreshed handle. No transaction
//! spans the request. The verifier resolves the slot from the stored sealed plan.
//!
//! An unconfirmed slot is NOT a proven missing article. Transport invalidates
//! affirmative evidence conservatively but grants no repost/terminal-failure
//! authority. Repair policy still distinguishes Missing from Transport. Neither
//! presence result is authenticated content, independent-backbone proof, or R2
//! cleanup permission. Queue and network consumers remain unwired.

use super::*;
use crate::two_spool::StatVerdict;

#[derive(Clone)]
pub struct PresenceCheckPermit {
    handle: GenerationHandle,
    slot: i32,
    side: ArticleSide,
    plan: String,
    backbone: String,
}

impl fmt::Debug for PresenceCheckPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PresenceCheckPermit([REDACTED])")
    }
}

fn backbone<'a>(custody: &PostedFileParams<'a>, side: ArticleSide) -> &'a str {
    match side {
        ArticleSide::Post => custody.backbone_post,
        ArticleSide::Verify => custody.backbone_verify,
    }
}

/// Bound to the original complete fence, stored plan, slot and configured side.
/// Presence may already be complete: rechecks must be able to invalidate it.
/// Preparation writes nothing and grants no authority to remint or repost.
pub async fn prepare_presence_check(
    client: &mut Client,
    handle: &GenerationHandle,
    slot: i32,
    side: ArticleSide,
) -> Result<Option<PresenceCheckPermit>, StoreError> {
    if slot < 0 {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(None);
    };
    if current.get::<_, &str>("state") != "verifying" || handle.round == 0 {
        return Ok(None);
    }
    if slot >= current.get::<_, i32>("article_count") {
        return Err(StoreError::InvalidInput);
    }
    let bundle = stored(&tx, &handle.generation).await?;
    let permit = PresenceCheckPermit {
        handle: handle.clone(),
        slot,
        side,
        plan: bundle.plan_sha256().to_owned(),
        backbone: backbone(&bundle.custody()?, side).to_owned(),
    };
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(Some(permit))
}

/// Commit the original callback, including nonaffirmative evidence, atomically.
/// A missing/unavailable article clears full-set presence completion and any
/// intact content observation for that side in this round. Other rounds/sides,
/// repair counters and scheduling remain untouched. This does not declare loss.
/// Any changed evidence retires competing permits via the health revision.
/// On an ambiguous commit recover the operation and load its checkpoint; do not
/// bind the old network answer to a freshly prepared permit.
pub async fn record_presence_check(
    client: &mut Client,
    permit: &PresenceCheckPermit,
    result: StatVerdict,
) -> Result<MutationOutcome, StoreError> {
    let handle = &permit.handle;
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        return Ok(MutationOutcome::StaleOrDiverged);
    };
    if current.get::<_, &str>("state") != "verifying" || handle.round == 0 {
        return Ok(MutationOutcome::StaleOrDiverged);
    }
    let bundle = stored(&tx, &handle.generation).await?;
    let custody = bundle.custody()?;
    if permit.slot < 0
        || permit.slot >= custody.article_count
        || permit.plan != bundle.plan_sha256()
        || permit.backbone != backbone(&custody, permit.side)
    {
        return Err(StoreError::Inconsistent);
    }
    // Validate existing content identities before treating an unchanged row as
    // idempotence. A corrupt side/plan must fail closed, not be overwritten.
    content::load_checks(&tx, handle).await?;
    let confirmed = result == StatVerdict::Present;
    let changed = tx
        .execute(
            "INSERT INTO posted_file_article_progress AS p \
             (generation_id, round, slot, side, confirmed) VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (generation_id, round, slot, side) DO UPDATE \
             SET confirmed = EXCLUDED.confirmed WHERE p.confirmed <> EXCLUDED.confirmed",
            &[
                &handle.generation,
                &handle.round,
                &permit.slot,
                &permit.side.db_value(),
                &confirmed,
            ],
        )
        .await
        .map_err(database_error)?;
    let mut invalidated = 0;
    if !confirmed {
        invalidated += tx
            .execute(
                "UPDATE posted_file_content_checks SET state = 'unavailable', checked_articles = 0, \
                 checked_at = floor(extract(epoch FROM clock_timestamp()))::bigint \
                 WHERE generation_id = $1 AND round = $2 AND side = $3 AND state = 'intact'",
                &[&handle.generation, &handle.round, &permit.side.db_value()],
            )
            .await
            .map_err(database_error)?;
        invalidated += tx
            .execute(
                "UPDATE posted_file_health SET verified_at = NULL, retention_checked_at = NULL \
                 WHERE generation_id = $1 \
                 AND (verified_at IS NOT NULL OR retention_checked_at IS NOT NULL)",
                &[&handle.generation],
            )
            .await
            .map_err(database_error)?;
    }
    if changed > 0 || invalidated > 0 {
        tx.execute(
            "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
            &[&handle.generation],
        )
        .await
        .map_err(database_error)?;
    }
    commit_mutation(tx, handle).await
}
