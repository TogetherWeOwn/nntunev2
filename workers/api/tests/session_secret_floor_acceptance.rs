//! Session secret-length floor pins (tests-only).
//!
//! Contract under test (`workers/api/src/session.rs:160,163,196,225,252`):
//!
//! - `MIN_SECRET_LEN == 32` (32 bytes / 256 bits). The entry points
//!   exercised here enforce it via `check_secret_len` and fail closed with
//!   [`session::SessionError::Misconfigured`] — never a working weak key.
//! - A 31-byte secret is refused at the public entry points exercised here:
//!   [`session::issue_session`] and [`session::verify_session`], plus the
//!   steady-state [`session::SessionKeys::single`] dual path
//!   ([`session::verify_session_dual`]) which length-checks the configured
//!   primary before token verification. The rotation-window `previous`-key
//!   length check is not pinned here (out of scope, see below).
//! - A 32-byte secret passes: issue then verify round-trips, including
//!   through the `SessionKeys::single` dual path.
//!
//! Sensitivity (verified by construction, production code untouched):
//!
//! - Lowering `MIN_SECRET_LEN` (e.g. to `16`) moves every 31-byte call off
//!   `Misconfigured` — the issue call succeeds, the garbage-token verifies
//!   fall through to `Invalid`, and the valid-token verify succeeds —
//!   turning all three `thirty_one_byte_*` tests red.
//! - Raising `MIN_SECRET_LEN` (e.g. to `64`) rejects the 32-byte round-trip,
//!   turning `thirty_two_byte_secret_roundtrips` red.
//! - Relaxing any `check_secret_len` call (or the `< MIN_SECRET_LEN`
//!   comparison) to a pass-through has the same effect as lowering the
//!   floor for that entry point.
//! - Renaming or moving the floor without updating this suite fails the
//!   `min_secret_len_is_exactly_32` literal pin.
//!
//! Out of scope: rotation-window `previous`-key behavior, skew rejection,
//! and any implementation change. Fixtures use synthetic
//! secrets only (public repo: never real secrets; no secret material is
//! logged or asserted on — only lengths and error variants).
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/api && cargo test --locked --test session_secret_floor_acceptance
//! ```

use nntune_api::session::{self, SessionError, SessionKeys};

const NOW: u64 = 1_700_000_000;
const TTL: u64 = 3600;

/// One byte below the floor.
fn short_secret() -> Vec<u8> {
    vec![7u8; 31]
}

/// Exactly the floor.
fn floor_secret() -> Vec<u8> {
    vec![7u8; 32]
}

#[test]
fn min_secret_len_is_exactly_32() {
    assert_eq!(
        session::MIN_SECRET_LEN,
        32,
        "session HMAC secret floor must stay exactly 32 bytes (256 bits)"
    );
}

#[test]
fn thirty_one_byte_secret_fails_issue_closed() {
    let short = short_secret();
    assert_eq!(
        session::issue_session(&short, "user-1", NOW, TTL),
        Err(SessionError::Misconfigured),
        "minting under a 31-byte secret must fail closed, never issue a weak-key token"
    );
}

#[test]
fn thirty_one_byte_secret_fails_verify_closed() {
    let short = short_secret();
    // Length gate fires before token parsing: even a garbage token maps to
    // Misconfigured, not Invalid.
    assert_eq!(
        session::verify_session(&short, "x.y", NOW),
        Err(SessionError::Misconfigured),
        "verifying with a 31-byte secret must fail closed before token parsing"
    );
    // Same gate against an otherwise-valid token minted under the floor:
    // proves the failure is the short verifier key, not a bad token.
    let floor = floor_secret();
    let token = session::issue_session(&floor, "user-1", NOW, TTL).expect("floor secret issues");
    assert_eq!(
        session::verify_session(&short, &token, NOW + 60),
        Err(SessionError::Misconfigured),
        "a valid token verified under a 31-byte secret must fail closed"
    );
}

#[test]
fn thirty_one_byte_secret_fails_dual_via_single_closed() {
    let short = short_secret();
    let keys = SessionKeys::single(&short);
    assert_eq!(
        session::verify_session_dual(&keys, "x.y", NOW),
        Err(SessionError::Misconfigured),
        "steady-state single-key verification with a 31-byte secret must fail closed"
    );
}

#[test]
fn thirty_two_byte_secret_roundtrips() {
    let floor = floor_secret();
    let token = session::issue_session(&floor, "user-1", NOW, TTL).expect("floor secret issues");
    let claims = session::verify_session(&floor, &token, NOW + 60).expect("floor secret verifies");
    assert_eq!(claims.user_id, "user-1");
    // Steady-state dual path with the same floor secret verifies too.
    let keys = SessionKeys::single(&floor);
    let dual =
        session::verify_session_dual(&keys, &token, NOW + 60).expect("single-key dual verifies");
    assert_eq!(dual, claims);
}
