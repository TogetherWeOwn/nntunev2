//! Acceptance: configured rate-limit maxima clamp to the durable budget cap
//! through the public `RateLimitConfig::from_vars` constructor.
//!
//! `workers/api/src/rate_limit.rs::parse_capped_max` clamps every `*_MAX`
//! var to [`MAX_BUDGET_SLOTS`] (100) so an over-large operator value relaxes
//! to the cap instead of denying everything at the store's admissibility
//! gate. The in-module unit test pins the same lines; this file pins the
//! behavior through the public config surface (`from_vars`), which is what
//! the Worker entry overlays [vars] through.
//!
//! Distinct from the `invalid_config_fails_closed_to_documented_defaults`
//! pin in `rate_limits_acceptance.rs` (zero/garbage shape only, no cap).

use nntune_api::rate_limit::{
    RateLimitConfig, TokenBucket, MAX_BUDGET_SLOTS, VAULT_TEST_IP_MAX_ATTEMPTS,
    VAULT_TEST_MAX_ATTEMPTS,
};
use std::collections::HashMap;

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn huge_max_clamps_to_exactly_100() {
    assert_eq!(MAX_BUDGET_SLOTS, 100);
    // Well above the cap, and the u32 extreme: every MAX var must relax to
    // exactly the cap, never pass through (which would deny-all at the store
    // gate) and never fall back to the default.
    for raw in ["101", "1000", &u32::MAX.to_string()] {
        let cfg = RateLimitConfig::from_vars(&vars(&[
            ("AUTH_RATE_LIMIT_MAX", raw),
            ("VAULT_TEST_RATE_LIMIT_MAX", raw),
            ("VAULT_TEST_IP_RATE_LIMIT_MAX", raw),
        ]));
        assert_eq!(cfg.auth_max_attempts, MAX_BUDGET_SLOTS, "raw={raw}");
        assert_eq!(cfg.vault_test_max_attempts, MAX_BUDGET_SLOTS, "raw={raw}");
        assert_eq!(
            cfg.vault_test_ip_max_attempts, MAX_BUDGET_SLOTS,
            "raw={raw}"
        );
    }
}

#[test]
fn zero_and_garbage_max_yield_the_default() {
    let defaults = RateLimitConfig::default();
    for raw in ["0", "abc", "", "unlimited"] {
        for key in [
            "AUTH_RATE_LIMIT_MAX",
            "VAULT_TEST_RATE_LIMIT_MAX",
            "VAULT_TEST_IP_RATE_LIMIT_MAX",
        ] {
            let cfg = RateLimitConfig::from_vars(&vars(&[(key, raw)]));
            assert_eq!(cfg, defaults, "key={key} raw={raw:?}");
        }
    }
}

#[test]
fn cap_value_100_passes_through_untouched() {
    // The boundary value is a legal budget, not an over-large one: it must
    // survive the clamp unchanged on every MAX var.
    let cfg = RateLimitConfig::from_vars(&vars(&[
        ("AUTH_RATE_LIMIT_MAX", "100"),
        ("VAULT_TEST_RATE_LIMIT_MAX", "100"),
        ("VAULT_TEST_IP_RATE_LIMIT_MAX", "100"),
    ]));
    assert_eq!(cfg.auth_max_attempts, 100);
    assert_eq!(cfg.vault_test_max_attempts, 100);
    assert_eq!(cfg.vault_test_ip_max_attempts, 100);
}

#[test]
fn clamped_config_admits_exactly_a_full_cap_burst() {
    // Functional pin: a huge configured max behaves as a 100-budget through
    // the public config — 100 takes pass at the same instant (no refill
    // involved: capacity/take are binary-exact), the 101st denies.
    let cfg = RateLimitConfig::from_vars(&vars(&[("AUTH_RATE_LIMIT_MAX", "9999")]));
    assert_eq!(cfg.auth_max_attempts, MAX_BUDGET_SLOTS);
    let now = 1_700_000_000u64;
    let mut bucket = TokenBucket::per_window(cfg.auth_max_attempts, cfg.auth_window_secs, now);
    for i in 0..MAX_BUDGET_SLOTS {
        assert!(
            bucket.take(now, 1.0),
            "take {i} of the capped burst must pass"
        );
    }
    assert!(!bucket.take(now, 1.0), "101st take must deny");
    // Sanity: the defaults this falls back to are the documented tunables.
    let defaults = RateLimitConfig::default();
    assert_eq!(defaults.vault_test_max_attempts, VAULT_TEST_MAX_ATTEMPTS);
    assert_eq!(
        defaults.vault_test_ip_max_attempts,
        VAULT_TEST_IP_MAX_ATTEMPTS
    );
}
