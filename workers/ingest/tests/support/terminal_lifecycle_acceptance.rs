//! Drive real checkpoint settlement into real raw-message delivery. Recording
//! peers replace only durable/edge effects, not either production sequencer.

use super::{
    apply_settlement_with, settle_checkpoint_with, DeliveryRuntime, Settlement, SettlementRuntime,
};
use crate::{
    jobs::{JobCheckpoint, JobSource, JobState},
    queue::QueueMsg,
    retry::{DlqEntry, JobError, ERR_PARSE_FAILED, ERR_PROBE_FAILED, ERR_WRITE_FAILED},
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    TerminalAttempt(QueueMsg, &'static str),
    DurableTerminal(QueueMsg, &'static str),
    Persist(JobCheckpoint),
    Enqueue(QueueMsg),
    Dlq(DlqEntry),
    Ack,
    Retry(u32),
}

#[derive(Default)]
struct RuntimePeer {
    events: RefCell<Vec<Event>>,
    terminal_results: RefCell<VecDeque<Result<(), JobError>>>,
    persist_results: RefCell<VecDeque<bool>>,
    enqueue_results: RefCell<VecDeque<bool>>,
    dlq_results: RefCell<VecDeque<bool>>,
    dlq_ack_allowed: Cell<Option<bool>>,
    saved: RefCell<Option<JobCheckpoint>>,
}

impl SettlementRuntime for RuntimePeer {
    async fn settle_job(&self, msg: &QueueMsg, status: &'static str) -> Result<(), JobError> {
        self.events
            .borrow_mut()
            .push(Event::TerminalAttempt(msg.clone(), status));
        tokio::task::yield_now().await;
        let result = self
            .terminal_results
            .borrow_mut()
            .pop_front()
            .unwrap_or(Ok(()));
        if result.is_ok() {
            self.events
                .borrow_mut()
                .push(Event::DurableTerminal(msg.clone(), status));
        }
        result
    }

    async fn persist_checkpoint(&self, cp: &JobCheckpoint) -> bool {
        self.events.borrow_mut().push(Event::Persist(cp.clone()));
        let accepted = self
            .persist_results
            .borrow_mut()
            .pop_front()
            .unwrap_or(true);
        if accepted {
            *self.saved.borrow_mut() = Some(cp.clone());
        }
        accepted
    }

    async fn enqueue_continuation(&self, msg: &QueueMsg) -> bool {
        self.events.borrow_mut().push(Event::Enqueue(msg.clone()));
        self.enqueue_results
            .borrow_mut()
            .pop_front()
            .unwrap_or(true)
    }
}

impl DeliveryRuntime for RuntimePeer {
    async fn quarantine(&self, entry: &DlqEntry) -> bool {
        self.events.borrow_mut().push(Event::Dlq(entry.clone()));
        self.dlq_ack_allowed.set(Some(false));
        tokio::task::yield_now().await;
        let accepted = self.dlq_results.borrow_mut().pop_front().unwrap_or(true);
        self.dlq_ack_allowed.set(Some(accepted));
        accepted
    }

    fn ack(&self) {
        assert_ne!(
            self.dlq_ack_allowed.get(),
            Some(false),
            "an in-flight or rejected DLQ send cannot authorize ack"
        );
        self.events.borrow_mut().push(Event::Ack);
    }

    fn retry(&self, seconds: u32) {
        self.events.borrow_mut().push(Event::Retry(seconds));
    }
}

fn message() -> QueueMsg {
    let mut msg = crate::dispatch::build_upload_dispatch("user_01", crate::fixtures::SMALL_NZB)
        .expect("valid upload fixture");
    msg.trace_id = Some("0123456789abcdef0123456789abcdef".into());
    msg.provider_id = Some("12345678-1234-4234-8234-123456789abc".into());
    msg
}

fn checkpoint(msg: &QueueMsg, state: JobState) -> JobCheckpoint {
    let mut cp = crate::checkpoint::initial(msg);
    cp.state = state;
    cp
}

fn failed_checkpoint(msg: &QueueMsg) -> JobCheckpoint {
    let mut cp = checkpoint(msg, JobState::Failed);
    cp.attempt = 5;
    cp.failure_code = Some(ERR_PARSE_FAILED.into());
    cp.failure_reason = Some("persisted parse failure".into());
    cp
}

fn dlq_entry() -> DlqEntry {
    let mut msg = message();
    msg.attempt = 5;
    DlqEntry {
        message: msg,
        attempts: 5,
        error_code: ERR_PARSE_FAILED.into(),
        reason: "parse_failed: persisted parse failure".into(),
    }
}

async fn settle_and_deliver(
    peer: &RuntimePeer,
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
    step: Result<(), JobError>,
    step_ran: bool,
) -> Settlement {
    let settlement = settle_checkpoint_with(peer, msg, cp, step, step_ran).await;
    apply_settlement_with(peer, settlement.clone()).await;
    settlement
}

fn quarantine(settlement: &Settlement) -> &DlqEntry {
    match settlement {
        Settlement::Quarantine(entry) => entry,
        other => panic!("permanent or spent-budget failure must quarantine, got {other:?}"),
    }
}

fn persisted(events: &[Event]) -> Vec<&JobCheckpoint> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Persist(cp) => Some(cp),
            _ => None,
        })
        .collect()
}

