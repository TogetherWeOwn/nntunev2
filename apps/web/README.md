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

CI fails `web-build` when total client JS exceeds **286,720 B (280 KiB)**,
measured as raw bytes over every `*.js` file under
`.svelte-kit/output/client` after `pnpm build`. The budget is pinned in
[`check_web_bundle_budget.py`](../../.github/scripts/check_web_bundle_budget.py)
(`BUDGET_CLIENT_JS_BYTES`), which also documents the value and this update
procedure — that script is the single source of truth. Baseline was 230,544 B
(~225 KiB, 36 files) on 2026-10-23, preserving the original ~54 KiB absolute
headroom while catching heavy additions (a charting or date library typically
adds 50+ KiB).

Production builds run a build-only Terser compression pass in Vite's
`renderChunk` hook, alongside the default Oxc minification. This happens before
chunk content hashes are finalized and supplies a source map for composition.
It uses safe ES2020 compression, without property mangling or unsafe transforms;
application features and the raw-byte gate remain unchanged. Verify the result
with `pnpm --dir apps/web build` followed by
`python3 .github/scripts/check_web_bundle_budget.py` from the repository root.

To update the budget deliberately, in its own PR: rebuild, confirm the growth
is expected from the dependency/route change, bump `BUDGET_CLIENT_JS_BYTES`
with a comment citing the reason, and update the number in this paragraph.
Never bump it just to make a red build green. Wasm/script size gates are out
of scope here (tracked separately as Wasm enforcement).

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
