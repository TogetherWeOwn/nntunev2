//! Per-plan cap enforcement: pure decision plus the 402/429 refusal wire.
//!
//! Pure Rust, native + wasm32, NO Worker runtime imports — same constraint
//! as `auth` / `vault` / `session` / `rate_limit`. Time is passed in by the
//! caller only as usage counters; this module never reads a clock, a DB, or
//! the environment, so the caps matrix is unit-testable without staging.
//!
//! Caps mirror ADR 0004 (Base / 5x / 20x) and the frontend fallback
//! (`apps/web/src/lib/billing/plans.ts`): prices and quotas are plan data,
//! and the literals below change only together with both of those sources.
//! Enforcement reads plan metadata the meter harness stages (the caller
//! supplies the resolved [`PlanId`] and [`UsageSnapshot`]); DB wiring
//! (subscriptions + usage_ledger reads via Hyperdrive) lands in a later
//! slice and calls these same pure checks.
//!
//! Decision rule (fail-closed, `>=` denies: at-cap is over-cap):
//! - monthly stream seconds, daily ingest jobs, library tracks → 402
//!   `plan_limit_exceeded` (terminal: only an upgrade or the period reset
//!   changes the answer, `retryable: false`);
//! - concurrent streams → 429 `rate_limited` (retryable: release or wait,
//!   `retryable: true` with `Retry-After`).
//!
//! On the combined stream path the monthly 402 takes precedence over the
//! concurrent 429: a terminal exhaustion must never read as retryable.
//!
//! Refusal wire (pinned by `docs/api-errors.md`, asserted end to end by
//! `assertPlanLimitRefusal` in `e2e/helpers/contract.ts`):
//! - status 402 or 429 with the status-consistent code and retryable flag;
//! - headers naming the limit (`x-plan-limit`) and the reset/retry
//!   (`x-plan-reset`; 429 additionally carries `retry-after`);
//! - body `{code, message, retryable, upgrade_url: "/pricing"}` — names
//!   only, no account, credential, or provider material, so the over-limit
//!   path can never leak PII and never needs a 500.
//!
//! Unknown plan ids resolve to the smallest caps (`free`): an unrecognised
//! tier limits on, never unlimited.

/// Value for the upgrade CTA pointer: the client's pricing route.
pub const UPGRADE_URL: &str = "/pricing";
/// Body key carrying the upgrade pointer (see the limit-refusal wire in
/// `docs/api-errors.md`).
pub const UPGRADE_KEY: &str = "upgrade_url";
/// Header naming the exhausted limit (preferred name per the wire).
pub const LIMIT_HEADER: &str = "x-plan-limit";
/// Header naming the reset / retry hint (preferred name per the wire).
pub const RESET_HEADER: &str = "x-plan-reset";
/// `Retry-After` header name (lowercase, as rendered; required on 429).
pub const RETRY_AFTER_HEADER: &str = "retry-after";

/// Retry hint for the concurrent-stream 429: a released lease frees a slot
/// promptly, so the client retries soon. Static: the check takes no clock.
pub const CONCURRENT_RETRY_AFTER_SECS: u64 = 30;

/// 402 envelope message (matches `docs/api-errors.md`).
pub const PLAN_LIMIT_MESSAGE: &str = "plan limit reached";
/// 429 envelope message (matches `docs/api-errors.md`).
pub const RATE_LIMIT_MESSAGE: &str = "too many requests";

/// Plan tiers in ADR 0004 order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlanId {
    Free,
    Plus,
    Pro5x,
    Pro20x,
}

impl PlanId {
    /// All tiers, cheapest first.
    pub fn all() -> [PlanId; 4] {
        [PlanId::Free, PlanId::Plus, PlanId::Pro5x, PlanId::Pro20x]
    }

