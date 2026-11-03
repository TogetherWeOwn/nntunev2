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
//!   plan/period; the plan only changes on newly reconciled money.
//! - Renewal grace: the first failed renewal after a paid period sets
//!   `grace_until` to that paid end plus [`RENEWAL_GRACE_SECS`]. Retries
//!   and duplicate events never extend a stored grace; successful recovery
//!   (new money reconciled) clears it, as does `active` and terminal
//!   state. A failed FIRST payment has no paid end and gets no grace.
//! - History: every confirmed paid service invoice yields exactly one
//!   [`PaidInvoiceRow`] (idempotent on the invoice id at the store layer),
//!   anchored on `status_transitions.paid_at`. Invoices without it are
//!   skipped: no anchor, no coverage, no history row.
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
/// are Unix seconds; `None` where the DB column is NULL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveRow {
    pub stripe_subscription_id: String,
    pub stripe_customer_id: String,
    pub stripe_item_id: String,
    pub stripe_price_id: String,
    pub plan_id: String,
    pub status: String,
    pub paid_through_unix: Option<i64>,
    pub grace_until_unix: Option<i64>,
}

/// Everything reconciliation needs from the store: the account's Stripe
/// customer (if bound yet), the live subscription row (if any), and the
/// full `plan_prices` map (a handful of rows; loaded whole so unknown
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

