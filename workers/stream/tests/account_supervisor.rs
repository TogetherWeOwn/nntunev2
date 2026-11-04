//! Account supervisor: durable reservation / physical-close boundary.
//!
//! Everything here runs against deterministic in-memory fakes (an atomic
//! clone-journal storage with injectable faults, and a cloneable close control
//! with a `confirmed` flag). These are NOT evidence of real Worker socket
//! closure: they only prove the supervisor's own bookkeeping and fencing rules.
//! Only the public API is used.

use std::cell::{Cell, RefCell};
use std::future::{pending, ready, Future, Ready};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::task::{Context, Waker};

use nntune_stream::account_pool::{
    with_session_deadline, AccountConfig, ConfigError, SessionExpired, WorkClass, SESSION_TTL_MS,
};
use nntune_stream::account_supervisor::{
    AccountJournal, AccountStorage, AccountSupervisor, CloseControl, CloseError, SessionId,
    SessionResult, StorageError, SupervisorError,
};
use nntune_stream::pool::{PoolError, HARD_MAX_CONNS};
use serde_json::{json, Value};

const ACCOUNT: &str = "12345678-1234-4234-8234-123456789abc";
const OTHER_ACCOUNT: &str = "52345678-1234-4234-8234-123456789abc";
const SECRET_A: &str = "22345678-1234-4234-8234-123456789abc";
const SECRET_B: &str = "32345678-1234-4234-8234-123456789abc";
const SECRET_C: &str = "42345678-1234-4234-8234-123456789abc";

// ---------------------------------------------------------------- fakes

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// Error, nothing written.
    FailBefore,
    /// Written (durable), then an error is reported.
    FailAfter,
    /// Never completes, nothing written.
    PendBefore,
    /// Written (durable), then never completes.
    PendAfter,
}

struct StoreState {
    journal: Option<AccountJournal>,
    load_fault: Fault,
    save_fault: Fault,
    loads: u32,
    save_attempts: u32,
    log: Vec<String>,
}

/// Shared handle: the test keeps one clone, the supervisor owns another.
#[derive(Clone)]
struct FakeStorage(Rc<RefCell<StoreState>>);

impl FakeStorage {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(StoreState {
            journal: None,
            load_fault: Fault::None,
            save_fault: Fault::None,
            loads: 0,
            save_attempts: 0,
            log: Vec::new(),
        })))
    }
    fn set_save_fault(&self, f: Fault) {
        self.0.borrow_mut().save_fault = f;
    }
    fn set_load_fault(&self, f: Fault) {
        self.0.borrow_mut().load_fault = f;
    }
    fn save_attempts(&self) -> u32 {
        self.0.borrow().save_attempts
    }
    fn loads(&self) -> u32 {
        self.0.borrow().loads
    }
    fn note(&self, what: &str) {
        self.0.borrow_mut().log.push(what.into());
    }
    fn log(&self) -> Vec<String> {
        self.0.borrow().log.clone()
    }
    fn json(&self) -> Value {
        serde_json::to_value(
            self.0
                .borrow()
                .journal
                .as_ref()
                .expect("a journal was saved"),
        )
        .unwrap()
    }
    fn has_journal(&self) -> bool {
        self.0.borrow().journal.is_some()
    }
    fn reservations(&self) -> usize {
        self.json()["reservations"].as_object().unwrap().len()
    }
    fn put_json(&self, v: Value) {
        let journal: AccountJournal = serde_json::from_value(v).expect("shape accepted by serde");
        self.0.borrow_mut().journal = Some(journal);
    }
    fn tamper(&self, f: impl FnOnce(&mut Value)) {
        let mut v = self.json();
        f(&mut v);
        self.put_json(v);
    }
}

impl AccountStorage for FakeStorage {
    async fn load(&mut self) -> Result<Option<AccountJournal>, StorageError> {
        let (fault, journal) = {
            let mut s = self.0.borrow_mut();
            s.loads += 1;
            (s.load_fault, s.journal.clone())
        };
        match fault {
            Fault::FailBefore | Fault::FailAfter => Err(StorageError),
            Fault::PendBefore | Fault::PendAfter => {
                pending::<()>().await;
                Err(StorageError)
            }
            Fault::None => Ok(journal),
        }
    }

