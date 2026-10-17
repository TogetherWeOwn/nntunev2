# NNTune v2

Serverless BYO-Usenet-provider music streaming on Cloudflare.

## Vision: BYOP

Users **bring their own Usenet provider** — NNTune v2 never bundles or
resells Usenet access. You connect your existing provider account; we provide
the music experience on top: search, streaming, library management and
offline-friendly caching.

## Plans

Usage-based plans, in the spirit of Claude Pro/Max style tiers (e.g. a base
plan plus 5x / 20x usage multipliers). Exact limits are set at launch; the
metering primitives below are built to support them from day one.

## Architecture

| Concern | Choice |
|---|---|
| Backend | **Rust** (`workers-rs` → `wasm32`) for **ALL** backend Workers, including Durable Objects |
| Shared core | `crates/nntune-core`: NNTP client, SIMD yEnc, NZB parsing, decrypt, par2-verify — compiled to `wasm32` **and** native |
| Frontend | **SvelteKit** on Cloudflare (`adapter-cloudflare`) |
| Relational data | **PlanetScale Postgres via Cloudflare Hyperdrive** for ALL relational data, including our own MusicBrainz mirror |
| Object cache | **R2** for audio / artwork cache |
| Ingest | **Queues + Workflows** |
| Config | **KV** |
| Usage enforcement (designed) | **Postgres** daily flow ledger and live cache-grant gauge; pool Durable Object leases for concurrency (`docs/adr/0009-stack.md:30`, pool semantics in `docs/adr/0004-plan-matrix.md`) |
| Observability | **Analytics Engine**, sampled telemetry only — never a billing/cap authority (`docs/adr/0009-stack.md:31`) |

This is the accepted design (`docs/adr/0009-stack.md:3`), not deployed
billing evidence: plan prices are launch proposals, not permission to
activate live billing (`docs/adr/0004-plan-matrix.md:12-13`). The decided
subscription mechanism lives in `docs/adr/0005-billing.md`; no vendor is
chosen or changed here.
| Batch only | **Containers** — MusicBrainz import, par2 repair (everything interactive stays serverless) |

```text
crates/nntune-core   # NNTP client, SIMD yEnc, NZB, decrypt, par2-verify (wasm32 + native)
workers/             # Rust workers-rs backends incl. Durable Objects
apps/web/            # SvelteKit frontend (adapter-cloudflare)
```

## Quickstart

Setup, test, then read the runbooks before touching staging. Every command
below was run verbatim against a clean checkout; expected results are noted.

### Prerequisites

- Rust stable with the `wasm32-unknown-unknown` target (all backends compile
  to `wasm32` for deployment):

  ```sh
  rustup target add wasm32-unknown-unknown
  ```

- Node 24 for `apps/web` (the CI `NODE_VERSION`) and pnpm 10 (the frontend
  pins its lockfile for pnpm); the `e2e` harness uses Node 26, kept in sync
  with `@types/node` in `e2e/package.json` (as in the CI `e2e-lint` job).
