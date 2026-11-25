import { fileURLToPath } from 'node:url';
import { expect, test as base, type Request } from '@playwright/test';
import { expectedSyntheticBody } from '../../../e2e/helpers/demo-audio.js';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fixtureCookie, hasFixtureSession, playQueuedTrack } from './helpers/private-music-fixture.js';
import { withIsolatedViteFixture } from './vite-fixture.helpers.js';

const { library, player } = PRIVATE_MUSIC;
const listener = 'synthetic-previous-listener';
const binding = 'M'.repeat(43);
const duration = 16_381;
const predecessor = { id: '11000000-0000-4000-8000-000000000001', title: 'Previous target', duration_ms: duration };
const decoy = { id: '11000000-0000-4000-8000-000000000002', title: 'Earlier queue track', duration_ms: duration };
const selected = { id: '11000000-0000-4000-8000-000000000003', title: 'Current private track', duration_ms: duration };
const catalogue = [predecessor, decoy, selected];
const audioBodies = new Map([
	[predecessor.id, expectedSyntheticBody('second-signal')],
	[decoy.id, expectedSyntheticBody(null)],
	[selected.id, expectedSyntheticBody('third-light')]
]);

declare global {
	interface Window { __privatePreviousAction?: MediaSessionActionHandler }
}

const test = base.extend<{ origin: string }>({
	serviceWorkers: 'block',
	origin: async ({}, use) => {
		const previous = process.env['PUBLIC_BROWSER_PLAYBACK'];
		process.env['PUBLIC_BROWSER_PLAYBACK'] = '1';
		try {
			await withIsolatedViteFixture({
				root: fileURLToPath(new URL('../', import.meta.url)), mode: 'browser-fixtures',
				server: { host: '127.0.0.1', port: 0, ws: false }
			}, use);
		} finally {
			if (previous === undefined) delete process.env['PUBLIC_BROWSER_PLAYBACK'];
			else process.env['PUBLIC_BROWSER_PLAYBACK'] = previous;
		}
	}
});

