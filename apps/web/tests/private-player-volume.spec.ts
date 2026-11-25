import { expect, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { fixtureCookie, hasFixtureSession, serveFlac, test } from './helpers/private-music-fixture.js';

test('the next private track starts at the selected volume after natural completion', async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const grantUrl = origin + player.plays;
	const tones = privateStagingAudio();
	const tracks = [
		{ id: '00000000-0000-4000-8000-000000000001', title: 'First Tone', duration_ms: 14_000,
			playId: `private_browser_${'a'.repeat(22)}`, body: tones.find(({ file }) => file === 'tone-a.flac')!.body },
		{ id: '00000000-0000-4000-8000-000000000002', title: 'Second Tone', duration_ms: 14_000,
			playId: `private_browser_${'b'.repeat(22)}`, body: tones.find(({ file }) => file === 'tone-b.flac')!.body }
	] as const;
	const [first, next] = tracks;
	const isListener = async (request: Request) => hasFixtureSession(await request.headerValue('cookie'), listener);
	const media = new Map<string, Buffer>();
	const sourceFor = (track: (typeof tracks)[number], session: string) => `${grantUrl}/media/${track.id}/${session}/${track.playId}`;

	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => await isListener(route.request())
		? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 }));
	await page.route(origin + library.api, async (route) => {
		if (!await isListener(route.request())) return route.fulfill({ status: 401 });
		return route.fulfill({ headers: { 'x-nntune-session-binding': binding },
			json: tracks.map(({ id, title, duration_ms }) => ({ id, title, duration_ms })) });
	});
	await page.route(grantUrl, async (route) => {
		const request = route.request();
		if (request.method() !== 'POST' || !await isListener(request) ||
			await request.headerValue('x-nntune-browser-playback') !== '1' ||
			await request.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 400 });
		const { track_id, session_id, play_id } = request.postDataJSON() ?? {};
		const track = tracks.find(({ id }) => id === track_id);
		if (!track || typeof session_id !== 'string' || !/^[0-9a-f-]{36}$/.test(session_id) ||
			play_id !== undefined) return route.fulfill({ status: 400 });
		media.set(sourceFor(track, session_id), track.body);
		return route.fulfill({ json: {
			trackId: track.id, playbackSessionId: session_id, sessionBinding: binding,
			playId: track.playId, durationMs: track.duration_ms
		} });
	});
	await page.route(`${grantUrl}/media/**`, async (route) => {
		const request = route.request();
		const body = media.get(request.url());
		if (!body || request.method() !== 'GET' || !await isListener(request)) return route.fulfill({ status: 404 });
		return serveFlac(route, body);
	});

	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);
	const active = page.locator(player.currentQueueItem);
	const volume = transport.getByRole('slider', { name: /^Volume\b/ });
	await page.goto(origin + library.path);
	for (const { id } of tracks) {
		await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
	}
	await expect(active).toHaveAttribute(player.queueId, first.id);
	const firstGrant = page.waitForRequest(grantUrl);
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	const session: string = (await firstGrant).postDataJSON().session_id;
	const firstSource = sourceFor(first, session);
	const nextSource = sourceFor(next, session);

	// Capture the next element's volume at startup, before a later update could hide a reset.
	const transition = await page.evaluateHandle(({ audioId, firstSource, nextSource }) => {
		type Sample = { at: number; volume: number; muted: boolean };
		const samples: { end?: Sample; start?: Sample } = {};
		const observe = (event: Event) => {
			const node = event.target;
			if (!event.isTrusted || !(node instanceof HTMLAudioElement) || node.dataset.testid !== audioId) return;
			const sample = { at: event.timeStamp, volume: node.volume, muted: node.muted };
			if (event.type === 'ended' && node.currentSrc === firstSource) samples.end ??= sample;
			if (event.type === 'playing' && node.currentSrc === nextSource) samples.start ??= sample;
		};
		document.addEventListener('ended', observe, true);
		document.addEventListener('playing', observe, true);
		return samples;
	}, { audioId: player.audio, firstSource, nextSource });

	await expect(audio).toHaveJSProperty('currentSrc', firstSource);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);
	await expect(volume).toHaveValue('100');
	await volume.fill('35');
	await expect(audio).toHaveJSProperty('volume', 0.35);

	// Let Chromium finish the first tone on its own.
	await expect(active).toHaveAttribute(player.queueId, next.id, { timeout: 18_000 });
	await expect(transport.getByTestId('library-now-playing')).toHaveText(next.title);
	const expectedSound = { volume: 0.35, muted: false };
	await expect.poll(() => transition.jsonValue(), 'volume at native completion and next playback start')
		.toMatchObject({ end: expectedSound, start: expectedSound });
	const { end, start } = await transition.jsonValue();
	expect(start!.at, 'the next track starts after the first track ends').toBeGreaterThanOrEqual(end!.at);
	await transition.dispose();

	await expect(volume).toHaveValue('35');
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => ({
		source: node.currentSrc, volume: node.volume, muted: node.muted,
		playing: !node.paused && !node.ended && !node.error && node.currentTime > 0.2
	}))).toEqual({ source: nextSource, ...expectedSound, playing: true });
});
