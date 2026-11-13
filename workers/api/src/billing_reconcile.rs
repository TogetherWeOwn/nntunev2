//! Pure Stripe reconciliation shared by the webhook and API/inline paths.
//!
//! Pure Rust, native + wasm32, NO Worker runtime imports — same constraint
//! as `auth` / `vault` / `session` / `rate_limit` / `plan_limits` /
//! `ledger` / `billing` / `stripe`. Time arrives only as `now_unix`; this
//! module never reads a clock, a DB, the network, or the environment, so
//! every coverage, grace, and fail-closed decision is unit-testable with
//! fixtures.
//!
//! ADR sources: the lifecycle in `docs/adr/0005-billing.md` (price identity
//! through `plan_prices`, per-user single flight, trial/`pending_if_incomplete`
//! conversion, the 3-hour renewal-settlement allowance, the first-failure
//! 7-day renewal grace, verbatim status storage) and the read contract in
//! `billing.rs` (which this module's rows must keep entitled). The reseed
//! card owns `trialing` semantics on the read side; here `trialing` is an
//! unknown provider state for a Checkout-shape subscription and fails
//! closed, never advancing coverage.
//!
//! One function, two callers: the `checkout.session.completed` webhook
//! handler and the inline/API path (purchase attempts, the hourly
//! `billing-reconcile` job) both feed a retrieved remote subscription plus
//! its paid invoices through [`reconcile_subscription`] and persist the
//! returned row + history through the store. Same remote state in means
//! the same stored state out, whichever path ran first: duplicates and
//! out-of-order deliveries converge instead of regressing.
//!
//! Coverage rules (ADR 0005, Checkout shape):
//! - `paid_through_at`/`plan_id` advance ONLY from confirmed paid service
//!   invoices whose lines resolve through `plan_prices` to the subscription
//!   item's plan, bound to the known customer + subscription + item. Any
//!   other line (one-off, credit, free-price, unknown price, another plan,
//!   non-positive amount, open-ended) confers nothing and is skipped for
//!   coverage; the applied item price itself MUST resolve or the whole
//!   pass fails closed.
//! - Only `active` and `past_due` advance. `incomplete`, `unpaid` and
//!   `paused` are known non-entitling states: recorded verbatim with no
//!   advance, no alert, no purchase disable. `canceled` and
//!   `incomplete_expired` are terminal: recorded verbatim, grace cleared,
//!   slot released by the status itself. `trialing` and every other status
//!   are unknown: recorded verbatim with no advance, an alert, and new
//!   purchases disabled for the account.
//! - Test-mode only: any `livemode` remote object fails closed. Customer
//!   binding is exact once the account holds a customer (a fresh account
//!   adopts the retrieved one); the subscription item must be exactly one
//!   with a price.
//! - Monotonic coverage under the per-user lock (held by the store
//!   layer): duplicates and older invoices never regress a later confirmed
//!   plan/period; the plan only changes on newly reconciled money, or
//!   relabels to the item's plan on an equal-end snapshot carrying
//!   equally-or-more recently paid evidence. A snapshot that does not
//!   contain the invoice anchoring the stored coverage is stale: it folded
//!   an older remote state, so it retains the stored plan, status, grace,
//!   bindings and anchor instead of regressing them. (Money-end comparison
//!   alone misfires after a plan switch-back, where the earlier plan's
//!   newest invoice can end before stored coverage from the later plan
//!   while the snapshot itself is fresh.) A snapshot with no stored anchor
//!   to check against falls back to the end comparison, and a snapshot with
//!   no item-plan money at all is not stale on that basis alone — a failed
//!   renewal observes `past_due` with no new invoice, and that observation
//!   sets the grace below.
//! - Renewal grace: the first failed renewal after a paid period sets
//!   `grace_until` to that paid end plus [`RENEWAL_GRACE_SECS`]. Retries
//!   and duplicate events never extend a stored grace; successful recovery
//!   (new money reconciled) clears it, as does `active` and terminal
//!   state. A failed FIRST payment has no paid end and gets no grace.
//! - History: every confirmed paid service invoice that resolves through
//!   `plan_prices` to any plan yields exactly one [`PaidInvoiceRow`]
//!   (idempotent on the invoice id at the store layer), labeled with the
//!   latest-ending line's plan — so earlier-plan invoices row after a plan
//!   change instead of being skipped — and spanning only that plan's own
//!   lines, so a mid-cycle upgrade invoice never attributes the earlier
//!   plan's service period to the later plan. Anchored on
//!   `status_transitions.paid_at`; invoices without it are skipped: no
//!   anchor, no coverage, no history row.
//!
//! New purchases disable on fail-closed only (the stored verbatim row is
//! nonterminal and unresolvable, so the purchase path cannot proceed past
//! it) — never on a clean known state.

use crate::stripe::{Invoice, Subscription};

/// Seven-day renewal-failure grace (ADR 0005): set once, from the paid
/// period's end, on the first failed renewal after a paid period. Never
/// extended by retries or duplicates, cleared on recovery.
pub const RENEWAL_GRACE_SECS: i64 = 7 * 24 * 3_600;

/// One `plan_prices` mapping row: exact Stripe price id → plan, with the
/// row's own currency/interval checks left to the seed card. Only
/// test-mode rows (`livemode == false`) ever resolve here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceBinding {
    pub price_id: String,
    pub plan_id: String,
    pub selectable: bool,
    pub livemode: bool,
}

/// The live-slot subscription row the store selected (the ADR 0002/0005
/// nonterminal statuses), or `None` when no live row exists. Timestamps
/// are Unix seconds; `None` where the DB column is NULL. The stored anchor
/// (`paid_invoice_id`) is what the staleness and equal-end order checks
/// compare the snapshot against: without it a pass cannot tell a fresh
/// snapshot from an older one at the same coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveRow {
    pub stripe_subscription_id: String,
    pub stripe_customer_id: String,
    pub stripe_item_id: String,
    pub stripe_price_id: String,
    pub plan_id: String,
    pub status: String,
    pub paid_through_unix: Option<i64>,
    pub paid_invoice_id: Option<String>,
    pub grace_until_unix: Option<i64>,
}

/// Everything reconciliation needs from the store: the account's Stripe
/// customer (if bound yet), the live subscription row (or explicitly targeted
/// row), and the full `plan_prices` map (a handful of rows; loaded whole so unknown
/// prices fail closed against complete data, never a partial cache).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconcileInput {
    pub stripe_customer_id: Option<String>,
    pub live: Option<LiveRow>,
    pub bindings: Vec<PriceBinding>,
}

/// The remote side: a retrieved subscription plus its paid invoices with
/// FULL lines (the caller follows `has_more` until exhausted — a first
/// page is never the full invoice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSubscription {
    pub subscription: Subscription,
    pub invoices: Vec<Invoice>,
}

/// Why a pass failed closed: observed state is still recorded, but nothing
/// advances, new purchases disable, and the caller raises the alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailClosedReason {
    /// A remote object (or the event) is live-mode on a test-mode worker.
    WrongLivemode,
    /// The remote customer does not match the bound account customer.
    CustomerMismatch,
    /// The subscription carries zero or several items, or an item without
    /// a price: unresolvable, never first-picked.
    AmbiguousItems,
    /// The applied item price has no test-mode `plan_prices` mapping.
    UnknownPrice,
    /// The remote status is not a known Checkout-shape state — including
    /// `trialing`, which the reseed card owns elsewhere.
    UnknownStatus,
}

/// The subscription row to persist (verbatim remote status by design —
/// no DB CHECK — so unknown states remain recorded and fail closed at
/// read time instead of being rejected at write time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledRow {
    pub stripe_subscription_id: String,
    pub stripe_customer_id: String,
    pub stripe_item_id: String,
    pub stripe_price_id: String,
    pub plan_id: String,
    pub status: String,
    pub current_period_end_unix: Option<i64>,
    pub cancel_at_period_end: bool,
    pub paid_through_unix: Option<i64>,
    pub paid_invoice_id: Option<String>,
    pub grace_until_unix: Option<i64>,
}

/// One confirmed paid service invoice: exactly one row per invoice,
/// idempotent on `stripe_invoice_id` at the store layer. The period comes
/// from the qualifying service lines (min start, max end); the money is
/// the invoice's own `amount_paid`/`currency`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaidInvoiceRow {
    pub stripe_invoice_id: String,
    pub stripe_subscription_id: String,
    pub plan_id: String,
    pub period_start_unix: i64,
    pub period_end_unix: i64,
    pub amount_cents: i64,
    pub currency: String,
    pub paid_at_unix: i64,
}

/// The outcome of one pass: the row to persist, the history rows to
/// insert (possibly empty), and the fail-closed marker (if any). Same
/// remote state in means the same outcome out, whichever caller ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileOutcome {
    pub row: ReconciledRow,
    pub history: Vec<PaidInvoiceRow>,
    pub fail_closed: Option<FailClosedReason>,
    /// Explicit stale proof suppresses the subscription write, not history.
    pub explicit_stale: bool,
    /// The plan the pass read (the stored plan at fold time) and the
    /// anchor it read (the stored anchor at fold time): the write's
    /// equal-end adopt arm requires the stored row to still match both, so
    /// a keep that read before a relabel committed cannot reverse the newer
    /// plan/anchor when it commits second — while a relabel onto the same
    /// shared invoice (emitted anchor equals the read anchor) still
    /// persists when nothing moved the row meanwhile. Carried through the
    /// outcome because the fold runs before the per-user lock is taken;
    /// never persisted itself.
    pub read_plan: Option<String>,
    /// See `read_plan`: the stored anchor at fold time.
    pub read_anchor: Option<String>,
}

impl ReconcileOutcome {
    /// New purchases disable on fail-closed only: the stored verbatim row
    /// is nonterminal and unresolvable, so the purchase path cannot
    /// proceed past it. Clean states never disable.
    pub fn disable_new_purchases(&self) -> bool {
        self.fail_closed.is_some()
    }
}

/// Statuses that reserve the live slot but confer nothing on their own.
/// Stored verbatim with no advance, no alert, no purchase disable.
fn is_held_status(status: &str) -> bool {
    matches!(status, "incomplete" | "unpaid" | "paused")
}

/// Terminal statuses: stored verbatim outside the live slot, grace
/// cleared, coverage retained as history only. Shared with the purchase
/// path, which must confirm a held subscription terminal before a
/// replacement checkout mints its session.
pub(crate) fn is_terminal_status(status: &str) -> bool {
    matches!(status, "canceled" | "incomplete_expired")
}

/// The only statuses that can advance coverage from paid invoices.
fn is_advancing_status(status: &str) -> bool {
    matches!(status, "active" | "past_due")
}

/// Resolve an applied price through the bindings: exact price id plus
/// test-mode row. Selectability gates the purchase path, not
/// reconciliation — historical nonselectable rows still reconcile old
/// invoices and subscriptions.
fn resolve_plan(bindings: &[PriceBinding], price_id: &str) -> Option<String> {
    bindings
        .iter()
        .find(|binding| !binding.livemode && binding.price_id == price_id)
        .map(|binding| binding.plan_id.clone())
}

/// One invoice line's qualifying contribution: its resolved (non-`free`)
/// plan plus its bounded service period. Positive amount, an exact
/// test-mode mapping, and a bounded end — one-off, credit, free-price,
/// unknown-price, or open-ended lines confer nothing and never fail the
/// pass on their own. Coverage additionally requires the line's plan to
/// match the subscription item's plan (see the fold); history accepts any
/// qualifying plan, so earlier-plan invoices still row. `start` defaults
/// to `end` when Stripe omits it (a zero-length point, never a guess).
struct QualifiedLine {
    plan: String,
    start: i64,
    end: i64,
}

fn qualify_line(
    bindings: &[PriceBinding],
    line_price: Option<&str>,
    amount: i64,
    period: Option<&crate::stripe::InvoiceLinePeriod>,
) -> Option<QualifiedLine> {
    if amount <= 0 {
        return None;
    }
    let price_id = line_price?;
    let plan = resolve_plan(bindings, price_id)?;
    if plan == "free" {
        return None;
    }
    let period = period?;
    let end = period.end?;
    let start = period.start.unwrap_or(end);
    Some(QualifiedLine { plan, start, end })
}

