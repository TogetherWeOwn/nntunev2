import { test, expect } from '@playwright/test';
import { API, TESTIDS } from '../helpers/contract.js';
import { readApiBody, stagingBaseUrl } from '../helpers/access.js';
import {
  accountHeaders,
  annotateSignupPath,
  apiGet,
  applyAccountToPage,
  completeByopWizard,
  requireApiAccount,
  submitPublicUiSignup,
  tryMintTestAccount,
  type TestAccount,
  waitForHydration,
} from '../helpers/accounts.js';
import { queueOrderFailure } from '../helpers/queue-order.js';
import {
  RAR_PLAYBACK_FOLLOW_UP,
  REAL_NZB_RAR_SKIP,
  realNzbActive,
  requireRealNzbFixture,
} from '../helpers/real-nzb-fixture.js';

// Queue-order journey: ingest the secret-held real NZB, queue the library in
// selection order, and require the player queue to advance in that same
// order. The offline `apps/web/tests/player-queue.spec.ts` pins queue
// mechanics with mocked audio; this spec is the staging side — a real
// ingested library, real selection order.
//
// The real NZB is a RAR set the pipeline cannot complete (no RAR read — an
// RAR-only subset fails permanent at transcode before any library row), so
// both the ingest leg and the queue-order leg skip by name in real-NZB mode
// until RAR-set playback + multi-track staging land and un-park under
// TWO-1174. There are no checked-in NZB fixtures (TWO-1169 deleted them
// all): the ingest path resolves lazily inside the test so a missing
// staging fails the run red, never silently.
//
// File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
// without launching a browser. See health.spec.ts.
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

// Queue hooks on /library, mirroring the demo player testids
// (`apps/web/tests/player-queue.spec.ts`: queue-item with data-track-id,
// now-playing, player-next). A missing hook fails by name below.
const QUEUE_ITEM = 'queue-item';
const NOW_PLAYING = 'now-playing';
const PLAYER_NEXT = 'player-next';