    async fn save(&mut self, journal: &AccountJournal) -> Result<(), StorageError> {
        let fault = {
            let mut s = self.0.borrow_mut();
            s.save_attempts += 1;
            let n = serde_json::to_value(journal).unwrap()["reservations"]
                .as_object()
                .unwrap()
                .len();
            s.log.push(format!("save:{n}"));
            s.save_fault
        };
        match fault {
            Fault::FailBefore => return Err(StorageError),
            Fault::PendBefore => {
                pending::<()>().await;
                return Err(StorageError);
            }
            _ => {}
        }
        // Atomic replace of the whole journal (clone), never partial.
        self.0.borrow_mut().journal = Some(journal.clone());
        match fault {
            Fault::FailAfter => Err(StorageError),
            Fault::PendAfter => {
                pending::<()>().await;
                Err(StorageError)
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseMode {
    /// Ok and the confirmed flag becomes true.
    Ok,
    /// Ok but confirmed stays false.
    OkUnconfirmed,
    /// Err, confirmed stays false.
    Err,
    /// Err although the confirmed flag is true.
    ErrConfirmed,
    /// Never completes.
    Pending,
}

struct CloseState {
    mode: CloseMode,
    calls: u32,
    confirmed: bool,
}

/// Cloneable: the test keeps a clone to steer/inspect the supervisor's copy.
#[derive(Clone)]
struct FakeControl(Rc<RefCell<CloseState>>);

impl FakeControl {
    fn new(mode: CloseMode) -> Self {
        Self(Rc::new(RefCell::new(CloseState {
            mode,
            calls: 0,
            confirmed: false,
        })))
    }
    fn set_mode(&self, mode: CloseMode) {
        self.0.borrow_mut().mode = mode;
    }
    fn calls(&self) -> u32 {
        self.0.borrow().calls
    }
    fn confirmed(&self) -> bool {
        self.0.borrow().confirmed
    }
}

impl CloseControl for FakeControl {
    async fn close(&self) -> Result<(), CloseError> {
        let mode = {
            let mut s = self.0.borrow_mut();
            s.calls += 1;
            s.mode
        };
        match mode {
            CloseMode::Ok => {
                self.0.borrow_mut().confirmed = true;
                Ok(())
            }
            CloseMode::OkUnconfirmed => Ok(()),
            CloseMode::Err => Err(CloseError),
            CloseMode::ErrConfirmed => {
                self.0.borrow_mut().confirmed = true;
                Err(CloseError)
            }
            CloseMode::Pending => {
                pending::<()>().await;
                Err(CloseError)
            }
        }
    }

    fn confirmed_closed(&self) -> bool {
        self.0.borrow().confirmed
    }
}

/// Poll exactly once, then drop the future (cancellation). True if it was pending.
fn poll_then_drop<F: Future>(fut: F) -> bool {
    let mut fut = Box::pin(fut);
    let mut cx = Context::from_waker(Waker::noop());
    fut.as_mut().poll(&mut cx).is_pending()
}

/// Sets a flag when dropped; moved into setup/operation futures.
struct DropFlag(Rc<Cell<bool>>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

// -------------------------------------------------------------- helpers

type Sup = AccountSupervisor<FakeStorage, FakeControl>;

fn cfg(cap: u32) -> AccountConfig {
    AccountConfig {
        id: ACCOUNT.into(),
        revision: 1,
        secret_ref: SECRET_A.into(),
        can_read: true,
        max_connections: cap,
        stream_reserve: 1,
        ingest_limit: cap - 1,
        tier: 0,
        enabled: true,
    }
}

/// Settings-only successor: same secret, later revision.
fn rev(mut c: AccountConfig, revision: u64) -> AccountConfig {
    c.revision = revision;
    c
}

fn rotated(mut c: AccountConfig, revision: u64, secret: &str) -> AccountConfig {
    c.revision = revision;
    c.secret_ref = secret.into();
    c
}

async fn open(store: &FakeStorage, config: AccountConfig) -> Sup {
    Sup::open(store.clone(), config)
        .await
        .expect("open must succeed")
}

async fn open_err(store: &FakeStorage, config: AccountConfig) -> SupervisorError {
    match Sup::open(store.clone(), config).await {
        Ok(_) => panic!("open must be refused"),
        Err(e) => e,
    }
}

async fn reserve(sup: &mut Sup, now: u64) -> SessionId {
    sup.reserve(WorkClass::Stream, now).await.unwrap()
}

/// Run the factory with a fresh Ok-mode control; returns the test's clone.
fn begin(sup: &mut Sup, id: SessionId, now: u64) -> FakeControl {
    let ctl = FakeControl::new(CloseMode::Ok);
    let handed = ctl.clone();
    let _setup: Ready<()> = sup
        .start(id, u64::MAX, now, move || Ok::<_, ()>((handed, ready(()))))
        .unwrap();
    ctl
}

async fn started(sup: &mut Sup, now: u64) -> (SessionId, FakeControl) {
    let id = reserve(sup, now).await;
    let ctl = begin(sup, id, now);
    (id, ctl)
}

fn exhausted() -> Result<SessionId, SupervisorError> {
    Err(SupervisorError::Unavailable(PoolError::Exhausted))
}

fn is_quarantined(r: Result<SessionId, SupervisorError>) -> bool {
    matches!(
        r,
        Err(SupervisorError::Unavailable(PoolError::Quarantined { .. }))
    )
}

// ---------------------------------------------------- persist ordering

#[tokio::test]
async fn reservation_is_durable_before_the_factory_runs() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    let id = reserve(&mut sup, 0).await;
    // The handle exists only after the save completed.
    assert_eq!(store.reservations(), 1);
    let probe = store.clone();
    let ctl = FakeControl::new(CloseMode::Ok);
    let handed = ctl.clone();
    let _setup = sup
        .start(id, u64::MAX, 0, move || {
            // At factory time the journal on "disk" already holds the reservation.
            assert_eq!(probe.reservations(), 1);
            probe.note("factory");
            Ok::<_, ()>((handed, ready(())))
        })
        .unwrap();
    assert_eq!(store.log(), ["save:0", "save:1", "factory"]);
}

#[tokio::test]
async fn a_failed_reserve_save_never_yields_a_handle_and_fences_the_instance() {
    for fault in [Fault::FailBefore, Fault::FailAfter] {
        let store = FakeStorage::new();
        let mut sup = open(&store, cfg(3)).await;
        store.set_save_fault(fault);
        assert_eq!(
            sup.reserve(WorkClass::Stream, 0).await,
            Err(SupervisorError::Storage)
        );
        store.set_save_fault(Fault::None);
        // Uncertain write: the instance never recovers, and the lease stays held.
        assert_eq!(
            sup.reserve(WorkClass::Stream, 0).await,
            Err(SupervisorError::Fenced)
        );
        let c = sup.candidate(WorkClass::Stream);
        assert!(!c.eligible);
        assert_eq!(c.in_flight, 1);
        // Restart consults only storage: FailBefore left nothing, FailAfter left a reservation.
        drop(sup);
        let mut again = open(&store, cfg(3)).await;
        let expect_fenced = fault == Fault::FailAfter;
        assert_eq!(store.reservations(), usize::from(expect_fenced));
        assert_eq!(
            again.reserve(WorkClass::Stream, 0).await.is_err(),
            expect_fenced,
            "{fault:?}"
        );
    }
}

#[tokio::test]
async fn cancelling_the_reserve_save_fences_and_never_releases_capacity() {
    for fault in [Fault::PendBefore, Fault::PendAfter] {
        let store = FakeStorage::new();
        let mut sup = open(&store, cfg(3)).await;
        store.set_save_fault(fault);
        assert!(poll_then_drop(sup.reserve(WorkClass::Stream, 0)));
        store.set_save_fault(Fault::None);
        assert_eq!(
            sup.reserve(WorkClass::Stream, 0).await,
            Err(SupervisorError::Fenced)
        );
        assert!(!sup.candidate(WorkClass::Stream).eligible);
        assert_eq!(sup.candidate(WorkClass::Stream).in_flight, 1);
        drop(sup);
        let expect_fenced = fault == Fault::PendAfter;
        let mut again = open(&store, cfg(3)).await;
        assert_eq!(
            again.reserve(WorkClass::Stream, 0).await.is_err(),
            expect_fenced,
            "{fault:?}"
        );
    }
}

#[tokio::test]
async fn open_creates_and_persists_a_fresh_journal() {
    let store = FakeStorage::new();
    assert!(!store.has_journal());
    let sup = open(&store, cfg(2)).await;
    assert!(store.has_journal());
    assert_eq!(store.reservations(), 0);
    assert!(sup.candidate(WorkClass::Stream).eligible);
    assert_eq!(sup.candidate(WorkClass::Stream).id, ACCOUNT);
}

#[tokio::test]
async fn storage_failures_at_open_are_refused() {
    let store = FakeStorage::new();
    store.set_load_fault(Fault::FailBefore);
    assert_eq!(open_err(&store, cfg(2)).await, SupervisorError::Storage);
    store.set_load_fault(Fault::None);
    store.set_save_fault(Fault::FailBefore);
    assert_eq!(open_err(&store, cfg(2)).await, SupervisorError::Storage);
    assert!(!store.has_journal());
    // An invalid current snapshot is refused before storage is even read.
    let fresh = FakeStorage::new();
    let mut bad = cfg(2);
    bad.revision = 0;
    assert_eq!(
        open_err(&fresh, bad).await,
        SupervisorError::Config(ConfigError::Invalid)
    );
    assert_eq!(fresh.loads(), 0);
    assert_eq!(fresh.save_attempts(), 0);
}

// ------------------------------------------------------- caps / classes

#[tokio::test]
async fn caps_and_stream_reserve_hold_through_the_supervisor() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    let a = sup.reserve(WorkClass::Ingest, 0).await.unwrap();
    let b = sup.reserve(WorkClass::Ingest, 0).await.unwrap();
    assert_eq!(sup.reserve(WorkClass::Ingest, 0).await, exhausted());
    // Ingest may not borrow the stream reserve; stream still gets the last slot.
    let c = sup.reserve(WorkClass::Stream, 0).await.unwrap();
    assert_eq!(sup.reserve(WorkClass::Stream, 0).await, exhausted());
    let cand = sup.candidate(WorkClass::Stream);
    assert_eq!(cand.in_flight, 3);
    assert!(!cand.eligible);
    assert_eq!(store.reservations(), 3);
    // Closing releases exactly one slot at a time.
    for id in [a, b, c] {
        begin(&mut sup, id, 0);
    }
    sup.close(a).await.unwrap();
    assert_eq!(store.reservations(), 2);
    assert!(sup.reserve(WorkClass::Stream, 0).await.is_ok());
    assert_eq!(sup.reserve(WorkClass::Stream, 0).await, exhausted());
}

#[tokio::test]
async fn stream_sessions_leave_no_room_for_ingest() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    reserve(&mut sup, 0).await;
    reserve(&mut sup, 0).await;
    assert_eq!(sup.reserve(WorkClass::Ingest, 0).await, exhausted());
    assert!(!sup.candidate(WorkClass::Ingest).eligible);
    assert!(sup.candidate(WorkClass::Stream).eligible);
}

#[tokio::test]
async fn a_single_connection_account_serves_stream_and_refuses_ingest() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(1)).await;
    assert!(sup.candidate(WorkClass::Stream).eligible);
    assert!(!sup.candidate(WorkClass::Ingest).eligible);
    assert_eq!(sup.reserve(WorkClass::Ingest, 0).await, exhausted());
    let (id, ctl) = started(&mut sup, 0).await;
    assert_eq!(sup.reserve(WorkClass::Stream, 0).await, exhausted());
    sup.record_result(id, SessionResult::Success).await.unwrap();
    sup.close(id).await.unwrap();
    assert!(ctl.confirmed());
    // Reusable afterwards, with a fresh (never reused) id.
    let (id2, _) = started(&mut sup, 1).await;
    assert_eq!(sup.reserve(WorkClass::Stream, 0).await, exhausted());
    sup.close(id2).await.unwrap();
}