fn delivery_events(events: &[Event]) -> Vec<Event> {
    events
        .iter()
        .filter(|event| matches!(event, Event::Dlq(_) | Event::Ack | Event::Retry(_)))
        .cloned()
        .collect()
}

fn assert_terminal_before_persist(events: &[Event], msg: &QueueMsg, status: &'static str) {
    let durable = events
        .iter()
        .position(|event| *event == Event::DurableTerminal(msg.clone(), status))
        .expect("terminal writer must complete before exposing terminal state");
    let persist = events
        .iter()
        .position(|event| matches!(event, Event::Persist(_)))
        .expect("newly terminal checkpoint must be persisted");
    assert!(
        durable < persist,
        "terminal SQL must precede KV: {events:?}"
    );
}

#[tokio::test]
async fn newly_done_is_durable_before_one_done_checkpoint_and_one_ack() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Done);
    let peer = RuntimePeer::default();
    assert_eq!(
        settle_and_deliver(&peer, &msg, &mut cp, Ok(()), true).await,
        Settlement::Ack
    );
    assert_eq!(
        *peer.events.borrow(),
        vec![
            Event::TerminalAttempt(msg.clone(), "done"),
            Event::DurableTerminal(msg, "done"),
            Event::Persist(cp.clone()),
            Event::Ack,
        ]
    );
    assert_eq!(*peer.saved.borrow(), Some(cp));
}

#[tokio::test]
async fn permanent_step_failure_terminalizes_failed_before_kv_then_dlq_then_ack() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Parsing);
    let peer = RuntimePeer::default();
    let settlement = settle_and_deliver(
        &peer,
        &msg,
        &mut cp,
        Err(JobError::permanent(ERR_PARSE_FAILED, "bad nzb")),
        true,
    )
    .await;
    let entry = quarantine(&settlement);
    assert_eq!(entry.error_code, ERR_PARSE_FAILED);
    assert_eq!(entry.attempts, 1);
    assert_eq!(entry.message.job_id, msg.job_id);
    assert_eq!(entry.message.user_id, msg.user_id);
    assert!(entry.reason.contains("bad nzb"));
    assert_eq!(cp.state, JobState::Failed);
    assert_eq!(cp.failure_code.as_deref(), Some(ERR_PARSE_FAILED));
    let events = peer.events.borrow();
    assert_terminal_before_persist(&events, &msg, "failed");
    assert_eq!(persisted(&events), vec![&cp]);
    assert_eq!(
        delivery_events(&events),
        vec![Event::Dlq(entry.clone()), Event::Ack]
    );
}

