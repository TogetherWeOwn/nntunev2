# ADR 0011: Deployed-build identity separation

- **Status:** Accepted design; implementation
  and independent review gates remain.
- **Date:** 2026-10-17
- **Roadmap:** M1 exit traceability row 7 (deployed-build identity gap).

## Context

`docs/m1-exit-traceability.md` row 7 records the gap: no spec ties the
deployed build to the exact commit SHA. The package version is not a build
identity — it is identical across rebuilds of the same version — so staging
cannot prove *which* commit is serving. Per `docs/ops.md`, neither a green
deploy run nor a bare `ok` health response proves the promoted SHA is live;
same-build evidence must not be claimed until a deployed-build identity is
exposed and compared with the promotion SHA.

Meanwhile `GET /api/health` deliberately serves the package version only —
exactly `{ ok, version }` where `version` is `APP_VERSION` from
`package.json` (`apps/web/src/lib/version.ts`). That contract is pinned in
two places that must keep passing unmodified:

- `apps/web/tests/smoke.spec.ts` (`health endpoint reports ok with the
  package version and nothing else`): asserts the body keys are exactly
  `['ok', 'version']` so no internal identifiers (commit SHAs, hostnames,
  bindings) may leak in.
- `apps/web/src/lib/version.test.ts`: asserts `APP_VERSION` equals
  `pkg.version` and matches no whitespace, hostname, path, or 20+ hex run
  pattern.

Any identity signal added to `/api/health` breaks the exact-keys smoke pin. The decision
therefore needs recording: where does the deployed-build identity live, and
why not in `/api/health`?

## Decision

**Deployed-build identity lives on a dedicated surface, separate from
`/api/health`, whose package-only semantics are frozen.**

- `GET /api/build` returns exactly `{ sha }`: the 40-character lowercase
  hex commit SHA. The sole source is the SHA baked at build time (sourced
  from `BUILD_SHA` / `GITHUB_SHA` via the `__BUILD_SHA__` build-time
  define, validated to `^[0-9a-f]{40}$` with everything else collapsing to
  `"unknown"`). The baked value is authoritative by design: a runtime var
  or Workers version field is mutable per-request/per-deploy state, so
  letting one override the bake would let an empty, invalid, or stale value
  mask the exact head this artifact was built from. Promotion lanes must
  compare the reported SHA against the promotion SHA rather than trust
  build provenance.
- Unknown or unset build identity renders as the explicit string
  `"unknown"` — never silently omitted, never an empty string. The `sha`
  key is always present. An `unknown` identity is a build-configuration
  signal, and promotion lanes must treat it as "cannot prove same-build",
  not as a match.
- The surface exposes no secret, hostname, or binding material: a commit SHA
  is public build metadata, validated to the strict `^[0-9a-f]{40}$` shape
  with everything else collapsing to `"unknown"`.
- `GET /api/health` keeps serving exactly `{ ok, version }` with the
  package version. The acceptance specs for this surface
  (`apps/web/src/lib/build-identity.test.ts`,
  `apps/web/tests/build-identity.spec.ts`) assert both the new surface and
  the untouched health pins.

## Rejected alternatives

1. **Add `sha` to `/api/health`.** Breaks the exact-keys assertion in
   `smoke.spec.ts`, pinned to pass unmodified. (The `version.test.ts`
   no-hex-run pin guards the `APP_VERSION` constant itself, so it constrains
   alternative 4, not a separate `sha` key.) It also
   conflates two audiences: the public version contract consumed by clients
   and monitors, and the deploy-verification signal consumed by promotion
   lanes. Every health consumer would need re-auditing for the widened
   shape; keeping health minimal keeps that audit closed.
2. **Reuse `/api/ready`.** Readiness is the boolean promotion gate (200 JSON
   `{ ok: true }` only; 404 and 503 fail, pinned by the readiness spec via
   `e2e/helpers/readiness.ts`). Overloading it with identity breaks the
   promotion-readiness contract and couples "is it up" to "which build is
   it" — a build that correctly reports `unknown` must still report
   readiness truthfully.
3. **Header-only identity (e.g. a worker-version header).** Headers are
   stripped or rewritten by proxies and CDN layers, and they are not
   uniformly assertable by the web acceptance specs. An explicit JSON body
   on a dedicated route is discoverable, test-pinned, and independent of
   transport intermediaries.
4. **Embed the SHA in the version string.** Violates the `version.test.ts`
   pin directly (no 20+ hex runs, no internal identifiers) and corrupts the
   semver-ish package contract that release tooling and clients parse.

## Consequences

- Staging e2e can assert exact-head deployment by comparing `/api/build`'s
  `sha` against the promotion SHA, closing the traceability row-7 gap (the
  promotion-SHA assertion itself is a separate card).
- `/api/health` and `/api/ready` contracts are unchanged; existing monitors
  and the smoke/readiness specs need no updates.
- A future need to expose more build metadata (build time, lane, image
  digest) extends the dedicated surface, never health — widening health
  needs a new ADR.
- Rollback trust assumptions are out of scope here; they are owned by
  a separate card.
- Until the build-identity surface lands, this ADR is the
  contract its implementation PR is reviewed against; the reviewer checks
  the diff for accuracy against that surface.
