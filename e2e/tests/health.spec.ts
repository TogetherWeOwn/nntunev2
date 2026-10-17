import { test, expect, request as playwrightRequest } from '@playwright/test';
import { API, TESTIDS } from '../helpers/contract.js';
import { accessHeaders, assertAccessWall, assertNoAccessWall, stagingBaseUrl } from '../helpers/access.js';
import { anonymousGet, apiGet, applyAccountToPage } from '../helpers/accounts.js';
import { assertPromotionReady } from '../helpers/readiness.js';

// Fast synthetic smoke: runs post-deploy AND on a schedule. Must stay under
// ~2 minutes total — the deep journey lives in the other specs.
//
// File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
// without launching a browser, so `npm run e2e:staging` stays green and fast
// until staging exists. In-test guards below remain as defense in depth.
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

test.describe('staging synthetic smoke', () => {
  test('GET /api/health reports ok:true (version optional until the app ships it)', async ({
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'health');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const started = Date.now();
    // apiGet fetches with maxRedirects: 0 so a staging 302 never carries the
    // Access pair or bearer to another origin (see helpers/accounts.ts).
    const res = await apiGet(request, `${baseURL}${API.health}`, accessHeaders(), 'health');
    const bodyText = await res.text().catch(() => '');
    assertNoAccessWall(
      { status: res.status(), headers: res.headers(), bodyText, url: `${baseURL}${API.health}` },
      'health',
    );
    expect(res.status(), `health body: ${bodyText.slice(0, 500)}`).toBe(200);
    const body = JSON.parse(bodyText) as Record<string, unknown>;
    expect(body.ok, `health body: ${bodyText.slice(0, 500)}`).toBe(true);
    // Contract drift (review 4/5): the merged SvelteKit app returns exactly
    // {ok:true} (apps/web/src/routes/api/health/+server.ts, pinned by its own
    // smoke.spec.ts toEqual({ok:true})). Require version only when the app
    // ships it, so the smoke stays green on the current app and still pins
    // the field once it lands. See e2e/README.md "Contract drift".
    if (body.version !== undefined) {
      expect(
        typeof body.version === 'string' && body.version.length > 0,
        `health version must be a non-empty string when present, got: ${bodyText.slice(0, 500)}`,
      ).toBe(true);
    }
    // Performance envelope: health must answer fast on staging.
    expect(Date.now() - started).toBeLessThan(2000);
  });

  test('GET /api/ready is promotion-ready (200 JSON {ok:true} only)', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'ready');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // apiGet fetches with maxRedirects: 0 so a staging 302 never carries the
    // Access pair to another origin (see helpers/accounts.ts).
    const res = await apiGet(request, `${baseURL}${API.ready}`, accessHeaders(), 'ready');
    const bodyText = await res.text().catch(() => '');
    const readyProbe = {
      status: res.status(),
      headers: res.headers(),
      bodyText,
      url: `${baseURL}${API.ready}`,
    };
    assertNoAccessWall(readyProbe, 'ready');
    // Promotion readiness is strict: only 200 JSON {ok:true} passes. A 404
    // (route missing), a 503 (well-formed or not), an HTML 200, malformed
    // JSON or {ok:false} must fail this smoke — never green it. The shared
    // helper is pinned offline by tests/readiness.spec.ts.
    assertPromotionReady(readyProbe, 'ready');
  });

  test('GET / serves the app shell behind Access', async ({ page }) => {
    const baseURL = stagingBaseUrl('skip', 'app-shell');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Browser loads carry the Access pair via the staging-origin route
    // (no context-level headers by design — see applyAccountToPage).
    await applyAccountToPage(page, null, baseURL);
    const response = await page.goto(baseURL, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    expect(response, 'no HTTP response for app shell').not.toBeNull();
    const bodyText = await page.content();
    const headers: Record<string, string> = {};
    for (const [k, v] of Object.entries(response?.headers() ?? {})) headers[k] = v;
    assertNoAccessWall(
      { status: response?.status() ?? 0, headers, bodyText, url: baseURL },
      'app-shell',
    );
    expect(response?.status(), 'app shell did not return 200 — is the frontend deployed?').toBe(
      200,
    );
    // Contract drift (review 4/5): apps/web/src has no data-testid yet, so a
    // hard app-root assertion would be red from the first live run. Accept the
    // hook when the app ships it; until then fall back to the landing's NNTune
    // heading, which the current app renders. See e2e/README.md.
    const appRoot = page.getByTestId(TESTIDS.appRoot);
    if ((await appRoot.count()) > 0) {
      await expect(appRoot, 'data-testid="app-root" present but not visible').toBeVisible({
        timeout: 15_000,
      });
    } else {
      await expect(
        page.getByRole('heading', { level: 1 }),
        'neither data-testid="app-root" nor the NNTune landing heading rendered',
      ).toContainText('NNTune', { timeout: 15_000 });
    }
  });

  test('anonymous GET / (no credentials) is denied by Access, not served the app shell', async () => {
    const baseURL = stagingBaseUrl('skip', 'access-anonymous');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Isolated request context: fresh cookie jar, no storageState, no
    // context-level headers. The Access pair deliberately never enters this
    // context — anonymousGet takes no headers and reads no env, and fetches
    // with maxRedirects: 0 so an Access-login 302 is classified, not
    // followed. The credentialed control is the test above (same origin,
    // same path, Access pair attached): 200 + app shell there, wall here.
    const anonymous = await playwrightRequest.newContext();
    try {
      const res = await anonymousGet(anonymous, `${baseURL}/`);
      // Body is read only to feed the wall predicate; it is never put in an
      // assertion message or log (anonymous responses must not be emitted).
      const bodyText = await res.text().catch(() => '');
      const probe = { status: res.status(), headers: res.headers(), bodyText, url: baseURL };
      await res.dispose().catch(() => {});
      assertAccessWall(probe, 'access-anonymous');
      // Not the app shell: a wall status is never the shell's 200. Named
      // with the status only — no body, headers or URL in the message.
      expect(
        probe.status,
        'anonymous app-shell request returned 200 instead of an Access denial',
      ).not.toBe(200);
    } finally {
      await anonymous.dispose();
    }
  });
});
