import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { expect, test, type Locator } from '@playwright/test';
import { accessHeaders, qaAuthHeader, readApiBody } from '../helpers/access.js';
import {
  apiGet, apiPost, applyAccountToPage, enableVirtualAuthenticator, retrieveStagedSignupCode,
  tryMintTestAccount, waitForHydration,
} from '../helpers/accounts.js';
import { assertServingBuildMatchesPromotion } from '../helpers/build-identity-live.js';
import { apiDeployOutcome } from '../helpers/build-identity.js';
import { playbackFailure } from '../helpers/playback.js';
import { resumeFloorAfterSeek, seekViaPlayerUi, waitForPaused } from '../helpers/player-ui.js';
import {
  assertCuratedLibrary, assertPrivateInvite, assertPrivateRefusals, assertPrivateTransport, PRIVATE_MUSIC,
} from '../helpers/private-music-journey.js';
import { parsePrivateStagingFixture, privateStagingAudio } from '../helpers/private-staging-fixture.js';

const ORIGIN = 'https://staging.nntune.com';
const REGISTER = '/v1/auth/passkey/register';
const MEDIA = /^\/v1\/plays\/media\/([a-zA-Z0-9_-]+)\/([a-zA-Z0-9_-]+)\/([a-zA-Z0-9_-]+)$/;

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
  const fixture = parsePrivateStagingFixture(process.env.PRIVATE_MUSIC_STAGE_FIXTURE ?? '', Math.floor(Date.now() / 1000));
  const inputs = privateStagingAudio();
  const access = accessHeaders();
  const getLibrary = async (context = page.request) => {
    const url = `${ORIGIN}${PRIVATE_MUSIC.library.api}`;
    const response = await apiGet(context, url, access, 'private-library');
    await readApiBody(response, url, 'private-library');
    return response.status();
  };
  const post = async (context: typeof request, path: string, body: unknown, headers = access) => {
    const url = `${ORIGIN}${path}`;
    const response = await apiPost(context, url, { ...headers, Origin: ORIGIN }, body, 'private-api');
    const text = await readApiBody(response, url, 'private-api');
    return { status: response.status(), text };
  };
  await check('opening deployed build identity', () => assertServingBuildMatchesPromotion(request, ORIGIN, 'private-opening'));

  const [firstPublication] = fixture.publications;
  const { missingInviteSignup, anonymousLibrary, uninvitedLibrary, uninvitedPlay } = await check('anonymous and uninvited refusal', async () => {
    const missingInvite = await post(request, `${REGISTER}/start`, { email: fixture.email }, { ...access, ...qaAuthHeader() });
    const anonymousLibrary = await getLibrary(request);
    const outsider = await browser.newContext({ serviceWorkers: 'block' });
    try {
      const account = await tryMintTestAccount(request, ORIGIN);
      if (!account || account.email === fixture.email) throw new Error('Expected a distinct synthetic session');
      await applyAccountToPage(await outsider.newPage(), account, ORIGIN);
      const uninvited = await post(outsider.request, PRIVATE_MUSIC.player.plays, {
        track_id: firstPublication!.trackId, session_id: randomUUID(),
      });
      return { missingInviteSignup: missingInvite.status, anonymousLibrary, uninvitedLibrary: await getLibrary(outsider.request), uninvitedPlay: uninvited.status };
    } finally {
      await outsider.close();
    }
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
    assertPrivateInvite({
      privateListening: finish.ok() && body.private_listening === true,
      destination: new URL(page.url()).pathname, inviteFragmentCleared: !new URL(page.url()).hash,
      libraryStatus: await getLibrary(),
    });
  });

  const rows = page.getByTestId(PRIVATE_MUSIC.library.item);
  const idsOf = (items: Locator, attr: string) => items.evaluateAll((elements, name) =>
    elements.flatMap((element) => element.getAttribute(name) ?? []), attr);
  const rangeHeaders = { ...access, Range: 'bytes=65536-69631', 'Sec-Fetch-Site': 'same-origin' };
  const { play, heartbeat, mediaUrl } = await check('curated playback, byte range, seek, queue and video posture', async () => {
    for (const item of fixture.publications) {
      await expect(rows.and(page.locator(`[data-id="${item.trackId}"]`))).toBeVisible();
    }
    const order = assertCuratedLibrary(inputs.map((item) => item.sha256), fixture.publications,
      await idsOf(rows, PRIVATE_MUSIC.library.id));
    for (const id of order) await rows.and(page.locator(`[data-id="${id}"]`)).click();
    const current = page.locator(PRIVATE_MUSIC.player.currentQueueItem);
    const firstTrackId = await current.getAttribute(PRIVATE_MUSIC.player.queueId);
    const audio = page.getByTestId(PRIVATE_MUSIC.player.audio);
    const transport = page.getByRole('region', { name: 'Player', exact: true });
    const baselineTime = await audio.evaluate((element) => (element as HTMLAudioElement).currentTime);
    await transport.getByRole('button', { name: 'Play', exact: true }).click();
    const playback = (await samplePlaying(audio, baselineTime)).playback;
    await transport.getByRole('button', { name: 'Pause', exact: true }).click();
    await waitForPaused(audio, true, 5_000, 'Player did not pause');
    const source = new URL(await audio.evaluate((element) => (element as HTMLAudioElement).currentSrc));
    const media = MEDIA.exec(source.pathname);
    if (!media || media[1] !== firstTrackId || source.origin !== ORIGIN || source.search || source.hash) {
      throw new Error('Expected the authenticated media relay for the selected track');
    }
    const digest = fixture.publications.find((item) => item.trackId === firstTrackId)?.sha256;
    const original = inputs.find((item) => item.sha256 === digest)!.body;
    const range = await apiGet(page.request, source.href, rangeHeaders, 'private-range');
    expect(range.status()).toBe(206);
    const headers = range.headers();
    expect(headers['content-type']).toBe('audio/flac');
    expect(headers['content-range']).toBe(`bytes 65536-69631/${original.length}`);
    expect(headers['content-length']).toBe('4096');
    expect((await range.body()).equals(original.subarray(65536, 69632))).toBe(true);

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
    expect(resume.minimumTime).toBeGreaterThanOrEqual(resumeFloorAfterSeek(paused.currentTime, paused.duration));

    const play = { track_id: media[1], session_id: media[2], play_id: media[3] };
    const renewed = await post(page.request, PRIVATE_MUSIC.player.plays, play);
    const lease = JSON.parse(renewed.text);
    expect(renewed.status === 200 && lease.play_id === play.play_id && Number.isSafeInteger(lease.lease_epoch)).toBe(true);
    const heartbeat = { session_id: play.session_id, play_id: play.play_id, lease_epoch: lease.lease_epoch };
    const active = await post(page.request, PRIVATE_MUSIC.player.heartbeat, heartbeat);
    expect(active.status === 200 && JSON.parse(active.text).lease === 'active').toBe(true);

    const queueOrder = await idsOf(page.getByTestId(PRIVATE_MUSIC.player.queueItem), PRIVATE_MUSIC.player.queueId);
    await page.getByTestId(PRIVATE_MUSIC.player.next).click();
    await expect(current).toHaveAttribute(PRIVATE_MUSIC.player.queueId, order[1]!);
    assertPrivateTransport(order, {
      firstTrackId, baselineTime, playback, seek: { ...paused, fraction, sawSeekedEvent: true },
      resume, queueOrder, nextTrackId: await current.getAttribute(PRIVATE_MUSIC.player.queueId),
    });
    await page.locator('a[href="/now-playing"]').filter({ visible: true }).first().click();
    await expect(page).toHaveURL(`${ORIGIN}/now-playing`);
    await expect(page.getByRole('tab', { name: 'Music Video' })).toHaveCount(0);
    await expect(page.locator('iframe, video')).toHaveCount(0);
    return { play, heartbeat, mediaUrl: source.href };
  });

  await check('same-session refusal at the configured cohort cutoff', async () => {
    const remaining = fixture.expiresAt * 1000 - Date.now();
    if (remaining <= 0) throw new Error('Cohort cutoff preceded completion of the active journey');
    await delay(remaining + 2_000);
    const revokedLibrary = await getLibrary();
    const revokedPlay = (await post(page.request, PRIVATE_MUSIC.player.plays, play)).status;
    const revokedHeartbeat = (await post(page.request, PRIVATE_MUSIC.player.heartbeat, heartbeat)).status;
    const range = await apiGet(page.request, mediaUrl, rangeHeaders, 'private-range-cutoff');
    expect([401, 403, 404]).toContain(range.status());
    await readApiBody(range, mediaUrl, 'private-range-cutoff');
    await page.goto(`${ORIGIN}${PRIVATE_MUSIC.library.path}`);
    await expect(page.getByText(PRIVATE_MUSIC.library.forbidden, { exact: true })).toBeVisible();
    const remainingCuratedRows = (await idsOf(rows, PRIVATE_MUSIC.library.id))
      .filter((id) => fixture.publications.some((item) => item.trackId === id)).length;
    assertPrivateRefusals({
      missingInviteSignup, anonymousLibrary, uninvitedLibrary, uninvitedPlay, revokedLibrary, revokedPlay, revokedHeartbeat,
    }, remainingCuratedRows);
  });
  await check('closing deployed build identity', () => assertServingBuildMatchesPromotion(request, ORIGIN, 'private-closing'));
});
