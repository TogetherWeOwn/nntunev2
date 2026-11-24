import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { expect, test, type Locator, type Response } from '@playwright/test';
import { accessHeaders, qaAuthHeader, readApiBody } from '../helpers/access.js';
import {
  apiGet, apiPost, applyAccountToPage, enableVirtualAuthenticator, retrieveStagedSignupCode,
  tryMintTestAccount, waitForHydration,
} from '../helpers/accounts.js';
import { assertServingBuildMatchesPromotion } from '../helpers/build-identity-live.js';
import { apiDeployOutcome } from '../helpers/build-identity.js';
import { playbackFailure } from '../helpers/playback.js';
import { seekViaPlayerUi, waitForPaused } from '../helpers/player-ui.js';
import {
  assertCuratedLibrary, assertPrivateInvite, assertPrivateRefusals, assertPrivateTransport, PRIVATE_MUSIC,
} from '../helpers/private-music-journey.js';
import {
  freshTrialPublication, parsePrivateStagingFixture, privateStagingAudio, PRIVATE_TRIAL_UPLOAD, SESSION_COOKIE,
} from '../helpers/private-staging-fixture.js';

const ORIGIN = 'https://staging.nntune.com';
const REGISTER = '/v1/auth/passkey/register';
const MEDIA = /^\/v1\/plays\/media\/([a-zA-Z0-9_-]+)\/([a-zA-Z0-9_-]+)\/([a-zA-Z0-9_-]+)$/;
const BROWSER_SELECTOR = 'x-nntune-browser-playback';
const BROWSER_BINDING = 'x-nntune-session-binding';

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

