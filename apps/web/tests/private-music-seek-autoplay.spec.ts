import { expect, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { seekViaPlayerUi } from '../../../e2e/helpers/player-ui.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { fixtureCookie, hasFixtureSession, serveFlac, test } from './helpers/private-music-fixture.js';

test('invited playback advances once after an absolute seek near the end', async ({ page, context, origin }) => {
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const tracks = privateStagingAudio().map(({ body }, index) => ({
		id: `00000000-0000-4000-8000-00000000000${index + 1}`,
		title: `Fixture Tone ${index + 1}`,
		duration_ms: 14_000,
		playId: `private_browser_${String(index + 1).repeat(22)}`,
		body
	}));
	const [first, second] = tracks;
	const { library, player: contract } = PRIVATE_MUSIC;
	const grantUrl = origin + contract.plays;
	const mediaPath = (track: (typeof tracks)[number], tab: string) =>
		`${contract.plays}/media/${track.id}/${tab}/${track.playId}`;
	const isListener = async (request: Request) => hasFixtureSession(await request.headerValue('cookie'), listener);
	const grants: Request[] = [];
	const grantedMedia = new Map<string, Buffer>();

	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => {
		if (!await isListener(route.request())) return route.fulfill({ status: 401 });
		return route.fulfill({ json: { session_binding: binding } });
	});
	await page.route(origin + library.api, async (route) => {
		if (!await isListener(route.request())) return route.fulfill({ status: 401 });
		return route.fulfill({
			headers: { 'x-nntune-session-binding': binding },
			json: tracks.map(({ id, title, duration_ms }) => ({ id, title, duration_ms }))
		});
	});
	page.on('request', (request) => { if (request.url() === grantUrl) grants.push(request); });
	await page.route(grantUrl, async (route) => {
		const call = route.request();
		if (call.method() !== 'POST' || !await isListener(call) ||
			await call.headerValue('x-nntune-browser-playback') !== '1' ||
			await call.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 400 });
		const { track_id, session_id, play_id } = call.postDataJSON() ?? {};
		const track = tracks.find(({ id }) => id === track_id);
		if (!track || typeof session_id !== 'string' || !/^[0-9a-f-]{36}$/.test(session_id) ||
			play_id !== undefined) return route.fulfill({ status: 400 });
		grantedMedia.set(origin + mediaPath(track, session_id), track.body);
		return route.fulfill({ json: {
			trackId: track.id, playbackSessionId: session_id, sessionBinding: binding,
			playId: track.playId, durationMs: track.duration_ms
		} });
	});
	await page.route(`${grantUrl}/media/**`, async (route) => {
		const call = route.request();
		const body = grantedMedia.get(call.url());
		if (!body || !await isListener(call)) return route.fulfill({ status: 404 });
		return serveFlac(route, body);
	});

	const rows = page.getByTestId(library.item);
	const player = page.getByRole('region', { name: 'Player', exact: true });
	const audio = player.getByTestId(contract.audio);
	const slider = player.getByRole('slider', { name: 'Seek', exact: true });
	const active = page.locator(contract.currentQueueItem);
	const queueIds = () => page.getByTestId(contract.queueItem).evaluateAll(
		(rows, attribute) => rows.map((row) => row.getAttribute(attribute)), contract.queueId
	);

	await page.goto(origin + library.path);
	for (const { id } of tracks) {
		await rows.and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
	}
	await expect.poll(queueIds).toEqual([first.id, second.id]);
	await expect(active).toHaveAttribute(contract.queueId, first.id);
	await expect(page.getByTestId('player-repeat')).toHaveValue('off');
	await expect(audio).toHaveCount(0);
	expect(grants).toHaveLength(0);

	// Record play attempts and forward to the browser's real decoder.
	const playCalls = await page.evaluateHandle(() => {
		const sources: (string | null)[] = [];
		const nativePlay = HTMLMediaElement.prototype.play;
		HTMLMediaElement.prototype.play = function (this: HTMLMediaElement) {
			if (this instanceof HTMLAudioElement) sources.push(this.getAttribute('src'));
			return nativePlay.call(this);
		};
		return sources;
	});
	const firstGrant = page.waitForRequest(grantUrl);
	await player.getByRole('button', { name: 'Play', exact: true }).click();
	const tab: string = (await firstGrant).postDataJSON().session_id;
	const firstPath = mediaPath(first, tab);
	await expect(audio).toHaveAttribute('src', firstPath);
	await expect(slider).toBeEnabled();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);

	const firstMedia = await audio.evaluateHandle((node: HTMLAudioElement) => {
		const beforeSeek = node.currentTime;
		const events: { type: string; trusted: boolean; at: number; position: number; duration: number; paused: boolean }[] = [];
		for (const type of ['seeked', 'ended']) {
			node.addEventListener(type, (event) => {
				events.push({ type, trusted: event.isTrusted, at: event.timeStamp,
					position: node.currentTime, duration: node.duration, paused: node.paused });
			}, { once: true });
		}
		return { beforeSeek, events };
	});
	// Leave enough media after the seek for the browser to play to the end.
	expect((await seekViaPlayerUi(page, audio, 0.82)).surface).toBe('custom');
	await expect.poll(async () => (await firstMedia.jsonValue()).events.map(({ type }) => type))
		.toEqual(['seeked', 'ended']);
	const { beforeSeek, events: [seeked, ended] } = await firstMedia.jsonValue();
	expect(seeked).toMatchObject({ trusted: true, paused: false });
	expect(seeked.position, 'native seek must land well ahead of the initial playback position').toBeGreaterThan(beforeSeek + 5);
	expect(seeked.position / seeked.duration).toBeGreaterThan(0.75);
	expect(seeked.position / seeked.duration).toBeLessThan(0.95);
	expect(ended.trusted).toBe(true);
	expect(ended.position).toBeCloseTo(ended.duration, 2);
	expect(ended.at).toBeGreaterThan(seeked.at);

	const secondPath = mediaPath(second, tab);
	await expect(active).toHaveAttribute(contract.queueId, second.id);
	await expect(player.getByTestId('library-now-playing')).toHaveText(second.title);
	await expect(audio).toHaveAttribute('src', secondPath);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);
	await expect.poll(queueIds).toEqual([first.id, second.id]);

	expect(grants.map((request) => request.postDataJSON())).toEqual([
		{ track_id: first.id, session_id: tab }, { track_id: second.id, session_id: tab }
	]);
	expect(await playCalls.jsonValue()).toEqual([firstPath, secondPath]);
	// Resource timing and the native event use the same browser clock.
	const grantStarts = await page.evaluate((url) =>
		performance.getEntriesByName(url, 'resource').map(({ startTime }) => startTime), grantUrl);
	expect(grantStarts).toHaveLength(2);
	expect(grantStarts[1], 'the successor grant must start after the first track ends').toBeGreaterThanOrEqual(ended.at);
});
