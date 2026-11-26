import { expect, type Request, type Response } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';
import { expectPlaybackRefusal, fixtureCookie, hasFixtureSession, serveFlac, test } from './helpers/private-music-fixture.js';

test('same-track Retry keeps the chosen speed after the private playback grant expires', async ({ page, context, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const track = { id: '00000000-0000-4000-8000-000000000121', title: 'Renewal tone', duration_ms: 14_000 };
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const listener = 'synthetic-listener', binding = 'A'.repeat(43), playId = `private_browser_${'a'.repeat(22)}`;
	const grantUrl = origin + player.plays;
	const isListener = async (request: Request) => hasFixtureSession(await request.headerValue('cookie'), listener);
	const grants: unknown[] = [];
	const grantLifetime = 134;
	let now = 0, expiresAt = 0, sessionId = '';
	let refuseRenewal = true;
	const mediaUrl = () => `${grantUrl}/media/${track.id}/${sessionId}/${playId}`;

	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.addCookies([{ name: fixtureCookie, value: listener, url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => await isListener(route.request())
		? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 }));
	await page.route(origin + library.api, async (route) => await isListener(route.request())
		? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: [track] }) : route.fulfill({ status: 401 }));
	await page.route(grantUrl, async (route) => {
		const request = route.request();
		if (request.method() !== 'POST' || !await isListener(request) ||
			await request.headerValue('x-nntune-browser-playback') !== '1' ||
			await request.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 404 });
		const input = request.postDataJSON();
		grants.push(input);
		if (input?.track_id !== track.id) return route.fulfill({ status: 404 });
		if (!sessionId && input.play_id === undefined && typeof input.session_id === 'string' &&
			/^[\da-f]{8}-(?:[\da-f]{4}-){3}[\da-f]{12}$/.test(input.session_id)) {
			sessionId = input.session_id;
		} else {
			if (input.session_id !== sessionId || input.play_id !== playId || now < expiresAt) {
				return route.fulfill({ status: 404 });
			}
			if (refuseRenewal) return route.fulfill({ status: 503 });
		}
		expiresAt = now + grantLifetime;
		return route.fulfill({ json: {
			trackId: track.id, playbackSessionId: sessionId, sessionBinding: binding, playId, durationMs: track.duration_ms
		} });
	});
	await page.route(`${grantUrl}/media/**`, async (route) => {
		const request = route.request();
		if (!sessionId || request.url() !== mediaUrl() || now >= expiresAt || !await isListener(request)) {
			return route.fulfill({ status: 404 });
		}
		return serveFlac(route, tone.body);
	});

	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio).and(page.locator('audio[controls]'));
	const retry = transport.getByRole('button', { name: 'Retry playback', exact: true });
	const speed = page.getByRole('combobox', { name: 'Playback speed', exact: true });
	const rate = 1.5;
	const expectRate = async () => {
		await expect(audio).toHaveJSProperty('playbackRate', rate);
		await expect(speed).toHaveValue(String(rate));
	};
	await page.goto(origin + library.path);
	await page.getByTestId(library.item).and(page.locator(`[${library.id}="${track.id}"]`)).locator(library.select).click();
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);
	await speed.selectOption(String(rate));
	await expectRate();
	const node = await audio.evaluateHandle((element: HTMLAudioElement) => element);
	const start = { track_id: track.id, session_id: sessionId };
	const continuation = { ...start, play_id: playId };
	const waitForMedia = (matches: (response: Response) => boolean) => page.waitForResponse((response) =>
		response.url() === mediaUrl() && response.request().resourceType() === 'media' && matches(response));

	now = expiresAt;
	await Promise.all([
		waitForMedia((response) => response.status() === 404),
		node.evaluate((element) => {
			// Force a read at expiry even if Chromium already buffered the entire tone.
			// Keep the chosen rate through this fixture reload so the real Retry owns the rate handoff.
			const chosen = element.playbackRate;
			element.load();
			element.playbackRate = chosen;
		})
	]);
	await expect(retry).toBeEnabled();
	await expectRate();
	await expect(audio).toHaveJSProperty('defaultPlaybackRate', 1);

	const [refusal] = await Promise.all([page.waitForResponse(grantUrl), retry.press('Enter')]);
	expect(refusal.status()).toBe(503);
	await expectPlaybackRefusal(page);
	await expect(retry).toBeEnabled();
	expect(await node.evaluate((element) => element.paused && element.error !== null)).toBe(true);
	await expectRate();
	expect(grants).toEqual([start, continuation]);

	refuseRenewal = false;
	await Promise.all([waitForMedia((response) => response.ok()), retry.press('Enter')]);
	await expect.poll(() => node.evaluate((element) => element.isConnected && !element.paused &&
		element.error === null && element.readyState >= element.HAVE_CURRENT_DATA)).toBe(true);
	await expect(audio).toHaveJSProperty('currentSrc', mediaUrl());
	await expectRate();
	await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, track.id);
	expect(grants).toEqual([start, continuation, continuation]);
	await node.dispose();
});
