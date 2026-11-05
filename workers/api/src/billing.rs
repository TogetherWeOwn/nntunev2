//! Billing schema read path: effective-plan resolution without Stripe calls.
//!
//! Pure Rust, native + wasm32, NO Worker runtime imports — same constraint
//! as `auth` / `vault` / `session` / `rate_limit` / `plan_limits` / `ledger`.
//! Time arrives only as `now_unix` (Unix seconds, UTC); this module never
//! reads a clock, a DB, or the environment, so the entitlement matrix is
//! unit-testable without staging. Nothing here calls Stripe: the tables
//! (`migrations/0021_billing_schema.sql`) hold last-confirmed provider state
//! that the later-gated creation/webhook slices reconcile; this slice only
//! reads it.
//!
//! ADR sources: rule 4 of `docs/adr/0002-data-model.md` (subscription slots
//! are not entitlement) plus the payment gate in
//! `docs/adr/0004-plan-matrix.md` and the lifecycle in
//! `docs/adr/0005-billing.md`. There is no trial: a Stripe-reported
//! `trialing` status is unknown here and fails closed (ADR 0005 revision
//! of 2026-11-03). The plans seed rows in the reseed migration are the
//! ADR 0004 decision table verbatim, pinned by
//! `seed_rows_match_adr_0004` via `include_str!` — the same doc-parity
//! technique as `plan_limits::caps_match_adr_0004_decision_table` — so
//! editing the ADR without reseeding (or vice versa) fails the suite.
//!
//! Resolution contract (`resolve_effective_plan`):
//! - `active` on a paid plan → that plan while `paid_through_at` covers now,
//!   or for [`RENEWAL_SETTLEMENT_ALLOWANCE_SECS`] past a non-NULL paid end
//!   when no scheduled cancellation awaits (Stripe's normal draft/charge
//!   window plus webhook delay — not evidence of payment, never advanced,
//!   never sliding on retry). Access cuts exactly at the bound.
//! - `past_due` on a paid plan → that plan while `paid_through_at` covers
//!   now (a failed mid-period upgrade keeps the still-paid tier), or while a
//!   stored `grace_until` covers now when a previous paid end exists
//!   (renewal-only seven-day grace; first-payment failures carry no paid end
//!   and get no grace).
//! - Anything else (no row, terminal or non-entitling status, unknown status,
//!   unknown plan id) → free caps, not entitled. Unknown plan ids fail closed
//!   through [`crate::plan_limits::resolve_or_free`]; unknown statuses are
//!   stored verbatim (no DB CHECK) and fail closed here.
//!
//! The Worker binds the SELECTs below (the live-slot subscription row, the
//! any-row-ever probe, the plans table, and the billing-account row) and
//! feeds them to the pure predicates. Entitlement derives from the
//! subscription row only; the account row carries the Stripe customer
//! binding for the creation path, and the plans/ever reads serve the
//! `GET /v1/plans` + `GET /v1/me/subscription` display DTOs.

use crate::plan_limits::{resolve_or_free, PlanId};

/// Fixed renewal-settlement allowance past the last confirmed paid end
/// (ADR 0002 rule 4): 3 hours covering Stripe's normal draft/charge window
/// and webhook delay. Requires a non-NULL previously paid end, excludes
/// scheduled cancellation, and never slides on retry.
pub const RENEWAL_SETTLEMENT_ALLOWANCE_SECS: i64 = 3 * 3_600;

/// Effective plan for one user: which caps apply, and whether the account
/// currently holds any service entitlement at all. Unentitled readers get
/// free caps (fail closed) with `entitled: false` — the call sites (wired in
/// the next slice) treat that as the paywall, never as free access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectivePlan {
    pub plan: PlanId,
    pub entitled: bool,
}

/// The live-slot subscription row the Worker selects: at most one per user
/// (the `uq_subscriptions_live_slot` partial unique). Timestamps are Unix
/// seconds (the statements project `EXTRACT(EPOCH ...)`), `None` where the
/// DB column is NULL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionView<'a> {
    pub plan_id: &'a str,
    pub status: &'a str,
    pub cancel_at_period_end: bool,
    pub paid_through_at_unix: Option<i64>,
    pub grace_until_unix: Option<i64>,
    /// Provider-reported current period end, for display (`GET
    /// /v1/me/subscription`). Not an entitlement input: coverage is
    /// `paid_through_at_unix`, advanced only by confirmed paid invoices.
    pub current_period_end_unix: Option<i64>,
}

