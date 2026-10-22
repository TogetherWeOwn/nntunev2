import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Browser pins for Range seeking on the deterministic demo audio route.
 *
 * The demo track is a ~16 s synthetic WAV served by `/api/demo/audio`
 * (`apps/web/src/lib/player/demoMedia.ts` generator,
 * `apps/web/src/routes/api/demo/audio/+server.ts` range parser). These specs
 * prove, in a real Chromium, that a mid-track Range seek moves the audible
 * position, that an unsatisfiable range fails honestly (an error state, never
 * a stuck spinner), and that pause -> seek -> resume holds the seek target
 * while paused and continues from it on resume.
 *
 * No stubs: real media decodes and advances, except for the injected-416 UI
 * case, which answers the audio route with the same 416 shape the server
 * returns for an unsatisfiable range.
 */

const MEDIA_BYTES = 256 * 1024;
const WAV_HEADER_BYTES = 44;
/** 8 kHz mono 16-bit PCM: 16 000 bytes carry one second of audio. */
const BYTES_PER_SECOND = 16_000;

/** Sample formula pinned by the demo generator: ((n * 7919 + 13) % 65536) - 32768. */
function expectedSampleValue(sample: number): number {
	return ((sample * 7919 + 13) % 65_536) - 32_768;
}

interface RangeProbe {
	status: number;
	contentRange: string | null;
	length: number;
	firstInt16: number | null;
}

/**
 * Issue a real Range request from inside the browser and report what came back.
 * `no-store` bypasses the HTTP cache: without it, the element's buffered full
 * body would satisfy the probe locally and a Range-ignoring server would pass.
 */
async function probeRange(page: Page, range: string): Promise<RangeProbe> {
	return page.evaluate(async (rangeHeader) => {
		const response = await fetch('/api/demo/audio', {
			headers: { range: rangeHeader },
			cache: 'no-store'
		});
		const buffer = await response.arrayBuffer();
		return {
			status: response.status,
			contentRange: response.headers.get('content-range'),
			length: buffer.byteLength,
			firstInt16: buffer.byteLength >= 2 ? new DataView(buffer).getInt16(0, true) : null
		};
	}, range);
}

async function audioSnapshot(audio: Locator) {
	return audio.evaluate((element) => {
		const player = element as HTMLAudioElement;
		return {
			currentTime: player.currentTime,
			duration: player.duration,
			paused: player.paused,
			ended: player.ended,
			readyState: player.readyState,
			errorCode: player.error ? player.error.code : null
		};
	});
}

/**
 * Open the demo player, start the default track, and wait until the element
 * is genuinely playing (unpaused with an advanced clock), mirroring the
 * "demo audio plays and advances" contract.
 */
async function startPlayback(page: Page): Promise<Locator> {
	await page.goto('/demo/player');
	await page.waitForFunction(
		() => typeof (window as unknown as { __seekStorm?: unknown }).__seekStorm === 'function'
	);

	const audio = page.getByTestId('demo-audio');
	await expect(audio).toBeVisible();
	await page.getByTestId('queue-select-demo-track').click();

	let playbackState = '';
	await expect
		.poll(
			async () => {
				playbackState = await audio.evaluate((element) => {
					const player = element as HTMLAudioElement;
					if (player.error) return `error:${player.error.code}`;
					if (!player.paused && player.currentTime > 0) return 'playing';
					return `waiting: paused=${player.paused}, currentTime=${player.currentTime}, readyState=${player.readyState}`;
				});
				return playbackState;
			},
			{ timeout: 15_000 }
		)
		.toMatch(/^(playing|error:\d+)$/);
	expect(playbackState, 'the selected demo track should decode and advance').toBe('playing');
	return audio;
}

test('Range seek mid-track advances the audible position to the seek target', async ({
	page
}) => {
	const audio = await startPlayback(page);

	const { duration } = await audioSnapshot(audio);
	expect(Number.isFinite(duration), `duration must decode to a finite value, got ${duration}`).toBe(
		true
	);
	expect(duration, 'the synthetic track must be long enough for a mid-track seek').toBeGreaterThan(4);

	// Seek to the audible position whose bytes we verify below: the byte
	// offset of the target second, aligned to a 16-bit sample boundary.
	const target = duration / 2;
	let offset = WAV_HEADER_BYTES + Math.floor(target * BYTES_PER_SECOND);
	if (offset % 2 === 1) offset -= 1;
	const end = offset + 99;
	expect(end, 'the mid-track probe must stay inside the representation').toBeLessThan(MEDIA_BYTES);

	// The server must serve the mid-track bytes the seek depends on: 206,
	// an exact Content-Range, and samples from the pinned generator formula.
	// A server that ignored the Range (200 full body) or answered the wrong
	// offset fails here, before the element even seeks.
	const probe = await probeRange(page, `bytes=${offset}-${end}`);
	expect(probe.status, 'mid-track Range request must answer 206').toBe(206);
	expect(probe.contentRange).toBe(`bytes ${offset}-${end}/${MEDIA_BYTES}`);
	expect(probe.length).toBe(100);
	expect(probe.firstInt16, 'mid-track bytes must match the pinned sample formula').toBe(
		expectedSampleValue((offset - WAV_HEADER_BYTES) / 2)
	);

	// The audible position must reach the seek target and keep advancing.
	await audio.evaluate((element, time) => {
		(element as HTMLAudioElement).currentTime = time;
	}, target);
	await expect
		.poll(async () => (await audioSnapshot(audio)).currentTime, { timeout: 15_000 })
		.toBeGreaterThan(target - 0.5);
	const seeked = await audioSnapshot(audio);
	expect(seeked.errorCode, 'seeking mid-track must not error the element').toBeNull();
	expect(Math.abs(seeked.currentTime - target)).toBeLessThan(0.75);

	const afterSeek = seeked.currentTime;
	await expect
		.poll(async () => (await audioSnapshot(audio)).currentTime, { timeout: 10_000 })
		.toBeGreaterThan(afterSeek + 0.2);
	const advancing = await audioSnapshot(audio);
	expect(advancing.paused, 'playback must continue after a mid-track seek').toBe(false);
	expect(advancing.errorCode).toBeNull();
	await expect(page.getByTestId('player-status')).toContainText('playing');
});

