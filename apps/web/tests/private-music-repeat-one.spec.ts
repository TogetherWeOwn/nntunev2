import { fileURLToPath } from 'node:url';
import { expect, test, type Request, type Route } from '@playwright/test';
import { preview } from 'vite';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';

const fixtureCookie = 'private_music_fixture_session';
const binding = 'A'.repeat(43);
const isListener = async (request: Request) =>
	(await request.headerValue('cookie'))?.split('; ').includes(`${fixtureCookie}=synthetic-listener`);

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

test.use({ serviceWorkers: 'block' });

test('invited Library playback opens a fresh receipt to repeat the current track on natural end', async ({ page, context }) => {
	test.setTimeout(45_000);
	// Arm private playback only in this loopback browser-fixtures preview.
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
		const origin = `http://127.0.0.1:${address.port}`;
		const { library, player: contract } = PRIVATE_MUSIC;
		const grantUrl = origin + contract.plays;
		const tracks = privateStagingAudio().map(({ file, body }, index) => ({
			id: `00000000-0000-4000-8000-00000000000${index + 1}`, title: file, duration_ms: 14_000, body
		}));
		const first = tracks.find(({ title }) => title === 'tone-a.flac')!;
		const successor = tracks.find(({ title }) => title === 'tone-b.flac')!;
		const playIdFor = (start: number) => `private_browser_${String(start).padStart(22, '0')}`;
		const pathFor = (trackId: string, tab: string, start: number) =>
			`${contract.plays}/media/${trackId}/${tab}/${playIdFor(start)}`;
		const grants: Request[] = [];
		const mediaRequests = new Set<string>();
		let issued = 0;
		let activeMedia: { path: string; body: Buffer } | undefined;

		await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
		await context.addCookies([{ name: fixtureCookie, value: 'synthetic-listener', url: origin, httpOnly: true }]);
		await page.route(`${origin}/v1/me/likes/session`, async (route) => (await isListener(route.request()))
			? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 }));
		await page.route(origin + library.api, async (route) => {
			if (!await isListener(route.request())) return route.fulfill({ status: 401 });
			return route.fulfill({ headers: { 'x-nntune-session-binding': binding },
				json: tracks.map(({ id, title, duration_ms }) => ({ id, title, duration_ms })) });
		});
		page.on('request', (request) => {
			const url = request.url();
			if (url === grantUrl) grants.push(request);
			if (url.startsWith(`${grantUrl}/media/`)) mediaRequests.add(url.slice(origin.length));
		});
		await page.route(grantUrl, async (route) => {
			const call = route.request();
			if (call.method() !== 'POST' || !await isListener(call) ||
				await call.headerValue('x-nntune-browser-playback') !== '1' ||
				await call.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 400 });
			const { track_id, session_id, play_id } = call.postDataJSON() ?? {};
			const track = tracks.find(({ id }) => id === track_id);
			if (!track || typeof session_id !== 'string' || !/^[0-9a-f-]{36}$/.test(session_id) ||
				play_id !== undefined) return route.fulfill({ status: 400 });
			activeMedia = { path: pathFor(track.id, session_id, ++issued), body: track.body };
			return route.fulfill({ json: { trackId: track.id, playbackSessionId: session_id,
				sessionBinding: binding, playId: playIdFor(issued), durationMs: track.duration_ms } });
		});
		await page.route(`${grantUrl}/media/**`, async (route) => {
			const call = route.request();
			if (call.method() !== 'GET' || !await isListener(call) || !activeMedia || call.url() !== origin + activeMedia.path) {
				return route.fulfill({ status: 404 });
			}
			return serveFlac(route, activeMedia.body);
		});

		const rows = page.getByTestId(library.item);
		const queue = page.getByTestId(contract.queueItem);
		const active = page.locator(contract.currentQueueItem);
		const player = page.getByRole('region', { name: 'Player', exact: true });
		const audio = page.getByTestId(contract.audio);
		const queueIds = () => queue.evaluateAll((items, attr) => items.map((item) => item.getAttribute(attr)), contract.queueId);
		const expectPlayingNearStart = () => expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
			!node.paused && !node.ended && node.currentTime > 0.2 && node.currentTime < 3
		)).toBe(true);

		await page.goto(origin + library.path);
		for (const { id } of [first, successor]) {
			await rows.and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
		}
		await expect.poll(queueIds).toEqual([first.id, successor.id]);
		await expect(active).toHaveAttribute(contract.queueId, first.id);
		await page.getByLabel('Repeat', { exact: true }).selectOption('one');
		await expect(page.getByTestId('repeat-status')).toHaveText('Repeat: One');
		await expect(audio).toHaveCount(0);
		expect(grants).toHaveLength(0);
		const firstGrant = page.waitForRequest(grantUrl);
		await player.getByRole('button', { name: 'Play', exact: true }).click();
		const tab: string = (await firstGrant).postDataJSON().session_id;
		const initialPath = pathFor(first.id, tab, 1);
		await expect(audio).toHaveAttribute('src', initialPath);
		await expectPlayingNearStart();

		// Let Chromium play the fixture to completion at its normal speed.
		const end = await audio.evaluate((node: HTMLAudioElement) => new Promise<{
			trusted: boolean; at: number; position: number; duration: number;
		}>((resolve) => {
			node.addEventListener('ended', (event) => resolve({
				trusted: event.isTrusted, at: event.timeStamp, position: node.currentTime, duration: node.duration
			}), { once: true });
		}));
		expect(end.trusted).toBe(true);
		expect(end.duration).toBeCloseTo(14, 2);
		expect(end.position).toBeCloseTo(end.duration, 2);

		await expectPlayingNearStart();
		await expect(active, 'natural completion keeps the first track selected').toHaveAttribute(contract.queueId, first.id);
		await expect(player.getByTestId('library-now-playing')).toHaveText(first.title);
		await expect.poll(queueIds).toEqual([first.id, successor.id]);
		const replayPath = pathFor(first.id, tab, 2);
		await expect(audio).toHaveAttribute('src', replayPath);
		expect([...mediaRequests]).toEqual([initialPath, replayPath]);
		// A repeat is a fresh start for the same tab, with no retry play ID.
		expect(grants.map((request) => request.postDataJSON())).toEqual([
			{ track_id: first.id, session_id: tab }, { track_id: first.id, session_id: tab }
		]);
		const grantStarts = await page.evaluate((url) =>
			performance.getEntriesByName(url, 'resource').map(({ startTime }) => startTime), grantUrl);
		expect(grantStarts).toHaveLength(2);
		expect(grantStarts[1], 'the second grant starts after the trusted end').toBeGreaterThanOrEqual(end.at);
	} finally {
		await server.close();
	}
});