/// Shared by the webhook and API/inline paths: fold one retrieved remote
/// subscription plus its paid invoices (full lines) against local state
/// into the row to persist, the history rows to insert, and the
/// fail-closed marker. No I/O, no clock reads beyond `now_unix`.
pub fn reconcile_subscription(
    local: &ReconcileInput,
    remote: &RemoteSubscription,
    now_unix: i64,
) -> ReconcileOutcome {
    let _ = now_unix;
    let sub = &remote.subscription;

    // Test-mode only: a live remote object on this worker fails closed
    // before any binding is trusted.
    if sub.livemode {
        return fail_closed(local, sub, None, None, FailClosedReason::WrongLivemode);
    }
    // Customer binding: exact once the account holds one; a fresh account
    // adopts the retrieved customer. An unbound remote object is not ours.
    let Some(remote_customer) = sub.customer.clone() else {
        return fail_closed(local, sub, None, None, FailClosedReason::CustomerMismatch);
    };
    if let Some(bound) = local.stripe_customer_id.as_deref() {
        if bound != remote_customer {
            return fail_closed(local, sub, None, None, FailClosedReason::CustomerMismatch);
        }
    }
    // Exactly one priced item: zero or several is unresolvable, never
    // first-picked.
    let (item_id, price_id) = match sub.single_item() {
        Some((item_id, Some(price_id))) => (item_id.to_string(), price_id.to_string()),
        _ => {
            return fail_closed(
                local,
                sub,
                Some(remote_customer),
                None,
                FailClosedReason::AmbiguousItems,
            )
        }
    };
    // The applied price must resolve through plan_prices: a missing
    // mapping disables the purchase, it never mints a guess.
    let Some(item_plan) = resolve_plan(&local.bindings, &price_id) else {
        return fail_closed(
            local,
            sub,
            Some(remote_customer),
            Some((item_id, price_id)),
            FailClosedReason::UnknownPrice,
        );
    };

    let status = sub.status.as_str();
    let previous = local.live.as_ref();
    if is_terminal_status(status) || is_held_status(status) {
        if let Some(kept) = previous.filter(|row| {
            row.stripe_subscription_id == sub.id
                && row.paid_invoice_id.as_ref().is_some_and(|anchor| {
                    !remote.invoices.iter().any(|invoice| &invoice.id == anchor)
                })
        }) {
            // The exhausted listing proves staleness even without folding
            // money. Do not re-pin an equal-end anchor: the write must still
            // recognize this evidence after another pass changes status/grace.
            return stale_snapshot(sub, remote_customer, kept, None, Vec::new());
        }
    }
    // Every non-stale branch below re-pins the stored anchor when it keeps
    // the stored coverage: the write guard reads an anchor-less equal-end
    // write against an anchored stored row as stale evidence, and only the
    // stale branch may emit that shape. A fresh observation (terminal,
    // held, fail-closed, or an equal-end keep) must stay distinguishable
    // from it, or the guard would suppress the fresh status it carries.
    if is_terminal_status(status) {
        // Terminal state releases the slot: recorded verbatim, coverage
        // retained as history only, grace cleared.
        let previous = local.live.as_ref();
        return ReconcileOutcome {
            row: ReconciledRow {
                stripe_subscription_id: sub.id.clone(),
                stripe_customer_id: remote_customer,
                stripe_item_id: item_id,
                stripe_price_id: price_id,
                plan_id: previous
                    .map(|row| row.plan_id.clone())
                    .unwrap_or("free".to_string()),
                status: status.to_string(),
                current_period_end_unix: sub.current_period_end,
                cancel_at_period_end: sub.cancel_at_period_end,
                paid_through_unix: previous.and_then(|row| row.paid_through_unix),
                paid_invoice_id: previous.and_then(|row| row.paid_invoice_id.clone()),
                grace_until_unix: None,
            },
            history: Vec::new(),
            fail_closed: None,
            explicit_stale: false,
            read_plan: previous.map(|row| row.plan_id.clone()),
            read_anchor: previous.and_then(|row| row.paid_invoice_id.clone()),
        };
    }
    if is_held_status(status) {
        // Known non-entitling: recorded verbatim, nothing advances, no
        // alert, purchases stay as they were.
        let previous = local.live.as_ref();
        return ReconcileOutcome {
            row: ReconciledRow {
                stripe_subscription_id: sub.id.clone(),
                stripe_customer_id: remote_customer,
                stripe_item_id: item_id,
                stripe_price_id: price_id,
                plan_id: previous
                    .map(|row| row.plan_id.clone())
                    .unwrap_or("free".to_string()),
                status: status.to_string(),
                current_period_end_unix: sub.current_period_end,
                cancel_at_period_end: sub.cancel_at_period_end,
                paid_through_unix: previous.and_then(|row| row.paid_through_unix),
                paid_invoice_id: previous.and_then(|row| row.paid_invoice_id.clone()),
                grace_until_unix: previous.and_then(|row| row.grace_until_unix),
            },
            history: Vec::new(),
            fail_closed: None,
            explicit_stale: false,
            read_plan: previous.map(|row| row.plan_id.clone()),
            read_anchor: previous.and_then(|row| row.paid_invoice_id.clone()),
        };
    }
    if !is_advancing_status(status) {
        // `trialing` and every other unknown state: observed verbatim, no
        // advance, alert, purchases disable.
        return fail_closed(
            local,
            sub,
            Some(remote_customer),
            Some((item_id, price_id)),
            FailClosedReason::UnknownStatus,
        );
    }

    // Advance path (`active` / `past_due`): fold every bound paid invoice.
    // A live or misbound invoice fails the pass; a merely unhelpful one
    // (wrong sub, unpaid, unanchored, no qualifying line) is skipped.
    //
    // Coverage advances ONLY from lines resolving to the subscription
    // item's plan; history rows cover every confirmed paid service invoice
    // resolving through `plan_prices` to ANY plan, so earlier-plan
    // invoices row (labeled with their own plan) after a plan change
    // instead of being skipped. The store inserts history idempotently on
    // the invoice id, so the next reconcile backfills rows that older
    // passes skipped.
    let mut history = Vec::new();
    let mut new_end: Option<i64> = None;
    let mut anchor_invoice: Option<String> = None;
    let mut anchor_paid_at: Option<i64> = None;
    for invoice in &remote.invoices {
        if invoice.livemode {
            return fail_closed(
                local,
                sub,
                Some(remote_customer),
                Some((item_id, price_id)),
                FailClosedReason::WrongLivemode,
            );
        }
        if invoice.status != "paid" {
            continue;
        }
        if invoice.customer.as_deref() != Some(remote_customer.as_str())
            || invoice.subscription.as_deref() != Some(sub.id.as_str())
        {
            continue;
        }
        let Some(paid_at) = invoice.paid_at() else {
            continue;
        };
        let Some(currency) = invoice.currency.clone() else {
            continue;
        };
        let lines = invoice
            .lines
            .as_ref()
            .map(|lines| lines.data.as_slice())
            .unwrap_or(&[]);
        let mut spans = std::collections::BTreeMap::<String, (i64, i64)>::new();
        let mut span_end: Option<i64> = None;
        let mut span_plan: Option<String> = None;
        let mut item_end: Option<i64> = None;
        for line in lines {
            let Some(qualified) = qualify_line(
                &local.bindings,
                line.price.as_deref(),
                line.amount,
                line.period.as_ref(),
            ) else {
                continue;
            };
            // Accumulate each plan independently so an intervening plan
            // cannot discard the eventual winner's earlier service days.
            // Strictly greater preserves ties: the first latest-ending
            // line still selects the plan, not the map's iteration order.
            let span = spans
                .entry(qualified.plan.clone())
                .or_insert((qualified.start, qualified.end));
            span.0 = span.0.min(qualified.start);
            span.1 = span.1.max(qualified.end);
            if span_end.is_none_or(|end| qualified.end > end) {
                span_end = Some(qualified.end);
                span_plan = Some(qualified.plan.clone());
            }
            if qualified.plan == item_plan {
                item_end = Some(item_end.map_or(qualified.end, |end: i64| end.max(qualified.end)));
            }
        }
        let Some(plan) = span_plan else {
            continue;
        };
        let (start, end) = spans[&plan];
        history.push(PaidInvoiceRow {
            stripe_invoice_id: invoice.id.clone(),
            stripe_subscription_id: sub.id.clone(),
            plan_id: plan,
            period_start_unix: start,
            period_end_unix: end,
            amount_cents: invoice.amount_paid,
            currency,
            paid_at_unix: paid_at,
        });
        if let Some(end) = item_end {
            if new_end.is_none_or(|current| end > current) {
                new_end = Some(end);
                anchor_invoice = Some(invoice.id.clone());
                anchor_paid_at = Some(paid_at);
            }
        }
    }

    let previous = local.live.as_ref();
    let previous_end = previous.and_then(|row| row.paid_through_unix);
    let stored_anchor = previous.and_then(|row| row.paid_invoice_id.clone());
    // Staleness is anchor containment, not an end comparison: a snapshot
    // that does not contain the invoice anchoring the stored coverage
    // folded an older remote state, however its item-plan money compares.
    // (Comparing item-plan money alone misfires after a plan switch-back:
    // the earlier plan's newest invoice can end before stored coverage
    // from the later plan while the snapshot itself is fresh — and that
    // fresh `past_due` must set the renewal grace below, not be
    // misclassified stale.) A stale snapshot retains the stored plan,
    // status, grace, bindings and anchor instead of regressing them; the
    // history rows above still emit (the store dedupes on the invoice id).
    // A snapshot with no stored anchor to check against (no coverage yet,
    // or an anchor-less keep) falls back to the end comparison, and a
    // snapshot with no item-plan money at all is NOT stale on that basis
    // alone: a first failed renewal observes `past_due` with no new
    // invoice, and that observation must set the grace below. Terminal and
    // held paths use the same anchor-containment check above, without a
    // money comparison or history fold. The invoice listing is followed to
    // exhaustion by the caller, so a fresh snapshot always contains the
    // stored anchor; a missing one proves the read predates that commit.
    //
    // The row carries the stale *evidence* end (`new_end`), not the
    // stored end: the write guard keys its staleness predicate off
    // `EXCLUDED.paid_through_at`, so a stale-reading pass that commits
    // second still compares strictly older and keeps the stored
    // status/grace instead of overwriting them with older verbatim
    // fields. A stale pass whose snapshot holds no item-plan money at
    // all emits no evidence end — and the guard only fires on non-NULL
    // evidence — so it falls back to the stored end: with the anchor
    // cleared that is exactly the anchor-less equal-end shape the guard
    // already reads as stale against an anchored stored row (a shape
    // only this branch emits). Coverage itself stays monotonic in the
    // write (the guard keeps the greatest confirmed end), and the
    // retained plan/anchor are kept with it there.
    let stale = match stored_anchor {
        Some(ref anchor) => !remote.invoices.iter().any(|inv| &inv.id == anchor),
        None => matches!((new_end, previous_end), (Some(end), Some(prev)) if end < prev),
    };
    if stale {
        let kept = previous.expect("stale requires stored coverage");
        return stale_snapshot(sub, remote_customer, kept, new_end, history);
    }
    // Monotonic coverage: duplicates and older invoices never regress a
    // later confirmed plan/period. New money (a strictly greater item-plan
    // end) adopts the subscription's current item plan and anchors on the
    // new invoice — a greater end self-orders, so no recency check applies.
    // An equal-end snapshot relabels to the current item plan without
    // advancing coverage — that is the defined equal-end plan-switch
    // outcome — but only when the new anchor is at least as fresh as the
    // stored one: equal coverage is not proof the snapshot is newer, and a
    // late-arriving older snapshot must not reverse a paid switch the
    // equal-end rule already committed. Freshness is the anchor invoices'
    // payment order as observed in this snapshot (the listing carries every
    // invoice, so both anchors are visible when the snapshot is fresh); a
    // snapshot that cannot prove recency keeps the stored plan and anchor
    // while status and grace still fold verbatim below. Anything strictly
    // older is unreachable here (stale returns above).
    let recovered = new_end.is_some_and(|end| previous_end.is_none_or(|prev| end > prev));
    let tied = !recovered && new_end.is_some() && new_end == previous_end;
    let stored_anchor_paid_at = stored_anchor.as_deref().and_then(|anchor| {
        remote
            .invoices
            .iter()
            .find(|inv| inv.id == anchor)
            .and_then(|inv| inv.paid_at())
    });
    let relabels = tied
        && anchor_invoice.is_some()
        && match (&stored_anchor, anchor_paid_at, stored_anchor_paid_at) {
            // No recorded anchor: accept the defined equal-end outcome.
            (None, _, _) => true,
            // Both anchors observed: relabel only onto equally-or-more
            // recently paid evidence.
            (Some(_), Some(new_paid), Some(old_paid)) => new_paid >= old_paid,
            // Stored anchor present but its payment time unobservable:
            // keep the stored plan rather than relabel on unordered
            // evidence.
            _ => false,
        };
    // A fresh pass that keeps the stored coverage re-pins the stored
    // anchor (instead of emitting none): the write guard reads an
    // anchor-less equal-end write against an anchored stored row as stale
    // evidence (only the stale branch emits that shape), so a fresh keep
    // must stay distinguishable from it — otherwise the guard would
    // suppress the fresh status and grace this pass folded. Re-pinning
    // makes a keep indistinguishable from a relabel on the anchor alone,
    // so the write's equal-end adopt additionally requires the emitted
    // anchor alone, so the write's equal-end adopt additionally requires
    // the stored row to still match what this pass read (`read_plan` /
    // `read_anchor`, bound as `$14` / `$13`): a keep committing after a
    // relabel then keeps the newer plan/anchor while still applying its own
    // status and grace — while a relabel onto the same shared invoice (the
    // emitted anchor equalling the read anchor) still persists when nothing
    // moved the row meanwhile.
    let (plan_id, paid_through_unix, paid_invoice_id) = if recovered || relabels {
        (item_plan, new_end, anchor_invoice)
    } else {
        (
            previous
                .map(|row| row.plan_id.clone())
                .unwrap_or("free".to_string()),
            previous_end,
            stored_anchor.clone(),
        )
    };
    // Grace: `active` and any recovery clear it; the first failed renewal
    // after a paid period sets it once from that paid end; a stored grace
    // is kept verbatim, never extended; without a paid end there is none.
    let grace_until_unix = if status == "active" || recovered {
        None
    } else if let Some(paid_end) = paid_through_unix {
        previous
            .and_then(|row| row.grace_until_unix)
            .or_else(|| Some(paid_end.saturating_add(RENEWAL_GRACE_SECS)))
    } else {
        None
    };
    ReconcileOutcome {
        row: ReconciledRow {
            stripe_subscription_id: sub.id.clone(),
            stripe_customer_id: remote_customer,
            stripe_item_id: item_id,
            stripe_price_id: price_id,
            plan_id,
            status: status.to_string(),
            current_period_end_unix: sub.current_period_end,
            cancel_at_period_end: sub.cancel_at_period_end,
            paid_through_unix,
            paid_invoice_id,
            grace_until_unix,
        },
        history,
        fail_closed: None,
        explicit_stale: false,
        read_plan: previous.map(|row| row.plan_id.clone()),
        read_anchor: stored_anchor,
    }
}

