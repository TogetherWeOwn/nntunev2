//! Per-user NNTP connection pool: the state machine that lives inside one
//! Durable Object instance per user.
//!
//! - Capacity is the user's provider connection cap (BYOP settings ∩ plan).
//! - [`Pool::try_acquire`] hands out [`Lease`]s; released connections stay
//!   warm so a steady play stream never reconnects per play.
//! - [`Lane`] prioritises playback over prefetch over ingest when contended.
//! - Provider errors drive exponential quarantine/backoff; success resets it.
//!
//! Time is a virtual millisecond clock ([`Pool::advance`]) so tests are
//! deterministic; the Durable Object drives it with wall-clock time.

/// Priority lane. Ordering is priority order: `Playback < Prefetch < Ingest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    Playback,
    Prefetch,
    Ingest,
}

/// A held pool connection. Drop it back via [`Pool::release`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    pub id: u64,
    pub lane: Lane,
}

/// Why an acquire failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolError {
    /// Pool is quarantined after provider errors; retry after the backoff.
    Quarantined { retry_after_ms: u64 },
    /// All connections are leased out.
    Exhausted,
}

/// Absolute ceiling: no user may hold more provider connections than this,
/// whatever their plan claims.
pub const HARD_MAX_CONNS: u32 = 50;

/// Effective connection cap: the lower of the provider cap (BYOP settings)
/// and the plan cap, clamped into `[1, HARD_MAX_CONNS]`.
pub fn effective_cap(provider_cap: u32, plan_cap: u32) -> u32 {
    provider_cap.min(plan_cap).clamp(1, HARD_MAX_CONNS)
}

/// Backoff after `consecutive_errors` provider failures: 1s, 5s, 30s, then
/// 5min capped.
pub fn backoff_ms(consecutive_errors: u32) -> u64 {
    match consecutive_errors {
        0 => 0,
        1 => 1_000,
        2 => 5_000,
        3 => 30_000,
        _ => 300_000,
    }
}

pub struct Pool {
    capacity: u32,
    in_use: u32,
    warm_idle: u32,
    connects: u64,
    reuses: u64,
    next_id: u64,
    consecutive_errors: u32,
    quarantined_until_ms: Option<u64>,
    now_ms: u64,
    pending: Vec<Lane>,
}