test('unsatisfiable Range refusal surfaces an honest error with no spinner hang', async ({
	page
}) => {
	await page.goto('/demo/player');
	await page.waitForFunction(
		() => typeof (window as unknown as { __seekStorm?: unknown }).__seekStorm === 'function'
	);

	// The wire refusal itself: beyond-the-end and end-before-start ranges
	// answer 416 with the complete length, never a hang and never a 200
	// masquerading as the requested bytes.
	for (const range of [`bytes=${MEDIA_BYTES}-`, 'bytes=200-100']) {
		const probe = await probeRange(page, range);
		expect(probe.status, `Range ${range} must be refused`).toBe(416);
		expect(probe.contentRange).toBe(`bytes */${MEDIA_BYTES}`);
		expect(probe.length, `Range ${range} must carry no body`).toBe(0);
	}

	// When every audio load is refused, the player must say so: the loading
	// state clears promptly into the generic recoverable error, not a spinner
	// that never resolves. Select a track whose bytes are not buffered yet so
	// the refusal is actually exercised (the initial track already decoded).
	let refusedRequests = 0;
	await page.route('**/api/demo/audio**', (route) => {
		refusedRequests += 1;
		void route.fulfill({
			status: 416,
			contentType: 'text/plain',
			headers: { 'content-range': `bytes */${MEDIA_BYTES}`, 'content-length': '0' },
			body: ''
		});
	});

	await page.getByTestId('queue-select-second-signal').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	const status = page.getByTestId('player-status');
	await expect(status, 'a refused load must leave the loading state promptly').not.toContainText(
		'loading',
		{ timeout: 15_000 }
	);
	await expect(status).toContainText('error');
	expect(refusedRequests, 'the error state must come from refused audio loads').toBeGreaterThan(0);

	const error = page.getByTestId('player-error');
	await expect(error).toBeVisible();
	await expect(error).toHaveAttribute('role', 'alert');
	await expect(error).toContainText(/couldn’t play|could not play|unable to play|playback failed/i);
	await expect(page.getByTestId('player-retry')).toBeVisible();
});

test('pause, seek while paused, and resume holds position then continues from the target', async ({
	page
}) => {
	const audio = await startPlayback(page);

	const started = await audioSnapshot(audio);
	expect(Number.isFinite(started.duration) && started.duration > 0).toBe(true);

	// Pause first: the transport must report paused and the clock must stop.
	await audio.evaluate((element) => {
		(element as HTMLAudioElement).pause();
	});
	await expect(page.getByTestId('player-status')).toContainText('paused', { timeout: 10_000 });
	const paused = await audioSnapshot(audio);
	expect(paused.paused).toBe(true);
	expect(paused.errorCode).toBeNull();

	// Seek while paused: the position must reach the target and hold there —
	// no drift while the element stays paused.
	const target = Math.min(started.duration - 1, Math.max(paused.currentTime + 4, 2));
	await audio.evaluate((element, time) => {
		(element as HTMLAudioElement).currentTime = time;
	}, target);
	await expect
		.poll(async () => (await audioSnapshot(audio)).currentTime, { timeout: 15_000 })
		.toBeGreaterThan(target - 0.5);
	const seeked = await audioSnapshot(audio);
	expect(seeked.paused, 'seeking must not resume a paused element').toBe(true);
	expect(Math.abs(seeked.currentTime - target)).toBeLessThan(0.75);

	await page.waitForTimeout(600);
	const held = await audioSnapshot(audio);
	expect(held.paused).toBe(true);
	expect(Math.abs(held.currentTime - target), 'position must hold while paused').toBeLessThan(0.25);
	expect(held.errorCode).toBeNull();

	// Resume: playback must continue from the seek target, not from the
	// pre-pause position and not from the start.
	await audio.evaluate((element) => {
		void (element as HTMLAudioElement).play();
	});
	await expect(page.getByTestId('player-status')).toContainText('playing', { timeout: 10_000 });
	await expect
		.poll(async () => (await audioSnapshot(audio)).currentTime, { timeout: 10_000 })
		.toBeGreaterThan(target + 0.2);
	const resumed = await audioSnapshot(audio);
	expect(resumed.paused).toBe(false);
	expect(resumed.errorCode).toBeNull();
});
