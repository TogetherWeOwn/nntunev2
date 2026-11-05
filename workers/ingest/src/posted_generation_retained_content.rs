//! Full-content observations for referenced, managed generations. This uses the
//! same trusted reader/report contract as candidate checks, not a second reader
//! or an untrusted callback API. No affirmative retention or cleanup is enabled.

use super::super::content::{checked_state, store_observation};
use super::*;

/// Original reference/attempt/health fence plus the immutable read identity.
/// Fields are private: a result cannot acquire a newer fence after the read.
#[derive(Clone)]
pub struct RetainedContentPermit {
    retained: RetainedCheckPermit,
    side: ArticleSide,
    backbone: String,
    plan: String,
    hash: String,
    size: i64,
    articles: i32,
}

impl fmt::Debug for RetainedContentPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RetainedContentPermit([REDACTED])")
    }
}

/// Prepare one managed retained read with its exact stored custody. Legacy rows
/// without a canonical plan remain negative-only through prepare_retained_check.
/// Round zero is not an evidence round; explicit invalidation can start a fresh
/// unknown round before a standing reader collects observations. Preparation
/// does not rotate a round, manufacture evidence, or extend attempt authority.
pub async fn prepare_retained_content_check(
    client: &mut Client,
    attempt: &AttemptHandle,
    generation: &str,
    kind: RetainedReferenceKind,
    owner: &str,
    side: ArticleSide,
) -> Result<Option<(RetainedContentPermit, StoredGeneration)>, StoreError> {
    let Some((retained, custody)) =
        prepare_retained_check(client, attempt, generation, kind, owner).await?
    else {
        return Ok(None);
    };
    let plan: Option<String> = custody
        .stored
        .row
        .try_get("plan_sha256")
        .map_err(database_error)?;
    let Some(plan) = plan else {
        return Ok(None);
    };
    let operation: Option<&str> = custody
        .stored
        .row
        .try_get("generation_operation")
        .map_err(database_error)?;
    if retained.round == 0 || operation.is_none() {
        return Ok(None);
    }
    if !crate::two_spool::validate_plaintext_sha256(&plan) {
        return Err(StoreError::Inconsistent);
    }
    let p = custody.custody()?;
    let permit = RetainedContentPermit {
        retained,
        side,
        backbone: match side {
            ArticleSide::Post => p.backbone_post,
            ArticleSide::Verify => p.backbone_verify,
        }
        .to_owned(),
        plan,
        hash: p.plaintext_sha256.to_owned(),
        size: p.plaintext_size,
        articles: p.article_count,
    };
    Ok(Some((permit, custody.stored)))
}

/// Record a trusted full required-set read under its ORIGINAL permit. An intact
/// report only stores evidence, never changes unknown to usable or timestamps
/// retention. Labels are checked against custody but are not topology proof.
/// A nonaffirmative result atomically records the old-round failure and withdraws
/// eligibility into a new unknown round. Historical evidence/budgets/custody and
/// references remain intact. No terminal failure or replacement is inferred.
/// CommitUnknown requires re-observation, not rebinding this result to a new permit.
pub async fn record_retained_content_check(
    client: &mut Client,
    permit: &RetainedContentPermit,
    result: ContentCheckResult<'_>,
) -> Result<RetainedInvalidation, StoreError> {
    let state = checked_state(
        result,
        &permit.backbone,
        &permit.plan,
        &permit.hash,
        permit.size,
        permit.articles,
    )?;
    let original = &permit.retained;
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(health) = lock_retained(
        &tx,
        &original.attempt,
        &original.generation,
        original.reference_kind,
        &original.reference_owner,
    )
    .await?
    else {
        return Ok(RetainedInvalidation::StaleOrMissing);
    };
    if !matches_fence(&health, original) {
        return Ok(RetainedInvalidation::StaleOrMissing);
    }
    let changed = store_observation(
        &tx,
        &original.generation,
        original.round,
        permit.side,
        &permit.backbone,
        state,
        permit.articles,
    )
    .await?;
    if state != ContentCheckState::Intact {
        withdraw_health(&tx, &original.generation).await?;
    } else if changed == 1 {
        tx.execute(
            "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
            &[&original.generation],
        )
        .await
        .map_err(database_error)?;
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(RetainedInvalidation::Applied)
}

/// Perform one explicit read, with no database lock over network work. A stale
/// callback stops; cancellation records nothing. Never skip a prior Intact row:
/// an explicit recheck must still be able to withdraw it on read failure.
pub async fn drive_retained_content_read<T: GenerationContentTransport>(
    client: &mut Client,
    attempt: &AttemptHandle,
    generation: &str,
    kind: RetainedReferenceKind,
    owner: &str,
    side: ArticleSide,
    transport: &mut T,
) -> Result<RetainedInvalidation, StoreError> {
    let Some((permit, bundle)) =
        prepare_retained_content_check(client, attempt, generation, kind, owner, side).await?
    else {
        return Ok(RetainedInvalidation::StaleOrMissing);
    };
    let result = transport.read_content(&bundle, side).await;
    record_retained_content_check(client, &permit, result).await
}