/// The billing-account row the Worker selects alongside the subscription:
/// the Stripe customer binding for the creation path. Not an entitlement
/// input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillingAccountView<'a> {
    pub user_id: &'a str,
    pub stripe_customer_id: Option<&'a str>,
}

fn closed() -> EffectivePlan {
    EffectivePlan {
        plan: PlanId::Free,
        entitled: false,
    }
}

/// Resolve `(plan_id, entitled)` from the live-slot subscription row.
///
/// `None` (no live row: never subscribed, or only terminal rows) is closed.
/// Timestamp comparisons are strict `<`: at-expiry cuts access immediately,
/// even if the hourly `billing-reconcile` job has not run yet.
pub fn resolve_effective_plan(
    subscription: Option<SubscriptionView<'_>>,
    now_unix: i64,
) -> EffectivePlan {
    let Some(sub) = subscription else {
        return closed();
    };
    let plan = resolve_or_free(sub.plan_id);
    match sub.status {
        "active" => {
            // `active` on free is never entitled: saving a card or a late
            // webhook must not extend free access.
            if plan == PlanId::Free {
                return closed();
            }
            match sub.paid_through_at_unix {
                Some(paid) if now_unix < paid => EffectivePlan {
                    plan,
                    entitled: true,
                },
                // Bounded settlement past a confirmed end: no scheduled
                // cancellation, non-NULL previous coverage, exact bound.
                Some(paid)
                    if !sub.cancel_at_period_end
                        && now_unix < paid.saturating_add(RENEWAL_SETTLEMENT_ALLOWANCE_SECS) =>
                {
                    EffectivePlan {
                        plan,
                        entitled: true,
                    }
                }
                _ => closed(),
            }
        }
        "past_due" => {
            if plan == PlanId::Free {
                return closed();
            }
            // Still inside confirmed coverage (including a failed mid-period
            // upgrade): the paid tier holds.
            if matches!(sub.paid_through_at_unix, Some(paid) if now_unix < paid) {
                return EffectivePlan {
                    plan,
                    entitled: true,
                };
            }
            // Renewal-only grace: a stored deadline plus a previous paid end
            // (first-payment failures carry none and get no grace).
            match (sub.paid_through_at_unix, sub.grace_until_unix) {
                (Some(_), Some(grace)) if now_unix < grace => EffectivePlan {
                    plan,
                    entitled: true,
                },
                _ => closed(),
            }
        }
        // `incomplete`, `unpaid`, `paused` reserve the slot but confer
        // nothing; `canceled` / `incomplete_expired` release it; unknown
        // provider states fail closed pending reconciliation.
        _ => closed(),
    }
}

/// Account states for the subscription gate's display surface (spec §4):
/// browse/search/library work in every state; only playback is paywalled.
/// Derived per read — never a copied `users.plan_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountState {
    /// Invited and signed up, never had a subscription row.
    NoSubscription,
    /// Paid coverage current (including settlement/grace per ADR 0005).
    Active,
    /// A subscription row exists (live or terminal) but confers no
    /// entitlement now: canceled past `paid_through_at`, unpaid, grace
    /// exhausted, or a provider state that fails closed.
    Lapsed,
}

impl AccountState {
    /// Wire name for the `GET /v1/me/subscription` DTO.
    pub fn as_str(self) -> &'static str {
        match self {
            AccountState::NoSubscription => "no_subscription",
            AccountState::Active => "active",
            AccountState::Lapsed => "lapsed",
        }
    }
}

/// Derive the account state from the live-slot row plus whether any
/// subscription row ever existed. Entitlement is exactly
/// [`resolve_effective_plan`]; this only names the closed cases so the
/// paywall copy can distinguish never-subscribed (→ pricing) from lapsed
/// (→ billing portal).
pub fn account_state(
    subscription: Option<SubscriptionView<'_>>,
    ever_subscribed: bool,
    now_unix: i64,
) -> AccountState {
    if resolve_effective_plan(subscription, now_unix).entitled {
        return AccountState::Active;
    }
    if subscription.is_some() || ever_subscribed {
        AccountState::Lapsed
    } else {
        AccountState::NoSubscription
    }
}

