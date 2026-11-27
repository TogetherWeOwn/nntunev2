import { expect, type Page, type Request, type Route } from '@playwright/test';
import { PRIVATE_MUSIC } from '../../../e2e/helpers/private-music-journey.js';
import { fractionClickPoint, seekViaPlayerUi } from '../../../e2e/helpers/player-ui.js';
import { mockInvitedLibraryPlayback } from './fixtures/invited-library-playback.js';
import { test } from './helpers/private-music-fixture.js';

const track = { id: '00000000-0000-4000-8000-000000001103', title: 'Interrupted seek audio', duration_ms: 14_000 };
const playId = `private_browser_${'a'.repeat(22)}`;
const duration = track.duration_ms / 1000;
const sampleRate = 48_000;
const bytesPerFrame = 4; // Two 16-bit PCM channels.
const bytesPerSecond = sampleRate * bytesPerFrame;
const prefixBytes = 512 * 1024;

function silentWav() {
	const dataBytes = duration * bytesPerSecond;
	const wav = Buffer.alloc(44 + dataBytes);
	wav.write('RIFF', 0);
	wav.writeUInt32LE(wav.length - 8, 4);
	wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16); // PCM format chunk length.
	wav.writeUInt16LE(1, 20); // PCM format code.
	wav.writeUInt16LE(2, 22);
	wav.writeUInt32LE(sampleRate, 24);
	wav.writeUInt32LE(bytesPerSecond, 28);
	wav.writeUInt16LE(bytesPerFrame, 32);
	wav.writeUInt16LE(16, 34);
	wav.write('data', 36);
	wav.writeUInt32LE(dataBytes, 40);
	return wav;
}

function serveSeekWav(route: Route, body: Buffer, start: number) {
	const end = start === 0 ? prefixBytes - 1 : body.length - 1;
	return route.fulfill({
		status: 206, contentType: 'audio/wav', body: body.subarray(start, end + 1),
		headers: { 'accept-ranges': 'bytes', 'cache-control': 'no-store', 'content-range': `bytes ${start}-${end}/${body.length}` }
	});
}

