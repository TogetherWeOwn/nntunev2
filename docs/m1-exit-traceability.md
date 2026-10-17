# M1 exit traceability matrix

Maps every M1 exit step to the `e2e/` spec(s) that cover it, or records an
explicit gap. M1 exits when the secure staging user journey
(signup → BYOP → ingest → play/seek) passes on an exact-head staging deploy
with Access enabled, and rollback evidence is recorded.

Re-verified against the `e2e/` tree on 2026-10-17 with the public
UI-signup case and the browser-playback spec merged:
`npm --prefix e2e run e2e:list` lists 203 tests in 12 files. Line numbers
below are from that head; re-verify with the commands at the end of this
document whenever a spec is added, renamed, or retitled, and update this
matrix in the same PR. CI enforces the file and title citations on every
PR (the traceability step in the `e2e-lint` job); line numbers stay a
manual re-verify item.

## Reading the statuses

- **COVERED** — a live spec asserts the step end to end.
- **PARTIAL** — part of the step is asserted; the missing part is named.
- **GAP** — no spec covers the step. The gap is a statement, not a spec:
  closing it is separate work.

One global caveat applies to every COVERED/PARTIAL row: all live specs
(`health`, `signup-byop`, `ingest-play-meter`) skip while
`STAGING_BASE_URL` is unset (file-level skip plus in-test guards, see
`e2e/README.md` "Activation progression"). Until staging is configured, a
green run is skip-green and is **not** M1 exit evidence. Once staging is
configured, the harness fails closed instead of skipping.

## Matrix

