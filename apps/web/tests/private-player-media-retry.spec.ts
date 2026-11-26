import { fileURLToPath } from 'node:url';
import { errors, expect, test as base, type Page, type Route } from '@playwright/test';
import { preview } from 'vite';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { privateStagingAudio } from '../../../e2e/helpers/private-staging-fixture.js';

const selected = { id: '00000000-0000-4000-8000-000000001101', title: 'Relay retry tone', duration_ms: 14_000 };
const companion = { id: '00000000-0000-4000-8000-000000001102', title: 'Queue companion', duration_ms: 14_000 };
const binding = 'A'.repeat(43);
const playId = `private_browser_${'a'.repeat(22)}`;
const tabId = /^[\da-f]{8}-(?:[\da-f]{4}-){3}[\da-f]{12}$/;
const cookie = { name: 'private_retry_fixture_session', value: 'synthetic-listener' };
const initialSpeed = '1.5';
const listener = async (route: Route) => (await route.request().headerValue('cookie'))
	?.split('; ').includes(`${cookie.name}=${cookie.value}`);

function serveFlac(route: Route, body: Buffer) {
	const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers()['range'] ?? '');
	const start = range ? Number(range[1]) : 0;
	const end = range?.[2] ? Math.min(Number(range[2]), body.length - 1) : body.length - 1;
	if (start > end) return route.fulfill({ status: 416, headers: { 'content-range': `bytes */${body.length}` } });
	return route.fulfill({
		status: range ? 206 : 200,
		contentType: 'audio/flac',
		body: body.subarray(start, end + 1),
		headers: { 'accept-ranges': 'bytes', ...(range && { 'content-range': `bytes ${start}-${end}/${body.length}` }) }
	});
}

const test = base.extend<{ origin: string }>({
	origin: async ({}, use) => {
		// Arm private playback only in this loopback preview of the browser-fixtures build.
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
			if (!address || typeof address === 'string') throw new Error('Missing retry fixture TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally {
			await server.close();
		}
	}
});

test.use({ serviceWorkers: 'block' });

