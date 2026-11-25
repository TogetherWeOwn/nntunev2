import { fileURLToPath } from 'node:url';
import { expect, test as base, type Route } from '@playwright/test';
import { preview } from 'vite';
import { assertCuratedLibrary, PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { freshTrialPublication, privateStagingAudio, PRIVATE_TRIAL_UPLOAD } from '../../../e2e/helpers/private-staging-fixture.js';
import { seekProofFailure, seekViaPlayerUi, waitForPaused } from '../../../e2e/helpers/player-ui.js';

const uploadedId = '00000000-0000-4000-8000-000000000001';
const companionId = '00000000-0000-4000-8000-000000000002';
const binding = 'A'.repeat(43);
const playId = '00000000-0000-4000-8000-000000000003';
// Loopback HTTP uses a fixture-only cookie. The staged runner sends the real cookie over HTTPS.
const fixtureCookie = 'private_music_fixture_session';
const listener = `${fixtureCookie}=synthetic-listener`;
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
		// Serve the existing browser-fixtures build with the runtime arm set only here.
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

test('fresh private publication reaches the invited Library and plays under its canonical ID', async ({ page, context, browser, origin }) => {
	const upload = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const companion = privateStagingAudio().find(({ file }) => file === 'tone-b.flac')!;
	const rows = page.getByTestId(PRIVATE_MUSIC.library.item);
	const refresh = page.getByRole('button', { name: PRIVATE_MUSIC.library.refresh, exact: true });
	const ids = () => rows.evaluateAll((items) => items.map((row) => row.getAttribute('data-id')!));
	const row = (id: string) => rows.and(page.locator(`[data-id="${id}"]`));
	let publication: { trackId: string; sha256: string } | undefined;
	let revoked = false;
	let playbackSession: string | undefined;
	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await page.route(`${origin}/v1/me/likes/session`, async (route) => {
		if (!isListener(await route.request().headerValue('cookie'))) return route.fulfill({ status: 401 });
		if (revoked) return route.fulfill({ status: 403 });
		return route.fulfill({ json: { session_binding: binding } });
	});
	await page.route(`${origin}${PRIVATE_MUSIC.library.api}`, async (route) => {
		const cookie = await route.request().headerValue('cookie');
		if (!isListener(cookie)) return route.fulfill({ status: 401 });
		if (revoked) return route.fulfill({ status: 403 });
		const seed = { id: companionId, title: 'Queue companion', duration_ms: 14_000 };
		const items = publication ? [{ id: publication.trackId, title: 'Fresh publication', duration_ms: 14_000 }, seed] : [seed];
		return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: items });
	});
	await page.route(`${origin}${PRIVATE_MUSIC.player.plays}`, async (route) => {
		const call = route.request();
		if (!isListener(await call.headerValue('cookie')) || revoked) return route.fulfill({ status: 403 });
		const input = call.postDataJSON();
		if (call.method() !== 'POST' || !publication || input.track_id !== publication.trackId ||
			await call.headerValue('x-nntune-browser-playback') !== '1' || await call.headerValue('x-nntune-session-binding') !== binding) {
			return route.fulfill({ status: 400 });
		}
		expect(input.session_id).toMatch(/^[0-9a-f-]{36}$/);
		playbackSession = input.session_id;
		return route.fulfill({ json: { trackId: publication.trackId, playbackSessionId: playbackSession, sessionBinding: binding, playId, durationMs: 14_000 } });
	});
	await page.route(`${origin}/v1/plays/media/**`, async (route) => {
		const call = route.request();
		if (!isListener(await call.headerValue('cookie')) || revoked || !publication || !playbackSession ||
			call.url() !== `${origin}/v1/plays/media/${publication.trackId}/${playbackSession}/${playId}`) {
			return route.fulfill({ status: 404 });
		}
		return serveFlac(route, upload.body);
	});

	const anonymous = page.waitForResponse((response) => response.url() === `${origin}${PRIVATE_MUSIC.library.api}`);
	await page.goto(`${origin}${PRIVATE_MUSIC.library.path}`);
	expect((await anonymous).status()).toBe(401);
	await expect(rows).toHaveCount(0);
	await context.addCookies([{ name: fixtureCookie, value: 'synthetic-listener', url: origin, httpOnly: true }]);
	await page.reload();
	await expect(row(companionId)).toBeVisible();
	const before = await ids();
	expect(before).toEqual([companionId]);

	// The browser keeps the invited session. A separate request uses the owner cookie.
	const owner = await browser.newContext({ serviceWorkers: 'block' });
	try {
		await owner.addCookies([{ name: fixtureCookie, value: 'synthetic-owner', url: origin, httpOnly: true }]);
		await owner.route(`${origin}${PRIVATE_TRIAL_UPLOAD}`, async (route) => {
			const call = route.request();
			if (call.method() !== 'POST' || await call.headerValue('cookie') !== `${fixtureCookie}=synthetic-owner` ||
				await call.headerValue('origin') !== origin || await call.headerValue('authorization')) return route.fulfill({ status: 404 });
			if (!call.postDataBuffer()?.equals(upload.body)) return route.fulfill({ status: 400 });
			publication = { trackId: uploadedId, sha256: upload.sha256 };
			return route.fulfill({ json: publication });
		});
		const ownerPage = await owner.newPage();
		await ownerPage.goto(`${origin}/api/build`);
		const receipt = await ownerPage.evaluate(async ({ path, bytes }) => {
			const response = await fetch(path, { method: 'POST', headers: { 'Content-Type': 'audio/flac' }, body: Uint8Array.from(bytes) });
			return { status: response.status, body: await response.text() };
		}, { path: PRIVATE_TRIAL_UPLOAD, bytes: [...upload.body] });
		expect(receipt.status).toBe(200);
		const fresh = freshTrialPublication(receipt.body, upload.sha256, before);
		await refresh.click();
		await expect(row(fresh.trackId)).toBeVisible();
		const order = assertCuratedLibrary([upload.sha256, companion.sha256], [fresh, { trackId: companionId, sha256: companion.sha256 }], await ids());
		expect(order).toEqual([fresh.trackId, companionId]);
		for (const id of order) await row(id).locator(PRIVATE_MUSIC.library.select).click();
	} finally {
		await owner.close();
	}

	const player = page.getByRole('region', { name: 'Player', exact: true });
	const audio = page.getByTestId(PRIVATE_MUSIC.player.audio);
	const active = page.locator(PRIVATE_MUSIC.player.currentQueueItem);
	await expect(active).toHaveAttribute(PRIVATE_MUSIC.player.queueId, uploadedId);
	await expect(audio).toHaveCount(0);
	await player.getByRole('button', { name: 'Play', exact: true }).click();
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => !el.paused && el.currentTime > 0.2)).toBe(true);
	await expect(audio).toHaveAttribute('src', `/v1/plays/media/${uploadedId}/${playbackSession}/${playId}`);
	await player.getByRole('button', { name: 'Pause', exact: true }).click();
	await waitForPaused(audio, true, 5_000, 'Player did not pause');
	await audio.evaluate((el) => el.addEventListener('seeked', () => { el.dataset.seeked = 'yes'; }, { once: true }));
	const { fraction } = await seekViaPlayerUi(page, audio, 0.55);
	await expect(audio).toHaveAttribute('data-seeked', 'yes');
	expect(seekProofFailure(await waitForPaused(audio, true, 5_000, 'Seek did not finish paused'), fraction, true)).toBeNull();
	await page.getByTestId(PRIVATE_MUSIC.player.next).click();
	await expect(active).toHaveAttribute(PRIVATE_MUSIC.player.queueId, companionId);

	revoked = true;
	await refresh.click();
	await expect(page.getByText(PRIVATE_MUSIC.library.forbidden, { exact: true })).toBeVisible();
	await expect(rows).toHaveCount(0);
	await expect(page.getByTestId(PRIVATE_MUSIC.player.queueItem)).toHaveCount(0);
});

