import { test, expect, type APIRequestContext } from '@playwright/test';
import {
  API,
  TESTIDS,
  assertPlanLimitRefusal,
  extractLibraryItems,
  extractLimitCode,
  extractPlanMeta,
  extractRecordingMbids,
  extractUsageBytes,
  type NamedSecret,
} from '../helpers/contract.js';
import { assertNoAccessWall, readApiBody, stagingBaseUrl } from '../helpers/access.js';
import { playbackFailure, type PlaybackEvidence } from '../helpers/playback.js';
import {
  mediaSessionFailure,
  type ExpectedTrack,
  type MediaSessionSnapshot,
} from '../helpers/media-session.js';
import {
  resumeFloorAfterSeek,
  seekProofFailure,
  seekViaPlayerUi,
  togglePlayViaPlayerUi,
} from '../helpers/player-ui.js';
import {
  accountHeaders,
  annotateSignupPath,
  apiGet,
  applyAccountToPage,
  requireApiAccount,
  submitPublicUiSignup,
  tryMintTestAccount,
  type TestAccount,
  waitForHydration,
} from '../helpers/accounts.js';
import {
  REAL_NZB_RAR_SKIP,
  realNzbActive,
  requireRealNzbFixture,
} from '../helpers/real-nzb-fixture.js';

// Golden path part 2 (parked under TWO-1635): user NZB ingest is retired
// (TWO-1630), so every leg that needs a user-uploaded track — ingest,
// reload, Range, meter, browser playback, seek and Media Session — skips by
// name (see USER_NZB_RETIRED_SKIP plus the journey gate allow-list in
// e2e/scripts/enforce-journey-evidence.mjs). The assertions stay in the file
// and return via admin-ingested tracks once admin ingest can seed a curated
// track on staging. The only live leg in this file is the stream plan-limit
// refusal on a separately minted over-limit account (the ingest-refusal leg
// is deleted: user ingest caps are retired with user ingest itself).

// Golden path part 2: ingest → stream with Range seeks → usage meter moves
// → plan metadata stays consistent → browser playback, seek and Media
// Session → a separately minted over-limit staging account is refused on
// stream with the documented plan-limit status (headers, code, upgrade CTA,
// no leaks). The plan-limit leg runs last in its isolated group: it fails
// red-by-design until enforcement lands, and in describe.serial a failure
// skips every later test in the same group.
//
// File-level skip: pre-deploy (no STAGING_BASE_URL) the whole file skips
// without launching a browser. See health.spec.ts.
test.skip(
  !process.env.STAGING_BASE_URL,
  'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.',
);

// A capped plan on a throwaway account — a staging-only test seam owned
// by the metering card. Never hammer a real account into over-limit: that
// would probe a mutating path. Fail-closed everywhere (locally and on CI):
// a skip here would leave plan-limit enforcement (an acceptance criterion)
// never exercised while the job stays green.
async function mintCappedAccount(
  request: APIRequestContext,
  baseURL: string,
  context: string,
): Promise<TestAccount> {
  let capped: TestAccount | null;
  try {
    capped = await tryMintTestAccount(request, baseURL, 'overlimit');
  } catch (error) {
    throw new Error(
      `[${context}] over-limit seam errored: ${(error as Error).message} — ` +
        'the metering card\'s POST /api/test-accounts {plan:"overlimit"} must be ' +
        'reachable; refusing to pass silently.',
    );
  }
  if (!capped) {
    throw new Error(
      `[${context}] over-limit seed seam unavailable (needs QA_AUTH_TOKEN + ` +
        'POST /api/test-accounts {plan:"overlimit"} from the metering card) — ' +
        'refusing to pass silently.',
    );
  }
  requireApiAccount(capped, context);
  return capped;
}

// Values that must never appear in a 402/429 body (names only in
// diagnostics — see secretLeak in helpers/contract.ts).
function cappedSecrets(capped: TestAccount): NamedSecret[] {
  const secrets: NamedSecret[] = [{ name: 'account email', value: capped.email }];
  if (capped.session) secrets.push({ name: 'account session', value: capped.session });
  return secrets;
}

