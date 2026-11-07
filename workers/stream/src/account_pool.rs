//! Account-scoped admission for ADR 0014. The Durable Object owns one of
//! these per account, never per user. Registry snapshots are authoritative;
//! selection hints are not leases and admission is always checked again here.
//!
//! The runtime must force-close and confirm the physical socket before a
//! session's guard frees a slot. A TTL or dropped future is not closure
//! evidence. Quarantine must be persisted by the runtime against `secret_ref`,
//! so eviction cannot turn a bad login into a retry loop.

use std::cell::RefCell;
use std::future::{poll_fn, Future};
use std::task::Poll;

use serde::{Deserialize, Serialize};

use crate::pool::{AccountLimits, Lane, Pool};

/// Whole-session deadline, including dial, authentication and article/probe I/O.
pub const SESSION_TTL_MS: u64 = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkClass {
    Stream,
    Ingest,
}

impl WorkClass {
    pub fn lane(self) -> Lane {
        match self {
            Self::Stream => Lane::Playback,
            Self::Ingest => Lane::Ingest,
        }
    }
}

/// Scheduling projection of an admin registry row. No credentials or username.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub id: String,
    pub revision: u64,
    pub secret_ref: String,
    pub can_read: bool,
    pub max_connections: u32,
    pub stream_reserve: u32,
    pub ingest_limit: u32,
    pub tier: u32,
    pub enabled: bool,
}

pub(crate) fn uuid_shaped(raw: &str) -> bool {
    raw.len() == 36
        && raw.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

impl AccountConfig {
    fn limits(&self) -> AccountLimits {
        AccountLimits {
            max_connections: self.max_connections,
            // Non-reading accounts can have zero reserve in the registry, but
            // their disabled read pool still has a valid, non-borrowable reserve.
            stream_reserve: self.stream_reserve.max(1),
            ingest_limit: self.ingest_limit,
            enabled: self.enabled && self.can_read,
        }
    }

    pub fn valid(&self) -> bool {
        uuid_shaped(&self.id)
            && uuid_shaped(&self.secret_ref)
            && self.revision > 0
            && self.revision <= i64::MAX as u64
            && self.tier <= 100
            && self.limits().valid()
            && (!self.can_read || self.stream_reserve > 0)
            && (self.can_read || self.ingest_limit == 0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Invalid,
    Stale,
    Conflict,
}

pub struct AccountPool<C> {
    config: RefCell<AccountConfig>,
    pool: RefCell<Pool<C>>,
}

impl<C> AccountPool<C> {
    pub fn new(config: AccountConfig) -> Result<Self, ConfigError> {
        if !config.valid() {
            return Err(ConfigError::Invalid);
        }
        let mut pool = Pool::new(config.max_connections);
        if !pool.configure_account(config.limits()) {
            return Err(ConfigError::Invalid);
        }
        Ok(Self {
            config: RefCell::new(config),
            pool: RefCell::new(pool),
        })
    }

    /// Apply monotonic snapshots without replacing the pool or its guards.
    /// A late lookup cannot re-enable an account or resurrect old credentials.
    pub fn reconcile(&self, next: AccountConfig) -> Result<(), ConfigError> {
        if !next.valid() {
            return Err(ConfigError::Invalid);
        }
        let mut current = self.config.borrow_mut();
        if next.id != current.id {
            return Err(ConfigError::Conflict);
        }
        if next.revision < current.revision {
            return Err(ConfigError::Stale);
        }
        if next.revision == current.revision {
            return if next == *current {
                Ok(())
            } else {
                Err(ConfigError::Conflict)
            };
        }
        let mut pool = self.pool.borrow_mut();
        if !pool.configure_account(next.limits()) {
            return Err(ConfigError::Invalid);
        }
        if next.secret_ref != current.secret_ref {
            pool.credentials_rotated();
        } else {
            pool.invalidate_sessions();
        }
        *current = next;
        Ok(())
    }

    pub fn pool(&self) -> &RefCell<Pool<C>> {
        &self.pool
    }

    pub fn candidate(&self, class: WorkClass) -> Candidate {
        let config = self.config.borrow();
        let pool = self.pool.borrow();
        Candidate {
            id: config.id.clone(),
            tier: config.tier,
            in_flight: pool.in_use(),
            eligible: pool.can_acquire(class.lane()),
        }
    }
}

/// Hint returned by an account owner, used only to rank the order of
/// authoritative acquire attempts. Stale/racing hints cannot exceed a cap.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub id: String,
    pub tier: u32,
    pub in_flight: u32,
    pub eligible: bool,
}

pub fn ranked_candidates(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates.retain(|c| c.eligible);
    candidates.sort_by(|a, b| (a.tier, a.in_flight, &a.id).cmp(&(b.tier, b.in_flight, &b.id)));
    candidates
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionExpired;

/// Race an operation against its deadline, dropping the losing future. This
/// does NOT prove physical socket closure. A Worker supervisor must retain
/// close authority AND the account lease outside this future, force-close on
/// every exit and await confirmed closure before releasing capacity. Never
/// place a Worker account lease inside `operation` and rely on its Drop alone.
/// Runtime supplies Worker Delay; native tests use a short Tokio timer.
pub async fn with_session_deadline<F, D>(
    operation: F,
    deadline: D,
) -> Result<F::Output, SessionExpired>
where
    F: Future,
    D: Future<Output = ()>,
{
    let mut operation = Box::pin(operation);
    let mut deadline = Box::pin(deadline);
    poll_fn(|cx| {
        if deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(SessionExpired));
        }
        operation.as_mut().poll(cx).map(Ok)
    })
    .await
}
