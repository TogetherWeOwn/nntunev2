//! Pool circuit window-expiry and half-open probe-retry pin.
//!
//! Pins the failure-window expiry and the half-open single-probe retry hint
//! against drift: failures whose first-to-last span exceeds the 30s window
//! restart the streak instead of tripping the breaker, and a denied half-open
//! acquire carries a 1s retry hint. Uses the virtual [`Pool::advance`] clock
//! so the timing is deterministic. Pure calls only: no sockets, providers,
//! or Durable Objects.
//!
//! Sensitivity: a window-reset mutant (`>` vs `>=`, window never reset, or
//! streak never restarted) trips the spread-failures tests; a retry-hint
//! mutant (0, `CIRCUIT_OPEN_MS`, or a computed remainder) trips the
//! half-open denial tests. Implementation unchanged by this pin.

use nntune_stream::pool::{
    CircuitState, Lane, Pool, PoolError, CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_OPEN_MS,
    CIRCUIT_WINDOW_MS, HALF_OPEN_PROBE_RETRY_MS,
};

#[test]
fn circuit_window_ms_is_thirty_seconds() {
    assert_eq!(CIRCUIT_WINDOW_MS, 30_000);
}

#[test]
fn half_open_probe_retry_ms_is_one_second() {
    assert_eq!(HALF_OPEN_PROBE_RETRY_MS, 1_000);
}

#[test]
fn failures_spread_wider_than_window_do_not_trip_breaker() {
    let mut pool: Pool<()> = Pool::new(4);
    // Three failures each separated by more than the window never accumulate:
    // every failure restarts the streak at 1.
    for i in 0..CIRCUIT_FAILURE_THRESHOLD {
        if i > 0 {
            pool.advance(CIRCUIT_WINDOW_MS + 1);
        }
        pool.record_provider_error();
        assert!(
            !pool.quarantined(),
            "spread failure {i} must not open the breaker"
        );
        assert_eq!(pool.circuit_state(), CircuitState::Closed);
    }
    pool.try_acquire(Lane::Playback)
        .expect("spread failures still acquire");
}

#[test]
fn failure_at_exact_window_boundary_remains_in_streak() {
    let mut pool: Pool<()> = Pool::new(4);
    for _ in 0..(CIRCUIT_FAILURE_THRESHOLD - 1) {
        pool.record_provider_error();
    }
    // A span of exactly CIRCUIT_WINDOW_MS is still inside the window (`>`
    // reset, not `>=`): the threshold-reaching failure must open the breaker.
    pool.advance(CIRCUIT_WINDOW_MS);
    pool.record_provider_error();
    assert!(
        pool.quarantined(),
        "failure exactly at the window edge stays in the streak"
    );
}

#[test]
fn two_close_failures_then_window_expiry_restarts_streak() {
    let mut pool: Pool<()> = Pool::new(4);
    pool.record_provider_error();
    pool.record_provider_error();
    assert!(!pool.quarantined(), "2 failures must not open");
    // The stale streak expires: the next failure restarts the count at 1.
    pool.advance(CIRCUIT_WINDOW_MS + 1);
    pool.record_provider_error();
    assert!(
        !pool.quarantined(),
        "window expired: count restarts at 1, breaker stays closed"
    );
    assert_eq!(pool.circuit_state(), CircuitState::Closed);
    // Two more back-to-back failures reach the fresh threshold of 3.
    pool.record_provider_error();
    assert!(!pool.quarantined());
    pool.record_provider_error();
    assert!(
        pool.quarantined(),
        "three new-window failures must open the breaker"
    );
}

#[test]
fn half_open_second_probe_denied_with_one_second_retry_hint() {
    let mut pool: Pool<()> = Pool::new(4);
    for _ in 0..CIRCUIT_FAILURE_THRESHOLD {
        pool.record_provider_error();
    }
    assert!(pool.quarantined());
    pool.advance(CIRCUIT_OPEN_MS);
    assert_eq!(pool.circuit_state(), CircuitState::HalfOpen);
    let probe = pool.try_acquire(Lane::Playback).expect("probe admitted");
    let err = pool
        .try_acquire(Lane::Playback)
        .expect_err("only one half-open probe may be in flight");
    assert_eq!(
        err,
        PoolError::Quarantined {
            retry_after_ms: 1_000,
        }
    );
    assert_eq!(
        err,
        PoolError::Quarantined {
            retry_after_ms: HALF_OPEN_PROBE_RETRY_MS,
        }
    );
    pool.release(probe, None);
}

#[test]
fn half_open_prefetch_denied_with_one_second_retry_hint() {
    let mut pool: Pool<()> = Pool::new(4);
    for _ in 0..CIRCUIT_FAILURE_THRESHOLD {
        pool.record_provider_error();
    }
    pool.advance(CIRCUIT_OPEN_MS);
    assert_eq!(pool.circuit_state(), CircuitState::HalfOpen);
    // Prefetch may never run the probe, even with no probe in flight.
    let err = pool
        .try_acquire(Lane::Prefetch)
        .expect_err("prefetch never probes");
    assert_eq!(
        err,
        PoolError::Quarantined {
            retry_after_ms: HALF_OPEN_PROBE_RETRY_MS,
        }
    );
    assert_eq!(
        err,
        PoolError::Quarantined {
            retry_after_ms: 1_000
        },
        "half-open denial carries retry_after_ms == 1_000"
    );
}
