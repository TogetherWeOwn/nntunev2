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
//! are not entitlement) plus the trial/payment gate in
//! `docs/adr/0004-plan-matrix.md` and the lifecycle in
//! `docs/adr/0005-billing.md`. The plans seed rows in the migration are the
//! ADR 0004 decision table verbatim, pinned by
//! `seed_rows_match_adr_0004` via `include_str!` — the same doc-parity
//! technique as `plan_limits::caps_match_adr_0004_decision_table` — so
//! editing the ADR without reseeding (or vice versa) fails the suite.
//!
//! Resolution contract (`resolve_effective_plan`):
//! - `trialing` with `now < trial_end` → free caps, entitled. `active` on
//!   free is never entitled, and an expired trial is a hard paywall: a late
//!   webhook cannot extend free access.
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
//! The Worker binds both SELECTs below (the live-slot subscription row plus
//! the billing-account row) and feeds them to the pure predicate. The
//! account row carries trial history for the creation path's trial-eligibility
//! check (ADR 0005: only NULL history is trial-eligible); entitlement itself
//! derives from the subscription row only.

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
    pub trial_end_unix: Option<i64>,
    pub paid_through_at_unix: Option<i64>,
    pub grace_until_unix: Option<i64>,
}

/// The billing-account row the Worker selects alongside the subscription.
/// Trial history for the creation path (ADR 0005): only NULL history here
/// AND in the tombstone index is trial-eligible. Not an entitlement input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillingAccountView<'a> {
    pub user_id: &'a str,
    pub stripe_customer_id: Option<&'a str>,
    pub trial_started_at_unix: Option<i64>,
}

/// Trial-history probe for the creation path (ADR 0005 step 1): `true` once
/// the write-once `trial_started_at` is set — that user (and that email
/// history) is never trial-eligible again, with or without a card.
pub fn trial_already_started(account: Option<BillingAccountView<'_>>) -> bool {
    matches!(account, Some(a) if a.trial_started_at_unix.is_some())
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
        "trialing" => match sub.trial_end_unix {
            // Live trial: free caps only. The stored plan id is ignored on
            // purpose — a `trialing` row never confers paid caps even if it
            // names one.
            Some(end) if now_unix < end => EffectivePlan {
                plan: PlanId::Free,
                entitled: true,
            },
            // Expired trial (or a trial row with no end): hard paywall.
            _ => closed(),
        },
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

// --- Statements: parameterized reads ---------------------------------------
//
// The Worker binds fields positionally ($1) and decodes the BIGINT epochs
// straight into the views above; never interpolates a value into SQL.
// The subscription read takes only the live-slot row (the partial-unique
// statuses from ADR 0002/0005): terminal rows resolve to `None`, hence to
// closed. Unknown statuses are stored verbatim by design (no DB CHECK) and
// fail closed in `resolve_effective_plan`, never in SQL.

/// Parameters: user_id. The account row for trial history, if any.
pub fn billing_account_select_statement() -> &'static str {
    "SELECT user_id, stripe_customer_id, EXTRACT(EPOCH FROM trial_started_at)::BIGINT AS trial_started_at_unix FROM billing_accounts WHERE user_id = $1"
}

