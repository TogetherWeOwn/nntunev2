import { test, expect } from '@playwright/test';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  API,
  PLAN_LIMIT_STATUSES,
  TESTIDS,
  extractPlanMeta,
  extractUsageBytes,
} from '../helpers/contract.js';
import { accessHeaders, qaAuthHeader, stagingBaseUrl } from '../helpers/access.js';
import { tryMintTestAccount } from '../helpers/accounts.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE_NZB = path.resolve(here, '../fixtures/synthetic.nzb');

// Golden path part 2: ingest the NZB fixture → stream with Range seeks →
// usage meter moves → plan metadata stays consistent → over-limit staging
// account is refused with the documented plan-limit status.
//
// File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
// without launching a browser. See health.spec.ts.
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

test.describe.serial('ingest → play → meter → plan limit', () => {
  // Ingest polling (queue + NNTP fetch + verify) can take minutes on staging.
  test.describe.configure({ timeout: 300_000 });

  test('NZB fixture ingests and appears in the library', async ({ page, request }) => {
    const baseURL = stagingBaseUrl('skip', 'ingest');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const minted = await tryMintTestAccount(request, baseURL);
    if (minted?.session) {
      await page.goto(baseURL, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    } else {
      await page.goto(`${baseURL}/signup`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    }

    await page.goto(`${baseURL}/library/import`, {
      waitUntil: 'domcontentloaded',
      timeout: 30_000,
    });
    const fileInput = page.getByTestId(TESTIDS.importFile);
    await expect(fileInput, 'NZB import control missing').toBeAttached({ timeout: 15_000 });
    await fileInput.setInputFiles(FIXTURE_NZB);
    const status = page.getByTestId(TESTIDS.importStatus);
    await expect(status, 'import gave no status').toBeVisible({ timeout: 60_000 });
    await expect(status, 'NZB ingest did not complete').toContainText(/complete|ready|done/i, {
      timeout: 240_000,
    });
    await page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    await expect(
      page.getByTestId(TESTIDS.libraryItem).first(),
      'ingested track missing from library',
    ).toBeVisible({ timeout: 60_000 });
  });

  test('streaming answers byte Ranges with 206 + Content-Range', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'range');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const headers = { ...accessHeaders(), ...qaAuthHeader() };
    const library = await request.get(`${baseURL}${API.library}`, { headers, timeout: 30_000 });
    expect(library.ok(), `library listing failed: HTTP ${library.status()}`).toBe(true);
    const items = (await library.json()) as Array<{ id?: string }>;
    const trackId = Array.isArray(items) ? items[0]?.id : undefined;
    expect(trackId, 'library is empty — ingest spec must run first').toBeTruthy();

    const head = await request.get(`${baseURL}${API.stream(trackId as string)}`, {
      headers: { ...headers, Range: 'bytes=0-8' },
      timeout: 30_000,
    });
    expect(head.status(), 'first Range seek did not return 206').toBe(206);
    expect(head.headers()['content-range'] !== undefined).toBe(true);
    expect(Number(head.headers()['content-length'])).toBe(9);

    const seek = await request.get(`${baseURL}${API.stream(trackId as string)}`, {
      headers: { ...headers, Range: 'bytes=9-17' },
      timeout: 30_000,
    });
    expect(seek.status(), 'mid-track Range seek did not return 206').toBe(206);
    expect((seek.headers()['content-range'] ?? '').startsWith('bytes 9-17/')).toBe(true);
  });

  test('usage meter increases after streaming and shows plan metadata', async ({
    page,
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'meter');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const headers = { ...accessHeaders(), ...qaAuthHeader() };
    const beforeRes = await request.get(`${baseURL}${API.usage}`, { headers, timeout: 30_000 });
    expect(beforeRes.ok(), `usage read failed: HTTP ${beforeRes.status()}`).toBe(true);
    const before = extractUsageBytes(await beforeRes.json());

    // Drive one more audible stream through the UI player when present;
    // the API Range seeks above already moved the meter otherwise.
    await page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    const player = page.getByTestId(TESTIDS.audioPlayer);
    if ((await player.count()) > 0) {
      await page.getByTestId(TESTIDS.libraryItem).first().click().catch(() => {});
      await page.waitForTimeout(5_000);
    }

    await expect(async () => {
      const res = await request.get(`${baseURL}${API.usage}`, { headers, timeout: 30_000 });
      expect(res.ok()).toBe(true);
      expect(extractUsageBytes(await res.json())).toBeGreaterThan(before);
    }).toPass({ timeout: 60_000 });

    const metaRes = await request.get(`${baseURL}${API.usage}`, { headers, timeout: 30_000 });
    const meta = extractPlanMeta(await metaRes.json());
    expect(meta.used).toBeLessThanOrEqual(meta.limit);
    await expect(page.getByTestId(TESTIDS.usageMeter).first()).toBeVisible({ timeout: 15_000 });
  });

  test('over-limit staging account is refused with a plan-limit status', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'plan-limit');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Deterministic enforcement needs a capped plan on a throwaway account —
    // a staging-only test seam owned by the metering card. Never hammer a
    // real account into over-limit: that would probe a mutating path.
    const capped = await tryMintTestAccount(request, baseURL, 'overlimit');
    if (!capped) {
      test.skip(
        true,
        'over-limit seed seam unavailable (needs POST /api/test-accounts {plan:"overlimit"} ' +
          'from the metering card) — usage/plan metadata above still verified.',
      );
      return;
    }
    const headers = { ...accessHeaders(), ...qaAuthHeader() };
    const library = await request.get(`${baseURL}${API.library}`, { headers, timeout: 30_000 });
    const items = (await library.json().catch(() => [])) as Array<{ id?: string }>;
    const trackId = Array.isArray(items) ? items[0]?.id : undefined;
    expect(trackId, 'library is empty for the capped account').toBeTruthy();
    const res = await request.get(`${baseURL}${API.stream(trackId as string)}`, {
      headers: { ...headers, Range: 'bytes=0-8' },
      timeout: 30_000,
    });
    expect(
      (PLAN_LIMIT_STATUSES as readonly number[]).includes(res.status()),
      `expected a plan-limit status (${PLAN_LIMIT_STATUSES.join('/')}), got HTTP ${res.status()}`,
    ).toBe(true);
  });
});
