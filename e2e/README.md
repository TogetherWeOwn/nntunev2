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
   and lists the specs so the harness cannot rot silently.
2. **Staging live:** the provision card stores `STAGING_BASE_URL` (+ Access +
   NNTP secrets). The suite activates: failures fail closed and block prod
   promotion via the production-environment protection rule.
3. **Contract drift:** a renamed `data-testid` or reshaped JSON body fails
   with the contract key named. Update app + `helpers/contract.ts` together.

## Run locally

```sh
cd e2e
npm ci
npx playwright install chromium
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

## Environment

| Name | Required | Purpose |
|---|---|---|
| `STAGING_BASE_URL` | yes (post-deploy) | `https://` staging origin. Unset → skip with notice. |
| `CF_ACCESS_CLIENT_ID` / `CF_ACCESS_CLIENT_SECRET` | when Access fronts staging | QA service-token pair. Half-set → fail closed. |
| `QA_AUTH_TOKEN` | optional | Bearer for `POST /api/test-accounts` (staging-only seam). Absent → UI signup fallback. |
| `SYNTHETIC_NNTP_HOST` / `_PORT` / `_USER` / `_TLS` | for BYOP spec | Hosted fixture server or test provider (defaults: `synthetic-nntp`, `563`, `e2e`, TLS on). |
| `SYNTHETIC_NNTP_PASS` | for BYOP spec | No default — fail closed when missing. |

How the suite uses them:

- Every request carries the Access headers when the pair is set; an Access
  wall (401/403 with Access markers) fails closed with a remediation message
  instead of masquerading as an app failure.
- Test accounts are throwaway (`e2e+<stamp>@example.test`); the API seam is
  tried first, UI signup second. The over-limit check needs the seam to
  support `{plan:"overlimit"}` (metering card); until then it skips with the
  reason recorded, while usage/plan metadata is still asserted.

## CI

- `.github/workflows/e2e-staging.yml` — full suite on `push` to `main`
  (post-deploy), manual dispatch, and a 6-hour synthetic schedule. Missing
  `STAGING_BASE_URL` → step-level skip with notice (pre-deploy); configured
  staging → fail closed. Failure artifacts kept 7 days.
- `e2e-lint` inside `ci.yml` (part of `ci-ok`) — `npm ci`, `tsc --noEmit`,
  `playwright test --list`. Runs on every PR with no staging needed.
- **Prod promotion gate:** the production environment requires the
  `e2e-staging` result on the same SHA (wired by the provision card). A red
  staging suite blocks promotion — that is the acceptance criterion.

## Evidence

On failure CI uploads `playwright-report/` + `test-results/` (7 days). A QA
PASS verdict cites the green `e2e-staging` run URL + head SHA; NEEDS WORK
cites the failing spec + step.