for (const pauseDuringSeek of [true, false]) {
	test(`private playback settles a held seek ${pauseDuringSeek ? 'after Pause' : 'without Pause'}`, async ({ page, origin }) => {
		const { library, player } = PRIVATE_MUSIC;
		const body = silentWav();
		const release = Promise.withResolvers<void>();
		const grants: unknown[] = [];
		let seekRead: Request | undefined;
		const fixture = await mockInvitedLibraryPlayback(page, origin, {
			libraryItems: () => [track],
			playableTrackId: (id) => id === track.id ? id : undefined,
			audio: async (route) => {
				const request = route.request();
				const range = /^bytes=(\d+)-$/.exec(request.headers()['range'] ?? '');
				const start = range ? Number(range[1]) : NaN;
				if (!Number.isSafeInteger(start) || start >= body.length) return route.fulfill({ status: 416 });
				// Keep sequential prefetch pending, forcing the slider to need a distant read.
				if (start > 0 && start <= prefixBytes) return;
				if (start > prefixBytes) {
					seekRead ??= request;
					await release.promise;
				}
				return serveSeekWav(route, body, start);
			}
		});
		page.on('request', (request) => {
			if (request.url() === origin + player.plays) grants.push(request.postDataJSON());
		});
		const transport = page.getByRole('region', { name: 'Player', exact: true });
		const play = transport.getByRole('button', { name: 'Play', exact: true });
		const pause = transport.getByRole('button', { name: 'Pause', exact: true });
		const slider = transport.getByRole('slider', { name: 'Seek', exact: true });
		const status = transport.getByRole('status');
		const audio = transport.getByTestId(player.audio);
		await fixture.signIn();
		await page.goto(origin + library.path);
		await page.getByTestId(library.item).filter({ hasText: track.title }).locator(library.select).click();
		await play.click();
		await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
			!node.paused && !node.seeking && node.currentTime > 0.2)).toBe(true);
		const source = origin + fixture.mediaPath(track.id);
		await expect(audio).toHaveJSProperty('currentSrc', source);
		const native = await audio.elementHandle();
		const events = await native.evaluateHandle((node: HTMLAudioElement) => {
			const timeline: { type: string; position: number; paused: boolean; seeking: boolean; trusted: boolean }[] = [];
			for (const type of ['seeking', 'seeked', 'play', 'playing', 'pause', 'emptied', 'loadstart']) {
				node.addEventListener(type, (event) => timeline.push({ type, position: node.currentTime,
					paused: node.paused, seeking: node.seeking, trusted: event.isTrusted }));
			}
			return timeline;
		});
		let target: number;
		try {
			expect(seekRead, 'A distant range should come from the seek gesture').toBeUndefined();
			const { surface, fraction } = await seekViaPlayerUi(page, audio, 0.8);
			expect(surface).toBe('custom');
			await expect.poll(() => seekRead?.url()).toBe(source);
			const pending = await native.evaluate((node: HTMLAudioElement) => ({
				position: node.currentTime, seeking: node.seeking, paused: node.paused,
				bufferedEnd: node.buffered.end(node.buffered.length - 1)
			}));
			expect(pending).toMatchObject({ seeking: true, paused: false });
			expect(pending.position).toBeGreaterThan(pending.bufferedEnd);
			expect(Math.abs(pending.position - duration * fraction)).toBeLessThan(duration * 0.05);
			target = pending.position;
			await expect(slider).toHaveAttribute('aria-valuetext', /^Seeking to /);
			if (pauseDuringSeek) {
				await pause.click();
				await expect.poll(() => events.jsonValue()).toContainEqual(expect.objectContaining({
					type: 'pause', paused: true, seeking: true, trusted: true
				}));
				await expect(play).toBeEnabled();
			}
			await expect(audio).toHaveJSProperty('seeking', true);
			expect((await events.jsonValue()).filter(({ type }) => type === 'seeked')).toEqual([]);
		} finally {
			release.resolve();
		}
		expect((await seekRead!.response())?.status()).toBe(206);
		await expect.poll(async () => (await events.jsonValue()).filter(({ type }) => type === 'seeked')).toEqual([
			{ type: 'seeked', position: expect.closeTo(target, 1), paused: pauseDuringSeek, seeking: false, trusted: true }
		]);
		await expect(slider).not.toHaveAttribute('aria-valuetext', /^Seeking to /);
		if (pauseDuringSeek) {
			await expect(audio).toHaveJSProperty('paused', true);
			await expect(status).toHaveText('Paused');
			expect(await native.evaluate((node: HTMLAudioElement) => node.currentTime)).toBeCloseTo(target, 2);
			expect(Number(await slider.inputValue())).toBeCloseTo(target, 2);
			await expect(play).toBeEnabled();
			expect((await events.jsonValue()).filter(({ type }) => type === 'play' || type === 'playing')).toEqual([]);
			await play.click();
		}
		await expect.poll(() => native.evaluate((node: HTMLAudioElement, target) =>
			!node.paused && !node.seeking && !node.ended && !node.error && node.currentTime > target + 0.2, target)).toBe(true);
		await expect(status).toHaveText('Playing');
		const timeline = await events.jsonValue();
		expect(timeline.filter(({ type }) => ['seeking', 'seeked', 'emptied', 'loadstart'].includes(type))
			.map(({ type }) => type)).toEqual(['seeking', 'seeked']);
		const plays = timeline.filter(({ type }) => type === 'play');
		if (pauseDuringSeek) {
			expect(plays).toEqual([{ type: 'play', position: expect.any(Number), paused: false, seeking: false, trusted: true }]);
			// The queued Play event can arrive once the decoder has already advanced.
			expect(plays[0].position).toBeGreaterThanOrEqual(target - 0.05);
		} else {
			expect(plays).toEqual([]);
		}
		await expect(audio).toHaveJSProperty('currentSrc', source);
		expect(await audio.evaluate((node, original) => node === original, native)).toBe(true);
		await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, track.id);
		expect(grants).toEqual([{ track_id: track.id, session_id: source.split('/').at(-2) }]);
	});
}

test('private playback retries an interrupted seek from the requested position', async ({ page, origin }) => {
	await checkInterruptedSeekRetry(page, origin);
});

test('private playback resumes at the volume selected while Retry is pending', async ({ page, origin }) => {
	await checkInterruptedSeekRetry(page, origin, { changeVolumeWhilePending: true });
});