// --- Statements: parameterized reads ---------------------------------------
//
// The Worker binds fields positionally ($1) and decodes the BIGINT epochs
// straight into the views above; never interpolates a value into SQL.
// The subscription read takes only the live-slot row (the partial-unique
// statuses from ADR 0002/0005): terminal rows resolve to `None`, hence to
// closed. Unknown statuses are stored verbatim by design (no DB CHECK) and
// fail closed in `resolve_effective_plan`, never in SQL.

/// Parameters: user_id. The billing-account row, if any.
pub fn billing_account_select_statement() -> &'static str {
    "SELECT user_id, stripe_customer_id FROM billing_accounts WHERE user_id = $1"
}

/// Parameters: user_id. The single live-slot subscription row, if any.
/// Timestamps arrive as Unix seconds (NULL stays NULL).
pub fn subscription_live_select_statement() -> &'static str {
    "SELECT plan_id, status, cancel_at_period_end, EXTRACT(EPOCH FROM paid_through_at)::BIGINT AS paid_through_at_unix, EXTRACT(EPOCH FROM grace_until)::BIGINT AS grace_until_unix, EXTRACT(EPOCH FROM current_period_end)::BIGINT AS current_period_end_unix FROM subscriptions WHERE user_id = $1 AND status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')"
}

/// Parameters: user_id. One row iff any subscription row ever existed for
/// the user (live or terminal): separates `lapsed` from
/// `no_subscription` when the live-slot read is empty.
pub fn subscription_any_select_statement() -> &'static str {
    "SELECT 1 AS present FROM subscriptions WHERE user_id = $1 LIMIT 1"
}

/// Every plan row, cheapest first: the `GET /v1/plans` display read. The
/// `plans` table is the one runtime source for prices/quotas (ADR 0004);
/// the handler maps rows to DTOs, marking the reserved `free` row
/// non-selectable.
pub fn plans_select_statement() -> &'static str {
    "SELECT id, price_cents, stream_hours_month, max_concurrent_streams FROM plans ORDER BY price_cents ASC, id ASC"
}

