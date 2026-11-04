//! Account owner: serialized, account-bound wrapper over the supervisor.
//!
//! Deterministic, single-threaded, no timers: pending states are produced by
//! fake storage "blocks" and futures are advanced by hand with a no-op waker.
//! The fakes are NOT evidence of real Worker storage or socket behaviour.

use std::cell::RefCell;
use std::future::{poll_fn, ready, Future, Ready};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use nntune_stream::account_owner::AccountOwner;
use nntune_stream::account_pool::{AccountConfig, ConfigError, WorkClass};
use nntune_stream::account_supervisor::{
    AccountJournal, AccountStorage, CloseControl, CloseError, SessionId, SessionResult,
    StorageError, SupervisorError,
};
use nntune_stream::pool::HARD_MAX_CONNS;
use serde_json::Value;

const ACCOUNT: &str = "12345678-1234-4234-8234-123456789abc";
const OTHER_ACCOUNT: &str = "52345678-1234-4234-8234-123456789abc";
const SECRET_A: &str = "22345678-1234-4234-8234-123456789abc";

// ---------------------------------------------------------------- fakes

struct StoreState {
    journal: Option<AccountJournal>,
    block_load: bool,
    block_save: bool,
    fail_load: bool,
    fail_save: bool,
    loads: u32,
    save_attempts: u32,
    /// One entry per save attempt: number of reservations in that journal.
    log: Vec<usize>,
}

#[derive(Clone)]
struct FakeStorage(Rc<RefCell<StoreState>>);

impl FakeStorage {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(StoreState {
            journal: None,
            block_load: false,
            block_save: false,
            fail_load: false,
            fail_save: false,
            loads: 0,
            save_attempts: 0,
            log: Vec::new(),
        })))
    }
    fn block_load(&self, v: bool) {
        self.0.borrow_mut().block_load = v;
    }
    fn block_save(&self, v: bool) {
        self.0.borrow_mut().block_save = v;
    }
    fn fail_load(&self, v: bool) {
        self.0.borrow_mut().fail_load = v;
    }
    fn fail_save(&self, v: bool) {
        self.0.borrow_mut().fail_save = v;
    }
    fn loads(&self) -> u32 {
        self.0.borrow().loads
    }
    fn saves(&self) -> u32 {
        self.0.borrow().save_attempts
    }
    fn log(&self) -> Vec<usize> {
        self.0.borrow().log.clone()
    }
    fn json(&self) -> Value {
        serde_json::to_value(self.0.borrow().journal.as_ref().expect("journal saved")).unwrap()
    }
    fn has_journal(&self) -> bool {
        self.0.borrow().journal.is_some()
    }
    fn reservations(&self) -> usize {
        self.json()["reservations"].as_object().unwrap().len()
    }
}