    /// Canonical id string (matches `plans.id` and the frontend `PLANS`).
    pub fn as_str(self) -> &'static str {
        match self {
            PlanId::Free => "free",
            PlanId::Plus => "plus",
            PlanId::Pro5x => "pro-5x",
            PlanId::Pro20x => "pro-20x",
        }
    }

    /// Parse a canonical id. Unknown ids return `None` — the caller fails
    /// closed to [`PlanId::Free`] via [`resolve_or_free`], never to a guess.
    pub fn parse_id(id: &str) -> Option<PlanId> {
        match id {
            "free" => Some(PlanId::Free),
            "plus" => Some(PlanId::Plus),
            "pro-5x" => Some(PlanId::Pro5x),
            "pro-20x" => Some(PlanId::Pro20x),
            _ => None,
        }
    }

    /// Caps for this tier.
    pub fn caps(self) -> PlanCaps {
        match self {
            PlanId::Free => PlanCaps {
                stream_seconds_month: 10 * 3600,
                max_concurrent_streams: 1,
                max_library_tracks: 1_000,
                ingest_jobs_day: 5,
            },
            PlanId::Plus => PlanCaps {
                stream_seconds_month: 100 * 3600,
                max_concurrent_streams: 2,
                max_library_tracks: 25_000,
                ingest_jobs_day: 25,
            },
            PlanId::Pro5x => PlanCaps {
                stream_seconds_month: 500 * 3600,
                max_concurrent_streams: 4,
                max_library_tracks: 100_000,
                ingest_jobs_day: 100,
            },
            PlanId::Pro20x => PlanCaps {
                stream_seconds_month: 2000 * 3600,
                max_concurrent_streams: 8,
                max_library_tracks: 500_000,
                ingest_jobs_day: 400,
            },
        }
    }
}

/// Resolve a plan id string, failing closed to the smallest caps on unknown
/// input. Never invents a paid tier for an unknown state.
pub fn resolve_or_free(id: &str) -> PlanId {
    PlanId::parse_id(id).unwrap_or(PlanId::Free)
}

/// Per-plan caps (see ADR 0004; stream cap stored here in seconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanCaps {
    pub stream_seconds_month: u64,
    pub max_concurrent_streams: u32,
    pub max_library_tracks: u64,
    pub ingest_jobs_day: u32,
}

/// Usage counters for one account (same four dimensions as the frontend
/// `UsageSnapshot`; the caller stages these from the meter harness today
/// and from usage_ledger reads once DB wiring lands).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageSnapshot {
    pub stream_seconds_month: u64,
    pub concurrent_now: u32,
    pub library_tracks: u64,
    pub ingest_today: u32,
}

/// Which cap refused the attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitDimension {
    StreamMonth,
    IngestDay,
    Library,
    Concurrent,
}

impl LimitDimension {
    /// Stable dimension name carried in the `x-plan-limit` header value.
    pub fn name(self) -> &'static str {
        match self {
            LimitDimension::StreamMonth => "stream_seconds_month",
            LimitDimension::IngestDay => "ingest_today",
            LimitDimension::Library => "library_tracks",
            LimitDimension::Concurrent => "concurrent_streams",
        }
    }

    /// Static reset hint carried in the `x-plan-reset` header value.
    /// Period-shaped (never account data): the client renders the upgrade
    /// CTA from this plus `upgrade_url`, never a timestamp it must parse.
    pub fn reset_hint(self) -> &'static str {
        match self {
            LimitDimension::StreamMonth => "next-calendar-month",
            LimitDimension::IngestDay => "next-utc-day",
            LimitDimension::Library => "on-upgrade-or-remove",
            LimitDimension::Concurrent => "on-release",
        }
    }

    fn cap_value(self, caps: &PlanCaps) -> u64 {
        match self {
            LimitDimension::StreamMonth => caps.stream_seconds_month,
            LimitDimension::IngestDay => u64::from(caps.ingest_jobs_day),
            LimitDimension::Library => caps.max_library_tracks,
            LimitDimension::Concurrent => u64::from(caps.max_concurrent_streams),
        }
    }
}

/// A refused attempt: status, code, and the limit facts the client needs to
/// render the upgrade path instead of a dead-end error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Denial {
    pub dimension: LimitDimension,
    pub status: u16,
    pub code: &'static str,
    pub message: &'static str,
    pub retryable: bool,
    pub limit_value: u64,
    pub reset_hint: &'static str,
    /// Present only on 429 (the wire requires `retry-after` there).
    pub retry_after_secs: Option<u64>,
}

impl Denial {
    fn plan_limited(dimension: LimitDimension, limit_value: u64, reset_hint: &'static str) -> Self {
        Self {
            dimension,
            status: 402,
            code: "plan_limit_exceeded",
            message: PLAN_LIMIT_MESSAGE,
            retryable: false,
            limit_value,
            reset_hint,
            retry_after_secs: None,
        }
    }