/// Retain the read row while carrying stale evidence through the write:
/// no anchor, and the evidence end (or the stored end when none was folded).
fn stale_snapshot(
    sub: &Subscription,
    customer: String,
    kept: &LiveRow,
    evidence_end: Option<i64>,
    history: Vec<PaidInvoiceRow>,
) -> ReconcileOutcome {
    ReconcileOutcome {
        row: ReconciledRow {
            stripe_subscription_id: sub.id.clone(),
            stripe_customer_id: customer,
            stripe_item_id: kept.stripe_item_id.clone(),
            stripe_price_id: kept.stripe_price_id.clone(),
            plan_id: kept.plan_id.clone(),
            status: kept.status.clone(),
            current_period_end_unix: sub.current_period_end,
            cancel_at_period_end: sub.cancel_at_period_end,
            paid_through_unix: evidence_end.or(kept.paid_through_unix),
            paid_invoice_id: None,
            grace_until_unix: kept.grace_until_unix,
        },
        history,
        fail_closed: None,
        explicit_stale: true,
        read_plan: Some(kept.plan_id.clone()),
        read_anchor: kept.paid_invoice_id.clone(),
    }
}

/// Fail-closed outcome: the observed remote state is recorded (verbatim
/// status, verbatim item/price bindings when present), nothing advances,
/// and the caller disables new purchases plus raises the alert.
fn fail_closed(
    local: &ReconcileInput,
    sub: &Subscription,
    customer: Option<String>,
    item: Option<(String, String)>,
    reason: FailClosedReason,
) -> ReconcileOutcome {
    let previous = local.live.as_ref();
    let (item_id, price_id) = item.unwrap_or_default();
    ReconcileOutcome {
        row: ReconciledRow {
            stripe_subscription_id: sub.id.clone(),
            stripe_customer_id: customer
                .or_else(|| local.stripe_customer_id.clone())
                .or_else(|| previous.map(|row| row.stripe_customer_id.clone()))
                .unwrap_or_default(),
            stripe_item_id: if item_id.is_empty() {
                previous
                    .map(|row| row.stripe_item_id.clone())
                    .unwrap_or_default()
            } else {
                item_id
            },
            stripe_price_id: if price_id.is_empty() {
                previous
                    .map(|row| row.stripe_price_id.clone())
                    .unwrap_or_default()
            } else {
                price_id
            },
            plan_id: previous
                .map(|row| row.plan_id.clone())
                .unwrap_or("free".to_string()),
            status: sub.status.clone(),
            current_period_end_unix: sub.current_period_end,
            cancel_at_period_end: sub.cancel_at_period_end,
            paid_through_unix: previous.and_then(|row| row.paid_through_unix),
            // Fresh observation, not stale evidence: re-pin the stored
            // anchor so the write guard applies the verbatim status.
            paid_invoice_id: previous.and_then(|row| row.paid_invoice_id.clone()),
            grace_until_unix: previous.and_then(|row| row.grace_until_unix),
        },
        history: Vec::new(),
        fail_closed: Some(reason),
        explicit_stale: false,
        read_plan: previous.map(|row| row.plan_id.clone()),
        read_anchor: previous.and_then(|row| row.paid_invoice_id.clone()),
    }
}

// --- Checkout single flight -------------------------------------------------------
//
// The serialized purchase attempt (ADR 0005 creation contract): at most one
// unresolved operation per user (the `uq_billing_operations_single_flight`
// partial unique), the open Checkout Session reused while open, the Stripe
// idempotency key derived from the operation id. Concurrent attempts
// converge: the second claimer finds the first claimer's open operation and
// reuses its session instead of minting a second subscription.

/// Operation states in the single-flight window. Anything else releases
/// the flight (the partial unique only covers these three).
pub const SINGLE_FLIGHT_STATES: [&str; 3] = ["pending", "submitted", "needs_reconcile"];

/// Terminal mark for an operation deliberately replaced (price-mismatch or
/// dead-session repair): the flight releases, so the replacement inserts a
/// fresh operation with a fresh Stripe idempotency key. A Stripe key is
/// therefore never reused after its session died — repairs cannot replay
/// a dead session inside Stripe's retention window. Never a member of
/// [`SINGLE_FLIGHT_STATES`], so marking it frees the slot.
pub const SUPERSEDED_OP_STATE: &str = "superseded";

/// True while the operation still reserves the per-user single flight.
pub fn is_single_flight_state(state: &str) -> bool {
    SINGLE_FLIGHT_STATES.contains(&state)
}

/// Requested-vs-selected price check for session reuse: an open operation
/// records the price its session was created for (`None` only for corrupt
/// payloads, which keep today's reuse). A second attempt naming a
/// different price must not inherit that session — the caller expires and
/// replaces deliberately instead of handing over the first attempt's
/// price.
pub fn checkout_price_matches(op: &StoredCheckoutOp, price_id: &str) -> bool {
    op.price_id.as_deref().is_none_or(|held| held == price_id)
}

/// A stored checkout operation (payload fields parsed tolerantly — an
/// unreadable payload never blocks the flight, it just carries no
/// session to reuse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCheckoutOp {
    pub id: String,
    pub user_id: String,
    pub state: String,
    pub price_id: Option<String>,
    pub session_id: Option<String>,
    pub subscription_id: Option<String>,
}

/// Outcome of the single-flight checkout-operation insert: `Created`
/// carries the fresh row, `Existing` the winner's row the loser must
/// reuse. Both converge on one operation id — hence one Stripe
/// idempotency key — per user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateOpOutcome {
    Created(StoredCheckoutOp),
    Existing(StoredCheckoutOp),
}

impl CreateOpOutcome {
    /// The converged operation row, whichever side of the race this
    /// caller was on.
    pub fn op(&self) -> &StoredCheckoutOp {
        match self {
            CreateOpOutcome::Created(op) | CreateOpOutcome::Existing(op) => op,
        }
    }
}

/// The `request_payload` shape the store layer persists for checkout
/// operations. Unknown fields are ignored on read.
pub fn checkout_operation_payload(price_id: &str, session_id: Option<&str>) -> String {
    match session_id {
        Some(session_id) => serde_json::json!({
            "kind": "checkout",
            "price_id": price_id,
            "session_id": session_id,
        })
        .to_string(),
        None => serde_json::json!({
            "kind": "checkout",
            "price_id": price_id,
        })
        .to_string(),
    }
}

/// Tolerant payload read: `(price_id, session_id)`, each `None` when
/// absent or misshapen. Never errors — a corrupt payload degrades to no
/// reuse, never to a blocked flight.
pub fn parse_checkout_operation_payload(payload: &str) -> (Option<String>, Option<String>) {
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(_) => return (None, None),
    };
    let string_field = |name: &str| {
        value
            .get(name)
            .and_then(|field| field.as_str())
            .map(str::to_string)
    };
    (string_field("price_id"), string_field("session_id"))
}

/// One purchase step under the single flight: reuse the open session, or
/// create the session under the (existing or new) operation id — whose
/// idempotency key makes a retried creation converge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutStep {
    ReuseSession {
        operation_id: String,
        session_id: String,
    },
    CreateSession {
        operation_id: String,
    },
}

/// Pure single-flight decision: an open operation with a session reuses
/// it; an open operation without one (a previous attempt died before the
/// session create) repairs by creating the session under the SAME
/// operation id; no open operation starts a new one.
pub fn decide_checkout(open: Option<&StoredCheckoutOp>) -> CheckoutStep {
    match open {
        Some(op) => match op.session_id.clone() {
            Some(session_id) => CheckoutStep::ReuseSession {
                operation_id: op.id.clone(),
                session_id,
            },
            None => CheckoutStep::CreateSession {
                operation_id: op.id.clone(),
            },
        },
        None => CheckoutStep::CreateSession {
            operation_id: String::new(),
        },
    }
}

// --- Statements: parameterized reads/writes --------------------------------------
//
// The Worker binds fields positionally (never interpolates a value into
// SQL). Epochs project to BIGINT; writes convert back with
// `to_timestamp($N::bigint)` (NULL stays NULL). The reconcile write runs
// as one transaction under `pg_advisory_xact_lock(hashtext(user_id))` so
// coverage advances monotonically per user; history inserts are
// `ON CONFLICT DO NOTHING` on the invoice id, so retried passes converge.

/// Parameters: user_id. The account's bound Stripe customer, if any.
pub fn reconcile_account_select_statement() -> &'static str {
    "SELECT user_id, stripe_customer_id FROM billing_accounts WHERE user_id = $1"
}

/// Parameters: customer_id. Reverse lookup for webhooks that carry no
/// operation binding (unknown customers ack without state).
pub fn reconcile_user_by_customer_select_statement() -> &'static str {
    "SELECT user_id FROM billing_accounts WHERE stripe_customer_id = $1"
}

/// Parameters: none. The whole price map (a handful of rows): unknown
/// prices fail closed against complete data, never a partial cache.
pub fn price_bindings_select_statement() -> &'static str {
    "SELECT stripe_price_id, plan_id, selectable, livemode FROM plan_prices"
}

