import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { isDeepStrictEqual } from 'node:util';
import { expect, request as playwrightRequest, test, type Locator, type Response } from '@playwright/test';
import { accessHeaders, qaAuthHeader, readApiBody } from '../helpers/access.js';
import {
  apiGet, apiPost, applyAccountToPage, enableVirtualAuthenticator, retrieveStagedSignupCode,
  tryMintTestAccount, waitForHydration,
} from '../helpers/accounts.js';
import { assertServingBuildMatchesPromotion } from '../helpers/build-identity-live.js';
import { apiDeployOutcome } from '../helpers/build-identity.js';
import { playbackFailure } from '../helpers/playback.js';
import { resumeFloorAfterSeek, seekProofFailure, seekViaPlayerUi, waitForPaused } from '../helpers/player-ui.js';
import {
  assertCuratedLibrary, assertPrivateInvite, assertPrivateRefusals, PRIVATE_MUSIC,
} from '../helpers/private-music-journey.js';
import {
  parsePrivateStagingFixture, privateStagingAudio, PRIVATE_TRIAL_UPLOAD, SESSION_COOKIE,
} from '../helpers/private-staging-fixture.js';
import { privateRunIdentity, privateTrackObservation } from './track-evidence.js';
import { uploadToLibrary } from './upload-to-library.js';

const ORIGIN = 'https://staging.nntune.com';
const REGISTER = '/v1/auth/passkey/register';
const SESSION_BINDING = '/v1/me/likes/session';
const MEDIA = /^\/v1\/plays\/media\/([a-zA-Z0-9_-]+)\/([a-zA-Z0-9_-]+)\/([a-zA-Z0-9_-]+)$/;
const BROWSER_SELECTOR = 'x-nntune-browser-playback';
const BROWSER_BINDING = 'x-nntune-session-binding';
const CUTOFF_LEAD_MS = 25_000;

// Suppress underlying browser and API diagnostics, which can include a URL,
// request headers or the value typed into a form. The step name is fixed.
async function check<T>(name: string, run: () => Promise<T>): Promise<T> {
  return test.step(name, async () => {
    try {
      return await run();
    } catch {
      throw new Error(`Private staging failed at ${name}`);
    }
  });
}

async function startSampling(audio: Locator) {
  await audio.evaluate((element) => {
    const player = element as HTMLAudioElement;
    player.dataset.minimumTime = String(player.currentTime);
    player.addEventListener('timeupdate', () => {
      player.dataset.minimumTime = String(Math.min(Number(player.dataset.minimumTime), player.currentTime));
    });
  });
}

async function currentMedia(audio: Locator, expectedTrackId: string) {
  const source = new URL(await audio.evaluate((element) => (element as HTMLAudioElement).currentSrc));
  const [, trackId, sessionId, playId] = MEDIA.exec(source.pathname) ?? [];
  if (trackId !== expectedTrackId || !sessionId || !playId || source.origin !== ORIGIN ||
      source.username || source.password || source.search || source.hash) {
    throw new Error('Expected the authenticated media relay for the selected track');
  }
  return { source, trackId, sessionId, playId };
}

async function samplePlaying(audio: Locator, baseline: number) {
  const sample = () => audio.evaluate((element) => {
    const player = element as HTMLAudioElement;
    return {
      minimumTime: Math.min(Number(player.dataset.minimumTime), player.currentTime),
      playback: {
        sawPlaying: false, maxCurrentTime: player.currentTime, currentTime: player.currentTime,
        duration: player.duration, paused: player.paused, ended: player.ended,
        readyState: player.readyState, errorCode: player.error?.code ?? null, hasSource: !!player.currentSrc,
      },
    };
  });
  let observed = await sample();
  await expect.poll(async () => {
    observed = await sample();
    return playbackFailure(observed.playback, baseline + 0.2);
  }).toBeNull();
  return observed;
}

