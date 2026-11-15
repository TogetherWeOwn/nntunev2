# NNTune v2 — E2E + staging smoke harness

Playwright suite covering the venture slice on **staging**: signup →
library → playback with Range seeks → usage meter → plan-limit
enforcement. Today the playback, Range, meter and plan-limit legs are
parked by name (user NZB ingest is retired; see the retired-leg notes
below) and run only once admin-ingested tracks exist, so they are not live
evidence yet; parking does not waive the catalogue task's real journey
gate. Plus a fast synthetic smoke (`health.spec.ts`) that doubles
as the post-deploy monitor.

Public repo: **secret names only, never values.** Values live in GitHub
Actions secrets (CI) or shell exports (local).

## Activation progression

1. **Pre-deploy (now):** `STAGING_BASE_URL` is unset. The synthetic smoke
   (schedule/dispatch) skips with a notice, and the `e2e-lint` CI job still
   typechecks and lists the specs so the harness cannot rot silently. PRs and
   `main` stay green only while the deploy gate keeps `staging-web` from
   running: once `staging-web` deploys, the post-deploy full journey fails
   closed on an unset `STAGING_BASE_URL` instead of skipping, so a skipped
   journey can never become promotion evidence. A skipped smoke run is not
   acceptance evidence either: it proves nothing about staging.
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
  in rendered pages. `/signup`'s real submission, ingest/library/stream/usage
  endpoints, and usage-meter enforcement still depend on their venture
  slices; the journey specs expose those missing backend behaviors directly.

## Track interaction hooks

Track menus and selection add hooks without renaming the existing library,
queue, audio or metering contracts. `helpers/contract.ts` registers
`track-more`, `favorite-more` and `track-selection-toolbar` on the library, and
`queue-more`, `queue-drop-target` and `queue-selection-toolbar` on the shared
queue panel. `track-context-menu` belongs to the shared top-layer menu;
only an open menu renders it. Scope row actions by the existing stable
`data-id` (library) or `data-track-id` (queue), never by row position.
The queue selection toolbar renders only while tracks are selected.

`apps/web/src/lib/contract.test.ts` checks both route and shared-component
sources for these declarations. Browser coverage lives in
`apps/web/tests/track-interactions.spec.ts`; it complements rather than
replaces the existing range-selection and playback journey suites.

Queue insertion/reordering and Select remain metadata-only. The separate
Play now menu action is an explicit transport gesture: it hydrates only the
selected track, attempts playback once, and reports blocked or streamless
outcomes without retrying. Batch Play now arranges the whole displayed selection,
including the old current track when selected. Queue toolbar moves and Play next
retain queue order even when checkboxes were activated in reverse order.
Later queue/auth changes invalidate pending playback feedback. Done restores stranded focus to a surviving control rather than the
page body; group drops resolve insertion boundaries outside the moving IDs.
Copy link is inline rather than a one-choice submenu. The playlist slot
is reserved and disabled with a "Not available yet" hint; backend routes
and a playlist picker are pending. Arrow keys can discover the slot, but
activation changes nothing and type-ahead skips it. Touch queue
grips autoscroll the nearest scrolling pane while held at its edge; drop,
Escape, hidden-page and unmount cleanup stop the animation.
Pending long-presses cancel on page blur, hiding, scroll or pointer release
outside the row; a hidden page also invalidates native drag tokens before
any later drop. Unit tests cover gesture lifecycle cleanup, and browser
coverage includes interruption cases plus open-menu/batch-selection axe scans
at desktop and narrow widths. Those scans reject serious/critical findings;
they do not replace manual keyboard, touch or assistive-technology review.

These UI changes follow **AGENTS.md — Clean-room, YouTube and MetaBrainz
guardrails**: original presentation, existing semantic colours, no changes to
embedded video or catalogue licensing. Offline interaction tests are not
staging ingest→play→meter acceptance evidence.

## Player contract migration (app-shell redesign, design spec v1)

