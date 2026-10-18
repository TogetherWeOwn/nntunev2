# ADR 0004: Plan Matrix (Base / 5x / 20x)

- **Status:** Accepted design; implementation and independent review gates remain.
- **Date:** 2026-10-13
- **Revised:** 2026-10-15 — concrete cache budgets, atomic stream accounting,
  payment-conditional trial conversion and bounded renewal settlement.
- **Revised:** 2026-10-18 — slice-1 per-play precursor: stream usage accrues in
  `stream_play_ledger` (one row per billed play window, deterministic `play:` keys,
  monthly folds). The daily `usage_ledger` aggregation (sum daily rows in the UTC
  calendar month) is built in a later slice by folding these windows; no daily table
  exists yet and no writer assumes its `(user_id, period)` upsert.
- **Revised:** 2026-10-18 — enforcement wire: over-cap
  ingest-write/stream acquisition refuses `402 plan_limit_exceeded` (monthly,
  daily, library) or `429 rate_limited` (concurrent) with `x-plan-limit` /
  `x-plan-reset` headers (`retry-after` on 429) and an `upgrade_url: /pricing`
  body pointer. Pure decision in `workers/api/src/plan_limits.rs`; DB wiring
  (subscriptions + usage reads) follows in a later slice.

## Context

A base paid tier and 5x / 20x stream allowances cover different usage levels.
NNTune never bundles Usenet access (BYOP): plans cover our service, not a
provider account. Prices are launch proposals for infrastructure cost recovery,
not a claim of revenue or permission to activate live billing.

## Decision

| Plan | USD/mo | Stream hrs/mo | Concurrent streams | Library tracks | Ingest jobs/day | Cache budget (GiB) | Cache retention (days) | Cache priority |
|---|---|---|---|---|---|---|---|---|
| Free (trial, 14d) | $0 | 10 | 1 | 1,000 | 5 | 2 | 7 | low |
| Plus | $8 | 100 | 2 | 25,000 | 25 | 20 | 30 | standard |
| Pro 5x | $30 | 500 | 4 | 100,000 | 100 | 100 | 90 | high |
| Pro 20x | $100 | 2,000 | 8 | 500,000 | 400 | 400 | 90 | highest |

Store `cache_bytes_budget` as an integer number of bytes: GiB means
1,073,741,824 bytes, so the four budgets are 2,147,483,648; 21,474,836,480;
107,374,182,400; and 429,496,729,600. `cache_retention_days` and
`cache_priority` are separate plan columns. These are reclaimable server-side
cache allowances, not durable storage promises.

### Measurement and enforcement

| Dimension | Authority and writer | Aggregation / enforcement |
|---|---|---|
| Stream hours | Stream worker inserts immutable `(event_id, played_at)` events and increments `usage_ledger.stream_seconds` from inserted rows in the **same transaction** (ADR 0002) | Sum daily rows in the UTC calendar month; cached plays count equally; duplicate delivery adds zero |
| Ingest jobs | API accept transaction inserts `ingest_jobs` and increments `usage_ledger.ingest_jobs` once per idempotency key | Current UTC day's row; queue retries do not count as new jobs |
| Cache bytes | Live join of `user_cache_grants` with `cache_objects.byte_size`; stream publication / GC change the grants | Gauge, never a monthly sum: count each hash once per user; compare with `plans.cache_bytes_budget` |
| Library tracks | `library_tracks` written at ingest `write` stage and on library edits | Count distinct tracks across that user's libraries |
| Concurrent streams | Per-user pool Durable Object owns active leases | Enforce at acquisition; release on completion/disconnect and expire abandoned leases |

`usage_ledger` stores **flows only** (stream seconds and accepted job counts).
It has no `cache_bytes` field. Analytics Engine is sampled observability, never
an enforcement or billing authority. A shared object counts in each granted
user's gauge, but only once for that user even if several tracks reference it.
ADR 0003 defines eviction and the grant deletion order.

