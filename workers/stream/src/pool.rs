//! Per-user NNTP connection pool: the state machine that lives inside one
//! Durable Object instance per user.
//!
//! - Capacity is the user's provider connection cap (BYOP settings ∩ plan).
//! - [`Pool::try_acquire`] hands out [`Lease`]s. The pool *owns* the idle
//!   connections (`C`, an [`NntpClient`](crate::nntp::NntpClient) in the play
//!   path): [`Pool::release`] takes the connection back, [`Pool::take_idle`]
//!   hands it to the next play, so a steady stream never reconnects per play.
//! - [`Lane`]s are enforced at acquire time: playback may use every
//!   connection; prefetch and ingest must leave one free for playback.
//! - Provider errors drive a circuit breaker ([`CIRCUIT_FAILURE_THRESHOLD`]
//!   consecutive failures inside [`CIRCUIT_WINDOW_MS`] opens it for
//!   [`CIRCUIT_OPEN_MS`], then a single half-open probe); success closes it.
//!
//! Time is a virtual millisecond clock ([`Pool::advance`]) so tests are
//! deterministic. The embedding Durable Object must advance it from wall-clock
//! deltas; this crate has no runtime loop that does so automatically.
//!
//! Leases are drop-safe: the slot count lives in shared state behind the
//! [`Lease`]'s guard, so a play that is dropped without `release` (client
//! disconnect, early `?`, panic unwind) still frees its slot. Only an
//! explicit [`Pool::release`] parks the connection back warm — a dropped
//! stream's connection is discarded, because its read position is unknown
//! and re-parking it could attribute one article's bytes to another play.

use std::sync::{Arc, Mutex};

/// Priority lane. Ordering is priority order: `Playback < Prefetch < Ingest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    Playback,
    Prefetch,
    Ingest,
}

/// A held pool connection. Hand it back via [`Pool::release`] (which may
/// re-park the connection warm); dropping it frees the slot but discards
/// the connection. `Lease` is not `Copy`/`Clone`: the guard must have a
/// single owner so the slot is released exactly once.
#[derive(Debug)]
pub struct Lease {
    pub id: u64,
    pub lane: Lane,
    guard: Option<SlotGuard>,
}

/// Metering owed to a play that ended without `finalize` (dropped stream,
/// `abort`, on either the miss or the hit path). The play path pushes these
/// into pool-level shared state; the pool owner drains them via
/// [`Pool::take_orphaned`] and emits one `BytesServed` event each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrphanedPlay {
    pub user_id: String,
    pub track_id: String,
    pub bytes: u64,
}

/// Slot bookkeeping shared between the pool and outstanding leases, so a
/// dropped lease still frees its slot.
#[derive(Debug, Default)]
struct SlotState {
    in_use: u32,
    probe_in_flight: Option<u64>,
}

#[derive(Debug)]
struct SlotGuard {
    /// `None` once [`Pool::release`] took the slot back explicitly; `Drop`
    /// only acts on a still-armed guard.
    slots: Option<Arc<Mutex<SlotState>>>,
    id: u64,
}

impl SlotGuard {
    /// Disarm: the pool took the slot back, so dropping this guard is a no-op.
    fn disarm(mut self) {
        self.slots.take();
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        // A lock failure means another thread panicked while holding it;
        // skip rather than panic inside a destructor.
        if let Some(slots) = self.slots.take() {
            if let Ok(mut state) = slots.lock() {
                state.in_use = state.in_use.saturating_sub(1);
                if state.probe_in_flight == Some(self.id) {
                    state.probe_in_flight = None;
                }
            }
        }
    }
}

/// Why an acquire failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolError {
    /// The circuit breaker is open, a half-open playback probe is already
    /// in flight, or the probe cannot run yet; retry after `retry_after_ms`.
    /// The play path maps this to `503` with a `Retry-After` header.
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

/// Circuit-breaker tuning (see the crate README for the documented values).
///
/// The breaker opens after [`CIRCUIT_FAILURE_THRESHOLD`] consecutive provider
/// failures whose first-to-last span fits inside [`CIRCUIT_WINDOW_MS`]; while
/// open every acquire fails with `Retry-After: CIRCUIT_OPEN_MS`. Once the open
/// interval elapses the breaker is half-open: exactly one probe acquire is
/// admitted. A provider response closes it; a provider failure re-opens it.
pub const CIRCUIT_FAILURE_THRESHOLD: u32 = 3;
/// Failures whose first-to-last span exceeds this window count as a fresh
/// streak (the count restarts at 1).
pub const CIRCUIT_WINDOW_MS: u64 = 30_000;
/// How long the breaker stays open before admitting a half-open probe.
pub const CIRCUIT_OPEN_MS: u64 = 30_000;
/// Retry hint while the single half-open probe is still running.
pub const HALF_OPEN_PROBE_RETRY_MS: u64 = 1_000;