#[tokio::test]
async fn reserve_refuses_time_overflow_without_taking_capacity() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(1)).await;
    assert_eq!(
        sup.reserve(WorkClass::Stream, u64::MAX).await,
        Err(SupervisorError::Refused)
    );
    assert_eq!(store.reservations(), 0);
    assert!(sup.reserve(WorkClass::Stream, 0).await.is_ok());
}

// ------------------------------------------- future drop / expiry holds

#[tokio::test]
async fn dropping_the_setup_future_or_the_handle_holds_capacity() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(1)).await;
    // A bare handle is not a guard: dropping it releases nothing.
    let _ = reserve(&mut sup, 0).await;
    assert_eq!(sup.reserve(WorkClass::Stream, 0).await, exhausted());

    // A started setup future that is polled and then dropped holds it too.
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(1)).await;
    let id = reserve(&mut sup, 0).await;
    let dropped = Rc::new(Cell::new(false));
    let flag = DropFlag(dropped.clone());
    let ctl = FakeControl::new(CloseMode::Ok);
    let handed = ctl.clone();
    let setup = sup
        .start(id, u64::MAX, 0, move || {
            Ok::<_, ()>((handed, async move {
                let _flag = flag;
                pending::<()>().await;
            }))
        })
        .unwrap();
    assert!(poll_then_drop(setup));
    assert!(dropped.get(), "the operation future really was dropped");
    assert_eq!(ctl.calls(), 0, "dropping never closes");
    assert_eq!(sup.reserve(WorkClass::Stream, 0).await, exhausted());
    assert_eq!(sup.candidate(WorkClass::Stream).in_flight, 1);
    assert_eq!(store.reservations(), 1);
    // Only an explicit confirmed close frees it.
    sup.close(id).await.unwrap();
    assert!(sup.reserve(WorkClass::Stream, 0).await.is_ok());
}

#[tokio::test]
async fn session_deadline_expiry_does_not_release_capacity() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(1)).await;
    let id = reserve(&mut sup, 1_000).await;
    let dropped = Rc::new(Cell::new(false));
    let flag = DropFlag(dropped.clone());
    let ctl = FakeControl::new(CloseMode::Ok);
    let handed = ctl.clone();
    let setup = sup
        .start(id, u64::MAX, 1_000, move || {
            Ok::<_, ()>((handed, async move {
                let _flag = flag;
                pending::<()>().await;
            }))
        })
        .unwrap();
    assert_eq!(
        with_session_deadline(setup, ready(())).await,
        Err(SessionExpired)
    );
    assert!(dropped.get());
    // Wall clock far beyond the TTL, plus pool clock movement: still held.
    sup.advance(10 * SESSION_TTL_MS);
    let late = 1_000 + 10 * SESSION_TTL_MS;
    assert_eq!(sup.reserve(WorkClass::Stream, late).await, exhausted());
    assert!(!sup.auth_permitted(id, u64::MAX, late));
    assert_eq!(sup.candidate(WorkClass::Stream).in_flight, 1);
    assert_eq!(store.reservations(), 1);
    assert_eq!(ctl.calls(), 0);
    sup.close(id).await.unwrap();
    assert!(sup.reserve(WorkClass::Stream, late).await.is_ok());
}

// -------------------------------------------------------------- close

#[tokio::test]
async fn close_needs_ok_and_confirmed_and_otherwise_keeps_everything_held() {
    for mode in [
        CloseMode::Err,
        CloseMode::ErrConfirmed,
        CloseMode::OkUnconfirmed,
    ] {
        let store = FakeStorage::new();
        let mut sup = open(&store, cfg(1)).await;
        let (id, ctl) = started(&mut sup, 0).await;
        ctl.set_mode(mode);
        let saves = store.save_attempts();
        assert_eq!(
            sup.close(id).await,
            Err(SupervisorError::CloseUnconfirmed),
            "{mode:?}"
        );
        assert_eq!(ctl.calls(), 1);
        assert_eq!(
            store.save_attempts(),
            saves,
            "no journal write without confirmation"
        );
        assert_eq!(store.reservations(), 1);
        assert_eq!(sup.reserve(WorkClass::Stream, 0).await, exhausted());
        // The control is retained: it can simply be retried.
        ctl.set_mode(CloseMode::Ok);
        sup.close(id).await.unwrap();
        assert_eq!(ctl.calls(), 2);
        assert_eq!(store.reservations(), 0);
        assert!(sup.reserve(WorkClass::Stream, 0).await.is_ok());
    }
}

#[tokio::test]
async fn close_before_start_or_after_factory_failure_has_no_control() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(2)).await;
    let unstarted = reserve(&mut sup, 0).await;
    assert_eq!(sup.close(unstarted).await, Err(SupervisorError::NotStarted));
    let broken = reserve(&mut sup, 0).await;
    let r: Result<Ready<()>, _> = sup.start(broken, u64::MAX, 0, || {
        Err::<(FakeControl, Ready<()>), ()>(())
    });
    assert_eq!(r.err(), Some(SupervisorError::Dial));
    assert_eq!(sup.close(broken).await, Err(SupervisorError::NotStarted));
    assert_eq!(sup.candidate(WorkClass::Stream).in_flight, 2);
}

#[tokio::test]
async fn cancelled_close_leaves_the_lease_held_and_retryable() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(1)).await;
    let (id, ctl) = started(&mut sup, 0).await;
    ctl.set_mode(CloseMode::Pending);
    assert!(poll_then_drop(sup.close(id)));
    assert_eq!(ctl.calls(), 1);
    assert!(!ctl.confirmed());
    assert_eq!(store.reservations(), 1);
    assert_eq!(sup.reserve(WorkClass::Stream, 0).await, exhausted());
    assert!(sup.candidate(WorkClass::Stream).in_flight == 1);
    // Not poisoned: a later close still works.
    ctl.set_mode(CloseMode::Ok);
    sup.close(id).await.unwrap();
    assert!(sup.reserve(WorkClass::Stream, 0).await.is_ok());
}

#[tokio::test]
async fn retire_save_failure_or_cancellation_after_confirmed_close_fences_but_never_releases() {
    for fault in [
        Fault::FailBefore,
        Fault::FailAfter,
        Fault::PendBefore,
        Fault::PendAfter,
    ] {
        let store = FakeStorage::new();
        let mut sup = open(&store, cfg(1)).await;
        let (id, ctl) = started(&mut sup, 0).await;
        store.set_save_fault(fault);
        match fault {
            Fault::FailBefore | Fault::FailAfter => {
                assert_eq!(sup.close(id).await, Err(SupervisorError::Storage));
            }
            _ => assert!(poll_then_drop(sup.close(id)), "{fault:?}"),
        }
        store.set_save_fault(Fault::None);
        assert!(ctl.confirmed());
        // Poisoned: no admission, no mutation, capacity not freed.
        assert_eq!(
            sup.reserve(WorkClass::Stream, 0).await,
            Err(SupervisorError::Fenced)
        );
        assert!(!sup.candidate(WorkClass::Stream).eligible);
        // The retained control can still be driven, but release never happens.
        assert_eq!(sup.close(id).await, Err(SupervisorError::Fenced));
        assert_eq!(ctl.calls(), 2);
        assert_eq!(
            sup.reserve(WorkClass::Stream, 0).await,
            Err(SupervisorError::Fenced)
        );
        // Restart sees exactly what is durable: *After wrote the retirement
        // (confirmed close preceded it), *Before did not.
        drop(sup);
        let mut again = open(&store, cfg(1)).await;
        let durable_retired = matches!(fault, Fault::FailAfter | Fault::PendAfter);
        assert_eq!(
            store.reservations(),
            usize::from(!durable_retired),
            "{fault:?}"
        );
        assert_eq!(
            again.reserve(WorkClass::Stream, 0).await.is_ok(),
            durable_retired,
            "{fault:?}"
        );
    }
}

