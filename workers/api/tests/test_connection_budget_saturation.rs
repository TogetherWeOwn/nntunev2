//! `TestConnectionBudget` `u64::MAX` saturation pins.
//!
//! `TestConnectionBudget::new` builds deadlines with `saturating_add`
//! (`workers/api/src/vault.rs`), so a start near `u64::MAX` must clamp both
//! deadlines to `u64::MAX` instead of wrapping. `security_regressions.rs`
//! pins small values only; these tests pin the saturation boundary.

use nntune_api::vault;

// u64::MAX saturates both deadlines: no wrap, both expired at MAX.
#[test]
fn max_start_clamps_both_deadlines_to_max() {
    let budget = vault::TestConnectionBudget::new(u64::MAX);
    assert_eq!(budget.start_ms, u64::MAX);
    assert_eq!(
        budget.dial_deadline_ms,
        u64::MAX,
        "dial deadline must saturate, not wrap"
    );
    assert_eq!(
        budget.total_deadline_ms,
        u64::MAX,
        "total deadline must saturate, not wrap"
    );
    assert!(
        budget.dial_expired(u64::MAX),
        "dial must read expired at the clamped deadline"
    );
    assert!(
        budget.total_expired(u64::MAX),
        "total must read expired at the clamped deadline"
    );
}

// Exact-boundary control: one below the clamp point lands exactly on MAX
// with no saturation, so the clamp above is a saturating_add effect.
#[test]
fn boundary_start_lands_exactly_on_max_without_clamping() {
    let start = u64::MAX - vault::TOTAL_TIMEOUT_MS;
    let budget = vault::TestConnectionBudget::new(start);
    assert_eq!(
        budget.total_deadline_ms,
        u64::MAX,
        "start = MAX - TOTAL must land exactly on MAX"
    );
    assert_eq!(
        budget.dial_deadline_ms,
        start + vault::DIAL_TIMEOUT_MS,
        "dial must be exact arithmetic below the clamp point"
    );
    assert!(
        !budget.total_expired(u64::MAX - 1),
        "total must not fire one tick before the exact deadline"
    );
    assert!(budget.total_expired(u64::MAX));
    assert!(!budget.dial_expired(budget.dial_deadline_ms - 1));
    assert!(budget.dial_expired(budget.dial_deadline_ms));

    let dial_start = u64::MAX - vault::DIAL_TIMEOUT_MS;
    let dial_budget = vault::TestConnectionBudget::new(dial_start);
    assert_eq!(
        dial_budget.dial_deadline_ms,
        u64::MAX,
        "start = MAX - DIAL must land exactly on MAX"
    );
    assert!(!dial_budget.dial_expired(u64::MAX - 1));
    assert!(dial_budget.dial_expired(u64::MAX));
}

// Small-value behavior unchanged (mirrors the security_regressions pin).
#[test]
fn small_start_behavior_unchanged() {
    let budget = vault::TestConnectionBudget::new(1_000);
    assert_eq!(
        budget.dial_deadline_ms - budget.start_ms,
        vault::DIAL_TIMEOUT_MS
    );
    assert_eq!(
        budget.total_deadline_ms - budget.start_ms,
        vault::TOTAL_TIMEOUT_MS
    );
    assert!(!budget.dial_expired(1_000));
    assert!(!budget.dial_expired(5_999));
    assert!(budget.dial_expired(6_000));
    assert!(!budget.total_expired(15_999));
    assert!(budget.total_expired(16_000));
}
