//! One bounded JSON journal, atomically replaced then durably synchronized.
//!
//! JSON is stored as a string, not a JS object: u64 revisions/deadlines retain
//! exact integers and reservation map keys/required null fields round-trip
//! identically on native and wasm. No resolver material enters this adapter.
//! The Worker factory is not called by any product route yet.

use crate::account_supervisor::{AccountJournal, AccountStorage, StorageError};

pub const ACCOUNT_JOURNAL_KEY: &str = "account-supervisor-v1";
pub const MAX_JOURNAL_BYTES: usize = 32 * 1024;

/// Exclusive per-DO backend. `put` is a single atomic replacement. `sync`
/// acknowledges durable commit, not just acceptance into a local write buffer.
/// Implementations must not maintain an application-level read cache.
#[allow(async_fn_in_trait)]
pub trait JournalBackend {
    async fn get(&mut self) -> Result<Option<String>, StorageError>;
    async fn put(&mut self, value: String) -> Result<(), StorageError>;
    async fn sync(&mut self) -> Result<(), StorageError>;
}

pub struct JournalStorage<B> {
    backend: B,
}

impl<B: JournalBackend> JournalStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }
}

impl<B: JournalBackend> AccountStorage for JournalStorage<B> {
    async fn load(&mut self) -> Result<Option<AccountJournal>, StorageError> {
        self.backend.sync().await?;
        self.backend
            .get()
            .await?
            .map(|raw| {
                if raw.len() > MAX_JOURNAL_BYTES {
                    return Err(StorageError);
                }
                serde_json::from_str(&raw).map_err(|_| StorageError)
            })
            .transpose()
    }

    async fn save(&mut self, journal: &AccountJournal) -> Result<(), StorageError> {
        let raw = serde_json::to_string(journal).map_err(|_| StorageError)?;
        if raw.len() > MAX_JOURNAL_BYTES {
            return Err(StorageError);
        }
        self.backend.put(raw).await?;
        self.backend.sync().await
    }
}

#[cfg(target_arch = "wasm32")]
mod worker_backend {
    use super::*;
    use crate::account_owner::AccountOwner;
    use crate::account_supervisor::SupervisorError;
    use nntune_core::transports::worker_pinned_tls::WorkerSocketControl;

    pub struct WorkerJournalBackend {
        storage: worker::Storage,
    }

    impl JournalBackend for WorkerJournalBackend {
        async fn get(&mut self) -> Result<Option<String>, StorageError> {
            self.storage
                .get(ACCOUNT_JOURNAL_KEY)
                .await
                .map_err(|_| StorageError)
        }

        async fn put(&mut self, value: String) -> Result<(), StorageError> {
            self.storage
                .put(ACCOUNT_JOURNAL_KEY, value)
                .await
                .map_err(|_| StorageError)
        }

        async fn sync(&mut self) -> Result<(), StorageError> {
            self.storage.sync().await.map_err(|_| StorageError)
        }
    }

    pub type WorkerAccountOwner =
        AccountOwner<JournalStorage<WorkerJournalBackend>, WorkerSocketControl>;

    /// Construct exactly once in a dedicated account DO's `new`, retaining the
    /// returned owner for that instance's entire lifetime. Never use this in a
    /// request handler, the user meter DO, or a second owner of the same storage.
    /// No namespace binding, migration, route or real dial is enabled here.
    pub fn worker_account_owner(
        state: &worker::State,
    ) -> Result<WorkerAccountOwner, SupervisorError> {
        let account_id = state.id().name().ok_or(SupervisorError::Refused)?;
        AccountOwner::new(
            account_id,
            JournalStorage::new(WorkerJournalBackend {
                storage: state.storage(),
            }),
        )
    }
}

#[cfg(target_arch = "wasm32")]
pub use worker_backend::{worker_account_owner, WorkerAccountOwner};
