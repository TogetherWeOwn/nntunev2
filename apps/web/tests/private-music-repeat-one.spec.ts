import { expect, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { fixtureCookie, hasFixtureSession, serveFlac, test } from './helpers/private-music-fixture.js';

const listener = 'synthetic-listener';
const binding = 'A'.repeat(43);
const isListener = async (request: Request) =>
	hasFixtureSession(await request.headerValue('cookie'), listener);

for (const { name, failFirstLoad } of [
	{ name: 'invited Library playback opens a fresh receipt to repeat the current track on natural end', failFirstLoad: false },
	{ name: 'invited Library playback repeats the recovered track on natural end after Retry', failFirstLoad: true }
]) {
	test(name, async ({ page, context, origin }) => {
		test.setTimeout(45_000);
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
		let initialMediaFailed = false;
		let continued = false;

		await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
		await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
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
			if (!track || typeof session_id !== 'string' || !/^[0-9a-f-]{36}$/.test(session_id)) {
				return route.fulfill({ status: 400 });
			}
			if (play_id === undefined) {
				activeMedia = { path: pathFor(track.id, session_id, ++issued), body: track.body };
			} else {
				if (!initialMediaFailed || continued || issued !== 1 || play_id !== playIdFor(1) ||
					activeMedia?.path !== pathFor(track.id, session_id, 1)) return route.fulfill({ status: 409 });
				continued = true;
			}
			return route.fulfill({ json: { trackId: track.id, playbackSessionId: session_id,
				sessionBinding: binding, playId: playIdFor(issued), durationMs: track.duration_ms } });
		});
		await page.route(`${grantUrl}/media/**`, async (route) => {
			const call = route.request();
			if (call.method() !== 'GET' || !await isListener(call) || !activeMedia || call.url() !== origin + activeMedia.path) {
				return route.fulfill({ status: 404 });
			}
			if (failFirstLoad && issued === 1 && !continued) {
				if (initialMediaFailed) return route.fulfill({ status: 409 });
				initialMediaFailed = true;
				return route.fulfill({ status: 503 });
			}
			return serveFlac(route, activeMedia.body);
		});

		const rows = page.getByTestId(library.item);
		const queue = page.getByTestId(contract.queueItem);
		const active = page.locator(contract.currentQueueItem);
		const player = page.getByRole('region', { name: 'Player', exact: true });
		const retry = player.getByRole('button', { name: 'Retry playback', exact: true });
		const audio = page.getByTestId(contract.audio);
		const queueIds = () => queue.evaluateAll((items, attr) => items.map((item) => item.getAttribute(attr)), contract.queueId);
		const expectPlayingNearStart = (path: string) => expect.poll(() => audio.evaluate((node: HTMLAudioElement, src) =>
			node.currentSrc === src && !node.paused && !node.ended && node.error === null &&
				node.currentTime > 0.2 && node.currentTime < 3,
			origin + path
		), 'private receipt audio plays near the start').toBe(true);

		await page.goto(origin + library.path);
		for (const { id } of [first, successor]) {
			await rows.and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
		}
		await expect.poll(queueIds).toEqual([first.id, successor.id]);
		await expect(active).toHaveAttribute(contract.queueId, first.id);
		const repeat = page.getByLabel('Repeat', { exact: true });
		const repeatStatus = page.getByTestId('repeat-status');
		await repeat.selectOption('one');
		await expect(repeatStatus).toHaveText('Repeat: One');
		await expect(audio).toHaveCount(0);
		expect(grants).toHaveLength(0);
		const firstGrant = page.waitForRequest(grantUrl);
		await player.getByRole('button', { name: 'Play', exact: true }).click();
		const tab: string = (await firstGrant).postDataJSON().session_id;
		const initialPath = pathFor(first.id, tab, 1);
		const start = { track_id: first.id, session_id: tab };
		const expectedGrants: (typeof start & { play_id?: string })[] = [start];
		if (failFirstLoad) {
			await expect(retry).toBeEnabled();
			await expect(audio).toHaveAttribute('src', initialPath);
			await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
				node.paused && node.error !== null)).toBe(true);
			expect(initialMediaFailed).toBe(true);
			expect(grants.map((request) => request.postDataJSON())).toEqual(expectedGrants);
			await retry.press('Enter');
			expectedGrants.push({ ...start, play_id: playIdFor(1) });
		}
		await expectPlayingNearStart(initialPath);
		await expect(retry).toHaveCount(0);
		await expect(repeat).toHaveValue('one');
		await expect(repeatStatus).toHaveText('Repeat: One');

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

		const replayPath = pathFor(first.id, tab, 2);
		await expectPlayingNearStart(replayPath);
		await expect(active, 'natural completion keeps the first track selected').toHaveAttribute(contract.queueId, first.id);
		await expect(player.getByTestId('library-now-playing')).toHaveText(first.title);
		await expect(repeat).toHaveValue('one');
		await expect(repeatStatus).toBeVisible();
		await expect(repeatStatus).toHaveText('Repeat: One');
		await expect.poll(queueIds).toEqual([first.id, successor.id]);
		expect([...mediaRequests]).toEqual([initialPath, replayPath]);
		// A repeat is a fresh start for the same tab, with no retry play ID.
		expect(grants.map((request) => request.postDataJSON())).toEqual([...expectedGrants, start]);
		const grantStarts = await page.evaluate((url) =>
			performance.getEntriesByName(url, 'resource').map(({ startTime }) => startTime), grantUrl);
		expect(grantStarts).toHaveLength(expectedGrants.length + 1);
		expect(grantStarts.at(-1), 'the replay grant starts after the trusted end').toBeGreaterThanOrEqual(end.at);
	});
}
