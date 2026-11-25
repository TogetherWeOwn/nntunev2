import { fileURLToPath } from 'node:url';
import { expect, test as base, type Request, type Route } from '@playwright/test';
import { preview } from 'vite';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';

const fixtureCookie = 'private_music_fixture_session';
const listener = `${fixtureCookie}=synthetic-listener`;
const binding = 'A'.repeat(43);
const { library, player: contract } = PRIVATE_MUSIC;

const isListener = (cookie: string | null) => cookie?.split('; ').includes(listener);

function serveFlac(route: Route, body: Buffer) {
	const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers()['range'] ?? '');
	const start = range ? Number(range[1]) : 0;
	const end = range?.[2] ? Math.min(Number(range[2]), body.length - 1) : body.length - 1;
	if (start > end) return route.fulfill({ status: 416, headers: { 'content-range': `bytes */${body.length}` } });
	return route.fulfill({
		status: range ? 206 : 200, contentType: 'audio/flac', body: body.subarray(start, end + 1),
		headers: { 'accept-ranges': 'bytes', ...(range && { 'content-range': `bytes ${start}-${end}/${body.length}` }) }
	});
}

const test = base.extend<{ origin: string }>({
	origin: async ({}, use) => {
		// Arm private browser playback only for the loopback browser-fixtures build.
		const previous = process.env['PUBLIC_BROWSER_PLAYBACK'];
		process.env['PUBLIC_BROWSER_PLAYBACK'] = '1';
		const server = await preview({
			root: fileURLToPath(new URL('../', import.meta.url)), mode: 'browser-fixtures',
			preview: { host: '127.0.0.1', port: 0 }
		}).finally(() => {
			if (previous === undefined) delete process.env['PUBLIC_BROWSER_PLAYBACK'];
			else process.env['PUBLIC_BROWSER_PLAYBACK'] = previous;
		});
		try {
			const address = server.httpServer.address();
			if (!address || typeof address === 'string') throw new Error('Missing private fixture TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally {
			await server.close();
		}
	}
});

test.use({ serviceWorkers: 'block' });

test('removing the upcoming private track preserves the playing receipt and skips it on natural end', async ({
	page, context, origin
}) => {
	const tones = privateStagingAudio();
	const toneA = tones.find(({ file }) => file === 'tone-a.flac')!.body;
	const toneB = tones.find(({ file }) => file === 'tone-b.flac')!.body;
	const first = {
		id: '00000000-0000-4000-8000-000000000001', title: 'Playing Tone', duration_ms: 14_000,
		playId: `private_browser_${'a'.repeat(22)}`, body: toneA
	};
	const removed = {
		id: '00000000-0000-4000-8000-000000000002', title: 'Upcoming Tone', duration_ms: 14_000,
		playId: `private_browser_${'b'.repeat(22)}`, body: toneA
	};
	const survivor = {
		id: '00000000-0000-4000-8000-000000000003', title: 'Surviving Tone', duration_ms: 14_000,
		playId: `private_browser_${'c'.repeat(22)}`, body: toneB
	};
	const tracks = [first, removed, survivor];
	const grantUrl = origin + contract.plays;
	const mediaPath = (track: Pick<typeof first, 'id' | 'playId'>, tab: string) =>
		`${contract.plays}/media/${track.id}/${tab}/${track.playId}`;
	const grants: Request[] = [];
	const media = new Map<string, Buffer>();

	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.addCookies([{ name: fixtureCookie, value: 'synthetic-listener', url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => {
		if (!isListener(await route.request().headerValue('cookie'))) return route.fulfill({ status: 401 });
		return route.fulfill({ json: { session_binding: binding } });
	});
	await page.route(origin + library.api, async (route) => {
		if (!isListener(await route.request().headerValue('cookie'))) return route.fulfill({ status: 401 });
		return route.fulfill({
			headers: { 'x-nntune-session-binding': binding },
			json: tracks.map(({ id, title, duration_ms }) => ({ id, title, duration_ms }))
		});
	});
	page.on('request', (request) => {
		if (request.url() === grantUrl) grants.push(request);
	});
	await page.route(grantUrl, async (route) => {
		const call = route.request();
		if (call.method() !== 'POST' || !isListener(await call.headerValue('cookie')) ||
			await call.headerValue('x-nntune-browser-playback') !== '1' ||
			await call.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 400 });
		const { track_id, session_id, play_id } = call.postDataJSON() ?? {};
		const track = tracks.find(({ id }) => id === track_id);
		if (!track || typeof session_id !== 'string' || !/^[0-9a-f-]{36}$/.test(session_id) ||
			play_id !== undefined) return route.fulfill({ status: 400 });
		media.set(origin + mediaPath(track, session_id), track.body);
		return route.fulfill({ json: {
			trackId: track.id, playbackSessionId: session_id, sessionBinding: binding,
			playId: track.playId, durationMs: track.duration_ms
		} });
	});
	await page.route(`${grantUrl}/media/**`, async (route) => {
		const call = route.request();
		const body = media.get(call.url());
		if (!body || !isListener(await call.headerValue('cookie'))) return route.fulfill({ status: 404 });
		return serveFlac(route, body);
	});

	const rows = page.getByTestId(library.item);
	const queue = page.getByTestId(contract.queueItem);
	const active = page.locator(contract.currentQueueItem);
	const audio = page.getByTestId(contract.audio);
	const player = page.getByRole('region', { name: 'Player', exact: true });
	const assertQueue = async (ids: string[]) => {
		await expect.poll(() => queue.evaluateAll((items, attr) => items.map((item) => item.getAttribute(attr)), contract.queueId))
			.toEqual(ids);
	};

	await page.goto(origin + library.path);
	for (const { id } of tracks) {
		await rows.and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
	}
	await assertQueue([first.id, removed.id, survivor.id]);
	await expect(active).toHaveAttribute(contract.queueId, first.id);
	await expect(audio).toHaveCount(0);
	expect(grants).toHaveLength(0);

	const firstGrant = page.waitForRequest(grantUrl);
	await player.getByRole('button', { name: 'Play', exact: true }).click();
	const tab: string = (await firstGrant).postDataJSON().session_id;
	const firstSource = mediaPath(first, tab);
	await expect(audio).toHaveAttribute('src', firstSource);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);

	const currentAudio = await audio.elementHandle();
	const beforeRemoval = await currentAudio.evaluate((node: HTMLAudioElement) => {
		const reloads: string[] = [];
		for (const type of ['emptied', 'loadstart']) node.addEventListener(type, () => reloads.push(type));
		node.addEventListener('ended', (event) => {
			node.dataset.fixtureCompletion = JSON.stringify({
				trusted: event.isTrusted, source: node.currentSrc, reloads, at: event.timeStamp
			});
		}, { once: true });
		return node.currentTime;
	});
	await queue.and(page.locator(`[${contract.queueId}="${removed.id}"]`))
		.getByRole('button', { name: `Remove ${removed.title} from queue`, exact: true }).click();
	await assertQueue([first.id, survivor.id]);
	await expect(active).toHaveAttribute(contract.queueId, first.id);
	await expect(audio).toHaveAttribute('src', firstSource);
	await expect.poll(() => currentAudio.evaluate((node: HTMLAudioElement, before) => ({
		connected: node.isConnected, source: node.currentSrc,
		advancing: !node.paused && !node.ended && node.currentTime > before + 0.1
	}), beforeRemoval)).toEqual({ connected: true, source: origin + firstSource, advancing: true });
	expect(grants.map((request) => request.postDataJSON())).toEqual([{ track_id: first.id, session_id: tab }]);

	// Let Chromium play the first FLAC all the way to its own ended event.
	await expect(active).toHaveAttribute(contract.queueId, survivor.id, { timeout: 18_000 });
	await expect(player.getByTestId('library-now-playing')).toHaveText(survivor.title);
	await expect(audio).toHaveAttribute('src', mediaPath(survivor, tab));
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);
	const completion = await currentAudio.evaluate((node: HTMLAudioElement) =>
		JSON.parse(node.dataset.fixtureCompletion ?? 'null'));
	expect(completion).toMatchObject({ trusted: true, source: origin + firstSource, reloads: [] });
	expect(grants.map((request) => request.postDataJSON())).toEqual([
		{ track_id: first.id, session_id: tab },
		{ track_id: survivor.id, session_id: tab }
	]);
	// Grant fetch starts and the native event use the same browser clock.
	const grantStarts = await page.evaluate((url) =>
		performance.getEntriesByName(url, 'resource').map((entry) => entry.startTime), grantUrl);
	expect(grantStarts).toHaveLength(2);
	expect(grantStarts[1]).toBeGreaterThanOrEqual(completion.at);
});
