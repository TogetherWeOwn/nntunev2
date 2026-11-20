# NNTune v2

Serverless music streaming on Cloudflare, from a shared curated catalogue.

Read the [roadmap](ROADMAP.md) for product direction, boundaries and conditional
future intentions. It is not a feature-availability or delivery-status tracker.

## Vision

Users do **not** bring a Usenet provider or NZBs. NNTune serves one shared,
admin-curated catalogue. The approved storage direction is a plain S3 client:
media comes from a configured S3-compatible provider, which owns storage-side
accounts, encryption, caching and repair ([ADR 0015](docs/adr/0015-standard-s3-storage-boundary.md)).
The existing NNTP implementation has not yet migrated. We provide search,
streaming and library management; there are no user provider accounts to
connect and no offline downloads.

## Plans

Usage-based plans, in the spirit of Claude Pro/Max style tiers (e.g. a base
plan plus 5x / 20x usage multipliers). Exact limits are set at launch; the
metering primitives below are built to support them from day one.

## Architecture

| Concern | Choice |
|---|---|
| Backend | **Rust** (`workers-rs` → `wasm32`) for backend Workers, including Durable Objects; the staging Tail alert Worker (`workers/observability`) is JavaScript |
| Shared core | `crates/nntune-core`: transitional NNTP re-export shim for [velmora](https://github.com/TogetherWeOwn/velmora), currently pinned by git `rev`. Remove the shim only after its last caller is gone; the approved target ([ADR 0015](docs/adr/0015-standard-s3-storage-boundary.md)) is a plain standard S3 client, not yet migrated |
| Frontend | **SvelteKit** on Cloudflare (`adapter-cloudflare`) |
| Relational data | **PlanetScale Postgres via Cloudflare Hyperdrive** for ALL relational data, including our own MusicBrainz mirror |
| Object cache | **R2** for audio / artwork cache |
| Ingest | **Queues + Workflows** |
| Config | **KV** |
| Usage enforcement (designed) | **Postgres** daily flow ledger and live cache-grant gauge; pool Durable Object leases for concurrency (`docs/adr/0009-stack.md:30`, pool semantics in `docs/adr/0004-plan-matrix.md`) |
| Observability | **Analytics Engine**, sampled telemetry only — never a billing/cap authority (`docs/adr/0009-stack.md:31`) |
| Batch only | **Containers** — par2 repair (everything interactive stays serverless); MusicBrainz import instead follows the [2026-11-07 no-Containers amendment](docs/adr/0007-mb-mirror.md#2026-11-07-amendment-retirement-and-conditional-bounded-replacement), with import/activation still gated |

This is the accepted design (`docs/adr/0009-stack.md:3`), not deployed
billing evidence: plan prices are launch proposals, not permission to
activate live billing (`docs/adr/0004-plan-matrix.md:12-13`). The decided
subscription mechanism lives in `docs/adr/0005-billing.md`; no vendor is
chosen or changed here.

```text
crates/nntune-core   # velmora engine re-export (NNTP, yEnc, NZB, decrypt, par2) + transcode (wasm32 + native)
workers/             # Rust workers-rs backends incl. Durable Objects
apps/web/            # SvelteKit frontend (adapter-cloudflare)
```

## Quickstart

Set up for focused development, push a PR branch for GitHub Actions
verification, then read the runbooks before touching staging. Full suites
and CI gates do not run locally.

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

Push commits to the PR branch and **open a draft PR while incomplete**.
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs the Rust
workspace and standalone Worker suites, frontend checks/builds, and offline
E2E harness validation; its aggregate gate is **`ci-ok`**. There is no
`check.yml` in this repo. Do not reproduce this workflow as a local full
suite or CI mirror. Mark the PR ready once the work is complete.

Local test execution is limited to focused reproduction of **one failing
test or crate** from Actions. Small Rust reproductions use the
[bounded pool](AGENTS.md#local-rust-builds-the-bounded-cargo-pool);
missing/refused pools never permit bare compiling Cargo or a per-worktree
`target/`. Heavy focused work uses the guarded `boat` PATH wrapper with
`--no-env`, source-only export and verified cleanup. Boat fallback does not
authorize a full suite, CI mirror or heavy raw-host run. See
[verification policy](CONTRIBUTING.md#verification-github-actions-first)
for the permitted paths; defer to Actions when none is available.

In Actions, the `cargo-deny` job's `Prove committed lockfiles are fresh`
step checks the workspace and api/mb/ingest/repair lockfiles.
Those four Worker jobs also pass --locked to every test/clippy/check
invocation, including wasm32 checks and clippy, so CI cannot silently
re-resolve their dependency graphs. Formatting checks are unchanged.
The workspace rust-test job separately pins one --locked probe:
nntune-core --no-default-features. This is not a claim that every workspace
command uses `--locked`; the standalone transcode Worker is outside the
four-Worker freshness/compiling-flag gate.

The four Worker test commands below document existing **Actions coverage**,
not a local checklist:

```text
cargo test --locked --manifest-path workers/api/Cargo.toml --all-targets
cargo test --locked --manifest-path workers/mb/Cargo.toml --all-targets
cargo test --locked --manifest-path workers/ingest/Cargo.toml --all-targets
cargo test --locked --manifest-path workers/repair/Cargo.toml --all-targets
```

The API job additionally selects its ignored acceptance cases and skips
Postgres cases handled by the separate database job. The transcode Worker
job runs its own tests without `--locked`. These dependency/security gates
remain unchanged.

Record the reviewed head SHA and exact-head **Actions run/job links** with
conclusions as CI evidence, not local transcripts. Reviewers and the
Release Manager use those links without waiving staging, rollback or
production approval gates. Live staging runs stay in Actions — see below.

For a documentation-only change, focused static/diff checks are sufficient
locally; for example the public-text scan (also part of `ci-ok`):

```sh
python3 .github/scripts/public_text_lint.py
```

### Deploy / staging evidence

Deploys are push-triggered in CI; there is nothing to deploy from a laptop:

- Pushing to `main` runs [`.github/workflows/deploy.yml`](.github/workflows/deploy.yml):
  `staging-web` deploys `apps/web`, then `post-deploy-e2e` sequences the full
  Playwright journey after both that web deploy and the `staging-api` job
  succeed. Pull requests get no preview deploy; PR CI (lint/test/build)
  stays. The `gate` job checks for
  `CLOUDFLARE_API_TOKEN`; deploys also require `CLOUDFLARE_ACCOUNT_ID`.
  Without the token, deploy jobs skip.
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
| `STAGING_BASE_URL` | Staging host the E2E workflow targets; unset means the staging E2E jobs skip |
| `CF_ACCESS_CLIENT_ID` / `CF_ACCESS_CLIENT_SECRET` | Cloudflare Access service-token credentials the staging E2E workflow sends; required once `STAGING_BASE_URL` is set |
| `QA_AUTH_TOKEN` | Bearer for the staging-only E2E seams; see `e2e/README.md` |

The Access service-token resource ID is not a substitute for the two client
credentials. Keep the Access gate enabled; do not publish or rotate the shared
QA token. See the [runbook](docs/ops.md) for the
current shared-cluster topology, host-reported isolation, and launch deferrals.

`HYPERDRIVE_ID_STAGING`, `HYPERDRIVE_ID_PROD`, and `HYPERDRIVE_ID_MB` are
non-credential GitHub Actions **variables**, not database URLs or secrets.
Their IDs and the environment-specific resource inventory are in
[the operations runbook](docs/ops.md). Database credentials remain private.

No secret values live in this repo. `gitleaks` is a required credential-pattern
scan; secret scanning + push protection are enabled. These checks do not replace
review for personal data or the policy keeping the full account ID private.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for required checks and the
review/merge policy. `ci-ok`, `gitleaks`, `pr-lint`, `migrations-staging`
and `Devin Review` must pass, and review findings must be fixed or answered.
CodeRabbit is advisory; answer its findings without waiting for it. The
final-stage reviewer merges the pinned head before approval; production
merges/promotions still require exact-head Release Manager authorization.

## Security

See [SECURITY.md](SECURITY.md). Please report vulnerabilities privately via
GitHub's private vulnerability reporting.
