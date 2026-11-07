//! Deterministic backend contract tests, not actual Worker durability evidence.

use std::cell::RefCell;
use std::future::{pending, Future};
use std::rc::Rc;
use std::task::{Context, Waker};

use nntune_stream::account_pool::{AccountConfig, WorkClass};
use nntune_stream::account_storage::{JournalBackend, JournalStorage, MAX_JOURNAL_BYTES};
use nntune_stream::account_supervisor::{
    AccountJournal, AccountStorage, AccountSupervisor, CloseControl, CloseError, StorageError,
    SupervisorError,
};
use nntune_stream::pool::HARD_MAX_CONNS;
use serde_json::{json, Value};

const ACCOUNT: &str = "12345678-1234-4234-8234-123456789abc";
const SECRET: &str = "22345678-1234-4234-8234-123456789abc";

#[derive(Clone, Copy, Default)]
enum Fault {
    #[default]
    None,
    Fail,
    Pending,
}

#[derive(Default)]
struct Data {
    raw: Option<String>,
    events: Vec<&'static str>,
    sync_fault: Fault,
    get_fault: Fault,
    put_fault: Fault,
}

#[derive(Clone, Default)]
struct Backend(Rc<RefCell<Data>>);

async fn fault(fault: Fault) -> Result<(), StorageError> {
    match fault {
        Fault::None => Ok(()),
        Fault::Fail => Err(StorageError),
        Fault::Pending => pending().await,
    }
}

impl JournalBackend for Backend {
    async fn get(&mut self) -> Result<Option<String>, StorageError> {
        let (f, raw) = {
            let mut data = self.0.borrow_mut();
            data.events.push("get");
            (data.get_fault, data.raw.clone())
        };
        fault(f).await?;
        Ok(raw)
    }

    async fn put(&mut self, value: String) -> Result<(), StorageError> {
        let f = {
            let mut data = self.0.borrow_mut();
            data.events.push("put");
            data.put_fault
        };
        fault(f).await?;
        self.0.borrow_mut().raw = Some(value);
        Ok(())
    }

    async fn sync(&mut self) -> Result<(), StorageError> {
        let f = {
            let mut data = self.0.borrow_mut();
            data.events.push("sync");
            data.sync_fault
        };
        fault(f).await
    }
}

struct Closed;
impl CloseControl for Closed {
    async fn close(&self) -> Result<(), CloseError> {
        Ok(())
    }
    fn confirmed_closed(&self) -> bool {
        true
    }
}

fn config() -> AccountConfig {
    AccountConfig {
        id: ACCOUNT.into(),
        revision: i64::MAX as u64,
        secret_ref: SECRET.into(),
        can_read: true,
        max_connections: HARD_MAX_CONNS,
        stream_reserve: 1,
        ingest_limit: HARD_MAX_CONNS - 1,
        tier: 0,
        enabled: true,
    }
}

fn journal_value() -> Value {
    json!({
        "version": 1,
        "config": config(),
        "next_id": 1,
        "quarantined_secret": null,
        "reservations": {}
    })
}

fn journal() -> AccountJournal {
    serde_json::from_value(journal_value()).unwrap()
}

fn poll_pending(f: std::pin::Pin<&mut impl Future>) {
    assert!(f.poll(&mut Context::from_waker(Waker::noop())).is_pending());
}

#[tokio::test]
async fn empty_load_syncs_before_get_and_save_syncs_after_atomic_put() {
    let backend = Backend::default();
    let mut store = JournalStorage::new(backend.clone());
    assert!(store.load().await.unwrap().is_none());
    assert_eq!(backend.0.borrow().events, ["sync", "get"]);
    store.save(&journal()).await.unwrap();
    assert_eq!(backend.0.borrow().events, ["sync", "get", "put", "sync"]);
    let loaded = store.load().await.unwrap().unwrap();
    assert!(loaded == journal());
    assert_eq!(
        serde_json::to_value(loaded).unwrap()["config"]["revision"],
        json!(i64::MAX as u64)
    );
}

#[tokio::test]
async fn no_application_read_cache_is_used() {
    let backend = Backend::default();
    let mut store = JournalStorage::new(backend.clone());
    store.save(&journal()).await.unwrap();
    let mut value = journal_value();
    value["config"]["enabled"] = json!(false);
    backend.0.borrow_mut().raw = Some(value.to_string());
    let loaded = store.load().await.unwrap().unwrap();
    assert_eq!(serde_json::to_value(loaded).unwrap(), value);
}

#[tokio::test]
async fn load_failures_are_redacted_and_never_reset_a_journal() {
    for raw in [
        "not JSON".to_string(),
        "{}".to_string(),
        "null".to_string(),
        " ".repeat(MAX_JOURNAL_BYTES + 1),
    ] {
        let backend = Backend::default();
        backend.0.borrow_mut().raw = Some(raw.clone());
        let mut store = JournalStorage::new(backend.clone());
        assert!(matches!(store.load().await, Err(StorageError)));
        assert_eq!(backend.0.borrow().raw.as_ref(), Some(&raw));
        assert_eq!(backend.0.borrow().events, ["sync", "get"]);
    }
    for field in ["config", "quarantined_secret", "reservations"] {
        let backend = Backend::default();
        let mut value = journal_value();
        value.as_object_mut().unwrap().remove(field);
        backend.0.borrow_mut().raw = Some(value.to_string());
        assert!(matches!(
            JournalStorage::new(backend).load().await,
            Err(StorageError)
        ));
    }
    assert_eq!(format!("{StorageError:?}"), "StorageError");
}