| # | M1 exit step | Status | Covering spec(s) | What is actually asserted |
|---|---|---|---|---|
| 1 | Signup | PARTIAL | `e2e/tests/signup-byop.spec.ts:91` "public UI signup succeeds even when the test-account seam is configured" (dedicated form case); `e2e/tests/signup-byop.spec.ts:32` "signup creates an account and lands on the BYOP wizard" (separate API-minted/fallback case); same form flow shared as fallback in `e2e/tests/ingest-play-meter.spec.ts:62` | The dedicated case never mints: it applies a null identity (Access pair only) and submits the real `/signup` form via `submitPublicUiSignup` (`e2e/helpers/accounts.ts:423`) — fill a unique synthetic mailbox, submit, no inline signup error, emailed-code step reached — so it runs the same way whether or not `QA_AUTH_TOKEN` is set. API-minted coverage stays separate in the `:32` case. The separation plus the no-wizard-assertion rule are pinned offline by `e2e/tests/workflows.spec.ts:192` (no mint in the dedicated case or the shared form helper; the helper must assert the `magic_code` step, never the wizard). Diagnostics name no mailbox or token values. **Gap:** the real page only reaches `/onboarding/byop` after the emailed code plus a passkey finish — no spec proves wizard arrival via the form. Closing it needs a staging-only code-retrieval seam (return the issued token for an `example.test` mailbox without minting the account) plus a WebAuthn virtual authenticator in the harness. |
| 2 | BYOP connect | COVERED | `e2e/tests/signup-byop.spec.ts:62` via `completeByopWizard` (`e2e/helpers/accounts.ts:340`); re-exercised in `ingest-play-meter.spec.ts:74` | Fills host/port/user/password for the synthetic NNTP endpoint, requires test-connection to report success within 60 s, saves, and requires landing on `/library`. Fails closed when `SYNTHETIC_NNTP_PASS` is unset rather than submitting an empty credential. |
| 3 | Ingest | COVERED | `e2e/tests/ingest-play-meter.spec.ts:47` "NZB fixture ingests and appears in the library" | Uploads `e2e/fixtures/synthetic.nzb` at `/library/import`, requires the import status to reach complete/ready/done within 240 s, then requires the track to be visible in `/library`. The ingesting identity completes the BYOP wizard first, because the ingest worker probes through the user's provider connection. |
| 4 | Play / seek | PARTIAL | Seek (API): `e2e/tests/ingest-play-meter.spec.ts:162` "streaming answers byte Ranges with 206 + Content-Range". Play: `e2e/tests/ingest-play-meter.spec.ts:367` "synthetic audio actually plays in the browser" | Two API Range GETs (`bytes=0-8`, `bytes=9-17`) must return 206 with `Content-Range` (and length 9 for the first), as the ingesting account. Playback is asserted separately in the browser at the end of the serial chain (after the over-limit test, which mints its own capped account, so a playback failure cannot skip the meter or plan-limit specs): selecting the staged synthetic library track (and the player's own play control when selection only loads it) must put the `<audio>` element into the playing state with an advancing position and no page error — a 206 alone cannot pass (`e2e/helpers/playback.ts`, pinned offline by `e2e/tests/playback.spec.ts`). Subject to the global caveat above: while staging is unconfigured the spec skips, which is not exit evidence, and it will fail by name until the library player is wired to the selected track. **Remaining gap:** no spec asserts a player-UI seek (dragging/keyboard on the seek bar); only API Range seeks are covered. |
| 5 | Access enforcement | PARTIAL | With-credentials side: every live spec, e.g. `assertNoAccessWall` in `e2e/tests/health.spec.ts:32`, `:70`, `:92`; fail-closed config in `e2e/playwright.config.ts` (pair validated at load), pinned offline by `e2e/tests/helpers.spec.ts:73` and both job preflights in `e2e/tests/workflows.spec.ts:46` | A green live run proves the journey passes **with Access enabled** and a valid QA service token: any wall response (401/403/302 with Access markers) fails the step by name instead of masquerading as an app failure, and configured staging without the full credential pair fails before any request. **Gap:** no spec sends a request *without* Access credentials and asserts Access blocks it, so enforcement against anonymous traffic is not proven by e2e; only wall *detection* is unit-tested, against synthetic responses (`helpers.spec.ts:123`). |
| 6 | Rollback evidence | GAP | none in `e2e/` | Rollback evidence is procedural, not a spec: `docs/ops.md` "Staging promotion checklist" requires a recorded rollback pointer (last good staging SHA + its push-triggered `deploy.yml` run with `staging-web` conclusion `success`), rollback by re-running all jobs on that run, and a successful `staging-web` rerun plus staging health. Workers are not deployed by that workflow and need their own rollback evidence; the evidence a future Worker lane must retain is defined in `docs/ops.md` "Future Worker deploy lanes: required rollback evidence", but no such lane exists yet, so this row stays GAP and no Worker deploy or rollback is claimed. e2e contributes only the reusable post-rollback health smoke (row 7). |
| 7 | Exact-head staging deploy (exit wrapper) | PARTIAL | `e2e/tests/health.spec.ts:19` (`/api/health`), `:54` (`/api/ready`), `:78` (app shell behind Access) | The smoke proves the deployed app answers `ok:true` in under 2 s, promotion readiness (200 JSON `{ok:true}` only; 404 and 503 — even with `Retry-After` — fail, pinned offline by `e2e/tests/readiness.spec.ts` via `e2e/helpers/readiness.ts`), and the app shell behind Access. **Gap:** no spec ties the deployed build to the exact commit SHA — `version` is asserted only when the app ships it and is a package version, not a build identity. Per `docs/ops.md`, same-build evidence must not be claimed until a deployed-build identity is exposed and compared with the promotion SHA. Sequencing is established: `e2e-staging.yml` has no push trigger, and `deploy.yml`'s `post-deploy-e2e` job calls it only after the same run's `staging-web` succeeds, so the E2E run cannot race its deploy — but that ordering still does not prove which build is serving. |
| 8 | E2E evidence pass (exit wrapper) | COVERED as process | `.github/workflows/e2e-staging.yml` (synthetic smoke + full journey jobs), called post-deploy by `.github/workflows/deploy.yml`; verdict format in `e2e/README.md` "Evidence"; sequencing graph pinned offline in `e2e/tests/workflows.spec.ts` "deploy calls e2e-staging only after a successful push staging-web (evaluated)" and "full journey runs only for the post-deploy call (evaluated)" | Both jobs run the fail-closed preflight and upload sanitized junit-only failure artifacts for 7 days. The full journey runs only on the post-deploy call after a successful same-run `staging-web` deploy — a skipped/failed deploy cannot produce a full-journey PASS — while scheduled/manual runs are synthetic monitoring only. A PASS verdict cites the green post-deploy run URL plus the source head SHA (not a build identity). Subject to the global caveat above: while staging is unconfigured both jobs skip with notice, which does not count as evidence. |

## Coverage that is not an M1 exit step

Listed so it is not mistaken for (or double-counted as) M1 coverage:

- Usage meter increase + plan metadata (`ingest-play-meter.spec.ts:212`)
  and over-limit refusal (`ingest-play-meter.spec.ts:279`, fail-closed on
  CI when the over-limit seam is unavailable) belong to the metering
  milestone, not the M1 exit list.
- `e2e/tests/helpers.spec.ts` and `e2e/tests/workflows.spec.ts` are
  offline self-tests (wall detection, credential scope, workflow
  preflights, artifact sanitization). They run in the `e2e-lint` CI job on
  every PR and keep the harness honest, but produce no staging evidence.
- `e2e/tests/account-identity.spec.ts` is the offline self-test for the
  test-account seam helpers (mint-schema validation, the account guard,
  credential headers, and browser credential scoping, all against fake
  seam responses). It keeps the identity plumbing the live specs share
  honest, but produces no staging evidence.
- `e2e/tests/byop-transport.spec.ts` and
  `e2e/tests/byop-tls-setting.spec.ts` pin the synthetic fixture transport
  contract offline (port allowlist, TLS-on enforcement, fail-closed wizard
  behavior against a mocked page). They guard the BYOP helper, not the
  live provider connection.
- `e2e/tests/redirect-diagnostics.spec.ts` pins offline that credentialed
  redirect errors never echo URL components or credential values
  (loopback fixtures plus a fake request context). Diagnostic hygiene,
  not journey coverage.
- `e2e/tests/security-headers.spec.ts` is the staging probe that the live
  app-shell document response still carries the security headers and
  nonce-bearing CSP (static posture pinned offline by
  `apps/web/tests/`). Staging hardening, not journey coverage.
- The `apps/web/tests/` offline specs below run in the `web-e2e` CI job
  on every PR with all network traffic intercepted and fulfilled locally.
  They pin product behavior outside the M1 exit list and produce no
  staging evidence:
  - `apps/web/tests/player-queue.spec.ts` — pins the 3-track queue
    play-through (next/prev/track-end), queue panel add/clear/reorder
    with in-session order persistence, and MediaSession metadata plus OS
    next/prev handlers following the current track.
  - `apps/web/tests/player-seek.spec.ts` — pins the demo audio/seek
    contract: private-cached audio vs public-immutable artwork, byte-Range
    206 handling, the artwork fallback chain, and 10x rapid seeks never
    wedging the player.
  - `apps/web/tests/player-browser-contract.spec.ts` — pins real-browser
    playback advancing, the seek storm ignoring offset-ignoring and
    mismatched Content-Range responses, and artwork staying understandable
    when its image and placeholder both fail.
  - `apps/web/tests/pricing-selection.spec.ts` — pins `/pricing`
    plan-selection serialization: at most one checkout POST in flight,
    re-entry refused while held, ordinary failure restoring choices, and
    success/401/402/429 routing unchanged.
  - `apps/web/tests/byop-reauth.spec.ts` — pins stale-session provider
    save directing the user to sign in again, and successful passkey
    reauthentication saving once with controls disabled while pending.
  - `apps/web/tests/auth-magic.spec.ts` — pins the magic-link landing
    never redeeming on load (explicit confirm redeems once, then names the
    account), swap-guard sign-out recovery, paywall token preservation,
    and logout failure/401 handling.
  - `apps/web/tests/library-cache.spec.ts` — pins the library snapshot
    cache contract: offline retry rendering the stale snapshot with a
    badge, 401/auth-transition invalidation, in-flight response discard,
    last-good preservation, and blocked-storage behavior.
  - `apps/web/tests/account-security-library-cache.spec.ts` — pins
    sign-out invalidating library snapshots across tabs (logout, revoke-all,
    and confirmed 401 clear stored snapshots; 500/transport failure claims
    nothing and clears nothing).

## Re-verifying this matrix

```sh
find e2e/tests -name '*.spec.ts' | sort
npm --prefix e2e run e2e:list   # spec files + test titles Playwright sees
find apps/web/tests -name '*.spec.ts' | sort
(cd apps/web && pnpm exec playwright test --list)   # web spec files + titles
grep -n "test('" e2e/tests/*.spec.ts apps/web/tests/*.spec.ts
```

Every path and title cited above must appear in that output. If a cited
spec moves or a new spec closes a PARTIAL/GAP row, update the row in the
same PR; a stale path here is a failed review, per the matrix's own test
plan.

CI enforces this on every PR (the traceability step in the `e2e-lint` job,
offline, no staging credentials): it captures `e2e:list` output and runs
`python3 .github/scripts/check_traceability.py`, which fails on any spec
file the matrix does not cite, any cited spec or `e2e/` path that no
longer exists, and any double-quoted title matching no listed test.
Reproduce a red run locally with the same two commands:

```sh
npm --prefix e2e run e2e:list > /tmp/e2e-list.txt 2>&1
python3 .github/scripts/check_traceability.py --matrix docs/m1-exit-traceability.md --list-output /tmp/e2e-list.txt
```

Matrix authoring rules for the checker: cite every spec file in backticks
(including offline self-tests, listed in the section above); reserve
double quotes for exact test titles; `:line` suffixes are informational
and are not checked.
