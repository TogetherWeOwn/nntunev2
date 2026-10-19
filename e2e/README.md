# NNTune v2 — E2E + staging smoke harness

Playwright suite covering the venture slice on **staging**: signup → BYOP
wizard (synthetic NNTP endpoint) → NZB ingest → playback with Range seeks →
usage meter → plan-limit enforcement. Plus a fast synthetic smoke
(`health.spec.ts`) that doubles as the post-deploy monitor.

Public repo: **secret names only, never values.** Values live in GitHub
Actions secrets (CI) or shell exports (local).

## Activation progression

1. **Pre-deploy (now):** `STAGING_BASE_URL` is unset. Every spec skips with a
   notice. PRs and `main` stay green; the `e2e-lint` CI job still typechecks
   and lists the specs so the harness cannot rot silently. A skipped live
   run is not acceptance evidence: it proves nothing about staging.
2. **Activation hold:** keep the Actions secret `STAGING_BASE_URL` unset until
   deployment exists and the authorized Operator provides a value-free delivery
   receipt for the existing QA Access pair through an approved secure source and
   repository-scoped contract. An Access policy or token resource ID is not
   delivery of `CF_ACCESS_CLIENT_ID` and `CF_ACCESS_CLIENT_SECRET`. Provisioning
   alone does not authorize distributing credentials. Name-only availability
   must then be rechecked; names and a receipt do not prove authentication.
   NNTP and QA-auth prerequisites remain separate and must also be verified.
3. **Staging live:** after the prerequisites are met, setting `STAGING_BASE_URL`
   activates fail-closed checks. QA must verify the deployed head with Access
   intact. Production promotion still requires exact-head staging evidence and
   the deployment lane's promotion gate; neither is established by this harness.
4. **Contract drift:** a renamed `data-testid` or reshaped JSON body fails
   with the contract key named. Update app + `helpers/contract.ts` together.

## Contract drift (current app, 2026-10-14)

