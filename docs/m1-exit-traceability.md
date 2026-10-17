# M1 exit traceability matrix

Maps every M1 exit step to the `e2e/` spec(s) that cover it, or records an
explicit gap. M1 exits when the secure staging user journey
(signup → BYOP → ingest → play/seek) passes on an exact-head staging deploy
with Access enabled, and rollback evidence is recorded.

Verified against the tree at `750dc91`, on 2026-10-17:
`npm --prefix e2e run e2e:list` lists 207 tests in 13 files, and
`(cd apps/web && pnpm exec playwright test --list)` lists 86 tests in
18 files. Line numbers below are from that head; re-verify with the
commands at the end of this document whenever a spec is added, renamed,
or retitled, and update this matrix in the same PR.

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
| 4 | Play / seek | PARTIAL | Seek (API): `e2e/tests/ingest-play-meter.spec.ts:162` "streaming answers byte Ranges with 206 + Content-Range". Play: `e2e/tests/ingest-play-meter.spec.ts:367` "synthetic audio actually plays in the browser". Player-UI seek: `e2e/tests/ingest-play-meter.spec.ts:524` "player UI seek moves the active audio position and playback continues" (last in the serial chain) | Two API Range GETs (`bytes=0-8`, `bytes=9-17`) must return 206 with `Content-Range` (and length 9 for the first), as the ingesting account — a lower-level stream control, not UI evidence. Playback is asserted separately in the browser after the over-limit test (which mints its own capped account, so a playback failure cannot skip the meter or plan-limit specs): selecting the staged synthetic library track (and the player's own play control when selection only loads it) must put the `<audio>` element into the playing state with an advancing position and no page error — a 206 alone cannot pass (`e2e/helpers/playback.ts`, pinned offline by `e2e/tests/playback.spec.ts`). The UI-seek spec clicks the ingested track, pauses via the native toggle, clicks the timeline past its middle (never assigning `currentTime`, never a test-issued Range GET), and requires the still-paused position the timeline renders to land in the 30–95% region with a `seeked` event fired — while paused only the seek can move it — then resumes via the toggle and requires the position to keep advancing from the seek point (the resume floor is seeded with the seek position and takes the min over `timeupdate` events plus every poll sample, so a restart-from-zero replay fails it), with no media or page errors. Paused seek is what lets it run against the pinned 210 ms synthetic WAV. Subject to the global caveat above: while staging is unconfigured both specs skip, which is not exit evidence, and both fail by name until the library player is wired to the selected track; the meter test still treats the player as an additional signal only (`ingest-play-meter.spec.ts:248`). |
| 5 | Access enforcement | PARTIAL | With-credentials side: every live spec, e.g. `assertNoAccessWall` in `e2e/tests/health.spec.ts:32`, `:70`, `:92`; fail-closed config in `e2e/playwright.config.ts` (pair validated at load), pinned offline by `e2e/tests/helpers.spec.ts:73` and both job preflights in `e2e/tests/workflows.spec.ts:46` | A green live run proves the journey passes **with Access enabled** and a valid QA service token: any wall response (401/403/302 with Access markers) fails the step by name instead of masquerading as an app failure, and configured staging without the full credential pair fails before any request. **Gap:** no spec sends a request *without* Access credentials and asserts Access blocks it, so enforcement against anonymous traffic is not proven by e2e; only wall *detection* is unit-tested, against synthetic responses (`helpers.spec.ts:123`). |
| 6 | Rollback evidence | GAP | none in `e2e/` | Rollback evidence is procedural, not a spec: `docs/ops.md` "Staging promotion checklist" requires a recorded rollback pointer (last good staging SHA + its push-triggered `deploy.yml` run with `staging-web` conclusion `success`), rollback by re-running all jobs on that run, and a successful `staging-web` rerun plus staging health. Workers are not deployed by that workflow and need their own rollback evidence; the evidence a future Worker lane must retain is defined in `docs/ops.md` "Future Worker deploy lanes: required rollback evidence", but no such lane exists yet, so this row stays GAP and no Worker deploy or rollback is claimed. e2e contributes only the reusable post-rollback health smoke (row 7). |
| 7 | Exact-head staging deploy (exit wrapper) | PARTIAL | `e2e/tests/health.spec.ts:19` (`/api/health`), `:54` (`/api/ready`), `:78` (app shell behind Access) | The smoke proves the deployed app answers `ok:true` in under 2 s, promotion readiness (200 JSON `{ok:true}` only; 404 and 503 — even with `Retry-After` — fail, pinned offline by `e2e/tests/readiness.spec.ts` via `e2e/helpers/readiness.ts`), and the app shell behind Access. **Gap:** no spec ties the deployed build to the exact commit SHA — `version` is asserted only when the app ships it and is a package version, not a build identity. Per `docs/ops.md`, same-build evidence must not be claimed until a deployed-build identity is exposed and compared with the promotion SHA. Sequencing is established: `e2e-staging.yml` has no push trigger, and `deploy.yml`'s `post-deploy-e2e` job calls it only after the same run's `staging-web` succeeds, so the E2E run cannot race its deploy — but that ordering still does not prove which build is serving. |
| 8 | E2E evidence pass (exit wrapper) | COVERED as process | `.github/workflows/e2e-staging.yml` (synthetic smoke + full journey jobs), called post-deploy by `.github/workflows/deploy.yml`; verdict format in `e2e/README.md` "Evidence"; sequencing graph pinned offline in `e2e/tests/workflows.spec.ts` "post-deploy sequencing" | Both jobs run the fail-closed preflight and upload sanitized junit-only failure artifacts for 7 days. The full journey runs only on the post-deploy call after a successful same-run `staging-web` deploy — a skipped/failed deploy cannot produce a full-journey PASS — while scheduled/manual runs are synthetic monitoring only. A PASS verdict cites the green post-deploy run URL plus the source head SHA (not a build identity). Subject to the global caveat above: while staging is unconfigured both jobs skip with notice, which does not count as evidence. |

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
  The offline `e2e/tests/account-identity.spec.ts`,
  `e2e/tests/byop-tls-setting.spec.ts`,
  `e2e/tests/byop-transport.spec.ts`, and
  `e2e/tests/redirect-diagnostics.spec.ts` specs likewise pin harness
  contracts and produce no staging evidence:
  - `account-identity.spec.ts` — pins the account-identity contract:
    seam mint validation (no fabricated email, names-only errors), the
    account guard passing only on a nonblank minted session (the QA seam
    token alone never satisfies it), and per-account bearer headers with
    no cross-account or QA-token leakage.
  - `byop-tls-setting.spec.ts` — pins malformed `SYNTHETIC_NNTP_TLS`
    spellings (`false`/`0`/`no`/`true`/`yes`/`maybe`/blank) rejected with
    a names-only diagnostic instead of silently treated as TLS-on.
  - `byop-transport.spec.ts` — pins BYOP transport fidelity: the port
    allowlist (119/563), TLS-off fixture config rejected before any page
    interaction, the TLS checkbox explicitly set and verified (a missing,
    unusable, or unsettable control fails before any test/save
    submission), and the supported TLS-on transports keep working.
  - `redirect-diagnostics.spec.ts` — pins credentialed redirect
    diagnostics never echoing request/Location URL components or
    credential values, staying actionable (caller context, category,
    status, secret names only), with no-follow unchanged
    (`maxRedirects: 0`, no foreign-origin follow).
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
  - `apps/web/tests/account-security.spec.ts` — pins revoke-all asking
    for confirmation before revoking and redirecting after success,
    per-session sign-out, and failure/401 handling staying on the page
    with a notice.
  - `apps/web/tests/billing-signed-out.spec.ts` — pins a signed-out
    billing visitor reaching sign-in via the Sign in link, with no
    billing actions shown.
  - `apps/web/tests/library-duplicate-ids.spec.ts` — pins duplicate-id
    library responses rendering unique rows with no page error, refresh
    still working afterwards, the stale fallback staying unique, and a
    seeded duplicate snapshot never rendering before session
    verification.
  - `apps/web/tests/media-session-artwork-errors.spec.ts` — pins a
    MediaMetadata construction failure not stranding Media Session
    controls, surfacing no page error, and leaving the queue healthy
    (plus a no-failure positive control).
  - `apps/web/tests/paywall.spec.ts` — pins 402/429 limit responses
    showing the paywall modal with an upgrade CTA (no dead-end error, no
    duplicate inline alert) and firing the `usage_limit_paywall_shown`
    analytics event with no PII; the upgrade CTA closes the modal before
    navigating.
  - `apps/web/tests/plan-limit-paywall-smoke.spec.ts` — pins mocked 402
    and 429 checkouts keeping the pricing trial-signup and current-usage
    actions resolving to real routes.
  - `apps/web/tests/pricing-blurb-contrast.spec.ts` — pins pricing plan
    blurbs meeting 4.5:1 contrast in light and dark modes, computed from
    rendered colors rather than source hex values.
  - `apps/web/tests/security-headers.spec.ts` — pins public pages sending
    HSTS/nosniff/DENY/no-referrer plus permissions-policy with no CSP
    violations.
  - `apps/web/tests/signup-paywall.spec.ts` — pins the signup start limit
    showing the paywall without a duplicate inline alert, and the finish
    limit preserving the code and challenge for retry.
  - `apps/web/tests/smoke.spec.ts` — pins the landing BYOP copy with
    working signup/pricing CTAs, the health/readiness endpoints,
    contract routes rendering their e2e hooks, signup/login/magic-link
    basics, and the provider wizard steps (allowlist ports, TLS-off
    acknowledgement, save gated on a passing connection test).

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
