import { expect, type Locator, type Page } from '@playwright/test';
import { AUDIO_PATH } from './fixtures/player-seek/paths.js';
import { test } from './fixtures/player-seek/test.js';

/**
 * The isolated player serves a checked-in ~16 s synthetic WAV. These specs
 * prove in Chromium that a mid-track Range seek moves the audible
 * position, that an unsatisfiable range fails honestly (an error state, never
 * a stuck spinner), and that pause -> seek -> resume holds the seek target
 * while paused and continues from it on resume.
 *
 * No stubs: real media decodes and advances, except for the injected-416 UI
 * case, which answers the audio request with the same 416 shape the fixture
 * returns for an unsatisfiable range.
 */

const MEDIA_BYTES = 256 * 1024;
const WAV_HEADER_BYTES = 44;
/** 8 kHz mono 16-bit PCM: 16 000 bytes carry one second of audio. */
const BYTES_PER_SECOND = 16_000;

/** Sample formula in the checked-in WAV: ((n * 7919 + 13) % 65536) - 32768. */
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
	return page.evaluate(async ({ path, rangeHeader }) => {
		const response = await fetch(path, {
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
	}, { path: AUDIO_PATH, rangeHeader: range });
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
 * Start the default track and wait for an unpaused element with an advanced clock.
 */
async function startPlayback(page: Page): Promise<Locator> {
	const audio = page.getByTestId('audio-player');
	await expect(audio).toBeVisible();
	await page.getByTestId('queue-select-first').click();

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
	expect(playbackState, 'the selected track should decode and advance').toBe('playing');
	return audio;
}

test('Range seek mid-track advances the audible position to the seek target', async ({
	page, openPlayer
}) => {
	await openPlayer();
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
	// The seeked counter is (re)armed BEFORE seeking: a bounded wait for the
	// event proves the element actually sought, so natural playback cannot
	// satisfy this pin (deleting the currentTime assignment must fail here
	// instead of riding the position poll up to the target).
	await audio.evaluate((element) => {
		const player = element as HTMLAudioElement;
		const win = window as unknown as { __seekedCount?: number };
		win.__seekedCount = 0;
		player.addEventListener('seeked', () => {
			win.__seekedCount = (win.__seekedCount ?? 0) + 1;
		});
	});
	await audio.evaluate((element, time) => {
		(element as HTMLAudioElement).currentTime = time;
	}, target);
	await expect
		.poll(
			async () =>
				audio.evaluate(
					() => (window as unknown as { __seekedCount?: number }).__seekedCount ?? 0
				),
			{ timeout: 15_000 }
		)
		.toBeGreaterThan(0);
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
	page, openPlayer
}) => {
	await openPlayer();
	const audio = page.getByTestId('audio-player');
	await expect.poll(() => audio.evaluate((element: HTMLAudioElement) =>
		element.readyState >= HTMLMediaElement.HAVE_METADATA
	)).toBe(true);

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
	const secondTrack = (url: URL) =>
		url.pathname === AUDIO_PATH && url.searchParams.get('track') === 'second';
	await page.route(secondTrack, (route) => {
		refusedRequests += 1;
		return route.fulfill({
			status: 416,
			contentType: 'text/plain',
			headers: { 'content-range': `bytes */${MEDIA_BYTES}`, 'content-length': '0' },
			body: ''
		});
	});

	const [refusal] = await Promise.all([
		page.waitForResponse((response) => secondTrack(new URL(response.url()))),
		page.getByTestId('queue-select-second').click()
	]);
	expect(refusal.status()).toBe(416);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	const status = page.getByTestId('player-status');
	await expect(status, 'a refused load must leave the loading state promptly').not.toContainText(
		'loading',
		{ timeout: 15_000 }
	);
	await expect(status).toContainText('error');
	expect(refusedRequests, 'the error state must come from refused audio loads').toBeGreaterThan(0);
	await expect.poll(async () => (await audioSnapshot(audio)).errorCode).not.toBeNull();

	const error = page.getByTestId('player-error');
	await expect(error).toBeVisible();
	await expect(error).toHaveAttribute('role', 'alert');
	await expect(error).toContainText(/couldn’t play|could not play|unable to play|playback failed/i);
	await expect(page.getByTestId('player-retry')).toBeVisible();
});

test('pause, seek while paused, and resume holds position then continues from the target', async ({
	page, openPlayer
}) => {
	await openPlayer();
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
	// pre-pause position and not from the start. The floor listener is armed
	// BEFORE resuming: every timeupdate sample plus every position-poll
	// sample from here on must stay at or above target-0.5, so a restart
	// from zero (or a return to the pre-pause position) trips the floor long
	// before the position poll could pass vacuously.
	await audio.evaluate((element) => {
		const player = element as HTMLAudioElement;
		const win = window as unknown as { __resumeMin?: number };
		win.__resumeMin = Number.POSITIVE_INFINITY;
		player.addEventListener('timeupdate', () => {
			win.__resumeMin = Math.min(
				win.__resumeMin ?? Number.POSITIVE_INFINITY,
				player.currentTime
			);
		});
	});
	await audio.evaluate((element) => {
		void (element as HTMLAudioElement).play();
	});
	await expect(page.getByTestId('player-status')).toContainText('playing', { timeout: 10_000 });
	let pollMin = Number.POSITIVE_INFINITY;
	await expect
		.poll(
			async () => {
				const now = (await audioSnapshot(audio)).currentTime;
				pollMin = Math.min(pollMin, now);
				return now;
			},
			{ timeout: 10_000 }
		)
		.toBeGreaterThan(target + 0.2);
	const eventMin = await audio.evaluate(
		() => (window as unknown as { __resumeMin?: number }).__resumeMin ?? Number.NaN
	);
	expect(
		Math.min(eventMin, pollMin),
		'resume must continue from the seek target, never dipping below it'
	).toBeGreaterThanOrEqual(target - 0.5);
	const resumed = await audioSnapshot(audio);
	expect(resumed.paused).toBe(false);
	expect(resumed.errorCode).toBeNull();
});
