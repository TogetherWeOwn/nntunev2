import { test, expect } from '@playwright/test';
import { TESTIDS } from '../helpers/contract.js';
import { accessHeaders, assertNoAccessWall, stagingBaseUrl } from '../helpers/access.js';
import { apiGet, applyAccountToPage } from '../helpers/accounts.js';

// Demo-media sessionless invariant (demo routes are no-auth-by-design):
// demo routes serve synthetic bytes with no app session, while the real
// library surface stays gated (redirect to /login, or the implemented
// signed-out gate with zero library items — either way, no library content).
//
// Sessionless construction: API calls carry ONLY the Cloudflare Access pair
// (no Authorization bearer, no cookies — the Playwright `request` fixture
// starts cookieless per test); browser loads go through
// applyAccountToPage(page, null, ...) (Access pair only) on a fresh context
// with cookies explicitly cleared.
//
// File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
// without launching a browser. See health.spec.ts.
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

test.describe('demo media serves sessionless; library stays gated', () => {
  test('sessionless GET /api/demo/audio serves synthetic bytes (200; 206 for Range)', async ({
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'demo-audio');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Sessionless by construction: Access pair only. A present Authorization
    // header would mean an app session credential leaked into this probe.
    const headers = accessHeaders();
    expect(headers['Authorization'], 'demo-audio probe must carry no app session').toBeUndefined();

    const url = `${baseURL}/api/demo/audio`;
    const res = await apiGet(request, url, headers, 'demo-audio');
    const body = await res.body().catch(() => Buffer.alloc(0));
    assertNoAccessWall(
      { status: res.status(), headers: res.headers(), bodyText: '', url },
      'demo-audio',
    );
    expect(res.status(), 'sessionless demo audio did not serve 200').toBe(200);
    expect(
      res.headers()['content-type'],
      'sessionless demo audio is not synthetic WAV bytes',
    ).toContain('audio/wav');
    const length = Number(res.headers()['content-length']);
    expect(Number.isInteger(length) && length > 0, 'demo audio content-length is not positive').toBe(
      true,
    );
    expect(body.length, 'demo audio body is empty (not synthetic bytes)').toBeGreaterThan(0);
    expect(
      body.subarray(0, 4).toString('ascii'),
      'demo audio body is not a WAV (missing RIFF magic)',
    ).toBe('RIFF');

    // Byte range on the same sessionless identity answers 206 with the slice.
    const rangeUrl = `${baseURL}/api/demo/audio`;
    const ranged = await apiGet(
      request,
      rangeUrl,
      { ...headers, Range: 'bytes=0-99' },
      'demo-audio-range',
    );
    const rangeBody = await ranged.body().catch(() => Buffer.alloc(0));
    assertNoAccessWall(
      { status: ranged.status(), headers: ranged.headers(), bodyText: '', url: rangeUrl },
      'demo-audio-range',
    );
    expect(ranged.status(), 'sessionless demo audio Range did not serve 206').toBe(206);
    expect(ranged.headers()['content-range'] ?? '', 'demo audio Range missing Content-Range').toMatch(
      /^bytes 0-99\//,
    );
    expect(Number(ranged.headers()['content-length']), 'demo audio Range length is not 100').toBe(
      100,
    );
    expect(rangeBody.length, 'demo audio Range body is not the 100-byte slice').toBe(100);
  });

  test('same sessionless identity does not receive library content at /library', async ({
    page,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'demo-library-gate');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Sessionless browser: Access pair only, cookies cleared — no app session.
    await applyAccountToPage(page, null, baseURL);
    await page.context().clearCookies();
    const response = await page.goto(`${baseURL}/library`, {
      waitUntil: 'domcontentloaded',
      timeout: 30_000,
    });
    expect(response, 'no HTTP response for sessionless /library').not.toBeNull();
    const headers: Record<string, string> = {};
    for (const [k, v] of Object.entries(response?.headers() ?? {})) headers[k] = v;
    assertNoAccessWall(
      {
        status: response?.status() ?? 0,
        headers,
        bodyText: await page.content().catch(() => ''),
        url: `${baseURL}/library`,
      },
      'demo-library-gate',
    );

    // Redirect mechanism: a login redirect gates the surface. Accept any 3xx
    // landing on /login (the browser followed it); the content assertions
    // below additionally prove no library items leaked through.
    if (/\/login/.test(page.url())) {
      await expect(
        page.getByTestId(TESTIDS.libraryItem),
        'sessionless /library redirect to login still rendered library items',
      ).toHaveCount(0);
      return;
    }

    // Implemented gate (current app): 200 app shell with a signed-out / empty /
    // unavailable state and zero library items — never library content.
    expect(response?.status(), 'sessionless /library did not answer 200 or redirect to login').toBe(
      200,
    );
    await expect(
      page.getByTestId(TESTIDS.libraryItem),
      'sessionless /library served library items without an app session',
    ).toHaveCount(0);
    await expect(
      page.getByTestId(TESTIDS.libraryList),
      'sessionless /library rendered a library list without an app session',
    ).toHaveCount(0);
    const gated = page.getByRole('alert').or(page.getByRole('status'));
    await expect(
      gated.first(),
      'sessionless /library shows neither a redirect to login nor a signed-out/empty gate',
    ).toContainText(/signed out|do not have access|empty|import|not available|not reachable/i, {
      timeout: 15_000,
    });
  });

  test('same sessionless identity does not receive library content at /api/library', async ({
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'demo-library-api-gate');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const headers = accessHeaders();
    expect(headers['Authorization'], 'library API probe must carry no app session').toBeUndefined();

    const url = `${baseURL}/api/library`;
    let res;
    try {
      res = await apiGet(request, url, headers, 'demo-library-api-gate');
    } catch (error) {
      const message = (error as Error).message;
      // No-follow wrapper throws on 3xx: a redirect gates the surface (never
      // followed with credentials). The Access wall stays a hard failure.
      if (/Access wall/i.test(message)) throw error;
      expect(message, 'sessionless /api/library failed without a redirect diagnostic').toMatch(
        /unexpected redirect/i,
      );
      return;
    }
    const bodyText = await res.text().catch(() => '');
    assertNoAccessWall(
      { status: res.status(), headers: res.headers(), bodyText, url },
      'demo-library-api-gate',
    );
    // Gated statuses prove the surface stays closed; 404 means the endpoint
    // has not landed (also not serving content). Only a 200 needs a leak check.
    if (res.status() === 401 || res.status() === 403 || res.status() === 404) return;
    expect(res.status(), 'sessionless /api/library answered an unexpected status').toBe(200);
    // A 200 at an /api/* route must be JSON library content (or an empty
    // equivalent) — HTML/a misroute here fails by name instead of passing.
    let parsed: unknown;
    try {
      parsed = JSON.parse(bodyText) as unknown;
    } catch {
      expect(
        bodyText,
        'sessionless /api/library answered 200 with a non-JSON body (misroute?)',
      ).toBe('');
      return;
    }
    const record = (parsed as Record<string, unknown>) ?? {};
    const containers = [
      parsed,
      record['items'],
      record['tracks'],
      record['library'],
      record['data'],
    ].filter(Array.isArray);
    for (const items of containers) {
      expect(
        (items as unknown[]).length,
        'sessionless /api/library served library items without an app session',
      ).toBe(0);
    }
  });
});
