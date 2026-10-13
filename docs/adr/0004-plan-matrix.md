# ADR 0004: Plan Matrix (Claude Pro/Max-style)

- **Status:** Accepted
- **Date:** 2026-10-13
- **Non-blocking note:** Metering primitives (`usage_ledger`, Analytics Engine)
  are built to support these tiers from day one; exact numbers tune at launch.

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
- **Overage:** none — streams past the cap degrade gracefully (transcode to a
  lower bitrate; artwork stays full quality) until the next period. No surprise
  bills, ever.
- **Free is a 14-day trial**, not a forever tier: it exists so signup → first
  stream converts, then paywalls. Trial requires a BYOP provider (no bundled
  Usenet to abuse).
- Prices are launch proposals; the `plans` table (ADR 0002) makes them data,
  not code. Changing numbers needs no deploy, no ADR.

## Consequences

- The API worker enforces caps from `usage_ledger` + `plans` on every stream
  start; enforcement reads must be Hyperdrive-cached, never cold. The
  effective plan is derived (active subscription, else the free/trial row) —
  no stored `users.plan_id` to diverge (ADR 0002 rule 4).
- `play_events` → `usage_ledger` is the metering authority for cap
  enforcement. Analytics Engine events (`stream_start`, `ingest_job`) are
  analytics/observability only — sampled and approximate, never used for
  enforcement or billing.
- Overage/degrade behavior is user-visible copy owned by the frontend card.
