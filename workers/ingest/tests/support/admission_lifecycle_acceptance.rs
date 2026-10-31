//! Independent acceptance of the real admission sequencer, with only its
//! durable ledger, queue and meter boundaries replaced by recording peers.

#[path = "admission_replay_fence_acceptance.rs"]
mod admission_replay_fence_acceptance;

use super::{
    drive_metered_admission_with_writer, AdmissionDecision, AdmissionLedger, AdmissionMark,
    MeterOutcome, MeterWriteError, QueueMsg,
};
use crate::retry::{JobError, ERR_WRITE_FAILED};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    WriteAttempt(QueueMsg),
    DurableJob(QueueMsg),
    Claim(String, String),
    Queue,
    MeterOk,
    Commit(String, String),
    Release(String, String),
}

#[derive(Default)]
struct LedgerState {
    events: Vec<Event>,
    mark: Option<AdmissionMark>,
    claimed: bool,
}

#[derive(Default)]
struct LedgerPeer {
    state: Rc<RefCell<LedgerState>>,
    writes: VecDeque<Result<(), JobError>>,
    fail_claim: bool,
    fail_commit: bool,
}

impl AdmissionLedger for LedgerPeer {
    async fn admit_job(&mut self, msg: &QueueMsg) -> Result<(), JobError> {
        self.state
            .borrow_mut()
            .events
            .push(Event::WriteAttempt(msg.clone()));
        // A call is not a durable write until its future has completed.
        tokio::task::yield_now().await;
        let result = self.writes.pop_front().unwrap_or(Ok(()));
        if result.is_ok() {
            self.state
                .borrow_mut()
                .events
                .push(Event::DurableJob(msg.clone()));
        }
        result
    }

    async fn record_stage_once(
        &mut self,
        job_id: &str,
        stage: &str,
    ) -> Result<AdmissionMark, MeterWriteError> {
        let mut state = self.state.borrow_mut();
        state.events.push(Event::Claim(job_id.into(), stage.into()));
        if self.fail_claim {
            return Err(MeterWriteError::Transport("claim unavailable".into()));
        }
        if state.mark == Some(AdmissionMark::Queued) {
            return Ok(AdmissionMark::Queued);
        }
        if state.claimed {
            return Ok(AdmissionMark::Busy);
        }
        state.claimed = true;
        state.mark = Some(AdmissionMark::Pending);
        Ok(AdmissionMark::Pending)
    }

    async fn mark_queued(&mut self, job_id: &str, stage: &str) -> Result<(), MeterWriteError> {
        let mut state = self.state.borrow_mut();
        state
            .events
            .push(Event::Commit(job_id.into(), stage.into()));
        if self.fail_commit || !state.claimed {
            return Err(MeterWriteError::Transport("commit unavailable".into()));
        }
        state.claimed = false;
        state.mark = Some(AdmissionMark::Queued);
        Ok(())
    }

    async fn release_claim(&mut self, job_id: &str, stage: &str) {
        let mut state = self.state.borrow_mut();
        state
            .events
            .push(Event::Release(job_id.into(), stage.into()));
        state.claimed = false;
    }
}

fn message() -> QueueMsg {
    let mut msg = crate::dispatch::build_upload_dispatch("user_01", crate::fixtures::SMALL_NZB)
        .expect("valid upload fixture");
    msg.trace_id = Some("0123456789abcdef0123456789abcdef".into());
    msg.provider_id = Some("12345678-1234-4234-8234-123456789abc".into());
    msg
}

async fn invoke(
    peer: &mut LedgerPeer,
    msg: &QueueMsg,
    stage: &str,
    queue_ok: bool,
    meter_ok: bool,
    yield_queue: bool,
) -> Result<AdmissionDecision, JobError> {
    let queue_state = Rc::clone(&peer.state);
    let meter_state = Rc::clone(&peer.state);
    drive_metered_admission_with_writer(
        peer,
        msg,
        stage,
        move || {
            let state = Rc::clone(&queue_state);
            async move {
                state.borrow_mut().events.push(Event::Queue);
                if yield_queue {
                    tokio::task::yield_now().await;
                }
                queue_ok
            }
        },
        move |outcome| {
            let state = Rc::clone(&meter_state);
            async move {
                assert_eq!(outcome, MeterOutcome::Ok);
                state.borrow_mut().events.push(Event::MeterOk);
                if meter_ok {
                    Ok(())
                } else {
                    Err(MeterWriteError::Rejected { status: 503 })
                }
            }
        },
    )
    .await
}

fn count(events: &[Event], predicate: impl Fn(&Event) -> bool) -> usize {
    events.iter().filter(|event| predicate(event)).count()
}

fn no_queue_or_meter(events: &[Event]) {
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, Event::Queue | Event::MeterOk | Event::Commit(_, _))),
        "unpublished admission must not queue, meter or commit: {events:?}"
    );
}

fn retryable_meter(result: Result<AdmissionDecision, JobError>) {
    match result {
        Ok(AdmissionDecision::MeterUnavailable(error)) => {
            assert!(error.is_retryable());
            assert_eq!(error.http_status(), 503);
        }
        other => panic!("ledger/meter failure must not invent admission success: {other:?}"),
    }
}

