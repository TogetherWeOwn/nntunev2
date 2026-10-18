import type { APIRequestContext } from '@playwright/test';
import { API } from './contract.js';
import { accessHeaders, assertNoAccessWall } from './access.js';
import { apiGet } from './accounts.js';
import {
  assertDeployedBuildMatchesPromotion,
  expectedPromotionSha,
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
 * deploy run — never a source checkout). Every gap throws by name;
 * callers skip only while `STAGING_BASE_URL` is unset. Returns the
 * deployed SHA so the closing leg can name it.
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
  return deployed;
}
