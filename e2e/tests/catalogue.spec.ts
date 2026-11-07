import { test, expect, type Page } from '@playwright/test';
import { accessHeaders, readApiBody, stagingBaseUrl } from '../helpers/access.js';
import {
  apiGet,
  applyAccountToPage,
  requireApiAccount,
  tryMintTestAccount,
  waitForHydration,
} from '../helpers/accounts.js';

// Enable only after the admin consumer has published a curated matched release
// to staging. A named parked leg below keeps missing publication fail-closed.
const sharedPublished = process.env.SHARED_CATALOGUE_PUBLISHED === 'true';
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

async function mintFreshAccount(page: Page, baseURL: string): Promise<string> {
  const account = await tryMintTestAccount(page.request, baseURL);
  requireApiAccount(account, 'catalogue');
  await applyAccountToPage(page, account, baseURL);
  if (!account) throw new Error('Fresh catalogue account was not minted.');
  return account.email;
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

test('catalogue aliases return the same library for each fresh account', async ({ browser }) => {
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
      if (!sharedPublished) expect(versioned).toEqual([]);
    } finally {
      await context.close();
    }
  }
});

test('catalogue collections expose member-scoped array contracts for a fresh account', async ({ page }) => {
  const baseURL = stagingBaseUrl('skip', 'catalogue');
  if (!baseURL) return;
  await mintFreshAccount(page, baseURL);
  for (const path of ['/v1/tracks', '/v1/releases', '/v1/artists']) {
    const rows = await readCollection(page, baseURL, path);
    expect(Array.isArray(rows), 'catalogue collection must be an array').toBe(true);
    if (!sharedPublished) expect(rows).toEqual([]);
  }
  // A valid but unowned MBID is not an empty successful release detail.
  const url = `${baseURL}/v1/releases/11111111-1111-4111-8111-111111111111`;
  const response = await apiGet(page.request, url, accessHeaders(), 'catalogue');
  await readApiBody(response, url, 'catalogue');
  expect(response.status(), 'unowned release detail must be hidden').toBe(404);
});

test('Library renders the real API state for a fresh account', async ({ page }) => {
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
  if (!sharedPublished) {
    expect(body.trim() === '[]', 'Library must receive the real empty-array contract').toBe(true);
  }
  await waitForHydration(page, 'catalogue');
  await expect(page.getByRole('heading', { name: 'Library', exact: true })).toBeVisible();
  if (sharedPublished) {
    await expect(page.getByTestId('library-item').first()).toBeVisible();
  } else {
    await expect(page.getByRole('status').filter({ hasText: /^Your library is empty\.$/ })).toBeVisible();
    await expect(page.getByTestId('library-item')).toHaveCount(0);
  }
  await expect(page.getByTestId('library-stale-badge')).toHaveCount(0);
});


test('admin-published rich catalogue is shared by two fresh accounts', async ({ browser }) => {
  test.fixme(!sharedPublished,
    'PARKED: un-park only after the admin consumer publishes a curated matched release to staging; set SHARED_CATALOGUE_PUBLISHED=true.');
  const baseURL = stagingBaseUrl('skip', 'catalogue');
  if (!baseURL) return;
  const identities: string[] = [];
  const libraries: unknown[] = [];
  for (let user = 0; user < 2; user += 1) {
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      identities.push(await mintFreshAccount(page, baseURL));
      const rows = await readCollection(page, baseURL, '/v1/library');
      expect(Array.isArray(rows), 'shared library must be an array').toBe(true);
      const richRows = (rows as unknown[]).filter((value) => {
        if (!value || typeof value !== 'object') return false;
        const row = value as Record<string, unknown>;
        return ['release_mbid', 'title', 'release', 'artist'].every((key) =>
          typeof row[key] === 'string' && row[key].trim().length > 0);
      });
      expect(richRows.length, 'admin publish must yield at least one mirror-enriched row').toBeGreaterThan(0);
      libraries.push(rows);
    } finally {
      await context.close();
    }
  }
  expect(identities[0] !== identities[1], 'readers must be distinct fresh accounts').toBe(true);
  expect(JSON.stringify(libraries[0]) === JSON.stringify(libraries[1]),
    'both fresh accounts must see the same shared rows').toBe(true);
});
