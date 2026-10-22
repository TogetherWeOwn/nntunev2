//! Container health-check budget literal pins (tests-only).
//!
//! Contract under test (`workers/repair/src/runtime.rs:38,299-303`):
//!
//! - `HEALTH_ATTEMPTS == 60` (max `/health` polls per cold start);
//! - the poll interval is the inline `Duration::from_millis(500)`;
//! - the effective cold-start budget is therefore
//!   `60 * 500 ms = 30_000 ms = 30 s`.
//!
//! The `runtime` module compiles only for `wasm32`, so a native
//! integration target cannot import the constant; the pins read
//! `runtime.rs` as source text via `include_str!` instead of a crate
//! import. `cargo build` never compiles `tests/`, so this costs the
//! container build contexts nothing.
//!
//! Sensitivity (implementation stays untouched):
//!
//! - lowering/raising `HEALTH_ATTEMPTS` (e.g. to 30 or 120) fails
//!   `health_attempts_literal_is_60`;
//! - changing the inline delay (e.g. to `from_millis(1000)`) fails
//!   `health_interval_literal_is_500_ms`;
//! - unrolling the loop bound to a bare literal instead of the named
//!   constant fails `poll_loop_uses_attempts_const`;
//! - editing the pinned arithmetic copies below without touching the
//!   source fails `effective_budget_is_30_seconds` on the product.
//!
//! Out of scope: repair contract caps and rounds; scratch-manifest
//! payload sizes (payload bounds, not the cold-start budget); any
//! runtime behaviour change. No network, no clock, no Container.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-repair --test health_budget_literals_acceptance
//! ```

/// `runtime.rs` source, read as text (NOT a crate import — the module is
/// `cfg(target_arch = "wasm32")` and unreachable from native tests).
const RUNTIME_RS: &str = include_str!("../src/runtime.rs");

/// Pinned copies of the budget literals; the source-text tests below prove
/// these cannot drift from `runtime.rs` without turning red.
const PINNED_ATTEMPTS: u32 = 60;
const PINNED_INTERVAL_MS: u64 = 500;
/// 60 polls * 500 ms per poll.
const PINNED_BUDGET_MS: u64 = 30_000;

#[test]
fn health_attempts_literal_is_60() {
    assert!(
        RUNTIME_RS.contains("const HEALTH_ATTEMPTS: u32 = 60;"),
        "runtime.rs must still declare `const HEALTH_ATTEMPTS: u32 = 60;`: \
         the Container cold-start budget is 60 polls and a silent attempts \
         change would stretch or shrink it"
    );
    assert_eq!(PINNED_ATTEMPTS, 60);
}

#[test]
fn health_interval_literal_is_500_ms() {
    assert!(
        RUNTIME_RS.contains("Duration::from_millis(500)"),
        "runtime.rs must still poll on `Duration::from_millis(500)`: \
         the per-poll interval is an inline literal and a silent change \
         would stretch or shrink the 30 s cold-start budget"
    );
    assert_eq!(PINNED_INTERVAL_MS, 500);
}

#[test]
fn poll_loop_uses_attempts_const() {
    assert!(
        RUNTIME_RS.contains("for _ in 0..HEALTH_ATTEMPTS"),
        "runtime.rs must still bound the /health poll loop with \
         `for _ in 0..HEALTH_ATTEMPTS`: a bare literal bound would silently \
         decouple the loop from the pinned attempts constant"
    );
}

#[test]
fn effective_budget_is_30_seconds() {
    assert_eq!(
        u64::from(PINNED_ATTEMPTS) * PINNED_INTERVAL_MS,
        PINNED_BUDGET_MS,
        "60 polls at 500 ms each must total exactly 30_000 ms"
    );
    assert_eq!(PINNED_BUDGET_MS, 30_000);
}
