//! Durable reservation/closure boundary for account sockets. Not runtime-wired.
//!
//! The single account owner serializes every mutation (`&mut self` across the
//! storage await). Storage must be exclusive to that owner, atomic and durable:
//! no cached reads, last-writer-wins competing owners or detached writes. The
//! Worker adapter must provide that contract before this can admit real dials.
//!
//! A reservation is saved before its handle escapes. The supervisor, not the
//! operation future, owns the pool lease and physical close control. Storage
//! uncertainty poisons the instance; recovery with ANY outstanding reservation
//! fences the entire account indefinitely. Neither TTL, credential rotation nor
//! restart is evidence that an old socket closed. There is deliberately no
//! automatic recovery/reset API or warm socket reuse in this first boundary.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::future::Future;
use std::rc::Rc;

use futures_util::future::join_all;
use serde::{Deserialize, Serialize};

use crate::account_pool::{
    uuid_shaped, AccountConfig, AccountPool, Candidate, ConfigError, WorkClass, SESSION_TTL_MS,
};
use crate::pool::{Lease, PoolError, HARD_MAX_CONNS};

/// Implementations must redact errors and atomically replace this one journal.
#[allow(async_fn_in_trait)]
pub trait AccountStorage {
    async fn load(&mut self) -> Result<Option<AccountJournal>, StorageError>;
    async fn save(&mut self, journal: &AccountJournal) -> Result<(), StorageError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageError;

/// Only physical forced-close confirmation qualifies, not async-write shutdown
/// or dropping a transport. An error/timeout must leave confirmation false.
#[allow(async_fn_in_trait)]
pub trait CloseControl {
    async fn close(&self) -> Result<(), CloseError>;
    fn confirmed_closed(&self) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseError;

/// Scheduling identities only: never store resolver output or credentials here.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountJournal {
    version: u32,
    config: AccountConfig,
    next_id: u32,
    #[serde(deserialize_with = "required_quarantine")]
    quarantined_secret: Option<String>,
    reservations: BTreeMap<u32, Reservation>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    revision: u64,
    secret_ref: String,
    class: WorkClass,
    expires_at_ms: u64,
}

fn required_quarantine<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

impl AccountJournal {
    fn valid(&self) -> bool {
        self.version == 1
            && self.config.valid()
            && self.next_id > 0
            && self.reservations.len() <= HARD_MAX_CONNS as usize
            && self
                .quarantined_secret
                .as_ref()
                .is_none_or(|s| s == &self.config.secret_ref)
            && self.reservations.iter().all(|(id, r)| {
                *id > 0
                    && *id < self.next_id
                    && r.revision > 0
                    && r.revision <= self.config.revision
                    && uuid_shaped(&r.secret_ref)
                    && r.expires_at_ms >= SESSION_TTL_MS
            })
    }
}

/// A reference only, never a lease guard: dropping/copying this releases nothing.
/// Account-bound and not deserializable; a persisted sequence prevents ABA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionId {
    sequence: u32,
    account: [u8; 36],
}

impl SessionId {
    fn new(sequence: u32, config: &AccountConfig) -> Self {
        let mut account = [0; 36];
        account.copy_from_slice(config.id.as_bytes());
        Self { sequence, account }
    }

