//! Serialized, account-bound owner for the durable supervisor. Not routed yet.
//!
//! A Durable Object keeps exactly one owner for its lifetime. The async mutex
//! covers initialization and every mutation, including storage acknowledgements;
//! there is no RefCell borrow across an await and no competing lazy initializer.
//! Cancellation during initialization leaves a terminal fence, not an empty slot
//! that could initialize again over an uncertain write.

use std::ops::{Deref, DerefMut};

use futures_util::lock::{Mutex, MutexGuard};

use crate::account_pool::{uuid_shaped, AccountConfig, ConfigError};
use crate::account_supervisor::{
    AccountStorage, AccountSupervisor, CloseControl, SessionId, SessionResult, SupervisorError,
};

pub struct AccountOwner<S, C> {
    account_id: String,
    state: Mutex<OwnerState<S, C>>,
}

enum OwnerState<S, C> {
    Unopened(S),
    Fenced,
    Ready {
        supervisor: Box<AccountSupervisor<S, C>>,
        last_now_ms: u64,
    },
}

impl<S: AccountStorage, C: CloseControl> AccountOwner<S, C> {
    /// `account_id` is the DO's name, not an envelope-supplied identity. Storage
    /// is exclusive to this owner; never construct two owners over the same DO.
    pub fn new(account_id: String, storage: S) -> Result<Self, SupervisorError> {
        if !uuid_shaped(&account_id) {
            return Err(SupervisorError::Config(ConfigError::Invalid));
        }
        Ok(Self {
            account_id,
            state: Mutex::new(OwnerState::Unopened(storage)),
        })
    }

    /// `current` must come from a fresh authenticated registry read, not a
    /// caller's selection hint. Acquire/reconcile again for every admission.
    /// Keep this guard through reserve/start or result/close acknowledgement;
    /// release it before awaiting the operation's socket I/O.
    pub async fn lock(
        &self,
        current: AccountConfig,
        now_ms: u64,
    ) -> Result<AccountGuard<'_, S, C>, SupervisorError> {
        if !current.valid() {
            return Err(SupervisorError::Config(ConfigError::Invalid));
        }
        if current.id != self.account_id {
            return Err(SupervisorError::Config(ConfigError::Conflict));
        }
        let mut state = self.state.lock().await;
        if matches!(*state, OwnerState::Unopened(_)) {
            let OwnerState::Unopened(storage) = std::mem::replace(&mut *state, OwnerState::Fenced)
            else {
                unreachable!()
            };
            let supervisor = AccountSupervisor::open(storage, current).await?;
            *state = OwnerState::Ready {
                supervisor: Box::new(supervisor),
                last_now_ms: now_ms,
            };
        } else if let OwnerState::Ready {
            supervisor,
            last_now_ms,
        } = &mut *state
        {
            supervisor.advance(now_ms.saturating_sub(*last_now_ms));
            *last_now_ms = (*last_now_ms).max(now_ms);
            supervisor.reconcile(current).await?;
        } else {
            return Err(SupervisorError::Fenced);
        }
        Ok(AccountGuard { state })
    }

    /// Cleanup never needs a successful registry read or reconciliation write.
    /// Even a poisoned owner must retain a path to drain physical sockets.
    pub async fn close(&self, id: SessionId) -> Result<(), SupervisorError> {
        let mut state = self.state.lock().await;
        match &mut *state {
            OwnerState::Ready { supervisor, .. } => supervisor.close(id).await,
            _ => Err(SupervisorError::Fenced),
        }
    }

    /// Tick at I/O completion, not at the last admission: slow operations must
    /// not consume part of the breaker's cooldown before they even fail.
    pub async fn record_result(
        &self,
        id: SessionId,
        result: SessionResult,
        now_ms: u64,
    ) -> Result<(), SupervisorError> {
        let mut state = self.state.lock().await;
        match &mut *state {
            OwnerState::Ready {
                supervisor,
                last_now_ms,
            } => {
                supervisor.advance(now_ms.saturating_sub(*last_now_ms));
                *last_now_ms = (*last_now_ms).max(now_ms);
                supervisor.record_result(id, result).await
            }
            _ => Err(SupervisorError::Fenced),
        }
    }

    pub async fn cancel_unstarted(&self, id: SessionId) -> Result<(), SupervisorError> {
        let mut state = self.state.lock().await;
        match &mut *state {
            OwnerState::Ready { supervisor, .. } => supervisor.cancel_unstarted(id).await,
            _ => Err(SupervisorError::Fenced),
        }
    }

    /// Synchronous AUTHINFO hook: refuse rather than wait or panic when a
    /// mutation is in progress. No guard or mutable reference escapes this read.
    pub fn auth_permitted(&self, id: SessionId, proof_expires_at_ms: u64, now_ms: u64) -> bool {
        let Some(state) = self.state.try_lock() else {
            return false;
        };
        match &*state {
            OwnerState::Ready { supervisor, .. } => {
                supervisor.auth_permitted(id, proof_expires_at_ms, now_ms)
            }
            _ => false,
        }
    }
}

pub struct AccountGuard<'a, S, C> {
    state: MutexGuard<'a, OwnerState<S, C>>,
}

impl<S, C> Deref for AccountGuard<'_, S, C> {
    type Target = AccountSupervisor<S, C>;

    fn deref(&self) -> &Self::Target {
        match &*self.state {
            OwnerState::Ready { supervisor, .. } => supervisor,
            _ => unreachable!(),
        }
    }
}

impl<S, C> DerefMut for AccountGuard<'_, S, C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match &mut *self.state {
            OwnerState::Ready { supervisor, .. } => supervisor,
            _ => unreachable!(),
        }
    }
}
