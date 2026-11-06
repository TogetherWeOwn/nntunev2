import { test, expect } from '@playwright/test';
import { Buffer } from 'node:buffer';
import { API } from '../helpers/contract.js';
import { accessHeaders, assertNoAccessWall, stagingBaseUrl } from '../helpers/access.js';
import { apiGet, applyAccountToPage } from '../helpers/accounts.js';

// Served web routes with no staging spec: the app serves these
// routes on every deploy, but no staging assertion proved they survived
// promotion — the manifest was pinned only by the `apps/web` unit spec
// (`pwa-manifest.spec.ts`, not staging evidence), and `/pricing` appeared
// only as a fixture string. All four probes are no-auth by design — Access
// pair only, never an account session or bearer — so they are parallel-safe
// and independent of the session-gating slice.
//
// File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
// without launching a browser, so `npm run e2e:staging` stays green and fast
// until staging exists. In-test guards below remain as defense in depth.
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

/** Access-pair-only headers: these routes serve with no app session. */
function sessionlessHeaders(context: string): Record<string, string> {
  const headers = accessHeaders();
  expect(headers['Authorization'], `[${context}] probe must carry no app session`).toBeUndefined();
  return headers;
}

test.describe('served web routes (no-auth, staging)', () => {
  test('GET /manifest.webmanifest serves application/manifest+json', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'served-manifest');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const headers = sessionlessHeaders('served-manifest');
    const url = `${baseURL}${API.manifest}`;
    const res = await apiGet(request, url, headers, 'served-manifest');
    const bodyText = await res.text().catch(() => '');
    assertNoAccessWall(
      { status: res.status(), headers: res.headers(), bodyText, url },
      'served-manifest',
    );
    expect(res.status(), '[served-manifest] expected HTTP 200').toBe(200);
    const contentType = res.headers()['content-type'] ?? '';
    expect(
      contentType,
      '[served-manifest] manifest must serve content-type application/manifest+json',
    ).toContain('application/manifest+json');
    let manifest: unknown;
    try {
      manifest = JSON.parse(bodyText) as unknown;
    } catch {
      manifest = null;
    }
    expect(
      manifest !== null && typeof manifest === 'object',
      '[served-manifest] manifest body is not JSON (names only — no body logged)',
    ).toBe(true);
    expect(
      (manifest as Record<string, unknown>).name,
      '[served-manifest] manifest carries no PWA name',
    ).toBe('NNTune');
  });

  test('GET /api/demo/artwork serves image/svg+xml with immutable caching', async ({
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'served-artwork');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const headers = sessionlessHeaders('served-artwork');
    const url = `${baseURL}${API.demoArtwork}`;
    const res = await apiGet(request, url, headers, 'served-artwork');
    const body = await res.body().catch(() => Buffer.alloc(0));
    assertNoAccessWall(
      { status: res.status(), headers: res.headers(), bodyText: '', url },
      'served-artwork',
    );
    expect(res.status(), '[served-artwork] expected HTTP 200').toBe(200);
    const contentType = res.headers()['content-type'] ?? '';
    expect(
      contentType,
      '[served-artwork] artwork must serve content-type image/svg+xml',
    ).toContain('image/svg+xml');
    const cacheControl = res.headers()['cache-control'] ?? '';
    expect(
      cacheControl,
      '[served-artwork] artwork must carry immutable caching (names only — no values logged)',
    ).toMatch(/immutable/);
    expect(
      cacheControl,
      '[served-artwork] artwork must be publicly cacheable (names only — no values logged)',
    ).toMatch(/public/);
    expect(
      body.length,
      '[served-artwork] artwork body is empty (not placeholder bytes)',
    ).toBeGreaterThan(0);
    expect(
      body.subarray(0, 4).toString('ascii'),
      '[served-artwork] artwork body is not SVG (missing <svg)',
    ).toBe('<svg');
  });

  test('GET /onboarding/provider and /onboarding/byop resolve to not-found (404)', async ({
    page,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'served-provider-retired');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    sessionlessHeaders('served-provider-retired');
    await applyAccountToPage(page, null, baseURL);
    // User provider setup is retired: the legacy redirect entry point and
    // the wizard both resolve to the app 404, never a credential form.
    for (const path of [API.onboardingProvider, '/onboarding/byop']) {
      const url = `${baseURL}${path}`;
      const response = await page.goto(url, {
        waitUntil: 'domcontentloaded',
        timeout: 30_000,
      });
      expect(response, `no HTTP response for retired route ${path}`).not.toBeNull();
      const headers: Record<string, string> = {};
      for (const [k, v] of Object.entries(response?.headers() ?? {})) headers[k] = v;
      assertNoAccessWall(
        {
          status: response?.status() ?? 0,
          headers,
          bodyText: await page.content().catch(() => ''),
          url,
        },
        'served-provider-retired',
      );
      expect(
        response?.status(),
        `[served-provider-retired] expected HTTP 404 for retired route ${path}`,
      ).toBe(404);
      expect(
        new URL(page.url()).pathname,
        `[served-provider-retired] retired route ${path} must not redirect`,
      ).toBe(path);
    }
    await expect(
      page.getByRole('heading', { level: 1 }),
      '[served-provider-retired] 404 page not visible after retired provider route',
    ).toContainText(/page not found/i, { timeout: 15_000 });
  });

  test('GET /pricing serves the plans page', async ({ page }) => {
    const baseURL = stagingBaseUrl('skip', 'served-pricing');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    sessionlessHeaders('served-pricing');
    await applyAccountToPage(page, null, baseURL);
    const pricingUrl = `${baseURL}${API.pricing}`;
    const response = await page.goto(pricingUrl, {
      waitUntil: 'domcontentloaded',
      timeout: 30_000,
    });
    expect(response, 'no HTTP response for pricing page').not.toBeNull();
    const headers: Record<string, string> = {};
    for (const [k, v] of Object.entries(response?.headers() ?? {})) headers[k] = v;
    assertNoAccessWall(
      {
        status: response?.status() ?? 0,
        headers,
        bodyText: await page.content().catch(() => ''),
        url: pricingUrl,
      },
      'served-pricing',
    );
    expect(response?.status(), '[served-pricing] pricing page did not return 200').toBe(200);
    await expect(
      page.getByRole('heading', { level: 1 }),
      '[served-pricing] pricing page rendered no plans heading',
    ).toContainText('Plans and pricing', { timeout: 15_000 });
  });
});
