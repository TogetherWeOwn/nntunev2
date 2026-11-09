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

/// Explicit controls, never inferred from Queue replay or re-upload. The caller
/// must enforce existing admin authorization and, for ResumeArchive, the archive
/// reader prerequisite before invoking the store. This type grants no authority.
#[derive(Clone, Copy)]
pub enum Control<'a> {
    ApproveReview { release: &'a str },
    RetryReview,
    ResumeArchive,
    Reject,
}

/// Derive a manual successor, not a caller-supplied replacement checkpoint.
/// Storage must compare the full expected snapshot and rotate its generation
/// under the same job lock as publication. No service calls occur here.
pub(crate) fn controlled_checkpoint(
    expected: &JobCheckpoint,
    control: Control<'_>,
    original_job: &str,
    original_key: &str,
    parsed: &Document,
) -> Result<JobCheckpoint, StageError> {
    validate_original(expected, original_job, original_key)?;
    if jobs::is_terminal(expected.state) {
        return Err(StageError::Invalid);
    }
    let mut next = expected.clone();
    let allowed = match control {
        Control::ApproveReview { release } => {
            if has_archive(parsed) {
                return Err(StageError::Invalid);
            }
            admin_gates::approve_review(&mut next, release, ProbePlan::new(parsed)?.batches())
        }
        Control::RetryReview => {
            if has_archive(parsed) {
                return Err(StageError::Invalid);
            }
            ProbePlan::new(parsed)?;
            admin_gates::retry_review(&mut next)
        }
        Control::ResumeArchive => {
            ProbePlan::new(parsed)?;
            jobs::resume_archive(&mut next)
        }
        Control::Reject => {
            next.state = JobState::Rejected;
            next.admin_release_mbid = None;
            next.probe_tally = None;
            next.failure_code = None;
            next.failure_reason = None;
            true
        }
    };
    if !allowed {
        return Err(StageError::Invalid);
    }
    Ok(next)
}

/// Returned only after the failure checkpoint commits. The caller schedules the
/// retry outside the transaction; this is not a durable timer or queue send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceFailureOutcome {
    Retry { delay_secs: u32 },
    Parked,
}

/// Settle only a classified transient match/probe service failure. Invalid input,
/// persistence errors, stale results and uncertain publication commits must never
/// spend this budget. No provider detail is accepted or retained. Progress and
/// generation survive retries; explicit review controls alone reset evidence.
pub(crate) fn service_failure_checkpoint(
    expected: &JobCheckpoint,
    failure: StageError,
    retry_after_secs: Option<u32>,
    original_job: &str,
    original_key: &str,
    parsed: &Document,
) -> Result<(JobCheckpoint, ServiceFailureOutcome), StageError> {
    validate_original(expected, original_job, original_key)?;
    let plan = ProbePlan::new(parsed)?;
    if has_archive(parsed) {
        return Err(StageError::Invalid);
    }
    let code = match (expected.state, failure) {
        (JobState::Matching, StageError::Match) => crate::retry::ERR_MATCH_FAILED,
        (JobState::Probing, StageError::Probe)
            if plan.matches(expected) && admin_gates::probe_ready(expected) =>
        {
            crate::retry::ERR_PROBE_FAILED
        }
        _ => return Err(StageError::Invalid),
    };
    let mut next = expected.clone();
    let outcome = if let Some(delay_secs) = crate::retry::record_redelivery(&mut next) {
        ServiceFailureOutcome::Retry {
            delay_secs: retry_after_secs.map_or(delay_secs, |hint| {
                delay_secs.max(hint.clamp(1, crate::retry::MAX_HINTED_RETRY_DELAY_SECS))
            }),
        }
    } else {
        // validate_original already bounded attempt, so this is exhaustion,
        // never a permanent Failed state or a payload-carrying DLQ entry.
        next.state = JobState::NeedsReview;
        next.failure_code = Some(code.into());
        next.failure_reason = Some("retry budget exhausted".into());
        ServiceFailureOutcome::Parked
    };
    Ok((next, outcome))
}

/// Reconstruct exactly one ordinary stage transition at the storage boundary.
/// Manual controls, retry-budget changes and publication are separate atomic
/// operations; none can be smuggled through ordinary stage persistence.
/// Match/probe results remain the trusted runtime's inputs, but their retained
/// geometry, ordering and every unchanged checkpoint field are checked here.
pub fn validate_stage_transition(
    expected: &JobCheckpoint,
    next: &JobCheckpoint,
    original_job: &str,
    original_key: &str,
    parsed: &Document,
) -> Result<(), StageError> {
    validate_original(expected, original_job, original_key)?;
    let plan = ProbePlan::new(parsed)?;
    if !matches!(expected.state, JobState::Queued | JobState::Parsing) && has_archive(parsed) {
        return Err(StageError::Invalid);
    }
    let mut computed = expected.clone();
    match expected.state {
        JobState::Queued => {
            jobs::advance_checkpoint(&mut computed, false);
        }
        JobState::Parsing => {
            if !jobs::complete_admin_parse(&mut computed, parsed) {
                return Err(StageError::Invalid);
            }
        }
        JobState::Matching => {
            if !admin_gates::complete_match(
                &mut computed,
                next.admin_release_mbid.as_deref(),
                plan.batches(),
            ) {
                return Err(StageError::Invalid);
            }
        }
        JobState::Probing => {
            if !plan.matches(expected) {
                return Err(StageError::Invalid);
            }
            if admin_gates::probe_ready(expected) {
                let tally = computed.probe_tally.as_mut().ok_or(StageError::Invalid)?;
                let index = (0..plan.batches())
                    .find(|i| !tally.missing_by_batch.contains_key(i))
                    .ok_or(StageError::Invalid)?;
                let missing = next
                    .probe_tally
                    .as_ref()
                    .and_then(|t| t.missing_by_batch.get(&index))
                    .copied()
                    .filter(|n| {
                        *n as usize
                            <= plan
                                .segments
                                .len()
                                .saturating_sub(index * MAX_PROBE_SEGMENTS)
                                .min(MAX_PROBE_SEGMENTS)
                    })
                    .ok_or(StageError::Invalid)?;
                tally.missing_by_batch.insert(index, missing);
            }
            admin_gates::advance_probe(&mut computed);
        }
        _ => return Err(StageError::Invalid),
    }
    if computed != *next {
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
