//! Acceptance tests: rate limits and abuse guards on Newznab search.
//!
//! TDD RED for [TOG-25181](/TOG/issues/TOG-25181) (roadmap R6 from
//! [TOG-25148](/TOG/issues/TOG-25148)). These tests FAIL until
//! `nntune_ingest::rate_limit` implements the search-throttle contract below.
//!
//! Builder: extend `workers/ingest/src/rate_limit.rs` (keep `TokenBucket` +
//! `PlanLimits` intact; add the items listed here), and NEVER edit this file.
//! Auth + vault test-connection throttling lives in `workers/api` — see
//! `workers/api/tests/rate_limits_acceptance.rs` on the same branch.
//!
//! Failing command (this card):
//!   `cd workers/ingest && cargo test --test search_rate_limits_acceptance`
//! Branch:
//!   `TOG-25191-acceptance-tests-rate-limits-and-abuse-guards-on-auth-vault-and-search`
//!
//! Contract under test (implement exactly these items; `TokenBucket` keeps its
//! existing `new` / `per_window` / `take` semantics — take only on success):
//! - `TokenBucket::remaining(now_s) -> f64` (pure view, no mutation) and
//!   `TokenBucket::retry_after_secs(now_s) -> u64` (pure view: 0 when a token
//!   is available, else `ceil((1 - available) / refill_per_sec)` minimum 1;
//!   never-refilling bucket returns `u64::MAX`).
//! - `SearchDecision::{Allow, Deny { retry_after_secs }}` (derive `Debug` at
//!   least) and
//!   `check_search_throttle(ip: &mut TokenBucket, account: &mut TokenBucket,
//!   now_s: u64) -> SearchDecision` — deny when EITHER bucket is empty;
//!   allowed attempts consume one token from BOTH; denied attempts consume
//!   NOTHING; denial retry-after is the max of both buckets.
//! - `SearchRateLimitConfig { max_attempts: u32, window_secs: u64 }`
//!   (`Debug + Clone + Copy + PartialEq + Eq`), `Default` = the documented
//!   consts, `from_vars(&HashMap<String, String>) -> Self`.
//! - `SEARCH_MAX_ATTEMPTS / SEARCH_WINDOW_SECS / SEARCH_RATE_LIMIT_STATUS
//!   (== 429) / RETRY_AFTER_HEADER (== "retry-after")`,
//!   `search_retry_after_header(secs: u64) -> String` (decimal seconds),
//!   `search_rate_limited_body() -> String` (ZERO args: no per-request data
//!   can leak in; must be `{"error":"too many requests"}`).
//! - `SearchThrottleKey::new(ip: &str, account: &str)` with a `Debug` impl
//!   that NEVER prints the IP in cleartext (keys are vault-resolved
//!   server-side; logs carry the redacted URL only).
//!
//! Documented defaults (provisional launch tunables; override via [vars]/KV,
//! never hardcoded at the call site):
//! | surface | max | window | vars                                            |
//! |---------|-----|--------|-------------------------------------------------|
//! | search  | 10  | 60s    | SEARCH_RATE_LIMIT_MAX / SEARCH_RATE_LIMIT_WINDOW_SECS |
//! (default max tracks `PLAN_BASE.searches_per_min` — pinned below.)
//! `from_vars` falls back to these defaults on missing, unparseable, or zero
//! values: invalid config keeps limiting on (never unlimited, never deny-all).
//!
//! Burst-script expectation (what the Builder's PR burst script must show;
//! the `burst_*` tests below are its executable form):
//! ```sh
//! for i in $(seq 1 11); do
//!   curl -s -o /dev/null -w "%{http_code}\n" \
//!     -H "Content-Type: application/json" \
//!     -d '{"user_id":"u","query":"x","indexer_ref":"vault_idx_01-ab","categories":[],"limit":25,"page":0}' \
//!     "$INGEST/v1/ingest/search"
//! done
//! # expected: ten non-429s, then 429 with `Retry-After: <secs>`; a single
//! # legit search from a fresh key still succeeds.
//! ```
//!
//! Fixtures are synthetic (TEST-NET-3 IPs, fake accounts). No secrets, no real
//! IPs, no credential material anywhere in this file.

use nntune_ingest::rate_limit::{
    check_search_throttle, search_rate_limited_body, search_retry_after_header, PlanLimits,
    SearchDecision, SearchRateLimitConfig, SearchThrottleKey, TokenBucket, PLAN_BASE,
    RETRY_AFTER_HEADER, SEARCH_MAX_ATTEMPTS, SEARCH_RATE_LIMIT_STATUS, SEARCH_WINDOW_SECS,
};
use std::collections::HashMap;