`origin/main` now ships the SvelteKit app (PR #12), so the harness is
checked against its real contract for the first time. The smoke stays green
on the current app; the items below are future contract, documented here so
the activation checklist is accurate:

- `GET /api/health` returns `{ok:true, version:<pkg>}`
  (`apps/web/src/routes/api/health/+server.ts`, pinned by
  `apps/web/tests/smoke.spec.ts` exact-keys assertion). `tests/health.spec.ts`
  requires `ok === true` and asserts `version` whenever the deployed app
  ships it, so the check stays green across the deploy boundary.
- `GET /api/ready` currently returns 200 `{ok:true}` while the stateless app
  can serve traffic (`apps/web/src/routes/api/ready/+server.ts`). The 503
  `{ok:false}` plus `Retry-After` shape in `apps/web/src/lib/ready.ts` is
  reserved for a future downstream dependency check; the current endpoint
  does not emit it. The smoke is a **promotion** gate, stricter than the
  route: `assertPromotionReady` (`helpers/readiness.ts`) passes only for
  HTTP 200 JSON `{ok:true}`. A 404 (build predating the route), a 503 (even
  well-formed, with `Retry-After`), an HTML 200, malformed JSON or
  `{ok:false}` all fail the smoke; the route's 503 not-ready semantics are
  unchanged. The gate is pinned offline by `tests/readiness.spec.ts`
  (`npm run test:unit`), with no staging or credentials needed.
- `apps/web/src/lib/contract.test.ts` checks that each hook in
  `helpers/contract.ts` `TESTIDS` is declared in its expected route file.
  This is a source-level route contract, not proof that conditional markup is
  reachable or visible at runtime; the smoke separately checks selected hooks
  in rendered pages. `/signup`'s real submission, the BYOP wizard's backend
  behavior, ingest/library/stream/usage endpoints, and usage-meter enforcement
  still depend on their venture slices; the journey specs expose those missing
  backend behaviors directly.

## Run locally

```sh
cd e2e
npm ci
npx playwright install --with-deps chromium
STAGING_BASE_URL=https://staging.example \
  CF_ACCESS_CLIENT_ID='<from ~/secure-drop>' \
  CF_ACCESS_CLIENT_SECRET='<from ~/secure-drop>' \
  SYNTHETIC_NNTP_HOST='<synthetic fixture host>' \
  SYNTHETIC_NNTP_PORT='563' \
  SYNTHETIC_NNTP_USER='<test user>' \
  SYNTHETIC_NNTP_PASS='<from ~/secure-drop>' \
  npm run e2e:staging
```

Smoke only: `npm run e2e:smoke`. Validate without staging: `npm run typecheck`,
`npm run test:unit`, and `npm run e2e:list`.

If `NODE_ENV=production` is exported in your shell, install with
`npm install --include=dev` — otherwise npm silently omits the dev
dependencies this harness needs.

Toolchain: CI runs Node 26 for this harness (both jobs in
`.github/workflows/e2e-staging.yml` and the `e2e-lint` job in `ci.yml`,
kept in sync with the `@types/node` major in `e2e/package.json`;
Playwright `1.64.0` there too). The app itself uses Node 24 + pnpm
(`NODE_VERSION`/`PNPM_VERSION` in `ci.yml`) and is unrelated to this
npm-based harness (`e2e/package-lock.json`).

## Environment

| Name | Required | Purpose |
|---|---|---|
| `STAGING_BASE_URL` | yes (post-deploy) | `https://` staging origin. Unset → skip with notice. |
| `CF_ACCESS_CLIENT_ID` / `CF_ACCESS_CLIENT_SECRET` | whenever staging is configured | QA service-token pair. Both absent or either absent → fail closed before requests. |
| `QA_AUTH_TOKEN` | optional | Bearer for `POST /api/test-accounts` (staging-only seam) and `POST /api/test-codes` (staging-only emailed-code retrieval seam for `example.test` mailboxes — returns the issued code without minting). Absent → UI signup fallback, and the wizard-arrival spec (`signup-wizard-arrival.spec.ts`) skips locally / fails closed on CI. The dedicated public UI-signup case ignores both seams either way. |
| `SYNTHETIC_NNTP_HOST` / `_PORT` / `_USER` / `_TLS` | for BYOP spec | Hosted fixture server or test provider (defaults: `synthetic-nntp`, `563`, `e2e`, TLS on). The port must be in the BYOP allowlist (`119` or `563`) and TLS-off is rejected — the harness fails closed on a fixture transport the wizard cannot represent. |
| `SYNTHETIC_NNTP_PASS` | for BYOP spec | No default — fail closed when missing. |

How the suite uses them:

- Configured staging requires both Access values before any request:
  `playwright.config.ts` validates the URL and pair at startup. Browser pages
  attach them ONLY to the staging origin via an origin-checked
  `context.route` (`applyAccountToPage` + `isStagingOrigin`): cross-origin
  subresources (fonts, analytics, stream/store URLs) never receive the
  service token or session bearer, and staging 302 redirects to another
  origin are followed WITHOUT the headers (no-redirect fetch + fulfill, so
  the follow-up request re-enters the scope check clean). API clients attach
  them per request through `apiGet`/`apiPost` (`helpers/accounts.ts`), which
  fetch with `maxRedirects: 0`: a staging 302 to another origin is never
  followed with credentials — the foreign origin receives zero requests and
  the 3xx fails by name (status + location, values never logged). Any raw
  `request.*` / `page.request.*` / `context.request.*` call
  (`get`/`post`/`put`/`patch`/`delete`/`head`/`fetch`) is banned in live
  specs — a headers-conditioned `get`/`post` pin is bypassable by nested-paren
  stream URLs and `fetch`.
  Both missing or either
  missing fails closed;
  offline checks remain usable with no staging URL and no Access pair. An Access wall (wall status + Access markers, including
  302s to the Access login) fails closed with a remediation message instead
  of masquerading as an app failure; plain app 4xx responses pass through.
  The smoke also proves the inverse for `/api/health` and `/api/ready`:
  `anonymousApiGet` requests both with no Access headers or credentials in
  a fresh, isolated request context (no-follow) and `assertAccessWall`
  passes only on an Access-specific denial — an application `ok:true`
  answer to an anonymous request fails the smoke. Diagnostics stay
  names-only (fixed context + status; no URL, header or body values).
- The smoke also proves the inverse for the app shell: an anonymous `GET /`
  from a fresh, isolated request context (no Access pair, no cookies,
  redirects never followed — `anonymousGet` in `helpers/accounts.ts`) must
  be denied by the Access wall under the same joint status+markers predicate
  (`assertAccessWall`, `helpers/access.ts`) and must not return the 200 app
  shell. The credentialed app-shell test in the same file is the authorized
  control. The anonymous probe changes no Access policy, needs no
  credential, and never emits response bodies, headers or cookies.
- Test-account sessions minted by the seam travel as
  `Authorization: Bearer <session>` (contract owned by the auth card —
  `helpers/accounts.ts` names it; cookie sessions would update the helper +
  specs together).
- Test accounts are throwaway (`e2e+<stamp>@example.test`); the API seam is
  tried first, a form-submission UI signup (emailed-code step) second (each journey file signs
  up its own identity — Playwright isolates module state per file, so there
  is no cross-file account sharing). Independently of that choice,
  `signup-byop.spec.ts` carries a dedicated public UI-signup case
  (`submitPublicUiSignup`, null identity): it always submits the real
  `/signup` form with a unique synthetic mailbox and requires the
  emailed-code step with no inline error, even when `QA_AUTH_TOKEN`
  is set, so a seam-backed run still proves public UI form submission.
  Form-to-wizard arrival is asserted by `signup-wizard-arrival.spec.ts`: the
  code-plus-passkey finish retrieves the code from the staging-only
  `POST /api/test-codes` seam, which is implemented in the API worker but
  awaits its staging deployment and activation
  (see row 1 of `docs/m1-exit-traceability.md`). Once `QA_AUTH_TOKEN` is set the seam is
  required infrastructure: transport errors, non-2xx and non-JSON mint
  responses throw instead of collapsing into a silent skip, and API-driven
  steps (`requireApiAccount`) refuse to run anonymously. The identity that
  ingests always completes the BYOP wizard first (shared
  `completeByopWizard` helper), because the ingest worker probes through the
  user's provider connection. The over-limit checks need the seam to support
  `{plan:"overlimit"}` (metering card): the stream leg and the ingest leg
  each mint their own capped account (never the journey identity, never each
  other's) and run AFTER the browser legs at the end of the serial chain, so
  a red-by-design capped-account failure skips no other verdict. The stream
  leg requires the seam to pre-seed a streamable track on its capped account:
  ingesting the fixture as the capped account would contradict the
  ingest-refusal leg (the same ingest must be refused). An unavailable
  over-limit seam fails instead of skipping — locally and on CI — so
  plan-limit enforcement can never pass unexercised. Each leg asserts the
  full refusal verdict (`assertPlanLimitRefusal` in `helpers/contract.ts`):
  402/429 status, limit + reset headers (429 additionally requires
  `retry-after`), the status-consistent limit code (`plan_limit_exceeded` on
  402, `rate_limited` on 429) with a present, status-consistent retryable
  flag, the upgrade CTA pointer (a matching key whose value points at
  `/pricing`), and no secret leakage.

## CI

- `.github/workflows/e2e-staging.yml` — no `push` trigger. The full
  journey runs only as a reusable-workflow call from `deploy.yml`'s
  `post-deploy-e2e` job, which `needs` the same run's `staging-web`
  deploy, so a skipped/failed deploy cannot produce a full-journey PASS
  and no run can start without its own deploy. A later main push can
  still deploy staging mid-journey (deploy releases its job-level group
  when the deploy finishes while E2E serializes separately) — that
  timing fails the closing build-identity re-read instead of passing
  green (see below). Manual dispatch and the
  6-hour schedule run the synthetic smoke only (monitoring, not
  post-deploy promotion evidence); dispatch defines no `post_deploy`
  input, so it cannot mint that evidence by hand.
  Missing `STAGING_BASE_URL` → step-level skip with notice (pre-deploy).
  Both jobs require the Access pair in preflight before activating tests;
  configured staging never skips for missing credentials. The full journey
  additionally runs the evidence gate
  (`e2e/scripts/enforce-journey-evidence.mjs`): a configured run with no
  junit evidence, zero journeys ran, every journey skipped, more skips than
  the two documented `test.fixme` legs (player queue order, recording_mbids),
  any skip outside that allow-list, any failure, or a missing `QA_AUTH_TOKEN`
  fails the job instead of passing silently. Failure artifacts
  are kept 7 days.
- `e2e-lint` inside `ci.yml` (part of `ci-ok`) — `npm ci`, Playwright
  chromium install (`--with-deps`), `tsc --noEmit`,
  helper/workflow self-tests, `playwright test --list`, and the
  traceability-matrix check (`docs/m1-exit-traceability.md` must cite every
  listed spec). Runs on every PR
  with no staging needed. Offline tests execute both checked-in preflights
  and the aggregate gate with dummy inputs; failures, cancellations and skips
  of required jobs make `ci-ok` red.
- **Prod promotion gate (required integration):** the deployment lane must
  require the post-deploy full-journey result (the `post-deploy-e2e` job in
  the same `deploy.yml` run as the successful `staging-web` deploy) and
  block promotion on failure. Sequencing after the deploy is wired, and the
  full journey now carries same-build evidence: the opening
  `build-identity.spec.ts` leg and the closing
  `zz-build-identity-final.spec.ts` leg (which sorts last under the
  serial config) both compare `GET /api/build` (baked at build time)
  with the promotion SHA `deploy.yml` passes as `expected_sha` from
  `github.sha` — never a source checkout — through one shared helper, so
  a green run proves the whole journey ran against the promoted build.
  Blocking a later production promotion on that result, and live
  enforcement, are still acceptance prerequisites of that later lane.

## Evidence

On failure CI sanitizes then uploads `test-results/` only (7 days, junit).
Raw traces and screenshots are disabled: tracing records request
headers/bodies and screenshots can expose account/provider information. CI
uses the junit reporter only — the html reporter embeds step fill values
(including the BYOP password) as a base64 zip in
`playwright-report/index.html`, so it is never published. Sanitization is
still required with traces off: Playwright (`1.64.0` in
`e2e/package.json`) writes
`test-results/**/error-context.md` (an ARIA snapshot rendering filled input
values, including the BYOP password) — the workflow deletes
error-context.md plus the whole `playwright-report/` dir before upload, and
offline self-tests run a real failing fill under the CI reporter config,
apply the exact checked-in sanitize blocks, and assert the marker password
is absent from every uploaded file including base64-decoded payloads. Do not
enable traces/screenshots or the html reporter for credential-bearing runs
without a verified redaction path. A QA PASS verdict cites the green
post-deploy run URL (the `deploy.yml` run whose `staging-web` succeeded
and whose `post-deploy-e2e` full journey passed) + the deployed head SHA
(the promotion SHA proven equal to `GET /api/build` at both the start
and the end of the journey by the build-identity opening + closing
specs) — never a scheduled/manual synthetic run, which
is monitoring only; NEEDS WORK cites the failing spec + step. Every spec that branches on the test-account seam records a
bounded `signup-path` test annotation — exactly `public-ui-signup` or
`api-minted-account`, with no email, account id, token or provider data.
Both staging jobs publish those labels to the run's step summary on every
configured run (pass or fail), because green runs upload no artifacts and
the list reporter does not print annotations; the sanitized junit upload
stays failure-only (7 days). The full journey's evidence gate publishes the
ran / skipped / failed counts beside those labels on every configured run,
so a PASS always names how many journeys actually ran. A seam-backed (`api-minted-account`) run
never counts as proof of the public UI signup.
