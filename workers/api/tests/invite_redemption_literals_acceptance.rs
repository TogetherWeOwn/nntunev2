//! Invite redemption window + attempt literal pins (tests-only).
//!
//! Contract under test (`workers/api/src/auth.rs:296,300`):
//!
//! - `INVITE_REDEMPTION_WINDOW_SECS == 300`;
//! - `INVITE_REDEMPTION_MAX_ATTEMPTS == 10`.
//!
//! Each literal is pinned twice: once as a direct equality (catches a quiet
//! constant edit), and once through its public behavior path (catches a
//! semantic drift that keeps the constant but changes the gate):
//!
//! - window through [`invite_attempt_retention_cutoff`]: `cutoff(NOW) ==
//!   NOW - 300`, with the expiry edge (at-cutoff ages out, just-inside
//!   counts) proving the 300 travels the retention path;
//! - attempts through [`invite_attempt_allowed`]: 9 fresh in-window
//!   attempts pass, 10 deny, proving the limit travels the gate.
//!
//! Boundary behavior beyond the values lives in `invite_window_expiry.rs`;
//! done TOG-30364 pins saturation behavior, not these values.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - changing `INVITE_REDEMPTION_WINDOW_SECS` to any other value fails
//!   `window_literal_is_300` directly, and fails
//!   `window_value_travels_retention_cutoff_path` /
//!   `window_edge_proves_300_through_allowed_path` via the shifted cutoff
//!   and expiry edge;
//! - changing `INVITE_REDEMPTION_MAX_ATTEMPTS` to any other value fails
//!   `max_attempts_literal_is_10` directly, and fails
//!   `attempt_limit_travels_allowed_gate_path` via the moved 9/10 trip point;
//! - widening the gate comparison (`<` to `<=`) or dropping the
//!   `< WINDOW` age-out filter flips the at-cutoff / just-inside edge
//!   assertions below.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-api --test invite_redemption_literals_acceptance
//! ```

use nntune_api::auth::{
    invite_attempt_allowed, invite_attempt_retention_cutoff, INVITE_REDEMPTION_MAX_ATTEMPTS,
    INVITE_REDEMPTION_WINDOW_SECS,
};

const NOW: u64 = 1_700_000_000;

#[test]
fn window_literal_is_300() {
    assert_eq!(
        INVITE_REDEMPTION_WINDOW_SECS, 300,
        "redemption window must stay exactly 300s"
    );
}

#[test]
fn window_value_travels_retention_cutoff_path() {
    // The public retention path must expose the 300s window arithmetically:
    // cutoff(NOW) == NOW - 300, saturating (never wrapping) near zero.
    assert_eq!(
        invite_attempt_retention_cutoff(NOW),
        NOW - 300,
        "cutoff(NOW) must be NOW - 300 through the public retention path"
    );
    assert_eq!(
        invite_attempt_retention_cutoff(300),
        0,
        "cutoff(300) must saturate exactly at zero"
    );
    assert_eq!(
        invite_attempt_retention_cutoff(30),
        0,
        "cutoff(30) must saturate to 0, never wrap"
    );
}

#[test]
fn window_edge_proves_300_through_allowed_path() {
    // At exactly NOW - 300 an attempt has aged out of the 300s window;
    // at NOW - 299 it is still inside. This edge proves the 300 travels
    // the `invite_attempt_allowed` path, not just the constant.
    let at_edge: Vec<u64> = vec![NOW - 300; 10];
    assert!(
        invite_attempt_allowed(&at_edge, NOW),
        "10 attempts exactly at NOW - 300 must age out -> allowed"
    );
    let just_inside: Vec<u64> = vec![NOW - 299; 10];
    assert!(
        !invite_attempt_allowed(&just_inside, NOW),
        "10 attempts at NOW - 299 must still count -> denied"
    );
}

#[test]
fn max_attempts_literal_is_10() {
    assert_eq!(
        INVITE_REDEMPTION_MAX_ATTEMPTS, 10,
        "max redemption attempts must stay exactly 10"
    );
}

#[test]
fn attempt_limit_travels_allowed_gate_path() {
    // The public gate must trip between 9 and 10 fresh in-window attempts,
    // proving the 10 travels the `invite_attempt_allowed` path.
    let nine: Vec<u64> = (0..9).map(|i| NOW - i).collect();
    assert!(
        invite_attempt_allowed(&nine, NOW),
        "9 fresh in-window attempts must pass through the public gate"
    );
    let ten: Vec<u64> = (0..10).map(|i| NOW - i).collect();
    assert!(
        !invite_attempt_allowed(&ten, NOW),
        "10 fresh in-window attempts must trip the public gate -> denied"
    );
}