impl Pool {
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity: capacity.clamp(1, HARD_MAX_CONNS),
            in_use: 0,
            warm_idle: 0,
            connects: 0,
            reuses: 0,
            next_id: 0,
            consecutive_errors: 0,
            quarantined_until_ms: None,
            now_ms: 0,
            pending: Vec::new(),
        }
    }

    /// Try to lease one connection on `lane`.
    pub fn try_acquire(&mut self, lane: Lane) -> Result<Lease, PoolError> {
        if let Some(until) = self.quarantined_until_ms {
            if self.now_ms < until {
                return Err(PoolError::Quarantined {
                    retry_after_ms: until - self.now_ms,
                });
            }
        }
        if self.in_use >= self.capacity {
            return Err(PoolError::Exhausted);
        }
        let lease = Lease {
            id: self.next_id,
            lane,
        };
        self.next_id += 1;
        if self.warm_idle > 0 {
            self.warm_idle -= 1;
            self.reuses += 1;
        } else {
            self.connects += 1;
        }
        self.in_use += 1;
        Ok(lease)
    }

    /// Return a lease; the connection stays warm for the next acquire.
    pub fn release(&mut self, lease: Lease) {
        debug_assert!(self.in_use > 0, "released without a matching acquire");
        let _ = lease;
        self.in_use = self.in_use.saturating_sub(1);
        self.warm_idle += 1;
    }

    /// A provider call failed: count it and (re)start quarantine backoff.
    pub fn record_provider_error(&mut self) {
        self.consecutive_errors = self.consecutive_errors.saturating_add(1);
        let backoff = backoff_ms(self.consecutive_errors);
        self.quarantined_until_ms = Some(self.now_ms.saturating_add(backoff));
    }

    /// A provider call succeeded: clear errors and quarantine.
    pub fn record_success(&mut self) {
        self.consecutive_errors = 0;
        self.quarantined_until_ms = None;
    }

    /// Advance the virtual clock (test seam; the DO uses wall-clock time).
    pub fn advance(&mut self, ms: u64) {
        self.now_ms = self.now_ms.saturating_add(ms);
    }

    /// Queue a waiter on `lane` for priority scheduling.
    pub fn queue(&mut self, lane: Lane) {
        self.pending.push(lane);
    }

    /// Pop the highest-priority queued waiter, if any.
    pub fn next_pending_lane(&mut self) -> Option<Lane> {
        let best = self.pending.iter().enumerate().min_by_key(|(_, l)| **l)?.0;
        Some(self.pending.remove(best))
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn in_use(&self) -> u32 {
        self.in_use
    }

    /// Physical NNTP connections opened so far.
    pub fn connects(&self) -> u64 {
        self.connects
    }

    /// Acquires served from a warm connection (no reconnect).
    pub fn reuses(&self) -> u64 {
        self.reuses
    }

    pub fn quarantined(&self) -> bool {
        match self.quarantined_until_ms {
            Some(until) => self.now_ms < until,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_cap_takes_the_minimum() {
        assert_eq!(effective_cap(10, 4), 4);
        assert_eq!(effective_cap(2, 30), 2);
    }

    #[test]
    fn effective_cap_clamps_to_sane_bounds() {
        assert_eq!(effective_cap(0, 0), 1);
        assert_eq!(effective_cap(500, 900), HARD_MAX_CONNS);
    }

    #[test]
    fn warm_acquire_does_not_reconnect() {
        let mut pool = Pool::new(4);
        let lease = pool.try_acquire(Lane::Playback).expect("first acquire");
        assert_eq!(pool.connects(), 1);
        pool.release(lease);
        let lease2 = pool.try_acquire(Lane::Playback).expect("second acquire");
        assert_eq!(pool.connects(), 1, "warm pool must not reconnect");
        assert_eq!(pool.reuses(), 1);
        pool.release(lease2);
    }

    #[test]
    fn exhausted_at_capacity() {
        let mut pool = Pool::new(1);
        let lease = pool.try_acquire(Lane::Playback).expect("acquire");
        assert_eq!(pool.try_acquire(Lane::Prefetch), Err(PoolError::Exhausted));
        pool.release(lease);
    }

    #[test]
    fn playback_beats_prefetch_beats_ingest_when_queued() {
        let mut pool = Pool::new(1);
        pool.queue(Lane::Ingest);
        pool.queue(Lane::Prefetch);
        pool.queue(Lane::Playback);
        assert_eq!(pool.next_pending_lane(), Some(Lane::Playback));
        assert_eq!(pool.next_pending_lane(), Some(Lane::Prefetch));
        assert_eq!(pool.next_pending_lane(), Some(Lane::Ingest));
        assert_eq!(pool.next_pending_lane(), None);
    }

    #[test]
    fn provider_errors_quarantine_with_exponential_backoff() {
        let mut pool = Pool::new(2);
        pool.record_provider_error();
        assert!(pool.quarantined());
        let err = pool.try_acquire(Lane::Playback).expect_err("quarantined");
        assert_eq!(
            err,
            PoolError::Quarantined {
                retry_after_ms: backoff_ms(1)
            }
        );
        assert_eq!(backoff_ms(1), 1_000);
        assert_eq!(backoff_ms(2), 5_000);
        assert_eq!(backoff_ms(3), 30_000);
        assert_eq!(backoff_ms(4), 300_000);
        assert_eq!(backoff_ms(99), 300_000);
        // Backoff has not elapsed yet: still quarantined.
        pool.advance(backoff_ms(1) - 1);
        assert!(pool.quarantined());
        // After the backoff elapses a second error backs off further.
        pool.advance(1);
        pool.record_provider_error();
        let err = pool.try_acquire(Lane::Playback).expect_err("quarantined");
        assert_eq!(
            err,
            PoolError::Quarantined {
                retry_after_ms: backoff_ms(2)
            }
        );
    }

    #[test]
    fn success_clears_quarantine() {
        let mut pool = Pool::new(2);
        pool.record_provider_error();
        assert!(pool.quarantined());
        pool.record_success();
        assert!(!pool.quarantined());
        pool.try_acquire(Lane::Playback)
            .expect("acquire after reset");
    }
}
