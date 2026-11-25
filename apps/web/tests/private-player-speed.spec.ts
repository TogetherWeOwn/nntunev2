import { expect, type Request } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { fixtureCookie, hasFixtureSession, serveFlac, test } from './helpers/private-music-fixture.js';

test('natural advance keeps the selected speed when the next private track starts playing', async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const listener = 'synthetic-listener';
	const binding = 'A'.repeat(43);
	const grantUrl = origin + player.plays;
	const tracks = [
		{ id: '00000000-0000-4000-8000-000000000001', title: 'First Tone', file: 'tone-a.flac',
			playId: `private_browser_${'a'.repeat(22)}`, duration_ms: 14_000 },
		{ id: '00000000-0000-4000-8000-000000000002', title: 'Second Tone', file: 'tone-b.flac',
			playId: `private_browser_${'b'.repeat(22)}`, duration_ms: 14_000 }
	] as const;
	const [first, second] = tracks;
	const tones = privateStagingAudio();
	const isListener = async (request: Request) => hasFixtureSession(await request.headerValue('cookie'), listener);
	const grants: Request[] = [];
	const media = new Map<string, Buffer>();
	const mediaUrl = (track: (typeof tracks)[number], session: string) => `${grantUrl}/media/${track.id}/${session}/${track.playId}`;

	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => await isListener(route.request())
		? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 }));
	await page.route(origin + library.api, async (route) => {
		if (!await isListener(route.request())) return route.fulfill({ status: 401 });
		return route.fulfill({ headers: { 'x-nntune-session-binding': binding },
			json: tracks.map(({ id, title, duration_ms }) => ({ id, title, duration_ms })) });
	});
	page.on('request', (request) => { if (request.url() === grantUrl) grants.push(request); });
	await page.route(grantUrl, async (route) => {
		const request = route.request();
		if (request.method() !== 'POST' || !await isListener(request) ||
			await request.headerValue('x-nntune-browser-playback') !== '1' ||
			await request.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 400 });
		const { track_id, session_id, play_id } = request.postDataJSON() ?? {};
		const track = tracks.find(({ id }) => id === track_id);
		if (!track || typeof session_id !== 'string' || !/^[0-9a-f-]{36}$/.test(session_id) ||
			play_id !== undefined) return route.fulfill({ status: 400 });
		media.set(mediaUrl(track, session_id), tones.find(({ file }) => file === track.file)!.body);
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
	const audio = transport.getByTestId(player.audio).and(page.locator('audio[controls]'));
	const active = page.locator(player.currentQueueItem);
	const speed = page.getByRole('combobox', { name: 'Playback speed', exact: true });
	await page.goto(origin + library.path);
	for (const { id } of tracks) {
		await page.getByTestId(library.item).and(page.locator(`[${library.id}="${id}"]`)).locator(library.select).click();
	}
	await expect(active).toHaveAttribute(player.queueId, first.id);
	const firstGrant = page.waitForRequest(grantUrl);
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	const session: string = (await firstGrant).postDataJSON().session_id;
	const firstSource = mediaUrl(first, session);
	const secondSource = mediaUrl(second, session);
	await expect(audio).toHaveJSProperty('currentSrc', firstSource);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);
	await speed.selectOption('1.5');
	await expect(audio).toHaveJSProperty('playbackRate', 1.5);

	// Capture the rate when the destination starts, before a later update can mask a reset.
	const transition = await page.evaluateHandle(({ audioId, firstSource, secondSource }) => {
		const playback: { endedAtRate?: number; startedAtRate?: number } = {};
		const observe = (event: Event) => {
			const node = event.target;
			if (!event.isTrusted || !(node instanceof HTMLAudioElement) || node.dataset.testid !== audioId) return;
			if (event.type === 'ended' && node.currentSrc === firstSource) playback.endedAtRate ??= node.playbackRate;
			if (event.type === 'playing' && node.currentSrc === secondSource) playback.startedAtRate ??= node.playbackRate;
		};
		document.addEventListener('ended', observe, true);
		document.addEventListener('playing', observe, true);
		return playback;
	}, { audioId: player.audio, firstSource, secondSource });

	// Let the first FLAC finish at the selected speed and advance through the normal end handler.
	await expect(active).toHaveAttribute(player.queueId, second.id, { timeout: 15_000 });
	await expect(transport.getByTestId('library-now-playing')).toHaveText(second.title);
	await expect.poll(() => transition.jsonValue(), 'native rates at first completion and second start')
		.toEqual({ endedAtRate: 1.5, startedAtRate: 1.5 });
	await transition.dispose();
	await expect(speed).toHaveValue('1.5');
	const position = await audio.evaluate((node: HTMLAudioElement) => node.currentTime);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement, before) => ({
		source: node.currentSrc, rate: node.playbackRate,
		advancing: !node.paused && !node.ended && !node.error && node.currentTime > before + 0.15
	}), position)).toEqual({ source: secondSource, rate: 1.5, advancing: true });
	expect(grants.map((request) => request.postDataJSON())).toEqual([
		{ track_id: first.id, session_id: session },
		{ track_id: second.id, session_id: session }
	]);
});
