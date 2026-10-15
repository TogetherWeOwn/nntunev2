# ADR 0005: Billing Provider and Subscription Lifecycle

- **Status:** Accepted design; implementation and independent review gates remain.
- **Date:** 2026-10-13
- **Revised:** 2026-10-15 — bounded renewal settlement, payment-conditional trial
  conversion/expiry and authoritative per-environment price mappings.

## Decision

**Stripe Billing, flat recurring subscriptions, no metered billing.** The
`usage_ledger` controls our service caps only; stream usage is never submitted
to a billed Stripe meter. Prices in ADR 0004 are proposals, not authorization
to activate live merchant processing. Legal/merchant and production gates remain.

Stripe supplies subscription lifecycle, invoices, trials and price changes.
The earlier claim that a merchant-of-record has no value is withdrawn: tax and
merchant obligations still need their own launch review. This ADR selects the
billing mechanism, not a conclusion about those obligations.

## Creation without a provisional entitlement

1. Create the user independently of Stripe. Under a per-user lock, persist a
   `billing_accounts` row and a `billing_operations` request with an immutable
   request id/payload. At most one unresolved billing operation per user is
   allowed. Pending setup grants no stream entitlement. Check the write-once
   `billing_accounts.trial_started_at` AND the `deleted_billing_tombstones`
   email-HMAC index (normalized email under the versioned billing-email
   pepper): only NULL history in both is eligible for the trial branch.
   Persist its proposed start/end in the immutable request payload.
   Existing history means a paid choice with **no** `trial_end`, `trial_period_days`
   or `trial_settings`; lack of paid consent leaves setup pending, not another
   trial. A concurrent request resumes the same operation instead of rechecking
   NULL history and issuing another create.
2. Create/reuse the Stripe Customer and Subscription outside the DB transaction,
   using stable idempotency keys derived from the operation id (distinct keys
   for customer creation and subscription creation). Persist the customer id
   before the next call. Include the local operation id as subscription metadata.
3. After Stripe returns a **real** subscription id, the API worker reconciles it
   under the same per-user lock. Insert/upsert `subscriptions` on the NOT NULL
   `stripe_subscription_id`; never insert a NULL-id `trialing` placeholder.
   The webhook can win this race: both paths reconcile the same id and row.
4. A lost response resumes the same operation, not a fresh create. Before a
   replay outside Stripe's idempotency-retention window, list/reconcile that
   customer's subscriptions using the recorded operation id. An uncertain
   outcome remains `needs_reconcile`; do not create another subscription until
   absence or the existing id is established. Persisted trial history is never
   reset by cancellation or a retry.

Both the API and webhook use one reconciliation function. Verify webhook
signatures against the raw body before any write, resolve the known customer/user
binding, deduplicate event ids, and retrieve current Stripe state rather than
letting an old event payload regress it. Per-user reconciliation is serialized;
webhooks defer while an API operation is in flight and a post-operation read
converges state. An old canceled subscription never displaces the current one.

## Trial that ends even when a card is present

Use the supported subscription-level free-trial API (`trial_end`), not the newer
Trial Offer API in the same request. The latter is a different integration;
Stripe now labels `trial_end` legacy, but still documents and supports it. Retain
this already-selected contract for this slice and pin/test the Stripe API version
in the implementing billing PR rather than silently mixing the two APIs.

Create a single-item subscription on the $0 trial price with:

```text
trial_end = persisted_trial_start + 14 days
trial_settings[end_behavior][missing_payment_method] = cancel
cancel_at_period_end = true
```

The returned item's current period end must equal `trial_end` before setup is
accepted. Do not override the billing anchor, add paid invoice items, prebill,
or attach another subscription item to a free trial. Only after this validation
succeeds, persist Stripe's returned `trial_start` as the write-once
`billing_accounts.trial_started_at` in the subscription reconciliation transaction.
A mismatching response fails setup closed, grants no access and does not record a
consumed trial. Keep its operation unresolved until the remote subscription is
confirmed canceled; do not retry creation beside it or accept a longer trial.

`missing_payment_method=cancel` alone handles **only the card-absent case**.
The scheduled end-of-period cancellation handles **both card-absent and
card-present cases**. The local entitlement predicate additionally requires
`status='trialing' AND now() < trial_end`; `active` on `plan_id='free'` never
confers access. Saving a card alone cannot select a paid plan or extend a trial.
The billing portal may collect a payment method, but must not unschedule a free
trial's cancellation or change its price outside the paid-choice flow below.

