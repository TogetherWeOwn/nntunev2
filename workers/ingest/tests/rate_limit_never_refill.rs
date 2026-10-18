//! TokenBucket never-refill `retry_after_secs` pin.
//!
//! Pins `TokenBucket::retry_after_secs` (`workers/ingest/src/rate_limit.rs:103-113`):
//! a bucket that can never refill (zero window, so `refill_per_sec == 0.0`)
//! reports `u64::MAX` once exhausted, instead of a finite wait or zero.
//! The `u64::MAX` sentinel is otherwise mentioned only in a doc comment in
//! `tests/search_rate_limits_acceptance.rs:20`, so without this pin a
//! MAX-to-zero regression would go unnoticed.
//!
//! Failing command (ingest is a standalone crate outside the root workspace):
//! `cd workers/ingest && cargo test --locked --test rate_limit_never_refill`
//!
//! Sensitivity: temporarily returning `0` instead of `u64::MAX` on the
//! never-refill arm must fail `exhausted_zero_window_reports_max` (got `0`,
//! want `u64::MAX`) while both controls stay green; the mutation is restored
//! before committing.

use nntune_ingest::rate_limit::TokenBucket;

const NOW: u64 = 1_700_000_000;

/// Exhausted zero-window bucket reports `u64::MAX` — and never refills.
#[test]
fn exhausted_zero_window_reports_max() {
    let mut b = TokenBucket::per_window(4, 0, NOW);
    for _ in 0..4 {
        assert!(b.take(NOW, 1.0), "zero-window bucket starts full");
    }
    assert!(!b.take(NOW, 1.0), "exhausted bucket must deny");
    assert_eq!(b.remaining(NOW), 0.0);
    assert_eq!(
        b.retry_after_secs(NOW),
        u64::MAX,
        "never-refilling bucket must report u64::MAX"
    );
    // No refill, ever: the wait does not decay with time.
    assert_eq!(b.remaining(NOW + 3_600), 0.0);
    assert_eq!(
        b.retry_after_secs(NOW + 3_600),
        u64::MAX,
        "never-refilling bucket must stay u64::MAX after time passes"
    );
}

/// Control: a bucket with a token available reports no wait.
#[test]
fn token_available_control_returns_zero() {
    // Normal-window fresh bucket.
    let full = TokenBucket::per_window(4, 8, NOW);
    assert_eq!(full.retry_after_secs(NOW), 0, "full bucket: no wait");
    // Partially spent but still holding a token.
    let mut partial = TokenBucket::per_window(4, 8, NOW);
    assert!(partial.take(NOW, 3.0));
    assert_eq!(partial.retry_after_secs(NOW), 0, "one token left: no wait");
    // Zero-window bucket before exhaustion also reports no wait.
    let fresh_never = TokenBucket::per_window(4, 0, NOW);
    assert_eq!(
        fresh_never.retry_after_secs(NOW),
        0,
        "unexhausted never-refill bucket: no wait"
    );
}

/// Control: an exhausted normal-window bucket reports a small positive wait
/// (binary-exact 4-per-8s bucket, rate 0.5/s, so the value is exact).
#[test]
fn normal_window_control_returns_small_positive_wait() {
    let mut b = TokenBucket::per_window(4, 8, NOW);
    for _ in 0..4 {
        assert!(b.take(NOW, 1.0));
    }
    assert!(!b.take(NOW, 1.0), "control bucket must be exhausted");
    assert_eq!(b.retry_after_secs(NOW), 2);
    assert_eq!(b.retry_after_secs(NOW + 1), 1);
    assert_eq!(b.retry_after_secs(NOW + 2), 0);
}
