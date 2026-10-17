import { test, expect } from '@playwright/test';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  API,
  PLAN_LIMIT_STATUSES,
  TESTIDS,
  extractPlanMeta,
  extractUsageBytes,
  uniqueTestEmail,
} from '../helpers/contract.js';
import { assertNoAccessWall, readApiBody, stagingBaseUrl } from '../helpers/access.js';
import { playbackFailure, type PlaybackEvidence } from '../helpers/playback.js';
import {
  accountHeaders,
  apiGet,
  applyAccountToPage,
  completeByopWizard,
  requireApiAccount,
  tryMintTestAccount,
  type TestAccount,
} from '../helpers/accounts.js';

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

  // Serial file: later tests reuse the ingest test's account so streaming
  // and metering run as the same identity. Retries mint a fallback.
  let sharedAccount: TestAccount | null = null;

  test('NZB fixture ingests and appears in the library', async ({ page, request }) => {
    const baseURL = stagingBaseUrl('skip', 'ingest');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const minted = await tryMintTestAccount(request, baseURL);
    await applyAccountToPage(page, minted, baseURL);
    if (minted?.viaApi) {
      sharedAccount = minted;
      await page.goto(baseURL, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    } else {
      // Real UI signup fallback (not a bare navigation): fill the form and
      // require the wizard. The browser identity is authenticated from here
      // via storage; API-driven steps below additionally require the seam —
      // see requireApiAccount.
      const email = uniqueTestEmail();
      await page.goto(`${baseURL}/signup`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
      await expect(page.getByTestId(TESTIDS.signupEmail)).toBeVisible({ timeout: 15_000 });
      await page.getByTestId(TESTIDS.signupEmail).fill(email);
      await page.getByTestId(TESTIDS.signupSubmit).click();
      const wizard = page.getByTestId(TESTIDS.byopHost);
      const error = page.getByTestId(TESTIDS.signupError);
      await expect(async () => {
        const onWizard = await wizard.isVisible().catch(() => false);
        const onError = await error.isVisible().catch(() => false);
        expect(onWizard || onError, 'neither BYOP wizard nor signup error appeared').toBe(true);
      }).toPass({ timeout: 30_000 });
      expect(await error.isVisible().catch(() => false), 'signup rejected the test mailbox').toBe(
        false,
      );
      expect(page.url()).toContain('/onboarding/byop');
      sharedAccount = { email, viaApi: false };
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

  test('ingested library persists across reload without re-import', async ({ page, request }) => {
    const baseURL = stagingBaseUrl('skip', 'reload');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    // Fail closed instead of asserting anonymously when neither a minted
    // session nor the QA seam token exists.
    requireApiAccount(account, 'reload');
    const headers = accountHeaders(account);
    const libraryUrl = `${baseURL}${API.library}`;
    const library = await apiGet(request, libraryUrl, headers, 'reload');
    const libraryBody = await readApiBody(library, libraryUrl, 'reload');
    expect(library.ok(), `library listing failed: HTTP ${library.status()}`).toBe(true);
    const items = JSON.parse(libraryBody) as Array<{ id?: string }>;
    const trackId = Array.isArray(items) ? items[0]?.id : undefined;
    expect(trackId, 'library is empty — ingest spec must run first').toBeTruthy();

    await applyAccountToPage(page, account, baseURL);
    await page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    const libraryItems = page.getByTestId(TESTIDS.libraryItem);
    await expect(
      libraryItems.first(),
      'ingested track missing from library before reload',
    ).toBeVisible({ timeout: 60_000 });
    const before = await libraryItems.evaluateAll((els) =>
      els.map((el) => ({
        id: el.getAttribute('data-id'),
        text: (el.textContent ?? '').trim(),
      })),
    );
    const beforeEntry = before.find((entry) => entry.id === trackId);
    expect(
      beforeEntry,
      `ingested track ${trackId} missing from library before reload`,
    ).toBeTruthy();
    expect(
      beforeEntry?.text.length ?? 0,
      `ingested track ${trackId} has no visible title before reload`,
    ).toBeGreaterThan(0);

    // No re-import step here by design: no /library/import visit, no
    // setInputFiles — a plain reload must keep the ingested track.
    await page.reload({ waitUntil: 'domcontentloaded', timeout: 30_000 });
    const reloadedItems = page.getByTestId(TESTIDS.libraryItem);
    await expect(
      reloadedItems.first(),
      `ingested track ${trackId} missing after reload without re-import`,
    ).toBeVisible({ timeout: 60_000 });
    const after = await reloadedItems.evaluateAll((els) =>
      els.map((el) => ({
        id: el.getAttribute('data-id'),
        text: (el.textContent ?? '').trim(),
      })),
    );
    const afterEntry = after.find((entry) => entry.id === trackId);
    expect(
      afterEntry,
      `ingested track ${trackId} missing after reload without re-import`,
    ).toBeTruthy();
    expect(
      afterEntry?.text,
      `ingested track ${trackId} text changed across reload`,
    ).toBe(beforeEntry?.text);
  });

  test('streaming answers byte Ranges with 206 + Content-Range', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'range');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    // Fail closed instead of probing anonymously when neither a minted
    // session nor the QA seam token exists.
    requireApiAccount(account, 'range');
    const headers = accountHeaders(account);
    const libraryUrl = `${baseURL}${API.library}`;
    const library = await apiGet(request, libraryUrl, headers, 'range');
    const libraryBody = await readApiBody(library, libraryUrl, 'range');
    expect(library.ok(), `library listing failed: HTTP ${library.status()}`).toBe(true);
    const items = JSON.parse(libraryBody) as Array<{ id?: string }>;
    const trackId = Array.isArray(items) ? items[0]?.id : undefined;
    expect(trackId, 'library is empty — ingest spec must run first').toBeTruthy();

    // Binary success bodies are probed headers-only (no megabyte buffering);
    // a 206 with Content-Range from the app is self-evidently not a wall.
    const probeHeaders = (
      status: number,
      responseHeaders: Record<string, string>,
      url: string,
    ): void =>
      assertNoAccessWall({ status, headers: responseHeaders, bodyText: '', url }, 'range');

    const head = await apiGet(
      request,
      `${baseURL}${API.stream(trackId as string)}`,
      { ...headers, Range: 'bytes=0-8' },
      'range',
    );
    probeHeaders(head.status(), head.headers(), `${baseURL}${API.stream(trackId as string)}`);
    expect(head.status(), 'first Range seek did not return 206').toBe(206);
    expect(head.headers()['content-range'] !== undefined).toBe(true);
    expect(Number(head.headers()['content-length'])).toBe(9);

    const seek = await apiGet(
      request,
      `${baseURL}${API.stream(trackId as string)}`,
      { ...headers, Range: 'bytes=9-17' },
      'range',
    );
    probeHeaders(seek.status(), seek.headers(), `${baseURL}${API.stream(trackId as string)}`);
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
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    requireApiAccount(account, 'meter');
    const headers = accountHeaders(account);
    const usageUrl = `${baseURL}${API.usage}`;
    const beforeRes = await apiGet(request, usageUrl, headers, 'meter');
    const beforeBody = await readApiBody(beforeRes, usageUrl, 'meter');
    expect(beforeRes.ok(), `usage read failed: HTTP ${beforeRes.status()}`).toBe(true);
    const before = extractUsageBytes(JSON.parse(beforeBody));

    // Deterministic traffic AFTER the baseline: one API Range GET for the
    // ingested track. The previous test's seeks ran before `before` was read
    // and cannot be the source of `after > before`.
    const libraryUrl = `${baseURL}${API.library}`;
    const library = await apiGet(request, libraryUrl, headers, 'meter');
    const libraryBody = await readApiBody(library, libraryUrl, 'meter');
    expect(library.ok(), `library listing failed: HTTP ${library.status()}`).toBe(true);
    const items = JSON.parse(libraryBody) as Array<{ id?: string }>;
    const trackId = Array.isArray(items) ? items[0]?.id : undefined;
    expect(trackId, 'meter needs the ingested track — ingest spec must run first').toBeTruthy();
    const streamUrl = `${baseURL}${API.stream(trackId as string)}`;
    const rangeRes = await apiGet(request, streamUrl, { ...headers, Range: 'bytes=0-0' }, 'meter');
    assertNoAccessWall(
      { status: rangeRes.status(), headers: rangeRes.headers(), bodyText: '', url: streamUrl },
      'meter',
    );
    expect(rangeRes.status(), 'deterministic Range GET did not return 206').toBe(206);

    // The UI player is an additional signal on top of the deterministic API
    // traffic above: wait for it properly instead of an immediate count()
    // gate that misses client-rendered players, with no fixed sleep as the
    // assertion mechanism.
    await applyAccountToPage(page, account, baseURL);
    await page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    const player = page.getByTestId(TESTIDS.audioPlayer);
    const playerVisible = await player
      .waitFor({ state: 'visible', timeout: 10_000 })
      .then(() => true)
      .catch(() => false);
    if (playerVisible) {
      await page.getByTestId(TESTIDS.libraryItem).first().click().catch(() => {});
      await page.waitForTimeout(2_000);
    }

    await expect(async () => {
      const res = await apiGet(request, usageUrl, headers, 'meter');
      const body = await readApiBody(res, usageUrl, 'meter');
      expect(res.ok()).toBe(true);
      expect(extractUsageBytes(JSON.parse(body))).toBeGreaterThan(before);
    }).toPass({ timeout: 60_000 });

    const metaRes = await apiGet(request, usageUrl, headers, 'meter');
    const metaBody = await readApiBody(metaRes, usageUrl, 'meter');
    expect(metaRes.ok(), `usage read failed: HTTP ${metaRes.status()}`).toBe(true);
    const meta = extractPlanMeta(JSON.parse(metaBody));
    expect(meta.used).toBeLessThanOrEqual(meta.limit);
    await expect(page.getByTestId(TESTIDS.usageMeter).first()).toBeVisible({ timeout: 15_000 });
  });

  test('over-limit staging account is refused with a plan-limit status', async ({
    page,
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'plan-limit');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Deterministic enforcement needs a capped plan on a throwaway account —
    // a staging-only test seam owned by the metering card. Never hammer a
    // real account into over-limit: that would probe a mutating path.
    // Fail-closed on CI: a skip here would leave plan-limit enforcement (an
    // acceptance criterion) never exercised while the job stays green.
    let capped: TestAccount | null;
    try {
      capped = await tryMintTestAccount(request, baseURL, 'overlimit');
    } catch (error) {
      if (!process.env.CI) {
        test.skip(true, `over-limit seam errored locally: ${(error as Error).message}`);
        return;
      }
      throw error;
    }
    if (!capped) {
      const reason =
        'over-limit seed seam unavailable (needs QA_AUTH_TOKEN + POST /api/test-accounts ' +
        '{plan:"overlimit"} from the metering card) — refusing to pass silently on CI.';
      if (process.env.CI) throw new Error(`[plan-limit] ${reason}`);
      test.skip(true, `${reason} Usage/plan metadata above still verified.`);
      return;
    }
    // Stream AS the capped account: its session (not the default identity)
    // must be the credential on these calls, or a 206 would falsely pass.
    const headers = accountHeaders(capped);
    const libraryUrl = `${baseURL}${API.library}`;
    const readTrackId = async (): Promise<string | undefined> => {
      const library = await apiGet(request, libraryUrl, headers, 'plan-limit');
      const libraryBody = await readApiBody(library, libraryUrl, 'plan-limit');
      let items: Array<{ id?: string }>;
      try {
        items = JSON.parse(libraryBody) as Array<{ id?: string }>;
      } catch {
        items = [];
      }
      return Array.isArray(items) ? items[0]?.id : undefined;
    };
    let trackId = await readTrackId();
    if (!trackId) {
      // No pre-seeded track is required from the seam: ingest the fixture AS
      // the capped account (BYOP first, since ingest needs the provider
      // connection) and re-list. Anything still empty is a named seam
      // precondition failure, not a silent pass.
      await applyAccountToPage(page, capped, baseURL);
      await completeByopWizard(page, baseURL);
      await page.goto(`${baseURL}/library/import`, {
        waitUntil: 'domcontentloaded',
        timeout: 30_000,
      });
      const fileInput = page.getByTestId(TESTIDS.importFile);
      await expect(fileInput, 'NZB import control missing').toBeAttached({ timeout: 15_000 });
      await fileInput.setInputFiles(FIXTURE_NZB);
      const importStatus = page.getByTestId(TESTIDS.importStatus);
      await expect(importStatus, 'import gave no status').toBeVisible({ timeout: 60_000 });
      await expect(importStatus, 'NZB ingest did not complete').toContainText(
        /complete|ready|done/i,
        { timeout: 240_000 },
      );
      trackId = await readTrackId();
    }
    expect(
      trackId,
      'library is empty for the capped account even after ingesting the fixture as that ' +
        'account (the overlimit seam must allow ingest or pre-seed a streamable track)',
    ).toBeTruthy();
    const streamUrl = `${baseURL}${API.stream(trackId as string)}`;
    const res = await apiGet(request, streamUrl, { ...headers, Range: 'bytes=0-8' }, 'plan-limit');
    if (!((PLAN_LIMIT_STATUSES as readonly number[]).includes(res.status()))) {
      // Only read the body on the unexpected path: a wall here must name
      // itself instead of surfacing as a bare status mismatch below.
      await readApiBody(res, streamUrl, 'plan-limit');
    }
    expect(
      (PLAN_LIMIT_STATUSES as readonly number[]).includes(res.status()),
      `expected a plan-limit status (${PLAN_LIMIT_STATUSES.join('/')}), got HTTP ${res.status()}`,
    ).toBe(true);
  });

  test('synthetic audio actually plays in the browser', async ({ page, request }) => {
    const baseURL = stagingBaseUrl('skip', 'playback');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Same identity discipline as Range/meter: playback must run as the
    // ingesting account, never anonymously or as the generic QA seam.
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    requireApiAccount(account, 'playback');

    // Names only: a page-error message can embed the staging origin or other
    // values that must stay out of public CI artifacts.
    const pageErrorNames: string[] = [];
    page.on('pageerror', (error) => pageErrorNames.push(error.name || 'Error'));

    await applyAccountToPage(page, account, baseURL);
    await page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    const item = page.getByTestId(TESTIDS.libraryItem).first();
    await expect(item, 'synthetic staged track missing from library — ingest spec must run first').toBeVisible({
      timeout: 60_000,
    });

    // User gesture #1: selecting the staged synthetic track in the library.
    // A player may start on selection or only load the track; both are
    // handled below, but nothing here calls play() programmatically.
    await item.click();
    const audio = page.getByTestId(TESTIDS.audioPlayer);
    await expect(audio, 'selecting the library track did not show the audio player').toBeVisible({
      timeout: 15_000,
    });

    // Record playing/timeupdate evidence on the element itself before any
    // play gesture: the synthetic WAV is sub-second, so polling alone could
    // sample only after it has already ended. Booleans/numbers only — the
    // source URL is never read out of the page.
    await audio.evaluate((element) => {
      const player = element as HTMLAudioElement & {
        __nnPlayback?: { sawPlaying: boolean; maxCurrentTime: number };
      };
      const recorder = {
        sawPlaying: !player.paused && !player.ended,
        maxCurrentTime: Number.isFinite(player.currentTime) ? player.currentTime : 0,
      };
      player.__nnPlayback = recorder;
      player.addEventListener('playing', () => {
        recorder.sawPlaying = true;
        recorder.maxCurrentTime = Math.max(recorder.maxCurrentTime, player.currentTime || 0);
      });
      player.addEventListener('timeupdate', () => {
        recorder.maxCurrentTime = Math.max(recorder.maxCurrentTime, player.currentTime || 0);
      });
    });
    const snapshot = (): Promise<PlaybackEvidence> =>
      audio.evaluate((element) => {
        const player = element as HTMLAudioElement & {
          __nnPlayback?: { sawPlaying: boolean; maxCurrentTime: number };
        };
        const recorder = player.__nnPlayback ?? { sawPlaying: false, maxCurrentTime: 0 };
        return {
          sawPlaying: recorder.sawPlaying,
          maxCurrentTime: Math.max(recorder.maxCurrentTime, player.currentTime || 0),
          paused: player.paused,
          ended: player.ended,
          currentTime: player.currentTime,
          duration: player.duration,
          readyState: player.readyState,
          errorCode: player.error ? player.error.code : null,
          hasSource: Boolean(player.currentSrc),
        };
      });

    await expect
      .poll(async () => (await snapshot()).hasSource, {
        timeout: 15_000,
        message: 'audio player never loaded the synthetic track after selection',
      })
      .toBe(true);

    let before = await snapshot();
    if (playbackFailure(before, 0) !== null) {
      let baseline: number;
      if (before.paused) {
        // Reset the recorder so only the gesture below can produce the
        // evidence (an ended element replays from 0; a paused one resumes
        // from its baseline). This also covers selection-autoplay of the
        // sub-second track ending before the recorder above was installed:
        // the missed first play is discarded and the replay below is the
        // measured one. Re-install if the player recreated the element
        // since the recorder was attached (a fresh node has no expando).
        baseline = await audio.evaluate((element) => {
          const player = element as HTMLAudioElement & {
            __nnPlayback?: { sawPlaying: boolean; maxCurrentTime: number };
          };
          const start = player.ended ? 0 : player.currentTime || 0;
          if (!player.__nnPlayback) {
            const recorder = { sawPlaying: false, maxCurrentTime: start };
            player.__nnPlayback = recorder;
            player.addEventListener('playing', () => {
              recorder.sawPlaying = true;
              recorder.maxCurrentTime = Math.max(recorder.maxCurrentTime, player.currentTime || 0);
            });
            player.addEventListener('timeupdate', () => {
              recorder.maxCurrentTime = Math.max(recorder.maxCurrentTime, player.currentTime || 0);
            });
          } else {
            player.__nnPlayback.sawPlaying = false;
            player.__nnPlayback.maxCurrentTime = start;
          }
          return start;
        });
        // User gesture #2: the player's own visible play control. Native
        // <audio controls> live in shadow DOM, and in the Desktop Chrome
        // project the leftmost control is play/pause — clicking the element
        // centre would hit the seek bar instead. This respects autoplay
        // policy; no evaluate-driven play() call is made.
        const box = await audio.boundingBox();
        expect(box, 'audio player is visible but has no clickable controls').not.toBeNull();
        await audio.click({ position: { x: 20, y: Math.floor(((box?.height ?? 40) / 2)) } });
      } else {
        // Already playing but not yet past 0: require further advancement
        // rather than toggling it paused with another click.
        baseline = before.maxCurrentTime;
      }
      // The poll value is the named playback failure (or null): a 206, a
      // loaded element, or a paused element keeps returning its reason
      // instead of passing. See helpers/playback.ts + tests/playback.spec.ts.
      await expect
        .poll(async () => playbackFailure(await snapshot(), baseline), {
          timeout: 15_000,
          message: 'synthetic audio did not enter the playing state and advance',
        })
        .toBeNull();
      before = await snapshot();
    }
    expect(playbackFailure(before, 0), 'final playback evidence regressed').toBeNull();
    expect(pageErrorNames, 'page errors during browser playback').toEqual([]);
  });

});