impl AccountStorage for FakeStorage {
    async fn load(&mut self) -> Result<Option<AccountJournal>, StorageError> {
        self.0.borrow_mut().loads += 1;
        poll_fn(|_| {
            if self.0.borrow().block_load {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        let s = self.0.borrow();
        if s.fail_load {
            return Err(StorageError);
        }
        Ok(s.journal.clone())
    }

    async fn save(&mut self, journal: &AccountJournal) -> Result<(), StorageError> {
        {
            let mut s = self.0.borrow_mut();
            s.save_attempts += 1;
            let n = serde_json::to_value(journal).unwrap()["reservations"]
                .as_object()
                .unwrap()
                .len();
            s.log.push(n);
        }
        poll_fn(|_| {
            if self.0.borrow().block_save {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        let mut s = self.0.borrow_mut();
        if s.fail_save {
            return Err(StorageError);
        }
        s.journal = Some(journal.clone());
        Ok(())
    }
}

struct CloseState {
    calls: u32,
    confirmed: bool,
    /// While set, `close` suspends (after counting the call) and confirms nothing.
    blocked: bool,
}

#[derive(Clone)]
struct FakeControl(Rc<RefCell<CloseState>>);

impl FakeControl {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(CloseState {
            calls: 0,
            confirmed: false,
            blocked: false,
        })))
    }
    fn block(&self, v: bool) {
        self.0.borrow_mut().blocked = v;
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
        self.0.borrow_mut().calls += 1;
        poll_fn(|_| {
            if self.0.borrow().blocked {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        self.0.borrow_mut().confirmed = true;
        Ok(())
    }
    fn confirmed_closed(&self) -> bool {
        self.0.borrow().confirmed
    }
}

// -------------------------------------------------------------- helpers

type Owner = AccountOwner<FakeStorage, FakeControl>;

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

fn owner(store: &FakeStorage) -> Owner {
    match Owner::new(ACCOUNT.into(), store.clone()) {
        Ok(o) => o,
        Err(e) => panic!("owner must construct: {e:?}"),
    }
}

/// One manual poll with a no-op waker (waiters are re-polled by the test).
fn poll<F: Future + ?Sized>(f: &mut Pin<Box<F>>) -> Poll<F::Output> {
    f.as_mut().poll(&mut Context::from_waker(Waker::noop()))
}

fn done<T>(p: Poll<T>) -> T {
    match p {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("expected the future to be ready"),
    }
}

fn is_pending<F: Future + ?Sized>(f: &mut Pin<Box<F>>) -> bool {
    poll(f).is_pending()
}

/// Drive a future that must complete without suspending.
fn now<F: Future>(f: F) -> F::Output {
    done(poll(&mut Box::pin(f)))
}

/// Reserve and start one stream session through the guard; returns the close control.
fn reserve_started(o: &Owner, config: AccountConfig, at: u64) -> (SessionId, FakeControl) {
    let mut g = now(o.lock(config, at)).expect("lock");
    let id = now(g.reserve(WorkClass::Stream, at)).unwrap();
    let ctl = FakeControl::new();
    let handed = ctl.clone();
    let _setup: Ready<()> = g
        .start(id, u64::MAX, at, move || Ok::<_, ()>((handed, ready(()))))
        .unwrap();
    (id, ctl)
}

// -------------------------------------------------------- lazy init

#[test]
fn concurrent_lock_initializes_storage_exactly_once() {
    let store = FakeStorage::new();
    let o = owner(&store);
    store.block_load(true);
    let mut a = Box::pin(o.lock(cfg(3), 0));
    let mut b = Box::pin(o.lock(cfg(3), 0));
    let mut c = Box::pin(o.lock(cfg(3), 0));
    assert!(is_pending(&mut a));
    assert!(is_pending(&mut b));
    assert!(is_pending(&mut c));
    assert_eq!(store.loads(), 1, "only the first lock reaches storage");
    store.block_load(false);

    let ga = done(poll(&mut a)).expect("first lock opens");
    assert!(is_pending(&mut b));
    assert!(is_pending(&mut c));
    drop(ga);
    let gb = done(poll(&mut b)).expect("second lock");
    assert!(is_pending(&mut c));
    drop(gb);
    drop(done(poll(&mut c)).expect("third lock"));

    assert_eq!(store.loads(), 1);
    // open's reconcile save, then one reconcile save per later lock
    assert_eq!(store.log(), vec![0, 0, 0]);
}

// ------------------------------------------------ guard serialization

#[test]
fn guard_serializes_across_a_pending_save() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let mut g = now(o.lock(cfg(3), 0)).expect("lock");
    assert_eq!(store.log(), vec![0]);

    store.block_save(true);
    let mut reserve = Box::pin(g.reserve(WorkClass::Stream, 0));
    assert!(is_pending(&mut reserve));
    assert_eq!(store.log(), vec![0, 1]);

    // Another admission queues behind the held guard.
    let mut other = Box::pin(o.lock(cfg(3), 0));
    assert!(is_pending(&mut other));
    assert!(is_pending(&mut other));
    assert_eq!(store.log(), vec![0, 1], "no storage touch while guard held");

    store.block_save(false);
    let id = done(poll(&mut reserve)).unwrap();
    drop(reserve);
    assert!(is_pending(&mut other), "guard still held after the save");
    drop(g);

    let g2 = done(poll(&mut other)).expect("second lock after release");
    assert_eq!(
        store.log(),
        vec![0, 1, 1],
        "second reconcile after the reserve"
    );
    assert!(g2.auth_permitted(id, u64::MAX, 0));
}

#[test]
fn dropping_a_queued_lock_does_not_fence_the_owner() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let g = now(o.lock(cfg(3), 0)).expect("lock");
    let mut queued = Box::pin(o.lock(cfg(3), 0));
    assert!(is_pending(&mut queued));
    drop(queued);
    drop(g);
    assert!(now(o.lock(cfg(3), 0)).is_ok());
}

// ------------------------------------- cancellation leaves a fence

#[test]
fn cancel_during_load_fences_without_reinit() {
    let store = FakeStorage::new();
    let o = owner(&store);
    store.block_load(true);
    let mut a = Box::pin(o.lock(cfg(3), 0));
    assert!(is_pending(&mut a));
    drop(a);
    store.block_load(false);

    let e = now(o.lock(cfg(3), 0)).err().expect("fenced");
    assert_eq!(e, SupervisorError::Fenced);
    assert_eq!(store.loads(), 1, "no second open over an uncertain state");
    assert_eq!(store.saves(), 0);
    assert!(!o.auth_permitted(unreachable_id(&store), 10, 0));
}

#[test]
fn cancel_during_open_save_fences_without_reinit() {
    let store = FakeStorage::new();
    let o = owner(&store);
    store.block_save(true);
    let mut a = Box::pin(o.lock(cfg(3), 0));
    assert!(is_pending(&mut a));
    drop(a);
    store.block_save(false);

    let e = now(o.lock(cfg(3), 0)).err().expect("fenced");
    assert_eq!(e, SupervisorError::Fenced);
    assert_eq!(store.loads(), 1);
    assert_eq!(store.saves(), 1, "the unknown write is not retried");
}

#[test]
fn cancel_during_reserve_save_fences_later_locks_and_admission() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let mut g = now(o.lock(cfg(3), 0)).expect("lock");
    store.block_save(true);
    let mut reserve = Box::pin(g.reserve(WorkClass::Stream, 0));
    assert!(is_pending(&mut reserve));
    drop(reserve);
    drop(g);
    store.block_save(false);

    let saves = store.saves();
    let e = now(o.lock(cfg(3), 0)).err().expect("fenced");
    assert_eq!(e, SupervisorError::Fenced);
    assert_eq!(store.saves(), saves, "fenced lock never writes");
    assert_eq!(store.loads(), 1);
}

#[test]
fn cancel_during_reconcile_save_fences() {
    let store = FakeStorage::new();
    let o = owner(&store);
    drop(now(o.lock(cfg(3), 0)).expect("lock"));
    store.block_save(true);
    let mut a = Box::pin(o.lock(cfg(3), 0));
    assert!(is_pending(&mut a));
    drop(a);
    store.block_save(false);
    let saves = store.saves();
    assert_eq!(
        now(o.lock(cfg(3), 0)).err().expect("fenced"),
        SupervisorError::Fenced
    );
    assert_eq!(store.saves(), saves);
    assert_eq!(store.loads(), 1);
}

#[test]
fn failed_open_is_terminal_even_after_storage_recovers() {
    let store = FakeStorage::new();
    let o = owner(&store);
    store.fail_load(true);
    assert_eq!(
        now(o.lock(cfg(3), 0)).err().expect("storage error"),
        SupervisorError::Storage
    );
    store.fail_load(false);
    assert_eq!(
        now(o.lock(cfg(3), 0)).err().expect("fenced"),
        SupervisorError::Fenced
    );
    assert_eq!(store.loads(), 1);
    assert!(!store.has_journal());
}

/// A SessionId can only come from a supervisor; borrow one from a scratch
/// owner so fenced-owner queries have a syntactically valid argument.
fn unreachable_id(_: &FakeStorage) -> SessionId {
    let scratch = owner(&FakeStorage::new());
    reserve_started(&scratch, cfg(3), 0).0
}

// ------------------------------------------- identity validation

#[test]
fn invalid_do_name_never_constructs_or_touches_storage() {
    let store = FakeStorage::new();
    for name in ["", "not-a-uuid", "12345678-1234-4234-8234-123456789ABC"] {
        match Owner::new(name.into(), store.clone()) {
            Err(e) => assert_eq!(e, SupervisorError::Config(ConfigError::Invalid)),
            Ok(_) => panic!("{name:?} must be refused"),
        }
    }
    assert_eq!((store.loads(), store.saves()), (0, 0));
}

#[test]
fn mismatched_or_invalid_snapshot_does_not_touch_storage_or_fence() {
    let store = FakeStorage::new();
    let o = owner(&store);

    let mut other = cfg(3);
    other.id = OTHER_ACCOUNT.into();
    assert_eq!(
        now(o.lock(other, 0)).err().expect("conflict"),
        SupervisorError::Config(ConfigError::Conflict)
    );

    let mut bad = cfg(3);
    bad.secret_ref = "nope".into();
    assert_eq!(
        now(o.lock(bad, 0)).err().expect("invalid"),
        SupervisorError::Config(ConfigError::Invalid)
    );
    assert_eq!((store.loads(), store.saves()), (0, 0));

    // Still initializes normally afterwards: refusal is not a fence.
    assert!(now(o.lock(cfg(3), 0)).is_ok());
    assert_eq!(store.loads(), 1);
}

// -------------------------------------------------- reserve cap

#[test]
fn concurrent_reserves_never_exceed_cap_or_ingest_reserve() {
    type Task<'a> = Pin<Box<dyn Future<Output = Result<SessionId, SupervisorError>> + 'a>>;
    fn task(o: &Owner, class: WorkClass) -> Task<'_> {
        Box::pin(async move {
            let mut g = o.lock(cfg(3), 0).await?;
            g.reserve(class, 0).await
        })
    }

    let store = FakeStorage::new();
    let o = owner(&store);
    let classes = [
        WorkClass::Ingest,
        WorkClass::Ingest,
        WorkClass::Ingest,
        WorkClass::Stream,
        WorkClass::Stream,
    ];
    store.block_save(true);
    let mut tasks: Vec<Option<Task<'_>>> = classes.iter().map(|c| Some(task(&o, *c))).collect();
    let mut results: Vec<Option<Result<SessionId, SupervisorError>>> = vec![None; tasks.len()];
    for t in tasks.iter_mut().flatten() {
        assert!(is_pending(t));
    }
    store.block_save(false);
    for _ in 0..50 {
        for (t, r) in tasks.iter_mut().zip(results.iter_mut()) {
            if let Some(f) = t {
                if let Poll::Ready(v) = poll(f) {
                    *r = Some(v);
                    *t = None;
                }
            }
        }
        if tasks.iter().all(Option::is_none) {
            break;
        }
    }
    assert!(tasks.iter().all(Option::is_none), "bounded drive finished");

    let ok: Vec<bool> = results.iter().map(|r| r.unwrap().is_ok()).collect();
    assert_eq!(ok.iter().filter(|b| **b).count(), 3, "exactly the cap");
    let ingest_ok = ok[..3].iter().filter(|b| **b).count();
    assert!(ingest_ok <= 2, "ingest never takes the stream reserve");
    for r in results.iter().map(|r| r.unwrap()).filter(|r| r.is_err()) {
        assert!(matches!(r, Err(SupervisorError::Unavailable(_))));
    }
    assert_eq!(store.reservations(), 3);
    assert!(store.reservations() <= HARD_MAX_CONNS as usize);
    let g = now(o.lock(cfg(3), 0)).expect("lock");
    assert_eq!(g.candidate(WorkClass::Stream).in_flight, 3);
    assert!(!g.candidate(WorkClass::Stream).eligible);
}

// ------------------------------------- stale / conflicting snapshots

#[test]
fn stale_and_conflicting_snapshots_refuse_without_restoring_settings() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let mut current = cfg(4);
    current.revision = 5;
    drop(now(o.lock(current.clone(), 0)).expect("rev 5"));
    let saves = store.saves();
    let before = store.json();

    let mut stale = cfg(2);
    stale.revision = 3;
    assert_eq!(
        now(o.lock(stale, 1)).err().expect("stale"),
        SupervisorError::Config(ConfigError::Stale)
    );
    let mut conflict = current.clone();
    conflict.tier = 7;
    assert_eq!(
        now(o.lock(conflict, 1)).err().expect("conflict"),
        SupervisorError::Config(ConfigError::Conflict)
    );
    assert_eq!(store.saves(), saves, "refusals write nothing");
    assert_eq!(store.json(), before);

    // The owner is not fenced and still holds the newest settings.
    let g = now(o.lock(current, 2)).expect("current still works");
    assert_eq!(g.candidate(WorkClass::Stream).tier, 0);
}

#[test]
fn stale_snapshot_on_reopen_leaves_stored_settings_untouched() {
    let store = FakeStorage::new();
    let mut current = cfg(4);
    current.revision = 5;
    drop(now(owner(&store).lock(current, 0)).expect("seed"));
    let before = store.json();
    let saves = store.saves();

    let reopened = owner(&store);
    let mut stale = cfg(2);
    stale.revision = 3;
    assert_eq!(
        now(reopened.lock(stale, 0)).err().expect("stale"),
        SupervisorError::Config(ConfigError::Stale)
    );
    assert_eq!(store.json(), before);
    assert_eq!(store.saves(), saves);
    // Open failed after claiming storage: terminal fence, not a re-init.
    let mut again = cfg(4);
    again.revision = 5;
    assert_eq!(
        now(reopened.lock(again, 0)).err().expect("fenced"),
        SupervisorError::Fenced
    );
    assert_eq!(store.json(), before);
}

// ------------------------------------------------- auth_permitted

#[test]
fn auth_permitted_refuses_while_locked_and_works_outside_the_guard() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let (id, _ctl) = reserve_started(&o, cfg(3), 100);
    // Guard already dropped: synchronous check works outside any guard.
    assert!(o.auth_permitted(id, 1_000, 100));
    // Expired proof (now >= proof expiry) and exact boundary are refused.
    assert!(!o.auth_permitted(id, 100, 100));
    assert!(!o.auth_permitted(id, 99, 100));
    let g = now(o.lock(cfg(3), 100)).expect("lock");
    assert!(
        !o.auth_permitted(id, 1_000, 100),
        "false, not a panic, while locked"
    );
    drop(g);
    assert!(o.auth_permitted(id, 1_000, 100));
    // Reservation TTL elapsed; a subsequent backward reading cannot restore it.
    assert!(!o.auth_permitted(id, u64::MAX, 100 + 20_000));
    assert!(!o.auth_permitted(id, 1_000, 100));
}