test('invited playback keeps the arranged queue and seek position across Library navigation', async ({ page, context, origin }) => {
	const tone = privateStagingAudio().find(({ file }) => file === 'tone-a.flac')!;
	const selected = { id: '00000000-0000-4000-8000-000000000004', title: 'Selected Tone', duration_ms: 14_000 };
	const catalogue = [
		{ id: uploadedId, title: 'First Tone', duration_ms: 14_000 },
		{ id: companionId, title: 'Second Tone', duration_ms: 14_000 },
		selected
	];
	const { library, player: contract } = PRIVATE_MUSIC;
	const grants: string[] = [];
	let mediaPath: string | undefined;
	await context.route('**/*', (route) => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
	await context.addCookies([{ name: fixtureCookie, value: 'synthetic-listener', url: origin, httpOnly: true }]);
	await page.route(`${origin}/v1/me/likes/session`, async (route) => {
		if (!isListener(await route.request().headerValue('cookie'))) return route.fulfill({ status: 401 });
		return route.fulfill({ json: { session_binding: binding } });
	});
	await page.route(`${origin}${library.api}`, async (route) => {
		if (!isListener(await route.request().headerValue('cookie'))) return route.fulfill({ status: 401 });
		return route.fulfill({ headers: { 'x-nntune-session-binding': binding }, json: catalogue });
	});
	await page.route(`${origin}${contract.plays}`, async (route) => {
		const call = route.request();
		const { track_id, session_id, play_id } = call.postDataJSON();
		grants.push(track_id);
		if (!isListener(await call.headerValue('cookie')) || call.method() !== 'POST' || track_id !== selected.id ||
			!/^[0-9a-f-]{36}$/.test(session_id) || play_id !== undefined ||
			await call.headerValue('x-nntune-browser-playback') !== '1' ||
			await call.headerValue('x-nntune-session-binding') !== binding) return route.fulfill({ status: 400 });
		mediaPath = `${contract.plays}/media/${track_id}/${session_id}/${playId}`;
		return route.fulfill({ json: {
			trackId: track_id, playbackSessionId: session_id, sessionBinding: binding, playId, durationMs: selected.duration_ms
		} });
	});
	await page.route(`${origin}${contract.plays}/media/**`, async (route) => {
		const call = route.request();
		if (!mediaPath || call.url() !== origin + mediaPath || !isListener(await call.headerValue('cookie'))) {
			return route.fulfill({ status: 404 });
		}
		return serveFlac(route, tone.body);
	});

	const rows = page.getByTestId(library.item);
	const queue = page.getByTestId(contract.queueItem);
	const active = page.locator(contract.currentQueueItem);
	const player = page.getByRole('region', { name: 'Player', exact: true });
	const audio = page.getByTestId(contract.audio);
	const seek = page.getByRole('slider', { name: 'Seek', exact: true });
	const feedback = page.getByTestId('player-seek-feedback');
	const assertQueue = async () => {
		await expect.poll(() => queue.evaluateAll((items, attr) => items.map((item) => item.getAttribute(attr)), contract.queueId))
			.toEqual([uploadedId, selected.id, companionId]);
		await expect(active).toHaveAttribute(contract.queueId, selected.id);
	};

	await page.goto(origin + library.path);
	for (const { id } of catalogue) {
		await rows.and(page.locator(`[data-id="${id}"]`)).locator(library.select).click();
	}
	await page.getByRole('button', { name: 'Move Selected Tone up', exact: true }).click();
	await page.getByRole('button', { name: 'Select Selected Tone', exact: true }).click();
	await assertQueue();
	await expect(audio).toHaveCount(0);
	await player.getByRole('button', { name: 'Play', exact: true }).click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);
	const authorizedMediaPath = mediaPath!;
	await player.getByRole('button', { name: 'Pause', exact: true }).click();
	await waitForPaused(audio, true, 5_000, 'Player did not pause');
	await expect(seek).toBeEnabled();
	await seek.press('Home');
	await expect(seek).toHaveAttribute('aria-valuetext', '0:00 of 0:14');
	await seek.press('PageUp');
	await expect(seek).toHaveAttribute('aria-valuetext', '0:01 of 0:14');
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => node.seeking)).toBe(false);

	const native = (await audio.elementHandle())!;
	const coldStartEvents = ['play', 'loadstart', 'emptied'];
	const start = await native.evaluate((node: HTMLAudioElement, events) => {
		for (const event of events) node.addEventListener(event, () => { node.dataset.playbackLifecycle = event; });
		return node.currentTime;
	}, coldStartEvents);
	expect(start).toBeCloseTo(1.4, 2);
	const assertPlayback = async (position: number, clock: string) => {
		await expect(audio).toHaveCount(1);
		await expect(audio).toHaveAttribute('src', authorizedMediaPath);
		await expect(player.getByTestId('library-now-playing')).toHaveText(selected.title);
		await expect(seek).toHaveAttribute('aria-valuetext', `${clock} of 0:14`);
		await expect.poll(() => native.evaluate((node: HTMLAudioElement) => node.seeking)).toBe(false);
		expect(await native.evaluate((node: HTMLAudioElement) => ({
			sameNode: node === document.querySelector('[data-testid="audio-player"]'),
			paused: node.paused,
			lifecycle: node.dataset.playbackLifecycle
		}))).toEqual({ sameNode: true, paused: true, lifecycle: undefined });
		await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => node.currentTime)).toBeCloseTo(position, 2);
	};

	await page.getByTestId('shell-rail').getByRole('link', { name: 'Home', exact: true }).click();
	await expect(page).toHaveURL(origin + '/home');
	await expect(page.getByRole('heading', { name: 'Home', exact: true })).toBeVisible();
	await expect(queue).toHaveCount(0);
	await assertPlayback(start, '0:01');
	await player.getByRole('button', { name: 'Forward 10 seconds', exact: true }).click();
	await expect(feedback).toHaveAttribute('aria-live', 'polite');
	await expect(feedback).toHaveText('Forward 10 seconds, now at 0:11.');
	await assertPlayback(start + 10, '0:11');

	await page.goBack();
	await expect(page).toHaveURL(origin + library.path);
	await expect(rows).toHaveCount(catalogue.length);
	await assertQueue();
	await assertPlayback(start + 10, '0:11');
	await player.getByRole('button', { name: 'Back 10 seconds', exact: true }).click();
	await expect(feedback).toHaveText('Back 10 seconds, now at 0:01.');
	await assertPlayback(start, '0:01');

	// Sample from before Play so a rewind fails even if the clock later passes the target.
	const resume = await native.evaluateHandle((node: HTMLAudioElement) => {
		const observed = { playTime: null as number | null, minimumTime: node.currentTime };
		const sample = () => { observed.minimumTime = Math.min(observed.minimumTime, node.currentTime); };
		node.addEventListener('play', () => {
			observed.playTime = node.currentTime;
			sample();
		}, { once: true });
		for (const event of ['playing', 'seeking', 'timeupdate']) node.addEventListener(event, sample);
		return observed;
	});
	await player.getByRole('button', { name: 'Play', exact: true }).click();
	await expect.poll(() => native.evaluate((node: HTMLAudioElement, target) =>
		!node.paused && node.currentTime > target + 0.2, start), {
		message: 'Native playback should advance beyond the paused seek target'
	}).toBe(true);
	const { playTime, minimumTime } = await resume.jsonValue();
	expect(playTime, 'Native Play should start at the seek target').toBeCloseTo(start, 0);
	expect(minimumTime, 'Native playback should stay at or beyond the seek target').toBeGreaterThanOrEqual(start - 0.05);
	await assertQueue();
	await expect(player.getByTestId('library-now-playing')).toHaveText(selected.title);
	await expect(audio).toHaveCount(1);
	expect(await audio.evaluate((node: HTMLAudioElement, original) => ({
		sameNode: node === original,
		source: node.currentSrc,
		error: node.error?.code ?? null
	}), native)).toEqual({ sameNode: true, source: origin + authorizedMediaPath, error: null });
	expect(grants, 'Resume should reuse the original playback grant').toEqual([selected.id]);
});