#[tokio::test]
async fn durable_write_and_claim_both_complete_before_queue_without_ordering_each_other() {
    let msg = message();
    let mut peer = LedgerPeer::default();
    assert_eq!(
        invoke(&mut peer, &msg, "parse", true, true, false).await,
        Ok(AdmissionDecision::Admitted)
    );
    let state = peer.state.borrow();
    let events = &state.events;
    let position = |wanted: &Event| events.iter().position(|event| event == wanted).unwrap();
    let queue = position(&Event::Queue);
    assert!(position(&Event::DurableJob(msg.clone())) < queue);
    assert!(position(&Event::Claim(msg.job_id.clone(), "parse".into())) < queue);
    assert!(queue < position(&Event::MeterOk));
    assert!(
        position(&Event::MeterOk) < position(&Event::Commit(msg.job_id.clone(), "parse".into()))
    );
    assert_eq!(
        count(events, |event| matches!(event, Event::WriteAttempt(_))),
        1
    );
    assert_eq!(
        count(events, |event| matches!(event, Event::DurableJob(_))),
        1
    );
    assert_eq!(count(events, |event| matches!(event, Event::Queue)), 1);
    assert_eq!(count(events, |event| matches!(event, Event::MeterOk)), 1);
    assert_eq!(
        count(events, |event| matches!(event, Event::Commit(_, _))),
        1
    );
    assert_eq!(state.mark, Some(AdmissionMark::Queued));
    assert!(!state.claimed);
}

#[tokio::test]
async fn already_queued_replay_still_awaits_durable_job_revalidation() {
    let msg = message();
    let mut peer = LedgerPeer::default();
    peer.state.borrow_mut().mark = Some(AdmissionMark::Queued);
    assert_eq!(
        invoke(&mut peer, &msg, "parse", true, true, false).await,
        Ok(AdmissionDecision::AlreadyQueued)
    );
    let state = peer.state.borrow();
    assert!(state.events.contains(&Event::DurableJob(msg)));
    assert_eq!(
        count(&state.events, |event| matches!(event, Event::DurableJob(_))),
        1
    );
    no_queue_or_meter(&state.events);
    assert_eq!(state.mark, Some(AdmissionMark::Queued));
}

async fn rejects_writer_error(error: JobError, queued_replay: bool) {
    let msg = message();
    let mut peer = LedgerPeer::default();
    if queued_replay {
        peer.state.borrow_mut().mark = Some(AdmissionMark::Queued);
    }
    peer.writes.push_back(Err(error.clone()));
    assert_eq!(
        invoke(&mut peer, &msg, "parse", true, true, false).await,
        Err(error),
        "writer classification, safe detail and retry hint must survive unchanged"
    );
    let state = peer.state.borrow();
    assert!(state.events.contains(&Event::WriteAttempt(msg)));
    assert!(!state
        .events
        .iter()
        .any(|event| matches!(event, Event::DurableJob(_))));
    no_queue_or_meter(&state.events);
    assert_eq!(state.mark == Some(AdmissionMark::Queued), queued_replay);
}

#[tokio::test]
async fn transient_admission_writer_error_retains_safe_classification_and_retry_hint() {
    rejects_writer_error(
        JobError::transient_after(ERR_WRITE_FAILED, "writer busy", 42),
        false,
    )
    .await;
}

#[tokio::test]
async fn permanent_admission_writer_error_is_not_relabelled_as_transient() {
    rejects_writer_error(
        JobError::permanent(ERR_WRITE_FAILED, "invalid durable row"),
        false,
    )
    .await;
}

#[tokio::test]
async fn deleting_account_never_queues_or_meters_a_new_job() {
    rejects_writer_error(
        JobError::permanent("account_deleting", "account deleting"),
        false,
    )
    .await;
}

#[tokio::test]
async fn queued_mark_cannot_hide_a_failed_durable_revalidation() {
    rejects_writer_error(
        JobError::transient(ERR_WRITE_FAILED, "writer unavailable"),
        true,
    )
    .await;
}

#[tokio::test]
async fn writer_failure_then_retry_then_replay_only_reports_real_admission_success() {
    let msg = message();
    let mut peer = LedgerPeer::default();
    let error = JobError::transient(ERR_WRITE_FAILED, "try again");
    peer.writes.push_back(Err(error.clone()));
    assert_eq!(
        invoke(&mut peer, &msg, "parse", true, true, false).await,
        Err(error)
    );
    no_queue_or_meter(&peer.state.borrow().events);
    assert_eq!(
        invoke(&mut peer, &msg, "parse", true, true, false).await,
        Ok(AdmissionDecision::Admitted)
    );
    assert_eq!(
        invoke(&mut peer, &msg, "parse", true, true, false).await,
        Ok(AdmissionDecision::AlreadyQueued)
    );
    let state = peer.state.borrow();
    assert_eq!(
        count(&state.events, |event| matches!(event, Event::DurableJob(_))),
        2
    );
    assert_eq!(
        count(&state.events, |event| matches!(event, Event::Queue)),
        1
    );
    assert_eq!(
        count(&state.events, |event| matches!(event, Event::MeterOk)),
        1
    );
    assert!(
        !state.claimed,
        "failed writer attempt must not strand a claim"
    );
}