#[test]
fn auth_permitted_is_false_before_initialization() {
    let scratch_id = unreachable_id(&FakeStorage::new());
    let store = FakeStorage::new();
    let o = owner(&store);
    assert!(!o.auth_permitted(scratch_id, 1_000, 0));
    assert_eq!((store.loads(), store.saves()), (0, 0));
}

#[test]
fn auth_permitted_clamps_a_backward_clock_to_the_owner_high_water_mark() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let (id, _ctl) = reserve_started(&o, cfg(3), 0);
    assert!(
        o.auth_permitted(id, 5_000, 4_999),
        "control: valid before the mark moves"
    );
    // A later admission advances the high-water mark; no cleanup ever runs.
    drop(now(o.lock(cfg(3), 6_000)).expect("lock"));
    assert!(
        !o.auth_permitted(id, 5_000, 100),
        "proof expired at the 6_000 high-water mark, not at the raw 100"
    );
    assert!(!o.auth_permitted(id, 6_000, 0));
    assert!(
        o.auth_permitted(id, 6_001, 100),
        "mark itself is still inside the proof"
    );
    // The same clamp applies to the session TTL.
    drop(now(o.lock(cfg(3), 20_000)).expect("lock"));
    assert!(!o.auth_permitted(id, u64::MAX, 0));
}

