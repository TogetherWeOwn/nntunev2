# M1 exit traceability matrix

Maps every M1 exit step to the `e2e/` spec(s) that cover it, or records an
explicit gap. M1 exits when the secure staging user journey
(signup → BYOP → ingest → play/seek) passes on an exact-head staging deploy
with Access enabled, and rollback evidence is recorded.

Re-verified against the merged `e2e/` tree on 2026-10-19 with the public
UI-signup case, the signup wizard-arrival spec, the browser-playback spec, the player-UI-seek spec, the
Media Session spec, the queue-order spec (#202), the two-track fixture spec, the cost-basis
worksheet spec, the recording-mbids
assertion, the staging demo-audio contract spec, the demo-media
sessionless spec, the PWA manifest spec (#211), the anonymous-Access
denial specs (#185, #188), the branded-error-page specs (#203), the
security-headers staging probe, the deployed-build identity route
(#196) merged, the deployed-build promotion-SHA opening assertion spec
(`e2e/tests/build-identity.spec.ts`), the closing same-build
re-read (`e2e/tests/zz-build-identity-final.spec.ts`), the
scoped-credentials spec (#239), and the api-staging-lane workflow pins
(#407) added:
`npm --prefix e2e run e2e:list` lists 373 tests in 23 files, and
`(cd apps/web && pnpm exec playwright test --list)` lists 110 tests in
23 files (94 in 21 files as previously verified, plus the 5-test
paywall-elapsed-time spec and the 11-test passkey-dismissal spec). Line numbers
below are from that head; re-verify with the commands at the end of this
document whenever a spec is added, renamed, or retitled, and update this
matrix in the same PR. CI enforces the file and title citations on
every PR (the traceability step in the `e2e-lint` job); line numbers stay
a manual re-verify item.

## Reading the statuses

- **COVERED** — a live spec asserts the step end to end.
- **PARTIAL** — part of the step is asserted; the missing part is named.
- **GAP** — no spec covers the step. The gap is a statement, not a spec:
  closing it is separate work.

One global caveat applies to every COVERED/PARTIAL row: all live specs
(`health`, `signup-byop`, `ingest-play-meter`, `player-queue-order`) skip while
`STAGING_BASE_URL` is unset (file-level skip plus in-test guards, see
`e2e/README.md` "Activation progression"). Until staging is configured, a
green run is skip-green and is **not** M1 exit evidence. Once staging is
configured, the harness fails closed instead of skipping.

## Matrix

| # | M1 exit step | Status | Covering spec(s) | What is actually asserted |
|---|---|---|---|---|

| 1 | Signup | PARTIAL | `e2e/tests/signup-byop.spec.ts:91` "public UI signup succeeds even when the test-account seam is configured" (dedicated form case); `e2e/tests/signup-byop.spec.ts:32` "signup creates an account and lands on the BYOP wizard" (separate API-minted/fallback case); same form flow shared as fallback in `e2e/tests/ingest-play-meter.spec.ts:121`; `e2e/tests/signup-wizard-arrival.spec.ts:37` "public UI signup with virtual authenticator reaches the BYOP wizard" (form-to-wizard leg) | The dedicated case never mints: it applies a null identity (Access pair only) and submits the real `/signup` form via `submitPublicUiSignup` (`e2e/helpers/accounts.ts:490`) — fill a unique synthetic mailbox, submit, no inline signup error, emailed-code step reached — so it runs the same way whether or not `QA_AUTH_TOKEN` is set. API-minted coverage stays separate in the `:32` case. The separation plus the no-wizard-assertion rule are pinned offline by `e2e/tests/workflows.spec.ts:236` (no mint in the dedicated case or the shared form helper; the helper must assert the `magic_code` step, never the wizard). Diagnostics name no mailbox or token values. The wizard-arrival leg closes the form-to-wizard gap: with a null identity it submits the real form via `submitPublicUiSignupToByopWizard` (`e2e/helpers/accounts.ts:637`) — email submit, code retrieved from the staging-only `POST /api/test-codes` seam (`retrieveStagedSignupCode`, `e2e/helpers/accounts.ts:526`; path pinned in `e2e/helpers/contract.ts`), passkey finish against a CDP WebAuthn virtual authenticator (`enableVirtualAuthenticator`, `e2e/helpers/accounts.ts:595`) — then requires `/onboarding/byop` plus the wizard field, still with no mint and names-only diagnostics (pinned offline by `e2e/tests/workflows.spec.ts:278`, `:294`). It skips pre-deploy and fails closed on CI without `QA_AUTH_TOKEN`. **Remaining gap:** the seam implementation and migration 0018 ship in this PR; the live leg still awaits API Worker staging deployment and activation — deploy the worker with its per-environment configuration, bind `POST /api/test-codes` to it, set `QA_CODE_SEAM=1` with a staged `QA_AUTH_TOKEN`, apply migration 0018, and satisfy the required Hyperdrive cache-disabled gate. Until then the live leg fails with the documented `[code-seam] POST /api/test-codes failed` signature instead of passing, so this row stays PARTIAL. |
| 2 | BYOP connect | COVERED | `e2e/tests/signup-byop.spec.ts:62` via `completeByopWizard` (`e2e/helpers/accounts.ts:407`); re-exercised in `ingest-play-meter.spec.ts:128` | Fills host/port/user/password for the synthetic NNTP endpoint, requires test-connection to report success within 60 s, saves, and requires landing on `/library`. Fails closed when `SYNTHETIC_NNTP_PASS` is unset rather than submitting an empty credential. |
| 3 | Ingest | COVERED | `e2e/tests/ingest-play-meter.spec.ts:101` "NZB fixture ingests and appears in the library" | Uploads `e2e/fixtures/synthetic.nzb` at `/library/import`, requires the import status to reach complete/ready/done within 240 s, then requires the track to be visible in `/library`. The ingesting identity completes the BYOP wizard first, because the ingest worker probes through the user's provider connection. |
| 4 | Play / seek | PARTIAL | Seek (API): `e2e/tests/ingest-play-meter.spec.ts:216` "streaming answers byte Ranges with 206 + Content-Range". Play: `e2e/tests/ingest-play-meter.spec.ts:333` "synthetic audio actually plays in the browser". Player-UI seek: `e2e/tests/ingest-play-meter.spec.ts:533` "player UI seek moves the active audio position and playback continues". Media Session: `e2e/tests/ingest-play-meter.spec.ts:742` "Media Session metadata reflects the playing ingested track" (last in the journey serial). Queue order: `e2e/tests/player-queue-order.spec.ts:111` "player queue advances in library-selection order" (verdict `e2e/helpers/queue-order.ts`, pinned offline by `e2e/tests/queue-order.spec.ts`) | Two API Range GETs (`bytes=0-8`, `bytes=9-17`) must return 206 with `Content-Range` (and length 9 for the first), as the ingesting account — a lower-level stream control, not UI evidence. All three browser specs run at the end of the journey serial (before the isolated plan-limit group, which mints its own capped accounts, so a browser failure cannot skip the meter verdicts and a plan-limit failure cannot skip the browser verdicts), with playback first, player-UI seek second, and Media Session last, so a Media Session failure cannot skip the play or seek verdicts: selecting the staged synthetic library track (and the player's own play control when selection only loads it) must put the `<audio>` element into the playing state with an advancing position and no page error — a 206 alone cannot pass (`e2e/helpers/playback.ts`, pinned offline by `e2e/tests/playback.spec.ts`). During browser playback the OS-visible Media Session metadata must match the ingested track: title/artist from the library API plus non-empty absolute-https artwork (the artwork-malformed guard, `e2e/helpers/media-session.ts`, pinned offline by `e2e/tests/media-session.spec.ts`), with no page errors. The UI-seek spec clicks the ingested track, pauses via the native toggle, clicks the timeline past its middle (never assigning `currentTime`, never a test-issued Range GET), and requires the still-paused position the timeline renders to land in the 30–95% region with a `seeked` event fired — while paused only the seek can move it — then resumes via the toggle and requires the position to keep advancing from the seek point (the resume floor is seeded with the seek position and takes the min over `timeupdate` events plus every poll sample, so a restart-from-zero replay fails it), with no media or page errors. Paused seek is what lets it run against the pinned 210 ms synthetic WAV. Queue order is asserted separately (`e2e/tests/player-queue-order.spec.ts:111`, currently `test.fixme`): after ingesting as the test account, selecting the library tracks top-to-bottom must fill the player queue in exactly that order, and `player-next` must surface the second selected track — any divergence fails by name (`e2e/helpers/queue-order.ts`). Subject to the global caveat above: while staging is unconfigured the specs skip, which is not exit evidence. The queue-order live leg is FIXMEd until the player slice lands it — the synthetic fixture ingests a single track (so the ≥2-track precondition fails before any UI step), library rows are plain `<li>` elements with no click handler, and `/library` has no `queue-item`/`now-playing`/`player-next` hooks — so on configured staging it can never fail or mask another verdict; un-fixme paths are a multi-track fixture plus selectable rows plus the queue hooks on `/library`. The meter test still treats the player as an additional signal only (`ingest-play-meter.spec.ts:302`). | Two-track staging precondition (first half of the queue-order un-fixme path): `e2e/tests/two-track-fixture.spec.ts:109` "two-track fixture stages ≥2 synthetic tracks in the library" requires ≥2 distinct synthetic tracks post-fixture via the ingest path — staged as two single-file uploads (`synthetic.nzb` plus `synthetic-second-track.nzb`; two uploads are two jobs and two rows per idempotency key) — with a missing second upload failing by the single-track precondition by name — verdict `e2e/helpers/two-track-fixture.ts`, pinned offline in the same file. |
| 5 | Access enforcement | COVERED | With-credentials side: every live spec, e.g. `assertNoAccessWall` in `e2e/tests/health.spec.ts:37`, `:75`, `:124`; anonymous side: `health.spec.ts:90` (`/api/health`) and `:100` (`/api/ready`) via `anonymousApiGet` (`e2e/helpers/accounts.ts:207`) plus `health.spec.ts:148` (anonymous `GET /`, denied not served) via `anonymousGet` (`e2e/helpers/accounts.ts:244`), all classified by `assertAccessWall` on the shared joint predicate (`isAccessWall`, `e2e/helpers/access.ts:68`); fail-closed config in `e2e/playwright.config.ts` (pair validated at load), pinned offline by `e2e/tests/helpers.spec.ts:79` and both job preflights in `e2e/tests/workflows.spec.ts:46`, the anonymous matrix by `helpers.spec.ts:234` (denial shapes), `:359` (cross-predicate agreement) and `:450` (no-credentials/no-follow server proof), plus the credential-free/no-follow pin at `workflows.spec.ts:197` | A green live run proves the journey passes **with Access enabled** and a valid QA service token: any wall response (401/403/302 with Access markers) fails the step by name instead of masquerading as an app failure, and configured staging without the full credential pair fails before any request. It also proves Access **denies anonymous traffic to `/api/health`, `/api/ready` and the app shell `/`**: each is requested with no Access headers or credentials in an isolated request context (no-follow), and only an Access-specific denial passes — an application `ok:true` answer, a served 200 app shell, or a bare app error/redirect without Access markers fails. The credentialed API checks and the app-shell smoke (`health.spec.ts:110`) stay as the authorized controls on the same paths. Diagnostics stay names-only (fixed context + status; no URL, header or body values). Subject to the global caveat: until staging is configured every anonymous spec skips like every live spec, so live denial evidence exists only from a configured-staging run. |

| 6 | Rollback evidence | GAP | none in `e2e/` | Rollback evidence is procedural, not a spec: `docs/ops.md` "Staging promotion checklist" requires a recorded rollback pointer (last good staging SHA + its push-triggered `deploy.yml` run with `staging-web` conclusion `success`), rollback by re-running all jobs on that run, and a successful `staging-web` rerun plus staging health. Workers are not deployed by that workflow and need their own rollback evidence; the evidence a future Worker lane must retain is defined in `docs/ops.md` "Future Worker deploy lanes: required rollback evidence", but no such lane exists yet, so this row stays GAP and no Worker deploy or rollback is claimed. e2e contributes only the reusable post-rollback health smoke (row 7). |
| 7 | Exact-head staging deploy (exit wrapper) | COVERED | `e2e/tests/build-identity.spec.ts:148` "deployed build matches the promotion SHA (exact-head staging evidence)" (opening live leg) plus `e2e/tests/zz-build-identity-final.spec.ts:26` "deployed build still matches the promotion SHA at journey end (superseded-deploy guard)" (closing live leg; shared live assertion `e2e/helpers/build-identity-live.ts`, verdict `e2e/helpers/build-identity.ts`, wiring + last-sorted order pinned offline by `e2e/tests/workflows.spec.ts`); `e2e/tests/health.spec.ts:24` (`/api/health`), `:59` (`/api/ready`), `:110` (app shell behind Access) | The smoke proves the deployed app answers `ok:true` in under 2 s, promotion readiness (200 JSON `{ok:true}` only; 404 and 503 — even with `Retry-After` — fail, pinned offline by `e2e/tests/readiness.spec.ts` via `e2e/helpers/readiness.ts`), and the app shell behind Access. Same-build proof: the opening live leg reads the deployed identity with the Access pair via the no-follow `apiGet` wrapper (`e2e/helpers/accounts.ts`), clears the Access-wall check, requires HTTP 200 with exactly `{ sha }` from `GET /api/build` (`apps/web/src/routes/api/build/+server.ts`, pinned offline by `apps/web/tests/build-identity.spec.ts:26`, `:48`, `:59`), and requires strict equality with the promotion SHA (`EXPECTED_BUILD_SHA`, passed by `deploy.yml` as `expected_sha` from `github.sha` through `e2e-staging.yml` — never a source checkout). An `unknown` identity, any mismatch, a malformed sha, or a missing/malformed promotion SHA fails the spec by name (fail-closed, never skip-green); the leg skips only while `STAGING_BASE_URL` is unset and runs only in the full journey (the synthetic smoke stays `health.spec.ts` alone, so monitoring cannot mint same-build evidence). The verdict contract (match/mismatch/`unknown`/malformed/missing-promotion cases) is pinned offline in the same spec file, which runs in `test:unit` with no staging. `version` in the smoke above stays a package version, not a build identity. Two reads bind the journey: the opening leg proves the deploy delivered the promoted build before the journey, and the closing leg re-reads `/api/build` after every other spec (`zz-` prefix sorts it last under the serial `workers: 1` config) through the same shared helper (`e2e/helpers/build-identity-live.ts`, so the reads cannot drift apart) and fails with a 'superseded by a newer deploy' message when the serving build changed — a green run therefore proves the whole journey ran against the promoted SHA. Deploy concurrency is unchanged on purpose: the deploy releases its job-level group when the deploy finishes while E2E serializes separately, so a later main push CAN deploy staging mid-journey — that timing now fails the closing leg instead of passing green, and the reverse timing (a newer deploy lands before the opening read) fails the opening leg. Sequencing is still established: `e2e-staging.yml` has no push trigger, and `deploy.yml`'s `post-deploy-e2e` job calls it only after the same run's `staging-web` succeeds, so no run can start without its own deploy — and the opening + closing reads above prove which build served it. |
| 8 | E2E evidence pass (exit wrapper) | COVERED as process | `.github/workflows/e2e-staging.yml` (synthetic smoke + full journey jobs), called post-deploy by `.github/workflows/deploy.yml`; verdict format in `e2e/README.md` "Evidence"; sequencing graph pinned offline in `e2e/tests/workflows.spec.ts` "deploy calls e2e-staging only after a successful push staging-web (evaluated)" and "full journey runs only for the post-deploy call (evaluated)" | Both jobs run the fail-closed preflight and upload sanitized junit-only failure artifacts for 7 days. The full journey runs only on the post-deploy call after a successful same-run `staging-web` deploy — a skipped/failed deploy cannot produce a full-journey PASS — while scheduled/manual runs are synthetic monitoring only. A PASS verdict cites the green post-deploy run URL plus the deployed head SHA (the promotion SHA proven equal to `GET /api/build` at both the start and the end of the journey by `e2e/tests/build-identity.spec.ts:148` and `e2e/tests/zz-build-identity-final.spec.ts:26`). Subject to the global caveat above: while staging is unconfigured both jobs skip with notice, which does not count as evidence. |

## Coverage that is not an M1 exit step

Listed so it is not mistaken for (or double-counted as) M1 coverage:

- Usage meter increase + plan metadata (`ingest-play-meter.spec.ts:266`)
  and over-limit refusal — stream (`ingest-play-meter.spec.ts:890`) and
  ingest (`ingest-play-meter.spec.ts:960`), each on a separately minted
  capped account in an isolated plan-limit group after the browser legs —
  belong to the metering milestone, not the M1 exit list. The stream leg
  requires the overlimit seam to pre-seed a streamable track (no
  ingest-as-capped fallback). Both legs fail closed (locally and on CI)
  when the over-limit seam is unavailable, and each asserts the full
  refusal verdict: 402/429 status, limit + reset headers (429 requires
  `retry-after`), the status-consistent limit code with a present,
  status-consistent retryable flag, the upgrade CTA pointer (a matching key
  whose value points at `/pricing`), and no secret leakage — pinned offline
  by `e2e/tests/helpers.spec.ts:1200` (plan-limit refusal verdict).
- Ingested-track recording MBIDs (`ingest-play-meter.spec.ts:481`
  "ingested track carries recording_mbids", currently `test.fixme`) belong
  to the MB-match milestone, not the M1 exit list: after the journey
  completes, the library track must carry a non-empty `recording_mbids`
  list filled by the release-to-recordings route, pinned offline by
  `e2e/tests/helpers.spec.ts:1108` (`isMbid`), `:1129`
  (`extractLibraryItems`) and `:1151` (`extractRecordingMbids`). The live
  leg is FIXMEd until the staging MB mirror can match the synthetic
  fixture (see the spec comment for the un-fixme paths); it sits below
  playback in the journey serial, FIXMEd so it can never fail or skip a
  later step, with Media Session last in that serial and the isolated
  plan-limit group after it.
- Staging demo-audio contract (`e2e/tests/demo-audio-contract.spec.ts:65`
  "default track serves the full vendored body as audio/wav" and `:102`
  "?track=second-signal is salted; unknown ?track= serves the default stream") is fixture support for
  row 4's pinned synthetic WAV, not an M1 exit step: it proves staging
  serves the vendored deterministic body (never mis-routed R2 or library
  bytes) at the no-auth demo-audio route, with the formula itself pinned
  offline in the same file (`:23`, run in `test:unit`). Live legs skip
  green without staging and fail closed once configured.

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
  - `readiness.spec.ts` — pins promotion-readiness parsing (200 JSON
    `{ok:true}` only; 404 and 503 fail), backing the row 7 smoke.
  - `build-identity.spec.ts` (offline describe in the same file) — pins the
    same-build verdict contract backing the row 7 live legs (match passes;
    mismatch, `unknown`, malformed and missing-promotion cases fail by
    name); the deploy-to-E2E promotion-SHA wiring, the shared opening +
    closing live helper (`e2e/helpers/build-identity-live.ts`), and the
    closing leg's last-sorted order are pinned offline by the
    deployed-build identity section of `workflows.spec.ts`.
    `zz-build-identity-final.spec.ts` carries the closing live leg only
    (no offline tests of its own): it skips pre-deploy, runs in
    `test:unit` like the opening spec, and fails closed once configured.
  - `playback.spec.ts` — pins the playback-helper contract backing the
    row 4 play assertion.
- `e2e/tests/security-headers.spec.ts` is the staging probe that the live
  app-shell document response still carries the security headers and
  nonce-bearing CSP (static posture pinned offline by
  `apps/web/tests/`). Staging hardening, not journey coverage.
- `e2e/tests/scoped-credentials.spec.ts` pins the scoped native REST
  credential contract (ADR 0010) both offline and live: the offline
  describe pins the scope vocabulary, canonical bearer routes and the
  session-only set on every PR via `test:unit` with no staging, while the
  staging legs prove the lifecycle (mint, scoped 200, revoke, closed
  denial), the cross-scope denials, the header-only transport and the
  secret-free list surface — skipping green without staging and failing
  closed once configured. Credential hardening, not journey coverage.
- `e2e/tests/demo-media-sessionless.spec.ts` pins the sessionless invariant
  live: `e2e/tests/demo-media-sessionless.spec.ts:25` "sessionless GET
  /api/demo/audio serves synthetic bytes (200; 206 for Range)" plus the
  same-identity library-gated probes at `/library` and `/api/library`.
  Demo routes serve synthetic bytes with no app session (Access pair only,
  never an Authorization bearer or cookies) while the real library surface
  stays gated. Demo hardening, not journey coverage.
- `e2e/tests/demo-audio-contract.spec.ts` pins the synthetic demo-audio
  contract both offline and live: `e2e/tests/demo-audio-contract.spec.ts:24`
  "default body is a 256 KiB deterministic WAV" plus the sample-formula
  and track-salt pins (offline, run on every PR via `test:unit` with no
  staging), and `e2e/tests/demo-audio-contract.spec.ts:65` "default track
  serves the full vendored body as audio/wav" (staging leg, Access pair
  only, never an account session), which proves staging serves the exact
  vendored bytes at the no-auth demo-audio route — never mis-routed R2 or
  library bytes. Demo hardening, not journey coverage.
- `e2e/tests/two-track-fixture.spec.ts` is the test-first acceptance for
  the two-track synthetic fixture (queue-order un-fixme path, first half):
  `e2e/tests/two-track-fixture.spec.ts:109` "two-track fixture stages ≥2 synthetic tracks in the library"
  stages the fixture through the ingest path (`/library/import` to
  `/api/library`) as a fresh test account and requires ≥2 distinct
  synthetic tracks in the staged library — a missing second track fails by
  name via the `e2e/helpers/two-track-fixture.ts` verdict, never by timeout
  — with the contract itself pinned offline in the same file
  (`e2e/tests/two-track-fixture.spec.ts:89` "single-track library fails naming the missing second track",
  run on every PR via `test:unit` with no staging, plus the offline
  `e2e/fixtures/two-track-self-check.mjs` gate in the same `e2e-lint` job
  with exact per-track segment IDs, corpus bytes, group and query parity).
  The leg stages the two single-file uploads in order (`synthetic.nzb`
  plus `synthetic-second-track.nzb`, both covered by the same self-check
  gate with exact segment IDs, corpus bytes, group and distinct queries):
  on current ingest one upload is one job and one library row per
  idempotency key, so the two uploads yield the two rows the gate requires —
  under the future per-file manifest (ADR 0002 accepted-audio-manifest)
  alike. A missing second upload falls back to the single-track NZB and
  fails with the documented single-track signature (red is the signal,
  never a timeout). The two-file `synthetic-two-track.nzb` is never
  staged; it stays pinned by the self-check as the reference two-file
  shape. Fixture support for the row 4 queue-order leg, not
  an M1 exit step — and never the no-auth demo-audio route or its
  `second-signal` track ids.
- `e2e/tests/cost-basis.spec.ts` pins the cost-basis worksheet arithmetic
  offline (run on every PR via `test:unit` with no staging):
  `e2e/tests/cost-basis.spec.ts:172` "§8 illustrative volumes total $6.00"
  plus the sibling §8 row totals ($15.45 R2, $3.00 KV, $2.00 Queues, $0.50
  prospective Analytics Engine, $13.75 PlanetScale storage, $6.60 egress,
  $56.80 current total / $57.30 with prospective) and the
  allowance-boundary cases, with `e2e/tests/cost-basis.spec.ts:436`
  "expected totals cover §8 services only — no §10 measured rows" pinning
  that the §10 measured bill stays evidence, not asserted arithmetic. Cost
  documentation, not journey coverage.
- The `apps/web/tests/` offline specs below run in the `web-e2e` CI job
  on every PR. Backend API calls are intercepted and fulfilled locally,
  while a few specs additionally assert against the real local
  preview-server responses (headers, route resolution, health endpoints).
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
  - `apps/web/tests/branded-error-page.spec.ts` — pins unknown paths
    rendering the branded NNTune 404 (404 status, NNTune brand, visible
    `/library` or `/login` CTA resolving to a live route), no stack
    trace or default SvelteKit chrome, and no secret, hostname, or
    binding material in the rendered output.
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
    analytics event with no PII; the upgrade CTA navigates to pricing and
    the dialog is hidden after navigation.
  - `apps/web/tests/plan-limit-paywall-smoke.spec.ts` — pins mocked 402
    and 429 checkouts keeping the pricing trial-signup and current-usage
    actions resolving to real routes.
  - `apps/web/tests/pricing-blurb-contrast.spec.ts` — pins pricing plan
    blurbs meeting 4.5:1 contrast in light and dark modes, computed from
    rendered colors rather than source hex values.
  - `apps/web/tests/security-headers.spec.ts` — pins public pages sending
    HSTS/nosniff/DENY/no-referrer, a permissions-policy disabling
    camera/microphone/geolocation, and a CSP (`default-src 'self'`,
    `object-src 'none'`, `frame-ancestors 'none'`, no `unsafe-inline` or
    `unsafe-eval` in `script-src`) asserted against the preview server's
    real response headers, with no CSP violations observed.
  - `apps/web/tests/signup-paywall.spec.ts` — pins the signup start limit
    showing the paywall without a duplicate inline alert, and the finish
    limit preserving the code and challenge for retry.
  - `apps/web/tests/paywall-elapsed-time.spec.ts` — pins the paywall
    retry countdown measuring elapsed wall-clock time from a per-model
    deadline, with unknown and zero waits never auto-retrying and a
    dismissed modal disposing its timer.
  - `apps/web/tests/passkey-dismissal.spec.ts` — pins an interrupted
    passkey ceremony showing a retryable notice mentioning dismissal,
    with the emailed signup code preserved for in-place retry (expiry
    still applies), while genuine finish failures and other ceremony
    errors keep their exact current messages and login stays
    non-enumerating.
  - `apps/web/tests/smoke.spec.ts` — pins the landing BYOP copy with
    working signup/pricing CTAs, the health/readiness endpoints,
    contract routes rendering their e2e hooks, signup/login/magic-link
    basics, and the provider wizard steps (allowlist ports, TLS-off
    acknowledgement, save gated on a passing connection test).

  - `apps/web/tests/pwa-manifest.spec.ts` — pins
    `/manifest.webmanifest` serving a valid PWA manifest with reachable
    icons, and the app head linking the manifest with the matching
    theme-color.
  - `apps/web/tests/build-identity.spec.ts` — pins the deployed-build
    identity surface: `GET /api/build` returns exactly `{ sha }` with a
    40-hex baked SHA or the explicit string `unknown`, never leaks
    secrets/hostnames/binding material, and the health endpoint still
    reports exactly `{ ok, version }` (regression guard).

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
