//! Tests-only pin: auth issuance near-`u64::MAX` saturates, never wraps.
//!
//! Implementation stays untouched: `issue_magic_link` and
//! `new_registration_challenge` both use `saturating_add` for `expires_at`.
//! These tests pin that behavior at the top of the `u64` range so a future
//! wrapping-add regression is caught.
//!
//! Cases:
//! - `now = u64::MAX` yields `expires_at == u64::MAX` without panic (both).
//! - `now = MAX - MAGIC_LINK_TTL_SECS` yields exactly `u64::MAX` (magic link).
//! - Small-`now` control keeps exact `now + TTL` (both).

use nntune_api::auth::{
    issue_magic_link, new_registration_challenge, CHALLENGE_TTL_SECS, MAGIC_LINK_TTL_SECS,
};

const RP_ID: &str = "nntune.example.com";
const ORIGIN: &str = "https://nntune.example.com";

#[test]
fn magic_link_at_max_saturates_to_max() {
    let (_, record) = issue_magic_link("u", "a@b.c", u64::MAX);
    assert_eq!(record.issued_at, u64::MAX);
    assert_eq!(record.expires_at, u64::MAX);
}

#[test]
fn registration_challenge_at_max_saturates_to_max() {
    let challenge = new_registration_challenge(RP_ID, ORIGIN, u64::MAX).expect("challenge");
    assert_eq!(challenge.issued_at, u64::MAX);
    assert_eq!(challenge.expires_at, u64::MAX);
}

#[test]
fn magic_link_at_max_minus_ttl_yields_exact_max() {
    let now = u64::MAX - MAGIC_LINK_TTL_SECS;
    let (_, record) = issue_magic_link("u", "a@b.c", now);
    assert_eq!(record.issued_at, now);
    assert_eq!(record.expires_at, u64::MAX);
}

#[test]
fn registration_challenge_at_max_minus_ttl_yields_exact_max() {
    let now = u64::MAX - CHALLENGE_TTL_SECS;
    let challenge = new_registration_challenge(RP_ID, ORIGIN, now).expect("challenge");
    assert_eq!(challenge.issued_at, now);
    assert_eq!(challenge.expires_at, u64::MAX);
}

#[test]
fn small_now_control_keeps_now_plus_ttl() {
    let now = 1_700_000_000u64;
    let (_, magic) = issue_magic_link("u", "a@b.c", now);
    assert_eq!(magic.expires_at, now + MAGIC_LINK_TTL_SECS);

    let challenge = new_registration_challenge(RP_ID, ORIGIN, now).expect("challenge");
    assert_eq!(challenge.expires_at, now + CHALLENGE_TTL_SECS);
}