#[tokio::test]
async fn unknown_stage_preserves_unknown_stage_result_without_any_boundary_effect() {
    let mut peer = LedgerPeer::default();
    let result = invoke(&mut peer, &message(), "teleport", true, true, false).await;
    match result {
        Ok(AdmissionDecision::MeterUnavailable(error @ MeterWriteError::UnknownStage(_))) => {
            assert!(!error.is_retryable());
        }
        other => panic!(
            "unknown stage must preserve its existing caller-error classification: {other:?}"
        ),
    }
    assert!(peer.state.borrow().events.is_empty());
}

#[tokio::test]
async fn busy_claim_does_not_send_meter_commit_or_release_the_other_owner() {
    let mut peer = LedgerPeer::default();
    peer.state.borrow_mut().claimed = true;
    assert_eq!(
        invoke(&mut peer, &message(), "parse", true, true, false).await,
        Ok(AdmissionDecision::Busy)
    );
    let state = peer.state.borrow();
    no_queue_or_meter(&state.events);
    assert!(!state
        .events
        .iter()
        .any(|event| matches!(event, Event::Release(_, _))));
    assert!(state.claimed);
}

#[tokio::test]
async fn claim_failure_remains_retryable_without_queue_or_meter() {
    let mut peer = LedgerPeer {
        fail_claim: true,
        ..LedgerPeer::default()
    };
    retryable_meter(invoke(&mut peer, &message(), "parse", true, true, false).await);
    no_queue_or_meter(&peer.state.borrow().events);
}

async fn failure_then_retry(queue_ok: bool, meter_ok: bool, fail_commit: bool) {
    let msg = message();
    let mut peer = LedgerPeer {
        fail_commit,
        ..LedgerPeer::default()
    };
    let result = invoke(&mut peer, &msg, "parse", queue_ok, meter_ok, false).await;
    if !queue_ok {
        assert_eq!(result, Ok(AdmissionDecision::QueueUnavailable));
    } else {
        retryable_meter(result);
    }
    {
        let state = peer.state.borrow();
        assert_eq!(state.mark, Some(AdmissionMark::Pending));
        assert!(!state.claimed, "failed attempt must release its own claim");
        assert_eq!(
            count(&state.events, |event| matches!(event, Event::Queue)),
            1
        );
        assert_eq!(
            count(&state.events, |event| matches!(event, Event::MeterOk)),
            usize::from(queue_ok)
        );
        assert!(state
            .events
            .contains(&Event::Release(msg.job_id.clone(), "parse".into())));
    }
    peer.fail_commit = false;
    assert_eq!(
        invoke(&mut peer, &msg, "parse", true, true, false).await,
        Ok(AdmissionDecision::Admitted)
    );
    let state = peer.state.borrow();
    assert_eq!(state.mark, Some(AdmissionMark::Queued));
    assert_eq!(
        count(&state.events, |event| matches!(event, Event::Queue)),
        2
    );
    assert_eq!(
        count(&state.events, |event| matches!(event, Event::MeterOk)),
        1 + usize::from(queue_ok)
    );
}

#[tokio::test]
async fn rejected_queue_releases_pending_claim_and_owes_no_meter_until_retry() {
    failure_then_retry(false, true, false).await;
}

#[tokio::test]
async fn rejected_meter_never_commits_admission_and_retry_redrives_it() {
    failure_then_retry(true, false, false).await;
}

#[tokio::test]
async fn rejected_commit_never_reports_admitted_and_retry_redrives_queue_and_meter() {
    failure_then_retry(true, true, true).await;
}

#[tokio::test]
async fn overlapping_requests_have_only_one_queue_meter_and_commit_owner() {
    let msg = message();
    let state = Rc::new(RefCell::new(LedgerState::default()));
    let mut first = LedgerPeer {
        state: Rc::clone(&state),
        ..LedgerPeer::default()
    };
    let mut second = LedgerPeer {
        state: Rc::clone(&state),
        ..LedgerPeer::default()
    };
    let (a, b) = tokio::join!(
        biased;
        invoke(&mut first, &msg, "parse", true, true, true),
        invoke(&mut second, &msg, "parse", true, true, false)
    );
    assert_eq!(a, Ok(AdmissionDecision::Admitted));
    assert_eq!(b, Ok(AdmissionDecision::Busy));
    let state = state.borrow();
    assert_eq!(
        count(&state.events, |event| matches!(event, Event::Queue)),
        1
    );
    assert_eq!(
        count(&state.events, |event| matches!(event, Event::MeterOk)),
        1
    );
    assert_eq!(
        count(&state.events, |event| matches!(event, Event::Commit(_, _))),
        1
    );
    assert_eq!(state.mark, Some(AdmissionMark::Queued));
    assert!(!state.claimed);
}