The custom player bar replaces the native `<audio controls>` timeline this
suite used to click. The migration is already done on the test side, so the
player-bar build changes product code only:

- **Kept hooks** (`helpers/contract.ts` `TESTIDS`): `library-item`,
  `audio-player` (the `<audio>` element — it loses its native `controls`),
  `usage-meter` (rail footer XOR library page — never two in the DOM at
  once).
- **Mapped hooks**: `now-playing` and `player-next` move from the demo/queue
  UI to the player bar's identity cluster and Next transport; the library
  page's `library-now-playing` readout retires into `now-playing`.
- **New hooks**: `player-bar`, `player-play-pause`, `player-seek` (a native
  `input[type=range]` with slider semantics and an `aria-valuetext` like
  "1:23 of 3:45"), `player-volume`, plus shell hooks `shell-topbar` (both
  shells), `shell-rail`, `shell-tabbar`, `mini-player`. These are now in
  `TESTIDS` in `helpers/contract.ts`, with their production component
  sources pinned by the web contract test. The custom transport and its
  seek helper activation ship together.
- **Dual-generation driver** (`helpers/player-ui.ts`): seeks press the
  `player-seek` slider at the proportional position when it is visible and
  fall back to the native timeline while `controls` are still rendered;
  play/pause uses `player-play-pause` or the native toggle. Neither path
  ever assigns `currentTime`. The pure verdict (surface choice, proof
  region, resume floor, pointer geometry) is pinned offline by
  `tests/player-ui.spec.ts`. The Media Session helper needed no change — it
  was already player-agnostic; the library page must keep applying Media
  Session metadata when the real player graduates, which the live spec
  already pins.
- **Staging journey impact**: none until the bar ships — the fallback is
  the exact native path the M1 journey uses today. When the bar ships, the
  same journey drives the slider with the same proofs (paused seek +
  `seeked` event + resume floor), so row 4 of `docs/m1-exit-traceability.md`
  keeps its meaning.

## Bot-contract conformance vectors (feeds B8)

The TWO-bot consumption contract (spec section 5) is banked as a JSON
fixture corpus plus an offline checker, ahead of the B-chain runtime work:

- Corpus: `e2e/fixtures/bot-contract/` (`grants.json`, `signed-url.json`,
  `profile-matrix.json`, documented in its `README.md`).
- Checker: `e2e/helpers/bot-contract.ts` — import `checkGrantVector`,
  `checkSignedUrlVector`, `checkMatrixRow` from a staging spec.
- Pins: `e2e/tests/bot-contract.spec.ts` (runs in `npm run test:unit`,
  no staging): the checker passes the corpus, fails each mutated shape by
  name, and the stable IDs + typed codes match their pinned sources.

B8 feeds the signed-URL vectors to staging and compares the wire verdicts
against the matrix expectations. Vector ids (`botgrant-NNN`, `boturl-NNN`,
`botmx-NNN`) are stable and citable; fixtures name secrets only.

## Run in GitHub Actions

Push the PR branch and open a draft PR while work is incomplete. The
`e2e-lint` job in [CI](../.github/workflows/ci.yml) runs harness typechecking,
offline self-tests and spec discovery as part of `ci-ok`; these are not a
local CI-mirror checklist. Full staging journeys run only through the
post-deploy Actions path described below, not from a local shell.