#[tokio::test]
async fn transient_step_failures_spend_five_durable_attempts_before_terminal_quarantine() {
    let msg = message();
    let original_msg = msg.clone();
    let mut cp = checkpoint(&msg, JobState::Parsing);
    let peer = RuntimePeer::default();
    for (attempt, delay) in [(1, 5), (2, 15), (3, 30), (4, 60)] {
        assert_eq!(cp.attempt, attempt);
        let start = peer.events.borrow().len();
        assert_eq!(
            settle_and_deliver(
                &peer,
                &msg,
                &mut cp,
                Err(JobError::transient(
                    ERR_PARSE_FAILED,
                    "temporarily unavailable"
                )),
                true,
            )
            .await,
            Settlement::RetryAfter(delay)
        );
        let events = peer.events.borrow();
        assert_eq!(
            &events[start..],
            &[Event::Persist(cp.clone()), Event::Retry(delay)]
        );
        assert_eq!(cp.state, JobState::Parsing);
        assert_eq!(cp.attempt, attempt + 1);
        assert_eq!(
            msg, original_msg,
            "native redelivery body must remain unchanged"
        );
        drop(events);
        cp = peer
            .saved
            .borrow()
            .clone()
            .expect("next delivery reads durable attempt");
    }
    let start = peer.events.borrow().len();
    let settlement = settle_and_deliver(
        &peer,
        &msg,
        &mut cp,
        Err(JobError::transient(
            ERR_PARSE_FAILED,
            "temporarily unavailable",
        )),
        true,
    )
    .await;
    let entry = quarantine(&settlement);
    assert_eq!(entry.error_code, ERR_PARSE_FAILED);
    assert_eq!(entry.attempts, 5);
    assert_eq!(entry.message.attempt, 5);
    assert_eq!(cp.state, JobState::Failed);
    let events = peer.events.borrow();
    let final_events = &events[start..];
    assert_terminal_before_persist(final_events, &msg, "failed");
    assert_eq!(persisted(final_events), vec![&cp]);
    assert_eq!(
        delivery_events(final_events),
        vec![Event::Dlq(entry.clone()), Event::Ack]
    );
}

#[tokio::test]
async fn done_replay_revalidates_terminal_without_rewriting_checkpoint_or_spending_attempt() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Done);
    cp.attempt = 4;
    let original = cp.clone();
    let peer = RuntimePeer::default();
    assert_eq!(
        settle_and_deliver(
            &peer,
            &msg,
            &mut cp,
            Err(JobError::permanent(
                ERR_PARSE_FAILED,
                "must not reinterpret replay"
            )),
            false,
        )
        .await,
        Settlement::Ack
    );
    assert_eq!(cp, original);
    assert_eq!(
        *peer.events.borrow(),
        vec![
            Event::TerminalAttempt(msg.clone(), "done"),
            Event::DurableTerminal(msg, "done"),
            Event::Ack,
        ]
    );
    assert!(peer.saved.borrow().is_none());
}

#[tokio::test]
async fn failed_replay_revalidates_failed_and_requarantines_original_failure_without_kv() {
    let msg = message();
    let mut cp = failed_checkpoint(&msg);
    let original = cp.clone();
    let peer = RuntimePeer::default();
    let settlement = settle_and_deliver(
        &peer,
        &msg,
        &mut cp,
        Err(JobError::permanent(
            ERR_WRITE_FAILED,
            "must not replace stored failure",
        )),
        false,
    )
    .await;
    let entry = quarantine(&settlement);
    assert_eq!(entry.error_code, ERR_PARSE_FAILED);
    assert_eq!(entry.attempts, 5);
    assert!(entry.reason.contains("persisted parse failure"));
    assert_eq!(cp, original);
    assert_eq!(
        *peer.events.borrow(),
        vec![
            Event::TerminalAttempt(msg.clone(), "failed"),
            Event::DurableTerminal(msg, "failed"),
            Event::Dlq(entry.clone()),
            Event::Ack,
        ]
    );
    assert!(peer.saved.borrow().is_none());
}

#[tokio::test]
async fn nonterminal_progress_persists_before_fresh_enqueue_and_only_then_acks() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Probing);
    cp.attempt = 4;
    cp.probe_tally = Some(crate::probe::ProbeTally::new(2));
    let original = cp.clone();
    let peer = RuntimePeer::default();
    assert_eq!(
        settle_and_deliver(
            &peer,
            &msg,
            &mut cp,
            Err(JobError::progress(ERR_PROBE_FAILED, "batch budget reached")),
            true,
        )
        .await,
        Settlement::Ack
    );
    assert_eq!(cp, original);
    assert_eq!(
        *peer.events.borrow(),
        vec![Event::Persist(cp), Event::Enqueue(msg), Event::Ack,]
    );
}