/// One qualifying service line: positive amount, a test-mode mapping to
/// the item's plan (never `free`, never another plan), and a bounded
/// period end. Everything else is skipped for coverage — one-off, credit,
/// free-price, unrelated-plan, unknown-price, or open-ended lines confer
/// nothing and never fail the pass on their own.
fn qualifying_end(
    bindings: &[PriceBinding],
    item_plan: &str,
    line_price: Option<&str>,
    amount: i64,
    period_end: Option<i64>,
) -> Option<i64> {
    if amount <= 0 || item_plan == "free" {
        return None;
    }
    let price_id = line_price?;
    let plan = resolve_plan(bindings, price_id)?;
    if plan != item_plan {
        return None;
    }
    period_end
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
                paid_invoice_id: None,
                grace_until_unix: None,
            },
            history: Vec::new(),
            fail_closed: None,
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
                paid_invoice_id: None,
                grace_until_unix: previous.and_then(|row| row.grace_until_unix),
            },
            history: Vec::new(),
            fail_closed: None,
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
    let mut history = Vec::new();
    let mut new_end: Option<i64> = None;
    let mut anchor_invoice: Option<String> = None;
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
        let mut period_start: Option<i64> = None;
        let mut period_end: Option<i64> = None;
        for line in lines {
            let line_end = qualifying_end(
                &local.bindings,
                &item_plan,
                line.price.as_deref(),
                line.amount,
                line.period.as_ref().and_then(|period| period.end),
            );
            let Some(line_end) = line_end else {
                continue;
            };
            let line_start = line
                .period
                .as_ref()
                .and_then(|period| period.start)
                .unwrap_or(line_end);
            period_start =
                Some(period_start.map_or(line_start, |start: i64| start.min(line_start)));
            period_end = Some(period_end.map_or(line_end, |end: i64| end.max(line_end)));
        }
        let (Some(start), Some(end)) = (period_start, period_end) else {
            continue;
        };
        history.push(PaidInvoiceRow {
            stripe_invoice_id: invoice.id.clone(),
            stripe_subscription_id: sub.id.clone(),
            plan_id: item_plan.clone(),
            period_start_unix: start,
            period_end_unix: end,
            amount_cents: invoice.amount_paid,
            currency,
            paid_at_unix: paid_at,
        });
        if new_end.is_none_or(|current| end > current) {
            new_end = Some(end);
            anchor_invoice = Some(invoice.id.clone());
        }
    }

    let previous = local.live.as_ref();
    let previous_end = previous.and_then(|row| row.paid_through_unix);
    // Monotonic coverage: duplicates and older invoices never regress a
    // later confirmed plan/period; the plan only changes on newly
    // reconciled money.
    let recovered = new_end.is_some_and(|end| previous_end.is_none_or(|prev| end > prev));
    let (plan_id, paid_through_unix, paid_invoice_id) = if recovered {
        (item_plan, new_end, anchor_invoice)
    } else {
        (
            previous
                .map(|row| row.plan_id.clone())
                .unwrap_or("free".to_string()),
            previous_end,
            None,
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
            paid_invoice_id: None,
            grace_until_unix: previous.and_then(|row| row.grace_until_unix),
        },
        history: Vec::new(),
        fail_closed: Some(reason),
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

/// True while the operation still reserves the per-user single flight.
pub fn is_single_flight_state(state: &str) -> bool {
    SINGLE_FLIGHT_STATES.contains(&state)
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
    "SELECT stripe_subscription_id, stripe_customer_id, stripe_subscription_item_id, stripe_price_id, plan_id, status, cancel_at_period_end, EXTRACT(EPOCH FROM current_period_end)::BIGINT AS current_period_end_unix, EXTRACT(EPOCH FROM paid_through_at)::BIGINT AS paid_through_unix, EXTRACT(EPOCH FROM grace_until)::BIGINT AS grace_until_unix FROM subscriptions WHERE user_id = $1 AND status IN ('incomplete', 'active', 'past_due', 'unpaid', 'paused')"
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
/// (at most one: `uq_billing_operations_single_flight`).
pub fn open_checkout_operation_select_statement() -> &'static str {
    "SELECT id::text, user_id, kind, state, stripe_subscription_id, request_payload FROM billing_operations WHERE user_id = $1 AND state IN ('pending', 'submitted', 'needs_reconcile') LIMIT 1"
}

/// Parameters: op_id. Operation lookup by id (the webhook's
/// `client_reference_id` binding). UUID cast failure is a store error —
/// a verified Stripe event never carries a malformed id.
pub fn checkout_operation_by_id_select_statement() -> &'static str {
    "SELECT id::text, user_id, kind, state, stripe_subscription_id, request_payload FROM billing_operations WHERE id = $1::uuid"
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
/// session (and later the terminal mark) on the operation row.
pub fn update_checkout_operation_statement() -> &'static str {
    "UPDATE billing_operations SET state = $2, request_payload = $3::jsonb, stripe_subscription_id = $4, updated_at = now() WHERE id = $1::uuid"
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
/// coverage instead of regressing it. Status, item bindings,
/// period end and grace still update verbatim (the pure fold owns the
/// grace-set-once/clear-on-recovery rules against its snapshot).
///
/// Full parameter order:
/// $1 user_id · $2 stripe_subscription_id · $3 stripe_customer_id ·
/// $4 stripe_subscription_item_id · $5 stripe_price_id · $6 plan_id ·
/// $7 status · $8 current_period_end epoch-or-NULL ·
/// $9 cancel_at_period_end · $10 paid_through epoch-or-NULL ·
/// $11 paid_invoice_id-or-NULL · $12 grace_until epoch-or-NULL.
pub fn store_reconciled_subscription_statement() -> &'static str {
    "INSERT INTO subscriptions (user_id, stripe_subscription_id, stripe_customer_id, stripe_subscription_item_id, stripe_price_id, plan_id, status, current_period_end, cancel_at_period_end, paid_through_at, paid_invoice_id, grace_until) VALUES ($1, $2, $3, $4, $5, $6, $7, to_timestamp($8::bigint), $9, to_timestamp($10::bigint), $11, to_timestamp($12::bigint)) ON CONFLICT (stripe_subscription_id) DO UPDATE SET stripe_customer_id = EXCLUDED.stripe_customer_id, stripe_subscription_item_id = EXCLUDED.stripe_subscription_item_id, stripe_price_id = EXCLUDED.stripe_price_id, plan_id = CASE WHEN EXCLUDED.paid_through_at IS NOT NULL AND (subscriptions.paid_through_at IS NULL OR EXCLUDED.paid_through_at > subscriptions.paid_through_at) THEN EXCLUDED.plan_id ELSE subscriptions.plan_id END, status = EXCLUDED.status, current_period_end = EXCLUDED.current_period_end, cancel_at_period_end = EXCLUDED.cancel_at_period_end, paid_through_at = CASE WHEN EXCLUDED.paid_through_at IS NULL THEN subscriptions.paid_through_at WHEN subscriptions.paid_through_at IS NULL THEN EXCLUDED.paid_through_at WHEN EXCLUDED.paid_through_at > subscriptions.paid_through_at THEN EXCLUDED.paid_through_at ELSE subscriptions.paid_through_at END, paid_invoice_id = CASE WHEN EXCLUDED.paid_through_at IS NOT NULL AND (subscriptions.paid_through_at IS NULL OR EXCLUDED.paid_through_at > subscriptions.paid_through_at) THEN COALESCE(EXCLUDED.paid_invoice_id, subscriptions.paid_invoice_id) ELSE subscriptions.paid_invoice_id END, grace_until = EXCLUDED.grace_until"
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
                grace_until_unix: grace,
            }),
            bindings: bindings(),
        }
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
        // History still records each confirmed service invoice exactly once
        // (the store layer dedupes replays on the invoice id).
        assert_eq!(outcome.history.len(), 1, "only the item-plan invoice rows");
        assert_eq!(outcome.history[0].stripe_invoice_id, "in_9");
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
        for released in ["succeeded", "failed", "canceled", ""] {
            assert!(!is_single_flight_state(released), "{released} releases");
        }
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
        assert!(
            release_webhook_event_statement()
                .contains("DELETE FROM billing_webhook_events WHERE stripe_event_id = $1"),
            "a failed handling releases the claim so the retry reprocesses",
        );
    }
}