/// Parameters: user_id. The single live-slot subscription row, if any.
/// Timestamps arrive as Unix seconds (NULL stays NULL).
pub fn subscription_live_select_statement() -> &'static str {
    "SELECT plan_id, status, cancel_at_period_end, EXTRACT(EPOCH FROM trial_end)::BIGINT AS trial_end_unix, EXTRACT(EPOCH FROM paid_through_at)::BIGINT AS paid_through_at_unix, EXTRACT(EPOCH FROM grace_until)::BIGINT AS grace_until_unix FROM subscriptions WHERE user_id = $1 AND status IN ('incomplete', 'trialing', 'active', 'past_due', 'unpaid', 'paused')"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn statements_match_exact_text() {
        assert_eq!(
            billing_account_select_statement(),
            "SELECT user_id, stripe_customer_id, EXTRACT(EPOCH FROM trial_started_at)::BIGINT AS trial_started_at_unix FROM billing_accounts WHERE user_id = $1",
            "billing account select",
        );
        assert_eq!(
            subscription_live_select_statement(),
            "SELECT plan_id, status, cancel_at_period_end, EXTRACT(EPOCH FROM trial_end)::BIGINT AS trial_end_unix, EXTRACT(EPOCH FROM paid_through_at)::BIGINT AS paid_through_at_unix, EXTRACT(EPOCH FROM grace_until)::BIGINT AS grace_until_unix FROM subscriptions WHERE user_id = $1 AND status IN ('incomplete', 'trialing', 'active', 'past_due', 'unpaid', 'paused')",
            "live subscription select",
        );
    }

    #[test]
    fn settlement_allowance_is_three_hours() {
        assert_eq!(RENEWAL_SETTLEMENT_ALLOWANCE_SECS, 10_800);
    }

    #[test]
    fn seed_rows_match_adr_0004() {
        // The ADR 0004 decision table is the single source for the plans
        // seed: parse it and assert every tier's INSERT tuple in migration
        // 0021 carries the same values (price $ to cents, cache GiB to
        // bytes), so editing the table without reseeding — or reseeding
        // without the table — fails instead of silently repricing. Same
        // include_str! doc-parity pattern as
        // plan_limits::caps_match_adr_0004_decision_table.
        let doc = include_str!("../../../docs/adr/0004-plan-matrix.md");
        let migration = include_str!("../../../migrations/0021_billing_schema.sql");
        let mut seen = HashSet::new();
        for line in doc.lines().filter(|l| l.starts_with('|')) {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            // cells: ["", plan, price, stream hrs, concurrent, library,
            //          ingest, cache GiB, retention days, priority, ""].
            // Header/separator rows lack a `$` price.
            if cells.len() < 10 || !cells[2].starts_with('$') {
                continue;
            }
            let (id, dollars, stream_hrs, concurrent, library, ingest, gib, retention, priority) =
                match cells[1] {
                    p if p.starts_with("Free") => (
                        "free", &cells[2], &cells[3], &cells[4], &cells[5], &cells[6], &cells[7],
                        &cells[8], &cells[9],
                    ),
                    "Plus" => (
                        "plus", &cells[2], &cells[3], &cells[4], &cells[5], &cells[6], &cells[7],
                        &cells[8], &cells[9],
                    ),
                    "Pro 5x" => (
                        "pro-5x", &cells[2], &cells[3], &cells[4], &cells[5], &cells[6], &cells[7],
                        &cells[8], &cells[9],
                    ),
                    "Pro 20x" => (
                        "pro-20x", &cells[2], &cells[3], &cells[4], &cells[5], &cells[6],
                        &cells[7], &cells[8], &cells[9],
                    ),
                    _ => continue,
                };
            let cents: i64 = dollars
                .trim_start_matches('$')
                .replace(',', "")
                .parse::<i64>()
                .expect("ADR price parses")
                * 100;
            let gib: i64 = gib.replace(',', "").parse().expect("ADR cache GiB parses");
            let expected = format!(
                "('{id}', {cents}, {stream_hrs}, {concurrent}, {library}, {ingest}, '{priority}', {bytes}, {retention})",
                stream_hrs = stream_hrs.replace(',', ""),
                concurrent = concurrent.replace(',', ""),
                library = library.replace(',', ""),
                ingest = ingest.replace(',', ""),
                bytes = gib * 1_073_741_824,
            );
            assert!(
                migration.contains(&expected),
                "plans seed for {id} must carry the ADR 0004 row: {expected}"
            );
            seen.insert(id);
        }
        assert_eq!(seen.len(), 4, "ADR table must pin all four tiers");
        // Seeds stay replay-safe: the migration gate reapplies every file.
        assert!(
            migration.contains("ON CONFLICT (id) DO NOTHING"),
            "plans seed must be replay-safe"
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
        trial_end_unix: Option<i64>,
        paid_through_at_unix: Option<i64>,
        grace_until_unix: Option<i64>,
    ) -> SubscriptionView<'a> {
        SubscriptionView {
            plan_id,
            status,
            cancel_at_period_end,
            trial_end_unix,
            paid_through_at_unix,
            grace_until_unix,
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
    fn trial_window() {
        // Live trial: free caps, entitled.
        assert_eq!(
            resolve_effective_plan(
                Some(sub("free", "trialing", true, Some(NOW + DAY), None, None)),
                NOW
            ),
            EffectivePlan {
                plan: PlanId::Free,
                entitled: true,
            }
        );
        // Trial end is strict: the exact second cuts to the paywall.
        for end in [NOW, NOW - 1, NOW - 14 * DAY] {
            assert_eq!(
                resolve_effective_plan(
                    Some(sub("free", "trialing", true, Some(end), None, None)),
                    NOW
                ),
                closed(),
                "trial end {end} must be closed"
            );
        }
        // A trial row with no end never entitles (fail closed, never open).
        assert_eq!(
            resolve_effective_plan(Some(sub("free", "trialing", true, None, None, None)), NOW),
            closed()
        );
        // `trialing` never confers paid caps, even if the row names one.
        assert_eq!(
            resolve_effective_plan(
                Some(sub("plus", "trialing", true, Some(NOW + DAY), None, None)),
                NOW
            ),
            EffectivePlan {
                plan: PlanId::Free,
                entitled: true,
            }
        );
    }

    #[test]
    fn active_free_is_never_entitled() {
        // Saving a card / a late webhook flipping free to `active` grants
        // nothing (ADR 0004 trial gate).
        for (trial_end, paid) in [
            (Some(NOW + DAY), None),
            (None, Some(NOW + DAY)),
            (None, None),
        ] {
            assert_eq!(
                resolve_effective_plan(
                    Some(sub("free", "active", false, trial_end, paid, None)),
                    NOW
                ),
                closed(),
                "active free must never entitle"
            );
        }
    }

    #[test]
    fn active_paid_coverage_and_settlement() {
        let live = sub("plus", "active", false, None, Some(NOW + DAY), None);
        assert_eq!(
            resolve_effective_plan(Some(live), NOW),
            EffectivePlan {
                plan: PlanId::Plus,
                entitled: true,
            }
        );
        // Inside the 3h allowance past a confirmed end: still the paid plan.
        let settling = sub("pro-5x", "active", false, None, Some(NOW - 7_200), None);
        assert_eq!(
            resolve_effective_plan(Some(settling), NOW),
            EffectivePlan {
                plan: PlanId::Pro5x,
                entitled: true,
            }
        );
        // Exactly at the bound cuts (strict `<`, never sliding).
        let at_bound = sub(
            "pro-5x",
            "active",
            false,
            None,
            Some(NOW - RENEWAL_SETTLEMENT_ALLOWANCE_SECS),
            None,
        );
        assert_eq!(resolve_effective_plan(Some(at_bound), NOW), closed());
        // Scheduled cancellation gets no allowance: paid end passed is done.
        let scheduled = sub("plus", "active", true, None, Some(NOW - 1), None);
        assert_eq!(resolve_effective_plan(Some(scheduled), NOW), closed());
        // An `active` row with no confirmed coverage grants nothing (the
        // allowance requires a non-NULL previously paid end).
        let unpaid = sub("plus", "active", false, None, None, None);
        assert_eq!(resolve_effective_plan(Some(unpaid), NOW), closed());
        // Lapsed well past the allowance is closed.
        let lapsed = sub("plus", "active", false, None, Some(NOW - 30 * DAY), None);
        assert_eq!(resolve_effective_plan(Some(lapsed), NOW), closed());
    }

    #[test]
    fn past_due_holds_paid_then_renewal_grace_only() {
        // Still inside confirmed coverage (failed mid-period upgrade keeps
        // the paid tier, never grants the unpaid higher one).
        let covered = sub("plus", "past_due", false, None, Some(NOW + DAY), None);
        assert_eq!(
            resolve_effective_plan(Some(covered), NOW),
            EffectivePlan {
                plan: PlanId::Plus,
                entitled: true,
            }
        );
        // Past coverage but inside a renewal grace: the last paid plan.
        let grace = sub(
            "pro-20x",
            "past_due",
            false,
            None,
            Some(NOW - DAY),
            Some(NOW + 6 * DAY),
        );
        assert_eq!(
            resolve_effective_plan(Some(grace), NOW),
            EffectivePlan {
                plan: PlanId::Pro20x,
                entitled: true,
            }
        );
        // Grace end is strict; past it is closed.
        let grace_over = sub(
            "pro-20x",
            "past_due",
            false,
            None,
            Some(NOW - 8 * DAY),
            Some(NOW),
        );
        assert_eq!(resolve_effective_plan(Some(grace_over), NOW), closed());
        // First-payment failure (no paid end, even with a grace value
        // present): no grace without a previous paid period.
        let first_failure = sub("plus", "past_due", false, None, None, Some(NOW + 6 * DAY));
        assert_eq!(resolve_effective_plan(Some(first_failure), NOW), closed());
        // Coverage lapsed and no grace stored: closed.
        let no_grace = sub("plus", "past_due", false, None, Some(NOW - DAY), None);
        assert_eq!(resolve_effective_plan(Some(no_grace), NOW), closed());
        // `past_due` on free confers nothing.
        let free_due = sub("free", "past_due", false, None, Some(NOW + DAY), None);
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
                        None,
                        Some(NOW + DAY),
                        Some(NOW + 6 * DAY)
                    )),
                    NOW
                ),
                closed(),
                "status {status:?} must be closed even with live coverage"
            );
        }
    }

    #[test]
    fn unknown_plan_ids_fail_closed() {
        // An unrecognised tier limits on, never unlimited: free caps, and no
        // entitlement is invented for it — even with live paid coverage.
        for unknown in ["", "FREE", "pro", "enterprise", "free "] {
            assert_eq!(
                resolve_effective_plan(
                    Some(sub(unknown, "active", false, None, Some(NOW + DAY), None)),
                    NOW
                ),
                closed(),
                "plan {unknown:?} must fail closed"
            );
        }
    }

    #[test]
    fn trial_history_probe() {
        assert!(!trial_already_started(None));
        assert!(!trial_already_started(Some(BillingAccountView {
            user_id: "u",
            stripe_customer_id: None,
            trial_started_at_unix: None,
        })));
        assert!(trial_already_started(Some(BillingAccountView {
            user_id: "u",
            stripe_customer_id: Some("cus_123"),
            trial_started_at_unix: Some(NOW - 30 * DAY),
        })));
    }
}