/// Parameters: user_id. Whether any subscription row — live or terminal
/// — ever reached paid coverage (a non-NULL paid end). The live-slot
/// select above cannot answer this: terminal rows are excluded from
/// it, and a canceled-after-paying account must still read `lapsed`,
/// not `never_subscribed`, at the playback gate
/// ([`crate::playback::subscription_refusal`]). An abandoned checkout
/// (`incomplete`, never paid) correctly reads false.
pub fn subscription_ever_paid_statement() -> &'static str {
    "SELECT EXISTS(SELECT 1 FROM subscriptions WHERE user_id = $1 AND paid_through_at IS NOT NULL) AS ever_paid"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn statements_match_exact_text() {
        assert_eq!(
            billing_account_select_statement(),
            "SELECT user_id, stripe_customer_id FROM billing_accounts WHERE user_id = $1",
            "billing account select",
        );
        assert_eq!(
            subscription_live_select_statement(),
            "SELECT plan_id, status, cancel_at_period_end, EXTRACT(EPOCH FROM paid_through_at)::BIGINT AS paid_through_at_unix, EXTRACT(EPOCH FROM grace_until)::BIGINT AS grace_until_unix, EXTRACT(EPOCH FROM current_period_end)::BIGINT AS current_period_end_unix FROM subscriptions WHERE user_id = $1 AND status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')",
            "live subscription select",
        );
        assert_eq!(
            subscription_any_select_statement(),
            "SELECT 1 AS present FROM subscriptions WHERE user_id = $1 LIMIT 1",
            "any-subscription select",
        );
        assert_eq!(
            plans_select_statement(),
            "SELECT id, price_cents, stream_hours_month, max_concurrent_streams FROM plans ORDER BY price_cents ASC, id ASC",
            "plans select",
        );
        assert_eq!(
            subscription_ever_paid_statement(),
            "SELECT EXISTS(SELECT 1 FROM subscriptions WHERE user_id = $1 AND paid_through_at IS NOT NULL) AS ever_paid",
            "ever-paid probe",
        );
    }

    #[test]
    fn settlement_allowance_is_three_hours() {
        assert_eq!(RENEWAL_SETTLEMENT_ALLOWANCE_SECS, 10_800);
    }

    #[test]
    fn seed_rows_match_adr_0004() {
        // The ADR 0004 decision table is the single source for the plans
        // seed: parse it and assert every row's INSERT tuple in the reseed
        // migration (0026, the effective seed — 0021's original seed is
        // superseded and never edited) carries the same values (price $
        // to cents), so editing the table without reseeding — or reseeding
        // without the table — fails instead of silently repricing. Same
        // include_str! doc-parity pattern as
        // plan_limits::caps_match_adr_0004_decision_table.
        let doc = include_str!("../../../docs/adr/0004-plan-matrix.md");
        let migration = include_str!("../../../migrations/0026_plan_reseed_no_trial.sql");
        let mut seen = HashSet::new();
        for line in doc.lines().filter(|l| l.starts_with('|')) {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            // cells: ["", plan, price, stream hrs, concurrent, ""].
            // Header/separator rows lack a `$` price.
            if cells.len() < 5 || !cells[2].starts_with('$') {
                continue;
            }
            let id = match cells[1] {
                p if p.starts_with("Free") => "free",
                "Plus" => "plus",
                "Pro" => "pro",
                "Max" => "max",
                _ => continue,
            };
            let cents: i64 = cells[2]
                .trim_start_matches('$')
                .replace(',', "")
                .parse::<i64>()
                .expect("ADR price parses")
                * 100;
            let expected = format!(
                "('{id}', {cents}, {stream_hrs}, {concurrent})",
                stream_hrs = cells[3].replace(',', ""),
                concurrent = cells[4].replace(',', ""),
            );
            assert!(
                migration.contains(&expected),
                "plans seed for {id} must carry the ADR 0004 row: {expected}"
            );
            seen.insert(id);
        }
        assert_eq!(seen.len(), 4, "ADR table must pin all four rows");
        // The reseed upserts, so replaying it is a clean no-op that also
        // re-asserts the corrected values over 0021's original seed.
        assert!(
            migration.contains("ON CONFLICT (id) DO UPDATE"),
            "plans reseed must be replay-safe"
        );
    }

    // --- Resolution matrix --------------------------------------------------
    //
    // Fixed clock: 2026-10-18T00:00:00Z. Every case names its ADR rule; the
    // boundary instants (exactly at expiry / exactly at the 3h bound) cut.

    const NOW: i64 = 1_786_032_000;
    const DAY: i64 = 86_400;

    fn sub<'a>(
        plan_id: &'a str,
        status: &'a str,
        cancel_at_period_end: bool,
        paid_through_at_unix: Option<i64>,
        grace_until_unix: Option<i64>,
    ) -> SubscriptionView<'a> {
        SubscriptionView {
            plan_id,
            status,
            cancel_at_period_end,
            paid_through_at_unix,
            grace_until_unix,
            current_period_end_unix: None,
        }
    }

    #[test]
    fn no_row_is_closed() {
        assert_eq!(
            resolve_effective_plan(None, NOW),
            EffectivePlan {
                plan: PlanId::Free,
                entitled: false,
            }
        );
    }

    #[test]
    fn trialing_is_unknown_and_closed() {
        // No flow can produce a trial (ADR 0005 revision of 2026-11-03):
        // a Stripe-reported `trialing` is an unknown status → fail closed,
        // even with live paid coverage on the row and even on a paid plan.
        for plan_id in ["free", "plus"] {
            assert_eq!(
                resolve_effective_plan(
                    Some(sub(
                        plan_id,
                        "trialing",
                        true,
                        Some(NOW + DAY),
                        Some(NOW + 6 * DAY)
                    )),
                    NOW
                ),
                closed(),
                "trialing on {plan_id} must be closed"
            );
        }
    }

    #[test]
    fn active_free_is_never_entitled() {
        // Saving a card / a late webhook flipping free to `active` grants
        // nothing (ADR 0004 payment gate): the reserved `free` row never
        // entitles, whatever the coverage fields say.
        for paid in [Some(NOW + DAY), None] {
            assert_eq!(
                resolve_effective_plan(Some(sub("free", "active", false, paid, None)), NOW),
                closed(),
                "active free must never entitle"
            );
        }
    }

    #[test]
    fn active_paid_coverage_and_settlement() {
        let live = sub("plus", "active", false, Some(NOW + DAY), None);
        assert_eq!(
            resolve_effective_plan(Some(live), NOW),
            EffectivePlan {
                plan: PlanId::Plus,
                entitled: true,
            }
        );
        // Inside the 3h allowance past a confirmed end: still the paid plan.
        let settling = sub("pro", "active", false, Some(NOW - 7_200), None);
        assert_eq!(
            resolve_effective_plan(Some(settling), NOW),
            EffectivePlan {
                plan: PlanId::Pro,
                entitled: true,
            }
        );
        // Scheduled cancellation keeps LIVE coverage: cancel_at_period_end is
        // not terminal (ADR 0005) — access runs to paid_through_at. Only the
        // settlement allowance below excludes it. This pins the live arm
        // against a `&& !cancel` regression (paying users cut at cancel time).
        let scheduled_live = sub("plus", "active", true, Some(NOW + DAY), None);
        assert_eq!(
            resolve_effective_plan(Some(scheduled_live), NOW),
            EffectivePlan {
                plan: PlanId::Plus,
                entitled: true,
            }
        );
        // Exactly at the bound cuts (strict `<`, never sliding).
        let at_bound = sub(
            "pro",
            "active",
            false,
            Some(NOW - RENEWAL_SETTLEMENT_ALLOWANCE_SECS),
            None,
        );
        assert_eq!(resolve_effective_plan(Some(at_bound), NOW), closed());
        // Scheduled cancellation gets no allowance: paid end passed is done.
        let scheduled = sub("plus", "active", true, Some(NOW - 1), None);
        assert_eq!(resolve_effective_plan(Some(scheduled), NOW), closed());
        // An `active` row with no confirmed coverage grants nothing (the
        // allowance requires a non-NULL previously paid end).
        let unpaid = sub("plus", "active", false, None, None);
        assert_eq!(resolve_effective_plan(Some(unpaid), NOW), closed());
        // Lapsed well past the allowance is closed.
        let lapsed = sub("plus", "active", false, Some(NOW - 30 * DAY), None);
        assert_eq!(resolve_effective_plan(Some(lapsed), NOW), closed());
    }

    #[test]
    fn settlement_boundary_is_exact_to_the_second() {
        // ADR 0005 renewal settlement: the paid plan is retained while
        // `now < paid_through + 3h` and denied at and after the bound.
        // One second inside still entitles.
        let inside = sub(
            "plus",
            "active",
            false,
            Some(NOW - (RENEWAL_SETTLEMENT_ALLOWANCE_SECS - 1)),
            None,
        );
        assert_eq!(
            resolve_effective_plan(Some(inside), NOW),
            EffectivePlan {
                plan: PlanId::Plus,
                entitled: true,
            }
        );
        // Exactly at `paid_through + 3h` cuts (strict `<`, never sliding).
        let at_bound = sub(
            "plus",
            "active",
            false,
            Some(NOW - RENEWAL_SETTLEMENT_ALLOWANCE_SECS),
            None,
        );
        assert_eq!(resolve_effective_plan(Some(at_bound), NOW), closed());
        // One second past the bound stays closed.
        let past_bound = sub(
            "plus",
            "active",
            false,
            Some(NOW - (RENEWAL_SETTLEMENT_ALLOWANCE_SECS + 1)),
            None,
        );
        assert_eq!(resolve_effective_plan(Some(past_bound), NOW), closed());
    }

    #[test]
    fn past_due_holds_paid_then_renewal_grace_only() {
        // Still inside confirmed coverage (failed mid-period upgrade keeps
        // the paid tier, never grants the unpaid higher one).
        let covered = sub("plus", "past_due", false, Some(NOW + DAY), None);
        assert_eq!(
            resolve_effective_plan(Some(covered), NOW),
            EffectivePlan {
                plan: PlanId::Plus,
                entitled: true,
            }
        );
        // Past coverage but inside a renewal grace: the last paid plan.
        let grace = sub(
            "max",
            "past_due",
            false,
            Some(NOW - DAY),
            Some(NOW + 6 * DAY),
        );
        assert_eq!(
            resolve_effective_plan(Some(grace), NOW),
            EffectivePlan {
                plan: PlanId::Max,
                entitled: true,
            }
        );
        // cancel_at_period_end never gates past_due: the coverage and grace
        // arms ignore it by design (ADR 0005 pins no cancel exclusion for
        // past_due) — pin both so a `&& !cancel` backport fails loudly.
        let covered_cancel = sub("plus", "past_due", true, Some(NOW + DAY), None);
        assert_eq!(
            resolve_effective_plan(Some(covered_cancel), NOW),
            EffectivePlan {
                plan: PlanId::Plus,
                entitled: true,
            }
        );
        let grace_cancel = sub(
            "max",
            "past_due",
            true,
            Some(NOW - DAY),
            Some(NOW + 6 * DAY),
        );
        assert_eq!(
            resolve_effective_plan(Some(grace_cancel), NOW),
            EffectivePlan {
                plan: PlanId::Max,
                entitled: true,
            }
        );
        // Grace end is strict; past it is closed.
        let grace_over = sub("max", "past_due", false, Some(NOW - 8 * DAY), Some(NOW));
        assert_eq!(resolve_effective_plan(Some(grace_over), NOW), closed());
        // First-payment failure (no paid end, even with a grace value
        // present): no grace without a previous paid period.
        let first_failure = sub("plus", "past_due", false, None, Some(NOW + 6 * DAY));
        assert_eq!(resolve_effective_plan(Some(first_failure), NOW), closed());
        // Coverage lapsed and no grace stored: closed.
        let no_grace = sub("plus", "past_due", false, Some(NOW - DAY), None);
        assert_eq!(resolve_effective_plan(Some(no_grace), NOW), closed());
        // `past_due` on free confers nothing.
        let free_due = sub("free", "past_due", false, Some(NOW + DAY), None);
        assert_eq!(resolve_effective_plan(Some(free_due), NOW), closed());
    }

    #[test]
    fn non_entitling_and_unknown_states_are_closed() {
        // Slot-reserving but non-entitling, terminal, and unknown provider
        // states all fail closed (unknowns stay stored verbatim for
        // reconciliation; they never entitle here).
        for status in [
            "incomplete",
            "unpaid",
            "paused",
            "canceled",
            "incomplete_expired",
            "weird_remote_status",
            "",
        ] {
            assert_eq!(
                resolve_effective_plan(
                    Some(sub(
                        "plus",
                        status,
                        false,
                        Some(NOW + DAY),
                        Some(NOW + 6 * DAY),
                    )),
                    NOW
                ),
                closed(),
                "status {status:?} must be closed even with live coverage"
            );
        }
    }

    #[test]
    fn account_state_names_the_closed_cases() {
        // Never subscribed: no live row, no row ever -> pricing surface.
        assert_eq!(
            account_state(None, false, NOW),
            AccountState::NoSubscription
        );
        assert_eq!(AccountState::NoSubscription.as_str(), "no_subscription");
        // Terminal-only history (canceled releases the live slot): lapsed,
        // so a returning subscriber is routed to the portal, never told
        // they never subscribed.
        assert_eq!(account_state(None, true, NOW), AccountState::Lapsed);
        // Entitled now -> active, whatever the history flag says.
        let live = sub("plus", "active", false, Some(NOW + DAY), None);
        assert_eq!(account_state(Some(live), false, NOW), AccountState::Active);
        assert_eq!(AccountState::Active.as_str(), "active");
        // A live row that confers nothing (unpaid, grace exhausted) is
        // lapsed even before any terminal row exists.
        let unpaid = sub("plus", "unpaid", false, Some(NOW - DAY), None);
        assert_eq!(
            account_state(Some(unpaid), false, NOW),
            AccountState::Lapsed
        );
        assert_eq!(AccountState::Lapsed.as_str(), "lapsed");
        // In-grace past_due is still entitled (ADR 0005): active, not
        // lapsed, so display never paywalls a grace-covered listener.
        let grace = sub(
            "max",
            "past_due",
            false,
            Some(NOW - DAY),
            Some(NOW + 6 * DAY),
        );
        assert_eq!(account_state(Some(grace), true, NOW), AccountState::Active);
    }

    #[test]
    fn unknown_plan_ids_fail_closed() {
        // An unrecognised tier limits on, never unlimited: free caps, and no
        // entitlement is invented for it — even with live paid coverage.
        for unknown in ["", "FREE", "pro-5x", "pro-20x", "enterprise", "free "] {
            assert_eq!(
                resolve_effective_plan(
                    Some(sub(unknown, "active", false, Some(NOW + DAY), None)),
                    NOW
                ),
                closed(),
                "plan {unknown:?} must fail closed"
            );
        }
    }
}