// ------------------------------------------------ cleanup close

#[test]
fn close_after_reconciliation_storage_error_closes_physically_but_stays_fenced() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let (id, ctl) = reserve_started(&o, cfg(3), 0);
    assert_eq!(store.reservations(), 1);

    // A newer snapshot whose reconcile write fails: poisons the instance.
    let mut next = cfg(3);
    next.revision = 2;
    store.fail_save(true);
    assert_eq!(
        now(o.lock(next.clone(), 1)).err().expect("storage"),
        SupervisorError::Storage
    );
    store.fail_save(false);
    assert_eq!(
        now(o.lock(next, 1)).err().expect("fenced"),
        SupervisorError::Fenced
    );

    // Cleanup still reaches the physical close, but never releases the slot.
    assert_eq!(now(o.close(id)), Err(SupervisorError::Fenced));
    assert_eq!(ctl.calls(), 1);
    assert!(ctl.confirmed());
    assert_eq!(store.reservations(), 1, "reservation remains durable");
    assert_eq!(now(o.close(id)), Err(SupervisorError::Fenced));
    assert_eq!(store.reservations(), 1);
    assert!(!o.auth_permitted(id, u64::MAX, 1));
}

#[test]
fn delayed_results_receive_full_breaker_cooldown() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let sessions: Vec<_> = (0..3).map(|_| reserve_started(&o, cfg(3), 0).0).collect();
    // I/O fails at t=19s, without any intervening admission to tick the pool.
    for id in sessions {
        now(o.record_result(id, SessionResult::ProviderFailure, 19_000)).unwrap();
        now(o.close(id)).unwrap();
    }
    let guard = now(o.lock(cfg(3), 30_000)).expect("lock");
    assert!(!guard.candidate(WorkClass::Stream).eligible);
    drop(guard);
    let guard = now(o.lock(cfg(3), 48_999)).expect("lock");
    assert!(!guard.candidate(WorkClass::Stream).eligible);
    drop(guard);
    let guard = now(o.lock(cfg(3), 49_000)).expect("lock");
    assert!(guard.candidate(WorkClass::Stream).eligible);
}

