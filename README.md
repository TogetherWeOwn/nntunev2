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
| Metering | **Analytics Engine** |
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
# Standalone Worker crates (outside the workspace, as in CI:
# api is locked via its committed Cargo.lock; mb, ingest and repair
# resolve fresh)
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

`npm run e2e:list` compiles the suite and lists the specs (202 tests across
11 files) with no staging needed. Live staging runs stay in CI — see below.

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