/// Circuit-breaker state, derived from the failure count and the virtual clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

pub struct Pool<C> {
    capacity: u32,
    slots: Arc<Mutex<SlotState>>,
    /// `BytesServed` records for plays that ended without `finalize`
    /// (dropped streams, `abort`), on either the miss or the hit path.
    /// Pool-level (not on the lease) so a `Cached` stream — which holds no
    /// lease — can record its served bytes too. Drained by the pool owner
    /// (which holds the meter) via [`Pool::take_orphaned`].
    orphaned: Arc<Mutex<Vec<OrphanedPlay>>>,
    idle: Vec<C>,
    connects: u64,
    reuses: u64,
    next_id: u64,
    consecutive_errors: u32,
    /// Virtual time of the first failure in the current streak; `None` when
    /// the streak is empty. Failures older than [`CIRCUIT_WINDOW_MS`] restart
    /// the streak instead of extending it.
    window_start_ms: Option<u64>,
    quarantined_until_ms: Option<u64>,
    now_ms: u64,
}

impl<C> Pool<C> {
    pub fn new(capacity: u32) -> Self {
        Self {
            capacity: capacity.clamp(1, HARD_MAX_CONNS),
            slots: Arc::new(Mutex::new(SlotState::default())),
            orphaned: Arc::new(Mutex::new(Vec::new())),
            idle: Vec::new(),
            connects: 0,
            reuses: 0,
            next_id: 0,
            consecutive_errors: 0,
            window_start_ms: None,
            quarantined_until_ms: None,
            now_ms: 0,
        }
    }

    /// Fail closed on a poisoned lock: report full so no new lease is issued.
    fn in_use_locked(&self) -> u32 {
        self.slots.lock().map(|s| s.in_use).unwrap_or(u32::MAX)
    }

    /// Slots a lane must leave free for playback: none for playback itself,
    /// one for prefetch and ingest.
    fn reserve(lane: Lane) -> u32 {
        match lane {
            Lane::Playback => 0,
            Lane::Prefetch | Lane::Ingest => 1,
        }
    }

    /// Would [`Pool::try_acquire`] succeed on `lane` right now?
    pub fn can_acquire(&self, lane: Lane) -> bool {
        let circuit_state = self.circuit_state();
        if circuit_state == CircuitState::Open {
            return false;
        }
        let Ok(state) = self.slots.lock() else {
            return false;
        };
        if circuit_state == CircuitState::HalfOpen
            && (lane != Lane::Playback || state.probe_in_flight.is_some())
        {
            return false;
        }
        state.in_use + Self::reserve(lane) < self.capacity
    }

    /// Try to lease one connection slot on `lane`.
    ///
    /// While the breaker is open this fails with [`PoolError::Quarantined`]
    /// (the play path maps that to `503` + `Retry-After`) without dialing out.
    /// While half-open exactly one probe is admitted, tracked by its lease id;
    /// further acquires get a non-zero retry hint while that probe runs.
    pub fn try_acquire(&mut self, lane: Lane) -> Result<Lease, PoolError> {
        let is_probe = if let Some(until) = self.quarantined_until_ms {
            if self.now_ms < until {
                return Err(PoolError::Quarantined {
                    retry_after_ms: until - self.now_ms,
                });
            }
            self.consecutive_errors >= CIRCUIT_FAILURE_THRESHOLD
        } else {
            false
        };
        let mut state = match self.slots.lock() {
            Ok(state) => state,
            Err(_) => return Err(PoolError::Exhausted),
        };
        if is_probe && (lane != Lane::Playback || state.probe_in_flight.is_some()) {
            return Err(PoolError::Quarantined {
                retry_after_ms: HALF_OPEN_PROBE_RETRY_MS,
            });
        }
        if state.in_use + Self::reserve(lane) >= self.capacity {
            return if is_probe {
                Err(PoolError::Quarantined {
                    retry_after_ms: HALF_OPEN_PROBE_RETRY_MS,
                })
            } else {
                Err(PoolError::Exhausted)
            };
        }
        let id = self.next_id;
        let lease = Lease {
            id,
            lane,
            guard: Some(SlotGuard {
                slots: Some(Arc::clone(&self.slots)),
                id,
            }),
        };
        self.next_id += 1;
        state.in_use += 1;
        if is_probe {
            state.probe_in_flight = Some(id);
        }
        Ok(lease)
    }