**Overage:** no usage-based bill. Stream-hour exhaustion uses the advertised
lower-bitrate degraded mode until the next calendar month; it does not bypass
concurrency, trial expiry or the payment gate. Cache pressure evicts least-recently
served grants within the plan budget; tracks and refetch sources remain. Library
and daily ingest limits reject new additions/jobs without deleting existing data.
A downgrade preserves library metadata, blocks additions over the new limit, and
makes excess cache grants eligible for eviction.

**Enforcement wire (2026-10-18):** at the API boundary, new acquisition over
cap is refused rather than degraded: monthly stream seconds, daily ingest jobs
and library tracks refuse with `402 plan_limit_exceeded` (`retryable: false`);
concurrent-stream exhaustion refuses with `429 rate_limited` (`retryable: true`
plus `Retry-After`). Every refusal carries `x-plan-limit` / `x-plan-reset`
headers and an `upgrade_url: /pricing` body pointer, names-only with no PII,
and never a 500. The lower-bitrate degraded mode above describes the
sustained quality of playback already admitted when the cap is reached —
not a right to start new playback, which the over-limit e2e legs assert is
refused with 402 until the next calendar month. Checks are `>=` (at-cap
denies); unknown plan ids fail closed to the free caps.

### Trial and payment gate

Free is a **single 14-day Stripe-native trial**, not a forever tier. ADR 0005 is
the normative lifecycle: a real Stripe subscription id precedes entitlement;
there is no provisional `trialing` DB row with a NULL Stripe id.

- The trial subscription uses the $0 trial price, `trial_end`,
  `trial_settings.end_behavior.missing_payment_method = cancel`, and
  `cancel_at_period_end = true`. Its single item's period end must equal
  `trial_end`. The scheduled cancellation applies **with or without a card**;
  saving a payment method is not selecting a paid plan.
- Trial caps apply only while `status='trialing' AND now() < trial_end`.
  `active` on the free price is never entitled. A late webhook therefore
  cannot extend free access. After expiry the result is a hard paywall.
- A paid choice submits a `pending_if_incomplete` update of the **same** Stripe
  subscription/item, ending the trial only on successful payment. Unpaid or
  abandoned confirmation preserves the original trial deadline/cancellation;
  the pending invoice expires within Stripe's bounded window (ADR 0005). It
  never creates a competing subscription. Paid caps require consent, a price
  resolved through the environment's relational `plan_prices` mapping, and
  confirmed paid invoice coverage, not a redirect or the $0 trial invoice.
- Active paid renewals retain the last confirmed plan/cache budget for the fixed
  **3-hour settlement allowance** in ADR 0002, excluding scheduled cancellation.
  Only failed **renewals of an already paid subscription** get seven days of
  grace. No first-payment allowance or paid access. Neither deadline slides on
  retries or permits a second billable subscription.
- Trial requires a BYOP provider. Resubscription does not reset trial history.

Prices and quotas are plan data, not code. Changes to advertised prices still
need the normal product/billing review; data-driven configuration is not
permission to change existing customer commitments.

## Consequences and implementation alignment

- Cap and entitlement decisions read fresh authoritative state via Hyperdrive;
  connection pooling is not permission to serve stale revocations or counters.
  The effective plan is derived, never copied to `users.plan_id` (ADR 0002).
- Ingest is already merged: its Analytics Engine-only metering needs an additive
  implementation follow-up for the idempotent `ingest_jobs` accept transaction
  and daily ledger. This document does not claim that writer exists today.
- Stream/cache follow-up implements the live gauge, retention and byte budgets;
  frontend billing displays the same values and the trial/payment states.
- Acceptance must cover two users sharing a hash, multiple mappings for one
  user's hash, downgrade eviction, a month boundary (no sum of cache snapshots),
  duplicate queue delivery, and every trial/payment case in ADR 0005.
