# NNTune web — billing slice

SvelteKit (`adapter-cloudflare`) frontend. This slice owns the user-visible
billing surface: pricing table, usage meters, upgrade prompts and the
graceful-degrade copy. Enforcement stays server-side.

## Routes

- `/` — landing with links to pricing/billing
- `/pricing` — plan matrix (ADR 0004 tiers), checkout entry
- `/account/billing` — subscription status, usage vs caps, upgrade + portal

## Library

- `src/lib/billing/plans.ts` — plan matrix mirroring the `plans` table;
  hydratable later from `GET /v1/plans` without changing components
- `src/lib/billing/usage.ts` — display-only usage evaluation (soft at 80%,
  hard at 100%; degrade copy, never billing)
- `src/lib/billing/billingClient.ts` — typed `fetch` wrappers for the
  backend contract below (session cookie, no secrets)

## Backend contract (sibling cards implement)

- `GET /v1/plans` → `{ plans: PlanDTO[] }`
- `GET /v1/me/usage` → `{ period, streamSecondsMonth, concurrentNow, libraryTracks, ingestToday }`
- `GET /v1/me/subscription` → `{ planId, status, currentPeriodEnd, trialEndsAt }`
- `POST /v1/billing/checkout { planId }` → `{ url }` (https Stripe Checkout)
- `POST /v1/billing/portal {}` → `{ url }` (https Stripe portal)

Entitlement checks read Postgres (`subscriptions` + `plans` + `usage_ledger`)
on the hot path; Stripe is reconciled async via webhooks. Webhook signature
verification and proration live backend-only.

## Commands

- `npm install` then `npm test` (vitest), `npm run check` (tsc), `npm run build`
- No secrets in this directory. Stripe keys live in Cloudflare Secrets /
  GitHub Actions secrets only.