    /// Take a warm connection for a fresh lease, if one is idle.
    pub fn take_idle(&mut self) -> Option<C> {
        self.idle.pop()
    }

    /// A warm connection passed peer and timeout validation for reuse.
    pub(crate) fn record_reuse(&mut self) {
        self.reuses += 1;
    }

    /// The caller dialed a new physical connection.
    pub fn record_connect(&mut self) {
        self.connects += 1;
    }

    /// Return a lease. `conn` is the still-healthy connection to keep warm;
    /// pass `None` after an I/O failure so the dead connection is dropped.
    /// Dropping the lease without `release` also frees the slot (via the
    /// guard) but never re-parks the connection.
    pub fn release(&mut self, mut lease: Lease, conn: Option<C>) {
        if let Some(guard) = lease.guard.take() {
            guard.disarm();
            if let Ok(mut state) = self.slots.lock() {
                debug_assert!(state.in_use > 0, "released without a matching acquire");
                state.in_use = state.in_use.saturating_sub(1);
                if state.probe_in_flight == Some(lease.id) {
                    state.probe_in_flight = None;
                }
            }
        }
        if let Some(conn) = conn {
            // Never hold more warm connections than the cap allows.
            if (self.idle.len() as u32) < self.capacity {
                self.idle.push(conn);
            }
        }
    }

    /// Stash metering for a play that ended without `finalize` (dropped
    /// stream, `abort`). A lock failure (another thread panicked while
    /// holding it) drops the record rather than panicking the caller.
    pub fn push_orphan(&mut self, play: OrphanedPlay) {
        if let Ok(mut orphaned) = self.orphaned.lock() {
            orphaned.push(play);
        }
    }

    /// Shared handle to the orphan queue, for streams that must record
    /// metering on `Drop` without holding the pool (in particular `Cached`
    /// streams, which hold no lease).
    pub fn orphan_sink(&self) -> Arc<Mutex<Vec<OrphanedPlay>>> {
        Arc::clone(&self.orphaned)
    }

    /// Metering owed to plays that ended without `finalize`.
    /// The pool owner (which holds the meter) drains these and emits
    /// `BytesServed` for each; called from `play` and `finalize`.
    pub fn take_orphaned(&mut self) -> Vec<OrphanedPlay> {
        self.orphaned
            .lock()
            .map(|mut orphaned| std::mem::take(&mut *orphaned))
            .unwrap_or_default()
    }

    /// A provider call failed: extend the streak (restarting it when the
    /// previous failures are older than [`CIRCUIT_WINDOW_MS`]) and open the
    /// breaker for [`CIRCUIT_OPEN_MS`] once the streak reaches
    /// [`CIRCUIT_FAILURE_THRESHOLD`]. A failure in half-open re-opens for a
    /// fresh interval without resetting the streak.
    pub fn record_provider_error(&mut self) {
        if self.circuit_state() == CircuitState::HalfOpen {
            self.reopen_after_probe();
            return;
        }
        self.record_failure_in_window();
    }

    /// Record a provider failure associated with a lease. When that lease is
    /// the half-open probe, re-open immediately instead of applying the
    /// original failure-window reset.
    pub fn record_provider_error_for(&mut self, lease_id: u64) {
        let is_probe = self.clear_probe(lease_id);
        if is_probe {
            self.reopen_after_probe();
        } else {
            self.record_provider_error();
        }
    }

    /// A non-provider result still proves that the half-open probe reached the
    /// provider (for example, an article-missing or decode response).
    pub fn record_non_provider_result_for(&mut self, lease_id: u64) {
        if self.probe_in_flight() == Some(lease_id) {
            self.record_success();
        }
    }

