# ADR 0005: Billing Provider (Stripe Billing)

- **Status:** Accepted
- **Date:** 2026-10-13

## Context

Plans (ADR 0004) need subscriptions, trials, upgrades/downgrades, and usage
records. Candidates: Stripe Billing, Paddle, Lemon Squeezy, homegrown.

## Decision

**Stripe Billing with usage-based metering.** Subscriptions map 1:1 to
`subscriptions` rows (ADR 0002); `stripe_customer_id` /
`stripe_subscription_id` are the join keys. Monthly stream usage is reported as
Stripe usage records from the `usage_ledger` rollup (idempotent per
`user_id + period`).

### Why Stripe (no reason not to)

- Native trial → paid conversion, proration on 5x/20x upgrades, and
  usage-based line items out of the box — exactly our plan shape.
- Webhook contract (`customer.subscription.*`, `invoice.*`) is well-understood;
  the API worker reconciles webhook state into `subscriptions.status`.
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