#[tokio::test]
async fn cancel_unstarted_retires_only_never_started_reservations() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(1)).await;
    let id = reserve(&mut sup, 0).await;
    sup.cancel_unstarted(id).await.unwrap();
    assert_eq!(store.reservations(), 0);
    assert_eq!(
        sup.cancel_unstarted(id).await,
        Err(SupervisorError::UnknownSession)
    );

    let (id2, ctl) = started(&mut sup, 0).await;
    assert_eq!(
        sup.cancel_unstarted(id2).await,
        Err(SupervisorError::AlreadyStarted)
    );
    assert_eq!(store.reservations(), 1);
    sup.close(id2).await.unwrap();
    assert!(ctl.confirmed());
}

#[tokio::test]
async fn cancel_unstarted_save_failure_and_cancellation_fence() {
    for fault in [Fault::FailBefore, Fault::PendBefore, Fault::PendAfter] {
        let store = FakeStorage::new();
        let mut sup = open(&store, cfg(1)).await;
        let id = reserve(&mut sup, 0).await;
        store.set_save_fault(fault);
        if fault == Fault::FailBefore {
            assert_eq!(
                sup.cancel_unstarted(id).await,
                Err(SupervisorError::Storage)
            );
        } else {
            assert!(poll_then_drop(sup.cancel_unstarted(id)));
        }
        store.set_save_fault(Fault::None);
        assert_eq!(
            sup.reserve(WorkClass::Stream, 0).await,
            Err(SupervisorError::Fenced)
        );
        assert_eq!(sup.cancel_unstarted(id).await, Err(SupervisorError::Fenced));
        drop(sup);
        let mut again = open(&store, cfg(1)).await;
        assert_eq!(
            again.reserve(WorkClass::Stream, 0).await.is_ok(),
            fault == Fault::PendAfter
        );
    }
}

// ------------------------------------------- storage fencing / poisoning

#[tokio::test]
async fn storage_uncertainty_fences_all_mutation_but_retained_controls_can_still_close() {
    for fault in [
        Fault::FailBefore,
        Fault::FailAfter,
        Fault::PendBefore,
        Fault::PendAfter,
    ] {
        let store = FakeStorage::new();
        let mut sup = open(&store, cfg(2)).await;
        let (id, ctl) = started(&mut sup, 0).await;
        let (other, other_ctl) = started(&mut sup, 0).await;

        store.set_save_fault(fault);
        let next = rev(cfg(2), 2);
        match fault {
            Fault::FailBefore | Fault::FailAfter => {
                assert_eq!(sup.reconcile(next).await, Err(SupervisorError::Storage));
            }
            _ => assert!(poll_then_drop(sup.reconcile(next))),
        }
        store.set_save_fault(Fault::None);

        assert_eq!(
            sup.reserve(WorkClass::Stream, 0).await,
            Err(SupervisorError::Fenced)
        );
        assert_eq!(
            sup.reconcile(rev(cfg(2), 3)).await,
            Err(SupervisorError::Fenced)
        );
        assert_eq!(
            sup.record_result(id, SessionResult::Success).await,
            Err(SupervisorError::Fenced)
        );
        assert_eq!(sup.cancel_unstarted(id).await, Err(SupervisorError::Fenced));
        assert!(!sup.auth_permitted(id, u64::MAX, 0));
        let c = sup.candidate(WorkClass::Stream);
        assert!(!c.eligible);
        assert_eq!(c.in_flight, 2);
        let refused = sup.start(other, u64::MAX, 0, || {
            Ok::<_, ()>((other_ctl.clone(), ready(())))
        });
        assert_eq!(refused.err(), Some(SupervisorError::Refused));

        // Physical close still runs; the release does not.
        assert_eq!(sup.close(id).await, Err(SupervisorError::Fenced));
        assert_eq!(ctl.calls(), 1);
        assert!(ctl.confirmed());
        assert_eq!(sup.candidate(WorkClass::Stream).in_flight, 2);
        assert_eq!(
            sup.reserve(WorkClass::Stream, 0).await,
            Err(SupervisorError::Fenced)
        );

        // Whatever was durable, restart still fences while a reservation remains.
        drop(sup);
        let mut again = open(&store, rev(cfg(2), 2)).await;
        assert_eq!(store.reservations(), 2);
        assert_eq!(
            again.reserve(WorkClass::Stream, 0).await,
            Err(SupervisorError::Fenced)
        );
    }
}

// -------------------------------------------------------- restart fence

#[tokio::test]
async fn outstanding_journal_fences_restart_despite_ttl_rotation_and_settings_changes() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    let (_id, _ctl) = started(&mut sup, 0).await;
    drop(sup); // eviction: socket fate unknown

    // Same config, and a time long after the TTL.
    let mut a = open(&store, cfg(3)).await;
    assert_eq!(
        a.reserve(WorkClass::Stream, 100 * SESSION_TTL_MS).await,
        Err(SupervisorError::Fenced)
    );
    let cand = a.candidate(WorkClass::Stream);
    assert!(!cand.eligible);
    assert_eq!(cand.in_flight, 1);
    assert_eq!(
        a.reserve(WorkClass::Ingest, 100 * SESSION_TTL_MS).await,
        Err(SupervisorError::Fenced)
    );
    drop(a);

    // Credential rotation at restart is not closure evidence either.
    let mut b = open(&store, rotated(cfg(3), 2, SECRET_B)).await;
    assert_eq!(
        b.reserve(WorkClass::Stream, u64::MAX / 2).await,
        Err(SupervisorError::Fenced)
    );
    // Later snapshots are accepted but never lift the fence.
    b.reconcile(rev(rotated(cfg(3), 2, SECRET_B), 3))
        .await
        .unwrap();
    b.reconcile(rotated(cfg(5), 4, SECRET_C)).await.unwrap();
    b.advance(1_000 * SESSION_TTL_MS);
    assert_eq!(
        b.reserve(WorkClass::Stream, 1 << 40).await,
        Err(SupervisorError::Fenced)
    );
    assert!(!b.candidate(WorkClass::Stream).eligible);
    assert_eq!(
        store.reservations(),
        1,
        "nothing retired the recovered reservation"
    );
    drop(b);

    // And a third restart still fences.
    let mut c = open(&store, rotated(cfg(5), 4, SECRET_C)).await;
    assert_eq!(
        c.reserve(WorkClass::Stream, 0).await,
        Err(SupervisorError::Fenced)
    );
}

#[tokio::test]
async fn any_single_outstanding_reservation_fences_restart_even_if_others_closed() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    let (a, _) = started(&mut sup, 0).await;
    let _unstarted = reserve(&mut sup, 0).await;
    sup.close(a).await.unwrap();
    assert_eq!(store.reservations(), 1);
    drop(sup);
    let mut again = open(&store, cfg(3)).await;
    assert_eq!(
        again.reserve(WorkClass::Stream, 0).await,
        Err(SupervisorError::Fenced)
    );
}

