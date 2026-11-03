# ADR 0005: Billing Provider and Subscription Lifecycle

- **Status:** Accepted design; implementation and independent review gates remain.
- **Date:** 2026-10-13
- **Revised:** 2026-10-15 — bounded renewal settlement, payment-conditional trial
  conversion/expiry and authoritative per-environment price mappings.
- **Revised:** 2026-10-18 — slice-1 per-play precursor: stream usage accrues in
  `stream_play_ledger` (per-play windows, `play:` keys, monthly folds), never a Stripe
  meter.
- **Revised:** 2026-11-03 — owner product model (TWO-1629): the trial is retired
  (no free tier, no trial branch anywhere below) and the creation shape is
  Stripe Checkout Sessions (`mode=subscription`). A Stripe-reported
  `trialing` status is from now on an unknown status: stored verbatim, fail
  closed, not slot-reserving.

## Decision

**Stripe Billing, flat recurring subscriptions, no metered billing.** The
monthly fold over `stream_play_ledger` controls our service caps only; stream
usage is never submitted to a billed Stripe meter. Prices in ADR 0004 are
proposals, not authorization to activate live merchant processing.
Legal/merchant and production gates remain.

Stripe supplies subscription lifecycle, invoices and price changes.
The earlier claim that a merchant-of-record has no value is withdrawn: tax and
merchant obligations still need their own launch review. This ADR selects the
billing mechanism, not a conclusion about those obligations.

## Creation without a provisional entitlement (Checkout Sessions)

The first purchase is a **Stripe Checkout Session (`mode=subscription`)** for
the explicitly chosen paid price: card data stays entirely on Stripe, and the
web client receives `{url}`. There is no trial branch: a Checkout Session is
created only for a mapped paid price, with **no** `trial_end`,
`trial_period_days` or `trial_settings`.

1. Create the user independently of Stripe. Under a per-user lock, persist a
   `billing_accounts` row and a `billing_operations` request with an immutable
   request id/payload (the chosen plan/price). At most one unresolved billing
   operation per user is allowed. Pending setup grants no stream entitlement.
   A concurrent request resumes the same operation instead of issuing another
   create.
2. Create/reuse the Stripe Customer and create the Checkout Session outside
   the DB transaction, using stable idempotency keys derived from the
   operation id (distinct keys for customer creation and session creation).
   Persist the customer id before the next call. Include the local operation
   id in the session/subscription metadata. **Single-flight means reuse the
   open session**: while the operation's session is still open, return its
   URL rather than creating another session; an expired/completed session is
   reconciled first, never silently replaced beside a live subscription.
3. Entitlement requires a **real** Stripe subscription id. Reconcile from
   `checkout.session.completed` plus subscription retrieval: after Stripe
   returns a real subscription id, the API worker reconciles it under the
   same per-user lock. Insert/upsert `subscriptions` on the NOT NULL
   `stripe_subscription_id`; never insert a NULL-id placeholder. The webhook
   can win this race: both paths reconcile the same id and row through the
   one reconciliation function shared by webhook and API paths.
   `paid_through_at` advances only from confirmed paid service invoices
   resolved through `plan_prices` — the Checkout redirect itself, or an
   `active` status alone, grants nothing.
4. A lost response resumes the same operation, not a fresh create. Before a
   replay outside Stripe's idempotency-retention window, list/reconcile that
   customer's subscriptions and sessions using the recorded operation id. An
   uncertain outcome remains `needs_reconcile`; do not create another
   subscription until absence or the existing id is established.

Both the API and webhook use one reconciliation function. Verify webhook
signatures against the raw body before any write, resolve the known customer/user
binding, deduplicate event ids, and retrieve current Stripe state rather than
letting an old event payload regress it. Per-user reconciliation is serialized;
webhooks defer while an API operation is in flight and a post-operation read
converges state. An old canceled subscription never displaces the current one.