#[test]
fn backward_clock_does_not_double_count_breaker_cooldown() {
    let store = FakeStorage::new();
    let o = owner(&store);
    for _ in 0..3 {
        let (id, _) = reserve_started(&o, cfg(3), 0);
        now(o.record_result(id, SessionResult::ProviderFailure, 0)).unwrap();
        now(o.close(id)).unwrap();
    }
    for at in [20_000, 10_000, 29_999] {
        let guard = now(o.lock(cfg(3), at)).expect("lock");
        assert!(!guard.candidate(WorkClass::Stream).eligible);
    }
    let guard = now(o.lock(cfg(3), 30_000)).expect("lock");
    assert!(guard.candidate(WorkClass::Stream).eligible);
}

// ------------------------------------------------ record_result

#[test]
fn record_result_persists_quarantine_through_reopen() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let (id, ctl) = reserve_started(&o, cfg(3), 0);
    now(o.record_result(id, SessionResult::AuthFailure, 0)).unwrap();
    assert_eq!(store.json()["quarantined_secret"], Value::from(SECRET_A));
    now(o.close(id)).unwrap();
    assert!(ctl.confirmed());
    assert_eq!(store.reservations(), 0);

    let reopened = owner(&store);
    let mut g = now(reopened.lock(cfg(3), 5)).expect("reopen");
    let r = now(g.reserve(WorkClass::Stream, 5));
    assert!(
        matches!(r, Err(SupervisorError::Unavailable(_))),
        "quarantined secret is not retried: {r:?}"
    );
    assert!(!g.candidate(WorkClass::Stream).eligible);
    assert_eq!(store.json()["quarantined_secret"], Value::from(SECRET_A));
}