#[tokio::test]
async fn clean_close_restarts_cleanly_with_monotonic_ids() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(2)).await;
    let (first, _) = started(&mut sup, 0).await;
    sup.close(first).await.unwrap();
    drop(sup);
    let mut again = open(&store, cfg(2)).await;
    assert!(again.candidate(WorkClass::Stream).eligible);
    let second = reserve(&mut again, 0).await;
    assert_ne!(
        first, second,
        "durable sequence prevents id reuse after restart"
    );
    // A stale handle from before the restart is not a session of this instance.
    assert_eq!(
        again.close(first).await,
        Err(SupervisorError::UnknownSession)
    );
    assert_eq!(
        again.record_result(first, SessionResult::AuthFailure).await,
        Err(SupervisorError::UnknownSession)
    );
    assert!(!again.auth_permitted(first, u64::MAX, 0));
}

#[tokio::test]
async fn retired_handles_are_dead_and_before_start_results_are_refused() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(2)).await;
    let id = reserve(&mut sup, 0).await;
    assert_eq!(
        sup.record_result(id, SessionResult::Success).await,
        Err(SupervisorError::NotStarted)
    );
    begin(&mut sup, id, 0);
    sup.close(id).await.unwrap();
    assert_eq!(sup.close(id).await, Err(SupervisorError::UnknownSession));
    assert_eq!(
        sup.record_result(id, SessionResult::ProviderFailure).await,
        Err(SupervisorError::UnknownSession)
    );
    assert_eq!(
        sup.cancel_unstarted(id).await,
        Err(SupervisorError::UnknownSession)
    );
    assert!(!sup.auth_permitted(id, u64::MAX, 0));
    let r = sup.start(id, u64::MAX, 0, || {
        Ok::<_, ()>((FakeControl::new(CloseMode::Ok), ready(())))
    });
    assert!(r.is_err());
}

// ---------------------------------------------------------- quarantine

fn quarantined_secret(store: &FakeStorage) -> Value {
    store.json()["quarantined_secret"].clone()
}

#[tokio::test]
async fn auth_failure_quarantine_survives_clean_close_restart_and_settings_only_updates() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(2)).await;
    let (id, _ctl) = started(&mut sup, 0).await;
    sup.record_result(id, SessionResult::AuthFailure)
        .await
        .unwrap();
    assert_eq!(quarantined_secret(&store), json!(SECRET_A));
    assert!(!sup.candidate(WorkClass::Stream).eligible);
    assert!(is_quarantined(sup.reserve(WorkClass::Stream, 0).await));
    assert!(!sup.auth_permitted(id, u64::MAX, 0));

    // A clean confirmed close does not clear it, nor does waiting or an identical snapshot.
    sup.close(id).await.unwrap();
    sup.advance(86_400_000);
    sup.reconcile(cfg(2)).await.unwrap();
    assert_eq!(quarantined_secret(&store), json!(SECRET_A));
    assert!(is_quarantined(sup.reserve(WorkClass::Stream, 0).await));
    assert_eq!(store.reservations(), 0);

    // Restart (no outstanding reservation, so only the quarantine blocks).
    drop(sup);
    let mut sup = open(&store, cfg(2)).await;
    assert!(is_quarantined(
        sup.reserve(WorkClass::Stream, 1_000_000).await
    ));
    assert!(!sup.candidate(WorkClass::Stream).eligible);

    // Settings-only revisions (cap, tier, disable then re-enable) keep it, live and across restart.
    let mut s2 = rev(cfg(3), 2);
    s2.tier = 4;
    sup.reconcile(s2.clone()).await.unwrap();
    assert!(is_quarantined(sup.reserve(WorkClass::Stream, 0).await));
    let mut s3 = rev(s2.clone(), 3);
    s3.enabled = false;
    sup.reconcile(s3.clone()).await.unwrap();
    let s4 = rev(s2.clone(), 4);
    sup.reconcile(s4.clone()).await.unwrap();
    assert_eq!(quarantined_secret(&store), json!(SECRET_A));
    assert!(is_quarantined(sup.reserve(WorkClass::Stream, 0).await));
    drop(sup);
    let mut sup = open(&store, s4.clone()).await;
    assert!(is_quarantined(sup.reserve(WorkClass::Stream, 0).await));
    assert_eq!(quarantined_secret(&store), json!(SECRET_A));

    // Only a new secret clears it, durably, and admission resumes.
    sup.reconcile(rotated(s4, 5, SECRET_B)).await.unwrap();
    assert_eq!(quarantined_secret(&store), Value::Null);
    let (id, _) = started(&mut sup, 0).await;
    assert!(sup.auth_permitted(id, u64::MAX, 0));
    sup.close(id).await.unwrap();
}

#[tokio::test]
async fn rotation_while_offline_clears_the_quarantine_at_open() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(2)).await;
    let (id, _) = started(&mut sup, 0).await;
    sup.record_result(id, SessionResult::AuthFailure)
        .await
        .unwrap();
    sup.close(id).await.unwrap();
    drop(sup);
    let mut sup = open(&store, rotated(cfg(2), 2, SECRET_B)).await;
    assert_eq!(quarantined_secret(&store), Value::Null);
    assert!(sup.reserve(WorkClass::Stream, 0).await.is_ok());
}

#[tokio::test]
async fn old_credential_auth_failure_after_rotation_does_not_poison_the_new_secret() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    let (old, _old_ctl) = started(&mut sup, 0).await;
    sup.reconcile(rotated(cfg(3), 2, SECRET_B)).await.unwrap();
    sup.record_result(old, SessionResult::AuthFailure)
        .await
        .unwrap();
    assert_eq!(quarantined_secret(&store), Value::Null);
    assert!(sup.candidate(WorkClass::Stream).eligible);
    let (fresh, _) = started(&mut sup, 0).await;
    assert!(sup.auth_permitted(fresh, u64::MAX, 0));
    assert!(
        !sup.auth_permitted(old, u64::MAX, 0),
        "old credential may not AUTH again"
    );
    sup.close(old).await.unwrap();
    sup.close(fresh).await.unwrap();
    drop(sup);
    // Nothing quarantined survives a restart either.
    let mut sup = open(&store, rotated(cfg(3), 2, SECRET_B)).await;
    assert!(sup.reserve(WorkClass::Stream, 0).await.is_ok());
}

#[tokio::test]
async fn same_secret_auth_failure_after_a_settings_revision_still_quarantines() {
    let store = FakeStorage::new();
    let mut sup = open(&store, rotated(cfg(3), 1, SECRET_A)).await;
    let (id, _) = started(&mut sup, 0).await;
    let (other, _) = started(&mut sup, 0).await;
    sup.reconcile(rev(cfg(3), 2)).await.unwrap(); // settings only
                                                  // Revision changed, so no further AUTH is allowed for the old session...
    assert!(!sup.auth_permitted(id, u64::MAX, 0));
    // ...but its failure is the same identity and must be remembered.
    sup.record_result(id, SessionResult::AuthFailure)
        .await
        .unwrap();
    assert_eq!(quarantined_secret(&store), json!(SECRET_A));
    assert!(is_quarantined(sup.reserve(WorkClass::Stream, 0).await));
    sup.close(id).await.unwrap();
    sup.close(other).await.unwrap();
    assert!(is_quarantined(sup.reserve(WorkClass::Stream, 0).await));
}

#[tokio::test]
async fn auth_failure_persist_failure_fences_and_restart_stays_fenced() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(2)).await;
    let (id, _ctl) = started(&mut sup, 0).await;
    store.set_save_fault(Fault::FailBefore);
    assert_eq!(
        sup.record_result(id, SessionResult::AuthFailure).await,
        Err(SupervisorError::Storage)
    );
    store.set_save_fault(Fault::None);
    // In memory the quarantine is already applied, and the instance is fenced.
    assert!(!sup.candidate(WorkClass::Stream).eligible);
    assert_eq!(
        sup.reserve(WorkClass::Stream, 0).await,
        Err(SupervisorError::Fenced)
    );
    drop(sup);
    // The failed write lost the quarantine, but the live reservation keeps the account fenced.
    let mut again = open(&store, cfg(2)).await;
    assert_eq!(
        again.reserve(WorkClass::Stream, 0).await,
        Err(SupervisorError::Fenced)
    );
}

