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
| Batch only | **Containers** — MusicBrainz import, par2 repair (everything interactive stays serverless) |

This is the accepted design (`docs/adr/0009-stack.md:3`), not deployed
billing evidence: plan prices are launch proposals, not permission to
activate live billing (`docs/adr/0004-plan-matrix.md:12-13`). The decided
subscription mechanism lives in `docs/adr/0005-billing.md`; no vendor is
chosen or changed here.

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

Local Rust builds go through the bounded Cargo pool wrapper — prefix
the `cargo` commands below with `python3 scripts/cargo_cache.py run --`,
e.g. `python3 scripts/cargo_cache.py run -- test --workspace`. Pool rules
(two slots, exit 75 when refused, never a per-worktree `target/`) live in
the [agent notes](AGENTS.md#local-rust-builds-the-bounded-cargo-pool);
checkouts without the pool use the bare commands as listed.

```sh
# Rust workspace (core, config, stream, containers/repair, test fixtures)
cargo test --workspace
# Standalone Worker crates (excluded from the workspace in Cargo.toml; each
# carries a committed lockfile: Cargo.lock plus workers/{api,ingest,mb,
# repair}/Cargo.lock, apps/web/pnpm-lock.yaml, e2e/package-lock.json).
# The cargo-deny job's `Prove committed lockfiles are fresh` step checks
# the workspace and api/mb/ingest/repair lockfiles (.github/workflows/ci.yml).
# Those four Worker jobs also pass --locked to every test/clippy/check
# invocation, including wasm32 checks and clippy, so CI cannot silently
# re-resolve their dependency graphs. Formatting checks are unchanged.
# The workspace rust-test job separately pins one --locked probe:
# nntune-core --no-default-features.
cargo test --locked --manifest-path workers/api/Cargo.toml --all-targets
cargo test --locked --manifest-path workers/mb/Cargo.toml --all-targets
cargo test --locked --manifest-path workers/ingest/Cargo.toml --all-targets
cargo test --locked --manifest-path workers/repair/Cargo.toml --all-targets
# Web frontend (from apps/web): unit tests, then typecheck
pnpm test
pnpm check
# E2E harness (from e2e): typecheck, offline self-tests, then spec discovery
npm run typecheck
npm run test:unit
npm run e2e:list
```

`npm run e2e:list` compiles the suite and lists the specs with no staging
needed. Discovery snapshot, verified 2026-10-18 via
`npm --prefix e2e run e2e:list` (with dev dependencies installed): 339
tests in 20 files. Totals drift as specs land — re-run the command for a
fresh count rather than quoting this one. Live staging runs stay in
CI — see below.

Public-repo hygiene gate (part of `ci-ok`, from the repo root):

```sh
python3 .github/scripts/public_text_lint.py
```

### Deploy / staging evidence

Deploys are push-triggered in CI; there is nothing to deploy from a laptop:

- Pushing to `main` runs [`.github/workflows/deploy.yml`](.github/workflows/deploy.yml):
  `staging-web` deploys `apps/web`, then `post-deploy-e2e` sequences the full
  Playwright journey after both that web deploy and the `staging-api` job
  succeed. Pull requests use
  `preview-web` for web previews. The `gate` job checks for
  `CLOUDFLARE_API_TOKEN`; deploys also require `CLOUDFLARE_ACCOUNT_ID`.
  Without the token, deploy/preview jobs skip, so fork PRs get neither a
  deploy nor a staging PASS.
- The same push also runs the staging-only API Worker lane, `staging-api`,
  behind the same token gate. Its deploy steps skip if any public Actions
  variable is missing: `HYPERDRIVE_ID_STAGING`, `API_RP_ID_STAGING` or
  `API_RP_ORIGIN_STAGING`. When configured, it requires the fail-closed
  `workers/api/check_hyperdrive_cache.sh` gate (no `--allow-skip`), a canonical
  HTTPS origin matching the RP ID, and Hyperdrive staging-inventory parity.
  Only after the cache gate passes does the generated config set
  `HYPERDRIVE_CACHING_DISABLED=1`; the deploy passes public `BUILD_SHA`.
- Lane existence or overall workflow success is not API runtime acceptance:
  even a successful `staging-api` job can have skipped deploy steps. Require
  a non-skipped API deploy plus exact-build health, activation and rollback
  evidence under the [per-Worker evidence contract](docs/ops.md#future-worker-deploy-lanes-required-rollback-evidence).
  `post-deploy-e2e` waits for both `staging-web` and `staging-api` (so it
  never runs against the previous API mid-deploy), but a green `staging-api`
  job can be an unconfigured skip, so the journey does not prove API deploy
  or rollback. Operator-owned staging configuration and
  Worker-secret wiring, authorized Access credential injection, and the
  separate `ingest`, `stream`, `mb` and `repair` deploy lanes remain prerequisites
  or deferred work; no backend runtime acceptance is claimed here.
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