#[tokio::test]
async fn unknown_fields_refuse() {
    let backend = Backend::default();
    let mut value = journal_value();
    value["credentials"] = json!("synthetic-only");
    backend.0.borrow_mut().raw = Some(value.to_string());
    assert!(matches!(
        JournalStorage::new(backend).load().await,
        Err(StorageError)
    ));
}

#[tokio::test]
async fn failed_read_sync_never_reads_and_failed_put_never_claims_sync() {
    let backend = Backend::default();
    backend.0.borrow_mut().sync_fault = Fault::Fail;
    let mut store = JournalStorage::new(backend.clone());
    assert!(matches!(store.load().await, Err(StorageError)));
    assert_eq!(backend.0.borrow().events, ["sync"]);
    {
        let mut data = backend.0.borrow_mut();
        data.events.clear();
        data.sync_fault = Fault::None;
        data.put_fault = Fault::Fail;
    }
    assert!(matches!(store.save(&journal()).await, Err(StorageError)));
    assert_eq!(backend.0.borrow().events, ["put"]);
    assert!(backend.0.borrow().raw.is_none());
}

#[tokio::test]
async fn get_error_is_not_treated_as_an_empty_journal() {
    let backend = Backend::default();
    backend.0.borrow_mut().get_fault = Fault::Fail;
    assert!(matches!(
        JournalStorage::new(backend.clone()).load().await,
        Err(StorageError)
    ));
    assert_eq!(backend.0.borrow().events, ["sync", "get"]);
}

#[tokio::test]
async fn save_is_pending_until_durable_sync_even_after_put_is_visible() {
    let backend = Backend::default();
    backend.0.borrow_mut().sync_fault = Fault::Pending;
    let mut store = JournalStorage::new(backend.clone());
    let j = journal();
    let mut saving = Box::pin(store.save(&j));
    poll_pending(saving.as_mut());
    assert!(backend.0.borrow().raw.is_some());
    assert_eq!(backend.0.borrow().events, ["put", "sync"]);
    drop(saving);
}

#[tokio::test]
async fn canceled_reservation_sync_never_exposes_a_handle_or_reopens_admission() {
    let backend = Backend::default();
    let mut owner =
        AccountSupervisor::<_, Closed>::open(JournalStorage::new(backend.clone()), config())
            .await
            .unwrap();
    backend.0.borrow_mut().sync_fault = Fault::Pending;
    let mut reserving = Box::pin(owner.reserve(WorkClass::Stream, 100));
    poll_pending(reserving.as_mut());
    let raw = backend.0.borrow().raw.clone().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&raw).unwrap()["reservations"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
    drop(reserving);
    assert!(!owner.candidate(WorkClass::Stream).eligible);
    assert_eq!(
        owner.reserve(WorkClass::Stream, 100).await,
        Err(SupervisorError::Fenced)
    );
    backend.0.borrow_mut().sync_fault = Fault::None;
    let recovered = AccountSupervisor::<_, Closed>::open(JournalStorage::new(backend), config())
        .await
        .unwrap();
    assert!(!recovered.candidate(WorkClass::Stream).eligible);
    assert_eq!(recovered.candidate(WorkClass::Stream).in_flight, 1);
}

#[tokio::test]
async fn failed_durable_sync_after_put_fences_even_if_value_was_written() {
    let backend = Backend::default();
    let mut owner =
        AccountSupervisor::<_, Closed>::open(JournalStorage::new(backend.clone()), config())
            .await
            .unwrap();
    backend.0.borrow_mut().sync_fault = Fault::Fail;
    assert_eq!(
        owner.reserve(WorkClass::Stream, 100).await,
        Err(SupervisorError::Storage)
    );
    assert!(!owner.candidate(WorkClass::Stream).eligible);
    backend.0.borrow_mut().sync_fault = Fault::None;
    let recovered = AccountSupervisor::<_, Closed>::open(JournalStorage::new(backend), config())
        .await
        .unwrap();
    assert!(!recovered.candidate(WorkClass::Stream).eligible);
}

#[tokio::test]
async fn maximal_account_journal_fits_bound_and_roundtrips_exact_integer_keys() {
    let backend = Backend::default();
    let mut owner =
        AccountSupervisor::<_, Closed>::open(JournalStorage::new(backend.clone()), config())
            .await
            .unwrap();
    for _ in 0..HARD_MAX_CONNS {
        owner
            .reserve(WorkClass::Stream, u64::MAX - 20_000)
            .await
            .unwrap();
    }
    let raw = backend.0.borrow().raw.clone().unwrap();
    assert!(raw.len() < MAX_JOURNAL_BYTES);
    let mut store = JournalStorage::new(backend.clone());
    let value = serde_json::to_value(store.load().await.unwrap().unwrap()).unwrap();
    let reservations = value["reservations"].as_object().unwrap();
    assert_eq!(reservations.len(), HARD_MAX_CONNS as usize);
    assert_eq!(reservations["50"]["expires_at_ms"], json!(u64::MAX));
    let recovered = AccountSupervisor::<_, Closed>::open(JournalStorage::new(backend), config())
        .await
        .unwrap();
    assert_eq!(
        recovered.candidate(WorkClass::Stream).in_flight,
        HARD_MAX_CONNS
    );
    assert!(!recovered.candidate(WorkClass::Stream).eligible);
}