## Plan changes and subsequent purchases

A user explicitly chooses a paid price and consents to the displayed charge.
Use `collection_method=charge_automatically` with a supported card payment method
for this flow.

**Upgrade:** while a paid subscription is nonterminal, **update that
subscription's existing item**: `items[0][id]` identifies it,
`items[0][price]` selects the mapped paid monthly price,
`cancel_at_period_end=false`, `proration_behavior=none`, and
**`payment_behavior=pending_if_incomplete`**. Submit the invoice-generating
change as one pending update. The paid selection applies **only on successful
payment**; a decline or abandoned authentication leaves the last confirmed
plan in effect. The UI completes the invoice PaymentIntent action flow before
claiming success. Never create a second subscription (Checkout or otherwise)
beside a live one.

Persist the pending invoice id and Stripe's `pending_update.expires_at` on the
same `billing_operations` row; keep it unresolved until confirmed paid/applied
or voided/expired. Stripe sets that deadline to the earlier item period end
when within 23 hours, otherwise 23 hours after the update. Retries resume payment
of **that invoice**, never resubmit a new update to extend the deadline. No pending
update extends the last confirmed entitlement. On expiry Stripe voids the
invoice and discards the update; the hourly `billing-reconcile` job retrieves
remote state, confirms that result and resolves the operation. If expiry has
elapsed but the update/invoice remains open, it voids that still-unpaid pending
invoice and re-reads state before resolving. A payment winning that race is
reconciled as paid, never treated as a failed change or refunded implicitly.
Stripe failure keeps the operation unresolved with an alert/retry. After
confirmed cleanup the user can make a fresh explicit choice. No permanent
`past_due` first-payment row is invented.

**Downgrade** applies at renewal; if month-to-date usage already exceeds the
new cap when it takes effect, playback stops until the UTC reset (usage is
never forgiven or reset).

`plan_id` is the last confirmed entitlement plan (`free` until the first paid
invoice), whereas `stripe_price_id` is the **applied** Stripe item price (the
pending choice stays in the operation payload until applied). Only reconciliation
of the matching paid service invoice advances `plan_id` and `paid_through_at`.
A Checkout redirect, `active` by itself, or an unpaid upgrade cannot
grant paid caps. Paid upgrades use the pending-update/payment gate above,
retaining the last paid plan until settlement.

### Price identity is authoritative relational data

`plan_prices` (ADR 0002) maps exact Stripe price ids to `plans.id` in each
environment's application DB. Seed three selectable mappings: the
$8/$15/$25 USD monthly prices → `plus`/`pro`/`max`. The reserved `free` row
has no price mapping and is never selectable. The reviewed environment-specific seed verifies the
configured Stripe account, `livemode`, currency, amount, interval and usage type;
it does not invent ids or copy test ids into live. A missing mapping disables
that purchase option. KV, a client parameter and Stripe metadata are not mapping
authority. Keep historical mappings immutable and nonselectable after a price
replacement so old invoices/subscriptions still reconcile; never repoint a price
to a different plan. At most one selectable price per plan is allowed, matching
the advertised `plans.price_cents`.

Resolve paid service invoice lines through that table, bound to the known
customer, subscription and `stripe_subscription_item_id`. Require the applied
or recorded pending operation's price and corresponding service period; unrelated
one-off/credit lines and the free price confer no paid caps. Read the full invoice,
not only its first line. Advance coverage monotonically under the per-user lock;
duplicated/older invoices cannot regress a later confirmed plan/period. An unknown
price or wrong mode/account fails closed: record observed remote state, do **not**
advance `plan_id`/`paid_through_at`, disable new purchases for it and alert for
reconciliation. Existing previously confirmed coverage/allowances retain only
their original deadlines; an unknown price grants no new or higher paid caps.

### Reconcile folds and purchase-flight replacement

