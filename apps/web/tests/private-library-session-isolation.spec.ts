import { expect, type BrowserContext, type Page, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import {
	expectPlaybackRefusal, fixtureCookie, observeMediaSources, playQueuedTrack,
	privateListeningRefusal, serveFlac, test
} from './helpers/private-music-fixture.js';

const { library, player } = PRIVATE_MUSIC;
const bindingHeader = 'x-nntune-session-binding';
const durationMs = 14_000;
const mediaPrefix = `${player.plays}/media/`;
const logoutPath = '/v1/session/logout';

type Track = {
	item: { id: string; title: string; duration_ms: number };
	playId: string;
	audio: Buffer;
};

type Invitee = { account: string; binding: string; tracks: [Track, ...Track[]] };

function makeTrack(key: number, audio: Buffer): Track {
	return {
		item: { id: `00000000-0000-4000-8000-00000000000${key}`, title: `Private tone ${key}`, duration_ms: durationMs },
		playId: `private_browser_000000000000000000000${key}`,
		audio
	};
}

function makeInvitee(index: number, audio: Buffer): Invitee {
	return {
		account: `invitee-${index + 1}`,
		binding: Buffer.alloc(32, index + 1).toString('base64url'),
		tracks: [makeTrack(2 * index + 1, audio)]
	};
}

function mediaPath(track: Track, sessionId: string) {
	return `${mediaPrefix}${track.item.id}/${sessionId}/${track.playId}`;
}

async function wireInviteeContext(
	context: BrowserContext,
	origin: string,
	sessions: Map<string, Invitee>,
	cookie: string,
	denyGrant: (invitee: Invitee) => boolean = () => false
) {
	await context.addCookies([{ name: fixtureCookie, value: cookie, url: origin, httpOnly: true }]);
	const sessionToken = async (request: Request) =>
		(await request.headerValue('cookie'))?.split('; ')
			.find((part) => part.startsWith(`${fixtureCookie}=`))?.slice(fixtureCookie.length + 1) ?? '';
	const inviteeFor = async (request: Request) => sessions.get(await sessionToken(request));
	const grantedMedia = new Map<string, { invitee: Invitee; audio: Buffer }>();

	await context.route('**/*', (route) =>
		new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.route(`${origin}/v1/me/subscription`, async (route) =>
		(await inviteeFor(route.request()))
			? route.fulfill({ json: { state: 'active' } })
			: route.fulfill({ status: 401 }));
	await context.route(`${origin}/v1/me/likes/session`, async (route) => {
		const invitee = await inviteeFor(route.request());
		return invitee ? route.fulfill({ json: { session_binding: invitee.binding } }) : route.fulfill({ status: 401 });
	});
	await context.route(`${origin}${library.api}`, async (route) => {
		const invitee = await inviteeFor(route.request());
		return invitee
			? route.fulfill({ headers: { [bindingHeader]: invitee.binding }, json: invitee.tracks.map(({ item }) => item) })
			: route.fulfill({ status: 401 });
	});
	await context.route(`${origin}${player.plays}`, async (route) => {
		const request = route.request();
		const invitee = await inviteeFor(request);
		if (!invitee) return route.fulfill({ status: 401 });
		if (request.method() !== 'POST' || await request.headerValue('origin') !== origin ||
			await request.headerValue(bindingHeader) !== invitee.binding ||
			await request.headerValue('x-nntune-browser-playback') !== '1') return route.fulfill({ status: 400 });
		const { track_id: trackId, session_id: playbackSessionId, play_id: priorPlayId } = request.postDataJSON();
		const track = invitee.tracks.find(({ item }) => item.id === trackId);
		if (!track || typeof playbackSessionId !== 'string' || !/^[0-9a-f-]{36}$/.test(playbackSessionId) ||
			priorPlayId !== undefined) return route.fulfill({ status: 400 });
		if (denyGrant(invitee)) return route.fulfill({ status: 403, json: privateListeningRefusal });

		grantedMedia.set(origin + mediaPath(track, playbackSessionId), { invitee, audio: track.audio });
		return route.fulfill({ json: { trackId, playbackSessionId, sessionBinding: invitee.binding, playId: track.playId, durationMs } });
	});
	await context.route(`${origin}${mediaPrefix}**`, async (route) => {
		const request = route.request();
		const grant = grantedMedia.get(request.url());
		if (!grant || grant.invitee !== await inviteeFor(request)) return route.fulfill({ status: 404 });
		return serveFlac(route, grant.audio);
	});
	await context.route(`${origin}${logoutPath}`, async (route) => {
		const request = route.request();
		if (request.method() !== 'POST' || await request.headerValue('origin') !== origin) return route.fulfill({ status: 403 });
		if (!sessions.delete(await sessionToken(request))) return route.fulfill({ status: 401 });
		return route.fulfill({
			headers: { 'set-cookie': `${fixtureCookie}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0` },
			json: { ok: true }
		});
	});
}

async function expectOwnLibrary(page: Page, invitee: Invitee) {
	const rows = page.getByTestId(library.item);
	await expect.poll(() => rows.evaluateAll((items, id) => items.map((row) => row.getAttribute(id)), library.id))
		.toEqual(invitee.tracks.map(({ item }) => item.id));
}

async function refreshOwnLibrary(page: Page, origin: string, invitee: Invitee) {
	const [response] = await Promise.all([
		page.waitForResponse(origin + library.api),
		page.getByRole('button', { name: library.refresh, exact: true }).click()
	]);
	expect(response.status()).toBe(200);
	expect(await response.headerValue(bindingHeader)).toBe(invitee.binding);
	await expectOwnLibrary(page, invitee);
}

function toggle(page: Page, name: 'Play' | 'Pause') {
	return page.getByRole('region', { name: 'Player', exact: true }).getByRole('button', { name, exact: true });
}

function playbackState(page: Page) {
	return page.getByTestId(player.audio).evaluate((audio: HTMLAudioElement) => ({
		paused: audio.paused,
		time: audio.currentTime,
		src: audio.getAttribute('src')
	}));
}

async function expectTrackPlaying(page: Page, origin: string, track: Track, sessionId: string) {
	const audio = page.getByTestId(player.audio);
	const source = mediaPath(track, sessionId);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => ({
		source: node.currentSrc, playing: !node.paused && node.error === null && node.currentTime > 0.2
	}))).toEqual({ source: origin + source, playing: true });
	await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, track.item.id);
	await expect(audio).toHaveAttribute('src', source);
}