    fn belongs_to(self, config: &AccountConfig) -> bool {
        self.account == config.id.as_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisorError {
    Storage,
    Corrupt,
    Config(ConfigError),
    Fenced,
    Unavailable(PoolError),
    UnknownSession,
    AlreadyStarted,
    NotStarted,
    Dial,
    CloseUnconfirmed,
    Refused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionResult {
    /// A local authorization/cancellation refusal says nothing about health.
    Aborted,
    Success,
    ProviderFailure,
    AuthFailure,
}

/// One owner-retained, reservation-bound completion latch. The operation can
/// publish a terminal result without acquiring the mutation mutex; dropping it
/// while waiting to finish cannot erase a known authentication failure. No new
/// entries or arbitrary session IDs can be manufactured through this handle.
#[derive(Clone, Default)]
pub struct SessionCompletion(Rc<Cell<Option<SessionResult>>>);

impl SessionCompletion {
    pub fn complete(&self, result: SessionResult) {
        if self.0.get().is_none() {
            self.0.set(Some(result));
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub retired: u32,
    /// All outstanding reservations, including active and recovery-fenced ones.
    pub retained: u32,
}

struct Held<C> {
    lease: Lease,
    error_epoch: u64,
    attempted: bool,
    // Terminal even if close is cancelled, fails, or the clock moves backwards.
    closing: bool,
    control: Option<C>,
    completion: SessionCompletion,
    completion_applied: bool,
}

pub struct AccountSupervisor<S, C> {
    storage: S,
    journal: AccountJournal,
    pool: AccountPool<()>,
    held: BTreeMap<u32, Held<C>>,
    // Also set BEFORE a persistence await. Cancelling that future must never
    // accidentally restore admission on the retained in-memory instance.
    poisoned: bool,
    recovery_fenced: bool,
    pending_result_write: bool,
    registry_missing: bool,
}

impl<S: AccountStorage, C: CloseControl> AccountSupervisor<S, C> {
    pub async fn open(mut storage: S, current: AccountConfig) -> Result<Self, SupervisorError> {
        if !current.valid() {
            return Err(SupervisorError::Config(ConfigError::Invalid));
        }
        let journal = match storage.load().await.map_err(|_| SupervisorError::Storage)? {
            Some(journal) => {
                if !journal.valid() {
                    return Err(SupervisorError::Corrupt);
                }
                journal
            }
            None => AccountJournal {
                version: 1,
                config: current.clone(),
                next_id: 1,
                quarantined_secret: None,
                reservations: BTreeMap::new(),
            },
        };
        let pool = AccountPool::new(journal.config.clone()).map_err(SupervisorError::Config)?;
        if journal.quarantined_secret.is_some() {
            pool.pool().borrow_mut().quarantine_auth();
        }
        let recovery_fenced = !journal.reservations.is_empty();
        let mut owner = Self {
            storage,
            journal,
            pool,
            held: BTreeMap::new(),
            poisoned: false,
            recovery_fenced,
            pending_result_write: false,
            registry_missing: false,
        };
        owner.reconcile(current).await?;
        Ok(owner)
    }

    async fn persist(&mut self) -> Result<(), SupervisorError> {
        self.poisoned = true;
        self.storage
            .save(&self.journal)
            .await
            .map_err(|_| SupervisorError::Storage)?;
        self.poisoned = false;
        Ok(())
    }

    /// Caller supplies a fresh authenticated registry snapshot before admission.
    /// No mutation can unpoison an uncertain storage write on this instance.
    pub async fn reconcile(&mut self, current: AccountConfig) -> Result<(), SupervisorError> {
        if self.poisoned {
            return Err(SupervisorError::Fenced);
        }
        self.pool
            .reconcile(current.clone())
            .map_err(SupervisorError::Config)?;
        if self.journal.config.secret_ref != current.secret_ref {
            self.journal.quarantined_secret = None;
        }
        self.journal.config = current;
        self.persist().await
    }

    pub fn candidate(&self, class: WorkClass) -> Candidate {
        let mut candidate = self.pool.candidate(class);
        if self.poisoned
            || self.recovery_fenced
            || self.registry_missing
            || self.pending_auth_failure()
        {
            candidate.eligible = false;
        }
        // Count recovered reservations and leases whose retirement write is
        // uncertain; neither may disappear from the scheduling projection.
        candidate.in_flight = self.outstanding();
        candidate
    }

    pub fn completion(&self, id: SessionId) -> Result<SessionCompletion, SupervisorError> {
        if !id.belongs_to(&self.journal.config) {
            return Err(SupervisorError::UnknownSession);
        }
        self.held
            .get(&id.sequence)
            .map(|h| h.completion.clone())
            .ok_or(SupervisorError::UnknownSession)
    }

    pub fn has_completions(&self) -> bool {
        self.pending_result_write
            || self
                .held
                .values()
                .any(|h| !h.completion_applied && h.completion.0.get().is_some())
    }

    fn pending_auth_failure(&self) -> bool {
        self.held.iter().any(|(id, h)| {
            h.completion.0.get() == Some(SessionResult::AuthFailure)
                && self
                    .journal
                    .reservations
                    .get(id)
                    .is_some_and(|r| r.secret_ref == self.journal.config.secret_ref)
        })
    }

    fn stage_completions(&mut self) {
        let pending: Vec<_> = self
            .held
            .iter()
            .filter_map(|(id, h)| {
                if h.completion_applied {
                    None
                } else {
                    h.completion.0.get().map(|result| (*id, result))
                }
            })
            .collect();
        for (sequence, result) in pending {
            let id = SessionId::new(sequence, &self.journal.config);
            if let Ok(dirty) = self.apply_result(id, result) {
                self.pending_result_write |= dirty;
            }
            let held = self.held.get_mut(&sequence).unwrap();
            held.completion_applied = true;
            held.closing = true;
        }
    }

    /// Authenticated registry absence is revocation, not a transient read error.
    /// It is terminal on this instance; only a fresh DO can recover membership.
    pub fn registry_missing(&mut self) {
        self.registry_missing = true;
    }

    pub async fn reserve(
        &mut self,
        class: WorkClass,
        now_ms: u64,
    ) -> Result<SessionId, SupervisorError> {
        if self.poisoned || self.recovery_fenced {
            return Err(SupervisorError::Fenced);
        }
        if self.registry_missing || self.pending_auth_failure() {
            return Err(SupervisorError::Refused);
        }
        let next_id = self
            .journal
            .next_id
            .checked_add(1)
            .ok_or(SupervisorError::Fenced)?;
        let expires_at_ms = now_ms
            .checked_add(SESSION_TTL_MS)
            .ok_or(SupervisorError::Refused)?;
        let lease = self
            .pool
            .pool()
            .borrow_mut()
            .try_acquire(class.lane())
            .map_err(SupervisorError::Unavailable)?;
        let id = self.journal.next_id;
        self.journal.next_id = next_id;
        self.journal.reservations.insert(
            id,
            Reservation {
                revision: self.journal.config.revision,
                secret_ref: self.journal.config.secret_ref.clone(),
                class,
                expires_at_ms,
            },
        );
        self.held.insert(
            id,
            Held {
                lease,
                error_epoch: self.pool.pool().borrow().provider_error_epoch(),
                attempted: false,
                closing: false,
                control: None,
                completion: SessionCompletion::default(),
                completion_applied: false,
            },
        );
        self.persist().await?;
        Ok(SessionId::new(id, &self.journal.config))
    }

    /// Check immediately before EACH AUTHINFO command. Runtime also cancels I/O
    /// and invokes close on deadline/revocation; this hook cannot retract writes.
    pub fn auth_permitted(&self, id: SessionId, proof_expires_at_ms: u64, now_ms: u64) -> bool {
        if self.poisoned
            || self.recovery_fenced
            || self.registry_missing
            || self.pending_auth_failure()
            || !id.belongs_to(&self.journal.config)
            || now_ms >= proof_expires_at_ms
        {
            return false;
        }
        let Some(reservation) = self.journal.reservations.get(&id.sequence) else {
            return false;
        };
        if self
            .held
            .get(&id.sequence)
            .is_none_or(|held| held.closing || held.completion.0.get().is_some())
        {
            return false;
        }
        let config = &self.journal.config;
        config.enabled
            && config.can_read
            && self.journal.quarantined_secret.is_none()
            && reservation.revision == config.revision
            && reservation.secret_ref == config.secret_ref
            && now_ms < reservation.expires_at_ms
            && (reservation.class != WorkClass::Ingest || config.ingest_limit > 0)
    }

    /// The factory must return independent close authority along with its setup
    /// future, before polling setup. It is invoked at most once per reservation.
    /// Even a factory error/panic is uncertain: that reservation cannot be
    /// cancelled as "never dialed". No retry is made here.
    pub fn start<F: Future, E>(
        &mut self,
        id: SessionId,
        proof_expires_at_ms: u64,
        now_ms: u64,
        factory: impl FnOnce() -> Result<(C, F), E>,
    ) -> Result<F, SupervisorError> {
        if !self.auth_permitted(id, proof_expires_at_ms, now_ms) {
            return Err(SupervisorError::Refused);
        }
        let held = self
            .held
            .get_mut(&id.sequence)
            .ok_or(SupervisorError::UnknownSession)?;
        if held.attempted {
            return Err(SupervisorError::AlreadyStarted);
        }
        held.attempted = true;
        held.error_epoch = self.pool.pool().borrow().provider_error_epoch();
        let (control, setup) = factory().map_err(|_| SupervisorError::Dial)?;
        held.control = Some(control);
        Ok(setup)
    }

    /// Cancel only a reservation whose factory has NEVER been invoked.
    pub async fn cancel_unstarted(&mut self, id: SessionId) -> Result<(), SupervisorError> {
        if self.poisoned || self.recovery_fenced {
            return Err(SupervisorError::Fenced);
        }
        if !id.belongs_to(&self.journal.config) {
            return Err(SupervisorError::UnknownSession);
        }
        let held = self
            .held
            .get(&id.sequence)
            .ok_or(SupervisorError::UnknownSession)?;
        if held.attempted {
            return Err(SupervisorError::AlreadyStarted);
        }
        self.retire(id).await
    }

    pub async fn record_result(
        &mut self,
        id: SessionId,
        result: SessionResult,
    ) -> Result<(), SupervisorError> {
        if self.apply_result(id, result)? {
            self.persist().await?;
        }
        Ok(())
    }

    fn apply_result(
        &mut self,
        id: SessionId,
        result: SessionResult,
    ) -> Result<bool, SupervisorError> {
        if self.poisoned || self.recovery_fenced {
            return Err(SupervisorError::Fenced);
        }
        if !id.belongs_to(&self.journal.config) {
            return Err(SupervisorError::UnknownSession);
        }
        let held = self
            .held
            .get(&id.sequence)
            .ok_or(SupervisorError::UnknownSession)?;
        if !held.attempted {
            return Err(SupervisorError::NotStarted);
        }
        let reservation = self
            .journal
            .reservations
            .get(&id.sequence)
            .ok_or(SupervisorError::UnknownSession)?;
        match result {
            SessionResult::Aborted => {}
            SessionResult::AuthFailure => {
                // Settings-only revisions must not forget a failed identity.
                // Conversely, an old failure must never quarantine new secrets.
                if reservation.secret_ref == self.journal.config.secret_ref {
                    self.journal.quarantined_secret = Some(reservation.secret_ref.clone());
                    self.pool.pool().borrow_mut().quarantine_auth();
                    return Ok(true);
                }
            }
            SessionResult::ProviderFailure => self
                .pool
                .pool()
                .borrow_mut()
                .record_lease_provider_error(&held.lease),
            SessionResult::Success => {
                if reservation.revision == self.journal.config.revision {
                    self.pool
                        .pool()
                        .borrow_mut()
                        .record_success_for_if_epoch_matches(held.lease.id, held.error_epoch);
                }
            }
        }
        Ok(false)
    }

    /// Finish under the owner's one mutation lock: apply quarantine before any
    /// await, then physically drain BEFORE waiting on storage. Keep capacity
    /// until both the result and retirement are durable. Cancellation anywhere
    /// leaves a terminal session for retained cleanup, never a retryable login.
    pub async fn finish(
        &mut self,
        id: SessionId,
        result: SessionResult,
    ) -> Result<(), SupervisorError> {
        self.completion(id)?.complete(result);
        self.stage_completions();
        if !id.belongs_to(&self.journal.config) {
            return Err(SupervisorError::UnknownSession);
        }
        let held = self
            .held
            .get_mut(&id.sequence)
            .ok_or(SupervisorError::UnknownSession)?;
        held.closing = true;
        let closed = match &held.control {
            Some(control) => control.close().await.is_ok() && control.confirmed_closed(),
            None => false,
        };
        if self.poisoned || self.recovery_fenced {
            return Err(SupervisorError::Fenced);
        }
        if self.pending_result_write {
            self.persist().await?;
            self.pending_result_write = false;
        }
        if !closed {
            return Err(SupervisorError::CloseUnconfirmed);
        }
        self.retire(id).await
    }

    /// Close is allowed even on a poisoned instance: drain physical sockets but
    /// never release any uncertain reservation. A close/storage error or cancelled
    /// close leaves the lease held. The runtime must bound the control's close.
    pub async fn close(&mut self, id: SessionId) -> Result<(), SupervisorError> {
        self.stage_completions();
        if !id.belongs_to(&self.journal.config) {
            return Err(SupervisorError::UnknownSession);
        }
        let held = self
            .held
            .get_mut(&id.sequence)
            .ok_or(SupervisorError::UnknownSession)?;
        held.closing = true;
        let control = held.control.as_ref().ok_or(SupervisorError::NotStarted)?;
        control
            .close()
            .await
            .map_err(|_| SupervisorError::CloseUnconfirmed)?;
        if !control.confirmed_closed() {
            return Err(SupervisorError::CloseUnconfirmed);
        }
        if self.poisoned || self.recovery_fenced {
            return Err(SupervisorError::Fenced);
        }
        self.retire(id).await
    }

    /// Drain expired/revoked sessions without fetching the registry. A runtime
    /// must invoke this on its deadline/revocation path even if requests or
    /// registry reads fail. Each control must bound its own close; all controls
    /// are polled together so one slow close cannot hide the others. Cancelling
    /// this sweep leaves every selected session terminal and its capacity held.
    /// Resolver proof expiry gates AUTHINFO, not an already authenticated session.
    pub async fn cleanup(&mut self, now_ms: u64) -> Result<CleanupReport, SupervisorError> {
        self.stage_completions();
        let invalid: Vec<u32> = self
            .held
            .keys()
            .copied()
            .filter(|sequence| {
                !self.auth_permitted(
                    SessionId::new(*sequence, &self.journal.config),
                    u64::MAX,
                    now_ms,
                )
            })
            .collect();
        // Fence ALL selected sessions before polling any close future.
        for sequence in invalid {
            self.held.get_mut(&sequence).unwrap().closing = true;
        }
        let retired: Vec<u32> = join_all(self.held.iter().filter(|(_, held)| held.closing).map(
            |(sequence, held)| async move {
                if !held.attempted {
                    return Some(*sequence);
                }
                let control = held.control.as_ref()?;
                if control.close().await.is_ok() && control.confirmed_closed() {
                    Some(*sequence)
                } else {
                    None
                }
            },
        ))
        .await
        .into_iter()
        .flatten()
        .collect();
        // A previous uncertain write may have removed journal entries already.
        // Drain controls anyway, but never infer safe capacity from that journal.
        if self.poisoned {
            return Err(SupervisorError::Fenced);
        }
        if self.recovery_fenced {
            return Ok(self.cleanup_report(0));
        }
        if retired.is_empty() {
            if self.pending_result_write {
                self.persist().await?;
                self.pending_result_write = false;
            }
            return Ok(self.cleanup_report(0));
        }
        for sequence in &retired {
            self.journal
                .reservations
                .remove(sequence)
                .ok_or(SupervisorError::UnknownSession)?;
        }
        self.persist().await?;
        self.pending_result_write = false;
        for sequence in &retired {
            let held = self
                .held
                .remove(sequence)
                .ok_or(SupervisorError::UnknownSession)?;
            self.pool.pool().borrow_mut().release(held.lease, None);
        }
        Ok(self.cleanup_report(retired.len() as u32))
    }

    /// Only live controls or safely cancellable unstarted reservations need a
    /// recurring wake. Recovered/uncertain occupancy stays fenced indefinitely,
    /// but must not create an endless alarm loop after physical sockets drain.
    pub fn needs_cleanup(&self) -> bool {
        (!self.poisoned && self.has_completions())
            || self.held.values().any(|held| match &held.control {
                Some(control) => !control.confirmed_closed(),
                None => !held.attempted && !self.poisoned,
            })
    }

    fn cleanup_report(&self, retired: u32) -> CleanupReport {
        CleanupReport {
            retired,
            retained: self.outstanding(),
        }
    }

    fn outstanding(&self) -> u32 {
        let held_only = self
            .held
            .keys()
            .filter(|sequence| !self.journal.reservations.contains_key(sequence))
            .count();
        (self.journal.reservations.len() + held_only) as u32
    }

    async fn retire(&mut self, id: SessionId) -> Result<(), SupervisorError> {
        self.journal
            .reservations
            .remove(&id.sequence)
            .ok_or(SupervisorError::UnknownSession)?;
        self.persist().await?;
        self.pending_result_write = false;
        let held = self
            .held
            .remove(&id.sequence)
            .ok_or(SupervisorError::UnknownSession)?;
        self.pool.pool().borrow_mut().release(held.lease, None);
        Ok(())
    }

    pub fn advance(&mut self, elapsed_ms: u64) {
        self.pool.pool().borrow_mut().advance(elapsed_ms);
    }
}

#[cfg(target_arch = "wasm32")]
impl CloseControl for nntune_core::transports::worker_pinned_tls::WorkerSocketControl {
    async fn close(&self) -> Result<(), CloseError> {
        self.close(std::time::Duration::from_secs(2))
            .await
            .map_err(|_| CloseError)
    }

    fn confirmed_closed(&self) -> bool {
        self.confirmed_closed()
    }
}
