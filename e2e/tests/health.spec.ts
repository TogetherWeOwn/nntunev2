import { test, expect } from '@playwright/test';
import { API, TESTIDS } from '../helpers/contract.js';
import {
  accessHeaders,
  assertAccessWall,
  assertNoAccessWall,
  stagingBaseUrl,
} from '../helpers/access.js';
import { anonymousApiGet, apiGet, applyAccountToPage } from '../helpers/accounts.js';
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

  // Anonymous denial (the other side of the authorized checks above, which
  // stay unchanged): a request with no Access headers or credentials, in an
  // isolated request context, must be denied BY Access. An application
  // ok:true answer here means anonymous traffic reached the app — a failure,
  // not a pass. The probe helper attaches no headers regardless of the
  // configured pair, and the assertion requires Access-specific markers,
  // so an ordinary app error cannot masquerade as enforcement.
  test('GET /api/health without Access credentials is denied by Access', async () => {
    const baseURL = stagingBaseUrl('skip', 'health-anonymous');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const probe = await anonymousApiGet(`${baseURL}${API.health}`, 'health-anonymous');
    assertAccessWall(probe, 'health-anonymous');
  });

  test('GET /api/ready without Access credentials is denied by Access', async () => {
    const baseURL = stagingBaseUrl('skip', 'ready-anonymous');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const probe = await anonymousApiGet(`${baseURL}${API.ready}`, 'ready-anonymous');
    assertAccessWall(probe, 'ready-anonymous');
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
});