    fn rate_limited(
        dimension: LimitDimension,
        limit_value: u64,
        reset_hint: &'static str,
        retry_after_secs: u64,
    ) -> Self {
        Self {
            dimension,
            status: 429,
            code: "rate_limited",
            message: RATE_LIMIT_MESSAGE,
            retryable: true,
            limit_value,
            reset_hint,
            retry_after_secs: Some(retry_after_secs),
        }
    }

    /// Serialized refusal body. Infallible: static envelope plus the static
    /// upgrade pointer — no per-request data (ids, emails, sessions) can
    /// flow into it, so the over-limit path is names-only by construction.
    pub fn body_json(&self) -> String {
        format!(
            "{{\"code\":\"{}\",\"message\":\"{}\",\"retryable\":{},\"{}\":\"{}\"}}",
            self.code,
            self.message,
            if self.retryable { "true" } else { "false" },
            UPGRADE_KEY,
            UPGRADE_URL,
        )
    }

    /// Refusal headers: the limit fact, the reset hint, and `retry-after`
    /// on 429. Values carry the dimension name, the numeric cap, and the
    /// static reset hint — never account data.
    pub fn headers(&self) -> Vec<(String, String)> {
        let mut out = vec![
            (
                LIMIT_HEADER.to_string(),
                format!("{}={}", self.dimension.name(), self.limit_value),
            ),
            (RESET_HEADER.to_string(), self.reset_hint.to_string()),
        ];
        if let Some(secs) = self.retry_after_secs {
            out.push((RETRY_AFTER_HEADER.to_string(), secs.to_string()));
        }
        out
    }
}

/// Allow or refuse one guarded attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(Denial),
}

impl Decision {
    pub fn is_allow(self) -> bool {
        matches!(self, Decision::Allow)
    }

    pub fn denial(self) -> Option<Denial> {
        match self {
            Decision::Allow => None,
            Decision::Deny(d) => Some(d),
        }
    }
}

fn deny_plan(dimension: LimitDimension, caps: &PlanCaps) -> Decision {
    Decision::Deny(Denial::plan_limited(
        dimension,
        dimension.cap_value(caps),
        dimension.reset_hint(),
    ))
}

/// Ingest-write guard: the NZB intake path calls this before accepting a
/// job. At-cap denies (`>=`): the day's last allowed job already ran.
pub fn check_ingest_write(plan: PlanId, ingest_today: u32) -> Decision {
    let caps = plan.caps();
    if ingest_today >= caps.ingest_jobs_day {
        deny_plan(LimitDimension::IngestDay, &caps)
    } else {
        Decision::Allow
    }
}

/// Library guard: new additions/writes call this before inserting `adding`
/// tracks. At-cap denies (`>=`); so does any batch whose post-insert count
/// would exceed the cap. The sum saturates, so a huge count can only deny,
/// never wrap into an allow.
pub fn check_library(plan: PlanId, library_tracks: u64, adding: u64) -> Decision {
    let caps = plan.caps();
    if library_tracks >= caps.max_library_tracks
        || library_tracks.saturating_add(adding) > caps.max_library_tracks
    {
        deny_plan(LimitDimension::Library, &caps)
    } else {
        Decision::Allow
    }
}

/// Monthly stream guard: the stream path calls this before serving new
/// playback. At-cap denies: the month's allowance is spent.
pub fn check_stream_month(plan: PlanId, stream_seconds_month: u64) -> Decision {
    let caps = plan.caps();
    if stream_seconds_month >= caps.stream_seconds_month {
        deny_plan(LimitDimension::StreamMonth, &caps)
    } else {
        Decision::Allow
    }
}

/// Concurrency guard: the per-user pool calls this at lease acquisition.
/// At-cap denies with 429: a release (or wait) frees the slot.
pub fn check_concurrent(plan: PlanId, concurrent_now: u32) -> Decision {
    let caps = plan.caps();
    if concurrent_now >= caps.max_concurrent_streams {
        Decision::Deny(Denial::rate_limited(
            LimitDimension::Concurrent,
            LimitDimension::Concurrent.cap_value(&caps),
            LimitDimension::Concurrent.reset_hint(),
            CONCURRENT_RETRY_AFTER_SECS,
        ))
    } else {
        Decision::Allow
    }
}

/// Stream-path guard: monthly exhaustion (402, terminal) takes precedence
/// over concurrency (429, retryable), so a spent month never reads as
/// "try again soon".
pub fn check_stream(plan: PlanId, stream_seconds_month: u64, concurrent_now: u32) -> Decision {
    match check_stream_month(plan, stream_seconds_month) {
        Decision::Deny(denial) => Decision::Deny(denial),
        Decision::Allow => check_concurrent(plan, concurrent_now),
    }
}