- Python 3.11+ (repo lint scripts).
- No secrets are needed for any local test. Secret *names* are listed under
  [Secrets](#secrets-never-committed); values live in GitHub Actions secrets
  or shell exports, never in this repo.

### Setup

```sh
# Web frontend
cd apps/web && pnpm install --frozen-lockfile && cd ../..
# E2E harness (Playwright; chromium needed for browser-level self-tests)
cd e2e && npm ci && npx playwright install chromium && cd ..
```

Rust needs no install step: `cargo` fetches dependencies on first build.
On Linux without system browser dependencies, use
`npx playwright install --with-deps chromium` instead (as CI does).
If `NODE_ENV=production` is exported in your shell, npm silently omits the
dev dependencies the harness needs — install it with
`npm ci --include=dev` instead (same caveat as in `e2e/README.md`).

### Test

```sh
# Rust workspace (core, config, stream, containers/repair, test fixtures)
cargo test --workspace
# Standalone Worker crates (excluded from the workspace in Cargo.toml; each
# carries a committed lockfile: Cargo.lock plus workers/{api,ingest,mb,
# repair}/Cargo.lock, apps/web/pnpm-lock.yaml, e2e/package-lock.json).
# Committed lockfiles are not the same as --locked enforcement: the
# cargo-deny lane proves all five Cargo lockfiles fresh
# (.github/workflows/ci.yml:114-118), but only the api lane passes --locked
# to test/clippy/check (.github/workflows/ci.yml:282-287); the mb, ingest
# and repair lanes resolve without --locked (.github/workflows/ci.yml:374,
# 415, 437).
cargo test --locked --manifest-path workers/api/Cargo.toml --all-targets
cargo test --manifest-path workers/mb/Cargo.toml --all-targets
cargo test --manifest-path workers/ingest/Cargo.toml --all-targets
cargo test --manifest-path workers/repair/Cargo.toml --all-targets
# Web frontend (from apps/web): unit tests, then typecheck
pnpm test
pnpm check
# E2E harness (from e2e): typecheck, offline self-tests, then spec discovery
npm run typecheck
npm run test:unit
npm run e2e:list
```

`npm run e2e:list` compiles the suite and lists the specs with no staging
needed. Discovery snapshot, verified 2026-10-17 via
`npm --prefix e2e run e2e:list` (with dev dependencies installed): 265
tests in 17 files. Totals drift as specs land — re-run the command for a
fresh count rather than quoting this one. Live staging runs stay in
CI — see below.

Public-repo hygiene gate (part of `ci-ok`, from the repo root):

```sh
python3 .github/scripts/public_text_lint.py
```

### Deploy / staging evidence

Deploys are push-triggered in CI; there is nothing to deploy from a laptop:

- Pushing to `main` runs `.github/workflows/deploy.yml`, which deploys
  `apps/web` to staging (`staging-web` job) and then sequences the full
  Playwright journey as `post-deploy-e2e` (same run, only after a successful
  deploy — a skipped/failed deploy cannot produce a PASS). Only `apps/web`
  is deployed (no workflow deploys the backend Workers), and staging is
  secret-gated: without `CLOUDFLARE_API_TOKEN` the `gate` job skips
  `staging-web` and `post-deploy-e2e`, so fork PRs get neither a deploy
  nor a staging PASS.
- The exact-head promotion boxes (CI green, deploy run, E2E result, Access
  intact, data-plane guards, rollback pointer) are defined in the
  [Staging promotion checklist](docs/ops.md#staging-promotion-checklist).
- How the harness uses staging credentials, what counts as evidence for a QA
  PASS verdict, and the pre-deploy skip behavior are documented in
  [e2e/README.md](e2e/README.md) (sections `Activation progression`, `CI`,
  `Evidence`).

Credential setup beyond these pointers is out of scope here: staging E2E
needs the authorized QA credential wiring described in those documents, and
production promotion is a separate gated lane.

## Secrets (never committed)

| Secret | Purpose |
|---|---|
| `CLOUDFLARE_API_TOKEN` | Cloudflare deploys (Workers, R2, KV, Queues) |
| `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account scope (full value kept private by repo policy) |
| `PLANETSCALE_MIGRATOR_URL_STAGING` | Staging migrations to `nntunev2_staging` on the shared `main` cluster; production migrator remains deferred to its gated workflow |
| `CF_ACCESS_CLIENT_ID` / `CF_ACCESS_CLIENT_SECRET` | **Pending:** not set as repo secrets; authorized QA credential wiring is required before enabling `STAGING_BASE_URL` for the staging E2E workflow |

The Access service-token resource ID is not a substitute for the two client
credentials. Keep the Access gate enabled; do not publish or rotate the shared
QA token to satisfy the pending wiring. See the [runbook](docs/ops.md) for the
current shared-cluster topology, host-reported isolation, and launch deferrals.

`HYPERDRIVE_ID_STAGING`, `HYPERDRIVE_ID_PROD`, and `HYPERDRIVE_ID_MB` are
non-credential GitHub Actions **variables**, not database URLs or secrets.
Their IDs and the environment-specific resource inventory are in
[the operations runbook](docs/ops.md). Database credentials remain private.

No secret values live in this repo. `gitleaks` is a required credential-pattern
scan; secret scanning + push protection are enabled. These checks do not replace
review for personal data or the policy keeping the full account ID private.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Every PR must pass `ci-ok`,
`gitleaks`, `pr-lint` and Paperclip Review 5/5.

## Security

See [SECURITY.md](SECURITY.md). Please report vulnerabilities privately via
GitHub's private vulnerability reporting.