test('private staging invite serves curated audio and refuses access at cutoff', async ({ page, request, browser }) => {
  if (process.env.STAGING_BASE_URL !== ORIGIN || apiDeployOutcome() !== 'deployed' || !process.env.QA_AUTH_TOKEN) {
    throw new Error('Private journey requires the exact staging origin, a same-run API deploy and the QA code seam');
  }
  const inputs = privateStagingAudio();
  const upload = inputs.find(({ file }) => file === 'tone-a.flac');
  const companion = inputs.find(({ file }) => file === 'tone-b.flac');
  if (!upload || !companion) throw new Error('Private staging requires both checked-in FLACs');
  const digests = inputs.map((item) => item.sha256);
  const fixture = parsePrivateStagingFixture(process.env.PRIVATE_MUSIC_STAGE_FIXTURE ?? '', Math.floor(Date.now() / 1000), companion.sha256);
  const access = accessHeaders();
  const getLibrary = async (context = page.request) => {
    const url = `${ORIGIN}${PRIVATE_MUSIC.library.api}`;
    const response = await apiGet(context, url, access, 'private-library');
    const body = await readApiBody(response, url, 'private-library');
    const items = response.ok() ? JSON.parse(body) as { id: string }[] : [];
    return { status: response.status(), items };
  };
  const post = async (context: typeof request, path: string, body: unknown, headers = access) => {
    const url = `${ORIGIN}${path}`;
    const response = await apiPost(context, url, { ...headers, Origin: ORIGIN }, body, 'private-api');
    const text = await readApiBody(response, url, 'private-api');
    return { status: response.status(), text };
  };
  const build = await check('build-identity', () => assertServingBuildMatchesPromotion(request, ORIGIN, 'build-identity'));
  const run = privateRunIdentity(build, process.env.GITHUB_RUN_ID, process.env.GITHUB_RUN_ATTEMPT);
  const missingInviteSignup = await check('missing invite refusal', async () => {
    const result = await post(request, `${REGISTER}/start`, { email: fixture.email }, { ...access, ...qaAuthHeader() });
    expect(result.status).toBe(401);
    return result.status;
  });

  await check('invite redemption and Library arrival', async () => {
    await applyAccountToPage(page, null, ORIGIN);
    await page.goto(`${ORIGIN}${PRIVATE_MUSIC.signup.path}`);
    await waitForHydration(page, 'private-signup');
    await page.evaluate((code) => { window.location.hash = `invite=${code}`; }, fixture.inviteCode);
    await expect.poll(() => new URL(page.url()).hash === '').toBe(true);
    await page.getByTestId(PRIVATE_MUSIC.signup.email).fill(fixture.email);
    await page.getByTestId(PRIVATE_MUSIC.signup.submit).click();
    const input = page.getByLabel(PRIVATE_MUSIC.signup.emailCode, { exact: true });
    await expect(input).toBeVisible();
    const code = await retrieveStagedSignupCode(request, ORIGIN, fixture.email);
    if (!code) throw new Error('Missing staged email code');
    await enableVirtualAuthenticator(page);
    await input.fill(code);
    const [finish] = await Promise.all([
      page.waitForResponse((response) => response.url() === `${ORIGIN}${REGISTER}/finish` && response.request().method() === 'POST'),
      page.getByRole('button', { name: PRIVATE_MUSIC.signup.finish, exact: true }).click(),
    ]);
    const body = await finish.json();
    await expect(page).toHaveURL(`${ORIGIN}${PRIVATE_MUSIC.library.path}`);
    const library = await getLibrary();
    assertPrivateInvite({
      privateListening: finish.ok() && body.private_listening === true,
      destination: new URL(page.url()).pathname, inviteFragmentCleared: !new URL(page.url()).hash,
      libraryStatus: library.status,
    });
  });

  const rows = page.getByTestId(PRIVATE_MUSIC.library.item);
  const rowFor = (id: string) => rows.and(page.locator(`[data-id="${id}"]`));
  const refresh = page.getByRole('button', { name: PRIVATE_MUSIC.library.refresh, exact: true });
  const observe = (type: string, publication: { trackId: string; sha256: string }, observedTrackId: unknown) => {
    const description = JSON.stringify(privateTrackObservation(run, publication, observedTrackId));
    test.info().annotations.push({ type, description });
  };
  const fresh = await check('upload-to-library', async () => {
    // Protected setup can admit the prepublished companion only after redemption.
    let priorIds: string[] = [];
    await expect.poll(async () => {
      const library = await getLibrary();
      priorIds = library.items.map(({ id }) => id);
      return library.status === 200 && priorIds.includes(fixture.companion.trackId);
    }, { timeout: 60_000, intervals: [1_000] }).toBe(true);
    await refresh.click();
    await expect(rowFor(fixture.companion.trackId)).toBeVisible();
    const { publication, annotations } = await uploadToLibrary(
      run, upload.sha256, priorIds,
      async () => {
        const owner = await playwrightRequest.newContext();
        return post(owner, PRIVATE_TRIAL_UPLOAD, upload.body, {
          ...access, 'Content-Type': 'audio/flac', Cookie: `${SESSION_COOKIE}=${fixture.ownerSession}`,
        }).finally(() => owner.dispose());
      },
      async (trackId) => {
        await refresh.click();
        const row = rowFor(trackId);
        await expect(row).toBeVisible();
        return { ...await getLibrary(), renderedTrackId: await row.getAttribute(PRIVATE_MUSIC.library.id) };
      },
    );
    test.info().annotations.push(...annotations);
    return publication;
  });
  const requestFreshPlay = async (context: typeof request, binding?: string) => {
    const target = { track_id: fresh.trackId, session_id: randomUUID() };
    const result = await post(context, PRIVATE_MUSIC.player.plays, target, {
      ...access, [BROWSER_SELECTOR]: '1', ...(binding && { [BROWSER_BINDING]: binding }),
    });
    return { status: result.status, trackId: target.track_id };
  };
  const anonymousLibrary = await check('anonymous-refused', async () => {
    const anonymous = await playwrightRequest.newContext();
    try {
      const library = await getLibrary(anonymous);
      const play = await requestFreshPlay(anonymous);
      expect([library.status, play.status]).toEqual([401, 401]);
      observe('private-anonymous-refusal', fresh, play.trackId);
      return library.status;
    } finally {
      await anonymous.dispose();
    }
  });
  const { uninvitedLibrary, uninvitedPlay } = await check('unlisted-refused', async () => {
    const outsider = await browser.newContext({ serviceWorkers: 'block' });
    try {
      const account = await tryMintTestAccount(request, ORIGIN);
      if (!account || account.email === fixture.email) throw new Error('Expected a distinct synthetic session');
      await applyAccountToPage(await outsider.newPage(), account, ORIGIN);
      const bindingUrl = `${ORIGIN}${SESSION_BINDING}`;
      const session = await apiGet(outsider.request, bindingUrl, access, 'private-unlisted-session');
      const { session_binding: binding } = JSON.parse(await readApiBody(session, bindingUrl, 'private-unlisted-session'));
      if (session.status() !== 200 || typeof binding !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(binding)) {
        throw new Error('Expected a live unlisted session binding');
      }
      const library = await getLibrary(outsider.request);
      const play = await requestFreshPlay(outsider.request, binding);
      // Without the selector, the API uses its separate native play path.
      const nativePlay = await post(outsider.request, PRIVATE_MUSIC.player.plays, {
        track_id: play.trackId, session_id: randomUUID(),
      });
      expect([library.status, play.status, nativePlay.status]).toEqual([403, 403, 403]);
      observe('private-unlisted-refusal', fresh, play.trackId);
      return { uninvitedLibrary: library.status, uninvitedPlay: play.status };
    } finally {
      await outsider.close();
    }
  });
  const publications = [fresh, fixture.companion];
  const idsOf = (items: Locator, attr: string) => items.evaluateAll((elements, name) =>
    elements.flatMap((element) => element.getAttribute(name) ?? []), attr);
  const order = await check('curated selection', async () => {
    const order = assertCuratedLibrary(digests, publications, await idsOf(rows, PRIVATE_MUSIC.library.id));
    expect(order[0] === fresh.trackId).toBe(true);
    for (const id of order) await rowFor(id).locator(PRIVATE_MUSIC.library.select).click();
    return order;
  });
  const rangeHeaders = { ...access, Range: 'bytes=65536-69631', 'Sec-Fetch-Site': 'same-origin' };
  const verifyRange = async (source: URL, original: Buffer) => {
    const range = await apiGet(page.request, source.href, rangeHeaders, 'private-range');
    const headers = range.headers();
    expect([range.status(), headers['content-type'], headers['content-length'], headers['content-range']])
      .toEqual([206, 'audio/flac', '4096', `bytes 65536-69631/${original.length}`]);
    expect((await range.body()).equals(original.subarray(65536, 69632))).toBe(true);
  };
  const current = page.locator(PRIVATE_MUSIC.player.currentQueueItem);
  const audio = page.getByTestId(PRIVATE_MUSIC.player.audio);
  const transport = page.getByRole('region', { name: 'Player', exact: true });
  const play = transport.getByRole('button', { name: 'Play', exact: true });
  const pause = async () => {
    await transport.getByRole('button', { name: 'Pause', exact: true }).click();
    await waitForPaused(audio, true, 5_000, 'Player did not pause');
  };
  const playSelected = async (trackId: string) => {
    await expect(current).toHaveAttribute(PRIVATE_MUSIC.player.queueId, trackId);
    const [start] = await Promise.all([
      page.waitForResponse((response) => response.url() === `${ORIGIN}${PRIVATE_MUSIC.player.plays}` &&
        response.request().method() === 'POST' && response.request().headers()[BROWSER_SELECTOR] === '1'),
      play.click(),
    ]);
    await samplePlaying(audio, 0);
    await pause();
    return { start, media: await currentMedia(audio, trackId) };
  };
  const retryBrowserPlay = async (start: Response, media: Awaited<ReturnType<typeof currentMedia>>) => {
    const request = start.request();
    const binding = request.headers()[BROWSER_BINDING];
    const issued = request.postDataJSON();
    const ack = await start.json();
    if (!binding) throw new Error('Missing browser playback session binding');
    expect(start.status() === 200 && issued?.track_id === media.trackId && issued.session_id === media.sessionId &&
      ack.sessionBinding === binding && ack.trackId === media.trackId &&
      ack.playbackSessionId === media.sessionId && ack.playId === media.playId &&
      Number.isSafeInteger(ack.durationMs) && ack.durationMs > 0 && Object.keys(ack).length === 5).toBe(true);
    const target = { track_id: media.trackId, session_id: media.sessionId, play_id: media.playId };
    const retry = async () => ({
      ...await post(page.request, PRIVATE_MUSIC.player.plays, target,
        { ...access, [BROWSER_SELECTOR]: '1', [BROWSER_BINDING]: binding }),
      trackId: target.track_id,
    });
    const positive = await retry();
    expect(positive.status).toBe(200);
    expect(isDeepStrictEqual(JSON.parse(positive.text), ack)).toBe(true);
    return { retry, trackId: ack.trackId as unknown };
  };
  const first = await check('play', async () => {
    await expect(audio).toHaveCount(0);
    const { start, media } = await playSelected(fresh.trackId);
    const { trackId } = await retryBrowserPlay(start, media);
    await verifyRange(media.source, upload.body);
    observe('private-play', fresh, trackId);
    return media;
  });
  await check('seek', async () => {
    await audio.evaluate((element) => {
      delete (element as HTMLElement).dataset.seeked;
      element.addEventListener('seeked', () => { (element as HTMLElement).dataset.seeked = 'yes'; }, { once: true });
    });
    const { fraction } = await seekViaPlayerUi(page, audio, 0.55);
    await expect(audio).toHaveAttribute('data-seeked', 'yes');
    const paused = await waitForPaused(audio, true, 5_000, 'Seek did not finish paused');
    expect(Number.isFinite(paused.currentTime) && Number.isFinite(paused.duration) && paused.duration > 0 &&
      fraction > 0.25 && fraction < 0.75 && !seekProofFailure(paused, fraction, true)).toBe(true);
    expect((await currentMedia(audio, fresh.trackId)).source.href === first.source.href).toBe(true);
    await startSampling(audio);
    await play.click();
    const resume = await samplePlaying(audio, paused.currentTime);
    expect(resume.minimumTime >= resumeFloorAfterSeek(paused.currentTime, paused.duration) &&
      resume.minimumTime <= resume.playback.maxCurrentTime).toBe(true);
    await pause();
    expect((await currentMedia(audio, fresh.trackId)).source.href === first.source.href).toBe(true);
  });
  const queueItems = page.getByTestId(PRIVATE_MUSIC.player.queueItem);
  await check('queue', async () => {
    expect(await idsOf(queueItems, PRIVATE_MUSIC.player.queueId)).toEqual(order);
    await page.getByTestId(PRIVATE_MUSIC.player.next).click();
    const { start, media } = await playSelected(fixture.companion.trackId);
    expect(media.sessionId === first.sessionId).toBe(true);
    await verifyRange(media.source, companion.body);
    await retryBrowserPlay(start, media);
  });
  await check('video-off', async () => {
    await page.locator('a[href="/now-playing"]').filter({ visible: true }).first().click();
    await expect(page).toHaveURL(`${ORIGIN}/now-playing`);
    await expect(page.getByRole('tab', { name: 'Music Video' })).toHaveCount(0);
    await expect(page.locator('iframe, video')).toHaveCount(0);
  });

  const cutoff = await check('fresh track works before cutoff', async () => {
    await page.locator('a[href="/library"]').filter({ visible: true }).first().click();
    await expect(page).toHaveURL(`${ORIGIN}${PRIVATE_MUSIC.library.path}`);
    const queuedFresh = queueItems.and(page.locator(`[${PRIVATE_MUSIC.player.queueId}="${fresh.trackId}"]`));
    await queuedFresh.getByRole('button', { name: /^Select / }).click();
    // The companion replaced the active lease. Open a new play for the fresh
    // track close to expiry, then keep that exact play for the cutoff probes.
    await delay(Math.max(0, fixture.expiresAt * 1000 - Date.now() - CUTOFF_LEAD_MS));
    const { start, media } = await playSelected(fresh.trackId);
    expect(media.sessionId === first.sessionId && media.playId !== first.playId).toBe(true);
    const { retry } = await retryBrowserPlay(start, media);
    await verifyRange(media.source, upload.body);
    const library = await getLibrary();
    expect(library.status === 200 && library.items.some(({ id }) => id === fresh.trackId)).toBe(true);
    return { retry, mediaUrl: media.source.href };
  });
  await check('revoked-or-expired-refused', async () => {
    const remaining = fixture.expiresAt * 1000 - Date.now();
    if (remaining <= 0) throw new Error('Cohort cutoff preceded completion of the active journey');
    await delay(remaining + 2_000);
    const revokedLibrary = (await getLibrary()).status;
    const revoked = await cutoff.retry();
    const range = await apiGet(page.request, cutoff.mediaUrl, rangeHeaders, 'private-range-cutoff');
    expect(range.status()).toBe(403);
    await readApiBody(range, cutoff.mediaUrl, 'private-range-cutoff');
    await page.goto(`${ORIGIN}${PRIVATE_MUSIC.library.path}`);
    await expect(page.getByText(PRIVATE_MUSIC.library.forbidden, { exact: true })).toBeVisible();
    const remainingCuratedRows = (await idsOf(rows, PRIVATE_MUSIC.library.id))
      .filter((id) => publications.some((item) => item.trackId === id)).length;
    assertPrivateRefusals({
      missingInviteSignup, anonymousLibrary, uninvitedLibrary, uninvitedPlay, revokedLibrary, revokedPlay: revoked.status,
    }, remainingCuratedRows);
    observe('private-cutoff-refusal', fresh, revoked.trackId);
  });
  await check('build-identity-final', () => assertServingBuildMatchesPromotion(request, ORIGIN, 'build-identity-final'));
});
