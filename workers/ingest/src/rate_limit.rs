//! Per-plan rate limits for ingest (token buckets, no clock dependency).
//!
//! Limits are provisional launch tunables, not product promises: the README
//! plans section says exact numbers are set at launch. The metering card
//! owns billing truth; this module only protects provider connections and
//! indexer quotas inside the worker. Time is passed in as `now_s` (unix
//! seconds) so the logic is unit-testable without a clock.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::api_error::ApiError;

/// Quotas attached to one plan tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanLimits {
    /// Concurrent non-terminal jobs per user.
    pub max_concurrent_jobs: u32,
    /// Newznab search requests per minute per user.
    pub searches_per_min: u32,
    /// NZB uploads per hour per user.
    pub nzb_per_hour: u32,
}

/// Provisional tiers matching the base / 5x / 20x plan shape.
pub const PLAN_BASE: PlanLimits = PlanLimits {
    max_concurrent_jobs: 2,
    searches_per_min: 10,
    nzb_per_hour: 20,
};
pub const PLAN_5X: PlanLimits = PlanLimits {
    max_concurrent_jobs: 6,
    searches_per_min: 40,
    nzb_per_hour: 80,
};
pub const PLAN_20X: PlanLimits = PlanLimits {
    max_concurrent_jobs: 12,
    searches_per_min: 120,
    nzb_per_hour: 240,
};

/// Token bucket with explicit time: refill on every [`TokenBucket::take`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last_tick_s: u64,
}

impl TokenBucket {
    /// `capacity`: burst size; `refill_per_sec`: sustained rate.
    /// The bucket starts full.
    pub fn new(capacity: f64, refill_per_sec: f64, now_s: u64) -> Self {
        Self {
            capacity: capacity.max(0.0),
            tokens: capacity.max(0.0),
            refill_per_sec: refill_per_sec.max(0.0),
            last_tick_s: now_s,
        }
    }

    /// Build from "N events per `window_s` seconds" (burst = N).
    pub fn per_window(n: u32, window_s: u64, now_s: u64) -> Self {
        let rate = if window_s == 0 {
            0.0
        } else {
            f64::from(n) / window_s as f64
        };
        Self::new(f64::from(n), rate, now_s)
    }

    /// Take `n` tokens if available. Clock skew backwards = no refill.
    pub fn take(&mut self, now_s: u64, n: f64) -> bool {
        if now_s > self.last_tick_s {
            let dt = (now_s - self.last_tick_s) as f64;
            self.tokens = (self.tokens + dt * self.refill_per_sec).min(self.capacity);
            self.last_tick_s = now_s;
        }
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }

    /// Tokens available at `now_s`. Pure view: never mutates. Clock skew
    /// backwards grants nothing (availability is the stored level).
    pub fn remaining(&self, now_s: u64) -> f64 {
        if now_s > self.last_tick_s {
            let dt = (now_s - self.last_tick_s) as f64;
            (self.tokens + dt * self.refill_per_sec).min(self.capacity)
        } else {
            self.tokens
        }
    }

    /// Seconds until the next whole token is available. Pure view. 0 when a
    /// token is available now; otherwise `ceil((1 - available) / refill)`
    /// with a minimum of 1; a bucket that can never refill returns
    /// `u64::MAX`. A 1e-9 epsilon keeps binary-inexact rates (e.g. 1/60)
    /// from rounding an exact-integer wait up by one second.
    pub fn retry_after_secs(&self, now_s: u64) -> u64 {
        let available = self.remaining(now_s);
        if available >= 1.0 {
            return 0;
        }
        if self.refill_per_sec <= 0.0 {
            return u64::MAX;
        }
        let wait = (1.0 - available) / self.refill_per_sec;
        (wait - 1e-9).ceil().max(1.0) as u64
    }
}

// --- Newznab search throttle (roadmap R6) -------------------------------------
//
// Documented defaults (provisional launch tunables; override via [vars],
// never hardcoded at the call site):
// | surface | max | window | vars                                            |
// |---------|-----|--------|-------------------------------------------------|
// | search  | 10  | 60s    | SEARCH_RATE_LIMIT_MAX / SEARCH_RATE_LIMIT_WINDOW_SECS |
// The default max IS the base-tier search quota (`PLAN_BASE.searches_per_min`),
// not a second number to drift; per-tier quotas stay in `PlanLimits`.
// `from_vars` falls back to the defaults on missing, unparseable, or zero
// values: invalid config keeps limiting on (never unlimited, never deny-all).

/// Search burst: the base-tier per-minute quota, single-sourced.
pub const SEARCH_MAX_ATTEMPTS: u32 = PLAN_BASE.searches_per_min;
/// Search window, seconds.
pub const SEARCH_WINDOW_SECS: u64 = 60;
/// HTTP status for a throttled search response.
pub const SEARCH_RATE_LIMIT_STATUS: u16 = 429;
/// `Retry-After` header name (lowercase, as rendered).
pub const RETRY_AFTER_HEADER: &str = "retry-after";

