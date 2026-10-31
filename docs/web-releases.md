# Web release identity and retained artifacts

This wiring does **not** enable production. The production workflow remains
manual, requires an explicit main SHA and certified source canary, and refuses
first unless `PRODUCTION_DEPLOY_ENABLED` is exactly `true`. This document is not
launch approval. No production deployment or runtime acceptance is established
by offline checks, a merged change, a green job label or a CalVer tag.

## Three identities, three contracts

| Surface | Meaning |
| --- | --- |
| `GET /api/build` → exactly `{sha}` | Source commit baked into the executable by `BUILD_SHA`; unchanged during promotion |
| `GET /api/health` → exactly `{ok,version}` | Health and internal package compatibility version; not release provenance |
| `GET /api/release` → exactly `{version}` | Runtime `DEPLOYED_RELEASE_VERSION`, or explicit `null` when absent; no-store |

A canary is `YYYY.MDD.P-canary.N`, with tag `canary/vYYYY.MDD.P-canary.N`.
A stable is `YYYY.MDD.P`, with tag `vYYYY.MDD.P`. Dates are UTC; the month is
not zero-padded and the day is two digits. Only stable publication creates a
GitHub Release. The pinned shared action owns allocation, collisions, retries,
notes and publication; the web adapters do not allocate versions.

Runtime release metadata is declared **private and nonstatic** in
`apps/web/src/env.ts`. Kit 3's Cloudflare adapter does not populate the old
request `platform` field, so the endpoint also reads its declared
`$app/env/private` binding. Merely adding an unregistered environment name can
compile away its read. No release constant is added to Vite and no package
version is changed. The same saved executable can therefore advertise a
canary in staging and a stable deployment release later.

## Source transaction

The existing main-push deployment workflow holds a distinct outer writer lock
through planning, building, deploying, post-deploy E2E and canary publication.
The existing lane/reusable-E2E locks are different keys. GitHub concurrency
can replace pending runs; this is not FIFO and it does not promise a tag for
every main commit. PR previews and manual monitoring cannot publish canaries.

1. The shared action freezes a deferred-source plan for the exact repository,
   SHA, run, attempt and required job names before building.
2. The producer runs the existing Vite build once, then pinned Wrangler's
   staging **dry-run bundling**, without deployment credentials. The current
   Cloudflare adapter's small loader imports a sibling server tree outside its
   assets directory. Archiving just that loader would omit the server. Retain
   Wrangler's complete emitted module tree under the artifact's `_worker.js/`
   directory, alongside the complete adapter static/hidden asset tree.
3. `ci/web-release-artifact.py package --bundle-root ...` records the exact plan,
   web-only components, source identity, compatibility settings, portable
   API-origin setting, and a complete per-file size/SHA-256 inventory. An
   unbundled known sibling-server loader refuses instead of being certified.
4. Upload the `.tar.gz` through immutable Actions artifacts (seven-day
   retention). Record upload ID and the GitHub ZIP archive digest; that digest
   is distinct from each file hash inside the tarball. Verify the payload and
   render a saved-output config, then deploy staging with `--no-bundle`.
5. Existing same-run post-deploy synthetic and full-journey E2E execute against
   the baked source SHA. The successor reads the exact attempt and every jobs
   page, verifies the GitHub ZIP digest and retained tarball, and certifies
   actual successful deployment, synthetic, journey **and evidence-gate steps**.
6. Only an eligible certificate permits publication of the exact saved plan.
   A successful E2E job with skipped steps is explicitly **not certified** and
   publishes no tag. Other evidence, archive or identity refusals fail the job.

The certificate always lists `components: ["web"]`. API deployment remains a
separate lane; a web journey never certifies API, ingest, stream, MB or repair.
The shared action independently rechecks the run/attempt, required jobs and
immutable artifact selectors. A stable source must be fully completed and
successful; only the deferred-source canary successor can use an active run.

## Dormant production consumer

When the existing production authority eventually enables the lane, dispatch
with the explicit SHA and its exact `source_canary`. The annotation selects a
specific source run/attempt/artifact/plan, never the newest artifact or a newer
same-SHA manual run. Missing, expired, foreign, ambiguous or mismatched evidence
refuses. Rebuilding is **not** a recovery path for a lost artifact; recertify an
eligible source under the ordinary deployment policy instead.

The consumer revalidates actual executed steps, complete inventory and archive
hashes before production credentials are used. A nonempty baked API origin
refuses portable promotion: the current producer deliberately uses same-origin
API/CSP settings. No API/CSP rewrite is performed during promotion.

Stable planning happens under the existing production mutation lock. Its UTC
date/version is frozen for the transaction, even across midnight. The generated
TOML points `main`, module collection and assets at the retained tree, contains
no build hook, and differs only in approved environment configuration and
runtime release metadata. Module collection is scoped to the Worker directory,
not the client JavaScript assets. Both upload and shape validation use the same
absolute generated-config path:

```sh
node ci/check-production-secrets-shape.mjs --config "$WEB_RELEASE_CONFIG"
```

The checker also validates the committed source and retains its JSON-shadow
refusal. The sole new production admission is a canonical **stable** string
`DEPLOYED_RELEASE_VERSION` under the conventional `[env.production.vars]` table.
Additional vars, alternate/duplicate declarations, bindings, routes, invalid
dates/counters and canary values refuse. No undeclared CLI `--var` is used.

The production Environment, production-only credentials, one-time single-version
100% seed, versions-only upload, unique deployment tag, gradual 5→50→100 gates,
smoke checks and existing rollback behavior remain. Upload uses `--no-bundle`;
there is no source build, source-entrypoint fallback or trigger synchronization.
Stable publication follows successful final smoke, never an unsuccessful rollout
or rollback. Final-smoke failure still requires the existing explicit restoration
procedure; it does not silently publish stable or change that rollback contract.

## Offline verification

Run the existing required paths; no new optional CI workflow is needed:

```sh
python3 ci/test-web-release-acceptance.py -v
node --test ci/test-check-production-secrets-shape.mjs
npm --prefix e2e run test:unit
npm --prefix apps/web test -- --maxWorkers=1
npm --prefix apps/web run check
```

The acceptance fixtures exercise independent archives, nested/hidden/WASM files,
missing/extra/equal-length tampering, links/path escapes, frozen-plan replacement,
portable rendering, complete pagination, job/step execution, wrong run/attempt and
foreign/expired artifact refusal. Runtime unit tests retain exact old endpoint
shapes. Separately verify a real producer bundle and saved-output Wrangler
staging dry-run: source module closure cannot be proved merely by a green upload
command. Local Worker-runtime probes can test the same saved bytes with different
metadata without contacting Cloudflare or enabling production.
