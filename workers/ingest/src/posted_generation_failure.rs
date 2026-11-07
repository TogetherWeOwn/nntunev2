//! Conclusive candidate-verification failure, not lost-custody or cleanup proof.
//! The existing verifier's settled-slot conclusion is fed only current durable
//! evidence. Unknown/transport stops and a timer alone never become terminal.

use super::*;
use crate::two_spool::{conclude, ArticleSpool, StatVerdict, TwoSpoolOutcome, TwoSpoolProgress};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationFailureOutcome {
    Applied,
    /// Receipt for this original handle only; no renewed posting authority.
    AlreadyFailed,
    InsufficientEvidence,
    StaleOrDiverged,
}

/// Conclude a failed two-backbone candidate only after the bounded schedule and
/// same-ID repair policy. Every unresolved slot/side must have an explicit fresh
/// Missing at its current answered-repost cap; all others must be confirmed.
/// A pre-repair Missing, partial/unknown set, transport stop, untracked schedule,
/// POST success, or content-integrity failure alone is insufficient. Reuse the
/// verifier's settled-slot policy, never its timer-only exhaustion as evidence.
///
/// The exact current attempt/claim/registry/round/health fences serialize the
/// decision with repair, presence and takeover. Stop the operation and withdraw
/// eligibility atomically, preserving ALL custody, evidence, budgets, references
/// and original/rollback selection. This is a verification failure, not proof
/// that the data is lost. R2 MUST remain. It grants no replacement, retention,
/// promotion, publication or cleanup capability. Multi-generation rollout remains
/// disabled. No database lock spans network work.
///
/// On an unknown commit acknowledgment, retry THIS original handle. A matching
/// stopped receipt is not recoverable/resumable custody authority; expired or
/// replaced attempts refuse even receipt acknowledgment.
pub async fn fail_candidate_verification(
    client: &mut Client,
    handle: &GenerationHandle,
) -> Result<VerificationFailureOutcome, StoreError> {
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(current) = lock_candidate(&tx, handle).await? else {
        let failed =
            stopped_candidate_receipt(&tx, handle, CandidateStop::VerificationFailed).await?;
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
        return Ok(if failed {
            VerificationFailureOutcome::AlreadyFailed
        } else {
            VerificationFailureOutcome::StaleOrDiverged
        });
    };
    if handle.round == 0
        || current.get::<_, &str>("state") != "verifying"
        || current.get::<_, Option<i64>>("verified_at").is_some()
    {
        return Ok(VerificationFailureOutcome::InsufficientEvidence);
    }
    let checkpoint = checkpoint_locked(&tx, handle, &current).await?;
    if checkpoint.schedule != Some(RecheckSchedule::Exhausted) {
        return Ok(VerificationFailureOutcome::InsufficientEvidence);
    }
    let bundle = stored(&tx, &handle.generation).await?;
    let custody = bundle.custody()?;
    let mut progress = TwoSpoolProgress::new(
        custody.backbone_post,
        custody.backbone_verify,
        checkpoint.slots.len() * 2,
    )
    .map_err(|_| StoreError::Inconsistent)?;
    for (index, (state, slot)) in progress
        .articles
        .iter_mut()
        .zip(checkpoint.slots.iter().flatten())
        .enumerate()
    {
        *state = if slot.confirmed {
            ArticleSpool::Verified
        } else if slot.presence == Some(StatVerdict::Missing)
            && slot.repost_attempts == MAX_REPOST_ATTEMPTS_PER_ARTICLE
            // A later intact full-set read contradicts this lane's absence.
            // It cannot establish independent retention here, but must veto loss.
            && checkpoint.content_checks[index % 2] != Some(ContentCheckState::Intact)
        {
            ArticleSpool::Exhausted
        } else {
            ArticleSpool::Unchecked
        };
    }
    // Do not feed timer-only unknowns through conclude's keep-original branch.
    if !progress.is_settled()
        || !matches!(conclude(&progress), TwoSpoolOutcome::RepostExhausted { .. })
    {
        return Ok(VerificationFailureOutcome::InsufficientEvidence);
    }
    stop_candidate(&tx, handle, CandidateStop::VerificationFailed).await?;
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    Ok(VerificationFailureOutcome::Applied)
}
