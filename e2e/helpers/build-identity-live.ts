import { test, type APIRequestContext } from '@playwright/test';
import { API } from './contract.js';
import { accessHeaders, assertNoAccessWall } from './access.js';
import { apiGet } from './accounts.js';
import {
  API_NOT_REDEPLOYED,
  apiDeployOutcome,
  assertApiBuildMatchesPromotion,
  assertDeployedBuildMatchesPromotion,
  expectedPromotionSha,
  extractApiBuildSha,
  extractDeployedSha,
} from './build-identity.js';

/**
 * Live same-build assertion shared by the opening
 * (`e2e/tests/build-identity.spec.ts`) and closing
 * (`e2e/tests/zz-build-identity-final.spec.ts`) legs.
 *
 * Reads the deployed identity with the Access pair via the no-follow
 * `apiGet` wrapper, clears the Access-wall check, requires HTTP 200 with
 * exactly `{ sha }` from `GET /api/build`, and requires strict equality
 * with the promotion SHA (`EXPECTED_BUILD_SHA` from the push-triggered
 * deploy run — never a source checkout). Then reads the API Worker's
 * `GET /healthz` `build_sha` through the same Access pair and holds it to
 * the same SHA unless `staging-api` did not redeploy this run
 * (`API_DEPLOY_OUTCOME` skipped/unconfigured), which is logged and
 * annotated explicitly. Logs one plain `web=… api=… head=…` line for QA.
 * Every gap throws by name; callers skip only while `STAGING_BASE_URL` is
 * unset. Returns the web SHA so the closing leg can name it.
 */
export async function assertServingBuildMatchesPromotion(
  request: APIRequestContext,
  baseURL: string,
  context: string,
): Promise<string> {
  // Promotion SHA from the push-triggered deploy run (EXPECTED_BUILD_SHA via
  // deploy.yml expected_sha from github.sha) — never a source checkout.
  // Missing or malformed values throw here: fail closed, never skip-green.
  const expected = expectedPromotionSha(context);
  // apiGet fetches with maxRedirects: 0 so a staging 302 never carries the
  // Access pair to another origin (see helpers/accounts.ts).
  const res = await apiGet(request, `${baseURL}${API.build}`, accessHeaders(), context);
  const bodyText = await res.text().catch(() => '');
  assertNoAccessWall(
    { status: res.status(), headers: res.headers(), bodyText, url: `${baseURL}${API.build}` },
    context,
  );
  if (res.status() !== 200) {
    throw new Error(
      `[${context}] build identity request failed: expected HTTP 200 from GET /api/build, ` +
        `got HTTP ${res.status()} (body ${bodyText.length} chars, ` +
        `promotion SHA ${expected}). See e2e/README.md.`,
    );
  }
  const deployed = extractDeployedSha(bodyText, context);
  assertDeployedBuildMatchesPromotion(deployed, expected, context);

  const apiUrl = `${baseURL}${API.healthz}`;
  const apiRes = await apiGet(request, apiUrl, accessHeaders(), context);
  const apiBody = await apiRes.text().catch(() => '');
  assertNoAccessWall(
    { status: apiRes.status(), headers: apiRes.headers(), bodyText: apiBody, url: apiUrl },
    context,
  );
  const outcome = apiDeployOutcome();
  // Logged before the strict verdict so a failing run still shows both values.
  const logIdentity = (sha: string | null) =>
    console.log(
      `[${context}] build identity: web=${deployed} api=${sha ?? '(absent)'} ` +
        `head=${expected} staging-api=${outcome || '(not provided)'}`,
    );
  let apiSha: string | null;
  let verdict: { sameBuild: boolean; note: string };
  if (apiRes.status() !== 200 && API_NOT_REDEPLOYED.has(outcome)) {
    // An unprovisioned API (no Worker route) answers from the web catch-all;
    // record it as unavailable instead of blocking the web-only evidence.
    apiSha = null;
    logIdentity(apiSha);
    verdict = {
      sameBuild: false,
      note:
        `staging-api ${outcome} this run: API GET /healthz answered HTTP ${apiRes.status()}, ` +
        `so no API identity is available (promotion SHA ${expected}). ` +
        'web=API=head is NOT claimed for this run.',
    };
  } else {
    if (apiRes.status() !== 200) {
      throw new Error(
        `[${context}] API build identity request failed: expected HTTP 200 from GET /healthz, ` +
          `got HTTP ${apiRes.status()} (body ${apiBody.length} chars, ` +
          `promotion SHA ${expected}). See e2e/README.md.`,
      );
    }
    apiSha = extractApiBuildSha(apiBody, context);
    logIdentity(apiSha);
    verdict = assertApiBuildMatchesPromotion(apiSha, expected, outcome, context);
  }
  if (verdict.sameBuild) {
    console.log(`[${context}] build identity: web = API = head = ${expected}`);
  } else {
    console.log(`[${context}] build identity: ${verdict.note}`);
    test.info().annotations.push({ type: 'api-not-redeployed', description: verdict.note });
  }
  return deployed;
}
