# NNTune web app

SvelteKit frontend for NNTune v2, deployed serverless on Cloudflare Workers via
`@sveltejs/adapter-cloudflare`. The app talks **only** to `workers/api` through
the typed client in `src/lib/api/client.ts` — never directly to NNTP providers,
Hyperdrive, R2 or KV.

Scaffolded with `npx sv create apps/web --template minimal --types ts --no-add-ons`.

## Developing

```sh
pnpm --dir apps/web install
pnpm --dir apps/web dev
```

## Checks

```sh
pnpm --dir apps/web test      # vitest unit tests
pnpm --dir apps/web check     # svelte-check typecheck
pnpm --dir apps/web build     # Cloudflare production build
pnpm --dir apps/web test:e2e  # Playwright smoke (builds + serves preview)
```

Playwright needs a Chromium build (`playwright install chromium`) plus the
system libraries Chromium links against; see `playwright.config.ts`.

## Bundle budget

CI fails `web-build` when total client JS exceeds its admitted ceiling,
starting from the frozen base **337,920 B (330 KiB)**,
measured as raw bytes over every `*.js` file under
`.svelte-kit/output/client` after `pnpm build`. The budget is pinned in
[`check_web_bundle_budget.py`](../../.github/scripts/check_web_bundle_budget.py)
(`BUDGET_CLIENT_JS_BYTES`) plus append-only decision deltas under
`ci/budgets/web-client-js/`. The checker documents the JSON schema and
validates the admission policy. Baseline was 306,065 B
(~299 KiB, 37 files) on 2026-10-27, leaving ~10% headroom for the next
first-party features while catching heavy additions (a charting or date
library typically adds 50+ KiB).

Production builds run a build-only Terser compression pass in Vite's
`renderChunk` hook, alongside the default Oxc minification. This happens before
chunk content hashes are finalized and supplies a source map for composition.
It uses safe ES2020 compression, without property mangling or unsafe transforms;
application features and the raw-byte gate remain unchanged. Verify the result
with `pnpm --dir apps/web build` followed by
`python3 .github/scripts/check_web_bundle_budget.py` from the repository root.

Admit explained growth in the **same feature PR**, after measuring current
base and integrated-head builds and checking for accidental dependencies or
duplicate imports. Add a new JSON decision with unit, positive integer delta,
measured before/after bytes, and cause; do not edit the frozen base or previous
decisions. CI sums every new file per PR and allows at most 0.5% of the current
ceiling. The rolling seven-day sum is capped at 2% of the ceiling before that
window; main merge timestamps, not file dates, determine inclusion. Queue
squashes are attributed separately and checked together against that allowance.
Full checkout history is required. Normal review and all other checks remain
mandatory; larger changes need a separately reviewed policy decision.

A nonblocking warning begins one block below the ceiling (0.5% rounded down
to 256 B; byte precision below 256 B, minimum 1 B). The hard byte ceiling,
missing/empty-output failures, and Wasm/script gates remain unchanged. Never
raise latency, rendering, security, or required-check targets to admit bytes.

## Deploy

The tracked Wrangler config is [`wrangler.toml`](wrangler.toml) in this
directory (staging/production names, no backend bindings or secret values).
[`.github/workflows/deploy.yml`](../../.github/workflows/deploy.yml) runs
`staging-web` on pushes to `main` and `preview-web` on pull requests. Its
`gate` job checks for `CLOUDFLARE_API_TOKEN`; deploys also require the
`CLOUDFLARE_ACCOUNT_ID` secret (names only; the full account ID stays private).
`post-deploy-e2e` follows a successful `staging-web`, not `staging-api`.

The same push runs the staging-only API Worker lane, `staging-api`, behind
that token gate. API deploy steps skip unless all public Actions variables
`HYPERDRIVE_ID_STAGING`, `API_RP_ID_STAGING` and `API_RP_ORIGIN_STAGING` are
present. A configured deploy requires the fail-closed
`workers/api/check_hyperdrive_cache.sh` gate (no `--allow-skip`), a canonical
HTTPS origin matching the RP ID, and Hyperdrive staging-inventory parity.
The generated API config sets `HYPERDRIVE_CACHING_DISABLED=1` only after the
cache gate passes, and the deploy passes public `BUILD_SHA`.

Lane existence or overall workflow success — even a successful `staging-api`
job whose deploy steps skipped — is not a non-skipped API deploy or proof of
exact-build health, activation or rollback. Follow the
[per-Worker evidence contract](../../docs/ops.md#future-worker-deploy-lanes-required-rollback-evidence)
alongside the [web staging checklist](../../docs/ops.md#staging-promotion-checklist).
Operator-owned staging configuration and Worker-secret wiring, authorized
Access credential injection, and backend binding activation, migrations,
health and rollback acceptance remain outstanding. The other backend Workers
(`ingest`, `stream`, `mb`, `repair`) still have no deploy lanes; production
promotion remains separate and gated. See the [operations runbook](../../docs/ops.md).
Locally, `pnpm --dir apps/web preview` serves the production build.

Never commit secrets. `HYPERDRIVE_ID_STAGING`, `HYPERDRIVE_ID_PROD` and
`HYPERDRIVE_ID_MB` are non-credential GitHub Actions **variables**, not
secrets or database URLs, and this frontend does not use them; actual
credentials live in GitHub Actions secrets and Cloudflare only.

## Structure

```text
src/lib/api/      # typed API-worker client (OpenAPI codegen replaces the types, not the call sites)
src/lib/billing/  # plan matrix, usage evaluation, checkout/portal client — display only, enforcement is server-side
src/lib/design/   # design tokens; app-wide styles in src/app.css
src/lib/format.ts # display formatting (durations, byte counts)
src/routes/       # landing (/), health (/api/health), pricing (/pricing), billing (/account/billing), …one route per slice
tests/            # Playwright smoke
```