#[tokio::test]
async fn continuation_enqueue_failure_retries_after_persist_without_ack_or_terminal_write() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Probing);
    cp.attempt = 4;
    let original = cp.clone();
    let peer = RuntimePeer::default();
    peer.enqueue_results.borrow_mut().push_back(false);
    assert_eq!(
        settle_and_deliver(
            &peer,
            &msg,
            &mut cp,
            Err(JobError::progress(ERR_PROBE_FAILED, "more batches remain")),
            true,
        )
        .await,
        Settlement::RetryAfter(5)
    );
    assert_eq!(cp, original);
    assert_eq!(
        *peer.events.borrow(),
        vec![
            Event::Persist(cp.clone()),
            Event::Enqueue(msg),
            Event::Retry(5),
        ]
    );
    assert_eq!(*peer.saved.borrow(), Some(cp));
}

#[tokio::test]
async fn continuation_checkpoint_failure_never_enqueues_or_acks() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Probing);
    let peer = RuntimePeer::default();
    peer.persist_results.borrow_mut().push_back(false);
    assert_eq!(
        settle_and_deliver(
            &peer,
            &msg,
            &mut cp,
            Err(JobError::progress(ERR_PROBE_FAILED, "more batches remain")),
            true,
        )
        .await,
        Settlement::RetryAfter(5)
    );
    assert_eq!(
        *peer.events.borrow(),
        vec![Event::Persist(cp), Event::Retry(5)]
    );
    assert!(peer.saved.borrow().is_none());
}

#[tokio::test]
async fn retry_hint_is_bounded_without_premature_terminalization() {
    for (hint, delay) in [(42, 42), (u32::MAX, 300)] {
        let msg = message();
        let mut cp = checkpoint(&msg, JobState::Parsing);
        let peer = RuntimePeer::default();
        assert_eq!(
            settle_and_deliver(
                &peer,
                &msg,
                &mut cp,
                Err(JobError::transient_after(ERR_PARSE_FAILED, "busy", hint)),
                true,
            )
            .await,
            Settlement::RetryAfter(delay)
        );
        assert_eq!(cp.state, JobState::Parsing);
        assert_eq!(cp.attempt, 2);
        assert_eq!(
            *peer.events.borrow(),
            vec![Event::Persist(cp), Event::Retry(delay)]
        );
    }
}

#[tokio::test]
async fn checkpoint_identity_mismatch_has_no_writer_persist_or_enqueue_effect() {
    for field in ["job", "user", "idempotency", "source"] {
        let msg = message();
        let mut cp = checkpoint(&msg, JobState::Done);
        match field {
            "job" => cp.job_id = "job_other".into(),
            "user" => cp.user_id = "other_user".into(),
            "idempotency" => cp.idempotency_key = "other-key".into(),
            "source" => cp.source = JobSource::IndexerSearch,
            _ => unreachable!(),
        }
        let original = cp.clone();
        let peer = RuntimePeer::default();
        let settlement = settle_checkpoint_with(&peer, &msg, &mut cp, Ok(()), true).await;
        assert!(
            !matches!(settlement, Settlement::Ack | Settlement::Continue),
            "{field} mismatch cannot succeed"
        );
        assert!(
            peer.events.borrow().is_empty(),
            "{field} mismatch touched durable effects"
        );
        assert_eq!(cp, original, "{field} mismatch mutated checkpoint");
    }
}

#[tokio::test]
async fn transient_done_writer_failure_cannot_persist_phantom_done_or_ack() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Done);
    let peer = RuntimePeer::default();
    peer.terminal_results
        .borrow_mut()
        .push_back(Err(JobError::transient(
            ERR_WRITE_FAILED,
            "writer unavailable",
        )));
    let settlement = settle_and_deliver(&peer, &msg, &mut cp, Ok(()), true).await;
    assert!(matches!(settlement, Settlement::RetryAfter(5..=300)));
    let events = peer.events.borrow();
    assert!(events.contains(&Event::TerminalAttempt(msg, "done")));
    assert!(persisted(&events)
        .iter()
        .all(|cp| !matches!(cp.state, JobState::Done | JobState::Failed)));
    assert!(matches!(
        delivery_events(&events).as_slice(),
        [Event::Retry(5..=300)]
    ));
    assert!(peer
        .saved
        .borrow()
        .as_ref()
        .is_none_or(|cp| cp.state != JobState::Done));
}

