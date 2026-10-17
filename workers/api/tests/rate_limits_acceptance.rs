//! Acceptance tests: rate limits and abuse guards on auth + vault test-connection.
//!
//! Failing-first acceptance tests for the rate-limit build (roadmap R6).
//! These tests FAIL until
//! `nntune_api::rate_limit` implements the contract below.
//!
//! Builder: implement `workers/api/src/rate_limit.rs` (pure Rust, native +
//! wasm32, NO Worker runtime imports — same constraint as auth/vault/session),
//! export it from `lib.rs` (`pub mod rate_limit;`), and NEVER edit this file.
//! Search throttling lives in `workers/ingest` — see
//! `workers/ingest/tests/search_rate_limits_acceptance.rs` on the same branch.
//!
//! Failing command:
//!   `cd workers/api && cargo test --test rate_limits_acceptance`
//!
//! Contract under test (implement exactly these items):
//! - `TokenBucket::new(capacity: f64, refill_per_sec: f64, now_s: u64)`
//!   (starts full; negative inputs clamp to 0),
//!   `TokenBucket::per_window(n, window_s, now_s)`,
//!   `take(now_s, n) -> bool`
//!   (take only on success — a failed take consumes nothing),
//!   `remaining(now_s) -> f64` (pure view, no mutation),
//!   `retry_after_secs(now_s) -> u64` (pure view: 0 when a token is available,
//!   else `ceil((1 - available) / refill_per_sec)` with a minimum of 1;
//!   a bucket that can never refill returns `u64::MAX`).
//! - `ThrottleDecision::{Allow, Deny { retry_after_secs }}` (derive `Debug`
//!   at least) and
//!   `check_dual_throttle(ip: &mut TokenBucket, account: &mut TokenBucket,
//!   now_s: u64) -> ThrottleDecision` — deny when EITHER bucket is empty;
//!   allowed attempts consume one token from BOTH; denied attempts consume
//!   NOTHING from either; the denial retry-after is the max of both buckets.
//! - `RateLimitConfig { auth_max_attempts: u32, auth_window_secs: u64,
//!   vault_test_max_attempts: u32, vault_test_window_secs: u64 }`
//!   (`Debug + Clone + Copy + PartialEq + Eq`), `Default` = the documented
//!   consts, `from_vars(&HashMap<String, String>) -> Self`.
//! - `AUTH_MAX_ATTEMPTS / AUTH_WINDOW_SECS / VAULT_TEST_MAX_ATTEMPTS /
//!   VAULT_TEST_WINDOW_SECS`, `RATE_LIMIT_STATUS == 429`,
//!   `RETRY_AFTER_HEADER == "retry-after"`,
//!   `retry_after_header_value(secs: u64) -> String` (decimal seconds),
//!   `rate_limited_body() -> String` (ZERO args: no per-request data can leak
//!   into the body; must be `{"error":"too many requests"}`).
//! - `ThrottleKey::new(ip: &str, account: &str)` with a `Debug` impl that
//!   NEVER prints the IP in cleartext (mirror the vault redaction rule).
//!
//! Documented defaults (provisional launch tunables; override via [vars]/KV,
//! never hardcoded at the call site):
//! | surface    | max | window | vars                                                      |
//! |------------|-----|--------|-----------------------------------------------------------|
//! | auth       | 10  | 60s    | AUTH_RATE_LIMIT_MAX / AUTH_RATE_LIMIT_WINDOW_SECS         |
//! | vault test | 5   | 60s    | VAULT_TEST_RATE_LIMIT_MAX / VAULT_TEST_RATE_LIMIT_WINDOW_SECS |
//! (vault-test default matches the existing
//! `TEST_CONNECTION_MAX_ATTEMPTS / TEST_CONNECTION_WINDOW_SECS`.)
//! `from_vars` falls back to these defaults on missing, unparseable, or zero
//! values: invalid config keeps limiting on (never unlimited, never deny-all).
//!
//! Burst-script expectation (what the Builder's PR burst script must show;
//! the `burst_*` tests below are its executable form):
//! ```sh
//! for i in $(seq 1 11); do
//!   curl -s -o /dev/null -w "%{http_code} %{header_json}" "$API/login"
//! done
//! # expected: ten non-429s (200s — or 401s for bad creds, still counted),
//! # then 429 with `Retry-After: <secs>`; a single legit attempt from a
//! # fresh key still succeeds.
//! ```
//!
//! Fixtures are synthetic (TEST-NET-3 IPs, fake accounts). No secrets, no real
//! IPs, no credential material anywhere in this file.