#[tokio::test]
async fn success_only_clears_the_failure_streak_when_no_newer_failure_happened() {
    // Control: a success from a session started AFTER the failures clears the streak.
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    let (s1, _) = started(&mut sup, 0).await;
    let (s2, _) = started(&mut sup, 0).await;
    sup.record_result(s1, SessionResult::ProviderFailure)
        .await
        .unwrap();
    sup.record_result(s2, SessionResult::ProviderFailure)
        .await
        .unwrap();
    let (s3, _) = started(&mut sup, 0).await;
    sup.record_result(s3, SessionResult::Success).await.unwrap();
    for id in [s1, s2, s3] {
        sup.close(id).await.unwrap();
    }
    let (s4, _) = started(&mut sup, 0).await;
    sup.record_result(s4, SessionResult::ProviderFailure)
        .await
        .unwrap();
    sup.close(s4).await.unwrap();
    assert!(
        sup.reserve(WorkClass::Stream, 0).await.is_ok(),
        "streak was reset to 1"
    );
}

#[tokio::test]
async fn stale_success_cannot_erase_newer_provider_failures() {
    // s3 began before s1/s2 failed, so its success must not clear their streak.
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    let (s1, _) = started(&mut sup, 0).await;
    let (s2, _) = started(&mut sup, 0).await;
    let (s3, _) = started(&mut sup, 0).await;
    sup.record_result(s1, SessionResult::ProviderFailure)
        .await
        .unwrap();
    sup.record_result(s2, SessionResult::ProviderFailure)
        .await
        .unwrap();
    sup.record_result(s3, SessionResult::Success).await.unwrap();
    for id in [s1, s2, s3] {
        sup.close(id).await.unwrap();
    }
    let (s4, _) = started(&mut sup, 0).await;
    sup.record_result(s4, SessionResult::ProviderFailure)
        .await
        .unwrap();
    assert!(
        !sup.candidate(WorkClass::Stream).eligible,
        "third failure opened the circuit"
    );
    sup.close(s4).await.unwrap();
    assert!(is_quarantined(sup.reserve(WorkClass::Stream, 0).await));
    // Half-open after the open interval admits exactly one probe.
    sup.advance(30_000);
    let probe = reserve(&mut sup, 0).await;
    assert!(is_quarantined(sup.reserve(WorkClass::Stream, 0).await));
    begin(&mut sup, probe, 0);
    sup.close(probe).await.unwrap();
}

#[tokio::test]
async fn success_from_an_older_revision_is_ignored_too() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    let (s1, _) = started(&mut sup, 0).await;
    let (s2, _) = started(&mut sup, 0).await;
    let (s3, _) = started(&mut sup, 0).await;
    sup.record_result(s1, SessionResult::ProviderFailure)
        .await
        .unwrap();
    sup.record_result(s2, SessionResult::ProviderFailure)
        .await
        .unwrap();
    sup.reconcile(rev(cfg(3), 2)).await.unwrap();
    sup.record_result(s3, SessionResult::Success).await.unwrap();
    for id in [s1, s2, s3] {
        sup.close(id).await.unwrap();
    }
    let (s4, _) = started(&mut sup, 0).await;
    sup.record_result(s4, SessionResult::ProviderFailure)
        .await
        .unwrap();
    sup.close(s4).await.unwrap();
    assert!(is_quarantined(sup.reserve(WorkClass::Stream, 0).await));
}

#[tokio::test]
async fn provider_failure_from_a_pre_rotation_session_is_ignored() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    let mut old = Vec::new();
    for _ in 0..3 {
        old.push(started(&mut sup, 0).await.0);
    }
    sup.reconcile(rotated(cfg(3), 2, SECRET_B)).await.unwrap();
    for &id in &old {
        sup.record_result(id, SessionResult::ProviderFailure)
            .await
            .unwrap();
    }
    for id in old {
        sup.close(id).await.unwrap();
    }
    assert!(
        sup.reserve(WorkClass::Stream, 0).await.is_ok(),
        "old leases cannot trip the new circuit"
    );
}

// ----------------------------------------------- cross-account tokens

#[tokio::test]
async fn a_session_token_from_another_account_is_rejected_everywhere() {
    let store_a = FakeStorage::new();
    let mut a = open(&store_a, cfg(2)).await;
    let store_b = FakeStorage::new();
    let mut other_cfg = cfg(2);
    other_cfg.id = OTHER_ACCOUNT.into();
    let mut b = open(&store_b, other_cfg).await;

    // Both owners hand out the same durable sequence number; only the account differs.
    let (ida, ctl_a) = started(&mut a, 0).await;
    let (idb, ctl_b) = started(&mut b, 0).await;
    assert_ne!(ida, idb);

    assert!(!a.auth_permitted(idb, u64::MAX, 0));
    assert!(!b.auth_permitted(ida, u64::MAX, 0));
    let r = a.start(idb, u64::MAX, 0, || {
        Ok::<_, ()>((FakeControl::new(CloseMode::Ok), ready(())))
    });
    assert_eq!(r.err(), Some(SupervisorError::Refused));
    assert_eq!(a.close(idb).await, Err(SupervisorError::UnknownSession));
    assert_eq!(
        a.record_result(idb, SessionResult::AuthFailure).await,
        Err(SupervisorError::UnknownSession)
    );
    assert_eq!(
        a.cancel_unstarted(idb).await,
        Err(SupervisorError::UnknownSession)
    );
    assert_eq!(
        ctl_b.calls(),
        0,
        "a foreign token never drives anyone's control"
    );
    assert_eq!(quarantined_secret(&store_a), Value::Null);
    assert_eq!(store_a.reservations(), 1);
    assert_eq!(store_b.reservations(), 1);
    assert!(a.candidate(WorkClass::Stream).eligible);

    // The right owners still work.
    a.close(ida).await.unwrap();
    b.close(idb).await.unwrap();
    assert_eq!(ctl_a.calls(), 1);
    assert_eq!(ctl_b.calls(), 1);
}

// -------------------------------------------- config refusal / lifecycle

#[tokio::test]
async fn stale_conflicting_foreign_and_invalid_snapshots_are_refused_without_effect() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    sup.reconcile(rev(cfg(3), 2)).await.unwrap();
    let saves = store.save_attempts();

    assert_eq!(
        sup.reconcile(cfg(3)).await,
        Err(SupervisorError::Config(ConfigError::Stale))
    );
    let mut conflict = rev(cfg(3), 2);
    conflict.tier = 9;
    assert_eq!(
        sup.reconcile(conflict).await,
        Err(SupervisorError::Config(ConfigError::Conflict))
    );
    let mut foreign = rev(cfg(3), 3);
    foreign.id = OTHER_ACCOUNT.into();
    assert_eq!(
        sup.reconcile(foreign).await,
        Err(SupervisorError::Config(ConfigError::Conflict))
    );
    let mut invalid = rev(cfg(3), 3);
    invalid.max_connections = 0;
    assert_eq!(
        sup.reconcile(invalid).await,
        Err(SupervisorError::Config(ConfigError::Invalid))
    );
    let mut bad_secret = rev(cfg(3), 3);
    bad_secret.secret_ref = "nope".into();
    assert_eq!(
        sup.reconcile(bad_secret).await,
        Err(SupervisorError::Config(ConfigError::Invalid))
    );
    assert_eq!(store.save_attempts(), saves, "refusals write nothing");
    // And they do not poison the instance.
    assert!(sup.candidate(WorkClass::Stream).eligible);
    assert!(sup.reserve(WorkClass::Stream, 0).await.is_ok());
    // An identical snapshot is idempotent.
    sup.reconcile(rev(cfg(3), 2)).await.unwrap();
}