const checkInterruptedSeekRetry = (
	page: Page, origin: string, { changeVolumeWhilePending = false } = {}
) => test.step('Recover an interrupted private seek', async () => {
	const { library, player } = PRIVATE_MUSIC;
	const playsUrl = origin + player.plays;
	const body = silentWav();
	const plays: unknown[] = [];
	const mediaUrls = new Set<string>();
	const releaseInterruption = Promise.withResolvers<void>();
	let receiptConfirmed = false;
	let interrupted: Request | undefined;
	let refused: Request | undefined;
	let recovered: Request | undefined;
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => [track],
		playableTrackId: (requested) => requested === track.id ? track.id : undefined,
		audio: async (route) => {
			const request = route.request();
			const range = /^bytes=(\d+)-$/.exec(request.headers()['range'] ?? '');
			const start = range ? Number(range[1]) : NaN;
			if (!Number.isSafeInteger(start) || start >= body.length) return route.fulfill({ status: 416 });

			// Let metadata and the opening seconds decode. Keep the sequential
			// prefetch pending so the slider has to issue a distant byte read.
			if (start > 0 && start <= prefixBytes) return;
			if (start > prefixBytes && !receiptConfirmed) {
				if (!interrupted) {
					interrupted = request;
					await releaseInterruption.promise;
					return route.abort('connectionreset');
				}
				// Chromium reissues a reset range. Keep the relay unavailable until
				// the user can exercise the existing explicit Retry control.
				refused ??= request;
				return route.fulfill({ status: 503 });
			}
			if (start > prefixBytes) recovered ??= request;
			return serveSeekWav(route, body, start);
		}
	});
	page.on('request', (request) => {
		if (request.url() === playsUrl) plays.push(request.postDataJSON());
		if (request.url().startsWith(`${playsUrl}/media/`)) mediaUrls.add(request.url());
	});

	await fixture.signIn();
	await page.goto(origin + library.path);
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);
	const status = transport.getByRole('status');
	const retry = transport.getByRole('button', { name: 'Retry playback', exact: true });
	const volume = transport.getByRole('slider', { name: /^Volume \d+%$/ });
	const currentTrack = page.locator(player.currentQueueItem);
	await page.getByTestId(library.item).filter({ hasText: track.title }).locator(library.select).click();
	await expect(audio).toHaveCount(0);
	expect(plays).toEqual([]);
	await transport.getByRole('button', { name: 'Play', exact: true }).click();
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) =>
		!node.paused && node.currentTime > 0.2)).toBe(true);
	const source = origin + fixture.mediaPath(track.id);
	const sessionId = source.split('/').at(-2);
	const admission = { track_id: track.id, session_id: sessionId };
	const continuation = { ...admission, play_id: playId };
	page.on('response', (response) => {
		if (response.url() === playsUrl && response.ok() && response.request().postDataJSON()?.play_id === playId) {
			receiptConfirmed = true;
		}
	});
	await expect(audio).toHaveJSProperty('currentSrc', source);
	const native = (await audio.elementHandle())!;
	if (changeVolumeWhilePending) {
		await volume.press('End');
		await volume.press('ArrowLeft');
		await expect(volume).toHaveValue('99');
		await expect(audio).toHaveJSProperty('volume', 0.99);
	}
	let target: number;
	try {
		const { surface, fraction } = await seekViaPlayerUi(page, audio, 0.8);
		expect(surface).toBe('custom');
		await expect.poll(() => interrupted, { message: 'Seeking should request audio beyond the opening prefix' }).toBeDefined();
		const pending = await native.evaluate((node: HTMLAudioElement) => ({
			position: node.currentTime, seeking: node.seeking, paused: node.paused,
			bufferedEnd: node.buffered.end(node.buffered.length - 1)
		}));
		expect(pending).toMatchObject({ seeking: true, paused: false });
		expect(pending.position).toBeGreaterThan(pending.bufferedEnd);
		expect(Math.abs(pending.position - duration * fraction)).toBeLessThan(duration * 0.05);
		target = pending.position;
	} finally {
		releaseInterruption.resolve();
	}
	await expect.poll(() => interrupted?.failure()?.errorText).toBe('net::ERR_CONNECTION_RESET');
	await expect(retry).toBeEnabled();
	await expect(status).toContainText('Retry playback');
	expect(await (await refused?.response())?.status()).toBe(503);
	expect(refused?.headers()['range']).toBe(interrupted?.headers()['range']);
	await expect(currentTrack).toHaveAttribute(player.queueId, track.id);
	expect(plays).toEqual([admission]);

	const resumed = await native.evaluateHandle((node: HTMLAudioElement) => {
		const observation = { position: null as number | null, trusted: false, volume: node.volume };
		node.addEventListener('playing', (event) => {
			observation.position = node.currentTime;
			observation.trusted = event.isTrusted;
			observation.volume = node.volume;
		}, { once: true });
		return observation;
	});
	const holdReceipt = Promise.withResolvers<void>();
	if (changeVolumeWhilePending) {
		// Gate the exact continuation before the invited fixture validates and answers it.
		await page.route(playsUrl, async (route) => {
			if (route.request().postDataJSON()?.play_id === playId) await holdReceipt.promise;
			await route.fallback();
		});
	}
	try {
		await retry.click();
		if (changeVolumeWhilePending) {
			await expect.poll(() => plays).toEqual([admission, continuation]);
			await volume.press('ArrowLeft');
			await expect(volume).toHaveValue('98');
			await expect(volume).toHaveAccessibleName('Volume 98%');
			expect(await native.evaluate((node: HTMLAudioElement) => ({
				failed: node.error !== null, volume: node.volume
			}))).toEqual({ failed: true, volume: 0.98 });
			expect(receiptConfirmed).toBe(false);
		}
	} finally {
		holdReceipt.resolve();
	}
	await expect(status).toHaveText('Playing');
	await expect.poll(() => native.evaluate((node: HTMLAudioElement, observation) => {
		const advancing = observation.position !== null && !node.paused && !node.seeking && node.error === null &&
			node.currentTime > observation.position + 0.2;
		return advancing ? { volume: node.volume } : null;
	}, resumed)).toEqual({ volume: changeVolumeWhilePending ? 0.98 : expect.any(Number) });
	const firstPlaying = await resumed.jsonValue();
	expect(firstPlaying.trusted).toBe(true);
	expect(firstPlaying.position).not.toBeNull();
	expect(Math.abs(firstPlaying.position! - target)).toBeLessThan(0.1);
	if (changeVolumeWhilePending) {
		expect(firstPlaying.volume).toBe(0.98);
		await expect(volume).toHaveValue('98');
		await expect(volume).toHaveAccessibleName('Volume 98%');
	}
	await expect(retry).toHaveCount(0);
	await expect(currentTrack).toHaveAttribute(player.queueId, track.id);
	expect(await audio.evaluate((node, original) => node === original, native)).toBe(true);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	expect(await (await recovered?.response())?.status()).toBe(206);
	expect(recovered?.headers()['range']).toBe(interrupted?.headers()['range']);
	expect(mediaUrls).toEqual(new Set([source]));
	expect(plays, 'Retry should confirm the original receipt without another admission').toEqual([admission, continuation]);
});