function selectLibraryTrack(page: Page, track: Track) {
	return page.getByTestId(library.item)
		.and(page.locator(`[${library.id}="${track.item.id}"]`)).locator(library.select).click();
}

async function startOwnPlayback(page: Page, origin: string, invitee: Invitee) {
	await expectOwnLibrary(page, invitee);
	for (const track of invitee.tracks) await selectLibraryTrack(page, track);
	await expect(page.getByTestId(player.queueItem)).toHaveCount(invitee.tracks.length);
	const [response] = await Promise.all([
		page.waitForResponse(origin + player.plays),
		toggle(page, 'Play').click()
	]);
	const first = invitee.tracks[0];
	const { session_id: sessionId } = response.request().postDataJSON() as { session_id: string };
	expect(response.status()).toBe(200);
	expect(response.request().postDataJSON()).toEqual({ track_id: first.item.id, session_id: sessionId });
	await expectTrackPlaying(page, origin, first, sessionId);
	return sessionId;
}

async function openInviteePages(
	page: Page, other: Page, origin: string, first: Invitee, second: Invitee, denyGrant?: (invitee: Invitee) => boolean
) {
	const sessions = new Map([[first.account, first], [second.account, second]]);
	await Promise.all([
		wireInviteeContext(page.context(), origin, sessions, first.account, denyGrant),
		wireInviteeContext(other.context(), origin, sessions, second.account, denyGrant)
	]);
	await Promise.all([page.goto(origin + library.path), other.goto(origin + library.path)]);
	return Promise.all([startOwnPlayback(page, origin, first), startOwnPlayback(other, origin, second)]);
}

test('private Library and playback stay isolated when another invitee signs out', async ({ browser, context, page, origin }) => {
	const tones = privateStagingAudio();
	const first = makeInvitee(0, tones[0].body);
	const second = makeInvitee(1, tones[1].body);
	const otherContext = await browser.newContext({ serviceWorkers: 'block' });
	try {
		const other = await otherContext.newPage();
		await openInviteePages(page, other, origin, first, second);
		await test.step('Sign out one invitee and resume the other', async () => {
			await expect.poll(async () => (await playbackState(other)).time).toBeGreaterThan(1);
			await toggle(other, 'Pause').click();
			await expect.poll(async () => (await playbackState(other)).paused).toBe(true);
			const before = await playbackState(other);
			expect(before.time).toBeGreaterThan(1);

			// Use ordinary sign-out in a second tab of the first invitee's browser.
			const security = await context.newPage();
			await security.goto(`${origin}/account/security`);
			const logout = security.waitForResponse(`${origin}${logoutPath}`);
			await security.getByRole('button', { name: 'Sign out', exact: true }).click();
			expect((await logout).status()).toBe(200);
			await security.waitForURL(`${origin}/login`);
			expect((await context.cookies(origin)).some(({ name }) => name === fixtureCookie)).toBe(false);
			await expect(page.getByTestId(library.item)).toHaveCount(0);
			await expect(page.getByTestId(player.queueItem)).toHaveCount(0);
			await expect(page.getByTestId(player.audio)).toHaveCount(0);

			await refreshOwnLibrary(other, origin, second);
			await expect(other.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, second.tracks[0].item.id);
			expect(await playbackState(other)).toEqual(before);

			// Sample media events before clicking Play so a restart cannot catch up between polls.
			const resume = await other.evaluateHandle(({ testId, time }) => {
				const evidence = { minimumTime: time };
				const sample = ({ target }: Event) => {
					if (target instanceof HTMLAudioElement && target.dataset.testid === testId) {
						evidence.minimumTime = Math.min(evidence.minimumTime, target.currentTime);
					}
				};
				for (const event of ['play', 'playing', 'seeking', 'timeupdate']) {
					document.addEventListener(event, sample, true);
				}
				return evidence;
			}, { testId: player.audio, time: before.time });
			await toggle(other, 'Play').click();
			let minimumTime = before.time;
			await expect.poll(async () => {
				const { paused, time, src } = await playbackState(other);
				minimumTime = Math.min(minimumTime, time);
				return !paused && time > before.time && src === before.src;
			}).toBe(true);
			const eventMinimum = await resume.evaluate((evidence) => evidence.minimumTime);
			expect(Math.min(minimumTime, eventMinimum), 'playback restarted on resume').toBeGreaterThanOrEqual(before.time - 0.25);
		});
	} finally {
		await otherContext.close();
	}
});