/// Outcome of a search throttle check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDecision {
    Allow,
    Deny { retry_after_secs: u64 },
}

/// Dual gate: deny when EITHER the per-IP or the per-account bucket is
/// empty. An allowed attempt consumes one token from BOTH buckets; a denied
/// attempt consumes NOTHING from either (the check peeks via the pure
/// [`TokenBucket::remaining`] view first, so no take is half-applied). The
/// denial retry-after is the max of both buckets' waits.
pub fn check_search_throttle(
    ip: &mut TokenBucket,
    account: &mut TokenBucket,
    now_s: u64,
) -> SearchDecision {
    if ip.remaining(now_s) >= 1.0 && account.remaining(now_s) >= 1.0 {
        // Both peeks passed at the same `now_s`, so both takes succeed.
        let _ = ip.take(now_s, 1.0);
        let _ = account.take(now_s, 1.0);
        SearchDecision::Allow
    } else {
        SearchDecision::Deny {
            retry_after_secs: ip
                .retry_after_secs(now_s)
                .max(account.retry_after_secs(now_s)),
        }
    }
}

/// Search rate-limit configuration. `Default` is the documented table
/// above; [`SearchRateLimitConfig::from_vars`] overlays [vars] overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchRateLimitConfig {
    pub max_attempts: u32,
    pub window_secs: u64,
}

impl Default for SearchRateLimitConfig {
    fn default() -> Self {
        Self {
            max_attempts: SEARCH_MAX_ATTEMPTS,
            window_secs: SEARCH_WINDOW_SECS,
        }
    }
}

fn parse_nonzero<T: std::str::FromStr + Default + PartialEq>(
    vars: &HashMap<String, String>,
    key: &str,
    fallback: T,
) -> T {
    match vars.get(key).and_then(|v| v.parse::<T>().ok()) {
        Some(value) if value != T::default() => value,
        _ => fallback,
    }
}

impl SearchRateLimitConfig {
    /// Build from worker [vars]. Missing, unparseable, or zero values fall
    /// back per-field to the documented defaults: invalid config keeps
    /// limiting on (never unlimited, never deny-all).
    pub fn from_vars(vars: &HashMap<String, String>) -> Self {
        let defaults = Self::default();
        Self {
            max_attempts: parse_nonzero(vars, "SEARCH_RATE_LIMIT_MAX", defaults.max_attempts),
            window_secs: parse_nonzero(vars, "SEARCH_RATE_LIMIT_WINDOW_SECS", defaults.window_secs),
        }
    }
}

/// `Retry-After` header value: decimal seconds.
pub fn search_retry_after_header(secs: u64) -> String {
    secs.to_string()
}

/// Generic 429 body for search: the standard `rate_limited` error
/// envelope (`docs/api-errors.md`), rendered through the same
/// [`ApiError`] all other ingest errors use so the shape cannot drift.
/// ZERO args by construction: the vault-resolved indexer key, the query
/// and the caller identity cannot flow into it.
pub fn search_rate_limited_body() -> String {
    ApiError::rate_limited("too many requests").body_json()
}

/// Identity a search throttle decision was made for. The `Debug` impl
/// NEVER prints the IP (or any fragment of it) in cleartext — keys are
/// vault-resolved server-side and logs carry redacted material only.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SearchThrottleKey {
    ip: String,
    account: String,
}

impl SearchThrottleKey {
    pub fn new(ip: &str, account: &str) -> Self {
        Self {
            ip: ip.to_string(),
            account: account.to_string(),
        }
    }

    /// Raw IP scope for bucket keying only; never log the return value.
    pub fn ip(&self) -> &str {
        &self.ip
    }

    pub fn account(&self) -> &str {
        &self.account
    }
}

impl std::fmt::Debug for SearchThrottleKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchThrottleKey")
            .field("ip", &"[redacted]")
            .field("account", &self.account)
            .finish()
    }
}

/// Cap on tracked bucket keys per dimension in [`SearchThrottleStore`].
/// Past the cap the maps are cleared (fresh buckets): the store is an
/// in-isolate best-effort layer, so bounding memory beats perfect recall.
pub const SEARCH_THROTTLE_MAX_KEYS: usize = 4096;