async function currentMedia(audio: Locator, trackId: string | null) {
  const source = new URL(await audio.evaluate((element) => (element as HTMLAudioElement).currentSrc));
  const media = MEDIA.exec(source.pathname);
  if (!media || media[1] !== trackId || source.origin !== ORIGIN || source.search || source.hash) {
    throw new Error('Expected the authenticated media relay for the selected track');
  }
  return { source, media };
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
    return { status: response.status(), ids: items.map(({ id }) => id) };
  };
  const post = async (context: typeof request, path: string, body: unknown, headers = access) => {
    const url = `${ORIGIN}${path}`;
    const response = await apiPost(context, url, { ...headers, Origin: ORIGIN }, body, 'private-api');
    const text = await readApiBody(response, url, 'private-api');
    return { status: response.status(), text };
  };
  await check('opening deployed build identity', () => assertServingBuildMatchesPromotion(request, ORIGIN, 'private-opening'));

  const { missingInviteSignup, anonymousLibrary, uninvitedLibrary, uninvitedPlay } = await check('anonymous and uninvited refusal', async () => {
    const missingInvite = await post(request, `${REGISTER}/start`, { email: fixture.email }, { ...access, ...qaAuthHeader() });
    const anonymousLibrary = await getLibrary(request);
    const outsider = await browser.newContext({ serviceWorkers: 'block' });
    try {
      const account = await tryMintTestAccount(request, ORIGIN);
      if (!account || account.email === fixture.email) throw new Error('Expected a distinct synthetic session');
      await applyAccountToPage(await outsider.newPage(), account, ORIGIN);
      const uninvited = await post(outsider.request, PRIVATE_MUSIC.player.plays, {
        track_id: fixture.companion.trackId, session_id: randomUUID(),
      });
      return {
        missingInviteSignup: missingInvite.status, anonymousLibrary: anonymousLibrary.status,
        uninvitedLibrary: (await getLibrary(outsider.request)).status, uninvitedPlay: uninvited.status,
      };
    } finally {
      await outsider.close();
    }
  });

  const priorIds = await check('invite redemption and Library arrival', async () => {
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
    expect(library.ids).toContain(fixture.companion.trackId);
    return library.ids;
  });

  const rows = page.getByTestId(PRIVATE_MUSIC.library.item);
  const rowFor = (id: string) => rows.and(page.locator(`[data-id="${id}"]`));
  const fresh = await check('owner upload and fresh publication in invited Library', async () => {
    await expect(rowFor(fixture.companion.trackId)).toBeVisible();
    const receipt = await post(request, PRIVATE_TRIAL_UPLOAD, upload.body, {
      ...access, 'Content-Type': 'audio/flac', Cookie: `${SESSION_COOKIE}=${fixture.ownerSession}`,
    });
    expect(receipt.status).toBe(200);
    const publication = freshTrialPublication(receipt.text, upload.sha256, priorIds);
    await expect(rowFor(publication.trackId)).toHaveCount(0);
    await page.getByRole('button', { name: PRIVATE_MUSIC.library.refresh, exact: true }).click();
    await expect(rowFor(publication.trackId)).toBeVisible();
    return publication;
  });
  const publications = [fresh, fixture.companion];
  const idsOf = (items: Locator, attr: string) => items.evaluateAll((elements, name) =>
    elements.flatMap((element) => element.getAttribute(name) ?? []), attr);
  const rangeHeaders = { ...access, Range: 'bytes=65536-69631', 'Sec-Fetch-Site': 'same-origin' };
  const verifyRange = async (trackId: string, source: URL) => {
    const digest = publications.find((item) => item.trackId === trackId)?.sha256;
    const original = inputs.find((item) => item.sha256 === digest)!.body;
    const range = await apiGet(page.request, source.href, rangeHeaders, 'private-range');
    const headers = range.headers();
    expect([range.status(), headers['content-type'], headers['content-length'], headers['content-range']])
      .toEqual([206, 'audio/flac', '4096', `bytes 65536-69631/${original.length}`]);
    expect((await range.body()).equals(original.subarray(65536, 69632))).toBe(true);
  };
  const startBrowserPlay = (button: Locator) => Promise.all([
    page.waitForResponse((response) => response.url() === `${ORIGIN}${PRIVATE_MUSIC.player.plays}` &&
      response.request().method() === 'POST' && response.request().headers()[BROWSER_SELECTOR] === '1'),
    button.click(),
  ]);
  const retryBrowserPlay = async (start: Response, media: RegExpExecArray) => {
    const binding = start.request().headers()[BROWSER_BINDING];
    const ack = await start.json();
    expect(start.status() === 200 && !!binding && ack.sessionBinding === binding &&
      ack.trackId === media[1] && ack.playbackSessionId === media[2] && ack.playId === media[3] &&
      Number.isSafeInteger(ack.durationMs) && ack.durationMs > 0 && Object.keys(ack).length === 5).toBe(true);
    const retry = () => post(page.request, PRIVATE_MUSIC.player.plays, {
      track_id: media[1], session_id: media[2], play_id: media[3],
    }, { ...access, [BROWSER_SELECTOR]: '1', [BROWSER_BINDING]: binding! });
    const positive = await retry();
    expect(positive.status).toBe(200);
    expect(JSON.parse(positive.text)).toEqual(ack);
    return retry;
  };
  const { retryAtCutoff, mediaUrl } = await check('curated playback, byte range, seek, queue and video posture', async () => {
    await expect(rowFor(fixture.companion.trackId)).toBeVisible();
    const order = assertCuratedLibrary(digests, publications,
      await idsOf(rows, PRIVATE_MUSIC.library.id));
    expect(order[0]).toBe(fresh.trackId);
    for (const id of order) await rowFor(id).locator(PRIVATE_MUSIC.library.select).click();
    const current = page.locator(PRIVATE_MUSIC.player.currentQueueItem);
    const firstTrackId = await current.getAttribute(PRIVATE_MUSIC.player.queueId);
    const audio = page.getByTestId(PRIVATE_MUSIC.player.audio);
    const transport = page.getByRole('region', { name: 'Player', exact: true });
    await expect(audio).toHaveCount(0);
    const baselineTime = 0;
    const [start] = await startBrowserPlay(transport.getByRole('button', { name: 'Play', exact: true }));
    const playback = (await samplePlaying(audio, baselineTime)).playback;
    await transport.getByRole('button', { name: 'Pause', exact: true }).click();
    await waitForPaused(audio, true, 5_000, 'Player did not pause');
    const first = await currentMedia(audio, firstTrackId);
    await retryBrowserPlay(start, first.media);
    await verifyRange(order[0]!, first.source);

    await audio.evaluate((element) => {
      delete (element as HTMLElement).dataset.seeked;
      element.addEventListener('seeked', () => { (element as HTMLElement).dataset.seeked = 'yes'; }, { once: true });
    });
    const { fraction } = await seekViaPlayerUi(page, audio, 0.55);
    await expect(audio).toHaveAttribute('data-seeked', 'yes');
    const paused = await waitForPaused(audio, true, 5_000, 'Seek did not finish paused');
    await startSampling(audio);
    await transport.getByRole('button', { name: 'Play', exact: true }).click();
    const resume = await samplePlaying(audio, paused.currentTime);
    await transport.getByRole('button', { name: 'Pause', exact: true }).click();
    await waitForPaused(audio, true, 5_000, 'Player did not pause before Next');

    const queueOrder = await idsOf(page.getByTestId(PRIVATE_MUSIC.player.queueItem), PRIVATE_MUSIC.player.queueId);
    await page.getByTestId(PRIVATE_MUSIC.player.next).click();
    await expect(current).toHaveAttribute(PRIVATE_MUSIC.player.queueId, order[1]!);
    assertPrivateTransport(order, {
      firstTrackId, baselineTime, playback, seek: { ...paused, fraction, sawSeekedEvent: true },
      resume, queueOrder, nextTrackId: await current.getAttribute(PRIVATE_MUSIC.player.queueId),
    });
    await delay(Math.max(0, fixture.expiresAt * 1000 - Date.now() - 25_000));
    const [secondStart] = await startBrowserPlay(transport.getByRole('button', { name: 'Play', exact: true }));
    await samplePlaying(audio, 0);
    const { source, media } = await currentMedia(audio, order[1]!);
    expect(media[2]).toBe(first.media[2]);
    await transport.getByRole('button', { name: 'Pause', exact: true }).click();
    await waitForPaused(audio, true, 5_000, 'Second track did not pause');
    await page.locator('a[href="/now-playing"]').filter({ visible: true }).first().click();
    await expect(page).toHaveURL(`${ORIGIN}/now-playing`);
    await expect(page.getByRole('tab', { name: 'Music Video' })).toHaveCount(0);
    await expect(page.locator('iframe, video')).toHaveCount(0);
    await verifyRange(order[1]!, source);
    const retryAtCutoff = await retryBrowserPlay(secondStart, media);
    return { retryAtCutoff, mediaUrl: source.href };
  });

  await check('same-session refusal at the configured cohort cutoff', async () => {
    const remaining = fixture.expiresAt * 1000 - Date.now();
    if (remaining <= 0) throw new Error('Cohort cutoff preceded completion of the active journey');
    await delay(remaining + 2_000);
    const revokedLibrary = (await getLibrary()).status;
    const revokedPlay = (await retryAtCutoff()).status;
    const range = await apiGet(page.request, mediaUrl, rangeHeaders, 'private-range-cutoff');
    expect([401, 403, 404]).toContain(range.status());
    await readApiBody(range, mediaUrl, 'private-range-cutoff');
    await page.goto(`${ORIGIN}${PRIVATE_MUSIC.library.path}`);
    await expect(page.getByText(PRIVATE_MUSIC.library.forbidden, { exact: true })).toBeVisible();
    const remainingCuratedRows = (await idsOf(rows, PRIVATE_MUSIC.library.id))
      .filter((id) => publications.some((item) => item.trackId === id)).length;
    assertPrivateRefusals({
      missingInviteSignup, anonymousLibrary, uninvitedLibrary, uninvitedPlay, revokedLibrary, revokedPlay,
    }, remainingCuratedRows);
  });
  await check('closing deployed build identity', () => assertServingBuildMatchesPromotion(request, ORIGIN, 'private-closing'));
});