#[test]
fn owner_operations_on_an_uninitialized_owner_are_fenced() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let id = unreachable_id(&store);
    assert_eq!(now(o.close(id)), Err(SupervisorError::Fenced));
    assert_eq!(
        now(o.record_result(id, SessionResult::Success, 0)),
        Err(SupervisorError::Fenced)
    );
    assert_eq!(now(o.cancel_unstarted(id)), Err(SupervisorError::Fenced));
    assert_eq!((store.loads(), store.saves()), (0, 0));
}

// ------------------------------------------------ cleanup (owner)

fn counts<E: std::fmt::Debug>(
    r: Result<nntune_stream::account_supervisor::CleanupReport, E>,
) -> (u32, u32) {
    let r = r.expect("cleanup reports");
    (r.retired, r.retained)
}

#[test]
fn cleanup_on_an_unopened_owner_reports_nothing_without_touching_storage() {
    let store = FakeStorage::new();
    let o = owner(&store);
    assert_eq!(counts(now(o.cleanup(0))), (0, 0));
    assert_eq!(counts(now(o.cleanup(u64::MAX))), (0, 0));
    assert_eq!((store.loads(), store.saves()), (0, 0));
    // Still unopened, not fenced: a normal lock initializes it afterwards.
    assert!(now(o.lock(cfg(3), 0)).is_ok());
    assert_eq!(store.loads(), 1);
}

