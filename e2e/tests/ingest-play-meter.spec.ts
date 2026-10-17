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

  // Player-UI seek, as distinct from the API Range control above: that test
  // proves the stream endpoint answers byte ranges; this one proves the
  // user-visible player actually moves the active track's playback position
  // when a person seeks, and can play from there. The seek is a click on the
  // native controls timeline (the contract markup is `<audio controls>`),
  // never a `currentTime` assignment and never a test-issued Range GET.
  //
  // Last in the serial chain on purpose: the library player is still a
  // contract placeholder, so on configured staging this test fails by name
  // until the player slice lands — placing it last keeps that expected red
  // from suppressing the meter/plan-limit evidence above it.
  //
  // The seek is performed while PAUSED. That is what makes the assertion
  // non-vacuous: a paused position cannot drift into the seeked region by
  // natural playback advance, so arriving there while still paused can only
  // be the seek. It also works with the pinned 210 ms synthetic WAV
  // (crates/nntune-test-nntp fixtures), where seek-while-playing would race
  // the end of the track.
  test('player UI seek moves the active audio position and playback continues', async ({
    page,
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'player-seek');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    // Fail closed instead of driving the player anonymously when neither a
    // minted session nor the QA seam token exists.
    requireApiAccount(account, 'player-seek');

    const pageErrors: string[] = [];
    page.on('pageerror', (error) => pageErrors.push(error.message));

    await applyAccountToPage(page, account, baseURL);
    await page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    const firstTrack = page.getByTestId(TESTIDS.libraryItem).first();
    await expect(
      firstTrack,
      'ingested track missing from library — ingest spec must run first',
    ).toBeVisible({ timeout: 60_000 });
    await firstTrack.click();

    const player = page.getByTestId(TESTIDS.audioPlayer);
    await expect(
      player,
      'clicking the ingested track did not surface the audio player',
    ).toBeVisible({ timeout: 15_000 });

    const audioState = (): Promise<{
      currentTime: number;
      duration: number;
      paused: boolean;
      ended: boolean;
      errorCode: number | null;
      hasSource: boolean;
      hasControls: boolean;
    }> =>
      player.evaluate((element) => {
        const audio = element as HTMLAudioElement;
        return {
          currentTime: audio.currentTime,
          duration: audio.duration,
          paused: audio.paused,
          ended: audio.ended,
          errorCode: audio.error?.code ?? null,
          hasSource: audio.currentSrc !== '',
          hasControls: audio.controls,
        };
      });

    // Metadata must load and give a finite, seekable duration. NaN (no
    // metadata yet) and Infinity (unbounded stream) are reported as not
    // ready; any positive finite duration works, including the 210 ms
    // fixture, because the seek below happens while paused.
    await expect
      .poll(
        async () => {
          const { duration } = await audioState();
          return Number.isFinite(duration) ? duration : 0;
        },
        {
          timeout: 30_000,
          message: 'player never reported a finite duration for the ingested track',
        },
      )
      .toBeGreaterThan(0);
    const ready = await audioState();
    expect(ready.hasSource, 'player has no media source for the selected track').toBe(true);
    expect(ready.errorCode, 'player reported a media error before seeking').toBeNull();
    expect(
      ready.hasControls,
      'player no longer exposes native controls — update this UI seek to the new controls',
    ).toBe(true);

    const box = await player.boundingBox();
    expect(box, 'player has no layout box to click').not.toBeNull();
    const { width, height } = box as { width: number; height: number };
    // The native play/pause toggle sits at the left edge of the control bar.
    const togglePosition = { x: 16, y: Math.round(height / 2) };

    // Reach a paused state through the UI: selecting the track may autoplay
    // (the 210 ms fixture can also have already ended, which reads paused).
    if (!ready.paused) {
      await player.click({ position: togglePosition });
    }
    await expect
      .poll(async () => (await audioState()).paused, {
        timeout: 15_000,
        message: 'player did not reach a paused state before the seek',
      })
      .toBe(true);
    const beforeSeek = await audioState();

    // User-visible seek: click the timeline a little past its middle. The
    // timeline does not span the full element width (play button and time
    // labels flank it), so assert the requested REGION — well past the
    // opening stretch, short of the end — rather than an exact timestamp.
    // `currentTime` is the position the native timeline renders; there is
    // no separate visible-position hook.
    await player.click({
      position: { x: Math.round(width * 0.55), y: Math.round(height / 2) },
    });

    const regionLow = beforeSeek.duration * 0.3;
    const regionHigh = beforeSeek.duration * 0.95;
    const minMove = beforeSeek.duration * 0.1;
    await expect
      .poll(
        async () => {
          const state = await audioState();
          // Still-paused is part of the match: while paused, only the seek
          // itself can have moved the position into the region.
          return (
            state.paused &&
            state.currentTime >= regionLow &&
            state.currentTime <= regionHigh &&
            Math.abs(state.currentTime - beforeSeek.currentTime) > minMove
          );
        },
        {
          timeout: 15_000,
          message: 'seeking via the player UI did not move the paused audio position',
        },
      )
      .toBe(true);
    const afterSeek = await audioState();
    expect(afterSeek.errorCode, 'player reported a media error after seeking').toBeNull();

    // Playback can continue from the seeked position: resume through the UI
    // toggle and require the position to advance past where the seek left
    // it (on the 210 ms fixture this is satisfied by playing out to the
    // end, where currentTime reaches duration).
    await player.click({ position: togglePosition });
    await expect
      .poll(async () => (await audioState()).currentTime, {
        timeout: 15_000,
        message: 'playback did not continue from the seeked position',
      })
      .toBeGreaterThan(afterSeek.currentTime);
    const continued = await audioState();
    expect(continued.errorCode, 'player reported a media error after resuming').toBeNull();
    expect(pageErrors, `page errors during player seek: ${pageErrors.join('; ')}`).toEqual([]);
  });
});
