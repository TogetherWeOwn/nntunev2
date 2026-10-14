# ADR 0004: Plan Matrix (Claude Pro/Max-style)

- **Status:** Accepted
- **Date:** 2026-10-13
- **Non-blocking note:** Metering primitives (`usage_ledger`, Analytics Engine)
  are built to support these tiers from day one; exact numbers tune at launch.
- **Rev 2026-10-14 (review round 3):** every capped dimension names its
  authoritative writer (ingest/cache/concurrent included); `usage_ledger`
  rows are daily; trial is Stripe-native with a hard post-trial paywall.

## Context

Plans scale like Claude Pro/Max: a base paid tier plus 5x / 20x usage
multipliers. Dimensions: monthly stream hours, concurrent streams, library
size, ingest jobs/day, cache priority. NNTune never bundles Usenet access
(BYOP) — plans meter *our* service, not provider bandwidth.

## Decision

| Plan | Price (USD/mo) | Stream hrs/mo | Concurrent streams | Library tracks | Ingest jobs/day | Cache priority |
|---|---|---|---|---|---|---|
| Free (trial, 14d) | $0 | 10 | 1 | 1,000 | 5 | low |
| Plus | $8 | 100 | 2 | 25,000 | 25 | standard |
| Pro 5x | $30 | 500 | 4 | 100,000 | 100 | high |
| Pro 20x | $100 | 2,000 | 8 | 500,000 | 400 | highest |

- **Stream hours** meter `SUM(stream_seconds)` per user per calendar month from
  `play_events` → `usage_ledger`. Cached hits count the same (metering is
  service usage, not egress).
- **Ingest jobs/day** meter accepted jobs from the `ingest_jobs` table
  (counted in the same transaction that accepts a job) → `usage_ledger`
  daily rows. **Cache bytes** meter cache accounting → `usage_ledger`.
  **Concurrent streams** enforce from the per-user pool Durable Object
  (the authority for live stream count). Every capped dimension therefore
  has an authoritative writer — none rests on Analytics Engine.
- **Overage:** none — streams past the cap degrade gracefully (transcode to a
  lower bitrate; artwork stays full quality) until the next period. No surprise
  bills, ever.
- **Free is a 14-day Stripe-native trial**, not a forever tier: signup creates
  a `trialing` subscription on the trial plan (ADR 0002 rule 4) — the only
  trial clock. After the window with no paid subscription there is no
  entitlement (hard paywall). Trial requires a BYOP provider (no bundled
  Usenet to abuse).
- Prices are launch proposals; the `plans` table (ADR 0002) makes them data,
  not code. Changing numbers needs no deploy, no ADR.

## Consequences

- The API worker enforces caps from `usage_ledger` + `plans` on every stream
  start; enforcement reads must be Hyperdrive-cached, never cold. The
  effective plan is derived (active subscription, else the free/trial row) —
  no stored `users.plan_id` to diverge (ADR 0002 rule 4).
- `usage_ledger` (daily rows; monthly caps sum the calendar month) is the
  metering authority for cap enforcement, fed by `play_events`
  (`stream_seconds`), the `ingest_jobs` table (`ingest_jobs`), and cache
  accounting (`cache_bytes`) — see ADR 0002 rule 5. Analytics Engine events
  (`stream_start`, `ingest_job`) are analytics/observability only — sampled
  and approximate, never used for enforcement or billing.
- Overage/degrade behavior is user-visible copy owned by the frontend card.
