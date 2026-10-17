//! Rate limits and abuse guards for the auth + vault surfaces: pure token
//! bucket math, the per-IP AND per-account dual gate, config-from-vars with
//! fail-safe defaults, and the generic 429 rendering primitives. Roadmap R6.
//!
//! Pure Rust, native + wasm32, NO Worker runtime imports — same constraint
//! as `auth`/`vault`/`session`. Time is passed in as `now_s` (unix seconds)
//! so the math is unit-testable without a clock.
//!
//! Documented defaults (provisional launch tunables; override via [vars],
//! never hardcoded at the call site):
//! | surface    | max | window | vars                                                      |
//! |------------|-----|--------|-----------------------------------------------------------|
//! | auth       | 10  | 60s    | AUTH_RATE_LIMIT_MAX / AUTH_RATE_LIMIT_WINDOW_SECS         |
//! | vault test | 5   | 60s    | VAULT_TEST_RATE_LIMIT_MAX / VAULT_TEST_RATE_LIMIT_WINDOW_SECS |
//!
//! `VAULT_TEST_*` are the `vault::TEST_CONNECTION_*` consts under their
//! rate-limit names (one source, no drift). [`RateLimitConfig::from_vars`]
//! falls back to these defaults on missing, unparseable, or zero values:
//! invalid config keeps limiting on (never unlimited, never deny-all).
//!
//! Live enforcement: the Worker entry overlays [vars] through
//! [`RateLimitConfig::from_vars`] into `handlers::Config`, and the
//! handler layer's durable slot budgets (`Store::reserve_rate_limit` /
//! `Store::reserve_rate_limit_pair`) remain the cross-isolate
//! authority: the auth config bounds auth-start bursts per IP
//! alongside the pre-existing hourly budgets, and the vault config
//! drives the test-connection dual (per-IP + per-account) claim. The
//! durable store caps any single budget at 100 slots, so a configured
//! max above 100 fails closed there. This module supplies the shared
//! config, bucket math and 429/`Retry-After` shape.
//!
//! No credential or IP in cleartext: [`rate_limited_body`] takes ZERO args
//! (per-request data cannot leak into it) and [`ThrottleKey`]'s `Debug`
//! redacts the IP, mirroring the vault redaction rule.

use std::collections::HashMap;

/// Auth surface (login) burst: attempts per [`AUTH_WINDOW_SECS`].
pub const AUTH_MAX_ATTEMPTS: u32 = 10;
/// Auth surface window, seconds.
pub const AUTH_WINDOW_SECS: u64 = 60;
/// Vault test-connection burst: the single-sourced `vault` budget.
pub const VAULT_TEST_MAX_ATTEMPTS: u32 = crate::vault::TEST_CONNECTION_MAX_ATTEMPTS;
/// Vault test-connection window, seconds (single-sourced with `vault`).
pub const VAULT_TEST_WINDOW_SECS: u64 = crate::vault::TEST_CONNECTION_WINDOW_SECS;

/// HTTP status for a throttled response.
pub const RATE_LIMIT_STATUS: u16 = 429;
/// `Retry-After` header name (lowercase, as rendered).
pub const RETRY_AFTER_HEADER: &str = "retry-after";

/// Token bucket with explicit time. Starts full; refills linearly at
/// `refill_per_sec` up to `capacity`; a backwards clock never refills.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last_tick_s: u64,
}

impl TokenBucket {
    /// `capacity`: burst size; `refill_per_sec`: sustained rate. Negative
    /// (or NaN) inputs clamp to 0. The bucket starts full.
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

