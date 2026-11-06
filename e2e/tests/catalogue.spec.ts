import { test, expect, type Page } from '@playwright/test';
import { accessHeaders, readApiBody, stagingBaseUrl } from '../helpers/access.js';
import {
  apiGet,
  applyAccountToPage,
  requireApiAccount,
  tryMintTestAccount,
  waitForHydration,
} from '../helpers/accounts.js';

// These fresh-account cases cover the deployed empty-state contract only.
// Populated-library isolation and row rendering require separately provisioned
// member rows; two empty lists must never be cited as that acceptance evidence.
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

async function mintFreshAccount(page: Page, baseURL: string): Promise<void> {
  const account = await tryMintTestAccount(page.request, baseURL);
  requireApiAccount(account, 'catalogue');
  await applyAccountToPage(page, account, baseURL);
}

async function readCollection(page: Page, baseURL: string, path: string): Promise<unknown> {
  const url = `${baseURL}${path}`;
  // Both aliases accept a session cookie. /v1/* native bearers are a different
  // credential type from the legacy alias's session bearer; do not mix them.
  const response = await apiGet(page.request, url, accessHeaders(), 'catalogue');
  const body = await readApiBody(response, url, 'catalogue');
  expect(response.status(), `catalogue collection failed: HTTP ${response.status()}`).toBe(200);
  try {
    return JSON.parse(body) as unknown;
  } catch {
    throw new Error('Catalogue collection returned invalid JSON (body withheld).');
  }
}

test('catalogue aliases return the same empty library for each fresh account', async ({ browser }) => {
  const baseURL = stagingBaseUrl('skip', 'catalogue');
  if (!baseURL) return;
  // Separate contexts keep both cookie jars independent. The seam mints a
  // different account on each call; no ambient QA identity is a library reader.
  for (let user = 0; user < 2; user += 1) {
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      await mintFreshAccount(page, baseURL);
      const versioned = await readCollection(page, baseURL, '/v1/library');
      const legacy = await readCollection(page, baseURL, '/api/library');
      expect(Array.isArray(versioned), 'versioned library must be a bare array').toBe(true);
      expect(legacy).toEqual(versioned);
      expect(versioned).toEqual([]);
    } finally {
      await context.close();
    }
  }
});

test('catalogue collections expose empty-array contracts for a fresh account', async ({ page }) => {
  const baseURL = stagingBaseUrl('skip', 'catalogue');
  if (!baseURL) return;
  await mintFreshAccount(page, baseURL);
  for (const path of ['/v1/tracks', '/v1/releases', '/v1/artists']) {
    expect(await readCollection(page, baseURL, path)).toEqual([]);
  }
  // A valid but unowned MBID is not an empty successful release detail.
  const url = `${baseURL}/v1/releases/11111111-1111-4111-8111-111111111111`;
  const response = await apiGet(page.request, url, accessHeaders(), 'catalogue');
  await readApiBody(response, url, 'catalogue');
  expect(response.status(), 'unowned release detail must be hidden').toBe(404);
});

test('Library renders the real API empty state for a fresh account', async ({ page }) => {
  const baseURL = stagingBaseUrl('skip', 'catalogue');
  if (!baseURL) return;
  await mintFreshAccount(page, baseURL);
  const [response] = await Promise.all([
    page.waitForResponse(
      (response) =>
        response.url() === `${baseURL}/api/library` && response.request().method() === 'GET',
      { timeout: 30_000 },
    ),
    page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 }),
  ]);
  expect(response.status(), 'Library must read the real API successfully').toBe(200);
  const body = await readApiBody(response, response.url(), 'catalogue');
  // Check the wire body without exposing arbitrary response text in evidence.
  expect(body.trim() === '[]', 'Library must receive the real empty-array contract').toBe(true);
  await waitForHydration(page, 'catalogue');
  await expect(page.getByRole('heading', { name: 'Library', exact: true })).toBeVisible();
  await expect(page.getByRole('status').filter({ hasText: /^Your library is empty\.$/ })).toBeVisible();
  await expect(page.getByTestId('library-item')).toHaveCount(0);
  await expect(page.getByTestId('library-stale-badge')).toHaveCount(0);
});