const NOW: u64 = 1_700_000_000;
const TEST_IP: &str = "203.0.113.9";
const TEST_ACCOUNT: &str = "user-acceptance";

fn is_allow(d: &SearchDecision) -> bool {
    matches!(d, SearchDecision::Allow)
}

fn denied_retry(d: &SearchDecision) -> u64 {
    match d {
        SearchDecision::Deny { retry_after_secs } => *retry_after_secs,
        SearchDecision::Allow => panic!("expected Deny, got Allow"),
    }
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn search_buckets(cfg: &SearchRateLimitConfig, now: u64) -> (TokenBucket, TokenBucket) {
    (
        TokenBucket::per_window(cfg.max_attempts, cfg.window_secs, now),
        TokenBucket::per_window(cfg.max_attempts, cfg.window_secs, now),
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
    let mut full = TokenBucket::per_window(4, 8, NOW);
    assert!(!full.take(NOW, 5.0), "over-capacity take must fail");
    assert_eq!(full.remaining(NOW), 4.0);
}

#[test]
fn bucket_refills_after_window_and_recovers_full_burst() {
    let mut b = TokenBucket::per_window(4, 8, NOW);
    for _ in 0..4 {
        assert!(b.take(NOW, 1.0));
    }
    assert_eq!(b.remaining(NOW + 4), 2.0);
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
    assert_eq!(b.retry_after_secs(NOW), 2);
    assert_eq!(b.retry_after_secs(NOW + 1), 1);
    assert_eq!(b.retry_after_secs(NOW + 2), 0);
}

// --- Dual gate: per-IP AND per-account --------------------------------------

#[test]
fn dual_gate_allows_single_legit_search_and_counts_one_each() {
    let cfg = SearchRateLimitConfig::default();
    let (mut ip, mut account) = search_buckets(&cfg, NOW);
    assert!(is_allow(&check_search_throttle(&mut ip, &mut account, NOW)));
    assert_eq!(ip.remaining(NOW), 9.0);
    assert_eq!(account.remaining(NOW), 9.0);
}

#[test]
fn dual_gate_denies_when_either_bucket_is_empty() {
    // One scraper host burning many accounts still trips the IP bucket.
    let mut ip = TokenBucket::per_window(1, 60, NOW);
    let mut account = TokenBucket::per_window(100, 60, NOW);
    assert!(ip.take(NOW, 1.0));
    let d = check_search_throttle(&mut ip, &mut account, NOW);
    assert!(!is_allow(&d));
    assert!(denied_retry(&d) >= 1);
    // One account scraped from many hosts still trips the account bucket.
    let mut ip2 = TokenBucket::per_window(100, 60, NOW);
    let mut account2 = TokenBucket::per_window(1, 60, NOW);
    assert!(account2.take(NOW, 1.0));
    let d2 = check_search_throttle(&mut ip2, &mut account2, NOW);
    assert!(!is_allow(&d2));
    assert!(denied_retry(&d2) >= 1);
}

#[test]
fn dual_gate_denied_attempt_consumes_nothing() {
    let mut ip = TokenBucket::per_window(2, 8, NOW);
    let mut account = TokenBucket::per_window(1, 8, NOW);
    assert!(is_allow(&check_search_throttle(&mut ip, &mut account, NOW)));
    assert_eq!((ip.remaining(NOW), account.remaining(NOW)), (1.0, 0.0));
    assert!(!is_allow(&check_search_throttle(
        &mut ip,
        &mut account,
        NOW
    )));
    assert!(!is_allow(&check_search_throttle(
        &mut ip,
        &mut account,
        NOW
    )));
    assert_eq!(ip.remaining(NOW), 1.0, "denials must not consume");
    assert_eq!(account.remaining(NOW), 0.0);
}

// --- Burst-script expectations (executable form) ----------------------------

#[test]
fn burst_search_gets_429_while_single_legit_search_passes() {
    let cfg = SearchRateLimitConfig::default();
    let (mut ip, mut account) = search_buckets(&cfg, NOW);
    for i in 0..cfg.max_attempts {
        assert!(
            is_allow(&check_search_throttle(&mut ip, &mut account, NOW)),
            "search {i} of the burst must pass"
        );
    }
    for _ in 0..3 {
        let d = check_search_throttle(&mut ip, &mut account, NOW);
        assert!(!is_allow(&d), "search past the cap must be denied");
        assert!(denied_retry(&d) >= 1, "denial must carry Retry-After");
    }
    let (mut fresh_ip, mut fresh_account) = search_buckets(&cfg, NOW);
    assert!(is_allow(&check_search_throttle(
        &mut fresh_ip,
        &mut fresh_account,
        NOW
    )));
}

#[test]
fn throttled_searcher_recovers_after_window_for_one_legit_search() {
    let cfg = SearchRateLimitConfig::default();
    let (mut ip, mut account) = search_buckets(&cfg, NOW);
    for _ in 0..cfg.max_attempts {
        assert!(is_allow(&check_search_throttle(&mut ip, &mut account, NOW)));
    }
    assert!(!is_allow(&check_search_throttle(
        &mut ip,
        &mut account,
        NOW
    )));
    assert!(is_allow(&check_search_throttle(
        &mut ip,
        &mut account,
        NOW + cfg.window_secs
    )));
}

// --- 429 / Retry-After wiring ------------------------------------------------

#[test]
fn rate_limited_status_is_429_with_retry_after_header() {
    assert_eq!(SEARCH_RATE_LIMIT_STATUS, 429);
    assert_eq!(RETRY_AFTER_HEADER, "retry-after");
    assert_eq!(search_retry_after_header(2), "2");
    assert_eq!(search_retry_after_header(0), "0");
    assert_eq!(search_retry_after_header(3_600), "3600");
}

#[test]
fn rate_limited_body_is_generic_and_leaks_nothing() {
    // Zero-arg by construction: the vault-resolved indexer key, the query,
    // and the caller identity cannot flow into this body.
    let body = search_rate_limited_body();
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("body must be JSON");
    assert_eq!(parsed["error"], "too many requests");
    for secret in [
        TEST_IP,
        TEST_ACCOUNT,
        "test-api-key-1",
        "super-secret-key",
        "dark side moon",
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
    assert_eq!(SEARCH_MAX_ATTEMPTS, 10);
    assert_eq!(SEARCH_WINDOW_SECS, 60);
    let cfg = SearchRateLimitConfig::default();
    assert_eq!(cfg.max_attempts, SEARCH_MAX_ATTEMPTS);
    assert_eq!(cfg.window_secs, SEARCH_WINDOW_SECS);
}

#[test]
fn default_search_quota_tracks_base_plan() {
    // The throttle default is the base-tier search quota, not a second
    // number to drift: per-tier quotas stay in `PlanLimits`.
    assert_eq!(
        SearchRateLimitConfig::default().max_attempts,
        PLAN_BASE.searches_per_min
    );
    let _: PlanLimits = PLAN_BASE;
}

#[test]
fn limits_come_from_vars_not_hardcode() {
    let cfg = SearchRateLimitConfig::from_vars(&vars(&[
        ("SEARCH_RATE_LIMIT_MAX", "3"),
        ("SEARCH_RATE_LIMIT_WINDOW_SECS", "120"),
    ]));
    assert_eq!(cfg.max_attempts, 3);
    assert_eq!(cfg.window_secs, 120);
    let (mut ip, mut account) = search_buckets(&cfg, NOW);
    for _ in 0..3 {
        assert!(is_allow(&check_search_throttle(&mut ip, &mut account, NOW)));
    }
    assert!(!is_allow(&check_search_throttle(
        &mut ip,
        &mut account,
        NOW
    )));
}

#[test]
fn invalid_config_fails_closed_to_documented_defaults() {
    assert_eq!(
        SearchRateLimitConfig::from_vars(&HashMap::new()),
        SearchRateLimitConfig::default()
    );
    for pairs in [
        &[("SEARCH_RATE_LIMIT_MAX", "abc")][..],
        &[("SEARCH_RATE_LIMIT_MAX", "")][..],
        &[("SEARCH_RATE_LIMIT_MAX", "0")][..],
        &[("SEARCH_RATE_LIMIT_WINDOW_SECS", "-5")][..],
        &[("SEARCH_RATE_LIMIT_WINDOW_SECS", "0")][..],
    ] {
        let cfg = SearchRateLimitConfig::from_vars(&vars(pairs));
        assert_eq!(
            cfg,
            SearchRateLimitConfig::default(),
            "invalid vars must fall back to defaults: {pairs:?}"
        );
    }
}

// --- No credential or IP in cleartext ----------------------------------------

#[test]
fn throttle_keys_never_print_ip_in_cleartext() {
    let key = SearchThrottleKey::new(TEST_IP, TEST_ACCOUNT);
    let dumped = format!("{key:?}");
    assert!(
        !dumped.contains(TEST_IP),
        "throttle key Debug leaked the IP: {dumped}"
    );
    assert!(
        !dumped.contains("113.9"),
        "throttle key Debug leaked IP material: {dumped}"
    );
}
