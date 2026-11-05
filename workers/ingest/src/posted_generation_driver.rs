//! Bounded consumer of the durable missing-set recheck schedule. The Worker
//! queue/pool route is not enabled here: the trusted transport still owns sealed
//! manifest opening, canonical account resolution and exact same-ID/byte repair.
//! No local/KV progress can replace the database checkpoint. All callbacks use
//! permits prepared before their request; errors stop this delivery and require
//! recovery, never rebinding an old answer to a new revision.
//!
//! Presence completion is NOT authenticated content, topology, reuse, promotion,
//! publication or deletion authority. Every outcome retains the R2 safety net.

use super::*;
use crate::two_spool::{StatVerdict, STAT_BATCH_SIZE};
use std::future::Future;

/// Requests refer only to slots in the exact stored sealed bundle, including
/// recovery slots. Implementations must resolve that plan, never remint it or
/// substitute queue-provided IDs/bytes. Provider errors map to redacted verdicts.
/// A duplicate POST must be confirmed by the existing NNTP client's STAT path.
pub trait GenerationTransport {
    fn stat(
        &mut self,
        bundle: &StoredGeneration,
        slot: i32,
        side: ArticleSide,
    ) -> impl Future<Output = StatVerdict>;

    fn repost(
        &mut self,
        bundle: &StoredGeneration,
        slot: i32,
        side: ArticleSide,
    ) -> impl Future<Output = PostVerdict>;
}

#[derive(Debug)]
pub enum RecheckStep {
    /// Resume by operation identity on the next delivery, not by a local cursor.
    Progress(Box<GenerationHandle>),
    Waiting {
        not_before: i64,
    },
    PresenceComplete(Box<GenerationHandle>),
    /// Scheduling history is exhausted or untracked, not proof of content loss.
    KeepOriginal,
    /// Current Missing evidence after the repair cap concluded verification
    /// failure. Keep the original, all custody and R2; never cleanup authority.
    VerificationFailed,
    StaleOrDiverged,
}

fn refreshed(outcome: MutationOutcome, handle: &mut GenerationHandle) -> Result<bool, StoreError> {
    match outcome {
        MutationOutcome::Applied(next) => {
            *handle = *next;
            Ok(true)
        }
        MutationOutcome::StaleOrDiverged => Ok(false),
        MutationOutcome::Incomplete => Err(StoreError::Inconsistent),
    }
}

async fn finish(
    client: &mut Client,
    mut handle: GenerationHandle,
) -> Result<RecheckStep, StoreError> {
    match finish_verification(client, &handle).await? {
        MutationOutcome::Applied(next) => {
            handle = *next;
            Ok(RecheckStep::PresenceComplete(Box::new(handle)))
        }
        MutationOutcome::Incomplete => Ok(RecheckStep::Progress(Box::new(handle))),
        MutationOutcome::StaleOrDiverged => Ok(RecheckStep::StaleOrDiverged),
    }
}

/// Resume one bounded delivery using only the persisted attempt and content
/// hash. Discover/recover the current candidate and let `drive_recheck` load its
/// durable cursor, schedule and counters; no Queue/KV handle or local progress
/// is accepted. Losing the returned handle does not lose committed progress.
/// Missing or no-longer-current work stops without network calls or a new mint.
/// Callers must explicitly admit/begin a round or take over an expired attempt;
/// this entry never performs those transitions implicitly. Terminal receipts
/// still use `drive_recheck` with their original handle, not hash discovery.
/// The production Queue/pool route remains unwired; trusted transport and all
/// original-permit callback fences are unchanged.
pub async fn drive_pending_recheck<T: GenerationTransport>(
    client: &mut Client,
    attempt: &AttemptHandle,
    hash: &str,
    transport: &mut T,
    max_slots: usize,
) -> Result<RecheckStep, StoreError> {
    if max_slots == 0 || max_slots > STAT_BATCH_SIZE {
        return Err(StoreError::InvalidInput);
    }
    match recover_pending(client, attempt, hash).await? {
        AdmissionOutcome::ResumedSameCandidate(handle, _) => {
            drive_recheck(client, &handle, transport, max_slots).await
        }
        AdmissionOutcome::InconsistentOrMissing | AdmissionOutcome::StaleOrDiverged => {
            Ok(RecheckStep::StaleOrDiverged)
        }
        _ => Err(StoreError::Inconsistent),
    }
}

