//! Real writer admission plus the real writer-aware admission sequencer.
//! Only SQL execution/ledger/queue/meter are simulated. A bare job-identity
//! query sees the persisted row even after deletion starts; a current-owner
//! fenced query does not. No pre-injected WriterDb/ledger admission errors.

#[path = "admission_identity_sql.rs"]
mod admission_identity_sql;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::retry::{JobError, ERR_ACCOUNT_DELETING};
use crate::writer_execution::{self, DbFailure, Value, WriterDb};
use crate::{
    drive_metered_admission_with_writer, AdmissionDecision, AdmissionLedger, AdmissionMark,
    MeterOutcome, MeterWriteError, QueueMsg,
};

const TIME: i64 = 1_730_000_123;

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Insert,
    IdentityQuery,
    WriterAccepted,
    Claim,
    Queue,
    Meter,
    Commit,
    Release,
}

type Events = Rc<RefCell<Vec<Event>>>;

struct OwnerDb {
    owner_live: Cell<bool>,
    stored_identity: Option<(String, String)>,
    msg: QueueMsg,
    events: Events,
}

impl OwnerDb {
    fn new(msg: &QueueMsg, live: bool, identity: Option<(String, String)>) -> Self {
        Self {
            owner_live: Cell::new(live),
            stored_identity: identity,
            msg: msg.clone(),
            events: Rc::default(),
        }
    }
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

impl WriterDb for OwnerDb {
    async fn execute(&self, sql: &'static str, params: &[Value]) -> Result<u64, DbFailure> {
        assert_eq!(sql, crate::ingest_jobs::upsert_ingest_job_sql());
        assert_eq!(
            params,
            [
                text(&self.msg.job_id),
                text(&self.msg.user_id),
                text(&self.msg.idempotency_key),
                Value::I64(TIME),
            ],
            "admission must bind the current request's owner and identity"
        );
        self.events.borrow_mut().push(Event::Insert);
        // Existing job => ON CONFLICT does nothing. No owner => the fenced
        // INSERT sees no row. Either path owes a real revalidation SELECT.
        Ok(0)
    }

    async fn query(
        &self,
        sql: &'static str,
        params: &[Value],
    ) -> Result<Vec<Vec<Value>>, DbFailure> {
        assert_eq!(params, [text(&self.msg.job_id)]);
        self.events.borrow_mut().push(Event::IdentityQuery);
        let live_fenced = admission_identity_sql::live_owner_identity_query(sql);
        if live_fenced && !self.owner_live.get() {
            return Ok(vec![]);
        }
        Ok(self
            .stored_identity
            .as_ref()
            .map(|(owner, key)| vec![vec![text(owner), text(key)]])
            .unwrap_or_default())
    }
}

struct WriterLedger {
    db: OwnerDb,
    mark: AdmissionMark,
    claimed: bool,
}

impl WriterLedger {
    fn replay(mark: AdmissionMark, live: bool) -> Self {
        let msg = message();
        let identity = Some((msg.user_id.clone(), msg.idempotency_key.clone()));
        Self {
            db: OwnerDb::new(&msg, live, identity),
            mark,
            claimed: false,
        }
    }
}

impl AdmissionLedger for WriterLedger {
    async fn admit_job(&mut self, msg: &QueueMsg) -> Result<(), JobError> {
        writer_execution::admit_job(
            &self.db,
            &msg.job_id,
            &msg.user_id,
            &msg.idempotency_key,
            TIME,
        )
        .await?;
        self.db.events.borrow_mut().push(Event::WriterAccepted);
        Ok(())
    }

    async fn record_stage_once(
        &mut self,
        job_id: &str,
        stage: &str,
    ) -> Result<AdmissionMark, MeterWriteError> {
        assert_eq!(job_id, self.db.msg.job_id);
        assert_eq!(stage, "parse");
        self.db.events.borrow_mut().push(Event::Claim);
        if self.mark == AdmissionMark::Queued {
            return Ok(AdmissionMark::Queued);
        }
        assert!(!self.claimed);
        self.claimed = true;
        Ok(AdmissionMark::Pending)
    }

    async fn mark_queued(&mut self, job_id: &str, stage: &str) -> Result<(), MeterWriteError> {
        assert_eq!(job_id, self.db.msg.job_id);
        assert_eq!(stage, "parse");
        assert!(self.claimed);
        self.db.events.borrow_mut().push(Event::Commit);
        self.mark = AdmissionMark::Queued;
        self.claimed = false;
        Ok(())
    }