#[tokio::test]
async fn open_refuses_stale_conflicting_or_foreign_current_against_the_journal() {
    let store = FakeStorage::new();
    drop(open(&store, rev(cfg(3), 5)).await);
    assert_eq!(
        open_err(&store, rev(cfg(3), 4)).await,
        SupervisorError::Config(ConfigError::Stale)
    );
    let mut conflict = rev(cfg(3), 5);
    conflict.tier = 1;
    assert_eq!(
        open_err(&store, conflict).await,
        SupervisorError::Config(ConfigError::Conflict)
    );
    let mut foreign = rev(cfg(3), 6);
    foreign.id = OTHER_ACCOUNT.into();
    assert_eq!(
        open_err(&store, foreign).await,
        SupervisorError::Config(ConfigError::Conflict)
    );
    assert_eq!(store.json()["config"]["revision"], json!(5));
}

#[tokio::test]
async fn disabling_or_removing_read_stops_admission_and_auth_but_not_closure() {
    for variant in 0..2 {
        let store = FakeStorage::new();
        let mut sup = open(&store, cfg(3)).await;
        let (id, ctl) = started(&mut sup, 0).await;
        assert!(sup.auth_permitted(id, u64::MAX, 0));
        let mut next = rev(cfg(3), 2);
        if variant == 0 {
            next.enabled = false;
        } else {
            next.can_read = false;
            next.ingest_limit = 0;
        }
        sup.reconcile(next.clone()).await.unwrap();
        assert!(!sup.candidate(WorkClass::Stream).eligible);
        assert_eq!(sup.reserve(WorkClass::Stream, 0).await, exhausted());
        assert_eq!(sup.reserve(WorkClass::Ingest, 0).await, exhausted());
        assert!(!sup.auth_permitted(id, u64::MAX, 0));
        // A late, stale "re-enable" is refused; only a later revision re-enables.
        assert_eq!(
            sup.reconcile(cfg(3)).await,
            Err(SupervisorError::Config(ConfigError::Stale))
        );
        assert!(!sup.candidate(WorkClass::Stream).eligible);
        // The held socket still gets a confirmed close and releases.
        sup.close(id).await.unwrap();
        assert!(ctl.confirmed());
        assert_eq!(store.reservations(), 0);
        assert!(!sup.candidate(WorkClass::Stream).eligible);
        sup.reconcile(rev(cfg(3), 3)).await.unwrap();
        assert!(sup.reserve(WorkClass::Stream, 0).await.is_ok());
    }
}

#[tokio::test]
async fn ingest_session_loses_auth_when_ingest_is_switched_off() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    let id = sup.reserve(WorkClass::Ingest, 0).await.unwrap();
    let stream = reserve(&mut sup, 0).await;
    let mut next = rev(cfg(3), 1);
    next.revision = 2;
    next.ingest_limit = 0;
    sup.reconcile(next).await.unwrap();
    // Revision moved on, so neither may AUTH; ingest additionally has no budget.
    assert!(!sup.auth_permitted(id, u64::MAX, 0));
    assert!(!sup.auth_permitted(stream, u64::MAX, 0));
}

// ---------------------------------------------------------- auth guard

#[tokio::test]
async fn auth_guard_is_checked_before_each_auth_against_proof_and_session_expiry() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(2)).await;
    let id = reserve(&mut sup, 1_000).await; // session expires at 1_000 + TTL
    let end = 1_000 + SESSION_TTL_MS;
    // AUTHINFO USER, then AUTHINFO PASS, each re-checked at its own instant.
    assert!(sup.auth_permitted(id, 5_000, 1_500));
    assert!(sup.auth_permitted(id, 5_000, 4_999));
    assert!(
        !sup.auth_permitted(id, 5_000, 5_000),
        "proof expiry is exclusive"
    );
    assert!(!sup.auth_permitted(id, 5_000, 9_000));
    assert!(sup.auth_permitted(id, u64::MAX, end - 1));
    assert!(
        !sup.auth_permitted(id, u64::MAX, end),
        "session TTL is exclusive"
    );
    assert!(!sup.auth_permitted(id, u64::MAX, end + 1));
    // An already-expired proof refuses start without invoking the factory or marking it attempted.
    let invoked = Cell::new(0u32);
    let r = sup.start(id, 1_000, 1_000, || {
        invoked.set(invoked.get() + 1);
        Ok::<_, ()>((FakeControl::new(CloseMode::Ok), ready(())))
    });
    assert_eq!(r.err(), Some(SupervisorError::Refused));
    assert_eq!(invoked.get(), 0);
    let r = sup.start(id, u64::MAX, end, || {
        invoked.set(invoked.get() + 1);
        Ok::<_, ()>((FakeControl::new(CloseMode::Ok), ready(())))
    });
    assert_eq!(r.err(), Some(SupervisorError::Refused));
    assert_eq!(invoked.get(), 0);
    // Expiry never released anything, and an unattempted reservation can still be cancelled.
    assert_eq!(sup.reserve(WorkClass::Ingest, end + 1).await, exhausted());
    sup.cancel_unstarted(id).await.unwrap();
}

#[tokio::test]
async fn auth_is_refused_after_poison_rotation_quarantine_and_retirement() {
    // Poisoned instance.
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(2)).await;
    let (id, _) = started(&mut sup, 0).await;
    assert!(sup.auth_permitted(id, u64::MAX, 0));
    store.set_save_fault(Fault::FailBefore);
    let _ = sup.reconcile(rev(cfg(2), 2)).await;
    assert!(!sup.auth_permitted(id, u64::MAX, 0));

    // Rotated secret.
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(2)).await;
    let (id, _) = started(&mut sup, 0).await;
    sup.reconcile(rotated(cfg(2), 2, SECRET_B)).await.unwrap();
    assert!(!sup.auth_permitted(id, u64::MAX, 0));

    // Quarantine from a sibling session.
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(2)).await;
    let (a, _) = started(&mut sup, 0).await;
    let (b, _) = started(&mut sup, 0).await;
    sup.record_result(a, SessionResult::AuthFailure)
        .await
        .unwrap();
    assert!(!sup.auth_permitted(b, u64::MAX, 0));
}

// -------------------------------------------------- dial: no retry

#[tokio::test]
async fn factory_runs_at_most_once_and_a_dial_failure_stays_held_without_retry() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(1)).await;
    let id = reserve(&mut sup, 0).await;
    let calls = Cell::new(0u32);
    let first = sup.start(id, u64::MAX, 0, || {
        calls.set(calls.get() + 1);
        Err::<(FakeControl, Ready<()>), ()>(())
    });
    assert_eq!(first.err(), Some(SupervisorError::Dial));
    let second = sup.start(id, u64::MAX, 0, || {
        calls.set(calls.get() + 1);
        Ok::<_, ()>((FakeControl::new(CloseMode::Ok), ready(())))
    });
    assert_eq!(second.err(), Some(SupervisorError::AlreadyStarted));
    assert_eq!(calls.get(), 1, "no retry");
    // Uncertain: may not be cancelled as never-dialed, nothing released, restart fences.
    assert_eq!(
        sup.cancel_unstarted(id).await,
        Err(SupervisorError::AlreadyStarted)
    );
    assert_eq!(sup.reserve(WorkClass::Stream, 0).await, exhausted());
    assert_eq!(store.reservations(), 1);
    // The failure is reportable and the account bookkeeping still works.
    sup.record_result(id, SessionResult::ProviderFailure)
        .await
        .unwrap();
    drop(sup);
    let mut again = open(&store, cfg(1)).await;
    assert_eq!(
        again.reserve(WorkClass::Stream, 0).await,
        Err(SupervisorError::Fenced)
    );
}

