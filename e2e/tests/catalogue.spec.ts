import { isDeepStrictEqual } from 'node:util';
import { test, expect, type APIResponse, type Page, type Response } from '@playwright/test';
import { accessHeaders, readApiBody, stagingBaseUrl } from '../helpers/access.js';
import { API, catalogueLibraryRows } from '../helpers/contract.js';
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
const bindingHeader = 'x-nntune-session-binding';
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

async function readCatalogueJson(response: APIResponse | Response): Promise<unknown> {
  const body = await readApiBody(response, response.url(), 'catalogue');
  expect(response.status(), `catalogue collection failed: HTTP ${response.status()}`).toBe(200);
  try {
    return JSON.parse(body) as unknown;
  } catch {
    throw new Error('Catalogue collection returned invalid JSON (body withheld).');
  }
}

async function readCollection(page: Page, baseURL: string, path: string): Promise<unknown> {
  // The context's session cookie authenticates these direct reads too.
  const response = await apiGet(page.request, `${baseURL}${path}`, accessHeaders(), 'catalogue');
  return readCatalogueJson(response);
}

async function expectOwnBinding(page: Page, baseURL: string, response: APIResponse | Response): Promise<string> {
  const binding = response.headers()[bindingHeader] ?? '';
  expect(/^[A-Za-z0-9_-]{43}$/.test(binding), 'Library requires a browser session binding').toBe(true);
  const verifier = await readCollection(page, baseURL, '/v1/me/likes/session') as { session_binding?: unknown } | null;
  expect(verifier?.session_binding === binding, 'Library must be bound to the current cookie session').toBe(true);
  return binding;
}

test('catalogue aliases return the same library for each fresh account', async ({ browser, request }) => {
  const baseURL = stagingBaseUrl('skip', 'catalogue');
  if (!baseURL) return;
  // Separate contexts keep both cookie jars independent. The seam mints a
  // different account on each call; no ambient QA identity is a library reader.
  const emails: string[] = [];
  const bindings: string[] = [];
  for (let user = 0; user < 2; user += 1) {
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      emails.push(await mintFreshAccount(page, baseURL));
      const versioned = catalogueLibraryRows(await readCollection(page, baseURL, '/v1/library'), sharedPublished);
      const legacy = await apiGet(page.request, `${baseURL}${API.library}`, accessHeaders(), 'catalogue');
      const rows = catalogueLibraryRows(await readCatalogueJson(legacy), sharedPublished);
      expect(isDeepStrictEqual(rows, versioned), 'Library aliases must return identical rows').toBe(true);
      bindings.push(await expectOwnBinding(page, baseURL, legacy));
    } finally {
      await context.close();
    }
  }
  expect(emails[0] !== emails[1] && bindings[0] !== bindings[1],
    'each fresh account must have its own identity and Library binding').toBe(true);

  const anonymous = await apiGet(request, `${baseURL}${API.library}`, accessHeaders(), 'catalogue');
  await readApiBody(anonymous, anonymous.url(), 'catalogue');
  expect(anonymous.status(), 'Access alone must not authorize a Library read').toBe(401);
  expect(bindingHeader in anonymous.headers(), 'anonymous reads must not receive a binding').toBe(false);
});

test('catalogue collections expose member-scoped array contracts for a fresh account', async ({ page }) => {
  const baseURL = stagingBaseUrl('skip', 'catalogue');
  if (!baseURL) return;
  await mintFreshAccount(page, baseURL);
  for (const path of ['/v1/tracks', '/v1/releases', '/v1/artists']) {
    const rows = await readCollection(page, baseURL, path);
    expect(Array.isArray(rows), 'catalogue collection must be an array').toBe(true);
    if (!sharedPublished) expect(Array.isArray(rows) && rows.length === 0, 'fresh catalogue must be empty').toBe(true);
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
  const expected = catalogueLibraryRows(await readCollection(page, baseURL, '/v1/library'), sharedPublished);
  const [response] = await Promise.all([
    page.waitForResponse(
      (response) =>
        response.url() === `${baseURL}${API.library}` && response.request().method() === 'GET',
      { timeout: 30_000 },
    ),
    page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 }),
  ]);
  const rows = catalogueLibraryRows(await readCatalogueJson(response), sharedPublished);
  expect(isDeepStrictEqual(rows, expected), 'browser Library must receive the same account rows as the versioned API').toBe(true);
  await expectOwnBinding(page, baseURL, response);
  await waitForHydration(page, 'catalogue');
  await expect(page.getByRole('heading', { name: 'Library', exact: true })).toBeVisible();
  if (sharedPublished) {
    const first = page.getByTestId('library-item').first();
    await expect(first).toBeVisible();
    const id = await first.getAttribute('data-id');
    expect(rows.some((row) => row.id === id), 'rendered track must be present in the live Library response').toBe(true);
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
      libraries.push(catalogueLibraryRows(await readCollection(page, baseURL, '/v1/library'), sharedPublished));
    } finally {
      await context.close();
    }
  }
  expect(identities[0] !== identities[1], 'readers must be distinct fresh accounts').toBe(true);
  expect(isDeepStrictEqual(libraries[0], libraries[1]),
    'both fresh accounts must see the same shared rows').toBe(true);
});