A reconcile pass folding an older remote snapshot must not regress a fresher
stored pass: coverage (`paid_through_at`, and the `plan_id`/`paid_invoice_id`
tied to it) advances monotonically, and the same staleness predicate — the new
evidence ends strictly before the stored paid end, or ties it while carrying
no anchor against an anchored stored row — guards stored `status` and
`grace_until` instead of writing stale verbatim fields over a fresher pass.
Staleness is anchor containment, not an end comparison: a snapshot that does
not contain the invoice anchoring the stored coverage folded an older remote
state, however its item-plan money compares (a snapshot with no stored anchor
to check against falls back to the end comparison, and a snapshot with no
item-plan money at all is not stale on that basis alone — a first failed
renewal observes `past_due` with no new invoice, and that observation still
sets the renewal grace). The stale fold carries its evidence end into the
write (not the stored end), so the predicate still fires when the
stale-reading pass commits second — falling back to the stored end, anchor
cleared, when its snapshot holds no item-plan money at all. Equal-money
status order across fresh passes is unknowable, so those still fold verbatim
and converge on the next hourly reconcile. When an equal paid end carries a
new plan (a plan switch landing exactly on the old period end), the fold
relabels to the subscription item's current plan with unchanged coverage,
anchored on the new evidence — but only onto equally-or-more-recently paid
evidence (payment order as observed in the snapshot) — and the write persists
that relabel only when the emitted anchor differs from the anchor the pass
read, so a keep that read before the relabel committed cannot reverse the
newer plan/anchor when it commits second (an equal-end pass carrying such
new-evidence anchor adopts `plan_id` / `paid_invoice_id`), and the switch
cannot stick on the old plan forever.

An existing open Checkout Session — or a single-flight loser converging onto
one — is reused only when its recorded price matches the newly requested
price. A second attempt naming a different plan never inherits the first
attempt's session: the open session is expired first, the flight is
superseded, and a fresh operation mints under a fresh Stripe idempotency key.
A session-less flight (never created, or its mark was lost) with a mismatched
price is resolved under its ORIGINAL operation id and recorded price before
anything supersedes it — the same-key replay recovers the session (never
duplicates), which then retires per its retrieved state — because a missing
mark does not mean Stripe has no session, and superseding blind would orphan
a payable one. Repairing a confirmed-expired or Stripe-404-missing session
replaces the same way (no expiry call: Stripe already converged the state),
so a repair can never replay a dead session inside Stripe's retention window.
Only `expired` deliberately replaces among the closed states: a `complete`
session that is not paid (`unpaid` async processing, `no_payment_required`
subscription already created) can still succeed, so the flight stays open for
the real completion and the retry fails retryable without minting a second
checkout beside it. An unconfirmed retrieve (any non-404 failure) fails
retryable instead of minting beside unknown state.

Paid-invoice history keeps one row per confirmed paid invoice under its own
lines' plan — including invoices from an earlier plan after a plan change —
idempotent on the invoice id; coverage still follows only the current item's
plan.

Once the previous subscription is confirmed Stripe-terminal, a new paid
subscription may be created with a new operation id (a fresh Checkout
Session).
The purchase path first reconciles any existing nonterminal row; it updates or
resumes that subscription, or completes its authorized cancellation and confirms
terminal state before creating a replacement. It never merely sets local
`expired` while Stripe can still collect on the old subscription.

## Status, entitlement and the unique slot

Store Stripe's status verbatim. The partial UNIQUE `(user_id)` covers
`incomplete`, `active`, `past_due`, `unpaid`, and `paused`.
These rows reserve the slot even when they confer no entitlement. Only confirmed
`canceled` and `incomplete_expired` rows sit outside the predicate. Unknown
provider statuses — including a Stripe-reported `trialing`, which no flow
here can produce — are stored verbatim, fail closed, do not reserve the slot,
and alert in reconciliation rather than getting a guessed mapping.

