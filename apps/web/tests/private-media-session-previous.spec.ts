import { fileURLToPath } from 'node:url';
import { expect, test as base, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { fixtureCookie, hasFixtureSession, serveFlac } from './helpers/private-music-fixture.js';
import { withIsolatedViteFixture } from './vite-fixture.helpers.js';

declare global {
	interface Window { __privateMediaSessionPrevious?: MediaSessionActionHandler }
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

test('Media Session Previous grants and plays the first private track after natural advance', async ({ page, context, origin }) => {
	test.setTimeout(45_000);
	const { library, player } = PRIVATE_MUSIC;
	const first = { id: '00000000-0000-4000-8000-000000000001', title: 'First private tone', duration_ms: 14_000 };
	const second = { id: '00000000-0000-4000-8000-000000000002', title: 'Second private tone', duration_ms: 14_000 };
	const tones = privateStagingAudio();
	const catalogue = [first, second];
	const media = new Map([
		[first.id, tones.find(({ file }) => file === 'tone-a.flac')!.body],
		[second.id, tones.find(({ file }) => file === 'tone-b.flac')!.body]
	]);
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const isListener = async (call: Request) => hasFixtureSession(await call.headerValue('cookie'), listener);
	const plays = origin + player.plays;
	const sourceFor = (id: string, tab: string, issue: number) => `${plays}/media/${id}/${tab}/fixture-previous-${issue}`;
	const requests: Request[] = [];
	let issued: { source: string; body: Buffer } | undefined;
	let issues = 0;

	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		return url.origin !== origin || /^\/(?:api|v1)\//.test(url.pathname) ? route.abort() : route.continue();
	});
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => await isListener(route.request())
		? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 }));
	await page.route(origin + library.api, async (route) => await isListener(route.request())
		? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: catalogue }) : route.fulfill({ status: 401 }));
	await page.route(plays, async (route) => {
		const call = route.request();
		requests.push(call);
		if (call.method() !== 'POST' || !await isListener(call) ||
			await call.headerValue('x-nntune-browser-playback') !== '1' ||
			await call.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 403 });
		const { track_id, session_id, play_id } = call.postDataJSON() ?? {};
		const track = catalogue.find(({ id }) => id === track_id);
		if (!track || typeof session_id !== 'string' || !/^[0-9a-f-]{36}$/.test(session_id) || play_id !== undefined) {
			return route.fulfill({ status: 400 });
		}
		const issue = ++issues;
		issued = { source: sourceFor(track.id, session_id, issue), body: media.get(track.id)! };
		return route.fulfill({ json: { trackId: track.id, playbackSessionId: session_id, sessionBinding: binding,
			playId: `fixture-previous-${issue}`, durationMs: track.duration_ms } });
	});
	await page.route(`${plays}/media/**`, async (route) => {
		const call = route.request();
		const grant = issued;
		if (call.method() !== 'GET' || !grant || call.url() !== grant.source || !await isListener(call)) {
			return route.fulfill({ status: 404 });
		}
		return serveFlac(route, grant.body);
	});
	await page.addInitScript(() => {
		const session = navigator.mediaSession;
		const register = session.setActionHandler.bind(session);
		session.setActionHandler = (action, handler) => {
			register(action, handler);
			if (action === 'previoustrack') window.__privateMediaSessionPrevious = handler ?? undefined;
		};
	});

	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const nowPlaying = transport.getByTestId('library-now-playing');
	const audio = transport.getByTestId(player.audio);
	const expectPlaying = (source: string) => expect.poll(() => audio.evaluate((node: HTMLAudioElement, url) =>
		node.currentSrc === url && !node.paused && !node.ended && node.error === null && node.currentTime > 0.2, source),
		`native playback advances on ${source}`).toBe(true);
	await page.goto(origin + library.path);
	for (const { id } of catalogue) {
		await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
	}
	await expect(nowPlaying).toHaveText(first.title);
	const start = page.waitForResponse(plays);
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	const initial = await start;
	expect(initial.status()).toBe(200);
	const tab: string = initial.request().postDataJSON().session_id;
	await expectPlaying(sourceFor(first.id, tab, 1));

	// The native end of the first FLAC advances to the second track.
	await expect(nowPlaying).toHaveText(second.title, { timeout: 20_000 });
	const secondSource = sourceFor(second.id, tab, 2);
	await expectPlaying(secondSource);
	expect(requests.map((call) => call.postDataJSON())).toEqual(catalogue.map(({ id }) => ({ track_id: id, session_id: tab })));
	await audio.evaluate(async (node: HTMLAudioElement, url) => {
		// Dispatch just after seeking to the start so a slow runner cannot cross the restart threshold.
		await new Promise<void>((resolve) => {
			node.addEventListener('seeked', () => resolve(), { once: true });
			node.currentTime = 0.5;
		});
		if (node.currentSrc !== url || node.paused || node.ended || node.currentTime >= 3) {
			throw new Error('Expected the second track to be playing near the start');
		}
		const previous = window.__privateMediaSessionPrevious;
		if (!previous) throw new Error('Missing Chromium Media Session Previous handler');
		previous({ action: 'previoustrack' });
	}, secondSource);

	await expect(nowPlaying).toHaveText(first.title);
	await expect.poll(() => requests.map((call) => call.postDataJSON()), 'Previous requests a fresh grant for the first track').toEqual([
		{ track_id: first.id, session_id: tab }, { track_id: second.id, session_id: tab }, { track_id: first.id, session_id: tab }
	]);
	await expectPlaying(sourceFor(first.id, tab, 3));
	await expect(audio).toHaveCount(1);
});