## Trial → paid and subsequent purchases

A user explicitly chooses a paid price and consents to the displayed charge.
Use `collection_method=charge_automatically` with a supported card payment method
for this flow. Collect/attach it in a setup flow before conversion; do not silently
fall back to a payment behavior that forfeits the trial.

While the trial is nonterminal, **update that subscription's existing item**:
`items[0][id]` identifies it, `items[0][price]` selects the mapped paid monthly
price, `trial_end=now`, `cancel_at_period_end=false`, `proration_behavior=none`,
and **`payment_behavior=pending_if_incomplete`**. Submit the invoice-generating
change as one pending update, not a separate early trial end/cancellation change.
The trial ends and the paid selection applies **only on successful payment**;
a decline or abandoned authentication leaves the original trial and scheduled
cancellation in effect. The UI completes the invoice PaymentIntent action flow
before claiming success. Do not use Checkout `mode=subscription` to create a
second subscription beside the trial.

Persist the pending invoice id and Stripe's `pending_update.expires_at` on the
same `billing_operations` row; keep it unresolved until confirmed paid/applied
or voided/expired. Stripe sets that deadline to the earlier trial/item period end
when within 23 hours, otherwise 23 hours after the update. Retries resume payment
of **that invoice**, never resubmit a new update to extend the deadline. No pending
conversion extends the original trial entitlement. On expiry Stripe voids the
invoice and discards the update; the hourly `billing-reconcile` job retrieves
remote state, confirms that result and resolves the operation. If expiry has
elapsed but the update/invoice remains open, it voids that still-unpaid pending
invoice and re-reads state before resolving. A payment winning that race is
reconciled as paid, never treated as a failed conversion or refunded implicitly.
Stripe failure keeps the operation unresolved with an alert/retry. Before the
original trial end the user retains free caps and can make a fresh explicit
choice after confirmed cleanup; at the original end there is no entitlement and
remote cancellation must be confirmed before a new paid subscription. Trial
history is never reset. No permanent `past_due` first-payment row is invented.

`plan_id` is the last confirmed entitlement plan (`free` until the first paid
invoice), whereas `stripe_price_id` is the **applied** Stripe item price (the
pending choice stays in the operation payload until applied). Only reconciliation
of the matching paid service invoice advances `plan_id` and `paid_through_at`.
The $0 trial invoice, a redirect, `active` by itself, or an unpaid upgrade cannot
grant paid caps. Paid upgrades use the same pending-update/payment gate, retaining
the last paid plan until settlement; downgrades apply at renewal.

### Price identity is authoritative relational data

`plan_prices` (ADR 0002) maps exact Stripe price ids to `plans.id` in each
environment's application DB. Seed four selectable mappings: the $0 monthly
trial price → `free`, and the $8/$30/$100 USD monthly licensed prices →
`plus`/`pro-5x`/`pro-20x`. The reviewed environment-specific seed verifies the
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

Once the previous subscription is confirmed Stripe-terminal, a new paid
subscription may be created with a new operation id and **no new trial**.
The purchase path first reconciles any existing nonterminal row; it updates or
resumes that subscription, or completes its authorized cancellation and confirms
terminal state before creating a replacement. It never merely sets local
`expired` while Stripe can still collect on the old subscription.

## Status, entitlement and the unique slot

Store Stripe's status verbatim. The partial UNIQUE `(user_id)` covers
`incomplete`, `trialing`, `active`, `past_due`, `unpaid`, and `paused`.
These rows reserve the slot even when they confer no entitlement. Only confirmed
`canceled` and `incomplete_expired` rows sit outside the predicate. Unknown
provider statuses fail closed and require reconciliation, not a guessed mapping.

| Stripe status | Access | Slot |
|---|---|---|
| `trialing` | Free caps only before `trial_end` | Reserved |
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
`now() < paid_through_at + interval '3 hours'`, retain the last confirmed plan,
including its cache budget. No synthetic advance of paid coverage, first-payment
allowance, sliding deadline or allowance after scheduled cancellation. This is
our bounded service policy, not a promise that Stripe always settles in 3 hours:
longer draft/webhook outages alert and cut access at the bound unless confirmed
payment or eligible renewal-failure grace is reconciled. Stream, ingest and
cache GC all use the same effective-plan predicate from ADR 0002, including this
allowance; GC must not independently zero a renewing user's budget at period end.