/// Combined guard across one usage snapshot plus the pending library
/// addition for this attempt. Terminal 402s win over the retryable 429 in
/// this order: stream month, ingest day, library, then concurrency. The
/// first denial names its own dimension. Snapshot-only callers pass
/// `library_adding: 0`.
pub fn evaluate(plan: PlanId, usage: &UsageSnapshot, library_adding: u64) -> Decision {
    let month = check_stream_month(plan, usage.stream_seconds_month);
    if !month.is_allow() {
        return month;
    }
    let ingest = check_ingest_write(plan, usage.ingest_today);
    if !ingest.is_allow() {
        return ingest;
    }
    let library = check_library(plan, usage.library_tracks, library_adding);
    if !library.is_allow() {
        return library;
    }
    check_concurrent(plan, usage.concurrent_now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn usage_zero() -> UsageSnapshot {
        UsageSnapshot {
            stream_seconds_month: 0,
            concurrent_now: 0,
            library_tracks: 0,
            ingest_today: 0,
        }
    }

    #[test]
    fn caps_match_adr_0004_and_frontend_plans() {
        // ADR 0004 matrix (hours) × 3600, plus the exact per-tier counts the
        // frontend PLANS table pins. Any tier/price change updates all three
        // together — a drift here fails instead of silently re-capping.
        let cases = [
            (PlanId::Free, "free", 10 * 3600, 1, 1_000, 5),
            (PlanId::Plus, "plus", 100 * 3600, 2, 25_000, 25),
            (PlanId::Pro5x, "pro-5x", 500 * 3600, 4, 100_000, 100),
            (PlanId::Pro20x, "pro-20x", 2000 * 3600, 8, 500_000, 400),
        ];
        assert_eq!(PlanId::all().len(), 4);
        for (plan, id, stream_s, concurrent, library, ingest) in cases {
            assert_eq!(plan.as_str(), id);
            assert_eq!(PlanId::parse_id(id), Some(plan));
            let caps = plan.caps();
            assert_eq!(caps.stream_seconds_month, stream_s, "{id} stream");
            assert_eq!(caps.max_concurrent_streams, concurrent, "{id} concurrent");
            assert_eq!(caps.max_library_tracks, library, "{id} library");
            assert_eq!(caps.ingest_jobs_day, ingest, "{id} ingest");
        }
    }

    #[test]
    fn unknown_plan_ids_fail_closed_to_free() {
        for unknown in ["", "FREE", "pro", "pro-20", "enterprise", "free "] {
            assert_eq!(PlanId::parse_id(unknown), None, "{unknown:?}");
            assert_eq!(resolve_or_free(unknown), PlanId::Free, "{unknown:?}");
        }
        // The fallback really is the smallest caps on every dimension.
        let free = PlanId::Free.caps();
        for plan in PlanId::all() {
            let caps = plan.caps();
            assert!(caps.stream_seconds_month >= free.stream_seconds_month);
            assert!(caps.max_concurrent_streams >= free.max_concurrent_streams);
            assert!(caps.max_library_tracks >= free.max_library_tracks);
            assert!(caps.ingest_jobs_day >= free.ingest_jobs_day);
        }
    }

    #[test]
    fn caps_matrix_under_at_over_per_dimension() {
        // Under allows; at-cap and over-cap deny. Covers every tier × every
        // guarded dimension: the matrix the acceptance criterion requires.
        for plan in PlanId::all() {
            let caps = plan.caps();
            // Ingest day (402).
            assert!(check_ingest_write(plan, 0).is_allow());
            if caps.ingest_jobs_day > 1 {
                assert!(check_ingest_write(plan, caps.ingest_jobs_day - 1).is_allow());
            }
            assert!(!check_ingest_write(plan, caps.ingest_jobs_day).is_allow());
            assert!(!check_ingest_write(plan, caps.ingest_jobs_day.saturating_add(1)).is_allow());
            assert!(!check_ingest_write(plan, u32::MAX).is_allow());
            // Library (402): current-count checks pass `adding: 0`.
            assert!(check_library(plan, 0, 0).is_allow());
            assert!(check_library(plan, caps.max_library_tracks.saturating_sub(1), 0).is_allow());
            assert!(!check_library(plan, caps.max_library_tracks, 0).is_allow());
            assert!(!check_library(plan, caps.max_library_tracks.saturating_add(1), 0).is_allow());
            assert!(!check_library(plan, u64::MAX, 0).is_allow());
            assert!(!check_library(plan, u64::MAX, u64::MAX).is_allow());
            // Stream month (402).
            assert!(check_stream_month(plan, 0).is_allow());
            assert!(
                check_stream_month(plan, caps.stream_seconds_month.saturating_sub(1)).is_allow()
            );
            assert!(!check_stream_month(plan, caps.stream_seconds_month).is_allow());
            assert!(
                !check_stream_month(plan, caps.stream_seconds_month.saturating_add(1)).is_allow()
            );
            assert!(!check_stream_month(plan, u64::MAX).is_allow());
            // Concurrent (429).
            assert!(check_concurrent(plan, 0).is_allow());
            if caps.max_concurrent_streams > 1 {
                assert!(check_concurrent(plan, caps.max_concurrent_streams - 1).is_allow());
            }
            assert!(!check_concurrent(plan, caps.max_concurrent_streams).is_allow());
            assert!(
                !check_concurrent(plan, caps.max_concurrent_streams.saturating_add(1)).is_allow()
            );
            assert!(!check_concurrent(plan, u32::MAX).is_allow());
        }
    }

    #[test]
    fn library_batch_addition_denies_cap_crossing() {
        // Batch writes state how many tracks they add: a batch that fits
        // allows, a batch that crosses the cap denies even though the
        // current count alone is under it, and the sum saturates instead
        // of wrapping on hostile counts. Landing exactly on the cap
        // allows (at-cap only denies the *next* addition).
        for plan in PlanId::all() {
            let caps = plan.caps();
            let below = caps.max_library_tracks.saturating_sub(10);
            assert!(check_library(plan, below, 0).is_allow());
            assert!(check_library(plan, below, 9).is_allow());
            assert!(check_library(plan, below, 10).is_allow());
            assert!(!check_library(plan, below, 11).is_allow());
            assert!(!check_library(plan, 0, u64::MAX).is_allow());
        }
        // The review's example: 999 existing Free tracks plus a 100-track
        // write must deny (cap 1,000), not pass on the current count.
        assert!(!check_library(PlanId::Free, 999, 100).is_allow());
        let denial = check_library(PlanId::Free, 999, 100)
            .denial()
            .expect("cap-crossing batch must deny");
        assert_eq!((denial.status, denial.code), (402, "plan_limit_exceeded"));
        assert_eq!(denial.dimension, LimitDimension::Library);
    }

    #[test]
    fn stream_path_monthly_402_beats_concurrent_429() {
        let caps = PlanId::Free.caps();
        // Both exhausted: the terminal monthly denial wins.
        let both = check_stream(
            plan_of(),
            caps.stream_seconds_month,
            caps.max_concurrent_streams,
        );
        let denial = both.denial().expect("both over must deny");
        assert_eq!((denial.status, denial.code), (402, "plan_limit_exceeded"));
        assert_eq!(denial.dimension, LimitDimension::StreamMonth);
        // Only concurrent exhausted: the retryable denial.
        let conc = check_stream(plan_of(), 0, caps.max_concurrent_streams);
        let denial = conc.denial().expect("concurrent over must deny");
        assert_eq!((denial.status, denial.code), (429, "rate_limited"));
        // Neither exhausted: allow.
        assert!(check_stream(plan_of(), 0, 0).is_allow());
    }

    fn plan_of() -> PlanId {
        PlanId::Free
    }

    #[test]
    fn combined_evaluate_names_first_terminal_denial() {
        let caps = PlanId::Plus.caps();
        // All clear allows.
        assert!(evaluate(PlanId::Plus, &usage_zero(), 0).is_allow());
        // Each dimension alone denies with its own status/dimension.
        let month = UsageSnapshot {
            stream_seconds_month: caps.stream_seconds_month,
            ..usage_zero()
        };
        assert_eq!(
            evaluate(PlanId::Plus, &month, 0)
                .denial()
                .map(|d| (d.status, d.dimension)),
            Some((402, LimitDimension::StreamMonth))
        );
        let ingest = UsageSnapshot {
            ingest_today: caps.ingest_jobs_day,
            ..usage_zero()
        };
        assert_eq!(
            evaluate(PlanId::Plus, &ingest, 0)
                .denial()
                .map(|d| (d.status, d.dimension)),
            Some((402, LimitDimension::IngestDay))
        );
        let library = UsageSnapshot {
            library_tracks: caps.max_library_tracks,
            ..usage_zero()
        };
        assert_eq!(
            evaluate(PlanId::Plus, &library, 0)
                .denial()
                .map(|d| (d.status, d.dimension)),
            Some((402, LimitDimension::Library))
        );
        // A batch that crosses the library cap denies even though the
        // snapshot count alone is under it; zero-addition below cap allows.
        let batch = UsageSnapshot {
            library_tracks: caps.max_library_tracks.saturating_sub(10),
            ..usage_zero()
        };
        assert!(evaluate(PlanId::Plus, &batch, 0).is_allow());
        assert_eq!(
            evaluate(PlanId::Plus, &batch, 11)
                .denial()
                .map(|d| (d.status, d.dimension)),
            Some((402, LimitDimension::Library))
        );
        let conc = UsageSnapshot {
            concurrent_now: caps.max_concurrent_streams,
            ..usage_zero()
        };
        assert_eq!(
            evaluate(PlanId::Plus, &conc, 0)
                .denial()
                .map(|d| (d.status, d.dimension)),
            Some((429, LimitDimension::Concurrent))
        );
        // Month beats concurrent when both are over.
        let both = UsageSnapshot {
            stream_seconds_month: caps.stream_seconds_month,
            concurrent_now: caps.max_concurrent_streams,
            ..usage_zero()
        };
        assert_eq!(
            evaluate(PlanId::Plus, &both, 0).denial().map(|d| d.status),
            Some(402)
        );
    }

    #[test]
    fn denial_status_code_retryable_match_the_wire() {
        // 402 is plan_limit_exceeded/never-retryable; 429 is
        // rate_limited/always-retryable (docs/api-errors.md). The registry
        // lookup agrees, so the decision can never emit a 500 or a
        // status/code mismatch on the over-limit path.
        for plan in PlanId::all() {
            let caps = plan.caps();
            let d402 = check_ingest_write(plan, caps.ingest_jobs_day)
                .denial()
                .unwrap();
            assert_eq!(
                (d402.status, d402.code, d402.retryable),
                (402, "plan_limit_exceeded", false)
            );
            assert_eq!(crate::api_error::spec(d402.code).status, 402);
            assert!(!crate::api_error::spec(d402.code).retryable);
            let d429 = check_concurrent(plan, caps.max_concurrent_streams)
                .denial()
                .unwrap();
            assert_eq!(
                (d429.status, d429.code, d429.retryable),
                (429, "rate_limited", true)
            );
            assert_eq!(crate::api_error::spec(d429.code).status, 429);
            assert!(crate::api_error::spec(d429.code).retryable);
        }
    }

    #[test]
    fn no_500_on_the_over_limit_path() {
        // Every denial across the full matrix is 402 or 429 — never a 5xx.
        for plan in PlanId::all() {
            let caps = plan.caps();
            let denials = [
                check_ingest_write(plan, caps.ingest_jobs_day),
                check_ingest_write(plan, u32::MAX),
                check_library(plan, caps.max_library_tracks, 0),
                check_library(plan, u64::MAX, u64::MAX),
                check_stream_month(plan, caps.stream_seconds_month),
                check_stream_month(plan, u64::MAX),
                check_concurrent(plan, caps.max_concurrent_streams),
                check_concurrent(plan, u32::MAX),
            ];
            for decision in denials {
                let d = decision.denial().expect("over-cap must deny");
                assert!(
                    d.status == 402 || d.status == 429,
                    "over-limit status must be 402/429, got {}",
                    d.status
                );
                assert!(d.status < 500, "over-limit path must never 500");
            }
        }
    }

    #[test]
    fn refusal_headers_name_limit_and_reset_with_retry_after_on_429() {
        // Mirrors assertPlanLimitHeaders (e2e/helpers/contract.ts): a header
        // naming the limit, a header naming the reset/retry, both non-empty;
        // 429 additionally requires a non-empty retry-after.
        for plan in PlanId::all() {
            let caps = plan.caps();
            let d402 = check_ingest_write(plan, caps.ingest_jobs_day)
                .denial()
                .unwrap();
            assert_plan_headers(&d402, 402);
            let d429 = check_concurrent(plan, caps.max_concurrent_streams)
                .denial()
                .unwrap();
            assert_plan_headers(&d429, 429);
        }
    }

    fn assert_plan_headers(d: &Denial, status: u16) {
        let headers = d.headers();
        let names: Vec<String> = headers.iter().map(|(k, _)| k.to_lowercase()).collect();
        let limit = names.iter().find(|n| n.contains("limit"));
        let reset = names
            .iter()
            .find(|n| n.contains("reset") || n.contains("retry-after"));
        assert!(limit.is_some(), "missing limit header, got {names:?}");
        assert!(reset.is_some(), "missing reset/retry header, got {names:?}");
        for (k, v) in &headers {
            assert!(!v.trim().is_empty(), "header {k} must carry a value");
        }
        if status == 429 {
            let retry = names.iter().find(|n| *n == "retry-after");
            assert!(retry.is_some(), "429 requires retry-after, got {names:?}");
            assert_eq!(d.retry_after_secs, Some(CONCURRENT_RETRY_AFTER_SECS));
        } else {
            assert_eq!(d.retry_after_secs, None);
        }
        // Preferred wire names.
        assert!(names.iter().any(|n| n == LIMIT_HEADER));
        assert!(names.iter().any(|n| n == RESET_HEADER));
    }

    #[test]
    fn refusal_body_is_envelope_plus_pricing_cta() {
        // Mirrors extractLimitCode + extractUpgradeCta + the retryable-flag
        // check in assertPlanLimitRefusal: status-consistent code, present
        // boolean status-consistent retryable, and an upgrade pointer at
        // /pricing.
        let d402 = check_ingest_write(PlanId::Free, 5).denial().unwrap();
        let v402: serde_json::Value = serde_json::from_str(&d402.body_json()).unwrap();
        assert_eq!(v402["code"], "plan_limit_exceeded");
        assert_eq!(v402["message"], PLAN_LIMIT_MESSAGE);
        assert_eq!(v402["retryable"], false);
        assert_eq!(v402[UPGRADE_KEY], UPGRADE_URL);
        let d429 = check_concurrent(PlanId::Free, 1).denial().unwrap();
        let v429: serde_json::Value = serde_json::from_str(&d429.body_json()).unwrap();
        assert_eq!(v429["code"], "rate_limited");
        assert_eq!(v429["message"], RATE_LIMIT_MESSAGE);
        assert_eq!(v429["retryable"], true);
        assert_eq!(v429[UPGRADE_KEY], UPGRADE_URL);
        // Exactly the documented keys: no room for a leaked field.
        for v in [&v402, &v429] {
            let keys: HashSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
            assert_eq!(
                keys,
                HashSet::from(["code", "message", "retryable", UPGRADE_KEY])
            );
        }
    }

    #[test]
    fn refusal_bodies_are_names_only_no_pii() {
        // Mirrors secretLeak (e2e/helpers/contract.ts): no credential-shaped
        // key anywhere in the body, and no secret value in the raw text.
        // Bodies are static by construction (Denial::body_json takes no
        // per-request input), so this pins that invariant across both
        // refusal shapes.
        let secrets = [
            ("account email", "capped+001@example.test"),
            ("account session", "sess_test_abc123"),
        ];
        for plan in PlanId::all() {
            let caps = plan.caps();
            let bodies = [
                check_ingest_write(plan, caps.ingest_jobs_day)
                    .denial()
                    .unwrap()
                    .body_json(),
                check_concurrent(plan, caps.max_concurrent_streams)
                    .denial()
                    .unwrap()
                    .body_json(),
            ];
            for body in bodies {
                for (_, value) in &secrets {
                    assert!(
                        !body.contains(value),
                        "refusal body leaks a secret value: {body}"
                    );
                }
                let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_no_secret_keys(&parsed);
            }
        }
    }

    fn assert_no_secret_keys(value: &serde_json::Value) {
        // Same credential shape as the e2e guard: password/passwd/secret,
        // api/private keys, session, token, credential, passkey.
        const NEEDLES: [&str; 8] = [
            "password",
            "passwd",
            "secret",
            "key",
            "session",
            "token",
            "credential",
            "passkey",
        ];
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    let lower = k.to_lowercase();
                    assert!(
                        !NEEDLES.iter().any(|n| lower.contains(n)),
                        "credential-shaped key in refusal body: {k}"
                    );
                    assert_no_secret_keys(v);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    assert_no_secret_keys(item);
                }
            }
            _ => {}
        }
    }
}
