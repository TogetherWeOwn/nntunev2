# ADR 0004: Plan Matrix (Plus / Pro / Max)

- **Status:** Accepted design; implementation and independent review gates remain.
- **Date:** 2026-10-13
- **Revised:** 2026-11-03 — owner product model (TWO-1629): no trial, no free tier,
  stream-hour caps with hard stop, 1 concurrent stream; ingest/library/cache
  plan dimensions removed.
- **Revised:** 2026-10-18 — slice-1 per-play precursor: stream usage accrues in
  `stream_play_ledger` (one row per billed play window, deterministic `play:` keys,
  monthly folds).
- **Revised:** 2026-10-18 — enforcement wire: refusals carry `x-plan-limit` /
  `x-plan-reset` headers (`retry-after` on 429) and an `upgrade_url: /pricing`
  body pointer. Pure decision in `workers/api/src/plan_limits.rs`; DB wiring
  (subscriptions + usage reads) follows in a later slice.

## Context

NNTune is an invite-only curated collection: playback is paywalled, and the
only differentiator between plans is stream hours per month. Every plan gets
the full catalogue, FLAC playback, YouTube fallback + music video, and
1 concurrent stream. Prices are launch proposals for infrastructure cost
recovery, not a claim of revenue or permission to activate live billing.

## Decision

| Plan | USD/mo | Stream hrs/mo | Concurrent streams |
|---|---|---|---|
| Free (reserved, non-selectable) | $0 | 0 | 1 |
| Plus | $8 | 100 | 1 |
| Pro | $15 | 300 | 1 |
| Max | $25 | 750 | 1 |

Three paid plans. No free tier, no trial. The `free` plan id is retained as
a reserved, non-selectable row (0 h) so "unknown plan fails closed" logic and
a future free tier have somewhere to land; it confers no playback. 750 h is
the physical ceiling for 1 concurrent stream (31 days × 24 h = 744 h); Max
may be marketed as "unlimited listening" but the stored cap stays 750 so the
meter, abuse detection, and cost model keep a real bound, and copy must not
promise "unlimited" without the 750 h figure available on the pricing page.

Dropped plan dimensions: library-track cap, ingest-jobs/day cap, per-user
cache budget/retention/priority (superseded by the shared popularity cache).
Library size and playlist counts are uncapped at launch (metadata only;
revisit only on measured abuse). User ingest keeps a fixed, non-plan daily
rate-limit constant until the user-ingest retirement removes that path; it is
never silently uncapped by a billing change.

Monthly billing only at launch. Prices are **proposals only**: before any
live activation, a `Money:` owner card must carry a per-hour cost estimate
and confirm prices there. Nothing here authorises live billing.

### Measurement and enforcement

| Dimension | Authority and writer | Aggregation / enforcement |
|---|---|---|
| Stream hours | Stream worker play windows in `stream_play_ledger` (deterministic `play:` keys; seconds from delivered bytes at the track's mean byte rate) | Fold of play windows for the UTC `period_month`; cached plays count equally; YouTube-sourced playback excluded; duplicate delivery adds zero |
| Concurrent streams | Per-user lease at grant issuance in the API worker | Enforce at acquisition (1 per user); release on completion/disconnect and expire abandoned leases |

Monthly usage for enforcement/display = the indexed `SUM(user, period_month)`
over `stream_play_ledger` — the single authority. There is no daily fold
table. Analytics Engine is sampled observability, never an enforcement or
billing authority.

**Overage:** no usage-based bill. When monthly usage reaches the cap,
starting any new playback is refused `402 plan_limit_exceeded` (hard stop,
no degraded mode); a track already playing plays to its end, and seeking
within it stays allowed. Upgrading mid-month lifts the stop as soon as the
higher plan is payment-confirmed; usage carries over and never resets on a
plan change. A downgrade applies at renewal; if month-to-date usage already
exceeds the new cap when it takes effect, playback stops until the UTC reset.

**Enforcement wire:** refusals are the subscription gate (no active paid
coverage → paywall), the hour cap (402 above), and concurrency
(`429 rate_limited`, `retryable: true` plus `Retry-After`). Every refusal
carries `x-plan-limit` / `x-plan-reset` headers and an
`upgrade_url: /pricing` body pointer, names-only with no PII, and never a
500. Checks are `>=` (at-cap denies); unknown plan ids fail closed to the
reserved 0-hour `free` row (no playback).

### Payment gate

There is no trial and no free tier. Browsing the catalogue, search,
library/playlist management, and account pages work without an active
subscription; only **playback** is paywalled. The first purchase goes through
Stripe Checkout Sessions (`mode=subscription`); ADR 0005 is the normative
lifecycle: a real Stripe subscription id precedes entitlement, and paid caps
require a price resolved through the environment's relational `plan_prices`
mapping plus confirmed paid invoice coverage — not a redirect. The gate is
evaluated server-side at signed-grant issuance in the API worker.

- Active paid renewals retain the last confirmed plan for the fixed
  **3-hour settlement allowance** in ADR 0002, excluding scheduled
  cancellation. Only failed **renewals of an already paid subscription** get
  seven days of grace. No first-payment allowance or paid access. Neither
  deadline slides on retries or permits a second billable subscription.
- Cancellation is `cancel_at_period_end` self-serve via the billing portal;
  access runs until `paid_through_at`, then lapses. Resubscription reuses the
  confirmed-terminal-then-create path (ADR 0005).

Prices and quotas are plan data, not code. Changes to advertised prices still
need the normal product/billing review; data-driven configuration is not
permission to change existing customer commitments.

## Consequences and implementation alignment

- Cap and entitlement decisions read fresh authoritative state via Hyperdrive;
  connection pooling is not permission to serve stale revocations or counters.
  The effective plan is derived, never copied to `users.plan_id` (ADR 0002).
  The API gate reads the `plans` row (a missing row fails closed to the
  reserved 0-hour `free`); the `PlanId` enum caps remain as the parity-test
  mirror of this table.
- Frontend billing displays the same values and the payment states.
- Acceptance must cover the plan table parity (ADR ↔ seed ↔ enum caps), the
  at-cap hard stop, unknown-plan fail-closed, a month boundary, duplicate
  queue delivery, and every payment case in ADR 0005 (see TWO-1632 §10).