#[tokio::test]
async fn transient_failed_writer_failure_cannot_persist_or_quarantine_successfully() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Parsing);
    let peer = RuntimePeer::default();
    peer.terminal_results
        .borrow_mut()
        .push_back(Err(JobError::transient(
            ERR_WRITE_FAILED,
            "writer unavailable",
        )));
    let settlement = settle_and_deliver(
        &peer,
        &msg,
        &mut cp,
        Err(JobError::permanent(ERR_PARSE_FAILED, "bad nzb")),
        true,
    )
    .await;
    assert!(matches!(settlement, Settlement::RetryAfter(5..=300)));
    let events = peer.events.borrow();
    assert!(events.contains(&Event::TerminalAttempt(msg, "failed")));
    assert!(persisted(&events)
        .iter()
        .all(|cp| cp.state != JobState::Failed));
    assert!(matches!(
        delivery_events(&events).as_slice(),
        [Event::Retry(5..=300)]
    ));
}

#[tokio::test]
async fn transient_writer_failure_on_terminal_replays_preserves_sticky_checkpoint_without_kv() {
    for state in [JobState::Done, JobState::Failed] {
        let msg = message();
        let mut cp = if state == JobState::Done {
            checkpoint(&msg, state)
        } else {
            failed_checkpoint(&msg)
        };
        let original = cp.clone();
        let status = if state == JobState::Done {
            "done"
        } else {
            "failed"
        };
        let peer = RuntimePeer::default();
        peer.terminal_results
            .borrow_mut()
            .push_back(Err(JobError::transient(ERR_WRITE_FAILED, "writer busy")));
        assert_eq!(
            settle_and_deliver(&peer, &msg, &mut cp, Ok(()), false).await,
            Settlement::RetryAfter(5)
        );
        assert_eq!(cp, original);
        assert_eq!(
            *peer.events.borrow(),
            vec![Event::TerminalAttempt(msg, status), Event::Retry(5)]
        );
        assert!(peer.saved.borrow().is_none());
    }
}

#[tokio::test]
async fn deterministic_done_writer_error_quarantines_write_failed_not_unbounded_retry_or_done() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Done);
    let peer = RuntimePeer::default();
    peer.terminal_results
        .borrow_mut()
        .push_back(Err(JobError::permanent(
            ERR_WRITE_FAILED,
            "invalid durable row",
        )));
    let settlement = settle_and_deliver(&peer, &msg, &mut cp, Ok(()), true).await;
    let entry = quarantine(&settlement);
    assert_eq!(entry.error_code, ERR_WRITE_FAILED);
    let events = peer.events.borrow();
    assert!(events.contains(&Event::TerminalAttempt(msg, "done")));
    // Failure recording is permitted; successful Done persistence is not.
    for saved in persisted(&events) {
        assert_eq!(saved.state, JobState::Failed);
        assert_eq!(saved.failure_code.as_deref(), Some(ERR_WRITE_FAILED));
    }
    assert_eq!(
        delivery_events(&events),
        vec![Event::Dlq(entry.clone()), Event::Ack]
    );
}

#[tokio::test]
async fn deterministic_failed_writer_error_uses_permanent_write_failure_policy() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Parsing);
    cp.attempt = 5;
    let peer = RuntimePeer::default();
    peer.terminal_results
        .borrow_mut()
        .push_back(Err(JobError::permanent(
            ERR_WRITE_FAILED,
            "invalid durable row",
        )));
    let settlement = settle_and_deliver(
        &peer,
        &msg,
        &mut cp,
        Err(JobError::transient(ERR_PARSE_FAILED, "spent parse budget")),
        true,
    )
    .await;
    let entry = quarantine(&settlement);
    assert_eq!(entry.error_code, ERR_WRITE_FAILED);
    let events = peer.events.borrow();
    assert!(events.contains(&Event::TerminalAttempt(msg, "failed")));
    for saved in persisted(&events) {
        assert_eq!(saved.state, JobState::Failed);
        assert_eq!(saved.failure_code.as_deref(), Some(ERR_WRITE_FAILED));
    }
    assert_eq!(
        delivery_events(&events),
        vec![Event::Dlq(entry.clone()), Event::Ack]
    );
}