async function checkRelayRetry(page: Page, origin: string, {
	refuseFirstContinuation = false, resumedSpeed = initialSpeed, expireWhilePlaying = false
} = {}) {
	const context = page.context();
	const { library, player: contract } = PRIVATE_MUSIC;
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const playsUrl = origin + contract.plays;
	const mediaPrefix = `${playsUrl}/media/`;
	const mediaPathFor = (sessionId: string) => `${contract.plays}/media/${selected.id}/${sessionId}/${playId}`;
	const plays: unknown[] = [];
	const mediaRequests: string[] = [];
	const releaseContinuation = Promise.withResolvers<void>();
	let playbackSessionId = '';
	let failFirstLoad = !expireWhilePlaying;
	let grantExpired = false;
	let continuationConfirmed = false;

	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.addCookies([{ ...cookie, url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => await listener(route)
		? route.fulfill({ json: { session_binding: binding } }) : route.fulfill({ status: 401 }));
	await page.route(origin + library.api, async (route) => await listener(route)
		? route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: [selected, companion] })
		: route.fulfill({ status: 401 }));
	await page.route(playsUrl, async (route) => {
		const call = route.request();
		if (call.method() !== 'POST') return route.fulfill({ status: 405 });
		const input = call.postDataJSON();
		plays.push(input);
		if (!await listener(route) || input?.track_id !== selected.id || !tabId.test(input?.session_id) ||
			await call.headerValue('x-nntune-browser-playback') !== '1' ||
			await call.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 400 });
		if (!playbackSessionId && input.play_id === undefined) {
			playbackSessionId = input.session_id;
		} else if (input.session_id !== playbackSessionId || input.play_id !== playId || (expireWhilePlaying && !grantExpired)) {
			return route.fulfill({ status: 409 });
		} else if (refuseFirstContinuation && plays.length === 2) {
			return route.fulfill({ status: 503 });
		} else {
			await releaseContinuation.promise;
		}
		return route.fulfill({ json: {
			trackId: selected.id, playbackSessionId, sessionBinding: binding, playId, durationMs: selected.duration_ms
		} });
	});
	page.on('response', (response) => {
		if (response.url() === playsUrl && response.ok() && response.request().postDataJSON()?.play_id === playId) {
			continuationConfirmed = true;
		}
	});
	await page.route(`${mediaPrefix}**`, async (route) => {
		const url = route.request().url();
		mediaRequests.push(url);
		if (!playbackSessionId || url !== origin + mediaPathFor(playbackSessionId) || !await listener(route)) {
			return route.fulfill({ status: 404 });
		}
		if (failFirstLoad) {
			failFirstLoad = false;
			return route.fulfill({ status: 503 });
		}
		if (!continuationConfirmed) {
			if (grantExpired) return route.fulfill({ status: 404 });
			if (!expireWhilePlaying) return route.fulfill({ status: 409 });
		}
		return serveFlac(route, tone.body);
	});

	const player = page.getByRole('region', { name: 'Player', exact: true });
	const audio = player.getByTestId(contract.audio);
	const status = player.getByRole('status');
	const retry = player.getByRole('button', { name: 'Retry playback', exact: true });
	const speed = page.getByRole('combobox', { name: 'Playback speed', exact: true });
	const seek = player.getByRole('slider', { name: 'Seek', exact: true });
	const playbackFailure = 'This audio could not be loaded or played. Retry playback or choose another track.';
	const audioFailed = () => audio.evaluate((node: HTMLAudioElement) => node.paused && node.error !== null);
	const expectNoPlaybackRequest = () => expect(page.waitForRequest(
		(request) => request.url() === playsUrl || request.url().startsWith(mediaPrefix), { timeout: 750 }
	)).rejects.toThrow(errors.TimeoutError);
	const assertSelection = async () => {
		await expect.poll(() => page.getByTestId(contract.queueItem).evaluateAll(
			(items, attr) => items.map((item) => item.getAttribute(attr)), contract.queueId
		)).toEqual([selected.id, companion.id]);
		await expect(page.locator(contract.currentQueueItem)).toHaveAttribute(contract.queueId, selected.id);
		await expect(player.getByTestId('library-now-playing')).toHaveText(selected.title);
	};

	await page.goto(origin + library.path);
	for (const { id } of [selected, companion]) {
		await page.getByTestId(library.item).and(page.locator(`[data-id="${id}"]`)).locator(library.select).click();
	}
	await assertSelection();
	await expect(audio).toHaveCount(0);
	expect(plays).toEqual([]);
	expect(mediaRequests).toEqual([]);

	const initialMedia = page.waitForResponse((response) => response.url().startsWith(mediaPrefix));
	await player.getByRole('button', { name: 'Play', exact: true }).click();
	let checkpoint: number | undefined;
	if (expireWhilePlaying) {
		expect((await initialMedia).ok()).toBe(true);
		await expect(status).toHaveText('Playing');
		await expect.poll(async () => Number(await seek.inputValue())).toBeGreaterThan(2);
		const slider = (await seek.elementHandle())!;
		grantExpired = true;
		const expiredRead = page.waitForResponse((response) => response.url().startsWith(mediaPrefix) && response.status() === 404);
		try {
			checkpoint = await audio.evaluate((node: HTMLAudioElement, control: HTMLInputElement) => {
				const elapsed = control.valueAsNumber;
				node.addEventListener('playing', () => { node.dataset.resumedAt = String(node.currentTime); }, { once: true });
				// Force Chromium to reread the expired locator even if the tone is fully buffered.
				node.load();
				return elapsed;
			}, slider);
		} finally {
			await slider.dispose();
		}
		await expiredRead;
	} else expect((await initialMedia).status()).toBe(503);

	const expectCheckpoint = async () => {
		if (checkpoint === undefined) return;
		await expect(seek).toBeDisabled();
		await expect(seek).toHaveValue(String(checkpoint));
	};
	await expect(status).toHaveText(playbackFailure);
	await expect(retry).toBeEnabled();
	await expect.poll(audioFailed).toBe(true);
	await expectCheckpoint();
	await assertSelection();

	await expectNoPlaybackRequest();
	await expect(retry).toBeEnabled();
	const start = { track_id: selected.id, session_id: playbackSessionId };
	expect(playbackSessionId).toMatch(tabId);
	expect(plays).toEqual([start]);
	const mediaPath = mediaPathFor(playbackSessionId);
	const mediaUrl = origin + mediaPath;
	const failedMediaRequests = [...mediaRequests];
	if (expireWhilePlaying) expect(failedMediaRequests.length).toBeGreaterThan(1);
	else expect(failedMediaRequests).toEqual([mediaUrl]);
	expect(new Set(failedMediaRequests)).toEqual(new Set([mediaUrl]));
	await expect(audio).toHaveAttribute('src', mediaPath);
	await speed.selectOption(initialSpeed);
	await expect(speed).toHaveValue(initialSpeed);
	await expect(audio).toHaveJSProperty('playbackRate', Number(initialSpeed));

	const continuation = { ...start, play_id: playId };
	const expectedPlays = refuseFirstContinuation ? [start, continuation, continuation] : [start, continuation];
	const firstPlaying = await audio.evaluateHandle((node: HTMLAudioElement) => {
		const observation = { rate: null as number | null };
		node.addEventListener('playing', (event) => {
			if (event.isTrusted) observation.rate ??= node.playbackRate;
		});
		return observation;
	});
	try {
		if (refuseFirstContinuation) {
			const refused = page.waitForResponse(playsUrl);
			await retry.press('Enter');
			expect((await refused).status()).toBe(503);
			await expect(page.getByTestId('queue-announcement')).toHaveText('Could not start playback.');
			await expect(status).toHaveText(playbackFailure);
			await expect(retry).toBeEnabled();
			await expectNoPlaybackRequest();
			expect(plays).toEqual([start, continuation]);
			expect(mediaRequests).toEqual(failedMediaRequests);
			expect(await audioFailed()).toBe(true);
			await expectCheckpoint();
			await expect(audio).toHaveAttribute('src', mediaPath);
			await assertSelection();
		}

		await retry.press('Enter');
		await expect.poll(() => plays).toEqual(expectedPlays);
		if (resumedSpeed !== initialSpeed) await speed.selectOption(resumedSpeed);
		await expect(speed).toHaveValue(resumedSpeed);
		await expect(audio).toHaveJSProperty('playbackRate', Number(resumedSpeed));
		// Hold the receipt response and check that Retry has not reloaded the decoder.
		await expectNoPlaybackRequest();
		expect(continuationConfirmed).toBe(false);
		expect(mediaRequests).toEqual(failedMediaRequests);
		expect(await audioFailed()).toBe(true);
	} finally {
		releaseContinuation.resolve();
	}
	await expect(status).toHaveText('Playing');
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement, elapsed) =>
		!node.paused && node.error === null && node.currentTime > elapsed + 0.2, checkpoint ?? 0)).toBe(true);
	if (checkpoint !== undefined) {
		await expect(audio).toHaveAttribute('data-resumed-at', /\d/);
		expect(Math.abs(Number(await audio.getAttribute('data-resumed-at')) - checkpoint)).toBeLessThan(0.3);
	}
	await expect.poll(() => firstPlaying.jsonValue()).toEqual({ rate: Number(resumedSpeed) });
	await firstPlaying.dispose();
	await expect(audio).toHaveJSProperty('playbackRate', Number(resumedSpeed));
	await expect(speed).toHaveValue(resumedSpeed);
	await expect(retry).toHaveCount(0);
	await expect(audio).toHaveAttribute('src', mediaPath);
	await assertSelection();
	expect(plays).toEqual(expectedPlays);
	// A single resumed FLAC load may legitimately make several byte-range requests.
	expect(mediaRequests.length).toBeGreaterThan(failedMediaRequests.length);
	expect(new Set(mediaRequests)).toEqual(new Set([mediaUrl]));
}

test('invited Library playback retries a failed relay using the original receipt', async ({ page, origin }) => {
	await checkRelayRetry(page, origin);
});

test('invited Library Retry resumes at the speed chosen while its receipt was pending', async ({ page, origin }) => {
	await checkRelayRetry(page, origin, { resumedSpeed: '1.75' });
});

test('invited Library playback waits for another Retry after a temporary receipt failure', async ({ page, origin }) => {
	await checkRelayRetry(page, origin, { refuseFirstContinuation: true });
});

test('invited Library playback resumes from its last position after an expired renewal returns 503', async ({ page, origin }) => {
	await checkRelayRetry(page, origin, { expireWhilePlaying: true, refuseFirstContinuation: true });
});
