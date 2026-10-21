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
//! | surface        | max | window | vars                                                      |
//! |----------------|-----|--------|-----------------------------------------------------------|
//! | auth           | 10  | 60s    | AUTH_RATE_LIMIT_MAX / AUTH_RATE_LIMIT_WINDOW_SECS         |
//! | vault test     | 5   | 60s    | VAULT_TEST_RATE_LIMIT_MAX / VAULT_TEST_RATE_LIMIT_WINDOW_SECS |
//! | vault test, IP | 25  | 60s    | VAULT_TEST_IP_RATE_LIMIT_MAX (window shared with vault test) |
//!
//! `VAULT_TEST_*` are the `vault::TEST_CONNECTION_*` consts under their
//! rate-limit names (one source, no drift). The per-IP vault budget is
//! sized above the per-account one (NAT-shared addresses serve many
//! accounts), mirroring the auth-start budgets in `handlers`.
//! [`RateLimitConfig::from_vars`] falls back to these defaults on
//! missing, unparseable, or zero values, and clamps any max to
//! [`MAX_BUDGET_SLOTS`]: invalid config keeps limiting on (never
//! unlimited, never deny-all).
//!
//! Live enforcement: the Worker entry overlays [vars] through
//! [`RateLimitConfig::from_vars`] into `handlers::Config`, and the
//! handler layer's durable slot budgets (`Store::reserve_rate_limit` /
//! `Store::reserve_rate_limit_pair`) are the cross-isolate authority:
//! the auth config bounds auth-start bursts per IP alongside the
//! pre-existing hourly budgets, and the vault config drives the
//! test-connection dual (per-IP + per-account) claim. This module
//! supplies the shared config and the 429/`Retry-After` shape.
//!
//! Reference model, not live behavior: [`TokenBucket`],
//! [`check_dual_throttle`], [`ThrottleKey`] and [`rate_limited_body`]
//! are the pure model the acceptance tests pin the budget semantics
//! against; no production path calls them. Live denials render in
//! `handlers` (register/start gets a real 429; vault test-connection
//! deliberately collapses to `200 {"ok":false}`).
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
/// Per-IP vault test-connection burst: 5x the per-account cap, so one
/// account's allowed probes cannot consume the whole budget of a
/// NAT-shared address (same sizing rule as the auth-start IP budget).
pub const VAULT_TEST_IP_MAX_ATTEMPTS: u32 = VAULT_TEST_MAX_ATTEMPTS * 5;

/// Largest budget the durable slot store will scan/claim. Configured
/// maxima clamp to this at parse time ([`RateLimitConfig::from_vars`])
/// so an over-large operator value can never turn into a deny-all at
/// the store's admissibility gate.
pub const MAX_BUDGET_SLOTS: u32 = 100;

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
    /// Per-IP vault test-connection cap (own var, larger default; the
    /// window stays `vault_test_window_secs`).
    pub vault_test_ip_max_attempts: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            auth_max_attempts: AUTH_MAX_ATTEMPTS,
            auth_window_secs: AUTH_WINDOW_SECS,
            vault_test_max_attempts: VAULT_TEST_MAX_ATTEMPTS,
            vault_test_window_secs: VAULT_TEST_WINDOW_SECS,
            vault_test_ip_max_attempts: VAULT_TEST_IP_MAX_ATTEMPTS,
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

/// Parse a `*_MAX` var: nonzero values apply but clamp to
/// [`MAX_BUDGET_SLOTS`], the largest budget the durable slot store
/// admits — an over-large configured max must relax to the cap, never
/// silently deny every request at the store gate.
fn parse_capped_max(vars: &HashMap<String, String>, key: &str, fallback: u32) -> u32 {
    parse_nonzero::<u32>(vars, key, fallback).min(MAX_BUDGET_SLOTS)
}

