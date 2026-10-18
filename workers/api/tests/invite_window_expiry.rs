//! Tests-only pin: invite redemption window-expiry saturation.
//!
//! Implementation stays untouched: `invite_attempt_allowed` counts only
//! timestamps with `now.saturating_sub(ts) < INVITE_REDEMPTION_WINDOW_SECS`
//! (300s), and `invite_attempt_retention_cutoff` returns
//! `now.saturating_sub(300)`. These tests pin the window-expiry edge so a
//! future off-by-one (`<=` vs `<`) or retention-prune mismatch is caught.
//!
//! Cases:
//! - 10 attempts exactly at the cutoff (`NOW - 300`) age out -> allowed.
//! - 10 attempts strictly below the cutoff age out -> allowed.
//! - 9 fresh + 1 stale (at cutoff) -> allowed (stale does not count).
//! - Saturation control: 10 fresh attempts -> denied (proves the allowed
//!   cases above are the expiry edge, not a raised limit).
//! - `retention_cutoff(NOW) == NOW - 300`; `retention_cutoff(30) == 0`
//!   (saturates, never wraps).
//! - Future timestamps count against the caller (fail-closed): 10 future
//!   attempts -> denied.
//!
//! Sensitivity: deleting the `< WINDOW` filter (counting every stored row)
//! turns the at-cutoff and 9+1 cases denied; widening the window to `<=`
//! keeps them allowed but flips the `NOW - 300` just-inside control denied.
//! Lowering `INVITE_REDEMPTION_MAX_ATTEMPTS` to 9 flips the 9-fresh control
//! denied.

use nntune_api::auth::{
    invite_attempt_allowed, invite_attempt_retention_cutoff, INVITE_REDEMPTION_MAX_ATTEMPTS,
    INVITE_REDEMPTION_WINDOW_SECS,
};

const NOW: u64 = 1_700_000_000;

#[test]
fn ten_attempts_at_cutoff_age_out() {
    let cutoff = invite_attempt_retention_cutoff(NOW);
    assert_eq!(cutoff, NOW - INVITE_REDEMPTION_WINDOW_SECS);
    let aged: Vec<u64> = vec![cutoff; INVITE_REDEMPTION_MAX_ATTEMPTS as usize];
    assert!(
        invite_attempt_allowed(&aged, NOW),
        "10 attempts exactly at the cutoff ({cutoff}) must age out -> allowed"
    );
}

#[test]
fn ten_attempts_below_cutoff_age_out() {
    let cutoff = invite_attempt_retention_cutoff(NOW);
    let older: Vec<u64> = vec![cutoff - 1; INVITE_REDEMPTION_MAX_ATTEMPTS as usize];
    assert!(
        invite_attempt_allowed(&older, NOW),
        "10 attempts below the cutoff must age out -> allowed"
    );
    let ancient: Vec<u64> = vec![0; INVITE_REDEMPTION_MAX_ATTEMPTS as usize];
    assert!(
        invite_attempt_allowed(&ancient, NOW),
        "10 ancient attempts must age out -> allowed"
    );
}

#[test]
fn nine_fresh_plus_one_stale_is_allowed() {
    let cutoff = invite_attempt_retention_cutoff(NOW);
    let mut attempts: Vec<u64> = (0..9).map(|i| NOW - i).collect();
    attempts.push(cutoff);
    assert_eq!(attempts.len(), 10);
    assert!(
        invite_attempt_allowed(&attempts, NOW),
        "9 fresh + 1 stale-at-cutoff must be allowed (stale does not count)"
    );
}

#[test]
fn ten_fresh_attempts_saturate_denied() {
    // Control: without expiry, a full window denies. Proves the allowed
    // cases above are the expiry edge, not a raised limit.
    let full: Vec<u64> = (0..10).map(|i| NOW - i).collect();
    assert!(
        !invite_attempt_allowed(&full, NOW),
        "10 fresh in-window attempts must saturate -> denied"
    );
}

#[test]
fn retention_cutoff_values_and_saturation() {
    assert_eq!(
        invite_attempt_retention_cutoff(NOW),
        NOW - INVITE_REDEMPTION_WINDOW_SECS,
        "cutoff(NOW) must be NOW - 300"
    );
    assert_eq!(
        invite_attempt_retention_cutoff(30),
        0,
        "cutoff(30) must saturate to 0, never wrap"
    );
    assert_eq!(invite_attempt_retention_cutoff(0), 0);
}

#[test]
fn future_timestamps_count_fail_closed() {
    let future: Vec<u64> = vec![NOW + 1_000; INVITE_REDEMPTION_MAX_ATTEMPTS as usize];
    assert!(
        !invite_attempt_allowed(&future, NOW),
        "10 future timestamps must count -> denied (fail-closed, not bypass)"
    );
    // A single future timestamp consumes one slot: 9 fresh + 1 future denies.
    let mut mixed: Vec<u64> = (0..9).map(|i| NOW - i).collect();
    mixed.push(NOW + 1_000);
    assert!(
        !invite_attempt_allowed(&mixed, NOW),
        "9 fresh + 1 future must saturate -> denied"
    );
}