    /// Take `n` tokens if available; a failed take consumes nothing.
    pub fn take(&mut self, now_s: u64, n: f64) -> bool {
        if !n.is_finite() || n < 0.0 {
            return false;
        }
        let available = self.remaining(now_s);
        if available >= n {
            self.tokens = available - n;
            if now_s > self.last_tick_s {
                self.last_tick_s = now_s;
            }
            true
        } else {
            false
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

/// Outcome of a dual-gate check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleDecision {
    Allow,
    Deny { retry_after_secs: u64 },
}

/// Dual gate: deny when EITHER the per-IP or the per-account bucket is
/// empty. An allowed attempt consumes one token from BOTH buckets; a denied
/// attempt consumes NOTHING from either (the check peeks via the pure
/// [`TokenBucket::remaining`] view first, so no take is half-applied). The
/// denial retry-after is the max of both buckets' waits.
pub fn check_dual_throttle(
    ip: &mut TokenBucket,
    account: &mut TokenBucket,
    now_s: u64,
) -> ThrottleDecision {
    if ip.remaining(now_s) >= 1.0 && account.remaining(now_s) >= 1.0 {
        // Both peeks passed at the same `now_s`, so both takes succeed.
        let _ = ip.take(now_s, 1.0);
        let _ = account.take(now_s, 1.0);
        ThrottleDecision::Allow
    } else {
        ThrottleDecision::Deny {
            retry_after_secs: ip
                .retry_after_secs(now_s)
                .max(account.retry_after_secs(now_s)),
        }
    }
}

/// Per-surface rate-limit configuration. `Default` is the documented
/// table above; [`RateLimitConfig::from_vars`] overlays [vars] overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitConfig {
    pub auth_max_attempts: u32,
    pub auth_window_secs: u64,
    pub vault_test_max_attempts: u32,
    pub vault_test_window_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            auth_max_attempts: AUTH_MAX_ATTEMPTS,
            auth_window_secs: AUTH_WINDOW_SECS,
            vault_test_max_attempts: VAULT_TEST_MAX_ATTEMPTS,
            vault_test_window_secs: VAULT_TEST_WINDOW_SECS,
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

impl RateLimitConfig {
    /// Build from worker [vars]. Missing, unparseable, or zero values fall
    /// back per-field to the documented defaults: invalid config keeps
    /// limiting on (never unlimited, never deny-all).
    pub fn from_vars(vars: &HashMap<String, String>) -> Self {
        let defaults = Self::default();
        Self {
            auth_max_attempts: parse_nonzero(
                vars,
                "AUTH_RATE_LIMIT_MAX",
                defaults.auth_max_attempts,
            ),
            auth_window_secs: parse_nonzero(
                vars,
                "AUTH_RATE_LIMIT_WINDOW_SECS",
                defaults.auth_window_secs,
            ),
            vault_test_max_attempts: parse_nonzero(
                vars,
                "VAULT_TEST_RATE_LIMIT_MAX",
                defaults.vault_test_max_attempts,
            ),
            vault_test_window_secs: parse_nonzero(
                vars,
                "VAULT_TEST_RATE_LIMIT_WINDOW_SECS",
                defaults.vault_test_window_secs,
            ),
        }
    }
}

/// `Retry-After` header value: decimal seconds.
pub fn retry_after_header_value(secs: u64) -> String {
    secs.to_string()
}

/// Generic 429 body. ZERO args by construction: no per-request data (IP,
/// account, credential) can flow into it.
pub fn rate_limited_body() -> String {
    r#"{"error":"too many requests"}"#.to_string()
}

/// Identity a throttle decision was made for. The `Debug` impl NEVER prints
/// the IP (or any fragment of it) in cleartext — logs may carry this key
/// for correlation without becoming a source-IP record.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ThrottleKey {
    ip: String,
    account: String,
}

impl ThrottleKey {
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

impl std::fmt::Debug for ThrottleKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThrottleKey")
            .field("ip", &"[redacted]")
            .field("account", &self.account)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_test_defaults_are_the_vault_consts() {
        // One budget, two names: the rate-limit config must never drift
        // from the documenting `vault` module (they once disagreed 10 vs 5
        // with both suites green — see handlers.rs).
        assert_eq!(
            VAULT_TEST_MAX_ATTEMPTS,
            crate::vault::TEST_CONNECTION_MAX_ATTEMPTS
        );
        assert_eq!(
            VAULT_TEST_WINDOW_SECS,
            crate::vault::TEST_CONNECTION_WINDOW_SECS
        );
    }

    #[test]
    fn negative_bucket_inputs_clamp_to_zero() {
        let mut b = TokenBucket::new(-4.0, -1.0, 100);
        assert_eq!(b.remaining(100), 0.0);
        assert!(!b.take(100, 1.0));
        assert_eq!(b.retry_after_secs(100), u64::MAX);
    }

    #[test]
    fn fractional_and_invalid_takes_are_safe() {
        let mut b = TokenBucket::per_window(4, 8, 100);
        assert!(!b.take(100, f64::NAN));
        assert!(!b.take(100, -1.0));
        assert_eq!(b.remaining(100), 4.0);
        assert!(b.take(100, 0.5));
        assert_eq!(b.remaining(100), 3.5);
    }
}