/// Parameters: user_id. The live-slot subscription row, if any (the same
/// five nonterminal statuses as `billing.rs` and the reseeded partial
/// unique index: `trialing` and terminal rows sit outside the predicate).
/// Unknown statuses stored verbatim by design (no DB CHECK) fail closed
/// in `reconcile_subscription`, never here.
pub fn reconcile_live_select_statement() -> &'static str {
    "SELECT stripe_subscription_id, stripe_customer_id, stripe_subscription_item_id, stripe_price_id, plan_id, status, cancel_at_period_end, EXTRACT(EPOCH FROM current_period_end)::BIGINT AS current_period_end_unix, EXTRACT(EPOCH FROM paid_through_at)::BIGINT AS paid_through_unix, paid_invoice_id, EXTRACT(EPOCH FROM grace_until)::BIGINT AS grace_until_unix FROM subscriptions WHERE user_id = $1 AND status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')"
}

/// Parameters: user_id, stripe_subscription_id. Select the exact stored row,
/// including statuses outside the live-slot predicate.
pub fn reconcile_target_select_statement() -> &'static str {
    "SELECT stripe_subscription_id, stripe_customer_id, stripe_subscription_item_id, stripe_price_id, plan_id, status, cancel_at_period_end, EXTRACT(EPOCH FROM current_period_end)::BIGINT AS current_period_end_unix, EXTRACT(EPOCH FROM paid_through_at)::BIGINT AS paid_through_unix, paid_invoice_id, EXTRACT(EPOCH FROM grace_until)::BIGINT AS grace_until_unix FROM subscriptions WHERE user_id = $1 AND stripe_subscription_id = $2"
}

/// Parameters: event_id. First claim wins (`true`); duplicates ack 200
/// with no state change. Retried deliveries converge here, before any
/// Stripe call or state write.
pub fn claim_webhook_event_statement() -> &'static str {
    "INSERT INTO billing_webhook_events (stripe_event_id) VALUES ($1) ON CONFLICT DO NOTHING RETURNING stripe_event_id"
}

/// Parameters: event_id. Releases a claim whose handling failed
/// mid-reconcile (Stripe 5xx, store outage): the row is deleted so the
/// retried delivery reprocesses instead of acking 200 on a consumed
/// claim. Only called on the error path — successful handling keeps the
/// row, and duplicates of a *processed* event still ack without change.
pub fn release_webhook_event_statement() -> &'static str {
    "DELETE FROM billing_webhook_events WHERE stripe_event_id = $1"
}

/// Parameters: user_id. The unresolved single-flight operation, if any
/// (at most one: `uq_billing_operations_single_flight`). The payload
/// projects `::text`: the column is JSONB and the store reads it as
/// `String`, which rejects the JSONB binary form.
pub fn open_checkout_operation_select_statement() -> &'static str {
    "SELECT id::text, user_id, kind, state, stripe_subscription_id, request_payload::text AS request_payload FROM billing_operations WHERE user_id = $1 AND state IN ('pending', 'submitted', 'needs_reconcile') LIMIT 1"
}

/// Parameters: op_id. Operation lookup by id (the webhook's
/// `client_reference_id` binding). UUID cast failure is a store error —
/// a verified Stripe event never carries a malformed id. Same `::text`
/// payload cast as the open-flight select (JSONB column, `String` read).
pub fn checkout_operation_by_id_select_statement() -> &'static str {
    "SELECT id::text, user_id, kind, state, stripe_subscription_id, request_payload::text AS request_payload FROM billing_operations WHERE id = $1::uuid"
}

/// Parameters: op_id, user_id, request_key, payload. Single-flight insert:
/// concurrent first attempts converge (the partial unique admits exactly
/// one; the loser reads the winner's row next). Returns the id on insert.
pub fn create_checkout_operation_statement() -> &'static str {
    "INSERT INTO billing_operations (id, user_id, kind, state, request_key, request_payload) VALUES ($1::uuid, $2, 'checkout', 'pending', $3, $4::jsonb) ON CONFLICT (user_id) WHERE state IN ('pending', 'submitted', 'needs_reconcile') DO NOTHING RETURNING id::text"
}

/// Parameters: user_id, customer_id. Bind the Stripe customer to the
/// account, but only when unbound: an existing binding is never
/// overwritten by a racing second customer create.
pub fn bind_account_customer_statement() -> &'static str {
    "INSERT INTO billing_accounts (user_id, stripe_customer_id) VALUES ($1, $2) ON CONFLICT (user_id) DO UPDATE SET stripe_customer_id = EXCLUDED.stripe_customer_id WHERE billing_accounts.stripe_customer_id IS NULL"
}

/// Parameters: op_id, state, payload, subscription_id. Records the created
/// session (and later the terminal mark) on the operation row — but only
/// while the row still holds the single flight: a mark that lands after a
/// deliberate supersede (the creator's Stripe create was still in flight
/// while the replacer expired its session and retired the row) must not
/// resurrect `submitted` on the retired row and re-occupy the flight. The
/// caller gets back whether the mark landed (`RETURNING` row present); a
/// missed mark means the flight moved on, and the caller retries into the
/// replacement instead of acting on the dead row.
pub fn update_checkout_operation_statement() -> &'static str {
    "UPDATE billing_operations SET state = $2, request_payload = $3::jsonb, stripe_subscription_id = $4, updated_at = now() WHERE id = $1::uuid AND state IN ('pending', 'submitted', 'needs_reconcile') RETURNING id::text"
}

/// Parameters: user_id. The per-user lock the reconcile write holds for
/// its transaction: coverage advances monotonically per user while the
/// history inserts converge on the invoice id. A separate statement (not
/// a prefix of the upsert) because parameterized execution takes exactly
/// one command — the store runs the lock first, then the writes.
pub fn reconcile_lock_statement() -> &'static str {
    "SELECT pg_advisory_xact_lock(hashtext($1))"
}

/// Parameters: the reconciled columns. Upsert by the remote subscription
/// id: the row key IS the real Stripe subscription, so no entitlement
/// exists before it. Coverage (`paid_through_at`, and the `plan_id` /
/// `paid_invoice_id` anchored to it) advances monotonically *in the
/// write itself*: the inputs are folded before the per-user advisory
/// lock is taken, so a pass that read an older remote snapshot can
/// commit after a fresher pass — the CASE keeps the greatest confirmed
/// coverage instead of regressing it. An equal-end pass carrying new
/// evidence (a non-NULL anchor) still relabels `plan_id` /
/// `paid_invoice_id`: that is the defined equal-end plan-switch outcome,
/// and only such passes carry an anchor at equal coverage. A fresh keep
/// re-pins the anchor it read, so the tie arm additionally requires the
/// stored row to still match what the pass read (`$14` plan, `$13`
/// anchor): a keep that read before a relabel committed then keeps the
/// newer plan/anchor while still applying its own status and grace — and
/// a relabel onto the same shared invoice still persists when nothing
/// moved the row meanwhile. The same
/// staleness guard covers `status` and `grace_until`: a pass whose money
/// ends strictly before the stored coverage keeps the stored status/grace
/// instead of writing stale verbatim fields over a fresher pass (the
/// stale fold carries its evidence end precisely so this predicate fires
/// even when the stale-reading pass commits second — falling back to
/// the stored end when its snapshot holds no item-plan money at all, so
/// the anchor-less equal-end shape still presents non-NULL evidence).
/// For outcomes without explicit stale proof, item bindings, period end
/// and cancel still update verbatim. Explicit stale proof suppresses the
/// entire subscription write; history is inserted independently.
/// Full staleness predicate (mirrored in both CASEs): EXCLUDED money is
/// non-NULL, stored money is non-NULL, and EXCLUDED ends strictly before
/// stored — or EXCLUDED ties stored while carrying no anchor against an
/// anchored stored row. The fold only emits that anchor-less equal-end
/// shape for a provably stale pass (a snapshot missing the stored
/// anchor); every fresh branch that keeps the stored coverage re-pins the
/// stored anchor precisely so this arm never mistakes a fresh observation
/// for stale evidence. Full advance-or-relabel predicate (mirrored in the
/// plan and anchor CASEs): EXCLUDED money is non-NULL and stored money is
/// NULL, or EXCLUDED ends strictly after stored, or EXCLUDED ties stored
/// while carrying an anchor and the stored plan/anchor still equal what the
/// pass read (`$14` / `$13`).
///
/// Full parameter order:
/// $1 user_id · $2 stripe_subscription_id · $3 stripe_customer_id ·
/// $4 stripe_subscription_item_id · $5 stripe_price_id · $6 plan_id ·
/// $7 status · $8 current_period_end epoch-or-NULL ·
/// $9 cancel_at_period_end · $10 paid_through epoch-or-NULL ·
/// $11 paid_invoice_id-or-NULL · $12 grace_until epoch-or-NULL ·
/// $13 read paid_invoice_id-or-NULL (the stored anchor at fold time) ·
/// $14 read plan_id-or-NULL (the stored plan at fold time) ·
/// $15 explicit stale proof (boolean).
pub fn store_reconciled_subscription_statement() -> &'static str {
    "INSERT INTO subscriptions (user_id, stripe_subscription_id, stripe_customer_id, stripe_subscription_item_id, stripe_price_id, plan_id, status, current_period_end, cancel_at_period_end, paid_through_at, paid_invoice_id, grace_until) SELECT $1, $2, $3, $4, $5, $6, $7, to_timestamp($8::bigint), $9, to_timestamp($10::bigint), $11, to_timestamp($12::bigint) WHERE NOT $15::boolean ON CONFLICT (stripe_subscription_id) DO UPDATE SET stripe_customer_id = EXCLUDED.stripe_customer_id, stripe_subscription_item_id = EXCLUDED.stripe_subscription_item_id, stripe_price_id = EXCLUDED.stripe_price_id, plan_id = CASE WHEN EXCLUDED.paid_through_at IS NOT NULL AND (subscriptions.paid_through_at IS NULL OR EXCLUDED.paid_through_at > subscriptions.paid_through_at OR (EXCLUDED.paid_through_at = subscriptions.paid_through_at AND EXCLUDED.paid_invoice_id IS NOT NULL AND subscriptions.plan_id = $14 AND subscriptions.paid_invoice_id IS NOT DISTINCT FROM $13)) THEN EXCLUDED.plan_id ELSE subscriptions.plan_id END, status = CASE WHEN EXCLUDED.paid_through_at IS NOT NULL AND subscriptions.paid_through_at IS NOT NULL AND (EXCLUDED.paid_through_at < subscriptions.paid_through_at OR (EXCLUDED.paid_through_at = subscriptions.paid_through_at AND EXCLUDED.paid_invoice_id IS NULL AND subscriptions.paid_invoice_id IS NOT NULL)) THEN subscriptions.status ELSE EXCLUDED.status END, current_period_end = EXCLUDED.current_period_end, cancel_at_period_end = EXCLUDED.cancel_at_period_end, paid_through_at = CASE WHEN EXCLUDED.paid_through_at IS NULL THEN subscriptions.paid_through_at WHEN subscriptions.paid_through_at IS NULL THEN EXCLUDED.paid_through_at WHEN EXCLUDED.paid_through_at > subscriptions.paid_through_at THEN EXCLUDED.paid_through_at ELSE subscriptions.paid_through_at END, paid_invoice_id = CASE WHEN EXCLUDED.paid_through_at IS NOT NULL AND (subscriptions.paid_through_at IS NULL OR EXCLUDED.paid_through_at > subscriptions.paid_through_at OR (EXCLUDED.paid_through_at = subscriptions.paid_through_at AND EXCLUDED.paid_invoice_id IS NOT NULL AND subscriptions.plan_id = $14 AND subscriptions.paid_invoice_id IS NOT DISTINCT FROM $13)) THEN COALESCE(EXCLUDED.paid_invoice_id, subscriptions.paid_invoice_id) ELSE subscriptions.paid_invoice_id END, grace_until = CASE WHEN EXCLUDED.paid_through_at IS NOT NULL AND subscriptions.paid_through_at IS NOT NULL AND (EXCLUDED.paid_through_at < subscriptions.paid_through_at OR (EXCLUDED.paid_through_at = subscriptions.paid_through_at AND EXCLUDED.paid_invoice_id IS NULL AND subscriptions.paid_invoice_id IS NOT NULL)) THEN subscriptions.grace_until ELSE EXCLUDED.grace_until END"
}

