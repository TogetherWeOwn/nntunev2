# ADR 0005: Billing Provider (Stripe Billing)

- **Status:** Accepted
- **Date:** 2026-10-13
- **Rev 2026-10-14 (review round 4, 4/5 → 5/5):** trial pinned to
  `trial_settings.missing_payment_method = cancel`; webhook contract
  extended (`customer.subscription.created/updated/deleted`,
  `trial_will_end`, `invoice.*`); provisional signup row + reconciler
  upsert; no persisting $0 subscription; 7-day `past_due` grace via
  `grace_until`.

## Context

Plans (ADR 0004) need subscriptions, trials, upgrades/downgrades, and usage
records. Candidates: Stripe Billing, Paddle, Lemon Squeezy, homegrown.

## Decision

**Stripe Billing with flat-rate subscriptions — no metered billing.**
Subscriptions map 1:1 to `subscriptions` rows (ADR 0002);
`stripe_customer_id` / `stripe_subscription_id` are the join keys. Stream
usage stays internal (`usage_ledger` drives cap enforcement only); nothing
usage-based is reported to Stripe, so plan caps can never produce a bill.

### Why Stripe (no reason not to)

- Native trial → paid conversion, flat recurring prices per plan, and
  proration on 5x/20x upgrades out of the box — exactly our plan shape.
- Webhook contract (`customer.subscription.created/updated/deleted`,
  `customer.subscription.trial_will_end`, `invoice.*`) is well-understood;
  the API worker reconciler upserts `subscriptions` on
  `stripe_subscription_id` (authoritative updater) and sets `grace_until`
  (7d from entering `past_due`), `cancel_at_period_end`, and `trial_end`.
  Trial creation pins `trial_settings[missing_payment_method] = cancel` —
  never the `create_invoice` default: a trialing subscription on the $0
  trial plan must not survive as an `active` $0 row (that would be the
  forever tier ADR 0004 rules out), and a trial on a paid price must not
  linger in `past_due`-inside-grace past the window. Signup writes the
  provisional `trialing` row with the user (ADR 0002 rule 4) so entitlement
  exists before `customer.subscription.created` arrives.
  `customer.subscription.deleted` (and dunning-exhausted cancels) move the
  local row to terminal `canceled`; a `past_due` row past `grace_until`
  with no Stripe successor moves to local-terminal `expired` to free the
  partial-unique slot for the replacement subscription.
- No merchant-of-record need (we sell direct SaaS, not a marketplace), so
  Paddle's MoR value doesn't apply; Lemon Squeezy adds nothing over Stripe
  here. Homegrown billing is a liability, not a feature.

## Consequences

- Stripe secret keys live only in Cloudflare Secrets / GitHub Actions secrets —
  never in repo (PUBLIC repo; gitleaks enforced).
- Webhook endpoint verifies signatures with the raw body; unverified events
  are dropped before any DB write.
- Entitlement checks read Postgres (`subscriptions` + `plans`), never Stripe,
  on the hot path; Stripe is reconciled async.
- Test mode covers the full trial → upgrade → downgrade matrix in CI with
  Stripe test clocks.
- No metered Stripe prices exist for stream usage. If informational usage
  reporting is ever wanted, it must point at a $0 meter behind a new ADR —
  never at a billed meter (ADR 0004 promises no overage bills).