A failed mid-period upgrade must not erase still-paid lower-tier access. Pending
updates normally preserve the applied subscription; if a separately failed
renewal puts it in `past_due`, paid coverage remains until `paid_through_at`
without granting the unpaid higher tier.

Only the first failed **renewal after a paid period** sets `grace_until` to that
paid period's end plus seven days. Invoice retries/duplicate events do not extend
it. A failed initial paid invoice has no grace; neither does an expired free
trial. Successful recovery reconciles the paid invoice and clears grace.

### Expiry has a real executor

The API worker's scheduled **`billing-reconcile` job runs hourly**, processing
bounded batches of unresolved operations (including pending-update expiry),
expired trials, paid rows at/past `paid_through_at`, and past-grace rows. It also
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
`trial_started_at`, open invoice/debt refs; no row when there is no customer
and no history) so trial single-use and debt survive, and handle webhooks for
unknown or tombstoned customers by cancel-and-alert when a live remote
subscription remains, otherwise by reconciling the tombstone without recreating
a user. Tombstones are retained 36 months, then deleted. The account
delete-cancel is immediate (not `cancel_at_period_end`): `proration_behavior=none`,
final invoice issued, no refund in this step; every listed subscription must be
confirmed terminal before local rows are deleted.

## Acceptance contract for the implementing billing PR

- Two concurrent signup attempts, webhook-before-response and crash-after-Stripe
  creation converge to one real-id row and one remote subscription; no 23505
  collision from a provisional row. Lost results past key retention reconcile.
- Advance a Stripe test clock through day 14 with no card, and with a saved card
  but no paid choice: both end with no free entitlement. Delayed/duplicated
  events cannot resurrect it. No charge occurs just for saving a card.
- Trial-to-paid keeps the same subscription id and item id. Success applies the
  paid change; decline/abandoned authentication leaves the original trial end and
  scheduled cancellation intact, with free caps only until that end. Resume the
  same invoice on repeated clicks. Advance past `pending_update.expires_at`
  **without a webhook**: the invoice is void, the pending change disappears and
  reconciliation resolves the operation without consuming remaining trial time
  or leaving an immortal first-payment slot. Test expiry/payment races, a lost
  API response, and conversion within 23 hours of trial end. Validate these
  pending fields on the pinned Stripe API version before enabling conversion;
  unsupported behavior fails setup, never falls back to `default_incomplete`.
- Cross a paid renewal boundary: during Stripe's draft hour and delayed paid
  webhook, streaming and cache budgets stay on the last paid plan. Payment moves
  confirmed coverage forward. No payment/event by T+3h denies access exactly at
  that bound; repeated events cannot extend it. Scheduled cancellation stops at
  T with no allowance. Payment failure inside the allowance transitions to
  fixed renewal grace without an intermediate access/cache-budget loss.
- Advance beyond a renewal's grace with **no new webhook**. The local read denies
  access; hourly and inline reconciliation settle or cancel the old subscription
  before replacement. Test every status in the table and an out-of-order old id.
- Verify all four per-environment price mappings, an unknown price, wrong mode,
  retired-but-known price, unrelated invoice lines and duplicate/out-of-order
  invoices. No unknown price advances paid caps/coverage. Verify plan changes and
  idempotent daily usage independently; no metered usage report is created.

## Sources and verification boundary

Official documentation checked on 2026-10-15:

- [Subscription free trials and missing-payment behavior](https://docs.stripe.com/billing/subscriptions/trials/free-trials)
- [Update Subscription: item id, price, trial_end and payment_behavior](https://docs.stripe.com/api/subscriptions/update)
- [Pending updates: supported fields, payment failure and expiry](https://docs.stripe.com/billing/subscriptions/pending-updates)
- [Subscription renewal invoices: draft then finalize/charge](https://docs.stripe.com/billing/invoices/subscription)
- [Scheduled cancellation and undo before period end](https://docs.stripe.com/billing/subscriptions/cancel)
- [Idempotent request retention](https://docs.stripe.com/api/idempotent_requests)

This PR changes documentation only. The lifecycle/test-clock cases above are
required implementation tests, **not tests run by this ADR PR**. No live account,
payment, invoice, cancellation, migration or key rotation was exercised here.