for (const { name, leakDenial } of [
	{ name: 'private playback grant refusal stays scoped to the revoked invitee', leakDenial: false },
	{ name: 'private playback isolation detects a grant refusal leaking to the active invitee', leakDenial: true }
]) {
	test(name, async ({ browser, page, origin }) => {
		const tones = privateStagingAudio();
		const revoked = makeInvitee(0, tones[0].body);
		const active = makeInvitee(1, tones[1].body);
		const revokedNext = makeTrack(2, tones[1].body);
		const activeNext = makeTrack(4, tones[0].body);
		revoked.tracks.push(revokedNext);
		let revokedAccount: string | undefined;
		const otherContext = await browser.newContext({ serviceWorkers: 'block' });
		try {
			const other = await otherContext.newPage();
			const mediaRequests = new Set<string>();
			page.on('request', (request) => {
				if (request.url().startsWith(origin + mediaPrefix)) mediaRequests.add(request.url());
			});
			const [revokedSession, activeSession] = await openInviteePages(page, other, origin, revoked, active,
				(invitee) => revokedAccount !== undefined && (invitee.account === revokedAccount || leakDenial));
			expect(activeSession).not.toBe(revokedSession);
			// Keep natural-end handling from initiating a switch before the explicit queue action.
			await Promise.all([toggle(page, 'Pause').click(), toggle(other, 'Pause').click()]);
			await expect.poll(async () => (await playbackState(page)).paused).toBe(true);
			const activeBefore = await playbackState(other);
			expect(activeBefore.paused).toBe(true);
			const originalSource = mediaPath(revoked.tracks[0], revokedSession);
			const sources = await observeMediaSources(page);

			// Model revocation only at new grant issuance. Existing media and sessions remain valid in the fixture.
			revokedAccount = revoked.account;
			const denial = await playQueuedTrack(page, origin, revokedNext.item.title);
			expect(denial.request().postDataJSON()).toEqual({ track_id: revokedNext.item.id, session_id: revokedSession });
			expect(denial.status()).toBe(403);
			expect(await denial.json()).toEqual(privateListeningRefusal);
			await expectPlaybackRefusal(page);
			await expect(page).toHaveURL(origin + library.path);
			await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, revoked.tracks[0].item.id);
			await expect(page.getByTestId(player.audio)).toHaveAttribute('src', originalSource);

			// The new row must come from the active invitee's refreshed Library.
			active.tracks.push(activeNext);
			await refreshOwnLibrary(other, origin, active);
			expect(await playbackState(other)).toEqual(activeBefore);
			await selectLibraryTrack(other, activeNext);
			const switched = await playQueuedTrack(other, origin, activeNext.item.title);
			expect(switched.request().postDataJSON()).toEqual({ track_id: activeNext.item.id, session_id: activeSession });
			const grantFailure = 'Active invitee should obtain a fresh playback grant';
			const expectActiveGrant = () => expect(switched.status(), grantFailure).toBe(200);
			if (leakDenial) {
				// Inject the same denial into the active grant and require the normal assertion to reject it.
				expect(switched.status()).toBe(403);
				expect(await switched.json()).toEqual(privateListeningRefusal);
				await expectPlaybackRefusal(other);
				expect(expectActiveGrant).toThrow(grantFailure);
			} else {
				expectActiveGrant();
				expect(await switched.json()).toEqual({
					trackId: activeNext.item.id, playbackSessionId: activeSession, sessionBinding: active.binding,
					playId: activeNext.playId, durationMs
				});
				await expectTrackPlaying(other, origin, activeNext, activeSession);
			}

			expect([...mediaRequests]).toEqual([origin + originalSource]);
			expect(await sources.evaluate((seen) => [...seen])).toEqual([originalSource]);
			await sources.dispose();
		} finally {
			await otherContext.close();
		}
	});
}