test('a delayed private range settles the latest seek on the same track', async ({ page, origin }) => {
	const { library, player } = PRIVATE_MUSIC;
	const next = { ...track, id: '00000000-0000-4000-8000-000000001104', title: 'Upcoming audio' };
	const tracks = [track, next];
	const body = silentWav();
	const releaseFirstRead = Promise.withResolvers<void>();
	const releaseLaterReads = Promise.withResolvers<void>();
	let firstSeekRead: Request | undefined;
	const grants: unknown[] = [];
	const fixture = await mockInvitedLibraryPlayback(page, origin, {
		libraryItems: () => tracks,
		playableTrackId: (id) => tracks.find((item) => item.id === id)?.id,
		audio: async (route) => {
			const request = route.request();
			const range = /^bytes=(\d+)-$/.exec(request.headers()['range'] ?? '');
			const start = range ? Number(range[1]) : NaN;
			if (!Number.isSafeInteger(start) || start >= body.length) return route.fulfill({ status: 416 });
			// Leave the opening prefetch pending so the first seek needs a distant range.
			if (start > 0 && start <= prefixBytes) return;
			if (start > prefixBytes) {
				firstSeekRead ??= request;
				await (request === firstSeekRead ? releaseFirstRead : releaseLaterReads).promise;
			}
			const end = start === 0 ? prefixBytes - 1 : body.length - 1;
			return route.fulfill({
				status: 206, contentType: 'audio/wav', body: body.subarray(start, end + 1),
				headers: { 'accept-ranges': 'bytes', 'cache-control': 'no-store', 'content-range': `bytes ${start}-${end}/${body.length}` }
			});
		}
	});
	page.on('request', (request) => {
		if (request.url() === origin + player.plays) grants.push(request.postDataJSON());
	});

	await fixture.signIn();
	await page.goto(origin + library.path);
	const transport = page.getByRole('region', { name: 'Player', exact: true });
	const audio = transport.getByTestId(player.audio);
	const seek = transport.getByRole('slider', { name: 'Seek', exact: true });
	const queueIds = () => page.getByTestId(player.queueItem)
		.evaluateAll((items, attribute) => items.map((item) => item.getAttribute(attribute)), player.queueId);
	for (const item of tracks) {
		await page.getByTestId(library.item).filter({ hasText: item.title }).locator(library.select).click();
	}
	await expect.poll(queueIds).toEqual(tracks.map(({ id }) => id));
	const [grant] = await Promise.all([
		page.waitForResponse(origin + player.plays),
		transport.getByRole('button', { name: 'Play', exact: true }).click()
	]);
	expect(grant.ok()).toBe(true);
	const source = origin + fixture.mediaPath(track.id);
	await expect(audio).toHaveJSProperty('currentSrc', source);
	await expect.poll(() => audio.evaluate((node: HTMLAudioElement) => !node.paused && node.currentTime > 0.2)).toBe(true);
	await transport.getByRole('button', { name: 'Pause', exact: true }).click();
	await expect(audio).toHaveJSProperty('paused', true);
	await expect(seek).toBeEnabled();
	const native = await audio.elementHandle();
	const completions = await native.evaluateHandle((node: HTMLAudioElement) => {
		const observed: { position: number; trusted: boolean }[] = [];
		node.addEventListener('seeked', (event) => {
			observed.push({ position: node.currentTime, trusted: event.isTrusted });
		});
		return observed;
	});

	const requests = await seek.evaluateHandle((slider: HTMLInputElement) => {
		const positions: number[] = [];
		// Read the browser's request before PlayerSeek restores the published value.
		slider.addEventListener('input', (event) => {
			if (event.isTrusted) positions.push(slider.valueAsNumber);
		}, { capture: true });
		return positions;
	});
	const sliderPosition = () => seek.evaluate((slider: HTMLInputElement) => slider.valueAsNumber);
	try {
		await seek.scrollIntoViewIfNeeded();
		const point = fractionClickPoint((await seek.boundingBox())!, 0.82);
		await page.mouse.move(point.x, point.y);
		expect(firstSeekRead, 'The distant range should start with the first seek').toBeUndefined();
		await page.mouse.down();
		let firstPosition: number;
		try {
			await expect.poll(() => firstSeekRead?.url(), { message: 'The first seek should need a distant range' }).toBe(source);
			await expect(seek).toHaveAttribute('aria-valuetext', 'Seeking to 0:11 of 0:14');
			firstPosition = await sliderPosition();
			// A small move changes the requested position within the same pending range.
			await page.mouse.move(point.x + 2, point.y);
		} finally {
			await page.mouse.up();
		}
		const requested = await requests.jsonValue();
		expect(requested).toHaveLength(2);
		const [firstTarget, latestTarget] = requested;
		expect(firstPosition).toBeCloseTo(firstTarget, 2);
		expect(latestTarget, 'The second input should request a distinct position').toBeGreaterThan(firstTarget + 0.02);
		await expect(audio).toHaveJSProperty('seeking', true);
		await expect(seek).toHaveAttribute('aria-valuetext', 'Seeking to 0:11 of 0:14');
		expect(await sliderPosition(), 'The pending slider should publish the second request').toBeCloseTo(latestTarget, 2);
		expect(await native.evaluate((node: HTMLAudioElement) => node.currentTime)).toBeCloseTo(latestTarget, 2);
		expect(await completions.jsonValue()).toEqual([]);

		// Only the read started by the first gesture may supply bytes for this settlement.
		releaseFirstRead.resolve();
		expect((await firstSeekRead!.response())?.status()).toBe(206);
		await expect(audio).toHaveJSProperty('seeking', false);
		await expect(seek).toHaveAttribute('aria-valuetext', '0:11 of 0:14');
		await expect.poll(() => completions.jsonValue()).toEqual([
			{ position: expect.closeTo(latestTarget, 2), trusted: true }
		]);
		expect(await sliderPosition()).toBeCloseTo(latestTarget, 2);
	} finally {
		releaseFirstRead.resolve();
		releaseLaterReads.resolve();
	}
	expect(await audio.evaluate((node: HTMLAudioElement, original) => ({
		sameNode: node === original, source: node.currentSrc, paused: node.paused, error: node.error?.code ?? null
	}), native)).toEqual({ sameNode: true, source, paused: true, error: null });
	await expect(page.locator(player.currentQueueItem)).toHaveAttribute(player.queueId, track.id);
	await expect(transport.getByTestId('library-now-playing')).toHaveText(track.title);
	expect(await queueIds()).toEqual(tracks.map(({ id }) => id));
	expect(grants).toEqual([{ track_id: track.id, session_id: source.split('/').at(-2) }]);
});