    async fn release_claim(&mut self, job_id: &str, stage: &str) {
        assert_eq!(job_id, self.db.msg.job_id);
        assert_eq!(stage, "parse");
        self.db.events.borrow_mut().push(Event::Release);
        self.claimed = false;
    }
}

fn message() -> QueueMsg {
    crate::dispatch::build_upload_dispatch("user_01", crate::fixtures::SMALL_NZB)
        .expect("valid owned upload fixture")
}

async fn sequence(ledger: &mut WriterLedger) -> Result<AdmissionDecision, JobError> {
    let msg = ledger.db.msg.clone();
    let queue_events = Rc::clone(&ledger.db.events);
    let meter_events = Rc::clone(&ledger.db.events);
    drive_metered_admission_with_writer(
        ledger,
        &msg,
        "parse",
        move || {
            let events = Rc::clone(&queue_events);
            async move {
                events.borrow_mut().push(Event::Queue);
                true
            }
        },
        move |outcome| {
            let events = Rc::clone(&meter_events);
            async move {
                assert_eq!(outcome, MeterOutcome::Ok);
                events.borrow_mut().push(Event::Meter);
                Ok(())
            }
        },
    )
    .await
}

#[track_caller]
fn deleting<T: std::fmt::Debug>(result: Result<T, JobError>) {
    let error = result.expect_err("persisted identity is not permission to admit a deleting owner");
    assert_eq!(error.code, ERR_ACCOUNT_DELETING);
    assert!(!error.transient);
    assert!(!error.made_progress);
    assert_eq!(error.retry_after_secs, None);
}

#[track_caller]
fn no_admission_effects(ledger: &WriterLedger, original: AdmissionMark) {
    let events = ledger.db.events.borrow();
    assert!(events.contains(&Event::Insert));
    assert!(events.contains(&Event::IdentityQuery));
    assert!(
        events.iter().all(|event| !matches!(
            event,
            Event::WriterAccepted | Event::Queue | Event::Meter | Event::Commit
        )),
        "deleting owner must not publish, meter or advance the ledger: {events:?}"
    );
    assert_eq!(ledger.mark, original);
    assert!(
        !ledger.claimed,
        "a refused writer must not strand its admission claim"
    );
}

#[tokio::test]
async fn actual_admit_job_refuses_a_persisted_same_identity_after_owner_deletion() {
    let msg = message();
    let db = OwnerDb::new(
        &msg,
        false,
        Some((msg.user_id.clone(), msg.idempotency_key.clone())),
    );
    deleting(
        writer_execution::admit_job(&db, &msg.job_id, &msg.user_id, &msg.idempotency_key, TIME)
            .await,
    );
    assert_eq!(*db.events.borrow(), [Event::Insert, Event::IdentityQuery]);
}

#[tokio::test]
async fn pending_replay_for_deleting_owner_never_queues_meters_or_commits() {
    let mut ledger = WriterLedger::replay(AdmissionMark::Pending, false);
    let result = sequence(&mut ledger).await;
    // Check the escaped side effects even when the regression wrongly returns
    // Admitted, so RED names the unsafe outcome rather than a fixture mismatch.
    no_admission_effects(&ledger, AdmissionMark::Pending);
    deleting(result);
}

#[tokio::test]
async fn queued_replay_for_deleting_owner_is_not_successful_admission() {
    let mut ledger = WriterLedger::replay(AdmissionMark::Queued, false);
    let result = sequence(&mut ledger).await;
    no_admission_effects(&ledger, AdmissionMark::Queued);
    deleting(result);
}

#[tokio::test]
async fn live_same_identity_pending_replay_reaches_queue_meter_then_commit() {
    let mut ledger = WriterLedger::replay(AdmissionMark::Pending, true);
    assert_eq!(sequence(&mut ledger).await, Ok(AdmissionDecision::Admitted));
    let events = ledger.db.events.borrow();
    let position = |event| {
        events
            .iter()
            .position(|candidate| *candidate == event)
            .unwrap()
    };
    assert!(position(Event::IdentityQuery) < position(Event::WriterAccepted));
    assert!(position(Event::WriterAccepted) < position(Event::Queue));
    assert!(position(Event::Claim) < position(Event::Queue));
    assert!(position(Event::Queue) < position(Event::Meter));
    assert!(position(Event::Meter) < position(Event::Commit));
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == Event::Queue)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == Event::Meter)
            .count(),
        1
    );
    assert_eq!(ledger.mark, AdmissionMark::Queued);
    assert!(!ledger.claimed);
}

#[tokio::test]
async fn live_queued_replay_revalidates_the_real_writer_without_duplicate_queue_or_meter() {
    let mut ledger = WriterLedger::replay(AdmissionMark::Queued, true);
    assert_eq!(
        sequence(&mut ledger).await,
        Ok(AdmissionDecision::AlreadyQueued)
    );
    let events = ledger.db.events.borrow();
    assert!(events.contains(&Event::IdentityQuery));
    assert!(events.contains(&Event::WriterAccepted));
    assert!(!events
        .iter()
        .any(|event| matches!(event, Event::Queue | Event::Meter | Event::Commit)));
    assert_eq!(ledger.mark, AdmissionMark::Queued);
}

#[tokio::test]
async fn deletion_starting_after_a_successful_admission_is_revalidated_on_replay() {
    let mut ledger = WriterLedger::replay(AdmissionMark::Pending, true);
    assert_eq!(sequence(&mut ledger).await, Ok(AdmissionDecision::Admitted));
    ledger.db.events.borrow_mut().clear();
    ledger.db.owner_live.set(false);
    let result = sequence(&mut ledger).await;
    no_admission_effects(&ledger, AdmissionMark::Queued);
    deleting(result);
}

#[tokio::test]
async fn missing_replay_identity_is_permanently_refused_by_the_actual_writer() {
    let msg = message();
    let db = OwnerDb::new(&msg, false, None);
    deleting(
        writer_execution::admit_job(&db, &msg.job_id, &msg.user_id, &msg.idempotency_key, TIME)
            .await,
    );
    assert_eq!(*db.events.borrow(), [Event::Insert, Event::IdentityQuery]);
}

#[tokio::test]
async fn live_owner_cannot_adopt_another_owners_or_idempotency_keys_job() {
    let msg = message();
    for (owner, key) in [
        ("other_owner", msg.idempotency_key.as_str()),
        (msg.user_id.as_str(), "ingest:user_01:other_payload:upload"),
    ] {
        let db = OwnerDb::new(&msg, true, Some((owner.into(), key.into())));
        let error =
            writer_execution::admit_job(&db, &msg.job_id, &msg.user_id, &msg.idempotency_key, TIME)
                .await
                .expect_err("live owner alone cannot repair a foreign replay identity");
        assert!(!error.transient);
        assert_eq!(*db.events.borrow(), [Event::Insert, Event::IdentityQuery]);
    }
}
