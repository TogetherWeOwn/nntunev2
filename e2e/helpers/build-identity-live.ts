import { test, type APIRequestContext } from '@playwright/test';
import { API } from './contract.js';
import { accessHeaders, assertNoAccessWall, readApiText } from './access.js';
import { apiGet } from './accounts.js';
import {
  apiDeployOutcome,
  assertDeployedBuildMatchesPromotion,
  expectedPromotionSha,
  extractDeployedSha,
  resolveApiIdentity,
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
 * Every gap throws by name. Only the opening and closing reads contribute
 * candidate identity annotations. Returns the web SHA for the closing leg.
 */
export async function assertServingBuildMatchesPromotion(
  request: APIRequestContext,
  baseURL: string,
  context: string,
  log: (message: string) => void = console.log,
): Promise<string> {
  // Promotion SHA from the push-triggered deploy run (EXPECTED_BUILD_SHA via
  // deploy.yml expected_sha from github.sha) — never a source checkout.
  // Missing or malformed values throw here: fail closed, never skip-green.
  const expected = expectedPromotionSha(context);
  // apiGet fetches with maxRedirects: 0 so a staging 302 never carries the
  // Access pair to another origin (see helpers/accounts.ts).
  const res = await apiGet(request, `${baseURL}${API.build}`, accessHeaders(), context);
  const bodyText = await readApiText(res, context);
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
  const apiBody = await readApiText(apiRes, context);
  assertNoAccessWall(
    { status: apiRes.status(), headers: apiRes.headers(), bodyText: apiBody, url: apiUrl },
    context,
  );
  const outcome = apiDeployOutcome();
  // Logged before the API verdict so a failing run still shows the web and
  // head values; the API value follows on the next line (or in the error).
  log(
    `[${context}] build identity: web=${deployed} head=${expected} ` +
      `staging-api=${outcome || '(not provided)'}`,
  );
  const verdict = resolveApiIdentity(apiRes.status(), apiBody, expected, outcome, context);
  log(
    `[${context}] build identity: web=${deployed} api=${verdict.apiSha ?? '(absent)'} ` +
      `head=${expected}`,
  );
  if (verdict.sameBuild) {
    if (context === 'build-identity' || context === 'build-identity-final') {
      // Admission accepts only the opening and closing receipt phases. Other
      // callers require the same strict match without contributing evidence.
      test.info().annotations.push({
        type: 'candidate-identity',
        description: JSON.stringify({
          phase: context,
          web: deployed,
          api: verdict.apiSha,
          expected,
          observed_at: new Date().toISOString(),
        }),
      });
    }
    log(`[${context}] build identity: web = API = head = ${expected}`);
  } else {
    log(`[${context}] build identity: ${verdict.note}`);
    test.info().annotations.push({ type: 'api-not-redeployed', description: verdict.note });
  }
  return deployed;
}