#[tokio::test]
async fn a_panicking_factory_is_treated_as_attempted_and_stays_held() {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(1)).await;
    let id = reserve(&mut sup, 0).await;
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let _ = sup.start(
            id,
            u64::MAX,
            0,
            || -> Result<(FakeControl, Ready<()>), ()> { panic!("dial exploded") },
        );
    }));
    assert!(outcome.is_err());
    let again = sup.start(id, u64::MAX, 0, || {
        Ok::<_, ()>((FakeControl::new(CloseMode::Ok), ready(())))
    });
    assert_eq!(again.err(), Some(SupervisorError::AlreadyStarted));
    assert_eq!(
        sup.cancel_unstarted(id).await,
        Err(SupervisorError::AlreadyStarted)
    );
    assert_eq!(sup.reserve(WorkClass::Stream, 0).await, exhausted());
}

// ---------------------------------------------- journal integrity

type Mutation = fn(&mut Value);

async fn journal_with_one_reservation() -> FakeStorage {
    let store = FakeStorage::new();
    let mut sup = open(&store, cfg(3)).await;
    let _ = started(&mut sup, 0).await;
    store
}

#[tokio::test]
async fn journal_holds_scheduling_identities_only() {
    let store = journal_with_one_reservation().await;
    let v = store.json();
    let keys = |v: &Value| -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(
        keys(&v),
        [
            "config",
            "next_id",
            "quarantined_secret",
            "reservations",
            "version"
        ]
    );
    assert_eq!(
        keys(&v["config"]),
        [
            "can_read",
            "enabled",
            "id",
            "ingest_limit",
            "max_connections",
            "revision",
            "secret_ref",
            "stream_reserve",
            "tier"
        ]
    );
    assert_eq!(
        keys(&v["reservations"]["1"]),
        ["class", "expires_at_ms", "revision", "secret_ref"]
    );
    assert_eq!(
        v["reservations"]["1"]["expires_at_ms"],
        json!(SESSION_TTL_MS)
    );
    assert_eq!(v["next_id"], json!(2));
    let text = v.to_string();
    for banned in ["password", "username", "host", "token"] {
        assert!(!text.contains(banned), "{banned}");
    }
}

#[tokio::test]
async fn structurally_corrupt_journals_are_refused_without_any_write() {
    let cases: Vec<(&str, Mutation)> = vec![
        ("version", |v| v["version"] = json!(2)),
        ("version0", |v| v["version"] = json!(0)),
        ("next_id zero", |v| v["next_id"] = json!(0)),
        ("reservation id not below next_id", |v| {
            v["next_id"] = json!(1)
        }),
        ("reservation id zero", |v| {
            let r = v["reservations"]
                .as_object_mut()
                .unwrap()
                .remove("1")
                .unwrap();
            v["reservations"]["0"] = r;
        }),
        ("reservation revision zero", |v| {
            v["reservations"]["1"]["revision"] = json!(0)
        }),
        ("reservation revision from the future", |v| {
            v["reservations"]["1"]["revision"] = json!(9)
        }),
        ("reservation secret garbage", |v| {
            v["reservations"]["1"]["secret_ref"] = json!("not-a-uuid")
        }),
        ("reservation secret uppercase", |v| {
            v["reservations"]["1"]["secret_ref"] = json!(SECRET_A.to_uppercase())
        }),
        ("expiry below ttl", |v| {
            v["reservations"]["1"]["expires_at_ms"] = json!(SESSION_TTL_MS - 1)
        }),
        ("expiry zero", |v| {
            v["reservations"]["1"]["expires_at_ms"] = json!(0)
        }),
        ("quarantine for a foreign secret", |v| {
            v["quarantined_secret"] = json!(SECRET_B)
        }),
        ("config revision zero", |v| {
            v["config"]["revision"] = json!(0)
        }),
        ("config no connections", |v| {
            v["config"]["max_connections"] = json!(0)
        }),
        ("config cap above hard max", |v| {
            v["config"]["max_connections"] = json!(HARD_MAX_CONNS + 1)
        }),
        ("config no stream reserve", |v| {
            v["config"]["stream_reserve"] = json!(0)
        }),
        ("config bad account id", |v| v["config"]["id"] = json!("x")),
        ("config bad secret", |v| {
            v["config"]["secret_ref"] = json!("x")
        }),
        ("too many reservations", |v| {
            let r = v["reservations"]["1"].clone();
            v["next_id"] = json!(HARD_MAX_CONNS + 10);
            for i in 2..=(HARD_MAX_CONNS + 1) {
                v["reservations"][i.to_string()] = r.clone();
            }
        }),
    ];
    for (name, mutate) in cases {
        let store = journal_with_one_reservation().await;
        store.tamper(mutate);
        let (loads, saves) = (store.loads(), store.save_attempts());
        let before = store.json();
        assert_eq!(
            open_err(&store, cfg(3)).await,
            SupervisorError::Corrupt,
            "{name}"
        );
        assert_eq!(store.loads(), loads + 1);
        assert_eq!(
            store.save_attempts(),
            saves,
            "{name}: a corrupt journal is never rewritten"
        );
        assert_eq!(store.json(), before, "{name}");
    }
}

#[tokio::test]
async fn shape_violations_cannot_even_deserialize_into_a_journal() {
    let store = journal_with_one_reservation().await;
    let base = store.json();
    let mutations: Vec<(&str, Mutation)> = vec![
        (
            "unknown top-level field",
            (|v| v["resolved_host"] = json!("203.0.113.1")),
        ),
        (
            "unknown config field",
            (|v| v["config"]["password"] = json!("x")),
        ),
        (
            "unknown reservation field",
            (|v| v["reservations"]["1"]["socket"] = json!(7)),
        ),
        (
            "missing field",
            (|v| drop(v.as_object_mut().unwrap().remove("next_id"))),
        ),
        (
            "missing quarantine marker",
            (|v| drop(v.as_object_mut().unwrap().remove("quarantined_secret"))),
        ),
        (
            "bad class",
            (|v| v["reservations"]["1"]["class"] = json!("prefetch")),
        ),
        (
            "negative revision",
            (|v| v["config"]["revision"] = json!(-1)),
        ),
        (
            "non numeric id key",
            (|v| {
                let r = v["reservations"]
                    .as_object_mut()
                    .unwrap()
                    .remove("1")
                    .unwrap();
                v["reservations"]["one"] = r;
            }),
        ),
    ];
    for (name, mutate) in mutations {
        let mut v = base.clone();
        mutate(&mut v);
        assert!(
            serde_json::from_value::<AccountJournal>(v).is_err(),
            "{name}"
        );
    }
}

#[tokio::test]
async fn foreign_account_journal_is_refused_and_untouched() {
    let store = journal_with_one_reservation().await;
    store.tamper(|v| v["config"]["id"] = json!(OTHER_ACCOUNT));
    let saves = store.save_attempts();
    assert_eq!(
        open_err(&store, cfg(3)).await,
        SupervisorError::Config(ConfigError::Conflict)
    );
    assert_eq!(store.save_attempts(), saves);
    assert_eq!(store.json()["config"]["id"], json!(OTHER_ACCOUNT));
}

#[tokio::test]
async fn valid_edits_for_this_account_are_honoured_and_fence() {
    // A reservation taken under an older secret is legal journal state.
    let store = journal_with_one_reservation().await;
    store.tamper(|v| {
        v["config"]["revision"] = json!(3);
        v["config"]["secret_ref"] = json!(SECRET_B);
        v["reservations"]["1"]["secret_ref"] = json!(SECRET_A);
        v["reservations"]["1"]["revision"] = json!(2);
    });
    let mut sup = open(&store, rotated(cfg(3), 3, SECRET_B)).await;
    assert_eq!(
        sup.reserve(WorkClass::Stream, 0).await,
        Err(SupervisorError::Fenced)
    );
    // A quarantine for the current secret in a loaded journal is honoured.
    let store = FakeStorage::new();
    drop(open(&store, cfg(3)).await);
    store.tamper(|v| v["quarantined_secret"] = json!(SECRET_A));
    let mut sup = open(&store, cfg(3)).await;
    assert!(is_quarantined(sup.reserve(WorkClass::Stream, 0).await));
}