#[tokio::test]
async fn done_checkpoint_persist_failure_after_terminal_write_retries_without_ack() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Done);
    let peer = RuntimePeer::default();
    peer.persist_results.borrow_mut().push_back(false);
    assert_eq!(
        settle_and_deliver(&peer, &msg, &mut cp, Ok(()), true).await,
        Settlement::RetryAfter(5)
    );
    assert_eq!(
        *peer.events.borrow(),
        vec![
            Event::TerminalAttempt(msg.clone(), "done"),
            Event::DurableTerminal(msg, "done"),
            Event::Persist(cp),
            Event::Retry(5),
        ]
    );
    assert!(peer.saved.borrow().is_none());
}

#[tokio::test]
async fn failed_checkpoint_persist_failure_after_terminal_write_never_sends_dlq_or_ack() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Parsing);
    let peer = RuntimePeer::default();
    peer.persist_results.borrow_mut().push_back(false);
    assert_eq!(
        settle_and_deliver(
            &peer,
            &msg,
            &mut cp,
            Err(JobError::permanent(ERR_PARSE_FAILED, "bad nzb")),
            true,
        )
        .await,
        Settlement::RetryAfter(5)
    );
    let events = peer.events.borrow();
    assert_terminal_before_persist(&events, &msg, "failed");
    assert_eq!(persisted(&events), vec![&cp]);
    assert_eq!(delivery_events(&events), vec![Event::Retry(5)]);
    assert!(peer.saved.borrow().is_none());
}

#[tokio::test]
async fn dlq_failure_then_failed_replay_revalidates_terminal_without_rewriting_kv_or_step() {
    let msg = message();
    let mut cp = checkpoint(&msg, JobState::Parsing);
    let peer = RuntimePeer::default();
    peer.dlq_results.borrow_mut().push_back(false);
    let first = settle_and_deliver(
        &peer,
        &msg,
        &mut cp,
        Err(JobError::permanent(ERR_PARSE_FAILED, "bad nzb")),
        true,
    )
    .await;
    let first_entry = quarantine(&first).clone();
    {
        let events = peer.events.borrow();
        assert_terminal_before_persist(&events, &msg, "failed");
        assert_eq!(
            delivery_events(&events),
            vec![Event::Dlq(first_entry.clone()), Event::Retry(5)]
        );
    }
    cp = peer
        .saved
        .borrow()
        .clone()
        .expect("failed checkpoint was durable before failed DLQ");
    peer.events.borrow_mut().clear();
    let original = cp.clone();
    let replay = settle_and_deliver(&peer, &msg, &mut cp, Ok(()), false).await;
    assert_eq!(quarantine(&replay), &first_entry);
    assert_eq!(cp, original);
    assert_eq!(
        *peer.events.borrow(),
        vec![
            Event::TerminalAttempt(msg.clone(), "failed"),
            Event::DurableTerminal(msg, "failed"),
            Event::Dlq(first_entry),
            Event::Ack,
        ]
    );
}

#[tokio::test]
async fn deterministic_writer_failure_on_replays_never_rewrites_sticky_terminal_checkpoint() {
    for state in [JobState::Done, JobState::Failed] {
        let msg = message();
        let mut cp = if state == JobState::Done {
            checkpoint(&msg, state)
        } else {
            failed_checkpoint(&msg)
        };
        let original = cp.clone();
        let status = if state == JobState::Done {
            "done"
        } else {
            "failed"
        };
        let peer = RuntimePeer::default();
        peer.terminal_results
            .borrow_mut()
            .push_back(Err(JobError::permanent(
                ERR_WRITE_FAILED,
                "invalid durable row",
            )));
        let settlement = settle_and_deliver(&peer, &msg, &mut cp, Ok(()), false).await;
        let entry = quarantine(&settlement);
        assert_eq!(entry.error_code, ERR_WRITE_FAILED);
        assert_eq!(cp, original);
        assert_eq!(
            *peer.events.borrow(),
            vec![
                Event::TerminalAttempt(msg, status),
                Event::Dlq(entry.clone()),
                Event::Ack,
            ]
        );
        assert!(peer.saved.borrow().is_none());
    }
}