#[test]
fn cleanup_on_a_fenced_owner_returns_fenced() {
    // Cancelled initialization leaves the terminal fence.
    let store = FakeStorage::new();
    let o = owner(&store);
    store.block_load(true);
    let mut a = Box::pin(o.lock(cfg(3), 0));
    assert!(is_pending(&mut a));
    drop(a);
    store.block_load(false);
    assert_eq!(now(o.cleanup(0)).err(), Some(SupervisorError::Fenced));
    assert_eq!(store.loads(), 1, "no second open over an uncertain state");
    assert_eq!(store.saves(), 0);

    // So does a failed initialization.
    let store = FakeStorage::new();
    let o = owner(&store);
    store.fail_load(true);
    assert_eq!(
        now(o.lock(cfg(3), 0)).err().expect("storage"),
        SupervisorError::Storage
    );
    store.fail_load(false);
    assert_eq!(now(o.cleanup(0)).err(), Some(SupervisorError::Fenced));
    assert_eq!(store.loads(), 1);
}

#[test]
fn cleanup_needs_no_registry_snapshot_and_leaves_active_sessions_alone() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let (old, old_ctl) = reserve_started(&o, cfg(3), 0);
    let (live, live_ctl) = reserve_started(&o, cfg(3), 15_000);
    let (loads, saves) = (store.loads(), store.saves());

    assert_eq!(counts(now(o.cleanup(19_999))), (0, 2));
    assert_eq!(
        (store.loads(), store.saves()),
        (loads, saves),
        "no-op writes nothing"
    );
    assert_eq!(counts(now(o.cleanup(20_000))), (1, 1));
    assert_eq!(old_ctl.calls(), 1);
    assert!(old_ctl.confirmed());
    assert_eq!(live_ctl.calls(), 0);
    assert_eq!(store.reservations(), 1);
    assert_eq!(store.loads(), loads, "never reloads");
    assert!(!o.auth_permitted(old, u64::MAX, 20_000));
    assert!(o.auth_permitted(live, u64::MAX, 20_000));
}

#[test]
fn cleanup_after_a_failed_reconciliation_still_closes_but_releases_nothing() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let (id, ctl) = reserve_started(&o, cfg(3), 0);
    let mut next = cfg(3);
    next.revision = 2;
    store.fail_save(true);
    assert_eq!(
        now(o.lock(next, 1)).err().expect("storage"),
        SupervisorError::Storage
    );
    store.fail_save(false);
    let saves = store.saves();

    let r = now(o.cleanup(20_000));
    assert!(
        matches!(r, Err(SupervisorError::Fenced | SupervisorError::Storage)),
        "poisoned owner cannot report a clean release"
    );
    assert_eq!(ctl.calls(), 1);
    assert!(ctl.confirmed());
    assert_eq!(store.saves(), saves);
    assert_eq!(store.reservations(), 1, "reservation remains durable");
    assert!(!o.auth_permitted(id, u64::MAX, 0));
}

#[test]
fn cleanup_storage_failure_poisons_the_owner_after_closing_physically() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let (id, ctl) = reserve_started(&o, cfg(3), 0);
    store.fail_save(true);
    assert_eq!(now(o.cleanup(20_000)).err(), Some(SupervisorError::Storage));
    store.fail_save(false);
    assert!(ctl.confirmed());
    assert!(!o.auth_permitted(id, u64::MAX, 0));
    assert_eq!(
        now(o.lock(cfg(3), 20_001)).err().expect("fenced"),
        SupervisorError::Fenced
    );
}

