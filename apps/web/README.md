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

CI fails `web-build` when total client JS exceeds **230,400 B (225 KiB)**,
measured as raw bytes over every `*.js` file under
`.svelte-kit/output/client` after `pnpm build`. The budget is pinned in
[`check_web_bundle_budget.py`](../../.github/scripts/check_web_bundle_budget.py)
(`BUDGET_CLIENT_JS_BYTES`), which also documents the value and this update
procedure — that script is the single source of truth. Baseline was 176,417 B
(~172 KiB, 31 files) on 2026-10-17, leaving ~30% headroom for framework
upgrades while catching heavy additions (a charting or date library typically
adds 50+ KiB).

To update the budget deliberately, in its own PR: rebuild, confirm the growth
is expected from the dependency/route change, bump `BUDGET_CLIENT_JS_BYTES`
with a comment citing the reason, and update the number in this paragraph.
Never bump it just to make a red build green. Wasm/script size gates are out
of scope here (tracked separately as Wasm enforcement).

## Deploy

The tracked Wrangler config is [`wrangler.toml`](wrangler.toml) in this
directory (staging/production names, no backend bindings or secret values).
[`.github/workflows/deploy.yml`](../../.github/workflows/deploy.yml) is
web-only: a push to `main` deploys `apps/web` to staging and pull requests
get previews, gated on the `CLOUDFLARE_API_TOKEN` and
`CLOUDFLARE_ACCOUNT_ID` GitHub Actions secrets (names only; the full account
ID stays private by repo policy). That workflow does not deploy the backend
Workers under `workers/`; backend binding activation, migrations, health and
rollback acceptance are still outstanding — see the
[operations runbook](../../docs/ops.md). Locally,
`pnpm --dir apps/web preview` serves the production build.

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
