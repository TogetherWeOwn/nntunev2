//! Auth TTL literal pins (tests-only).
//!
//! Contract under test (`workers/api/src/auth.rs:18,20,80,160`):
//!
//! - `MAGIC_LINK_TTL_SECS == 900` (email magic-link lifetime, 15 minutes);
//! - `CHALLENGE_TTL_SECS == 300` (passkey challenge lifetime, 5 minutes);
//! - `issue_magic_link` at fixed `now` yields `expires_at == now + 900`;
//! - `new_registration_challenge` at fixed `now` yields
//!   `expires_at == now + 300`.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - changing either constant (e.g. `900` -> `600`, `300` -> `120`) fails
//!   its literal pin below — the lifetime is a client-visible auth contract
//!   (email-link validity window, ceremony validity window), so drift must be
//!   a deliberate slice with docs and caller updates, not a silent edit;
//! - rewiring `issue_magic_link` / `new_registration_challenge` away from
//!   `saturating_add(<TTL>)` (fixed offset, different base, wall-clock read)
//!   fails the `expires_at == now + <literal>` pins even when the constants
//!   still read `900` / `300` — the expiry assertions go through issuance,
//!   not just the bare symbols;
//! - shifting `issued_at` off `now` fails the `issued_at == now` legs, which
//!   anchor the `expires_at` arithmetic to the caller-supplied clock.
//!
//! Boundary: saturation/overflow behavior stays pinned in
//! `auth_issuance_saturation.rs` (near-`u64::MAX` `saturating_add`); the
//! invite redemption window is owned by its own slice; limit enforcement is
//! out of scope. This suite pins only the two TTL literals and their
//! fixed-`now` expiry wiring. Tests only; no production code is touched.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --manifest-path workers/api/Cargo.toml --test auth_ttl_literals_acceptance
//! ```

use nntune_api::auth::{
    issue_magic_link, new_registration_challenge, CHALLENGE_TTL_SECS, MAGIC_LINK_TTL_SECS,
};

/// Fixed clock for the expiry-wiring pins: far from `0` and from `u64::MAX`
/// so `now + TTL` is plain addition (saturation lives in
/// `auth_issuance_saturation.rs`, not here).
const NOW: u64 = 1_700_000_000;
const RP_ID: &str = "nntune.example.com";
const ORIGIN: &str = "https://nntune.example.com";

#[test]
fn magic_link_ttl_is_900_secs() {
    assert_eq!(
        MAGIC_LINK_TTL_SECS, 900,
        "magic-link lifetime must stay exactly 900s (15 minutes)"
    );
}

#[test]
fn challenge_ttl_is_300_secs() {
    assert_eq!(
        CHALLENGE_TTL_SECS, 300,
        "passkey challenge lifetime must stay exactly 300s (5 minutes)"
    );
}

#[test]
fn issue_magic_link_expires_now_plus_900() {
    // Through issuance, not just the bare constant: a TTL edit or a
    // rewiring of the `expires_at` computation fails this pin.
    let (_, record) = issue_magic_link("u", "a@b.c", NOW).expect("issue");
    assert_eq!(record.issued_at, NOW, "magic link must be issued at now");
    assert_eq!(
        record.expires_at,
        NOW + 900,
        "magic link must expire exactly now + 900"
    );
    assert_eq!(
        record.expires_at - record.issued_at,
        MAGIC_LINK_TTL_SECS,
        "magic-link window must equal MAGIC_LINK_TTL_SECS"
    );
}

#[test]
fn new_registration_challenge_expires_now_plus_300() {
    // Through ceremony creation, not just the bare constant: a TTL edit or a
    // rewiring of the `expires_at` computation fails this pin.
    let challenge =
        new_registration_challenge(RP_ID, ORIGIN, NOW).expect("valid rp/origin must issue");
    assert_eq!(challenge.issued_at, NOW, "challenge must be issued at now");
    assert_eq!(
        challenge.expires_at,
        NOW + 300,
        "challenge must expire exactly now + 300"
    );
    assert_eq!(
        challenge.expires_at - challenge.issued_at,
        CHALLENGE_TTL_SECS,
        "challenge window must equal CHALLENGE_TTL_SECS"
    );
}
