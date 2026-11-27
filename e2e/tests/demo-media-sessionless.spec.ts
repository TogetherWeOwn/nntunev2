import { test, expect, type Page } from '@playwright/test';
import { TESTIDS } from '../helpers/contract.js';
import {
  accessHeaders,
  assertNoAccessWall,
  isStagingOrigin,
  stagingBaseUrl,
  type WallProbe,
} from '../helpers/access.js';
import { apiGet, applyAccountToPage } from '../helpers/accounts.js';

// Demo-media sessionless invariant (demo routes are no-auth-by-design):
// demo routes serve synthetic bytes with no app session, while the real
// Library serves its signed-out gate or redirects to sign-in. The API denial
// is covered independently in api-worker-anonymous.spec.ts.
//
// Sessionless construction: API calls carry ONLY the Cloudflare Access pair
// (no Authorization bearer, no cookies — the Playwright `request` fixture
// starts cookieless per test); browser loads go through
// applyAccountToPage(page, null, ...) (Access pair only) on a fresh context
// with cookies explicitly cleared.
//
const SIGNED_OUT_LIBRARY = /^You are signed out or do not have access to this library\.$/;

function anonymousLibraryPagePath(probe: WallProbe, baseURL: string): '/login' | '/library' {
  assertNoAccessWall(probe, 'demo-library-gate');
  if (probe.status === 200 && isStagingOrigin(probe.url, baseURL)) {
    const { pathname } = new URL(probe.url);
    if (pathname === '/login' || pathname === '/library') return pathname;
  }
  throw new Error('[demo-library-gate] expected the Library or sign-in page on the staging origin');
}

async function assertRenderedLibraryDenial(
  page: Page,
  path: '/login' | '/library',
  timeout?: number,
): Promise<void> {
  const check = expect.configure({ timeout });
  if (path === '/login') {
    await check(page.getByRole('heading', { name: 'Sign in', exact: true })).toBeVisible();
  } else {
    await check(page.getByRole('alert').filter({ hasText: SIGNED_OUT_LIBRARY })).toBeVisible();
    await check(page.getByRole('link', { name: 'Sign in', exact: true })).toBeVisible();
  }
  await check(page.getByTestId(TESTIDS.libraryList)).toHaveCount(0);
  await check(page.getByTestId(TESTIDS.libraryItem)).toHaveCount(0);
}

test.describe('demo media serves sessionless; library stays gated', () => {
  test.skip(
    !process.env.STAGING_BASE_URL,
    'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
  );
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
    if (!response) throw new Error('[demo-library-gate] no HTTP response for /library');
    const path = anonymousLibraryPagePath(
      {
        status: response.status(),
        headers: response.headers(),
        bodyText: await page.content().catch(() => ''),
        url: response.url(),
      },
      baseURL,
    );

    await assertRenderedLibraryDenial(page, path);
  });
});

test.describe('anonymous Library denial (offline fixtures)', () => {
  const origin = 'https://library.example.test';

  test('requires an exact same-origin Library or sign-in page', () => {
    const probe: WallProbe = { status: 200, headers: {}, bodyText: '', url: origin + '/library' };
    expect(anonymousLibraryPagePath(probe, origin)).toBe('/library');
    expect(anonymousLibraryPagePath({ ...probe, url: origin + '/login?next=%2Flibrary' }, origin)).toBe('/login');
    for (const url of [
      origin + '/logins',
      origin + '/other?next=/login',
      'https://elsewhere.example.test/login',
    ]) {
      expect(() => anonymousLibraryPagePath({ ...probe, url }, origin)).toThrow(
        /expected the Library or sign-in/,
      );
    }
    expect(() => anonymousLibraryPagePath({ ...probe, status: 404 }, origin)).toThrow(/expected the Library or sign-in/);
    expect(() => anonymousLibraryPagePath({
      ...probe, status: 403, headers: { 'cf-access-denied': 'fixture' },
    }, origin)).toThrow(/Cloudflare Access wall/);
  });

  test('requires visible denial and no Library lists or items in the rendered page', async ({ page }) => {
    const alert = '<p role="alert">You are signed out or do not have access to this library.</p>';
    const link = '<a href="/login">Sign in</a>';
    await page.setContent(alert + link);
    await assertRenderedLibraryDenial(page, '/library');
    await page.setContent('<h1>Sign in</h1>');
    await assertRenderedLibraryDenial(page, '/login');

    for (const html of [
      '<p role="alert">Your library is empty.</p>' + link,
      '<div hidden>' + alert + '</div>' + link,
      alert,
      alert + link + '<div data-testid="library-list"></div>',
      alert + link + '<div data-testid="library-item">Fixture track</div>',
    ]) {
      await page.setContent(html);
      await expect(assertRenderedLibraryDenial(page, '/library', 300)).rejects.toThrow();
    }
    await page.setContent('<h1>Library</h1>');
    await expect(assertRenderedLibraryDenial(page, '/login', 300)).rejects.toThrow();
  });
});