| Stripe status | Access | Slot |
|---|---|---|
| `active` | Last confirmed paid plan (`plan_id != 'free'`) while `paid_through_at > now()`, or for the fixed 3-hour renewal-settlement allowance below | Reserved |
| `past_due` | Last confirmed paid plan while `paid_through_at > now()`, or during an eligible renewal's `grace_until > now()` | Reserved |
| `incomplete`, `unpaid`, `paused` | None | Reserved; resolve/cancel existing subscription before replacement |
| `canceled`, `incomplete_expired` | None, even with an old future period end | Released |

For a paid-through end-of-period cancellation, Stripe remains `active` with
`cancel_at_period_end=true`; this is not the terminal `canceled` case.
`current_period_end` alone is not proof of payment: it can advance on an unpaid
renewal. `paid_through_at` comes from confirmed paid service invoice coverage.
Stripe normally keeps a renewal invoice in draft for about one hour before
charging. To avoid a recurring access gap, `RENEWAL_SETTLEMENT_ALLOWANCE` is
**3 hours from the last confirmed `paid_through_at`**. For `active` paid rows
with `cancel_at_period_end=false`, non-NULL paid coverage and
`now() < paid_through_at + interval '3 hours'`, retain the last confirmed plan.
No synthetic advance of paid coverage, first-payment
allowance, sliding deadline or allowance after scheduled cancellation. This is
our bounded service policy, not a promise that Stripe always settles in 3 hours:
longer draft/webhook outages alert and cut access at the bound unless confirmed
payment or eligible renewal-failure grace is reconciled. The playback gate
uses the same effective-plan predicate from ADR 0002, including this
allowance.

A failed mid-period upgrade must not erase still-paid lower-tier access. Pending
updates normally preserve the applied subscription; if a separately failed
renewal puts it in `past_due`, paid coverage remains until `paid_through_at`
without granting the unpaid higher tier.

Only the first failed **renewal after a paid period** sets `grace_until` to that
paid period's end plus seven days. Invoice retries/duplicate events do not extend
it. A failed initial paid invoice has no grace. Successful recovery
reconciles the paid invoice and clears grace.

### Expiry has a real executor

The API worker's scheduled **`billing-reconcile` job runs hourly**, processing
bounded batches of unresolved operations (including pending-update expiry),
paid rows at/past `paid_through_at`, and past-grace rows. It also
runs for one user inline before any new-subscription attempt. Poll pending
operations every run; do not require a webhook or a non-NULL `grace_until` to
clean up an abandoned first payment. Read remote pending-update/expiry state if
a crash left its local invoice id/deadline unset. No Stripe webhook is expected
when a local grace clock elapses. Access is cut off by the full effective-plan
timestamp predicate, including the bounded settlement allowance, independently
of the job.

For past-grace rows, retrieve current Stripe/invoice state first. Recover a
payment that already settled; otherwise cancel the old remote subscription and
persist terminal status before releasing its slot. Stripe failure leaves the
nonterminal slot reserved and setup pending, with an alert/retry, not a duplicate
bill. Fresh authenticated purchases and cancellation reconciliation use the same
per-user operation serialization. Retain invoice/debt history; no data deletion
or implicit refund is part of expiration. Account deletion follows the same gate
(ADR 0002 rule 3): confirm remote terminal status through serialized reconcile
before deleting local billing rows, retain the Stripe customer remotely, copy a
tombstone (`stripe_customer_id` when non-NULL, email HMAC per ADR 0002,
open invoice/debt refs; no row when there is no customer
and no history) so invoice/debt history survives, and handle webhooks for
unknown or tombstoned customers by cancel-and-alert when a live remote
subscription remains, otherwise by reconciling the tombstone without recreating
a user. Tombstones are retained 36 months for invoice/debt reconciliation,
then deleted. The account
delete-cancel is immediate (not `cancel_at_period_end`): `DELETE
/v1/subscriptions/{id}` with `prorate=false`, `invoice_now=true` (the final
invoice), and `cancellation_details[comment]=account_deletion` as the
deletion marker (`comment` is a request field on Cancel a subscription;
`cancellation_details[reason]` is response-only and MUST NOT be sent —
Stripe rejects unrecognised parameters with 400); no refund in
this step (`proration_behavior=none` belongs only to the update call at the
pinned API version). Cancelling stops automatic collection of open finalized
invoices, so they are left uncollected and their refs recorded in the
tombstone's `open_invoice_refs`/`debt_refs`; every listed subscription must be
confirmed terminal before local rows are deleted. The implementing PR asserts
these exact request parameters by sending the pinned request to stripe-mock
with OpenAPI validation (or Stripe test mode) and asserting it is accepted,
not only that the parameter names match this text.