test.describe.serial('player queue follows library-selection order', () => {
  // The ingest job (queue + NNTP fetch + verify) can take minutes.
  test.describe.configure({ timeout: 600_000 });

  // Serial file: the queue-order test reuses the ingest test's account so
  // selection runs as the ingesting identity. Retries mint a fallback.
  let sharedAccount: TestAccount | null = null;

  test('NZB fixture ingests and appears in the library', async ({ page, request }, testInfo) => {
    const baseURL = stagingBaseUrl('skip', 'queue-order-ingest');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // No completable ingest exists in real-NZB mode (RAR set, no RAR read),
    // so this leg skips by name like every other ingest-completion leg.
    if (realNzbActive()) {
      test.skip(true, REAL_NZB_RAR_SKIP);
      return;
    }
    // One upload of the staged real NZB: one ingest job, one library row.
    // Lazy so a missing staging fails this run red (no synthetic fallback).
    const stagedNzb = requireRealNzbFixture();
    const minted = await tryMintTestAccount(request, baseURL);
    // Evidence provenance: this test duplicates the signup branch, so it
    // records the same bounded signup-path label (no account data).
    annotateSignupPath(testInfo, minted);
    await applyAccountToPage(page, minted, baseURL);
    if (minted?.viaApi) {
      sharedAccount = minted;
      await page.goto(baseURL, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    } else {
      // Real UI signup fallback (not a bare navigation): shared helper with
      // the signup spec. It asserts the emailed-code step — the
      // code-plus-passkey finish needs a staging code-retrieval seam that
      // does not exist yet, so this path cannot enroll an account. API-driven
      // steps below additionally require the seam — see requireApiAccount.
      sharedAccount = await submitPublicUiSignup(page, baseURL);
    }

    // The ingest worker probes through the user's authenticated provider
    // connection, so the ingesting identity completes the BYOP wizard first
    // (shared helper with the signup spec). Fails closed without the NNTP
    // password rather than ingesting into a provider-less account.
    await completeByopWizard(page, baseURL);

    await page.goto(`${baseURL}/library/import`, {
      waitUntil: 'domcontentloaded',
      timeout: 30_000,
    });
    await waitForHydration(page, 'player-queue-order');
    const fileInput = page.getByTestId(TESTIDS.importFile);
    await expect(fileInput, 'NZB import control missing').toBeAttached({ timeout: 15_000 });
    await fileInput.setInputFiles(stagedNzb);
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

  // Parked until RAR-set playback + multi-track staging land
  // (RAR_PLAYBACK_FOLLOW_UP): the real NZB yields a single library row, so
  // the ≥2-track queue assertion has nothing to run against. Skips by name —
  // the evidence gate allow-lists this title in real-NZB mode.
  test('player queue advances in library-selection order', async ({ page, request }) => {
    const baseURL = stagingBaseUrl('skip', 'queue-order');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    if (realNzbActive()) {
      test.skip(true, `${REAL_NZB_RAR_SKIP} (un-parks under ${RAR_PLAYBACK_FOLLOW_UP})`);
      return;
    }
    // Same identity discipline as the ingest journey: queue order must run
    // as the ingesting account, never anonymously or as the generic QA seam.
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    requireApiAccount(account, 'queue-order');
    const headers = accountHeaders(account);

    // Precondition via the API (no megabyte buffering: library bodies are
    // small JSON): the queue-order assertion needs ≥2 ingested tracks.
    const libraryUrl = `${baseURL}${API.library}`;
    const library = await apiGet(request, libraryUrl, headers, 'queue-order');
    const libraryBody = await readApiBody(library, libraryUrl, 'queue-order');
    expect(library.ok(), `library listing failed: HTTP ${library.status()}`).toBe(true);
    const parsed: unknown = JSON.parse(libraryBody) as unknown;
    const apiItems = (Array.isArray(parsed) ? parsed : (parsed as { items?: unknown }).items) as
      | Array<{ id?: string }>
      | undefined;
    const apiIds = Array.isArray(apiItems)
      ? apiItems.map((item) => item?.id).filter((id): id is string => typeof id === 'string')
      : [];
    expect(
      apiIds.length,
      '[queue-order] library holds fewer than 2 tracks — queue-order needs ≥2',
    ).toBeGreaterThanOrEqual(2);

    // Names only: a page-error message can embed the staging origin or other
    // values that must stay out of public CI artifacts.
    const pageErrorNames: string[] = [];
    page.on('pageerror', (error) => pageErrorNames.push(error.name || 'Error'));

    await applyAccountToPage(page, account, baseURL);
    await page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    await waitForHydration(page, 'player-queue-order');
    const rows = page.getByTestId(TESTIDS.libraryItem);
    await expect(
      rows.first(),
      'ingested tracks missing from library — ingest test must run first',
    ).toBeVisible({ timeout: 60_000 });

    // Library-selection order: rendered top-to-bottom data-id order.
    const libraryOrder = await rows.evaluateAll((items) =>
      items.map((item) => item.getAttribute('data-id')),
    );
    expect(
      libraryOrder.every((id): id is string => typeof id === 'string' && id.length > 0),
      '[queue-order] library rows must carry data-id (TESTIDS.libraryItem contract)',
    ).toBe(true);
    const selectionOrder = libraryOrder as string[];
    expect(
      selectionOrder.length,
      '[queue-order] rendered library holds fewer than 2 tracks — queue-order needs ≥2',
    ).toBeGreaterThanOrEqual(2);

    // Queue the ingested tracks in library-selection order (user gestures
    // only; nothing here calls play() programmatically).
    for (let index = 0; index < selectionOrder.length; index += 1) {
      await rows.nth(index).click();
    }

    // The queue must converge on exactly the selection order. The poll value
    // is the named verdict (or null): a missing queue or any divergence keeps
    // returning its reason instead of passing. See helpers/queue-order.ts.
    const readQueue = async (): Promise<string[] | null> => {
      const items = page.getByTestId(QUEUE_ITEM);
      if ((await items.count()) === 0) return null;
      const ids = await items.evaluateAll((elements) =>
        elements.map(
          (element) => element.getAttribute('data-track-id') ?? element.getAttribute('data-id'),
        ),
      );
      if (ids.some((id) => typeof id !== 'string' || (id as string).length === 0)) return null;
      return ids as string[];
    };
    await expect
      .poll(async () => queueOrderFailure(selectionOrder, await readQueue()), {
        timeout: 15_000,
        message:
          'player queue did not converge on library-selection order ' +
          '(missing queue-item/now-playing/player-next hooks or a divergent order)',
      })
      .toBeNull();

    // Advancing the queue must surface the second selected track. Titles are
    // read from the rendered rows (text before the " — artist" suffix).
    const secondRowText = (await rows.nth(1).innerText().catch(() => '')).trim();
    const secondTitle = secondRowText.split('—')[0]?.trim() || selectionOrder[1]!;
    await page.getByTestId(PLAYER_NEXT).click();
    await expect(
      page.getByTestId(NOW_PLAYING),
      `advancing the queue must surface the second selected track (${NOW_PLAYING} naming it)`,
    ).toContainText(secondTitle, { timeout: 15_000 });

    expect(pageErrorNames, 'page errors during queue-order playback').toEqual([]);
  });
});
