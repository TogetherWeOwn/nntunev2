//! Circuit-breaker failure-threshold literal pin (tests-only).
//!
//! Contract under test (`workers/stream/src/pool.rs:132,540`):
//!
//! - `CIRCUIT_FAILURE_THRESHOLD == 3`: the breaker opens after three
//!   consecutive provider failures whose first-to-last span fits inside the
//!   window; while open every acquire fails, and once the open interval
//!   elapses exactly one half-open probe is admitted
//!   ([`Pool::circuit_state`]).
//!
//! Boundaries pinned here (synthetic pool follows
//! `tests/pool_circuit_window_acceptance.rs`; pure calls only: no sockets,
//! providers, or Durable Objects):
//!
//! - the threshold constant is literally `3`;
//! - two consecutive windowed failures leave the breaker closed (acquires
//!   still succeed); the third consecutive failure opens it (quarantined,
//!   `CircuitState::Open`, acquires denied with the `30_000` ms hint).
//!   Failure counts are written as literals (`1`/`2`/`3`), never as loops
//!   over `CIRCUIT_FAILURE_THRESHOLD`.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - a threshold mutant (`3` -> `5`, `2`, or any other value) trips the
//!   literal pin; the `3` -> `5` drift that keeps the symbolic
//!   `0..CIRCUIT_FAILURE_THRESHOLD` suites green trips the staged
//!   closed/closed/open assertions here (with a threshold of 5 the third
//!   failure would still report closed);
//! - a counting mutant (opens on the 2nd failure, or never opens on the
//!   3rd) trips the staged assertions in either direction.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-stream --test circuit_threshold_literal_acceptance
//! ```

use nntune_stream::pool::{
    CircuitState, Lane, Pool, PoolError, CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_OPEN_MS,
};

#[test]
fn circuit_failure_threshold_is_three() {
    assert_eq!(
        CIRCUIT_FAILURE_THRESHOLD, 3,
        "circuit breaker must open after exactly 3 consecutive windowed failures"
    );
}

#[test]
fn two_windowed_failures_leave_breaker_closed_third_opens_it() {
    let mut pool: Pool<()> = Pool::new(4);

    // Failure 1 of 3: breaker stays closed.
    pool.record_provider_error();
    assert!(
        !pool.quarantined(),
        "failure 1 of 3 must not open the breaker"
    );
    assert_eq!(pool.circuit_state(), CircuitState::Closed);
    pool.try_acquire(Lane::Playback)
        .expect("one failure still acquires");

    // Failure 2 of 3: breaker still stays closed (no clock advance, so both
    // failures sit inside the same window).
    pool.record_provider_error();
    assert!(
        !pool.quarantined(),
        "failure 2 of 3 must not open the breaker"
    );
    assert_eq!(pool.circuit_state(), CircuitState::Closed);
    pool.try_acquire(Lane::Playback)
        .expect("two failures still acquire");

    // Failure 3 of 3: the breaker opens.
    pool.record_provider_error();
    assert!(pool.quarantined(), "failure 3 of 3 must open the breaker");
    assert_eq!(pool.circuit_state(), CircuitState::Open);
    let err = pool
        .try_acquire(Lane::Playback)
        .expect_err("open breaker denies acquires");
    assert_eq!(
        err,
        PoolError::Quarantined {
            retry_after_ms: 30_000,
        }
    );
    assert_eq!(
        err,
        PoolError::Quarantined {
            retry_after_ms: CIRCUIT_OPEN_MS,
        }
    );
}