## Acceptance contract for the implementing billing PR

- Two concurrent purchase attempts, webhook-before-response and crash-after-Stripe
  creation converge to one real-id row and one remote subscription; no 23505
  collision from a provisional row. Lost results past key retention reconcile.
- First purchase via Checkout: an abandoned or expired session grants nothing;
  a completed session reconciles from `checkout.session.completed` plus
  subscription retrieval, and entitlement starts only at confirmed paid
  invoice coverage. Repeated clicks while a session is open reuse that
  session's URL; no second subscription is created beside a live one.
  Delayed/duplicated events cannot resurrect a lapsed entitlement. No charge
  occurs just for saving a card.
- Upgrade keeps the same subscription id and item id. Success applies the
  paid change; decline/abandoned authentication leaves the last confirmed
  plan intact. Resume the same invoice on repeated clicks. Advance past
  `pending_update.expires_at`
  **without a webhook**: the invoice is void, the pending change disappears and
  reconciliation resolves the operation without leaving an immortal
  first-payment slot. Test expiry/payment races and a lost
  API response. Validate these
  pending fields on the pinned Stripe API version before enabling upgrades;
  unsupported behavior fails setup, never falls back to `default_incomplete`.
- Cross a paid renewal boundary: during Stripe's draft hour and delayed paid
  webhook, streaming stays on the last paid plan. Payment moves
  confirmed coverage forward. No payment/event by T+3h denies access exactly at
  that bound; repeated events cannot extend it. Scheduled cancellation stops at
  T with no allowance. Payment failure inside the allowance transitions to
  fixed renewal grace without an intermediate access loss.
- Advance beyond a renewal's grace with **no new webhook**. The local read denies
  access; hourly and inline reconciliation settle or cancel the old subscription
  before replacement. Test every status in the table and an out-of-order old id.
- Verify all three per-environment price mappings, an unknown price, wrong mode,
  retired-but-known price, unrelated invoice lines and duplicate/out-of-order
  invoices. No unknown price advances paid caps/coverage. Verify plan changes and
  idempotent monthly usage independently; no metered usage report is created.

## Sources and verification boundary

Official documentation checked on 2026-10-15 (Checkout Sessions adoption
rechecked 2026-11-03):

- [Checkout Sessions for subscriptions](https://docs.stripe.com/billing/subscriptions/build-subscriptions)
- [Update Subscription: item id, price and payment_behavior](https://docs.stripe.com/api/subscriptions/update)
- [Pending updates: supported fields, payment failure and expiry](https://docs.stripe.com/billing/subscriptions/pending-updates)
- [Subscription renewal invoices: draft then finalize/charge](https://docs.stripe.com/billing/invoices/subscription)
- [Scheduled cancellation and undo before period end](https://docs.stripe.com/billing/subscriptions/cancel)
- [Idempotent request retention](https://docs.stripe.com/api/idempotent_requests)

This PR changes documentation only. The lifecycle/test-clock cases above are
required implementation tests, **not tests run by this ADR PR**. No live account,
payment, invoice, cancellation, migration or key rotation was exercised here.
