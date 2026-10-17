# M1 exit traceability matrix

Maps every M1 exit step to the `e2e/` spec(s) that cover it, or records an
explicit gap. M1 exits when the secure staging user journey
(signup → BYOP → ingest → play/seek) passes on an exact-head staging deploy
with Access enabled, and rollback evidence is recorded.

Verified against the `e2e/` tree at `1ef120d` plus the anonymous-Access
change on 2026-10-17: `npm --prefix e2e run e2e:list` lists 183 tests in
10 files. Line numbers below are from that tree; re-verify with the
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
| 1 | Signup | PARTIAL | `e2e/tests/signup-byop.spec.ts:27` "signup creates an account and lands on the BYOP wizard"; same UI flow duplicated as fallback in `e2e/tests/ingest-play-meter.spec.ts:56` | With the test-account seam (`QA_AUTH_TOKEN` set), the account is minted via `POST /api/test-accounts` and the test asserts only that the BYOP wizard is reachable (`signup-byop.spec.ts:38`). The real `/signup` form submission (fill email, submit, no inline error, land on `/onboarding/byop`) runs only when the seam is absent. **Gap:** a seam-backed run does not prove public UI signup; the exit evidence must record which path ran. |
| 2 | BYOP connect | COVERED | `e2e/tests/signup-byop.spec.ts:70` via `completeByopWizard` (`e2e/helpers/accounts.ts:328`); re-exercised in `ingest-play-meter.spec.ts:84` | Fills host/port/user/password for the synthetic NNTP endpoint, requires test-connection to report success within 60 s, saves, and requires landing on `/library`. Fails closed when `SYNTHETIC_NNTP_PASS` is unset rather than submitting an empty credential. |
| 3 | Ingest | COVERED | `e2e/tests/ingest-play-meter.spec.ts:45` "NZB fixture ingests and appears in the library" | Uploads `e2e/fixtures/synthetic.nzb` at `/library/import`, requires the import status to reach complete/ready/done within 240 s, then requires the track to be visible in `/library`. The ingesting identity completes the BYOP wizard first, because the ingest worker probes through the user's provider connection. |
| 4 | Play / seek | PARTIAL | Seek: `e2e/tests/ingest-play-meter.spec.ts:105` "streaming answers byte Ranges with 206 + Content-Range". Play: none | Two API Range GETs (`bytes=0-8`, `bytes=9-17`) must return 206 with `Content-Range` (and length 9 for the first), as the ingesting account. **Gap:** no spec asserts in-browser audio playback or player-UI seek; the meter test treats the player as an optional extra signal and asserts nothing about playback state (`ingest-play-meter.spec.ts:195`). |
| 5 | Access enforcement | COVERED | With-credentials side: every live spec, e.g. `assertNoAccessWall` in `e2e/tests/health.spec.ts:32`, `:70`, `:92`; fail-closed config in `e2e/playwright.config.ts` (pair validated at load), pinned offline by `e2e/tests/helpers.spec.ts:76` and both job preflights in `e2e/tests/workflows.spec.ts:46`. Anonymous side: `e2e/tests/health.spec.ts:116` "anonymous GET / (no credentials) is denied by Access, not served the app shell" via an isolated request context + `anonymousGet` (`e2e/helpers/accounts.ts:165`) and `assertAccessWall` (`health.spec.ts:136`, predicate `isAccessWall` at `e2e/helpers/access.ts:79`), pinned offline by `e2e/tests/helpers.spec.ts:231` and the no-credentials/no-follow server proof at `helpers.spec.ts:311` | A green live run proves the journey passes **with Access enabled** and a valid QA service token: any wall response (401/403/302 with Access markers) fails the step by name instead of masquerading as an app failure, and configured staging without the full credential pair fails before any request. The anonymous spec sends `GET /` with no Access headers, cookies or credentials from a fresh request context (redirects never followed) and requires the **same joint wall predicate** — wall status *and* Access markers — plus a non-200 status, so a served app shell, a plain app 403 or a plain app redirect all fail by name instead of counting as denial. The credentialed app-shell smoke (`health.spec.ts:78`) stays as the authorized control on the same path. Neither spec emits credentials, cookies or response bodies. Subject to the global caveat: until staging is configured the anonymous spec skips like every live spec, so live denial evidence exists only from a configured-staging run. |
| 6 | Rollback evidence | GAP | none in `e2e/` | Rollback evidence is procedural, not a spec: `docs/ops.md` "Staging promotion checklist" requires a recorded rollback pointer (last good staging SHA + its push-triggered `deploy.yml` run with `staging-web` conclusion `success`), rollback by re-running all jobs on that run, and a successful `staging-web` rerun plus staging health. Workers are not deployed by that workflow and need their own rollback evidence. e2e contributes only the reusable post-rollback health smoke (row 7). |
| 7 | Exact-head staging deploy (exit wrapper) | PARTIAL | `e2e/tests/health.spec.ts:19` (`/api/health`), `:54` (`/api/ready`), `:78` (app shell behind Access) | The smoke proves the deployed app answers `ok:true` in under 2 s, promotion readiness (200 JSON `{ok:true}` only; 404 and 503 — even with `Retry-After` — fail, pinned offline by `e2e/tests/readiness.spec.ts` via `e2e/helpers/readiness.ts`), and the app shell behind Access. **Gap:** no spec ties the deployed build to the exact commit SHA — `version` is asserted only when the app ships it and is a package version, not a build identity. Per `docs/ops.md`, same-build evidence must not be claimed until a deployed-build identity is exposed and compared with the promotion SHA, and the push-triggered e2e run must not race the independent deploy (sequence by dispatch or a later scheduled run). |
| 8 | E2E evidence pass (exit wrapper) | COVERED as process | `.github/workflows/e2e-staging.yml` (synthetic smoke + full journey jobs); verdict format in `e2e/README.md` "Evidence" | Both jobs run the fail-closed preflight, upload sanitized junit-only failure artifacts for 7 days, and a PASS verdict cites the green run URL plus the deployed head SHA. Subject to the global caveat above: while staging is unconfigured both jobs skip with notice, which does not count as evidence. |

## Coverage that is not an M1 exit step

Listed so it is not mistaken for (or double-counted as) M1 coverage:

- Usage meter increase + plan metadata (`ingest-play-meter.spec.ts:155`)
  and over-limit refusal (`ingest-play-meter.spec.ts:222`, fail-closed on
  CI when the over-limit seam is unavailable) belong to the metering
  milestone, not the M1 exit list.
- `e2e/tests/helpers.spec.ts`, `e2e/tests/workflows.spec.ts`,
  `readiness.spec.ts`, `redirect-diagnostics.spec.ts`,
  `account-identity.spec.ts`, `byop-transport.spec.ts` and
  `byop-tls-setting.spec.ts` are offline self-tests (wall detection both
  ways, credential scope, workflow preflights, artifact sanitization).
  They run in the `e2e-lint` CI job on every PR and keep the harness
  honest, but produce no staging evidence.

## Re-verifying this matrix

```sh
find e2e/tests -name '*.spec.ts' | sort
npm --prefix e2e run e2e:list   # spec files + test titles Playwright sees
grep -n "test('" e2e/tests/*.spec.ts
```

Every path and title cited above must appear in that output. If a cited
spec moves or a new spec closes a PARTIAL/GAP row, update the row in the
same PR; a stale path here is a failed review, per the matrix's own test
plan.
