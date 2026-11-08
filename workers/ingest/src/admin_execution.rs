//! One resumable admin stage per call, using the existing checkpoint. This is
//! internal execution, not an authentication boundary or a live queue route.
//! The caller must supply job/key/document from the authenticated original
//! reader, load the durable checkpoint, and enforce its service deadline.
//! Match and STAT are replayable reads; publication is deliberately not here.

use std::future::Future;

use crate::admin_gates;
use crate::jobs::{self, JobCheckpoint, JobSource, JobState};
use crate::probe::{ProbeOutcome, ProbeRequest, MAX_PROBE_SEGMENTS, MAX_SEGMENT_BYTES};
use nntune_core::nzb::Document;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageError {
    Invalid,
    Match,
    Probe,
    Store,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageOutcome {
    Continue,
    Parked,
    ReadyToPublish,
    Settled,
}

/// Implementations use the CC0 catalogue mirror and the authorized read-account
/// probe service, not user-supplied providers. Neither parser content nor probe
/// envelopes may be logged or persisted. Errors are fixed categories only.
pub trait StageRuntime {
    fn match_release(
        &mut self,
        parsed: &Document,
    ) -> impl Future<Output = Result<Option<String>, StageError>>;
    fn probe(
        &mut self,
        request: &ProbeRequest,
    ) -> impl Future<Output = Result<ProbeOutcome, StageError>>;
    /// Atomically compare the complete expected checkpoint and persist next.
    /// False means stale: reload before doing any more work. No unconditional
    /// KV put can implement this contract. This is not a publication lease.
    /// Implementations must also enforce the active admin-job identity; live
    /// parked-state SQL persistence waits for its approved schema integration.
    fn persist(
        &mut self,
        expected: &JobCheckpoint,
        next: &JobCheckpoint,
    ) -> impl Future<Output = Result<bool, StageError>>;
}

/// Borrow segment identities rather than clone a document-sized manifest. Only
/// the selected bounded batch is copied into an ephemeral service request.
struct ProbePlan<'a> {
    segments: Vec<(&'a str, Option<u64>)>,
}

impl<'a> ProbePlan<'a> {
    fn new(parsed: &'a Document) -> Result<Self, StageError> {
        if parsed.files.is_empty() || parsed.files.len() > crate::nzb_intake::MAX_FILES {
            return Err(StageError::Invalid);
        }
        let mut segments = Vec::new();
        let mut total_bytes = 0u64;
        for file in &parsed.files {
            if file.segments.is_empty() {
                return Err(StageError::Invalid);
            }
            for segment in &file.segments {
                if segments.len() == crate::nzb_intake::MAX_SEGMENTS
                    || !crate::probe::validate_message_id(&segment.message_id)
                {
                    return Err(StageError::Invalid);
                }
                let bytes = u64::try_from(segment.encoded_bytes)
                    .ok()
                    .filter(|n| (1..=MAX_SEGMENT_BYTES).contains(n))
                    .ok_or(StageError::Invalid)?;
                total_bytes = total_bytes.checked_add(bytes).ok_or(StageError::Invalid)?;
                if total_bytes > 64 << 30 {
                    return Err(StageError::Invalid);
                }
                segments.push((segment.message_id.as_str(), Some(bytes)));
            }
        }
        Ok(Self { segments })
    }

    fn batches(&self) -> usize {
        self.segments.len().div_ceil(MAX_PROBE_SEGMENTS)
    }

    fn matches(&self, cp: &JobCheckpoint) -> bool {
        cp.probe_tally.as_ref().is_some_and(|t| {
            t.total_batches == self.batches()
                && t.missing_by_batch.iter().all(|(&index, &missing)| {
                    index < self.batches()
                        && missing as usize
                            <= self
                                .segments
                                .len()
                                .saturating_sub(index * MAX_PROBE_SEGMENTS)
                                .min(MAX_PROBE_SEGMENTS)
                })
        })
    }

    fn request(&self, cp: &JobCheckpoint, index: usize) -> ProbeRequest {
        let batch = &self.segments
            [index * MAX_PROBE_SEGMENTS..self.segments.len().min((index + 1) * MAX_PROBE_SEGMENTS)];
        ProbeRequest {
            job_id: cp.job_id.clone(),
            user_id: cp.user_id.clone(),
            idempotency_key: cp.idempotency_key.clone(),
            batch_index: index,
            segment_ids: batch.iter().map(|(id, _)| (*id).to_owned()).collect(),
            segment_bytes: batch.iter().map(|(_, bytes)| *bytes).collect(),
        }
    }
}

/// Persist before returning progress, never mutate the caller's checkpoint on
/// a failed/stale write, and never run two provider stages in one invocation.
/// The next call must use a freshly loaded checkpoint and authenticated original.
/// A crash after STAT but before persist re-asks that batch; a saved batch is
/// skipped. This does not spend or reset the caller's existing retry budget.
pub async fn run_stage(
    runtime: &mut impl StageRuntime,
    cp: &mut JobCheckpoint,
    original_job: &str,
    original_key: &str,
    parsed: &Document,
) -> Result<StageOutcome, StageError> {
    validate_original(cp, original_job, original_key)?;
    if jobs::is_parked(cp.state) {
        return Ok(StageOutcome::Parked);
    }
    if jobs::is_terminal(cp.state) {
        return Ok(StageOutcome::Settled);
    }
    let plan = ProbePlan::new(parsed)?;
    if !matches!(cp.state, JobState::Queued | JobState::Parsing) && has_archive(parsed) {
        return Err(StageError::Invalid);
    }
    run_planned_stage(runtime, cp, parsed, plan).await
}

fn has_archive(parsed: &Document) -> bool {
    parsed
        .files
        .iter()
        .any(|file| file.role == "archive" || matches!(file.container.as_str(), "rar" | "7z"))
}

fn validate_original(
    cp: &JobCheckpoint,
    original_job: &str,
    original_key: &str,
) -> Result<(), StageError> {
    if cp.source != JobSource::AdminUpload
        || cp.job_id != original_job
        || cp.idempotency_key != original_key
        || !jobs::validate_job_id(original_job)
        || !(1..=jobs::MAX_ATTEMPTS).contains(&cp.attempt)
        || cp.repair_rounds != 0
        || cp.provider_id.is_some()
        || cp.par2_set_id.is_some()
        || cp.missing_blocks != 0
        || !cp.transcode_inputs.is_empty()
        || cp.transcode_next != 0
    {
        return Err(StageError::Invalid);
    }
    Ok(())
}

/// Recheck full-original coverage at the storage boundary. The caller still
/// must compare this checkpoint with durable state inside its transaction;
/// this pure check is not an execution lease or a publication authorization.
pub fn validate_publication(
    cp: &JobCheckpoint,
    original_job: &str,
    original_key: &str,
    parsed: &Document,
) -> Result<(), StageError> {
    validate_original(cp, original_job, original_key)?;
    if !admin_gates::publish_ready(cp)
        || has_archive(parsed)
        || !ProbePlan::new(parsed)?.matches(cp)
    {
        return Err(StageError::Invalid);
    }
    Ok(())
}

async fn run_planned_stage(
    runtime: &mut impl StageRuntime,
    cp: &mut JobCheckpoint,
    parsed: &Document,
    plan: ProbePlan<'_>,
) -> Result<StageOutcome, StageError> {
    let mut next = cp.clone();
    match cp.state {
        JobState::Queued => {
            jobs::advance_checkpoint(&mut next, false);
        }
        JobState::Parsing => {
            if !jobs::complete_admin_parse(&mut next, parsed) {
                return Err(StageError::Invalid);
            }
        }
        JobState::Matching => {
            let release = runtime.match_release(parsed).await?;
            if !admin_gates::complete_match(&mut next, release.as_deref(), plan.batches()) {
                return Err(StageError::Invalid);
            }
        }
        JobState::Probing | JobState::Writing => {
            if !plan.matches(cp) {
                return Err(StageError::Invalid);
            }
            if cp.state == JobState::Writing {
                return if admin_gates::publish_ready(cp) {
                    Ok(StageOutcome::ReadyToPublish)
                } else {
                    Err(StageError::Invalid)
                };
            }
            // Also handles an old persisted complete Probing checkpoint without
            // a provider read. Invalid evidence never gets as far as the service.
            if admin_gates::probe_ready(cp) {
                let tally = cp.probe_tally.as_ref().ok_or(StageError::Invalid)?;
                let index = (0..plan.batches())
                    .find(|i| !tally.missing_by_batch.contains_key(i))
                    .ok_or(StageError::Invalid)?;
                let request = plan.request(cp, index);
                let answer = runtime.probe(&request).await?;
                if !admin_gates::record_probe(&mut next, &request, &answer) {
                    return Err(StageError::Invalid);
                }
            } else {
                admin_gates::advance_probe(&mut next);
            }
        }
        _ => return Err(StageError::Invalid),
    }
    if !runtime.persist(cp, &next).await? {
        return Err(StageError::Stale);
    }
    *cp = next;
    Ok(if jobs::is_parked(cp.state) {
        StageOutcome::Parked
    } else if cp.state == JobState::Writing {
        StageOutcome::ReadyToPublish
    } else {
        StageOutcome::Continue
    })
}

#[cfg(test)]
#[path = "admin_execution_tests.rs"]
mod tests;