impl RateLimitConfig {
    /// Build from worker [vars]. Missing, unparseable, or zero values fall
    /// back per-field to the documented defaults, and maxima clamp to
    /// [`MAX_BUDGET_SLOTS`]: invalid config keeps limiting on (never
    /// unlimited, never deny-all).
    pub fn from_vars(vars: &HashMap<String, String>) -> Self {
        let defaults = Self::default();
        let vault_test_max_attempts = parse_capped_max(
            vars,
            "VAULT_TEST_RATE_LIMIT_MAX",
            defaults.vault_test_max_attempts,
        );
        Self {
            auth_max_attempts: parse_capped_max(
                vars,
                "AUTH_RATE_LIMIT_MAX",
                defaults.auth_max_attempts,
            ),
            auth_window_secs: parse_nonzero(
                vars,
                "AUTH_RATE_LIMIT_WINDOW_SECS",
                defaults.auth_window_secs,
            ),
            vault_test_max_attempts,
            vault_test_window_secs: parse_nonzero(
                vars,
                "VAULT_TEST_RATE_LIMIT_WINDOW_SECS",
                defaults.vault_test_window_secs,
            ),
            // Unset IP cap scales with the CONFIGURED account cap
            // (5x, clamped), not the compiled one: raising only
            // VAULT_TEST_RATE_LIMIT_MAX must not strand the IP budget
            // at 25 below the account budget it is meant to exceed.
            vault_test_ip_max_attempts: parse_capped_max(
                vars,
                "VAULT_TEST_IP_RATE_LIMIT_MAX",
                vault_test_max_attempts.saturating_mul(5),
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

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn configured_max_above_store_cap_clamps_instead_of_denying_all() {
        // The durable slot store rejects budgets over MAX_BUDGET_SLOTS,
        // so an unclamped 200 here would make every auth start and
        // vault test a silent deny. from_vars must clamp to the cap.
        for raw in ["101", "200", &u32::MAX.to_string()] {
            let cfg = RateLimitConfig::from_vars(&vars(&[
                ("AUTH_RATE_LIMIT_MAX", raw),
                ("VAULT_TEST_RATE_LIMIT_MAX", raw),
                ("VAULT_TEST_IP_RATE_LIMIT_MAX", raw),
            ]));
            assert_eq!(cfg.auth_max_attempts, MAX_BUDGET_SLOTS, "{raw}");
            assert_eq!(cfg.vault_test_max_attempts, MAX_BUDGET_SLOTS, "{raw}");
            assert_eq!(cfg.vault_test_ip_max_attempts, MAX_BUDGET_SLOTS, "{raw}");
        }
        // At-or-under values pass through untouched.
        let cfg = RateLimitConfig::from_vars(&vars(&[("AUTH_RATE_LIMIT_MAX", "100")]));
        assert_eq!(cfg.auth_max_attempts, 100);
    }

    #[test]
    fn vault_ip_cap_defaults_above_account_cap_and_has_own_var() {
        let defaults = RateLimitConfig::default();
        assert_eq!(
            defaults.vault_test_ip_max_attempts,
            VAULT_TEST_IP_MAX_ATTEMPTS
        );
        assert!(
            defaults.vault_test_ip_max_attempts > defaults.vault_test_max_attempts,
            "NAT-shared IP budget must exceed the per-account budget"
        );
        let cfg = RateLimitConfig::from_vars(&vars(&[("VAULT_TEST_IP_RATE_LIMIT_MAX", "7")]));
        assert_eq!(cfg.vault_test_ip_max_attempts, 7);
        assert_eq!(cfg.vault_test_max_attempts, VAULT_TEST_MAX_ATTEMPTS);
        // Unset IP cap follows the configured account cap at 5x
        // (clamped to the store cap): raising only the account var
        // must not leave the IP budget below it.
        let cfg = RateLimitConfig::from_vars(&vars(&[("VAULT_TEST_RATE_LIMIT_MAX", "10")]));
        assert_eq!(cfg.vault_test_max_attempts, 10);
        assert_eq!(cfg.vault_test_ip_max_attempts, 50);
        let cfg = RateLimitConfig::from_vars(&vars(&[("VAULT_TEST_RATE_LIMIT_MAX", "30")]));
        assert_eq!(cfg.vault_test_ip_max_attempts, MAX_BUDGET_SLOTS);
        // An explicit IP var still wins over the scaled fallback.
        let cfg = RateLimitConfig::from_vars(&vars(&[
            ("VAULT_TEST_RATE_LIMIT_MAX", "10"),
            ("VAULT_TEST_IP_RATE_LIMIT_MAX", "12"),
        ]));
        assert_eq!(cfg.vault_test_ip_max_attempts, 12);
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

    #[test]
    fn constructor_table_pins_capacity_and_refill_floors() {
        // Pin the `TokenBucket::new` floors (lines 87-89).
        // Each row: (capacity, refill) -> expected initial availability.
        // Removing any `.max(0.0)` must turn a row red.
        for (capacity, refill, want_remaining) in [
            (-5.0, 1.0, 0.0),
            (-0.5, 10.0, 0.0),
            (0.0, 1.0, 0.0),
            (0.0, 0.0, 0.0),
            (5.0, -2.0, 5.0),
            (5.0, 0.0, 5.0),
            (f64::NAN, 1.0, 0.0),
            (5.0, f64::NAN, 5.0),
            (f64::NAN, f64::NAN, 0.0),
        ] {
            let b = TokenBucket::new(capacity, refill, 100);
            assert_eq!(
                b.remaining(100),
                want_remaining,
                "capacity={capacity} refill={refill}"
            );
        }
        // Negative capacity starts empty and stays empty: nothing to take,
        // and time cannot refill above the zero cap.
        let mut b = TokenBucket::new(-4.0, 2.0, 100);
        assert!(!b.take(100, 1.0));
        assert_eq!(b.remaining(1_000), 0.0);
        // Negative/NaN refill clamps to zero: a full bucket drains, then
        // never refills no matter how much time passes.
        for refill in [-3.0, f64::NAN] {
            let mut b = TokenBucket::new(2.0, refill, 100);
            assert!(b.take(100, 2.0), "refill={refill}");
            assert_eq!(b.remaining(100), 0.0, "refill={refill}");
            assert_eq!(b.remaining(100 + 3_600), 0.0, "refill={refill}");
            assert_eq!(b.retry_after_secs(100 + 3_600), u64::MAX, "refill={refill}");
        }
    }

    #[test]
    fn retry_after_secs_ceils_subsecond_waits_to_one() {
        // Pin the line-146 `.max(1.0)` ceiling — a fractional wait must
        // report exactly 1, never 0. Without the ceiling these rows return 0.
        for (capacity, refill, take) in [
            (1.0, 10.0, 1.0),
            (1.0, 100.0, 1.0),
            (2.0, 4.0, 1.9),
            (10.0, 1.0, 9.5),
        ] {
            let mut b = TokenBucket::new(capacity, refill, 100);
            assert!(b.take(100, take), "cap={capacity} refill={refill}");
            assert!(
                b.remaining(100) < 1.0,
                "cap={capacity} refill={refill}: test setup must leave <1 token"
            );
            assert_eq!(
                b.retry_after_secs(100),
                1,
                "cap={capacity} refill={refill} take={take}"
            );
        }
        // Near-full bucket: a sub-nanosecond wait must still report 1,
        // never 0. Without `.max(1.0)`, `(wait - 1e-9).ceil()` is `-0.0`
        // here, which casts to `0`.
        let mut b = TokenBucket::new(1.0, 1.0, 100);
        assert!(b.take(100, 1e-10));
        assert!(b.remaining(100) < 1.0);
        assert_eq!(b.retry_after_secs(100), 1);
        // Exact-integer waits stay exact: the 1e-9 epsilon must not push an
        // on-the-second wait up by one.
        let mut b = TokenBucket::new(2.0, 1.0, 100);
        assert!(b.take(100, 2.0));
        assert_eq!(b.retry_after_secs(100), 1);
        let mut b = TokenBucket::new(3.0, 0.5, 100);
        assert!(b.take(100, 3.0));
        assert_eq!(b.retry_after_secs(100), 2);
    }

    #[test]
    fn zero_capacity_bucket_admits_nothing_without_panicking() {
        // Zero-capacity buckets must deny every positive take (no panic,
        // no refill above the zero cap) regardless of rate.
        for refill in [0.0, 1.0, 10.0] {
            let mut b = TokenBucket::new(0.0, refill, 100);
            assert_eq!(b.remaining(100), 0.0, "refill={refill}");
            assert_eq!(b.remaining(100 + 3_600), 0.0, "refill={refill}");
            assert!(!b.take(100, 1.0), "refill={refill}");
            assert!(!b.take(100, 0.5), "refill={refill}");
            assert_eq!(b.remaining(100), 0.0, "refill={refill}");
            let _ = b.retry_after_secs(100);
            let _ = b.retry_after_secs(100 + 3_600);
        }
        // `per_window(0, _)` is the same zero bucket through the window ctor.
        let mut b = TokenBucket::per_window(0, 60, 100);
        assert_eq!(b.remaining(100), 0.0);
        assert!(!b.take(100, 1.0));
        // Retry shape: with refill there is always at least a 1s wait;
        // without refill the bucket can never refill.
        assert_eq!(TokenBucket::new(0.0, 1.0, 100).retry_after_secs(100), 1);
        assert_eq!(
            TokenBucket::new(0.0, 0.0, 100).retry_after_secs(100),
            u64::MAX
        );
    }
}
