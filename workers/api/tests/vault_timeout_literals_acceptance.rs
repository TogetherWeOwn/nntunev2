//! Vault dial/total timeout literal pins (tests-only).
//!
//! Contract under test (`workers/api/src/vault.rs:33,35`, enforced together
//! at `:454-455`):
//!
//! - `DIAL_TIMEOUT_MS == 5_000` (per-attempt TCP/TLS handshake budget);
//! - `TOTAL_TIMEOUT_MS == 15_000` (whole `test connection` attempt budget);
//! - `TestConnectionBudget::new` at a fixed start yields
//!   `dial_deadline_ms == start + 5_000` and
//!   `total_deadline_ms == start + 15_000`.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - changing either constant (e.g. `5_000` -> `3_000`, `15_000` -> `30_000`)
//!   fails its literal pin below — the deadlines are a client-visible
//!   `test-connection` contract (pool Worker/DO abort points), so drift must
//!   be a deliberate slice with docs and caller updates, not a silent edit;
//! - rewiring `TestConnectionBudget::new` away from
//!   `start.saturating_add(<TIMEOUT>)` (fixed offset, different base,
//!   wall-clock read) fails the `deadline == start + <literal>` pins even
//!   when the constants still read `5_000` / `15_000` — the deadline
//!   assertions go through the helper, not just the bare symbols, and spell
//!   the offsets as bare literals so the suite (not the symbol) owns the
//!   values.
//!
//! Boundary: `u64::MAX` saturation behavior stays pinned in
//! `test_connection_budget_saturation.rs` (near-`u64::MAX` `saturating_add`);
//! the lease duration, dial-address ceiling, and pool-DO dial path are owned
//! by their own slices. This suite pins only the two timeout literals and
//! their fixed-start deadline wiring. Tests only; no production code is
//! touched.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-api --test vault_timeout_literals_acceptance
//! ```

use nntune_api::vault::{TestConnectionBudget, DIAL_TIMEOUT_MS, TOTAL_TIMEOUT_MS};

/// Fixed start for the deadline-wiring pins: far from `0` and from `u64::MAX`
/// so `start + timeout` is plain addition (saturation lives in
/// `test_connection_budget_saturation.rs`, not here).
const START_MS: u64 = 1_700_000_000_000;

#[test]
fn dial_timeout_is_5000_ms() {
    assert_eq!(
        DIAL_TIMEOUT_MS, 5_000,
        "vault dial timeout must stay exactly 5_000 ms; \
         a new budget needs a threat-model update plus this pin"
    );
}

#[test]
fn total_timeout_is_15000_ms() {
    assert_eq!(
        TOTAL_TIMEOUT_MS, 15_000,
        "vault total timeout must stay exactly 15_000 ms; \
         a new budget needs a threat-model update plus this pin"
    );
}

#[test]
fn deadlines_from_fixed_start_match_bare_literals() {
    let budget = TestConnectionBudget::new(START_MS);
    assert_eq!(
        budget.start_ms, START_MS,
        "budget must retain the caller-supplied start"
    );
    assert_eq!(
        budget.dial_deadline_ms,
        START_MS + 5_000,
        "dial deadline must be exactly start + 5_000 ms through the helper"
    );
    assert_eq!(
        budget.total_deadline_ms,
        START_MS + 15_000,
        "total deadline must be exactly start + 15_000 ms through the helper"
    );
}