#[test]
fn cleanup_timestamps_are_monotonic_so_a_backward_clock_cannot_resurrect_a_session() {
    // A later lock establishes the high-water mark; an earlier cleanup uses it.
    let store = FakeStorage::new();
    let o = owner(&store);
    let (id, ctl) = reserve_started(&o, cfg(3), 0);
    drop(now(o.lock(cfg(3), 30_000)).expect("lock"));
    assert_eq!(counts(now(o.cleanup(1))), (1, 0));
    assert!(ctl.confirmed());
    assert!(!o.auth_permitted(id, u64::MAX, 1));

    // A cancelled cleanup at a later time, then a backward one.
    let store = FakeStorage::new();
    let o = owner(&store);
    let (id, ctl) = reserve_started(&o, cfg(3), 0);
    ctl.block(true);
    let mut c = Box::pin(o.cleanup(25_000));
    assert!(is_pending(&mut c));
    assert_eq!(ctl.calls(), 1);
    drop(c);
    for t in [0, 1, 24_999, 25_000] {
        assert!(!o.auth_permitted(id, u64::MAX, t), "terminal at {t}");
    }
    ctl.block(false);
    assert_eq!(counts(now(o.cleanup(0))), (1, 0), "treated as >= 25_000");
    assert_eq!(store.reservations(), 0);
}

#[test]
fn cleanup_queues_behind_a_held_guard() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let (_, ctl) = reserve_started(&o, cfg(3), 0);
    let g = now(o.lock(cfg(3), 1)).expect("lock");
    let mut c = Box::pin(o.cleanup(20_000));
    assert!(is_pending(&mut c));
    assert_eq!(ctl.calls(), 0, "serialized behind the guard");
    drop(g);
    assert_eq!(counts(done(poll(&mut c))), (1, 0));
    assert_eq!(ctl.calls(), 1);
}

#[test]
fn explicit_close_cancellation_blocks_authentication_through_the_owner() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let (id, ctl) = reserve_started(&o, cfg(3), 0);
    assert!(o.auth_permitted(id, u64::MAX, 0));
    ctl.block(true);
    let mut c = Box::pin(o.close(id));
    assert!(is_pending(&mut c));
    drop(c);
    assert!(
        !o.auth_permitted(id, u64::MAX, 0),
        "terminal before the await"
    );
    assert_eq!(store.reservations(), 1);
    ctl.block(false);
    assert_eq!(
        counts(now(o.cleanup(0))),
        (1, 0),
        "closing sessions are finished"
    );
}

#[test]
fn consecutive_auth_checks_retain_observed_time_without_another_mutation() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let (id, _) = reserve_started(&o, cfg(3), 100);
    assert!(!o.auth_permitted(id, 5_000, 5_000));
    assert!(!o.auth_permitted(id, 5_000, 4_999));
    assert!(o.auth_permitted(id, 5_001, 0));
    assert!(!o.auth_permitted(id, u64::MAX, 20_100));
    assert!(!o.auth_permitted(id, u64::MAX, 20_099));
}

#[test]
fn auth_time_observations_do_not_double_count_breaker_cooldown() {
    let store = FakeStorage::new();
    let o = owner(&store);
    let (id, _) = reserve_started(&o, cfg(3), 0);
    for _ in 0..3 {
        now(o.record_result(id, SessionResult::ProviderFailure, 0)).unwrap();
    }
    assert!(
        !now(o.lock(cfg(3), 0))
            .unwrap()
            .candidate(WorkClass::Stream)
            .eligible
    );
    assert!(o.auth_permitted(id, u64::MAX, 19_000));
    assert!(
        !now(o.lock(cfg(3), 29_999))
            .unwrap()
            .candidate(WorkClass::Stream)
            .eligible
    );
    assert!(
        now(o.lock(cfg(3), 30_000))
            .unwrap()
            .candidate(WorkClass::Stream)
            .eligible
    );
}