/// Bucket store behind the live search route: one bucket per source IP and
/// one per account, checked through [`check_search_throttle`].
///
/// Scope, stated honestly: this lives in the Worker isolate
/// (`thread_local` in `lib.rs`), so it bounds bursts per isolate, not
/// fleet-wide. It complements — never replaces — the durable per-plan
/// metering downstream; a durable cross-isolate slot store for search is
/// follow-up work. Keys are held in memory only and never logged
/// ([`SearchThrottleKey`]'s `Debug` redacts the IP).
#[derive(Debug, Default)]
pub struct SearchThrottleStore {
    by_ip: HashMap<String, TokenBucket>,
    by_account: HashMap<String, TokenBucket>,
}

impl SearchThrottleStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check (and on allow, charge) one search attempt for `key` under
    /// `cfg`. New IPs/accounts start from a full bucket at `now_s`.
    pub fn check(
        &mut self,
        key: &SearchThrottleKey,
        cfg: &SearchRateLimitConfig,
        now_s: u64,
    ) -> SearchDecision {
        if self.by_ip.len() >= SEARCH_THROTTLE_MAX_KEYS
            || self.by_account.len() >= SEARCH_THROTTLE_MAX_KEYS
        {
            self.by_ip.clear();
            self.by_account.clear();
        }
        let fresh = || TokenBucket::per_window(cfg.max_attempts, cfg.window_secs, now_s);
        // Peek + decide on copies, then write back: a denial must not
        // mutate either stored bucket (`take` refills `last_tick_s`, which
        // is harmless, but write-back keeps the store exactly the buckets
        // the decision was made against).
        let mut ip_bucket = self.by_ip.get(key.ip()).copied().unwrap_or_else(fresh);
        let mut account_bucket = self
            .by_account
            .get(key.account())
            .copied()
            .unwrap_or_else(fresh);
        let decision = check_search_throttle(&mut ip_bucket, &mut account_bucket, now_s);
        if decision == SearchDecision::Allow {
            self.by_ip.insert(key.ip().to_string(), ip_bucket);
            self.by_account
                .insert(key.account().to_string(), account_bucket);
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_tiers_allow_bigger_bursts() {
        // Each tier's per-minute quota is spendable as one burst, then cut
        // off: exercises the bucket through its public API, not the consts.
        for tier in [PLAN_BASE, PLAN_5X, PLAN_20X] {
            let mut b = TokenBucket::per_window(tier.searches_per_min, 60, 0);
            for _ in 0..tier.searches_per_min {
                assert!(b.take(0, 1.0));
            }
            assert!(!b.take(0, 1.0));
        }
        // Tiers stay ordered base < 5x < 20x (collected at runtime).
        let searches: Vec<u32> = [PLAN_BASE, PLAN_5X, PLAN_20X]
            .iter()
            .map(|t| t.searches_per_min)
            .collect();
        assert!(searches.windows(2).all(|w| w[0] < w[1]));
        let jobs: Vec<u32> = [PLAN_BASE, PLAN_5X, PLAN_20X]
            .iter()
            .map(|t| t.max_concurrent_jobs)
            .collect();
        assert!(jobs.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn bucket_exhausts_then_refills() {
        let mut b = TokenBucket::per_window(10, 60, 1_000);
        for _ in 0..10 {
            assert!(b.take(1_000, 1.0));
        }
        assert!(!b.take(1_000, 1.0));
        assert!(b.take(1_060, 1.0));
    }

    #[test]
    fn throttle_store_gates_per_ip_and_per_account_independently() {
        let cfg = SearchRateLimitConfig {
            max_attempts: 2,
            window_secs: 60,
        };
        let mut store = SearchThrottleStore::new();
        let a = SearchThrottleKey::new("203.0.113.9", "user-a");
        assert_eq!(store.check(&a, &cfg, 1_000), SearchDecision::Allow);
        assert_eq!(store.check(&a, &cfg, 1_000), SearchDecision::Allow);
        assert!(matches!(
            store.check(&a, &cfg, 1_000),
            SearchDecision::Deny { .. }
        ));
        // Same IP, fresh account: the IP bucket is spent.
        let b = SearchThrottleKey::new("203.0.113.9", "user-b");
        assert!(matches!(
            store.check(&b, &cfg, 1_000),
            SearchDecision::Deny { .. }
        ));
        // Fresh IP + fresh account passes; the spent key recovers a token
        // once the window refills (2 per 60s -> +1 token by t+30).
        let c = SearchThrottleKey::new("203.0.113.10", "user-c");
        assert_eq!(store.check(&c, &cfg, 1_000), SearchDecision::Allow);
        assert_eq!(store.check(&a, &cfg, 1_030), SearchDecision::Allow);
    }

    #[test]
    fn backwards_clock_never_grants_extra() {
        let mut b = TokenBucket::per_window(1, 60, 1_000);
        assert!(b.take(1_000, 1.0));
        assert!(!b.take(1_000, 1.0));
        assert!(!b.take(900, 1.0));
        assert!(b.take(1_060, 1.0));
    }
}
