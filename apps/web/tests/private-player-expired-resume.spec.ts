import { expect } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { fixtureCookie, hasFixtureSession, serveFlac, test } from './helpers/private-music-fixture.js';

test('private Play resumes the same listen after its grant expires during Pause', async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const tracks = ['Paused tone', 'First upcoming tone', 'Second upcoming tone'].map((title, index) => ({
		id: `00000000-0000-4000-8000-00000000011${index}`, title, duration_ms: 14_000
	}));
	const [selected] = tracks;
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const member = 'synthetic-listener', binding = 'A'.repeat(43), playId = `private_browser_${'a'.repeat(22)}`;
	const playsUrl = origin + player.plays;
	const grantLifetime = 134_000; // 14 seconds of media plus the private browser window's slack.
	let now = Date.UTC(2026, 0, 1);
	const clockSkew = 12 * 60 * 60 * 1000;
	let expiresAt = 0, sessionId = '';
	const mediaPath = () => `${player.plays}/media/${selected.id}/${sessionId}/${playId}`;
	const plays: unknown[] = [], listenEvents: string[] = [];
	let allowRenewal = Promise.resolve();

	await page.clock.setFixedTime(now - clockSkew);
	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.addCookies([{ name: fixtureCookie, value: member, url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => hasFixtureSession(await route.request().headerValue('cookie'), member)
		? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 }));
	await page.route(origin + library.api, async (route) => hasFixtureSession(await route.request().headerValue('cookie'), member)
		? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: tracks }) : route.fulfill({ status: 401 }));
	await page.route(`${playsUrl}/events**`, (route) => {
		listenEvents.push(route.request().url());
		return route.fulfill({ status: 404 });
	});
	await page.route(playsUrl, async (route) => {
		const request = route.request();
		const input = request.postDataJSON();
		plays.push(input);
		if (request.method() !== 'POST' || !hasFixtureSession(await request.headerValue('cookie'), member) ||
			await request.headerValue('x-nntune-browser-playback') !== '1' ||
			await request.headerValue('x-nntune-session-binding') !== binding || input?.track_id !== selected.id) {
			return route.fulfill({ status: 404 });
		}
		if (!sessionId && input.play_id === undefined && typeof input.session_id === 'string' &&
			/^[\da-f]{8}-(?:[\da-f]{4}-){3}[\da-f]{12}$/.test(input.session_id)) {
			sessionId = input.session_id;
		} else {
			if (input.session_id !== sessionId || input.play_id !== playId) return route.fulfill({ status: 404 });
			await allowRenewal;
		}
		if (now >= expiresAt) expiresAt = now + grantLifetime;
		return route.fulfill({ headers: { 'x-nntune-playback-ttl': String((expiresAt - now) / 1000) }, json: {
			trackId: selected.id, playbackSessionId: sessionId, sessionBinding: binding, playId, durationMs: selected.duration_ms
		} });
	});
	await page.route(`${playsUrl}/media/**`, async (route) => {
		if (now >= expiresAt || route.request().url() !== origin + mediaPath() ||
			!hasFixtureSession(await route.request().headerValue('cookie'), member)) return route.fulfill({ status: 404 });
		return serveFlac(route, tone.body);
	});

	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const play = transport.getByRole('button', { name: 'Play', exact: true });
	const pause = transport.getByRole('button', { name: 'Pause', exact: true });
	const audio = transport.getByTestId(player.audio);
	const expectQueue = async () => {
		await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, selected.id);
		await expect(transport.getByTestId('library-now-playing')).toHaveText(selected.title);
		await expect.poll(() => page.getByTestId(player.queueItem).evaluateAll(
			(items, attr) => items.map((item) => item.getAttribute(attr)), player.queueId
		)).toEqual(tracks.map(({ id }) => id));
	};
	await page.goto(origin + library.path);
	for (const { id } of tracks) {
		await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
	}
	await expectQueue();
	await play.click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 1)).toBe(true);
	const initial = { track_id: selected.id, session_id: sessionId };
	const continuation = { ...initial, play_id: playId };
	const readRange = () => page.evaluate(async (src) => {
		const response = await fetch(src, { cache: 'no-store', headers: { range: 'bytes=0-3' } });
		return { status: response.status, bytes: await response.text() };
	}, mediaPath());
	const expectedPlays = [initial];
	for (let attempt = 0; attempt < 2; attempt++) {
		await pause.click();
		await expect(audio).toHaveJSProperty('paused', true);
		const resume = await audio.evaluateHandle((node: HTMLAudioElement) => {
			const observation = { node, pausedAt: node.currentTime, starts: [] as number[] };
			node.addEventListener('play', () => observation.starts.push(node.currentTime), { once: true });
			return observation;
		});
		now += grantLifetime;
		await page.clock.setFixedTime(now - clockSkew);
		expect(await readRange()).toEqual({ status: 404, bytes: '' });
		await expectQueue();
		expect(plays).toEqual(expectedPlays);
		const renewal = Promise.withResolvers<void>();
		allowRenewal = renewal.promise;
		expectedPlays.push(continuation);
		try {
			await play.click();
			await expect.poll(() => plays).toEqual(expectedPlays);
			await expect(audio).toHaveJSProperty('paused', true);
			expect(await resume.evaluate(({ starts }) => starts)).toEqual([]);
		} finally {
			renewal.resolve();
		}
		await expect.poll(() => resume.evaluate(({ node, pausedAt }) => !node.paused && !node.error && node.currentTime > pausedAt + 0.2)).toBe(true);
		const { pausedAt, starts } = await resume.evaluate(({ pausedAt, starts }) => ({ pausedAt, starts }));
		expect(starts).toHaveLength(1);
		expect(Math.abs(starts[0] - pausedAt)).toBeLessThan(0.05);
		await expect(audio).toHaveJSProperty('currentSrc', origin + mediaPath());
		expect(await readRange()).toEqual({ status: 206, bytes: 'fLaC' });
		await expectQueue();
		await resume.dispose();
	}
	expect(plays).toEqual(expectedPlays);
	expect(listenEvents).toEqual([]);
});