#[tokio::test]
async fn terminal_writer_recovery_retries_the_real_write_before_success_and_replay() {
    let msg = message();
    let durable_before_step = checkpoint(&msg, JobState::Writing);
    let mut cp = durable_before_step.clone();
    cp.state = JobState::Done;
    let peer = RuntimePeer::default();
    peer.terminal_results
        .borrow_mut()
        .push_back(Err(JobError::transient(ERR_WRITE_FAILED, "writer busy")));
    assert!(matches!(
        settle_and_deliver(&peer, &msg, &mut cp, Ok(()), true).await,
        Settlement::RetryAfter(5..=300)
    ));
    {
        let events = peer.events.borrow();
        assert!(events.contains(&Event::TerminalAttempt(msg.clone(), "done")));
        assert!(persisted(&events)
            .iter()
            .all(|cp| !matches!(cp.state, JobState::Done | JobState::Failed)));
        assert!(matches!(
            delivery_events(&events).as_slice(),
            [Event::Retry(5..=300)]
        ));
    }
    cp = peer.saved.borrow().clone().unwrap_or(durable_before_step);
    // Redelivery resumes the last durable nonterminal checkpoint; the writing
    // step completes again before entering this settlement boundary.
    cp.state = JobState::Done;
    peer.events.borrow_mut().clear();
    assert_eq!(
        settle_and_deliver(&peer, &msg, &mut cp, Ok(()), true).await,
        Settlement::Ack
    );
    assert_eq!(
        *peer.events.borrow(),
        vec![
            Event::TerminalAttempt(msg.clone(), "done"),
            Event::DurableTerminal(msg.clone(), "done"),
            Event::Persist(cp.clone()),
            Event::Ack,
        ]
    );
    cp = peer
        .saved
        .borrow()
        .clone()
        .expect("successful completion persisted Done");
    peer.events.borrow_mut().clear();
    assert_eq!(
        settle_and_deliver(&peer, &msg, &mut cp, Ok(()), false).await,
        Settlement::Ack
    );
    assert_eq!(
        *peer.events.borrow(),
        vec![
            Event::TerminalAttempt(msg.clone(), "done"),
            Event::DurableTerminal(msg, "done"),
            Event::Ack,
        ]
    );
}

#[tokio::test]
async fn raw_ack_acknowledges_once_without_retry_or_dlq() {
    let peer = RuntimePeer::default();
    apply_settlement_with(&peer, Settlement::Ack).await;
    assert_eq!(*peer.events.borrow(), vec![Event::Ack]);
}

#[tokio::test]
async fn raw_retry_forwards_its_delay_once_without_ack_or_dlq() {
    let peer = RuntimePeer::default();
    apply_settlement_with(&peer, Settlement::RetryAfter(37)).await;
    assert_eq!(*peer.events.borrow(), vec![Event::Retry(37)]);
}

#[tokio::test]
async fn defensive_unhandled_continue_retries_instead_of_acknowledging() {
    let peer = RuntimePeer::default();
    apply_settlement_with(&peer, Settlement::Continue).await;
    assert_eq!(*peer.events.borrow(), vec![Event::Retry(5)]);
}

#[tokio::test]
async fn raw_quarantine_acks_only_after_dlq_acceptance() {
    let peer = RuntimePeer::default();
    let entry = dlq_entry();
    apply_settlement_with(&peer, Settlement::Quarantine(Box::new(entry.clone()))).await;
    assert_eq!(*peer.events.borrow(), vec![Event::Dlq(entry), Event::Ack]);
}

#[tokio::test]
async fn raw_quarantine_rejection_retries_without_ack() {
    let peer = RuntimePeer::default();
    let entry = dlq_entry();
    peer.dlq_results.borrow_mut().push_back(false);
    apply_settlement_with(&peer, Settlement::Quarantine(Box::new(entry.clone()))).await;
    assert_eq!(
        *peer.events.borrow(),
        vec![Event::Dlq(entry), Event::Retry(5)]
    );
}