/// Process at most `max_slots` slot/side pairs (1..=STAT_BATCH_SIZE). Each pair
/// issues at most one STAT and, ONLY after an answered Missing, one same-ID POST.
/// POST/duplicate success spends accounting but never confirms presence: the next
/// scheduled pass must STAT again. Transport/refused answers spend no repost
/// budget and cannot authorize fresh posting. Already confirmed slots are skipped.
///
/// Callers acquire/recover the persisted attempt/candidate and explicitly begin
/// the first round. This function never starts a new round to reset a budget.
/// There is no transaction across a transport await. Physical POST can repeat
/// after a crash before its acknowledgment commits; only stored-plan same-ID
/// bytes are permitted, not a network exactly-once claim. Unknown commits return
/// immediately; recover/load before any subsequent request.
pub async fn drive_recheck<T: GenerationTransport>(
    client: &mut Client,
    handle: &GenerationHandle,
    transport: &mut T,
    max_slots: usize,
) -> Result<RecheckStep, StoreError> {
    if max_slots == 0 || max_slots > STAT_BATCH_SIZE {
        return Err(StoreError::InvalidInput);
    }
    let Some(checkpoint) = load_checkpoint(client, handle).await? else {
        // The terminal commit may have succeeded without its acknowledgment.
        // Retry only the original fence's receipt, never rebind an old answer.
        let tx = client.transaction().await.map_err(database_error)?;
        let failed =
            stopped_candidate_receipt(&tx, handle, CandidateStop::VerificationFailed).await?;
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
        return Ok(if failed {
            RecheckStep::VerificationFailed
        } else {
            RecheckStep::StaleOrDiverged
        });
    };
    if checkpoint.presence_verified {
        return Ok(RecheckStep::PresenceComplete(Box::new(handle.clone())));
    }
    // Repair a lost acknowledgment after all evidence was committed, including
    // a crash between pass completion and its separate presence-completion write.
    if checkpoint.round > 0 && checkpoint.slots.iter().flatten().all(|s| s.confirmed) {
        return finish(client, handle.clone()).await;
    }
    let permit = match prepare_recheck(client, handle).await? {
        RecheckPreparation::Ready(permit) => permit,
        RecheckPreparation::Waiting { not_before } => {
            return Ok(RecheckStep::Waiting { not_before });
        }
        RecheckPreparation::Exhausted => {
            return Ok(match fail_candidate_verification(client, handle).await? {
                VerificationFailureOutcome::Applied | VerificationFailureOutcome::AlreadyFailed => {
                    RecheckStep::VerificationFailed
                }
                VerificationFailureOutcome::InsufficientEvidence => RecheckStep::KeepOriginal,
                VerificationFailureOutcome::StaleOrDiverged => RecheckStep::StaleOrDiverged,
            });
        }
        RecheckPreparation::Untracked => return Ok(RecheckStep::KeepOriginal),
        RecheckPreparation::StaleOrDiverged => return Ok(RecheckStep::StaleOrDiverged),
    };
    // Only a due delivery renews the existing live claim. Waiting, exhausted
    // and already-complete deliveries do not keep otherwise idle work alive.
    if !renew_candidate_lease(client, handle).await? {
        return Ok(RecheckStep::StaleOrDiverged);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    if lock_candidate(&tx, handle).await?.is_none() {
        return Ok(RecheckStep::StaleOrDiverged);
    }
    let bundle = stored(&tx, &handle.generation).await?;
    if bundle.plan_sha256() != checkpoint.plan_sha256() {
        return Err(StoreError::Inconsistent);
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;

    let mut handle = handle.clone();
    let total = checkpoint.article_count() as i32 * 2;
    let end = (permit.cursor + max_slots as i32).min(total);
    for cursor in permit.cursor..end {
        let slot = cursor / 2;
        let side = if cursor % 2 == 0 {
            ArticleSide::Post
        } else {
            ArticleSide::Verify
        };
        if !checkpoint.slots[slot as usize][side.db_value() as usize].confirmed {
            let Some(presence) = prepare_presence_check(client, &handle, slot, side).await? else {
                return Ok(RecheckStep::StaleOrDiverged);
            };
            let verdict = transport.stat(&bundle, slot, side).await;
            if !refreshed(
                record_presence_check(client, &presence, verdict).await?,
                &mut handle,
            )? {
                return Ok(RecheckStep::StaleOrDiverged);
            }
            if verdict == StatVerdict::Missing {
                match prepare_repost(client, &handle, slot, side).await? {
                    RepostPreparation::Ready(repost) => {
                        let answer = transport.repost(&bundle, slot, side).await;
                        match record_repost(client, &repost, answer).await? {
                            MutationOutcome::Incomplete
                                if matches!(
                                    answer,
                                    PostVerdict::Transport | PostVerdict::Refused
                                ) => {}
                            outcome => {
                                if !refreshed(outcome, &mut handle)? {
                                    return Ok(RecheckStep::StaleOrDiverged);
                                }
                            }
                        }
                    }
                    RepostPreparation::AlreadyConfirmed | RepostPreparation::BudgetExhausted => {}
                    RepostPreparation::StaleOrDiverged => return Ok(RecheckStep::StaleOrDiverged),
                }
            }
        }
        if !refreshed(
            rechecks::advance_cursor(client, &handle, &permit, cursor).await?,
            &mut handle,
        )? {
            return Ok(RecheckStep::StaleOrDiverged);
        }
    }
    if end != total {
        return Ok(RecheckStep::Progress(Box::new(handle)));
    }
    if !refreshed(
        complete_recheck(client, &handle, &permit).await?,
        &mut handle,
    )? {
        return Ok(RecheckStep::StaleOrDiverged);
    }
    finish(client, handle).await
}