// TWO-1635: user NZB ingest retired (TWO-1630). Every leg below that needs a
// user-uploaded track carries this skip; the journey gate allow-lists each
// parked title by name (e2e/scripts/enforce-journey-evidence.mjs). Un-park
// (remove the guard here and the allow-list entry there) once admin ingest
// can seed a curated track on staging. The predicate stays a call (rather
// than inlining `test.skip(true, ...)` plus `return`) so the retained
// assertions below stay reachable and keep typechecking — an unconditional
// skip+return would make them unreachable and drop narrowing.
const USER_NZB_RETIRED_SKIP =
  'user NZB ingest retired (TWO-1630); reseeds when admin ingest can seed a curated track on staging';

function userNzbIngestRetired(): boolean {
  return true;
}

test.describe.serial('ingest → play → meter → browser', () => {
  // Ingest polling (queue + NNTP fetch + verify) can take minutes on staging.
  test.describe.configure({ timeout: 300_000 });

  // Serial file: later tests reuse the ingest test's account so streaming
  // and metering run as the same identity. Retries mint a fallback.
  let sharedAccount: TestAccount | null = null;

  test('NZB fixture ingests and appears in the library', async ({ page, request }, testInfo) => {
    const baseURL = stagingBaseUrl('skip', 'ingest');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Parked: user NZB ingest retired (see USER_NZB_RETIRED_SKIP).
    if (userNzbIngestRetired()) {
      test.skip(true, USER_NZB_RETIRED_SKIP);
      return;
    }
    // Real-NZB mode cannot complete ingest: the staged RAR subset has no
    // decodable audio and the pipeline fails it permanent at transcode
    // (zero transcode inputs, no library row) — parked by name until RAR
    // read lands. Live provider proof in this mode is the provider 430
    // control (the BYOP wizard's test-connection retired with it).
    if (realNzbActive()) {
      test.skip(true, REAL_NZB_RAR_SKIP);
      return;
    }
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

    await page.goto(`${baseURL}/library/import`, {
      waitUntil: 'domcontentloaded',
      timeout: 30_000,
    });
    await waitForHydration(page, 'ingest');
    const fileInput = page.getByTestId(TESTIDS.importFile);
    await expect(fileInput, 'NZB import control missing').toBeAttached({ timeout: 15_000 });
    // Names-only failure: Playwright prints the input path when the upload
    // control rejects the file, and junit goes to public artifacts — never
    // leak the staged path (RUNNER_TEMP layout) into the failure text.
    try {
      await fileInput.setInputFiles(requireRealNzbFixture());
    } catch {
      throw new Error(
        'NZB upload control rejected the staged fixture file (names only — ' +
          'path withheld from public artifacts).',
      );
    }
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
    // Parked: user NZB ingest retired (see USER_NZB_RETIRED_SKIP).
    if (userNzbIngestRetired()) {
      test.skip(true, USER_NZB_RETIRED_SKIP);
      return;
    }
    // No completable ingest in real-NZB mode means no library row to reload.
    if (realNzbActive()) {
      test.skip(true, REAL_NZB_RAR_SKIP);
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
    const items = extractLibraryItems(libraryBody);
    const trackId = items[0]?.id;
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
    // Parked: user NZB ingest retired (see USER_NZB_RETIRED_SKIP).
    if (userNzbIngestRetired()) {
      test.skip(true, USER_NZB_RETIRED_SKIP);
      return;
    }
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    // Fail closed instead of probing anonymously when neither a minted
    // session nor the QA seam token exists.
    if (realNzbActive()) {
      test.skip(true, REAL_NZB_RAR_SKIP);
      return;
    }
    requireApiAccount(account, 'range');
    const headers = accountHeaders(account);
    const libraryUrl = `${baseURL}${API.library}`;
    const library = await apiGet(request, libraryUrl, headers, 'range');
    const libraryBody = await readApiBody(library, libraryUrl, 'range');
    expect(library.ok(), `library listing failed: HTTP ${library.status()}`).toBe(true);
    const items = extractLibraryItems(libraryBody);
    const trackId = items[0]?.id;
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
    // Parked: user NZB ingest retired (see USER_NZB_RETIRED_SKIP).
    if (userNzbIngestRetired()) {
      test.skip(true, USER_NZB_RETIRED_SKIP);
      return;
    }
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    if (realNzbActive()) {
      test.skip(true, REAL_NZB_RAR_SKIP);
      return;
    }
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
    const items = extractLibraryItems(libraryBody);
    const trackId = items[0]?.id;
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
    await waitForHydration(page, 'meter');
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

  test('ingested audio actually plays in the browser', async ({ page, request }) => {
    const baseURL = stagingBaseUrl('skip', 'playback');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Parked: user NZB ingest retired (see USER_NZB_RETIRED_SKIP).
    if (userNzbIngestRetired()) {
      test.skip(true, USER_NZB_RETIRED_SKIP);
      return;
    }
    // Same identity discipline as Range/meter: playback must run as the
    // ingesting account, never anonymously or as the generic QA seam.
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    if (realNzbActive()) {
      test.skip(true, REAL_NZB_RAR_SKIP);
      return;
    }
    requireApiAccount(account, 'playback');

    // Names only: a page-error message can embed the staging origin or other
    // values that must stay out of public CI artifacts.
    const pageErrorNames: string[] = [];
    page.on('pageerror', (error) => pageErrorNames.push(error.name || 'Error'));

    await applyAccountToPage(page, account, baseURL);
    await page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    await waitForHydration(page, 'ingest');
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
        // User gesture #2: the player's own visible play control (custom
        // player-play-pause button, or the native toggle while controls are
        // still rendered — see helpers/player-ui.ts). Clicking the audio
        // element centre would hit the seek bar instead. This respects
        // autoplay policy; no evaluate-driven play() call is made.
        await togglePlayViaPlayerUi(page, audio);
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
          message: 'ingested audio did not enter the playing state and advance',
        })
        .toBeNull();
      before = await snapshot();
    }
    expect(playbackFailure(before, 0), 'final playback evidence regressed').toBeNull();
    expect(pageErrorNames, 'page errors during browser playback').toEqual([]);
  });

  // FIXMEd until the staging MB mirror can match the staged real NZB: the
  // secret-held RAR set derives its match query from real Usenet subjects,
  // and whether the shared MB mirror holds that release is unproven — an
  // empty mirror match keeps the library write empty by design
  // (library_write_for in workers/ingest/src/matching.rs drops recordings
  // without a release; the queue consumer resolves unmatched to Vec::new()).
  // Below playback and FIXMEd, so it can never fail or skip a later step.
  // Un-fixme path: confirm the mirror holds the real NZB's release (or seed
  // it in staging) once RAR-set ingest + matching evidence exists.
  test.fixme('ingested track carries recording_mbids', async ({ request }) => {
    const baseURL = stagingBaseUrl('skip', 'recording-mbids');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    // Fail closed instead of asserting anonymously when neither the
    // ingesting session nor the QA seam token exists.
    requireApiAccount(account, 'recording-mbids');
    const headers = accountHeaders(account);
    const libraryUrl = `${baseURL}${API.library}`;
    // The MB match + library write settle after the import status flips,
    // so poll the listing until the ingested track carries its recording
    // MBIDs. Every failure names the gap: transport/list shape, an empty
    // library, or the missing/empty/malformed fill itself (see
    // extractRecordingMbids in e2e/helpers/contract.ts).
    await expect(async () => {
      const library = await apiGet(request, libraryUrl, headers, 'recording-mbids');
      const libraryBody = await readApiBody(library, libraryUrl, 'recording-mbids');
      expect(library.ok(), `library listing failed: HTTP ${library.status()}`).toBe(true);
      const items = extractLibraryItems(libraryBody);
      expect(items.length, 'library is empty — ingest spec must run first').toBeGreaterThan(0);
      const mbids = extractRecordingMbids(items[0]);
      expect(
        mbids.length,
        '[recording-mbids] ingested track carries an empty recording_mbids list',
      ).toBeGreaterThan(0);
    }).toPass({ timeout: 60_000 });
  });

  // Player-UI seek, as distinct from the API Range control above: that test
  // proves the stream endpoint answers byte ranges; this one proves the
  // user-visible player actually moves the active track's playback position
  // when a person seeks, and can play from there. The seek is a pointer
  // press on the player's seek surface — `player-seek` slider in the custom
  // player bar, or the native controls timeline before it (see
  // helpers/player-ui.ts) — never a `currentTime` assignment and never a
  // test-issued Range GET.
  //
  // After the meter/plan-limit specs on purpose: the library player is still a
  // contract placeholder, so on configured staging this test fails by name
  // until the player slice lands — placing it after them keeps that expected
  // red from suppressing the meter/plan-limit evidence above it. Media Session
  // closes the chain after this test (it needs everything this test needs
  // plus applyMediaSession), so a Media Session failure cannot skip this
  // seek verdict.
  //
  // The seek is performed while PAUSED. That is what makes the assertion
  // non-vacuous: a paused position cannot drift into the seeked region by
  // natural playback advance, so arriving there while still paused can only
  // be the seek. (This leg skips by name in real-NZB mode — see
  // REAL_NZB_RAR_SKIP — so the note below covers non-staged runs only: the
  // short in-memory corpus tones keep seek-while-playing from racing the end
  // of the track.)
  test('player UI seek moves the active audio position and playback continues', async ({
    page,
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'player-seek');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Parked: user NZB ingest retired (see USER_NZB_RETIRED_SKIP).
    if (userNzbIngestRetired()) {
      test.skip(true, USER_NZB_RETIRED_SKIP);
      return;
    }
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    // Fail closed instead of driving the player anonymously when neither a
    // minted session nor the QA seam token exists.
    if (realNzbActive()) {
      test.skip(true, REAL_NZB_RAR_SKIP);
      return;
    }
    requireApiAccount(account, 'player-seek');

    const pageErrors: string[] = [];
    page.on('pageerror', (error) => pageErrors.push(error.message));

    await applyAccountToPage(page, account, baseURL);
    await page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    await waitForHydration(page, 'ingest');
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
    // No `controls` assertion: the seek surface (custom `player-seek`
    // slider or native timeline) is chosen at act time by
    // helpers/player-ui.ts, which fails by name when neither exists —
    // the custom player bar removing native controls is the plan, not a
    // failure. See e2e/README.md ("Player contract migration").

    // Reach a paused state through the UI: selecting the track may autoplay
    // (the 210 ms fixture can also have already ended, which reads paused).
    if (!ready.paused) {
      await togglePlayViaPlayerUi(page, player);
    }
    await expect
      .poll(async () => (await audioState()).paused, {
        timeout: 15_000,
        message: 'player did not reach a paused state before the seek',
      })
      .toBe(true);
    const beforeSeek = await audioState();

    // User-visible seek: press the seek surface a little past its middle.
    // The native timeline does not span the full element width (play
    // button and time labels flank it), so assert the requested REGION —
    // well past the opening stretch, short of the end — rather than an
    // exact timestamp (seekProofRegion, pinned offline by
    // tests/player-ui.spec.ts). `currentTime` is the position the player
    // UI renders; there is no separate visible-position hook.
    //
    // Movement is proven by a `seeked` event (counted by a listener installed
    // before the click), not by distance from the pre-seek position: with
    // selection-autoplay the pause can land arbitrarily close to where the
    // seek click lands, so a distance predicate can never become true even
    // when the seek worked.
    await player.evaluate((element) => {
      const audio = element as HTMLAudioElement & { __seeked?: number };
      audio.__seeked = 0;
      audio.addEventListener('seeked', () => {
        audio.__seeked = (audio.__seeked ?? 0) + 1;
      });
    });
    const seekTarget = await seekViaPlayerUi(page, player, 0.55);

    await expect
      .poll(
        async () => {
          const state = await audioState();
          const seeked = await player.evaluate(
            (element) =>
              (element as HTMLAudioElement & { __seeked?: number }).__seeked ?? 0,
          );
          return seekProofFailure(state, seekTarget.fraction, seeked > 0);
        },
        {
          timeout: 15_000,
          message: 'seeking via the player UI did not move the paused audio position',
        },
      )
      .toBeNull();
    const afterSeek = await audioState();
    expect(afterSeek.errorCode, 'player reported a media error after seeking').toBeNull();

    // Playback must continue from the seeked position: resume through the UI
    // toggle and require the position to advance past where the seek left
    // it (on the 210 ms fixture this is satisfied by playing out to the
    // end, where currentTime reaches duration). Advancing past the seek
    // alone is not enough — restarting the track from 0 and playing to the
    // end would also pass — so track the lowest position observed during
    // resumed playback and require playback never to have dropped back
    // toward the start.
    //
    // The floor is seeded with the paused seek position and then takes the
    // minimum over both the browser's `timeupdate` events and every poll
    // sample, so the check always has a finite sample: an `Infinity`
    // initial value would pass the floor comparison vacuously when the
    // clip ends before any `timeupdate` fires.
    await player.evaluate((element) => {
      const audio = element as HTMLAudioElement & { __resumeMin?: number };
      audio.__resumeMin = audio.currentTime;
      audio.addEventListener('timeupdate', () => {
        audio.__resumeMin = Math.min(audio.__resumeMin ?? Infinity, audio.currentTime);
      });
    });
    await togglePlayViaPlayerUi(page, player);
    let polledMin = Infinity;
    await expect
      .poll(
        async () => {
          const { currentTime } = await audioState();
          if (Number.isFinite(currentTime)) {
            polledMin = Math.min(polledMin, currentTime);
          }
          return currentTime;
        },
        {
          timeout: 15_000,
          message: 'playback did not continue from the seeked position',
        },
      )
      .toBeGreaterThan(afterSeek.currentTime);
    const eventMin = await player.evaluate(
      (element) =>
        (element as HTMLAudioElement & { __resumeMin?: number }).__resumeMin,
    );
    const resumeMin = Math.min(eventMin ?? Infinity, polledMin);
    expect(
      Number.isFinite(resumeMin),
      'no playback position was sampled during resumed playback',
    ).toBe(true);
    expect(
      resumeMin,
      'playback restarted instead of continuing from the seek',
    ).toBeGreaterThanOrEqual(resumeFloorAfterSeek(afterSeek.currentTime, beforeSeek.duration));
    const continued = await audioState();
    expect(continued.errorCode, 'player reported a media error after resuming').toBeNull();
    expect(pageErrors, `page errors during player seek: ${pageErrors.join('; ')}`).toEqual([]);
  });

  // Last in the serial chain: Media Session needs everything the seek test
  // needs plus applyMediaSession on the library page, so it runs after seek —
  // a Media Session failure cannot skip the M1 seek verdict above it.
  test('Media Session metadata reflects the playing ingested track', async ({ page, request }) => {
    const baseURL = stagingBaseUrl('skip', 'media-session');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    // Parked: user NZB ingest retired (see USER_NZB_RETIRED_SKIP).
    if (userNzbIngestRetired()) {
      test.skip(true, USER_NZB_RETIRED_SKIP);
      return;
    }
    // Same identity discipline as Range/meter/playback: the metadata must be
    // read as the ingesting account, never anonymously or as the generic QA
    // seam. Full playback proof stays with the playback spec — here a light
    // playing gate precedes the metadata read so the assertion runs during
    // browser playback, not against an idle element.
    const account = sharedAccount ?? (await tryMintTestAccount(request, baseURL));
    if (realNzbActive()) {
      test.skip(true, REAL_NZB_RAR_SKIP);
      return;
    }
    requireApiAccount(account, 'media-session');

    // Names only: a page-error message can embed the staging origin or other
    // values that must stay out of public CI artifacts.
    const pageErrorNames: string[] = [];
    page.on('pageerror', (error) => pageErrorNames.push(error.name || 'Error'));

    // Expected identity comes from the library API (deterministic), never
    // from the page: metadata must match the ingested track itself.
    const headers = accountHeaders(account);
    const libraryUrl = `${baseURL}${API.library}`;
    const library = await apiGet(request, libraryUrl, headers, 'media-session');
    const libraryBody = await readApiBody(library, libraryUrl, 'media-session');
    expect(library.ok(), `library listing failed: HTTP ${library.status()}`).toBe(true);
    let libraryItems: unknown;
    try {
      libraryItems = JSON.parse(libraryBody) as unknown;
    } catch {
      libraryItems = [];
    }
    const first = (Array.isArray(libraryItems) ? libraryItems : [])[0] as Record<string, unknown>;
    const titleOf = (value: unknown): string | null =>
      typeof value === 'string' && value.trim() !== '' ? value : null;
    const expectedTitle =
      titleOf(first?.title) ?? titleOf(first?.name) ?? titleOf(first?.track_title);
    expect(
      expectedTitle,
      'library API returned no usable track title (keys: ' +
        `${Object.keys((first as object) ?? {}).join(', ') || '(empty)'}) — ` +
        'the contract drifted; update the title keys + this spec together',
    ).toBeTruthy();
    const expected: ExpectedTrack = {
      title: expectedTitle as string,
      artist: titleOf(first?.artist) ?? titleOf(first?.artist_name),
    };

    await applyAccountToPage(page, account, baseURL);
    await page.goto(`${baseURL}/library`, { waitUntil: 'domcontentloaded', timeout: 30_000 });
    await waitForHydration(page, 'ingest');
    const item = page.getByTestId(TESTIDS.libraryItem).first();
    await expect(
      item,
      'ingested track missing from library — ingest spec must run first',
    ).toBeVisible({ timeout: 60_000 });
    await item.click();
    const audio = page.getByTestId(TESTIDS.audioPlayer);
    await expect(
      audio,
      'selecting the library track did not show the audio player (player slice must land first)',
    ).toBeVisible({ timeout: 15_000 });

    // Light playing gate (booleans/numbers only — the source URL is never
    // read out of the page): record playing/timeupdate evidence, use the
    // player's own visible play control as the gesture, then require an
    // advancing element. The synthetic track is sub-second, so ended with
    // recorded advancement counts — ended with none does not.
    await audio.evaluate((element) => {
      const player = element as HTMLAudioElement & {
        __nnMediaSession?: { sawPlaying: boolean; maxTime: number };
      };
      const recorder = {
        sawPlaying: !player.paused && !player.ended,
        maxTime: Number.isFinite(player.currentTime) ? player.currentTime : 0,
      };
      player.__nnMediaSession = recorder;
      player.addEventListener('playing', () => {
        recorder.sawPlaying = true;
        recorder.maxTime = Math.max(recorder.maxTime, player.currentTime || 0);
      });
      player.addEventListener('timeupdate', () => {
        recorder.maxTime = Math.max(recorder.maxTime, player.currentTime || 0);
      });
    });
    // The player's own visible play control (either generation — see
    // helpers/player-ui.ts), never an element-centre click (that seeks).
    await togglePlayViaPlayerUi(page, audio);
    await expect
      .poll(
        async () =>
          audio.evaluate((element) => {
            const player = element as HTMLAudioElement & {
              __nnMediaSession?: { sawPlaying: boolean; maxTime: number };
            };
            const recorder = player.__nnMediaSession ?? { sawPlaying: false, maxTime: 0 };
            const maxTime = Math.max(recorder.maxTime, player.currentTime || 0);
            const errored = player.error !== null;
            const advancing =
              recorder.sawPlaying || !player.paused || player.ended ? maxTime > 0 : false;
            return !errored && advancing && Number.isFinite(player.duration) && player.duration > 0;
          }),
        {
          timeout: 15_000,
          message:
            'audio element never entered an advancing playing state for the Media Session read',
        },
      )
      .toBe(true);

    // Metadata is applied by a client effect after hydration: poll the pure
    // verdict (null = reflects the playing track). Titles/artists/artwork
    // URLs are never interpolated — the failure names the field, not values.
    const readMetadata = (): Promise<MediaSessionSnapshot | null> =>
      page.evaluate(() => {
        const metadata = navigator.mediaSession?.metadata;
        if (!metadata) return null;
        return {
          title: typeof metadata.title === 'string' ? metadata.title : null,
          artist: typeof metadata.artist === 'string' ? metadata.artist : null,
          artworkSrcs: (metadata.artwork ?? []).map((entry) => entry.src),
        };
      });
    await expect
      .poll(async () => mediaSessionFailure(await readMetadata(), expected), {
        timeout: 15_000,
        message: 'Media Session metadata does not reflect the playing ingested track',
      })
      .toBeNull();
    expect(
      mediaSessionFailure(await readMetadata(), expected),
      'final Media Session metadata regressed',
    ).toBeNull();
    expect(pageErrorNames, 'page errors during Media Session playback').toEqual([]);
  });
});

