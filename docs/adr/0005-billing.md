# ADR 0005: Billing Provider and Subscription Lifecycle

- **Status:** Accepted design; implementation and independent review gates remain.
- **Date:** 2026-10-13
- **Revised:** 2026-10-15 — real-id creation, one nonterminal subscription,
  card-present trial cutoff, complete status mapping and scheduled reconciliation.

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
   `billing_accounts.trial_started_at`: only NULL history is eligible for the
   trial branch. Persist its proposed start/end in the immutable request payload.
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
While the trial is nonterminal, **update that subscription's existing item**:
`items[0][id]` identifies it, `items[0][price]` selects the paid monthly price,
`trial_end=now`, `cancel_at_period_end=false`, `proration_behavior=none`, and
`payment_behavior=default_incomplete`. This ends the trial immediately and
requires payment confirmation; the UI completes the invoice PaymentIntent flow
(including authentication) before claiming success. Do not use Checkout
`mode=subscription` to create a second subscription beside the trial. A setup
flow may collect a payment method without creating a subscription.

`plan_id` is the last confirmed entitlement plan (`free` until the first paid
invoice), whereas `stripe_price_id` is the currently selected Stripe price.
Only reconciliation of the matching paid service invoice advances `plan_id` and
`paid_through_at`. The $0 trial invoice, a redirect, `active` by itself, or an
unpaid upgrade cannot grant paid caps. Paid upgrades likewise retain the last
paid plan until the upgrade invoice settles; downgrades apply at renewal.

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
| `active` | Last confirmed paid plan only when `plan_id != 'free'` and `paid_through_at > now()` | Reserved |
| `past_due` | Last confirmed paid plan while `paid_through_at > now()`, or during an eligible renewal's `grace_until > now()` | Reserved |
| `incomplete`, `unpaid`, `paused` | None | Reserved; resolve/cancel existing subscription before replacement |
| `canceled`, `incomplete_expired` | None, even with an old future period end | Released |

For a paid-through end-of-period cancellation, Stripe remains `active` with
`cancel_at_period_end=true`; this is not the terminal `canceled` case.
`current_period_end` alone is not proof of payment: it can advance on an unpaid
renewal. `paid_through_at` comes from confirmed paid service invoice coverage.
A failed mid-period upgrade must not erase still-paid lower-tier access: if it
puts the subscription in `past_due`, that coverage remains until `paid_through_at`
without inventing grace or granting the unpaid higher tier.

Only the first failed **renewal after a paid period** sets `grace_until` to that
paid period's end plus seven days. Invoice retries/duplicate events do not extend
it. A failed initial paid invoice has no grace; neither does an expired free
trial. Successful recovery reconciles the paid invoice and clears grace.

### Expiry has a real executor

The API worker's scheduled **`billing-reconcile` job runs hourly**, processing
bounded batches of pending operations, expired trials, past-grace rows and
stale nonterminal subscriptions. It also runs for one user inline before any
new-subscription attempt. No Stripe webhook is expected when a local grace clock
elapses. Access is cut off by the timestamp predicate immediately, independent
of the job.

For past-grace rows, retrieve current Stripe/invoice state first. Recover a
payment that already settled; otherwise cancel the old remote subscription and
persist terminal status before releasing its slot. Stripe failure leaves the
nonterminal slot reserved and setup pending, with an alert/retry, not a duplicate
bill. Fresh authenticated purchases and cancellation reconciliation use the same
per-user operation serialization. Retain invoice/debt history; no data deletion
or implicit refund is part of expiration.

## Acceptance contract for the implementing billing PR

- Two concurrent signup attempts, webhook-before-response and crash-after-Stripe
  creation converge to one real-id row and one remote subscription; no 23505
  collision from a provisional row. Lost results past key retention reconcile.
- Advance a Stripe test clock through day 14 with no card, and with a saved card
  but no paid choice: both end with no free entitlement. Delayed/duplicated
  events cannot resurrect it. No charge occurs just for saving a card.
- Trial-to-paid keeps the same subscription id and item id. Cover paid success,
  decline, authentication required, interrupted confirmation, and repeated
  upgrade clicks. No first-payment grace or unpaid higher-tier entitlement.
- Advance beyond a renewal's grace with **no new webhook**. The local read denies
  access; hourly and inline reconciliation settle or cancel the old subscription
  before replacement. Test every status in the table and an out-of-order old id.
- Verify plan downgrade/upgrade invoice coverage and idempotent daily usage
  independently; no metered price or usage report is created.

## Sources and verification boundary

Official documentation checked on 2026-10-15:

- [Subscription free trials and missing-payment behavior](https://docs.stripe.com/billing/subscriptions/trials/free-trials)
- [Update Subscription: item id, price, trial_end and payment_behavior](https://docs.stripe.com/api/subscriptions/update)
- [Scheduled cancellation and undo before period end](https://docs.stripe.com/billing/subscriptions/cancel)
- [Idempotent request retention](https://docs.stripe.com/api/idempotent_requests)

This PR changes documentation only. The lifecycle/test-clock cases above are
required implementation tests, **not tests run by this ADR PR**. No live account,
payment, invoice, cancellation, migration or key rotation was exercised here.