Local tests are limited to focused reproduction of one failing test from
an Actions job. Select that test explicitly rather than invoking the full
`test:unit`, `e2e:staging` or smoke suite. Heavy focused reproduction must
use the guarded `boat` PATH wrapper with `--no-env`, source-only export and
verified cleanup; do not forward staging or host credentials. A fallback
cannot authorize a full suite or heavy raw-host run. If the reproduction
cannot meet these limits, use Actions or route its actual prerequisite.
See [verification policy](../CONTRIBUTING.md#verification-github-actions-first).

If `NODE_ENV=production` is exported in your shell, install with
`npm install --include=dev` — otherwise npm silently omits the dev
dependencies this harness needs.

Toolchain: CI runs Node 26 for this harness (both jobs in
`.github/workflows/e2e-staging.yml` and the `e2e-lint` job in `ci.yml`,
kept in sync with the `@types/node` major in `e2e/package.json`;
same Playwright pin there too). The app itself uses Node 24 + pnpm
(`NODE_VERSION`/`PNPM_VERSION` in `ci.yml`) and is unrelated to this
npm-based harness (`e2e/package-lock.json`).

## Environment

| Name | Required | Purpose |
|---|---|---|
| `STAGING_BASE_URL` | yes (post-deploy) | `https://` staging origin. Unset → the synthetic smoke skips with notice; the post-deploy full journey fails closed. |
| `CF_ACCESS_CLIENT_ID` / `CF_ACCESS_CLIENT_SECRET` | whenever staging is configured | QA service-token pair. Both absent or either absent → fail closed before requests. |
| `QA_AUTH_TOKEN` | optional | Bearer for `POST /api/test-accounts` (staging-only seam) and `POST /api/test-codes` (staging-only emailed-code retrieval seam for `example.test` mailboxes — returns the issued code without minting). Absent → UI signup fallback, and the library-arrival spec (`signup-arrival.spec.ts`) skips locally / fails closed on CI. The dedicated public UI-signup case ignores both seams either way. |
| `SYNTHETIC_NNTP_HOST` / `_PORT` / `_USER` / `_TLS` | for provider 430 control | Real staging NNTP provider (a real service despite the `SYNTHETIC_` names — rename needs an operator; defaults: `synthetic-nntp`, `563`, `e2e`, TLS on). The port must be in the provider allowlist (`119` or `563`) and TLS-off is rejected — the harness fails closed on a transport the endpoint helper cannot represent. |
| `SYNTHETIC_NNTP_PASS` | for provider 430 control | No default — fail closed when missing. |
| `TEST_NZB_PATH` | retired | Runtime path of the former secret-held real-NZB staging fixture. That leg is retired under the CC0-only fixture rule (`docs/fixture-audio-licences.md`): no workflow sets this anymore, staging ingests no NZB, and the legs that needed an uploaded track skip by name until admin ingest can seed a curated track. Never set. |
| `REAL_NZB_MODE` | retired | Former journey-gate mode flag for the retired real-NZB leg. No workflow sets it; the gate's standard twelve-skip budget governs every run. |
| `SHARED_CATALOGUE_PUBLISHED` | after admin publish | Set to `true` only after an admin consumer publishes a curated matched release on staging. Before that, the named shared-catalogue case is a budgeted `PARKED:` skip, not acceptance. Once true, a skip fails the gate; a missing case fails configured runs in either mode. |
| `NNTUNE_TEST_NZB_XZ_B64_1` / `_2` | retired | Former NZB fixture secrets for the retired staging leg. No workflow references them; the operator deletes them. |

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
- Test-account sessions minted by the seam travel as the `__Host-`
  session cookie in browser pages (planted by `applyAccountToPage` — the
  only credential session routes such as `POST /v1/vault/*` accept) and as
  `Authorization: Bearer <session>` in account-specific API calls (contract
  owned by the auth card — `helpers/accounts.ts` names it).
- Test accounts are throwaway (`e2e+<stamp>@example.test`); the API seam is
  tried first, a form-submission UI signup (emailed-code step) second (each journey file signs
  up its own identity — Playwright isolates module state per file, so there
  is no cross-file account sharing). Independently of that choice,
  `signup.spec.ts` carries a dedicated public UI-signup case
  (`submitPublicUiSignup`, null identity): it always submits the real
  `/signup` form with a unique synthetic mailbox and requires the
  emailed-code step with no inline error, even when `QA_AUTH_TOKEN`
  is set, so a seam-backed run still proves public UI form submission.
  Form-to-billing arrival is asserted by `signup-arrival.spec.ts`: the
  code-plus-passkey finish retrieves the code from the staging-only
  `POST /api/test-codes` seam, which is live on staging
  (see row 1 of `docs/m1-exit-traceability.md`). Once `QA_AUTH_TOKEN` is set the seam is
  required infrastructure: transport errors, non-2xx and non-JSON mint
  responses throw instead of collapsing into a silent skip, and API-driven
  steps (`requireApiAccount`) refuse to run anonymously. User NZB ingest is
  retired (TWO-1630), so no journey leg completes a provider wizard or
  uploads at `/library/import` anymore. The over-limit check needs the seam
  to support `{plan:"overlimit"}` (metering card): the stream leg mints its
  own capped account (never the journey identity) and runs AFTER the browser
  legs in its isolated group, so a red-by-design capped-account failure
  skips no other verdict. The stream
  leg requires the seam to pre-seed a streamable track on its capped account:
  there is no user-ingest path left that could fill it, so the leg currently
  skips by name with the other parked legs (below) until admin ingest can
  seed a curated track. When it runs, an unavailable over-limit seam fails
  instead of skipping — locally and on CI — so plan-limit enforcement can
  never pass unexercised. Each leg asserts the
  full refusal verdict (`assertPlanLimitRefusal` in `helpers/contract.ts`):
  402/429 status, limit + reset headers (429 additionally requires
  `retry-after`), the status-consistent limit code (`plan_limit_exceeded` on
  402, `rate_limited` on 429) with a present, status-consistent retryable
  flag, the upgrade CTA pointer (a matching key whose value points at
  `/pricing`), and no secret leakage.
- Staging real-NZB mode is retired: the post-deploy journey used to ingest
  a secret-held real NZB rebuilt from repo secrets into `RUNNER_TEMP`.
  Under the CC0-only fixture rule (`docs/fixture-audio-licences.md`) no
  workflow stages or ingests an NZB anymore — there are no checked-in NZB
  fixtures (TWO-1169) and no NZB fixture secrets are referenced, so the
  operator can delete them without failing a check. Live legs are signup,
  library arrival, returning-user login and the provider 430 control; the
  user-NZB legs (ingest completion in both spec files, reload, Range,
  meter, playback, seek, Media Session, queue order) plus the stream
  plan-limit refusal leg — which needs a pre-seeded streamable track —
  skip by name under TWO-1635. User NZB ingest is retired, so they wait
  for admin ingest to seed a curated track on staging (RAR-set legs
  additionally keep their TWO-1174 park). The journey gate budgets the
  standard budget of twelve skips (fixme, the ten parked legs and the
  shared-catalogue park until publication); any failure or off-list skip
  still fails the run. The shared park is never counted as passed. The dormant
  resolver (`helpers/real-nzb-fixture.ts`), the truncation script
  (`scripts/prepare-real-nzb.mjs`) and the gate's `REAL_NZB_MODE` branch
  stay only until the parked specs un-park or are deleted; owner rule
  (public repo) still holds — no NZB bytes, message-IDs, subjects, file
  names or segment lists in the repo or public logs.
- Provider budget + 430 control: the staging NNTP provider is a shared
  account with production, so each run holds at most a trickle of
  connections. The workflow concurrency group (`e2e-staging`,
  cancel-in-progress: false) serializes provider runs, and
  `E2E_NNTP_MAX_CONNECTIONS` (default 2, hard max 10 —
  `helpers/provider-budget.ts`) caps each run so 8+ connections stay free.
  The provider 430 control reads the run budget through the same helper, so
  the live proof never exceeds the run cap. Legs fetch only the segments
  they need. `tests/provider-430-control.spec.ts`
  is the live proof that costs almost nothing: one direct NNTP connection
  asserting a minted never-posted probe ID answers 430 — 502 fails fast
  with no retry (Playwright retries disabled for the file), the socket
  closes in a `finally`, and counts-only evidence (peak connections, bytes
  fetched) attaches to the run for the PR evidence comment.

## CI

- `.github/workflows/e2e-staging.yml` — no `push` trigger. The full
  journey runs only as a reusable-workflow call from `deploy.yml`'s
  `post-deploy-e2e` job, which `needs` the same run's `staging-web` and
  `staging-api` jobs, so a skipped/failed job cannot produce a full-journey
  PASS, no run can start without its own web deployment, and the journey
  never runs against the previous api mid-deploy. A green `staging-api`
  job is not an api deployment: when staging is unconfigured it succeeds
  with `outcome=unconfigured`, which `deploy.yml` passes as
  `api_deploy_outcome` so build identity reports the api as not redeployed
  instead of claiming web=API=head. A later main push can
  still deploy staging mid-journey (deploy releases its job-level group
  when the deploy finishes while E2E serializes separately) — that
  timing fails the closing build-identity re-read instead of passing
  green (see below). Manual dispatch and the
  6-hour schedule run the synthetic smoke only (monitoring, not
  post-deploy promotion evidence); dispatch defines no `post_deploy`
  input, so it cannot mint that evidence by hand.
  Missing `STAGING_BASE_URL` → the synthetic smoke skips with notice
  (pre-deploy); the post-deploy full journey fails in preflight, because a
  skipped journey after a real `staging-web` deploy would otherwise pass
  green and be accepted as promotion evidence.
  Both jobs require the Access pair in preflight before activating tests;
  configured staging never skips for missing credentials. The full journey
  additionally runs the evidence gate
  (`e2e/scripts/enforce-journey-evidence.mjs`): a configured run with no
  junit evidence, zero journeys ran, every journey skipped, more skips than
  the one remaining `test.fixme` leg (recording_mbids),
  any skip outside that allow-list, any failure, or a missing `QA_AUTH_TOKEN`
  fails the job instead of passing silently. The named populated-catalogue leg
  in `tests/catalogue.spec.ts` has a budgeted `PARKED:` skip while
  `SHARED_CATALOGUE_PUBLISHED` is not `true`: the configured full-journey gate
  can be green, but that is not populated-catalogue acceptance. With the variable
  `true`, a skip fails the gate. A missing, duplicated or failed case fails in
  either mode. After the admin consumer publishes
  a curated matched release on staging, set the repository variable
  `SHARED_CATALOGUE_PUBLISHED=true` and rerun the post-deploy journey. The leg
  must then observe identical nonempty, mirror-enriched libraries in two distinct
  fresh QA-seam accounts; empty data cannot pass merely by setting the variable.
  Failure artifacts are kept 7 days.
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
  The same helper reads the API Worker's `GET /healthz` `build_sha`
  and logs `web=… api=… head=…`. When `staging-api` deployed this run
  (`API_DEPLOY_OUTCOME=deployed`, or unset), the API must equal the
  promotion SHA too. When it was `skipped`/`unconfigured`, the log and a
  `api-not-redeployed` annotation state that web=API=head is not claimed.
  Blocking a later production promotion on that result, and live
  enforcement, are still acceptance prerequisites of that later lane.

## Evidence

For PR CI, record the full reviewed head SHA plus exact-head Actions run
and `e2e-lint`/`ci-ok` job URLs and conclusions, following
[the evidence contract](../CONTRIBUTING.md#evidence-for-review-and-release).
Reviewers and the Release Manager accept those links as CI evidence, not
local full-suite transcripts. They do not replace the same-deployed-build
journey, authorized production promotion, migration or rollback gates.

On failure CI sanitizes then uploads `test-results/` only (7 days, junit).
Raw traces and screenshots are disabled: tracing records request
headers/bodies and screenshots can expose account/provider information. CI
uses the junit reporter only — the html reporter embeds step fill values
(including filled credential values) as a base64 zip in
`playwright-report/index.html`, so it is never published. Sanitization is
still required with traces off: Playwright (as pinned in
`e2e/package.json`) writes
`test-results/**/error-context.md` (an ARIA snapshot rendering filled input
values) — the workflow deletes
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