    fn record_failure_in_window(&mut self) {
        if let Some(start) = self.window_start_ms {
            if self.now_ms.saturating_sub(start) > CIRCUIT_WINDOW_MS {
                self.consecutive_errors = 0;
                self.window_start_ms = None;
            }
        }
        if self.consecutive_errors == 0 {
            self.window_start_ms = Some(self.now_ms);
        }
        self.consecutive_errors = self.consecutive_errors.saturating_add(1);
        if self.consecutive_errors >= CIRCUIT_FAILURE_THRESHOLD {
            self.quarantined_until_ms = Some(self.now_ms.saturating_add(CIRCUIT_OPEN_MS));
        }
    }

    fn reopen_after_probe(&mut self) {
        self.consecutive_errors = self.consecutive_errors.max(CIRCUIT_FAILURE_THRESHOLD);
        self.quarantined_until_ms = Some(self.now_ms.saturating_add(CIRCUIT_OPEN_MS));
    }

    fn clear_probe(&self, lease_id: u64) -> bool {
        let Ok(mut state) = self.slots.lock() else {
            return false;
        };
        if state.probe_in_flight == Some(lease_id) {
            state.probe_in_flight = None;
            true
        } else {
            false
        }
    }

    fn probe_in_flight(&self) -> Option<u64> {
        self.slots.lock().ok()?.probe_in_flight
    }

    /// A provider call succeeded: clear the streak and close the breaker
    /// (including after a half-open probe).
    pub fn record_success(&mut self) {
        self.consecutive_errors = 0;
        self.window_start_ms = None;
        self.quarantined_until_ms = None;
        if let Ok(mut state) = self.slots.lock() {
            state.probe_in_flight = None;
        }
    }

    /// A provider call succeeded on `lease_id`. Only the admitted half-open
    /// probe closes the breaker; a stale lease's success clears a
    /// closed-state streak but can never close an open breaker or release
    /// another play's probe.
    pub fn record_success_for(&mut self, lease_id: u64) {
        if self.probe_in_flight() == Some(lease_id) {
            self.record_success();
        } else if self.quarantined_until_ms.is_none() {
            self.consecutive_errors = 0;
            self.window_start_ms = None;
        }
    }