/// Parameters: user_id · invoice_id · subscription_id · plan_id ·
/// period_start epoch · period_end epoch · amount_cents · currency ·
/// paid_at epoch. Exactly one row per invoice: replays converge on the
/// primary key and insert nothing.
pub fn insert_paid_invoice_history_statement() -> &'static str {
    "INSERT INTO paid_invoice_history (user_id, stripe_invoice_id, stripe_subscription_id, plan_id, period_start, period_end, amount_cents, currency, paid_at) VALUES ($1, $2, $3, $4, to_timestamp($5::bigint), to_timestamp($6::bigint), $7, $8, to_timestamp($9::bigint)) ON CONFLICT DO NOTHING"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::RENEWAL_SETTLEMENT_ALLOWANCE_SECS;
    use crate::stripe::{
        InvoiceLine, InvoiceLines, InvoiceStatusTransitions, SubscriptionItem, SubscriptionItems,
    };

    const NOW: i64 = 1_786_032_000;
    const PRICE_PLUS: &str = "price_plus_test";
    const PRICE_PRO: &str = "price_pro_test";
    const PRICE_FREE: &str = "price_free_test";

    fn bindings() -> Vec<PriceBinding> {
        vec![
            PriceBinding {
                price_id: PRICE_PLUS.to_string(),
                plan_id: "plus".to_string(),
                selectable: true,
                livemode: false,
            },
            PriceBinding {
                price_id: PRICE_PRO.to_string(),
                plan_id: "pro-5x".to_string(),
                selectable: true,
                livemode: false,
            },
            PriceBinding {
                price_id: PRICE_FREE.to_string(),
                plan_id: "free".to_string(),
                selectable: true,
                livemode: false,
            },
        ]
    }

    fn local(plan: &str, paid_through: Option<i64>, grace: Option<i64>) -> ReconcileInput {
        ReconcileInput {
            stripe_customer_id: Some("cus_123".to_string()),
            live: Some(LiveRow {
                stripe_subscription_id: "sub_123".to_string(),
                stripe_customer_id: "cus_123".to_string(),
                stripe_item_id: "si_123".to_string(),
                stripe_price_id: PRICE_PLUS.to_string(),
                plan_id: plan.to_string(),
                status: "active".to_string(),
                paid_through_unix: paid_through,
                paid_invoice_id: None,
                grace_until_unix: grace,
            }),
            bindings: bindings(),
        }
    }

    /// Like [`local`], but the stored row carries the committing invoice's
    /// anchor, as production rows do after any advancing pass.
    fn anchored(plan: &str, paid_through: i64, anchor: &str) -> ReconcileInput {
        let mut input = local(plan, Some(paid_through), None);
        input.live.as_mut().expect("live").paid_invoice_id = Some(anchor.to_string());
        input
    }

    fn fresh() -> ReconcileInput {
        ReconcileInput {
            stripe_customer_id: Some("cus_123".to_string()),
            live: None,
            bindings: bindings(),
        }
    }

    fn subscription(status: &str, price: &str) -> Subscription {
        Subscription {
            id: "sub_123".to_string(),
            customer: Some("cus_123".to_string()),
            status: status.to_string(),
            cancel_at_period_end: false,
            current_period_end: Some(1_786_291_200),
            items: Some(SubscriptionItems {
                data: vec![SubscriptionItem {
                    id: "si_123".to_string(),
                    price: Some(price.to_string()),
                }],
            }),
            livemode: false,
        }
    }

    fn paid_invoice(id: &str, price: &str, start: i64, end: i64, paid_at: i64) -> Invoice {
        Invoice {
            id: id.to_string(),
            customer: Some("cus_123".to_string()),
            subscription: Some("sub_123".to_string()),
            status: "paid".to_string(),
            amount_paid: 800,
            currency: Some("usd".to_string()),
            status_transitions: Some(InvoiceStatusTransitions {
                paid_at: Some(paid_at),
            }),
            lines: Some(InvoiceLines {
                data: vec![InvoiceLine {
                    id: format!("{id}-li"),
                    amount: 800,
                    currency: Some("usd".to_string()),
                    price: Some(price.to_string()),
                    period: Some(crate::stripe::InvoiceLinePeriod {
                        start: Some(start),
                        end: Some(end),
                    }),
                }],
                has_more: false,
            }),
            livemode: false,
        }
    }

    // --- Advance: first paid invoice -------------------------------------------

    #[test]
    fn first_paid_invoice_advances_plan_and_coverage_with_history() {
        let remote = RemoteSubscription {
            subscription: subscription("active", PRICE_PLUS),
            invoices: vec![paid_invoice(
                "in_1",
                PRICE_PLUS,
                NOW - 30 * 86_400,
                NOW + 30 * 86_400,
                NOW,
            )],
        };
        let outcome = reconcile_subscription(&fresh(), &remote, NOW);
        assert_eq!(outcome.fail_closed, None);
        assert!(!outcome.disable_new_purchases());
        assert_eq!(outcome.row.plan_id, "plus");
        assert_eq!(outcome.row.paid_through_unix, Some(NOW + 30 * 86_400));
        assert_eq!(outcome.row.status, "active");
        assert_eq!(outcome.row.paid_invoice_id.as_deref(), Some("in_1"));
        assert_eq!(outcome.row.grace_until_unix, None, "active clears grace");
        assert_eq!(outcome.history.len(), 1);
        let row = &outcome.history[0];
        assert_eq!(row.stripe_invoice_id, "in_1");
        assert_eq!(row.plan_id, "plus");
        assert_eq!(row.period_end_unix, NOW + 30 * 86_400);
        assert_eq!((row.amount_cents, row.currency.as_str()), (800, "usd"));
        assert_eq!(row.paid_at_unix, NOW);
    }

    // --- Monotonicity: duplicates and older invoices never regress --------------

    #[test]
    fn duplicate_and_older_invoices_never_regress() {
        // Later plan/period already confirmed locally (an upgrade paid).
        let confirmed = local("pro-5x", Some(NOW + 60 * 86_400), None);
        let remote = RemoteSubscription {
            subscription: subscription("active", PRICE_PRO),
            invoices: vec![
                // Duplicate of the confirming invoice.
                paid_invoice(
                    "in_9",
                    PRICE_PRO,
                    NOW - 30 * 86_400,
                    NOW + 60 * 86_400,
                    NOW - 100,
                ),
                // Older invoice for the previous plan.
                paid_invoice(
                    "in_1",
                    PRICE_PLUS,
                    NOW - 90 * 86_400,
                    NOW - 30 * 86_400,
                    NOW - 200,
                ),
            ],
        };
        let mut remote = remote;
        // The remote item moved to the upgrade price; both invoices bind.
        remote.subscription.items = Some(SubscriptionItems {
            data: vec![SubscriptionItem {
                id: "si_123".to_string(),
                price: Some(PRICE_PRO.to_string()),
            }],
        });
        let outcome = reconcile_subscription(&confirmed, &remote, NOW);
        assert_eq!(outcome.fail_closed, None);
        // No new money: plan and coverage stay exactly where they were.
        assert_eq!(outcome.row.plan_id, "pro-5x");
        assert_eq!(outcome.row.paid_through_unix, Some(NOW + 60 * 86_400));
        // History records each confirmed service invoice exactly once —
        // including the earlier-plan invoice, labeled with its own plan
        // (the store layer dedupes replays on the invoice id).
        assert_eq!(outcome.history.len(), 2);
        let older = outcome
            .history
            .iter()
            .find(|row| row.stripe_invoice_id == "in_1")
            .expect("earlier-plan invoice rows");
        assert_eq!(older.plan_id, "plus");
        assert_eq!(older.period_end_unix, NOW - 30 * 86_400);
        let current = outcome
            .history
            .iter()
            .find(|row| row.stripe_invoice_id == "in_9")
            .expect("item-plan invoice rows");
        assert_eq!(current.plan_id, "pro-5x");
    }

    // --- Stale snapshots cannot regress status/grace; equal ends relabel ---

    #[test]
    fn stale_snapshot_cannot_regress_status_or_grace() {
        let fresh_end = NOW + 60 * 86_400;
        let stale_end = NOW + 30 * 86_400;
        // Stored: active with later coverage, no grace. A pass folding an
        // older snapshot (past_due, older money) retains everything.
        let stored = local("plus", Some(fresh_end), None);
        let stale = RemoteSubscription {
            subscription: subscription("past_due", PRICE_PLUS),
            invoices: vec![paid_invoice(
                "in_old",
                PRICE_PLUS,
                NOW - 30 * 86_400,
                stale_end,
                NOW - 10,
            )],
        };
        let outcome = reconcile_subscription(&stored, &stale, NOW);
        assert_eq!(outcome.fail_closed, None, "stale is clean, not fail-closed");
        assert_eq!(outcome.row.plan_id, "plus", "stored plan retained");
        assert_eq!(
            outcome.row.paid_through_unix,
            Some(stale_end),
            "stale carries its evidence end so the write guard's staleness predicate fires"
        );
        assert_eq!(outcome.row.status, "active", "stored status retained");
        assert_eq!(outcome.row.grace_until_unix, None, "no grace resurrected");
        assert_eq!(outcome.row.stripe_price_id, PRICE_PLUS);
        // The stale snapshot's invoice still rows for history (the store
        // dedupes replays on the invoice id).
        assert_eq!(outcome.history.len(), 1);
        assert_eq!(outcome.history[0].stripe_invoice_id, "in_old");

        // Mirror direction: stored past_due with a grace, folded over by an
        // older `active` snapshot, keeps the fresher status and grace.
        let grace = fresh_end + RENEWAL_GRACE_SECS;
        let stored = local("plus", Some(fresh_end), Some(grace));
        let mut stored = stored;
        stored.live.as_mut().expect("live").status = "past_due".to_string();
        let stale = RemoteSubscription {
            subscription: subscription("active", PRICE_PLUS),
            invoices: vec![paid_invoice(
                "in_older",
                PRICE_PLUS,
                NOW - 60 * 86_400,
                stale_end,
                NOW - 20,
            )],
        };
        let outcome = reconcile_subscription(&stored, &stale, NOW);
        assert_eq!(outcome.row.status, "past_due");
        assert_eq!(outcome.row.grace_until_unix, Some(grace));
        assert_eq!(
            outcome.row.paid_through_unix,
            Some(stale_end),
            "stale carries its evidence end; the write keeps stored coverage"
        );

        // No item-plan money at all is NOT stale on that basis alone: a
        // first failed renewal observes `past_due` with no new invoice,
        // and that observation must set the grace. The snapshot's status
        // still applies verbatim — only provably older money retains.
        let stored = local("plus", Some(fresh_end), None);
        let failed = RemoteSubscription {
            subscription: subscription("past_due", PRICE_PLUS),
            invoices: vec![],
        };
        let outcome = reconcile_subscription(&stored, &failed, NOW);
        assert_eq!(outcome.row.status, "past_due", "verbatim on empty money");
        assert_eq!(
            outcome.row.grace_until_unix,
            Some(fresh_end + RENEWAL_GRACE_SECS),
            "empty-money failure still sets the first grace"
        );
        assert_eq!(outcome.row.plan_id, "plus");
        assert_eq!(outcome.row.paid_through_unix, Some(fresh_end));
    }

    #[test]
    fn equal_end_plan_switch_adopts_current_item_plan() {
        let end = NOW + 30 * 86_400;
        // Stored plus coverage; the subscription's item moved to pro with a
        // new invoice ending at the same instant: relabel, same coverage,
        // anchor on the new evidence.
        let stored = local("plus", Some(end), None);
        let switched = RemoteSubscription {
            subscription: subscription("active", PRICE_PRO),
            invoices: vec![paid_invoice(
                "in_new",
                PRICE_PRO,
                NOW - 30 * 86_400,
                end,
                NOW,
            )],
        };
        let outcome = reconcile_subscription(&stored, &switched, NOW);
        assert_eq!(outcome.fail_closed, None);
        assert_eq!(outcome.row.plan_id, "pro-5x", "equal end relabels");
        assert_eq!(outcome.row.paid_through_unix, Some(end), "no advance");
        assert_eq!(outcome.row.paid_invoice_id.as_deref(), Some("in_new"));
        assert_eq!(outcome.row.status, "active");
        assert_eq!(outcome.row.grace_until_unix, None);
        assert_eq!(outcome.history.len(), 1);
        assert_eq!(outcome.history[0].plan_id, "pro-5x");

        // Equal end, same plan (duplicate delivery): everything stays, and
        // the anchor refreshes to the identical evidence harmlessly.
        let stored = local("plus", Some(end), None);
        let duplicate = RemoteSubscription {
            subscription: subscription("active", PRICE_PLUS),
            invoices: vec![paid_invoice(
                "in_1",
                PRICE_PLUS,
                NOW - 30 * 86_400,
                end,
                NOW,
            )],
        };
        let outcome = reconcile_subscription(&stored, &duplicate, NOW);
        assert_eq!(outcome.row.plan_id, "plus");
        assert_eq!(outcome.row.paid_through_unix, Some(end));
        assert_eq!(outcome.row.paid_invoice_id.as_deref(), Some("in_1"));
        assert_eq!(outcome.row.status, "active");
    }

    #[test]
    fn tie_past_due_sets_first_grace_but_stale_never_resurrects() {
        let end = NOW - 3_600;
        // Stored active coverage, no grace. An equal-money `past_due`
        // snapshot is NOT stale: the first failed renewal after a paid
        // period sets the grace, exactly as a fresh observation would.
        let stored = local("plus", Some(end), None);
        let failed = RemoteSubscription {
            subscription: subscription("past_due", PRICE_PLUS),
            invoices: vec![paid_invoice(
                "in_1",
                PRICE_PLUS,
                end - 30 * 86_400,
                end,
                NOW,
            )],
        };
        let outcome = reconcile_subscription(&stored, &failed, NOW);
        assert_eq!(outcome.row.status, "past_due");
        assert_eq!(
            outcome.row.grace_until_unix,
            Some(end + RENEWAL_GRACE_SECS),
            "tie observes the failure and sets the grace"
        );
        // An OLDER-money `past_due` snapshot against the same stored row is
        // stale: it must not resurrect a grace (nor flip the status).
        let older = RemoteSubscription {
            subscription: subscription("past_due", PRICE_PLUS),
            invoices: vec![paid_invoice(
                "in_0",
                PRICE_PLUS,
                end - 60 * 86_400,
                end - 30 * 86_400,
                NOW - 100,
            )],
        };
        let outcome = reconcile_subscription(&stored, &older, NOW);
        assert_eq!(outcome.row.status, "active");
        assert_eq!(outcome.row.grace_until_unix, None);
        assert_eq!(
            outcome.row.paid_through_unix,
            Some(end - 30 * 86_400),
            "stale carries its evidence end; the write keeps stored coverage"
        );
    }

    // --- Anchor containment: fresh switch-back vs older snapshot ----------

    #[test]
    fn switchback_failed_renewal_is_fresh_and_sets_grace() {
        // Plus paid through E1; an upgrade to pro paid through E2 > E1; the
        // item then switches back to plus and its renewal fails. The
        // snapshot contains the stored pro anchor, so it is fresh even
        // though the item-plan money ends before the stored coverage: the
        // `past_due` folds verbatim and sets the grace off the stored end
        // instead of being misclassified stale.
        let e1 = NOW + 30 * 86_400;
        let e2 = NOW + 60 * 86_400;
        let stored = anchored("pro-5x", e2, "in_pro");
        let snapshot = RemoteSubscription {
            subscription: subscription("past_due", PRICE_PLUS),
            invoices: vec![
                paid_invoice("in_pro", PRICE_PRO, NOW - 30 * 86_400, e2, NOW - 10),
                paid_invoice("in_plus", PRICE_PLUS, NOW - 60 * 86_400, e1, NOW - 100),
            ],
        };
        let outcome = reconcile_subscription(&stored, &snapshot, NOW);
        assert_eq!(outcome.fail_closed, None);
        assert_eq!(
            outcome.row.plan_id, "pro-5x",
            "no new plan money: plan stays"
        );
        assert_eq!(outcome.row.paid_through_unix, Some(e2));
        assert_eq!(outcome.row.status, "past_due", "fresh failure applies");
        assert_eq!(
            outcome.row.grace_until_unix,
            Some(e2 + RENEWAL_GRACE_SECS),
            "grace anchors on the stored paid end"
        );
        assert_eq!(
            outcome.row.paid_invoice_id.as_deref(),
            Some("in_pro"),
            "fresh keep re-pins the stored anchor"
        );
    }

    #[test]
    fn snapshot_missing_the_stored_anchor_is_stale() {
        // Same stored row, but the snapshot predates the pro payment: it
        // lacks the stored anchor, so it is stale and retains everything.
        let e1 = NOW + 30 * 86_400;
        let e2 = NOW + 60 * 86_400;
        let stored = anchored("pro-5x", e2, "in_pro");
        let stale = RemoteSubscription {
            subscription: subscription("past_due", PRICE_PLUS),
            invoices: vec![paid_invoice(
                "in_plus",
                PRICE_PLUS,
                NOW - 60 * 86_400,
                e1,
                NOW - 100,
            )],
        };
        let outcome = reconcile_subscription(&stored, &stale, NOW);
        assert_eq!(outcome.fail_closed, None, "stale is clean, not fail-closed");
        assert_eq!(outcome.row.plan_id, "pro-5x", "stored plan retained");
        assert_eq!(outcome.row.status, "active", "stored status retained");
        assert_eq!(outcome.row.grace_until_unix, None, "no grace resurrected");
        assert_eq!(
            outcome.row.paid_through_unix,
            Some(e1),
            "stale carries its evidence end; the write keeps stored coverage"
        );
        assert_eq!(
            outcome.row.paid_invoice_id, None,
            "only the stale branch emits the anchor-less shape"
        );
        assert_eq!(
            outcome.read_plan.as_deref(),
            Some("pro-5x"),
            "the write compares the stored plan against what the pass read"
        );
        assert_eq!(
            outcome.read_anchor.as_deref(),
            Some("in_pro"),
            "the write needs the read plan/anchor to tell a moved row from a quiet one"
        );
    }

    #[test]
    fn stale_without_item_plan_money_carries_the_stored_end() {
        // The snapshot lost the stored anchor AND holds no item-plan money
        // (an empty listing): the stale outcome has no evidence end, but
        // the write guard only fires on non-NULL evidence — so the fold
        // falls back to the stored end. With the anchor cleared that is
        // the anchor-less equal-end shape the guard reads as stale.
        let e2 = NOW + 60 * 86_400;
        let stored = anchored("pro-5x", e2, "in_pro");
        let stale = RemoteSubscription {
            subscription: subscription("past_due", PRICE_PLUS),
            invoices: vec![],
        };
        let outcome = reconcile_subscription(&stored, &stale, NOW);
        assert_eq!(outcome.fail_closed, None, "stale is clean, not fail-closed");
        assert_eq!(
            outcome.row.paid_through_unix,
            Some(e2),
            "no evidence end falls back to the stored end so the guard fires"
        );
        assert_eq!(
            outcome.row.paid_invoice_id, None,
            "only the stale branch emits the anchor-less shape"
        );
        assert_eq!(
            outcome.read_plan.as_deref(),
            Some("pro-5x"),
            "the write compares the stored plan against what the pass read"
        );
        assert_eq!(
            outcome.read_anchor.as_deref(),
            Some("in_pro"),
            "the write needs the read plan/anchor to tell a moved row from a quiet one"
        );
    }

    // --- Equal-end ordering: newer-paid evidence only ---------------------

    #[test]
    fn equal_end_relabel_requires_fresher_paid_evidence() {
        let end = NOW + 30 * 86_400;
        // Stored plus coverage anchored on in_plus (paid t1). A fresh
        // snapshot shows the pro switch paid later (t2): relabels.
        let stored = anchored("plus", end, "in_plus");
        let switched = RemoteSubscription {
            subscription: subscription("active", PRICE_PRO),
            invoices: vec![
                paid_invoice("in_plus", PRICE_PLUS, NOW - 30 * 86_400, end, NOW - 200),
                paid_invoice("in_pro", PRICE_PRO, NOW - 30 * 86_400, end, NOW - 100),
            ],
        };
        let outcome = reconcile_subscription(&stored, &switched, NOW);
        assert_eq!(outcome.fail_closed, None);
        assert_eq!(outcome.row.plan_id, "pro-5x", "equal end relabels");
        assert_eq!(outcome.row.paid_through_unix, Some(end), "no advance");
        assert_eq!(outcome.row.paid_invoice_id.as_deref(), Some("in_pro"));

        // A late-arriving older snapshot: the stored pro switch (paid t2)
        // is in the listing, but the equal-end plus evidence is older-paid
        // (t1). The plan must NOT reverse.
        let stored = anchored("pro-5x", end, "in_pro");
        let late = RemoteSubscription {
            subscription: subscription("active", PRICE_PLUS),
            invoices: vec![
                paid_invoice("in_pro", PRICE_PRO, NOW - 30 * 86_400, end, NOW - 100),
                paid_invoice("in_plus", PRICE_PLUS, NOW - 30 * 86_400, end, NOW - 200),
            ],
        };
        let outcome = reconcile_subscription(&stored, &late, NOW);
        assert_eq!(
            outcome.row.plan_id, "pro-5x",
            "older equal-end evidence never reverses a paid switch"
        );
        assert_eq!(outcome.row.paid_through_unix, Some(end));
        assert_eq!(
            outcome.row.paid_invoice_id.as_deref(),
            Some("in_pro"),
            "kept coverage re-pins the stored anchor"
        );
        assert_eq!(outcome.row.status, "active");
    }

    // --- Mixed-plan invoices: history spans only the selected plan --------

    fn mixed_invoice(id: &str, plus_first: bool, paid_at: i64) -> Invoice {
        let (d1, d15, d30) = (NOW - 30 * 86_400, NOW - 15 * 86_400, NOW);
        let plus = InvoiceLine {
            id: format!("{id}-plus"),
            amount: 400,
            currency: Some("usd".to_string()),
            price: Some(PRICE_PLUS.to_string()),
            period: Some(crate::stripe::InvoiceLinePeriod {
                start: Some(d1),
                end: Some(d15),
            }),
        };
        let pro = InvoiceLine {
            id: format!("{id}-pro"),
            amount: 900,
            currency: Some("usd".to_string()),
            price: Some(PRICE_PRO.to_string()),
            period: Some(crate::stripe::InvoiceLinePeriod {
                start: Some(d15),
                end: Some(d30),
            }),
        };
        let mut invoice = paid_invoice(id, PRICE_PLUS, d1, d15, paid_at);
        invoice.lines = Some(InvoiceLines {
            data: if plus_first {
                vec![plus, pro]
            } else {
                vec![pro, plus]
            },
            has_more: false,
        });
        invoice
    }

    #[test]
    fn mixed_plan_invoice_history_spans_only_the_selected_plan() {
        // Mid-cycle upgrade on one invoice: a plus line for days 1–15 and
        // a pro line for days 15–30. The history row names the
        // latest-ending plan (pro) and spans only the pro line — the plus
        // days are never attributed to pro, whichever order the lines
        // arrive in.
        for (id, plus_first) in [("in_mix_a", true), ("in_mix_b", false)] {
            let remote = RemoteSubscription {
                subscription: subscription("active", PRICE_PRO),
                invoices: vec![mixed_invoice(id, plus_first, NOW)],
            };
            let outcome = reconcile_subscription(&fresh(), &remote, NOW);
            assert_eq!(outcome.fail_closed, None);
            assert_eq!(outcome.history.len(), 1);
            let row = &outcome.history[0];
            assert_eq!(row.stripe_invoice_id, id);
            assert_eq!(row.plan_id, "pro-5x");
            assert_eq!(
                (row.period_start_unix, row.period_end_unix),
                (NOW - 15 * 86_400, NOW),
                "span covers the pro line only"
            );
        }
    }

    #[test]
    fn interleaved_plan_history_is_permutation_invariant_and_preserves_ties() {
        let permutations = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        for plus_end in [15, 30] {
            let line = |id, price, start, end| {
                paid_invoice(id, price, NOW + start, NOW + end, NOW)
                    .lines
                    .expect("lines")
                    .data
                    .remove(0)
            };
            let lines = [
                line("pro_early", PRICE_PRO, 1, 10),
                line("plus", PRICE_PLUS, 0, plus_end),
                line("pro_late", PRICE_PRO, 10, 30),
            ];
            for order in permutations {
                let mut invoice = paid_invoice("in_interleaved", PRICE_PRO, NOW, NOW + 30, NOW);
                invoice.lines.as_mut().expect("lines").data =
                    order.map(|i| lines[i].clone()).to_vec();
                let remote = RemoteSubscription {
                    subscription: subscription("active", PRICE_PRO),
                    invoices: vec![invoice],
                };
                let outcome = reconcile_subscription(&fresh(), &remote, NOW);
                assert_eq!(outcome.fail_closed, None);
                assert_eq!(outcome.history.len(), 1);
                // Equal latest ends keep the first line reaching that end,
                // not the first plan encountered or map iteration order.
                let plus_wins = plus_end == 30
                    && order.iter().position(|&i| i == 1) < order.iter().position(|&i| i == 2);
                let row = &outcome.history[0];
                assert_eq!(row.plan_id, if plus_wins { "plus" } else { "pro-5x" });
                assert_eq!(row.period_start_unix, NOW + if plus_wins { 0 } else { 1 });
                assert_eq!(row.period_end_unix, NOW + 30);
                assert_eq!(row.stripe_invoice_id, "in_interleaved");
                assert_eq!((row.amount_cents, row.currency.as_str()), (800, "usd"));
                assert_eq!(row.paid_at_unix, NOW);
                assert_eq!(outcome.row.plan_id, "pro-5x", "coverage uses the item plan");
                assert_eq!(outcome.row.paid_through_unix, Some(NOW + 30));
                assert_eq!(reconcile_subscription(&fresh(), &remote, NOW), outcome);
            }
        }
    }

    #[test]
    fn same_plan_multiline_invoice_history_spans_all_its_lines() {
        // The scoping above must not drop same-plan days: two pro lines
        // arriving latest-end-last still span their full range.
        let mut invoice = paid_invoice("in_pro2", PRICE_PRO, NOW - 10 * 86_400, NOW, NOW);
        invoice
            .lines
            .as_mut()
            .expect("lines")
            .data
            .push(InvoiceLine {
                id: "in_pro2-early".to_string(),
                amount: 900,
                currency: Some("usd".to_string()),
                price: Some(PRICE_PRO.to_string()),
                period: Some(crate::stripe::InvoiceLinePeriod {
                    start: Some(NOW - 20 * 86_400),
                    end: Some(NOW - 5 * 86_400),
                }),
            });
        let remote = RemoteSubscription {
            subscription: subscription("active", PRICE_PRO),
            invoices: vec![invoice],
        };
        let outcome = reconcile_subscription(&fresh(), &remote, NOW);
        assert_eq!(outcome.history.len(), 1);
        assert_eq!(outcome.history[0].plan_id, "pro-5x");
        assert_eq!(
            (
                outcome.history[0].period_start_unix,
                outcome.history[0].period_end_unix
            ),
            (NOW - 20 * 86_400, NOW),
        );
    }

    #[test]
    fn unrelated_one_off_and_credit_lines_confer_nothing() {
        let mut invoice = paid_invoice(
            "in_2",
            PRICE_PLUS,
            NOW - 30 * 86_400,
            NOW + 30 * 86_400,
            NOW,
        );
        let lines = invoice.lines.as_mut().expect("lines");
        // One-off (unknown price), credit (negative), and free-price lines.
        lines.data.push(InvoiceLine {
            id: "in_2-extra".to_string(),
            amount: 100,
            currency: Some("usd".to_string()),
            price: Some("price_oneoff".to_string()),
            period: Some(crate::stripe::InvoiceLinePeriod {
                start: Some(NOW),
                end: Some(NOW + 86_400),
            }),
        });
        lines.data.push(InvoiceLine {
            id: "in_2-credit".to_string(),
            amount: -800,
            currency: Some("usd".to_string()),
            price: Some(PRICE_PLUS.to_string()),
            period: Some(crate::stripe::InvoiceLinePeriod {
                start: Some(NOW),
                end: Some(NOW + 86_400),
            }),
        });
        lines.data.push(InvoiceLine {
            id: "in_2-free".to_string(),
            amount: 0,
            currency: Some("usd".to_string()),
            price: Some(PRICE_FREE.to_string()),
            period: Some(crate::stripe::InvoiceLinePeriod {
                start: Some(NOW),
                end: Some(NOW + 86_400),
            }),
        });
        let remote = RemoteSubscription {
            subscription: subscription("active", PRICE_PLUS),
            invoices: vec![invoice],
        };
        let outcome = reconcile_subscription(&fresh(), &remote, NOW);
        assert_eq!(
            outcome.fail_closed, None,
            "skipped lines never fail the pass"
        );
        // Coverage comes from the service line only — not the one-off end.
        assert_eq!(outcome.row.paid_through_unix, Some(NOW + 30 * 86_400));
        assert_eq!(outcome.history.len(), 1);
    }

    #[test]
    fn paid_but_unanchored_invoice_is_skipped() {
        let mut invoice = paid_invoice(
            "in_3",
            PRICE_PLUS,
            NOW - 30 * 86_400,
            NOW + 30 * 86_400,
            NOW,
        );
        invoice.status_transitions = Some(InvoiceStatusTransitions { paid_at: None });
        let remote = RemoteSubscription {
            subscription: subscription("active", PRICE_PLUS),
            invoices: vec![invoice],
        };
        let outcome = reconcile_subscription(&fresh(), &remote, NOW);
        assert_eq!(outcome.fail_closed, None);
        assert_eq!(
            outcome.row.paid_through_unix, None,
            "no anchor, no coverage"
        );
        assert!(outcome.history.is_empty(), "no anchor, no history row");
    }

    // --- Fail-closed: unknown price / livemode / binding / status -----------------

    #[test]
    fn unknown_price_records_state_without_advance_and_disables() {
        let remote = RemoteSubscription {
            subscription: subscription("active", "price_never_seeded"),
            invoices: vec![],
        };
        let outcome = reconcile_subscription(&fresh(), &remote, NOW);
        assert_eq!(outcome.fail_closed, Some(FailClosedReason::UnknownPrice));
        assert!(outcome.disable_new_purchases());
        assert_eq!(
            outcome.row.status, "active",
            "observed state recorded verbatim"
        );
        assert_eq!(outcome.row.paid_through_unix, None, "no advance");
        assert_eq!(outcome.row.plan_id, "free");
        assert!(outcome.history.is_empty());
    }

    #[test]
    fn trialing_and_unknown_statuses_fail_closed() {
        for status in ["trialing", "suspended", "weird-future-state"] {
            let remote = RemoteSubscription {
                subscription: subscription(status, PRICE_PLUS),
                invoices: vec![],
            };
            // Even with local coverage: retained, never advanced.
            let outcome =
                reconcile_subscription(&local("plus", Some(NOW + 100), None), &remote, NOW);
            assert_eq!(
                outcome.fail_closed,
                Some(FailClosedReason::UnknownStatus),
                "status {status}"
            );
            assert!(outcome.disable_new_purchases(), "status {status}");
            assert_eq!(outcome.row.status, status, "verbatim");
            assert_eq!(
                outcome.row.paid_through_unix,
                Some(NOW + 100),
                "retained, status {status}"
            );
        }
    }

    #[test]
    fn live_remote_objects_and_customer_mismatch_fail_closed() {
        let mut remote = RemoteSubscription {
            subscription: subscription("active", PRICE_PLUS),
            invoices: vec![paid_invoice("in_1", PRICE_PLUS, NOW - 1, NOW + 1, NOW)],
        };
        remote.subscription.livemode = true;
        let outcome = reconcile_subscription(&fresh(), &remote, NOW);
        assert_eq!(outcome.fail_closed, Some(FailClosedReason::WrongLivemode));

        let mut remote = RemoteSubscription {
            subscription: subscription("active", PRICE_PLUS),
            invoices: vec![],
        };
        remote.subscription.livemode = false;
        remote.subscription.customer = Some("cus_other".to_string());
        let outcome = reconcile_subscription(&fresh(), &remote, NOW);
        assert_eq!(
            outcome.fail_closed,
            Some(FailClosedReason::CustomerMismatch)
        );

        // Multi-item subscriptions never resolve by picking one.
        let mut remote = RemoteSubscription {
            subscription: subscription("active", PRICE_PLUS),
            invoices: vec![],
        };
        remote.subscription.items = Some(SubscriptionItems {
            data: vec![
                SubscriptionItem {
                    id: "si_1".to_string(),
                    price: Some(PRICE_PLUS.to_string()),
                },
                SubscriptionItem {
                    id: "si_2".to_string(),
                    price: Some(PRICE_PRO.to_string()),
                },
            ],
        });
        let outcome = reconcile_subscription(&fresh(), &remote, NOW);
        assert_eq!(outcome.fail_closed, Some(FailClosedReason::AmbiguousItems));
    }

    // --- Known non-entitling statuses: recorded, quiet -------------------------------

    #[test]
    fn held_statuses_record_without_alert_or_disable() {
        for status in ["incomplete", "unpaid", "paused"] {
            let remote = RemoteSubscription {
                subscription: subscription(status, PRICE_PLUS),
                invoices: vec![],
            };
            let outcome = reconcile_subscription(&fresh(), &remote, NOW);
            assert_eq!(outcome.fail_closed, None, "status {status}");
            assert!(!outcome.disable_new_purchases(), "status {status}");
            assert_eq!(outcome.row.status, status);
            assert_eq!(outcome.row.plan_id, "free", "no local plan to retain");
            assert!(outcome.history.is_empty());
        }
    }

    #[test]
    fn terminal_statuses_release_the_slot_and_clear_grace() {
        for status in ["canceled", "incomplete_expired"] {
            let remote = RemoteSubscription {
                subscription: subscription(status, PRICE_PLUS),
                invoices: vec![],
            };
            let outcome = reconcile_subscription(
                &local("plus", Some(NOW + 100), Some(NOW + 200)),
                &remote,
                NOW,
            );
            assert_eq!(outcome.fail_closed, None, "status {status}");
            assert_eq!(outcome.row.status, status);
            assert_eq!(outcome.row.plan_id, "plus", "history retained");
            assert_eq!(outcome.row.paid_through_unix, Some(NOW + 100));
            assert_eq!(outcome.row.grace_until_unix, None, "terminal clears grace");
        }
    }

    #[test]
    fn fresh_held_and_terminal_snapshots_keep_verbatim_status() {
        let end = NOW + 100;
        let grace = end + RENEWAL_GRACE_SECS;
        for status in [
            "incomplete",
            "unpaid",
            "paused",
            "canceled",
            "incomplete_expired",
        ] {
            for has_anchor in [false, true] {
                let mut input = local("plus", Some(end), Some(grace));
                let mut remote = RemoteSubscription {
                    subscription: subscription(status, PRICE_PRO),
                    invoices: vec![],
                };
                if has_anchor {
                    input.live.as_mut().expect("live").paid_invoice_id =
                        Some("in_seed".to_string());
                    // Freshness depends on containment, not whether the
                    // current item's plan has money as late as stored coverage.
                    remote
                        .invoices
                        .push(paid_invoice("in_seed", PRICE_PLUS, NOW, end, NOW));
                }
                let outcome = reconcile_subscription(&input, &remote, NOW);
                assert_eq!(outcome.fail_closed, None);
                assert_eq!(outcome.row.status, status);
                assert_eq!(outcome.row.plan_id, "plus");
                assert_eq!(outcome.row.paid_through_unix, Some(end));
                assert_eq!(
                    outcome.row.paid_invoice_id.as_deref(),
                    has_anchor.then_some("in_seed")
                );
                assert_eq!(
                    outcome.row.grace_until_unix,
                    if is_terminal_status(status) {
                        None
                    } else {
                        Some(grace)
                    }
                );
                assert_eq!(outcome.row.stripe_price_id, PRICE_PRO);
                assert!(outcome.history.is_empty());
            }
        }
    }

    #[test]
    fn terminal_replay_does_not_use_replacement_subscription_anchor() {
        // A delayed completed Checkout for the old subscription can be
        // retrieved after its replacement owns the user's live slot. That
        // replacement's invoice cannot prove the old subscription stale.
        let mut input = anchored("plus", NOW + 100, "in_replacement");
        input.live.as_mut().expect("live").stripe_subscription_id = "sub_replacement".to_string();
        for status in ["canceled", "incomplete_expired"] {
            for empty in [false, true] {
                let remote = RemoteSubscription {
                    subscription: subscription(status, PRICE_PLUS),
                    invoices: if empty {
                        vec![]
                    } else {
                        vec![paid_invoice(
                            "in_old",
                            PRICE_PLUS,
                            NOW - 100,
                            NOW,
                            NOW - 100,
                        )]
                    },
                };
                let outcome = reconcile_subscription(&input, &remote, NOW);
                assert_eq!(outcome.fail_closed, None);
                assert_eq!(outcome.row.stripe_subscription_id, "sub_123");
                assert_eq!(
                    outcome.row.status, status,
                    "old subscription must not become active"
                );
                assert_eq!(outcome.row.grace_until_unix, None);
                assert!(outcome.history.is_empty());
            }
        }
    }

    // --- Renewal grace: set once, kept verbatim, cleared on recovery -----------------

    #[test]
    fn first_failed_renewal_sets_fixed_grace_duplicates_keep_it() {
        let end = NOW - 3_600;
        let past_due = RemoteSubscription {
            subscription: subscription("past_due", PRICE_PLUS),
            invoices: vec![],
        };
        // First observation: grace anchors on the paid end, exactly +7d.
        let first = reconcile_subscription(&local("plus", Some(end), None), &past_due, NOW);
        assert_eq!(first.fail_closed, None);
        assert_eq!(first.row.paid_through_unix, Some(end));
        assert_eq!(first.row.grace_until_unix, Some(end + RENEWAL_GRACE_SECS));
        assert_eq!(RENEWAL_GRACE_SECS, 604_800);

        // Duplicate event: same stored grace, never extended.
        let input = ReconcileInput {
            stripe_customer_id: Some("cus_123".to_string()),
            live: Some(LiveRow {
                stripe_subscription_id: "sub_123".to_string(),
                stripe_customer_id: "cus_123".to_string(),
                stripe_item_id: "si_123".to_string(),
                stripe_price_id: PRICE_PLUS.to_string(),
                plan_id: "plus".to_string(),
                status: "past_due".to_string(),
                paid_through_unix: Some(end),
                paid_invoice_id: None,
                grace_until_unix: Some(end + RENEWAL_GRACE_SECS),
            }),
            bindings: bindings(),
        };
        let second = reconcile_subscription(&input, &past_due, NOW + 3_600);
        assert_eq!(second.row.grace_until_unix, Some(end + RENEWAL_GRACE_SECS));
    }

    #[test]
    fn failed_first_payment_gets_no_grace_and_recovery_clears() {
        // No paid end: past_due on a first payment sets no grace.
        let past_due = RemoteSubscription {
            subscription: subscription("past_due", PRICE_PLUS),
            invoices: vec![],
        };
        let outcome = reconcile_subscription(&fresh(), &past_due, NOW);
        assert_eq!(outcome.fail_closed, None);
        assert_eq!(outcome.row.grace_until_unix, None);

        // Recovery: new money reconciled clears a stored grace.
        let end = NOW - 3_600;
        let mut input = local("plus", Some(end), Some(end + RENEWAL_GRACE_SECS));
        input.live.as_mut().expect("live").status = "past_due".to_string();
        let recovered = RemoteSubscription {
            subscription: subscription("active", PRICE_PLUS),
            invoices: vec![paid_invoice(
                "in_2",
                PRICE_PLUS,
                end,
                NOW + 30 * 86_400,
                NOW,
            )],
        };
        let outcome = reconcile_subscription(&input, &recovered, NOW);
        assert_eq!(outcome.row.paid_through_unix, Some(NOW + 30 * 86_400));
        assert_eq!(outcome.row.grace_until_unix, None, "recovery clears grace");
        assert_eq!(outcome.history.len(), 1, "only the new invoice rows");
    }

    // --- Settlement allowance is the read path's, not a copy ---------------------------

    #[test]
    fn settlement_allowance_matches_the_read_path_constant() {
        assert_eq!(
            RENEWAL_SETTLEMENT_ALLOWANCE_SECS, 10_800,
            "the reconcile slice reuses billing.rs, never a copy",
        );
    }

    // --- Single flight --------------------------------------------------------------------

    #[test]
    fn checkout_steps_reuse_repair_or_start() {
        // Open operation with a session: every concurrent attempt reuses it
        // (same operation id → same Stripe idempotency key → one session).
        let open = StoredCheckoutOp {
            id: "op-1".to_string(),
            user_id: "u1".to_string(),
            state: "submitted".to_string(),
            price_id: Some(PRICE_PLUS.to_string()),
            session_id: Some("cs_123".to_string()),
            subscription_id: None,
        };
        assert_eq!(
            decide_checkout(Some(&open)),
            CheckoutStep::ReuseSession {
                operation_id: "op-1".to_string(),
                session_id: "cs_123".to_string(),
            }
        );
        // Open operation without a session (died before the create): repair
        // under the SAME id instead of minting a second flight.
        let orphan = StoredCheckoutOp {
            session_id: None,
            ..open.clone()
        };
        assert_eq!(
            decide_checkout(Some(&orphan)),
            CheckoutStep::CreateSession {
                operation_id: "op-1".to_string(),
            }
        );
        // No flight: the caller mints the id, then inserts single-flight.
        assert_eq!(
            decide_checkout(None),
            CheckoutStep::CreateSession {
                operation_id: String::new(),
            }
        );
    }

    #[test]
    fn single_flight_states_match_the_partial_unique() {
        assert!(is_single_flight_state("pending"));
        assert!(is_single_flight_state("submitted"));
        assert!(is_single_flight_state("needs_reconcile"));
        for released in ["succeeded", "failed", "canceled", SUPERSEDED_OP_STATE, ""] {
            assert!(!is_single_flight_state(released), "{released} releases");
        }
    }

    #[test]
    fn price_check_matches_held_price_only() {
        let op = StoredCheckoutOp {
            id: "op-1".to_string(),
            user_id: "u1".to_string(),
            state: "submitted".to_string(),
            price_id: Some(PRICE_PLUS.to_string()),
            session_id: Some("cs_123".to_string()),
            subscription_id: None,
        };
        assert!(checkout_price_matches(&op, PRICE_PLUS));
        assert!(!checkout_price_matches(&op, PRICE_PRO));
        // A corrupt payload (no recorded price) keeps today's reuse rather
        // than stranding the flight.
        let corrupt = StoredCheckoutOp {
            price_id: None,
            ..op
        };
        assert!(checkout_price_matches(&corrupt, PRICE_PRO));
    }

    #[test]
    fn operation_payload_round_trips_and_tolerates_corruption() {
        let payload = checkout_operation_payload(PRICE_PLUS, Some("cs_123"));
        assert_eq!(
            parse_checkout_operation_payload(&payload),
            (Some(PRICE_PLUS.to_string()), Some("cs_123".to_string()))
        );
        let bare = checkout_operation_payload(PRICE_PLUS, None);
        assert_eq!(
            parse_checkout_operation_payload(&bare),
            (Some(PRICE_PLUS.to_string()), None)
        );
        // Corrupt payloads degrade to no reuse, never to a blocked flight.
        assert_eq!(parse_checkout_operation_payload("not json"), (None, None));
        assert_eq!(
            parse_checkout_operation_payload(r#"{"price_id": 7}"#),
            (None, None)
        );
    }

    // --- Statements --------------------------------------------------------------------------

    #[test]
    fn statements_bind_positionally_and_stay_pinned() {
        assert!(
            reconcile_live_select_statement()
                .contains("status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')"),
            "live slot matches the read path predicate",
        );
        assert!(
            reconcile_live_select_statement().contains("paid_invoice_id"),
            "the fold needs the stored anchor for its staleness and equal-end order checks",
        );
        // Session marks only land on unresolved rows: a mark arriving after
        // a deliberate supersede reports no row instead of resurrecting the
        // retired row into the single flight.
        assert!(
            update_checkout_operation_statement().contains(
                "WHERE id = $1::uuid AND state IN ('pending', 'submitted', 'needs_reconcile') RETURNING id::text"
            ),
            "late session marks must miss retired rows",
        );
        assert!(
            create_checkout_operation_statement().contains(
                "ON CONFLICT (user_id) WHERE state IN ('pending', 'submitted', 'needs_reconcile')"
            ),
            "insert converges on the single-flight index",
        );
        assert!(
            insert_paid_invoice_history_statement().contains("ON CONFLICT DO NOTHING"),
            "history replays converge on the invoice id",
        );
        assert!(
            reconcile_lock_statement().contains("pg_advisory_xact_lock(hashtext($1))"),
            "coverage advances under the per-user lock",
        );
        assert!(
            store_reconciled_subscription_statement()
                .contains("WHEN EXCLUDED.paid_through_at > subscriptions.paid_through_at"),
            "coverage advances monotonically in the write itself: a pass folded from an older snapshot never regresses confirmed coverage",
        );
        // The same staleness predicate guards status and grace: a stale
        // pass committing second keeps the stored verbatim fields instead
        // of writing older ones over a fresher pass — including the
        // anchor-less equal-end shape only the stale fold emits (fresh
        // branches re-pin the stored anchor precisely to stay
        // distinguishable from it).
        for guarded in [
            "status = CASE WHEN EXCLUDED.paid_through_at IS NOT NULL AND subscriptions.paid_through_at IS NOT NULL AND (EXCLUDED.paid_through_at < subscriptions.paid_through_at OR (EXCLUDED.paid_through_at = subscriptions.paid_through_at AND EXCLUDED.paid_invoice_id IS NULL AND subscriptions.paid_invoice_id IS NOT NULL)) THEN subscriptions.status ELSE EXCLUDED.status END",
            "grace_until = CASE WHEN EXCLUDED.paid_through_at IS NOT NULL AND subscriptions.paid_through_at IS NOT NULL AND (EXCLUDED.paid_through_at < subscriptions.paid_through_at OR (EXCLUDED.paid_through_at = subscriptions.paid_through_at AND EXCLUDED.paid_invoice_id IS NULL AND subscriptions.paid_invoice_id IS NOT NULL)) THEN subscriptions.grace_until ELSE EXCLUDED.grace_until END",
        ] {
            assert!(
                store_reconciled_subscription_statement().contains(guarded),
                "stale-fold guard missing: {guarded}",
            );
        }
        // The equal-end relabel: a pass tying stored coverage while
        // carrying an anchor adopts the relabeled plan and anchor — but
        // only when the stored row still matches what the pass read (plan
        // `$14`, anchor `$13`): a keep committing after a relabel then
        // keeps the newer plan/anchor, an older pass committing after a
        // newer relabel cannot revert it, and a relabel onto the same
        // shared invoice still persists. Anchor-less ties keep the stored
        // ones.
        for guarded in [
            "plan_id = CASE WHEN EXCLUDED.paid_through_at IS NOT NULL AND (subscriptions.paid_through_at IS NULL OR EXCLUDED.paid_through_at > subscriptions.paid_through_at OR (EXCLUDED.paid_through_at = subscriptions.paid_through_at AND EXCLUDED.paid_invoice_id IS NOT NULL AND subscriptions.plan_id = $14 AND subscriptions.paid_invoice_id IS NOT DISTINCT FROM $13)) THEN EXCLUDED.plan_id ELSE subscriptions.plan_id END",
            "paid_invoice_id = CASE WHEN EXCLUDED.paid_through_at IS NOT NULL AND (subscriptions.paid_through_at IS NULL OR EXCLUDED.paid_through_at > subscriptions.paid_through_at OR (EXCLUDED.paid_through_at = subscriptions.paid_through_at AND EXCLUDED.paid_invoice_id IS NOT NULL AND subscriptions.plan_id = $14 AND subscriptions.paid_invoice_id IS NOT DISTINCT FROM $13)) THEN COALESCE(EXCLUDED.paid_invoice_id, subscriptions.paid_invoice_id) ELSE subscriptions.paid_invoice_id END",
        ] {
            assert!(
                store_reconciled_subscription_statement().contains(guarded),
                "equal-end relabel missing: {guarded}",
            );
        }
        assert!(
            release_webhook_event_statement()
                .contains("DELETE FROM billing_webhook_events WHERE stripe_event_id = $1"),
            "a failed handling releases the claim so the retry reprocesses",
        );
        // `request_payload` is the only JSONB column the billing reads
        // project (every other select carries TEXT/BOOL/BIGINT, which the
        // `String`/`bool`/`i64` reads accept): both operation lookups must
        // cast it to text, or every stored-operation read fails in
        // production while the struct fakes keep passing.
        for select in [
            open_checkout_operation_select_statement(),
            checkout_operation_by_id_select_statement(),
        ] {
            assert!(
                select.contains("request_payload::text"),
                "operation lookup must read the JSONB payload as text, got: {select}",
            );
        }
    }
}