for (const restart of [false, true]) {
	test(`private Previous ${restart ? 'restarts the authorized source after three seconds' : 'plays the live queue predecessor near track start'}`, async ({ page, context, origin }) => {
		const playsUrl = origin + player.plays;
		const grants: Request[] = [];
		const issued = new Map<string, Buffer>();
		const mediaRequests = new Set<string>();
		const isListener = async (request: Request) => hasFixtureSession(await request.headerValue('cookie'), listener);
		await context.route('**/*', (route) => {
			const url = new URL(route.request().url());
			return url.origin !== origin || /^\/(?:api|v1)\//.test(url.pathname) ? route.abort() : route.continue();
		});
		await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
		await page.route(`${origin}/v1/me/likes/session`, async (route) => await isListener(route.request())
			? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 }));
		await page.route(origin + library.api, async (route) => await isListener(route.request())
			? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: catalogue }) : route.fulfill({ status: 401 }));
		await page.route(playsUrl, async (route) => {
			const request = route.request();
			grants.push(request);
			if (request.method() !== 'POST' || !await isListener(request) ||
				await request.headerValue('x-nntune-browser-playback') !== '1' ||
				await request.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 403 });
			const { track_id, session_id, play_id } = request.postDataJSON();
			const body = audioBodies.get(track_id);
			if (!body || typeof session_id !== 'string' || !/^[0-9a-f-]{36}$/.test(session_id) || play_id !== undefined) {
				return route.fulfill({ status: 400 });
			}
			const playId = `previous_fixture_${grants.length}`;
			issued.set(`${playsUrl}/media/${track_id}/${session_id}/${playId}`, body);
			return route.fulfill({ json: { trackId: track_id, playbackSessionId: session_id,
				sessionBinding: binding, playId, durationMs: duration } });
		});
		await page.route(`${playsUrl}/media/**`, async (route) => {
			const request = route.request();
			mediaRequests.add(request.url());
			const body = issued.get(request.url());
			if (request.method() !== 'GET' || !body || !await isListener(request)) return route.fulfill({ status: 404 });
			const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers()['range'] ?? '');
			const start = range ? Number(range[1]) : 0;
			const end = range?.[2] ? Math.min(Number(range[2]), body.length - 1) : body.length - 1;
			if (start > end) return route.fulfill({ status: 416, headers: { 'content-range': `bytes */${body.length}` } });
			return route.fulfill({ status: range ? 206 : 200, contentType: 'audio/wav', body: body.subarray(start, end + 1),
				headers: { 'accept-ranges': 'bytes', ...(range && { 'content-range': `bytes ${start}-${end}/${body.length}` }) } });
		});
		await page.addInitScript(() => {
			const session = navigator.mediaSession;
			const register = session.setActionHandler.bind(session);
			session.setActionHandler = (action, handler) => {
				register(action, handler);
				if (action === 'previoustrack') window.__privatePreviousAction = handler ?? undefined;
			};
		});

		const queue = page.getByTestId(player.queueItem);
		const active = page.locator(player.currentQueueItem);
		const transport = page.getByRole('region', { name: 'Player', exact: true });
		const audio = transport.getByTestId(player.audio);
		const queueIds = () => queue.evaluateAll((items, attribute) => items.map((item) => item.getAttribute(attribute)), player.queueId);
		const expectPlaying = (source: string) => expect.poll(() => audio.evaluate((node: HTMLAudioElement, url) =>
			node.currentSrc === url && node.error === null && !node.paused && node.currentTime > 0.2, source)).toBe(true);

		await page.goto(origin + library.path);
		for (const { id } of catalogue) {
			await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
		}
		await expect.poll(queueIds).toEqual([predecessor.id, decoy.id, selected.id]);
		const initialGrant = await playQueuedTrack(page, origin, selected.title);
		expect(initialGrant.status()).toBe(200);
		const { session_id: tab } = initialGrant.request().postDataJSON();
		const source = `${playsUrl}/media/${selected.id}/${tab}/previous_fixture_1`;
		await expectPlaying(source);
		await queue.getByRole('button', { name: `Move ${predecessor.title} down`, exact: true }).click();
		const order = [decoy.id, predecessor.id, selected.id];
		await expect.poll(queueIds).toEqual(order);
		await expect(active).toHaveAttribute(player.queueId, selected.id);

		const native = await audio.elementHandle();
		if (restart) await expect.poll(() => native.evaluate((node: HTMLAudioElement) => node.currentTime)).toBeGreaterThan(4);
		const before = await native.evaluate(async (node: HTMLAudioElement, restart) => {
			// Reordering can take long enough to cross the cutoff on a busy runner.
			// Seek back and dispatch Previous in the same browser turn once it lands.
			if (!restart) await new Promise<void>((resolve) => {
				node.addEventListener('seeked', () => resolve(), { once: true });
				node.currentTime = 0.5;
			});
			const previous = window.__privatePreviousAction;
			if (!previous) throw new Error('Missing Media Session Previous handler');
			const snapshot = { position: node.currentTime, source: node.currentSrc, playing: !node.paused };
			previous({ action: 'previoustrack' });
			return snapshot;
		}, restart);
		expect(before).toMatchObject({ source, playing: true });
		expect(before.position).toBeGreaterThan(restart ? 4 : 0.2);
		expect(before.position).toBeLessThan(restart ? 8 : 3);

		if (restart) {
			await expect(transport.getByTestId('library-restart-status')).toHaveText(`Restarted ${selected.title}.`);
			await expect.poll(() => native.evaluate((node: HTMLAudioElement, url) => node.isConnected &&
				node.currentSrc === url && !node.paused && !node.seeking && node.currentTime < 2, source)).toBe(true);
			await expect(active).toHaveAttribute(player.queueId, selected.id);
			expect(grants.map((request) => request.postDataJSON())).toEqual([{ track_id: selected.id, session_id: tab }]);
			expect([...mediaRequests]).toEqual([source]);
		} else {
			const previousSource = `${playsUrl}/media/${predecessor.id}/${tab}/previous_fixture_2`;
			await expect(active).toHaveAttribute(player.queueId, predecessor.id);
			await expect(audio).toHaveJSProperty('currentSrc', previousSource);
			await expectPlaying(previousSource);
			expect(await native.evaluate((node: HTMLAudioElement) => !node.isConnected && node.paused)).toBe(true);
			expect(grants.map((request) => request.postDataJSON())).toEqual([
				{ track_id: selected.id, session_id: tab }, { track_id: predecessor.id, session_id: tab }
			]);
			expect([...mediaRequests]).toEqual([source, previousSource]);
		}
		await expect(audio).toHaveCount(1);
		await expect.poll(queueIds).toEqual(order);
	});
}