// Plan-limit leg in its own group AFTER the journey serial: it fails
// red-by-design until enforcement lands, and in describe.serial a failure
// skips every later test in the SAME group — isolating it here means a red
// plan-limit leg (or a red browser leg above) can never skip the other
// group's verdicts. It mints its own capped account (never the journey
// identity), so a capped-account failure implicates only this leg. The leg
// requires the overlimit seam to pre-seed a streamable track: user NZB
// ingest is retired (TWO-1630), so there is no user-ingest path left for a
// capped account to take (the ingest-refusal leg was deleted with it).
test.describe('plan limit', () => {
  test.describe.configure({ timeout: 300_000 });

  test('over-limit staging account is refused with a plan-limit status', async ({
    request,
  }) => {
    const baseURL = stagingBaseUrl('skip', 'plan-limit-stream');
    if (!baseURL) {
      test.skip(true, 'STAGING_BASE_URL not configured (pre-deploy) — see e2e/README.md.');
      return;
    }
    if (realNzbActive()) {
      // The capped stream leg needs a pre-seeded streamable track, which the
      // RAR set cannot produce until RAR read lands.
      test.skip(true, REAL_NZB_RAR_SKIP);
      return;
    }
    const capped = await mintCappedAccount(request, baseURL, 'plan-limit-stream');
    // Stream AS the capped account: its session (not the default identity)
    // must be the credential on these calls, or a 206 would falsely pass.
    const headers = accountHeaders(capped);
    const libraryUrl = `${baseURL}${API.library}`;
    const readTrackId = async (): Promise<string | undefined> => {
      const library = await apiGet(request, libraryUrl, headers, 'plan-limit-stream');
      const libraryBody = await readApiBody(library, libraryUrl, 'plan-limit-stream');
      let items: Array<Record<string, unknown>>;
      try {
        items = extractLibraryItems(libraryBody);
      } catch {
        // Fail fast when the listing itself is a limit refusal: without a
        // pre-seeded track there is nothing to stream, and the empty-library
        // message below would mislead.
        let code: string | null = null;
        try {
          code = extractLimitCode(libraryBody, 'plan-limit-stream');
        } catch {
          code = null;
        }
        if (code) {
          throw new Error(
            `[plan-limit-stream] library listing for the capped account is itself refused ` +
              `with limit code '${code}': the stream leg cannot reach a track — ` +
              `the overlimit seam must pre-seed a streamable track when listing is capped.`,
          );
        }
        items = [];
      }
      const trackId = items[0]?.id;
      return typeof trackId === 'string' ? trackId : undefined;
    };
    const trackId = await readTrackId();
    expect(
      trackId,
      'library is empty for the capped account: the overlimit seam must pre-seed ' +
        'a streamable track (user NZB ingest is retired, so no upload path can fill it)',
    ).toBeTruthy();
    const streamUrl = `${baseURL}${API.stream(trackId as string)}`;
    const res = await apiGet(
      request,
      streamUrl,
      { ...headers, Range: 'bytes=0-8' },
      'plan-limit-stream',
    );
    // The Range is 9 bytes, so reading the refusal body never buffers media.
    const bodyText = await readApiBody(res, streamUrl, 'plan-limit-stream');
    // The full refusal verdict: status, limit + reset headers (429 requires
    // retry-after), the status-consistent limit code with a present,
    // status-consistent retryable flag, the upgrade CTA pointer, and no
    // secret leakage. Without enforcement this fails naming the 206 (and
    // the missing headers); with enforcement it passes.
    assertPlanLimitRefusal({
      status: res.status(),
      headers: res.headers(),
      bodyText,
      secrets: cappedSecrets(capped),
      context: 'plan-limit-stream',
    });
  });

});
