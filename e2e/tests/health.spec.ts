import { test, expect } from '@playwright/test';
import { API, TESTIDS } from '../helpers/contract.js';
import { accessHeaders, assertNoAccessWall, stagingBaseUrl } from '../helpers/access.js';

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
  test('GET /api/health reports ok:true with a version', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'health');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const started = Date.now();
    const res = await request.get(`${baseURL}${API.health}`, {
      headers: accessHeaders(),
      timeout: 30_000,
    });
    const bodyText = await res.text().catch(() => '');
    assertNoAccessWall(
      { status: res.status(), headers: res.headers(), bodyText, url: `${baseURL}${API.health}` },
      'health',
    );
    expect(res.status(), `health body: ${bodyText.slice(0, 500)}`).toBe(200);
    const body = JSON.parse(bodyText) as Record<string, unknown>;
    expect(body.ok, `health body: ${bodyText.slice(0, 500)}`).toBe(true);
    expect(typeof body.version === 'string' && body.version.length > 0).toBe(true);
    // Performance envelope: health must answer fast on staging.
    expect(Date.now() - started).toBeLessThan(2000);
  });

  test('GET /api/ready reports readiness shape (200, or 503 with Retry-After)', async ({
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'ready');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const res = await request.get(`${baseURL}${API.ready}`, {
      headers: accessHeaders(),
      timeout: 30_000,
    });
    const bodyText = await res.text().catch(() => '');
    assertNoAccessWall(
      { status: res.status(), headers: res.headers(), bodyText, url: `${baseURL}${API.ready}` },
      'ready',
    );
    expect([200, 503], `ready body: ${bodyText.slice(0, 500)}`).toContain(res.status());
    if (res.status() === 503) {
      expect(res.headers()['retry-after'] !== undefined).toBe(true);
    }
  });

  test('GET / serves the app shell behind Access', async ({ page }) => {
    const baseURL = stagingBaseUrl('skip', 'app-shell');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
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
    await expect(page.getByTestId(TESTIDS.appRoot), 'data-testid="app-root" missing').toBeVisible({
      timeout: 15_000,
    });
  });
});