    /// Advance the virtual clock by elapsed milliseconds. The embedding
    /// runtime must call this with wall-clock deltas before time-based pool
    /// operations; the pool does not advance time automatically.
    pub fn advance(&mut self, ms: u64) {
        self.now_ms = self.now_ms.saturating_add(ms);
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn in_use(&self) -> u32 {
        self.in_use_locked()
    }

    /// Warm connections currently parked in the pool.
    pub fn idle(&self) -> usize {
        self.idle.len()
    }

    /// Physical NNTP connections opened so far.
    pub fn connects(&self) -> u64 {
        self.connects
    }

    /// First-segment fetches served from a validated warm connection.
    pub fn reuses(&self) -> u64 {
        self.reuses
    }

    pub fn quarantined(&self) -> bool {
        match self.quarantined_until_ms {
            Some(until) => self.now_ms < until,
            None => false,
        }
    }

    /// Current breaker state: open while the quarantine interval runs,
    /// half-open once it has elapsed with the failure streak still at
    /// threshold (exactly one probe acquire is admitted), closed otherwise.
    pub fn circuit_state(&self) -> CircuitState {
        match self.quarantined_until_ms {
            Some(until) if self.now_ms < until => CircuitState::Open,
            Some(_) if self.consecutive_errors >= CIRCUIT_FAILURE_THRESHOLD => {
                CircuitState::HalfOpen
            }
            _ => CircuitState::Closed,
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

    type TestPool = Pool<u32>;

    #[test]
    fn warm_connection_is_handed_back_not_redialed() {
        let mut pool = TestPool::new(4);
        let lease = pool.try_acquire(Lane::Playback).expect("first acquire");
        assert_eq!(pool.take_idle(), None, "nothing warm yet");
        pool.record_connect();
        pool.release(lease, Some(7));
        assert_eq!(pool.idle(), 1);
        let lease2 = pool.try_acquire(Lane::Playback).expect("second acquire");
        assert_eq!(pool.take_idle(), Some(7), "same connection comes back");
        assert_eq!(pool.reuses(), 0, "take alone is not a successful reuse");
        pool.record_reuse();
        assert_eq!((pool.connects(), pool.reuses()), (1, 1));
        pool.release(lease2, Some(7));
    }

    #[test]
    fn dropping_a_lease_without_release_still_frees_its_slot() {
        let mut pool = TestPool::new(1);
        {
            let _lease = pool.try_acquire(Lane::Playback).expect("acquire");
            assert_eq!(pool.in_use(), 1);
            // Dropped here without `release`.
        }
        assert_eq!(pool.in_use(), 0);
        pool.try_acquire(Lane::Playback)
            .expect("slot freed by drop");
    }

    #[test]
    fn failed_connection_is_dropped_not_parked() {
        let mut pool = TestPool::new(2);
        let lease = pool.try_acquire(Lane::Playback).expect("acquire");
        pool.release(lease, None);
        assert_eq!(pool.idle(), 0);
        assert_eq!(pool.in_use(), 0);
    }

    #[test]
    fn exhausted_at_capacity() {
        let mut pool = TestPool::new(1);
        let lease = pool.try_acquire(Lane::Playback).expect("acquire");
        assert!(matches!(
            pool.try_acquire(Lane::Playback),
            Err(PoolError::Exhausted)
        ));
        pool.release(lease, None);
    }

    #[test]
    fn prefetch_and_ingest_always_leave_a_slot_for_playback() {
        let mut pool = TestPool::new(1);
        assert!(matches!(
            pool.try_acquire(Lane::Prefetch),
            Err(PoolError::Exhausted)
        ));
        assert!(matches!(
            pool.try_acquire(Lane::Ingest),
            Err(PoolError::Exhausted)
        ));
        assert!(!pool.can_acquire(Lane::Prefetch));
        pool.try_acquire(Lane::Playback)
            .expect("playback still gets the only slot");

        let mut pool = TestPool::new(2);
        let prefetch = pool.try_acquire(Lane::Prefetch).expect("one prefetch");
        assert!(matches!(
            pool.try_acquire(Lane::Prefetch),
            Err(PoolError::Exhausted)
        ));
        assert!(matches!(
            pool.try_acquire(Lane::Ingest),
            Err(PoolError::Exhausted)
        ));
        pool.try_acquire(Lane::Playback)
            .expect("playback takes the reserved slot");
        pool.release(prefetch, None);
    }

    #[test]
    fn capped_pool_never_parks_more_than_capacity() {
        let mut pool = TestPool::new(1);
        let a = pool.try_acquire(Lane::Playback).expect("a");
        pool.release(a, Some(1));
        let b = pool.try_acquire(Lane::Playback).expect("b");
        pool.release(b, Some(2));
        assert_eq!(pool.idle(), 1);
    }

    #[test]
    fn open_breaker_holds_for_the_open_interval_then_admits_a_probe() {
        use super::CIRCUIT_OPEN_MS;
        let mut pool = TestPool::new(2);
        for _ in 0..super::CIRCUIT_FAILURE_THRESHOLD {
            pool.record_provider_error();
        }
        assert!(pool.quarantined());
        let err = pool.try_acquire(Lane::Playback).expect_err("quarantined");
        assert_eq!(
            err,
            PoolError::Quarantined {
                retry_after_ms: CIRCUIT_OPEN_MS
            }
        );
        pool.advance(CIRCUIT_OPEN_MS - 1);
        assert!(pool.quarantined());
        assert_eq!(pool.circuit_state(), super::CircuitState::Open);
        pool.advance(1);
        assert!(!pool.quarantined());
        assert_eq!(pool.circuit_state(), super::CircuitState::HalfOpen);
        let probe = pool.try_acquire(Lane::Playback).expect("probe admitted");

        // A failed probe after the original window expired still re-opens.
        pool.advance(1);
        pool.record_provider_error_for(probe.id);
        pool.release(probe, None);
        assert_eq!(pool.circuit_state(), super::CircuitState::Open);
        let err = pool
            .try_acquire(Lane::Playback)
            .expect_err("failed probe keeps the breaker open");
        assert_eq!(
            err,
            PoolError::Quarantined {
                retry_after_ms: CIRCUIT_OPEN_MS
            }
        );
    }

    #[test]
    fn half_open_gate_tracks_the_probe_not_unrelated_leases() {
        use super::{CIRCUIT_OPEN_MS, HALF_OPEN_PROBE_RETRY_MS};
        let mut pool = TestPool::new(3);
        let existing = pool.try_acquire(Lane::Playback).expect("existing lease");
        for _ in 0..super::CIRCUIT_FAILURE_THRESHOLD {
            pool.record_provider_error();
        }
        pool.advance(CIRCUIT_OPEN_MS);

        {
            let probe = pool.try_acquire(Lane::Playback).expect("probe admitted");
            assert_eq!(pool.in_use(), 2, "existing lease does not block probe");
            let err = pool
                .try_acquire(Lane::Playback)
                .expect_err("only one half-open probe may be in flight");
            assert_eq!(
                err,
                PoolError::Quarantined {
                    retry_after_ms: HALF_OPEN_PROBE_RETRY_MS
                }
            );
            assert!(!pool.can_acquire(Lane::Playback));
            drop(probe);
        }

        let next_probe = pool
            .try_acquire(Lane::Playback)
            .expect("dropping the probe clears its in-flight marker");
        pool.record_success();
        pool.release(next_probe, None);
        pool.release(existing, None);
    }

    #[test]
    fn half_open_probe_waits_for_capacity_with_a_retry_hint() {
        use super::{CIRCUIT_OPEN_MS, HALF_OPEN_PROBE_RETRY_MS};

        let mut pool = TestPool::new(1);
        let existing = pool.try_acquire(Lane::Playback).expect("existing lease");
        for _ in 0..super::CIRCUIT_FAILURE_THRESHOLD {
            pool.record_provider_error();
        }
        pool.advance(CIRCUIT_OPEN_MS);

        assert!(!pool.can_acquire(Lane::Playback));
        assert_eq!(
            pool.try_acquire(Lane::Playback)
                .expect_err("full pool cannot run the probe"),
            PoolError::Quarantined {
                retry_after_ms: HALF_OPEN_PROBE_RETRY_MS
            }
        );

        pool.release(existing, None);
        let probe = pool
            .try_acquire(Lane::Playback)
            .expect("probe runs once capacity is available");
        pool.record_success();
        pool.release(probe, None);
    }

    #[test]
    fn expired_failure_window_restarts_then_counts_the_new_streak() {
        use super::{CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_WINDOW_MS};
        let mut pool = TestPool::new(2);
        pool.record_provider_error();
        pool.record_provider_error();
        pool.advance(CIRCUIT_WINDOW_MS + 1);
        pool.record_provider_error();
        assert!(!pool.quarantined(), "the stale streak restarts at one");
        pool.record_provider_error();
        assert!(!pool.quarantined());
        pool.record_provider_error();
        assert_eq!(CIRCUIT_FAILURE_THRESHOLD, 3);
        assert!(pool.quarantined(), "three new-window failures open");
    }

    #[test]
    fn breaker_opens_only_after_threshold_failures_inside_window() {
        use super::{CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_OPEN_MS, CIRCUIT_WINDOW_MS};
        assert_eq!(CIRCUIT_FAILURE_THRESHOLD, 3);
        assert_eq!(CIRCUIT_WINDOW_MS, 30_000);
        assert_eq!(CIRCUIT_OPEN_MS, 30_000);
        let mut pool = TestPool::new(4);
        // Below threshold: counted but the pool still serves.
        pool.record_provider_error();
        assert!(!pool.quarantined(), "1 failure must not open the breaker");
        pool.try_acquire(Lane::Playback)
            .expect("below threshold still acquires");
        pool.record_provider_error();
        assert!(!pool.quarantined(), "2 failures must not open the breaker");
        // Threshold reached: open with a Retry-After.
        pool.record_provider_error();
        assert!(pool.quarantined(), "3 consecutive failures must open");
        let err = pool.try_acquire(Lane::Playback).expect_err("open breaker");
        assert_eq!(
            err,
            PoolError::Quarantined {
                retry_after_ms: CIRCUIT_OPEN_MS
            }
        );
        // Stale failures outside the window do not count: advance past the
        // window, let the open expire, recover, then one fresh failure alone
        // must not reopen.
        pool.advance(CIRCUIT_OPEN_MS);
        pool.record_success();
        assert!(!pool.quarantined());
        pool.advance(CIRCUIT_WINDOW_MS + 1);
        pool.record_provider_error();
        assert!(!pool.quarantined(), "window expired: count restarts at 1");
    }

    #[test]
    fn half_open_probe_recovers_to_closed_on_success() {
        use super::CIRCUIT_OPEN_MS;
        let mut pool = TestPool::new(2);
        for _ in 0..super::CIRCUIT_FAILURE_THRESHOLD {
            pool.record_provider_error();
        }
        assert!(pool.quarantined());
        assert_eq!(pool.circuit_state(), super::CircuitState::Open);
        // Open duration elapses: half-open, exactly one probe is admitted.
        pool.advance(CIRCUIT_OPEN_MS);
        assert_eq!(pool.circuit_state(), super::CircuitState::HalfOpen);
        let _probe = pool.try_acquire(Lane::Playback).expect("probe admitted");
        assert!(
            pool.try_acquire(Lane::Playback).is_err(),
            "half-open admits a single probe"
        );
        // Probe success closes the breaker.
        pool.record_success();
        assert_eq!(pool.circuit_state(), super::CircuitState::Closed);
        assert!(!pool.quarantined());
        pool.try_acquire(Lane::Playback)
            .expect("closed breaker serves");
    }

    #[test]
    fn orphaned_plays_queue_at_pool_level_and_drain_once() {
        let mut pool = TestPool::new(2);
        pool.push_orphan(OrphanedPlay {
            user_id: "u".into(),
            track_id: "t".into(),
            bytes: 7,
        });
        // Streams record through the shared sink without holding the pool
        // (this is how `Drop` meters a lease-less cache-hit stream).
        pool.orphan_sink()
            .lock()
            .expect("sink lock")
            .push(OrphanedPlay {
                user_id: "u".into(),
                track_id: "t".into(),
                bytes: 8,
            });
        let drained = pool.take_orphaned();
        assert_eq!(
            drained.iter().map(|o| o.bytes).sum::<u64>(),
            15,
            "both records drain together"
        );
        assert!(pool.take_orphaned().is_empty(), "drain is once-only");
    }

    #[test]
    fn success_clears_quarantine() {
        let mut pool = TestPool::new(2);
        for _ in 0..super::CIRCUIT_FAILURE_THRESHOLD {
            pool.record_provider_error();
        }
        assert!(pool.quarantined());
        pool.record_success();
        assert!(!pool.quarantined());
        assert_eq!(pool.circuit_state(), super::CircuitState::Closed);
        pool.try_acquire(Lane::Playback)
            .expect("acquire after reset");
    }

    #[test]
    fn stale_lease_success_while_open_leaves_the_breaker_open() {
        use super::{CircuitState, CIRCUIT_OPEN_MS};
        let mut pool = TestPool::new(2);
        let stale = pool.try_acquire(Lane::Playback).expect("stale lease");
        for _ in 0..super::CIRCUIT_FAILURE_THRESHOLD {
            pool.record_provider_error();
        }
        assert!(pool.quarantined());
        pool.record_success_for(stale.id);
        assert!(
            pool.quarantined(),
            "a stale lease must not close an open breaker"
        );
        assert_eq!(pool.circuit_state(), CircuitState::Open);
        pool.release(stale, None);
        // The streak is untouched too: expiry still half-opens for a probe.
        pool.advance(CIRCUIT_OPEN_MS);
        assert_eq!(pool.circuit_state(), CircuitState::HalfOpen);
    }

    #[test]
    fn probe_success_closes_but_unrelated_success_does_not() {
        use super::{CircuitState, CIRCUIT_OPEN_MS};
        let mut pool = TestPool::new(3);
        let stale = pool.try_acquire(Lane::Playback).expect("stale lease");
        for _ in 0..super::CIRCUIT_FAILURE_THRESHOLD {
            pool.record_provider_error();
        }
        pool.advance(CIRCUIT_OPEN_MS);
        assert_eq!(pool.circuit_state(), CircuitState::HalfOpen);
        let probe = pool.try_acquire(Lane::Playback).expect("probe admitted");

        // A concurrent stale success leaves the half-open breaker alone.
        pool.record_success_for(stale.id);
        assert_eq!(pool.circuit_state(), CircuitState::HalfOpen);

        // The probe's own success closes it.
        pool.record_success_for(probe.id);
        assert_eq!(pool.circuit_state(), CircuitState::Closed);
        pool.release(probe, None);
        pool.release(stale, None);
    }

    #[test]
    fn closed_state_success_still_clears_the_streak() {
        let mut pool = TestPool::new(2);
        let lease = pool.try_acquire(Lane::Playback).expect("lease");
        pool.record_provider_error();
        pool.record_provider_error();
        pool.record_success_for(lease.id);
        pool.release(lease, None);
        pool.record_provider_error();
        pool.record_provider_error();
        assert!(
            !pool.quarantined(),
            "a closed-state success restarts the streak"
        );
    }
}