use nntune_api::auth::AuthError;
use nntune_api::rate_limit::{
    check_dual_throttle, rate_limited_body, retry_after_header_value, RateLimitConfig,
    ThrottleDecision, ThrottleKey, TokenBucket, AUTH_MAX_ATTEMPTS, AUTH_WINDOW_SECS,
    RATE_LIMIT_STATUS, RETRY_AFTER_HEADER, VAULT_TEST_MAX_ATTEMPTS, VAULT_TEST_WINDOW_SECS,
};
use std::collections::HashMap;

const NOW: u64 = 1_700_000_000;
const TEST_IP: &str = "203.0.113.7";
const TEST_ACCOUNT: &str = "user-acceptance";

fn is_allow(d: &ThrottleDecision) -> bool {
    matches!(d, ThrottleDecision::Allow)
}

fn denied_retry(d: &ThrottleDecision) -> u64 {
    match d {
        ThrottleDecision::Deny { retry_after_secs } => *retry_after_secs,
        ThrottleDecision::Allow => panic!("expected Deny, got Allow"),
    }
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn auth_buckets(cfg: &RateLimitConfig, now: u64) -> (TokenBucket, TokenBucket) {
    (
        TokenBucket::per_window(cfg.auth_max_attempts, cfg.auth_window_secs, now),
        TokenBucket::per_window(cfg.auth_max_attempts, cfg.auth_window_secs, now),
    )
}

fn vault_buckets(cfg: &RateLimitConfig, now: u64) -> (TokenBucket, TokenBucket) {
    (
        TokenBucket::per_window(cfg.vault_test_max_attempts, cfg.vault_test_window_secs, now),
        TokenBucket::per_window(cfg.vault_test_max_attempts, cfg.vault_test_window_secs, now),
    )
}

// --- Bucket math ------------------------------------------------------------
// NOTE: exact `retry_after_secs` / `remaining` values are asserted only on
// binary-exact buckets (capacity 4, window 8, rate 0.5/s). Decimal-rate
// buckets (10/60) assert `>= 1` only — `ceil` on inexact floats must not be
// pinned to a single integer here.

#[test]
fn bucket_burst_then_denies_without_overdraft() {
    let mut b = TokenBucket::per_window(4, 8, NOW);
    for _ in 0..4 {
        assert!(b.take(NOW, 1.0));
    }
    assert!(!b.take(NOW, 1.0), "exhausted bucket must deny");
    assert_eq!(b.remaining(NOW), 0.0);
    // A take larger than capacity fails and consumes nothing.
    let mut full = TokenBucket::per_window(4, 8, NOW);
    assert!(!full.take(NOW, 5.0));
    assert_eq!(full.remaining(NOW), 4.0);
}

#[test]
fn bucket_refills_after_window_and_recovers_full_burst() {
    let mut b = TokenBucket::per_window(4, 8, NOW);
    for _ in 0..4 {
        assert!(b.take(NOW, 1.0));
    }
    assert!(!b.take(NOW, 1.0));
    // Half window: exactly half the tokens are back (rate 0.5/s is exact).
    assert_eq!(b.remaining(NOW + 4), 2.0);
    assert!(b.take(NOW + 4, 1.0));
    assert!(b.take(NOW + 4, 1.0));
    assert!(!b.take(NOW + 4, 1.0));
    // Full window from exhaustion: the whole burst is back.
    let mut c = TokenBucket::per_window(4, 8, NOW);
    for _ in 0..4 {
        assert!(c.take(NOW, 1.0));
    }
    for _ in 0..4 {
        assert!(c.take(NOW + 8, 1.0), "window must fully refill");
    }
    assert!(!c.take(NOW + 8, 1.0));
}

#[test]
fn bucket_backwards_clock_never_grants_extra() {
    let mut b = TokenBucket::per_window(4, 8, NOW);
    for _ in 0..4 {
        assert!(b.take(NOW, 1.0));
    }
    assert!(!b.take(NOW, 1.0));
    assert!(!b.take(NOW - 1_000, 1.0), "clock skew must not refill");
    assert_eq!(b.remaining(NOW - 1_000), 0.0);
}

#[test]
fn bucket_retry_after_counts_down_to_next_token() {
    let mut b = TokenBucket::per_window(4, 8, NOW);
    assert_eq!(b.retry_after_secs(NOW), 0, "full bucket: no wait");
    assert!(b.take(NOW, 3.0));
    assert_eq!(b.retry_after_secs(NOW), 0, "one token left: no wait");
    assert!(b.take(NOW, 1.0));
    // Empty, rate 0.5/s: next token in exactly 2s.
    assert_eq!(b.retry_after_secs(NOW), 2);
    assert_eq!(b.retry_after_secs(NOW + 1), 1);
    assert_eq!(b.retry_after_secs(NOW + 2), 0);
}

#[test]
fn bucket_that_can_never_refill_reports_max_wait() {
    let b = TokenBucket::new(0.0, 0.0, NOW);
    assert_eq!(b.retry_after_secs(NOW), u64::MAX);
}

// --- Dual gate: per-IP AND per-account --------------------------------------

#[test]
fn dual_gate_allows_single_legit_flow_and_counts_one_each() {
    let (mut ip, mut account) = auth_buckets(&RateLimitConfig::default(), NOW);
    assert!(is_allow(&check_dual_throttle(&mut ip, &mut account, NOW)));
    assert_eq!(ip.remaining(NOW), 9.0);
    assert_eq!(account.remaining(NOW), 9.0);
}

#[test]
fn dual_gate_denies_when_either_bucket_is_empty() {
    // IP exhausted, account fresh -> deny (one compromised host cannot burn
    // through on a fresh account).
    let mut ip = TokenBucket::per_window(1, 60, NOW);
    let mut account = TokenBucket::per_window(100, 60, NOW);
    assert!(ip.take(NOW, 1.0));
    let d = check_dual_throttle(&mut ip, &mut account, NOW);
    assert!(!is_allow(&d));
    assert!(denied_retry(&d) >= 1);
    // Account exhausted, IP fresh -> deny (credential-stuffing one account
    // from many IPs still trips the account bucket).
    let mut ip2 = TokenBucket::per_window(100, 60, NOW);
    let mut account2 = TokenBucket::per_window(1, 60, NOW);
    assert!(account2.take(NOW, 1.0));
    let d2 = check_dual_throttle(&mut ip2, &mut account2, NOW);
    assert!(!is_allow(&d2));
    assert!(denied_retry(&d2) >= 1);
}

#[test]
fn dual_gate_denial_retry_after_is_the_max_of_both_buckets() {
    // IP bucket (4,8): empty -> 2s. Account bucket exhausted longer-window
    // bucket (1,60): empty -> 60s. Denial must carry the longer wait.
    let mut ip = TokenBucket::per_window(4, 8, NOW);
    for _ in 0..4 {
        assert!(ip.take(NOW, 1.0));
    }
    let mut account = TokenBucket::per_window(1, 60, NOW);
    assert!(account.take(NOW, 1.0));
    let d = check_dual_throttle(&mut ip, &mut account, NOW);
    assert_eq!(denied_retry(&d), 60);
}

#[test]
fn dual_gate_denied_attempt_consumes_nothing() {
    let mut ip = TokenBucket::per_window(2, 8, NOW);
    let mut account = TokenBucket::per_window(1, 8, NOW);
    assert!(is_allow(&check_dual_throttle(&mut ip, &mut account, NOW)));
    assert_eq!((ip.remaining(NOW), account.remaining(NOW)), (1.0, 0.0));
    // Account bucket is empty: deny, and the surviving IP token must NOT burn.
    assert!(!is_allow(&check_dual_throttle(&mut ip, &mut account, NOW)));
    assert!(!is_allow(&check_dual_throttle(&mut ip, &mut account, NOW)));
    assert_eq!(ip.remaining(NOW), 1.0, "denials must not consume");
    assert_eq!(account.remaining(NOW), 0.0);
}

// --- Burst-script expectations (executable form) ----------------------------

#[test]
fn burst_auth_gets_429_while_single_legit_flow_passes() {
    let cfg = RateLimitConfig::default();
    let (mut ip, mut account) = auth_buckets(&cfg, NOW);
    for i in 0..cfg.auth_max_attempts {
        assert!(
            is_allow(&check_dual_throttle(&mut ip, &mut account, NOW)),
            "attempt {i} of the burst must pass"
        );
    }
    // Burst over the cap: 429 with a real Retry-After.
    for _ in 0..3 {
        let d = check_dual_throttle(&mut ip, &mut account, NOW);
        assert!(!is_allow(&d), "burst past the cap must be denied");
        assert!(denied_retry(&d) >= 1, "denial must carry Retry-After");
    }
    // A single legit flow from a fresh key is unaffected.
    let (mut fresh_ip, mut fresh_account) = auth_buckets(&cfg, NOW);
    assert!(is_allow(&check_dual_throttle(
        &mut fresh_ip,
        &mut fresh_account,
        NOW
    )));
}

#[test]
fn burst_vault_test_connection_gets_429_while_single_legit_flow_passes() {
    let cfg = RateLimitConfig::default();
    let (mut ip, mut account) = vault_buckets(&cfg, NOW);
    for i in 0..cfg.vault_test_max_attempts {
        assert!(
            is_allow(&check_dual_throttle(&mut ip, &mut account, NOW)),
            "attempt {i} of the burst must pass"
        );
    }
    let d = check_dual_throttle(&mut ip, &mut account, NOW);
    assert!(!is_allow(&d), "6th test-connection in the window must deny");
    assert!(denied_retry(&d) >= 1);
    let (mut fresh_ip, mut fresh_account) = vault_buckets(&cfg, NOW);
    assert!(is_allow(&check_dual_throttle(
        &mut fresh_ip,
        &mut fresh_account,
        NOW
    )));
}

#[test]
fn throttled_key_recovers_after_window_for_one_legit_attempt() {
    let cfg = RateLimitConfig::default();
    let (mut ip, mut account) = vault_buckets(&cfg, NOW);
    for _ in 0..cfg.vault_test_max_attempts {
        assert!(is_allow(&check_dual_throttle(&mut ip, &mut account, NOW)));
    }
    assert!(!is_allow(&check_dual_throttle(&mut ip, &mut account, NOW)));
    // One window later the next legit attempt passes (single take, not a full
    // burst — decimal-rate refill is not pinned to an exact full recovery).
    assert!(is_allow(&check_dual_throttle(
        &mut ip,
        &mut account,
        NOW + cfg.vault_test_window_secs
    )));
}

// --- 429 / Retry-After wiring ------------------------------------------------

#[test]
fn rate_limited_status_is_429_with_retry_after_header() {
    assert_eq!(RATE_LIMIT_STATUS, 429);
    assert_eq!(RETRY_AFTER_HEADER, "retry-after");
    assert_eq!(retry_after_header_value(2), "2");
    assert_eq!(retry_after_header_value(0), "0");
    assert_eq!(retry_after_header_value(3_600), "3600");
}

#[test]
fn rate_limited_body_is_generic_and_leaks_nothing() {
    // Zero-arg by construction: per-request data (IP, account, tokens,
    // passwords, keys) cannot flow into this body.
    let body = rate_limited_body();
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("body must be JSON");
    assert_eq!(parsed["error"], "too many requests");
    for secret in [
        TEST_IP,
        TEST_ACCOUNT,
        "s3cret",
        "super-secret-pw",
        "test-api-key-1",
        "acceptance-magic-token",
    ] {
        assert!(
            !body.contains(secret),
            "rate-limit body must not echo per-request data: {secret}"
        );
    }
}

// --- Config, not hardcoded ---------------------------------------------------

#[test]
fn defaults_are_documented_and_pinned() {
    assert_eq!(AUTH_MAX_ATTEMPTS, 10);
    assert_eq!(AUTH_WINDOW_SECS, 60);
    assert_eq!(VAULT_TEST_MAX_ATTEMPTS, 5);
    assert_eq!(VAULT_TEST_WINDOW_SECS, 60);
    let cfg = RateLimitConfig::default();
    assert_eq!(cfg.auth_max_attempts, AUTH_MAX_ATTEMPTS);
    assert_eq!(cfg.auth_window_secs, AUTH_WINDOW_SECS);
    assert_eq!(cfg.vault_test_max_attempts, VAULT_TEST_MAX_ATTEMPTS);
    assert_eq!(cfg.vault_test_window_secs, VAULT_TEST_WINDOW_SECS);
}

#[test]
fn limits_come_from_vars_not_hardcode() {
    // Overrides apply per surface; untouched surfaces keep defaults.
    let cfg = RateLimitConfig::from_vars(&vars(&[
        ("AUTH_RATE_LIMIT_MAX", "3"),
        ("AUTH_RATE_LIMIT_WINDOW_SECS", "120"),
    ]));
    assert_eq!(cfg.auth_max_attempts, 3);
    assert_eq!(cfg.auth_window_secs, 120);
    assert_eq!(cfg.vault_test_max_attempts, VAULT_TEST_MAX_ATTEMPTS);
    assert_eq!(cfg.vault_test_window_secs, VAULT_TEST_WINDOW_SECS);
    // Overridden limits actually gate: 3 passes, 4th denies.
    let (mut ip, mut account) = (
        TokenBucket::per_window(cfg.auth_max_attempts, cfg.auth_window_secs, NOW),
        TokenBucket::per_window(cfg.auth_max_attempts, cfg.auth_window_secs, NOW),
    );
    for _ in 0..3 {
        assert!(is_allow(&check_dual_throttle(&mut ip, &mut account, NOW)));
    }
    assert!(!is_allow(&check_dual_throttle(&mut ip, &mut account, NOW)));
}

#[test]
fn invalid_config_fails_closed_to_documented_defaults() {
    // Missing -> defaults.
    assert_eq!(
        RateLimitConfig::from_vars(&HashMap::new()),
        RateLimitConfig::default()
    );
    // Garbage, empty, or zero values must NOT disable limiting (never
    // unlimited) and must NOT deny everything (never deny-all): fall back.
    for pairs in [
        &[("AUTH_RATE_LIMIT_MAX", "abc")][..],
        &[("AUTH_RATE_LIMIT_MAX", "")][..],
        &[("AUTH_RATE_LIMIT_MAX", "0")][..],
        &[("AUTH_RATE_LIMIT_WINDOW_SECS", "-5")][..],
        &[("VAULT_TEST_RATE_LIMIT_MAX", "unlimited")][..],
        &[("VAULT_TEST_RATE_LIMIT_WINDOW_SECS", "0")][..],
    ] {
        let cfg = RateLimitConfig::from_vars(&vars(pairs));
        assert_eq!(
            cfg,
            RateLimitConfig::default(),
            "invalid vars must fall back to defaults: {pairs:?}"
        );
    }
}

// --- No credential or IP in cleartext ----------------------------------------

#[test]
fn throttle_keys_never_print_ip_in_cleartext() {
    let key = ThrottleKey::new(TEST_IP, TEST_ACCOUNT);
    let dumped = format!("{key:?}");
    assert!(
        !dumped.contains(TEST_IP),
        "throttle key Debug leaked the IP: {dumped}"
    );
    // Partial material must not leak either: the last two octets never appear
    // in any other fixture string in this file.
    assert!(
        !dumped.contains("113.7"),
        "throttle key Debug leaked IP material: {dumped}"
    );
    // Login errors stay generic even under throttle pressure.
    assert_eq!(
        AuthError::Invalid.to_string(),
        "check your email and try again"
    );
}
