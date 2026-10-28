import { expect, test, type Page } from '@playwright/test';

/**
 * Test-Author owned RED acceptance for rate truth on `/demo/player`: the
 * actual media rate, the speed control and the persisted preference must
 * agree, including a restored 1.3, a native-control change, and a rejected
 * request. The Builder wires the page to satisfy this file and must not
 * edit it. Stable case IDs: RATETRUTH-B01..B10.
 *
 * LOCKED PAGE CONTRACT (extends the comfort suite's hooks; all on
 * `/demo/player`, existing hooks unchanged):
 * - `playback-speed`: the speed select. It keeps the standard preset options
 *   (at least 0.5, 1, 1.5, 2) and, when the effective rate is a supported
 *   non-preset such as a restored or native 1.3, its value is that exact rate
 *   with a visible label naming it (rather than a blank unmatched select).
 *   The stored `nntune:playback-speed:v1` value is the last verified actual
 *   rate, never an unapplied request.
 * - A `ratechange` event on the current keyed audio element reconciles the
 *   speed state, the displayed selection and best-effort persistence to the
 *   element's verified rate. Events from detached or pre-swap elements are
 *   ignored. Reconciliation never writes back into the element (no
 *   assignment/event feedback loop) and never implies play or seek.
 * - When the media rejects a requested rate (setter no-op, clamp, or throw),
 *   the control keeps the last accepted rate everywhere (audio, selection,
 *   storage), raises no uncaught exception, and the `comforts-panel` names a
 *   concise failure mentioning the request did not apply (it must match
 *   /not applied|not set|couldn|failed|rejected/i; recommended phrasing:
 *   "Speed 1.5x not applied — still 1x"). Non-finite native readings keep
 *   the last accepted rate and never reach storage.
 * - Applying the preference on source swap/mount and operating with blocked
 *   site storage keep working in memory. Rate handling preserves queue,
 *   time, playback intent, Media Session publishing and repeat/radio/sleep
 *   accounting.
 */

const SPEED_KEY = 'nntune:playback-speed:v1';

/** Keep media loading synthetic and pending; only fixture events settle playback. */
async function openPlayer(
	page: Page,
	options: { seedSpeed?: string; blockStorage?: boolean } = {}
) {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(
		({ seed, block }: { seed: string | null; block: boolean }) => {
			if (block) {
				Object.defineProperty(window, 'localStorage', {
					configurable: true,
					get() {
						throw new DOMException('Storage access blocked', 'SecurityError');
					}
				});
			} else {
				if (seed !== null) localStorage.setItem('nntune:playback-speed:v1', seed);
				localStorage.removeItem('nntune:resume:v1');
			}
			try {
				sessionStorage.removeItem('nntune:demo-queue:v1');
			} catch {
				// A fresh context starts empty; nothing to clear.
			}
			HTMLMediaElement.prototype.play = function () {
				return new Promise<void>(() => {});
			};
			HTMLMediaElement.prototype.pause = function () {};
		},
		{ seed: options.seedSpeed ?? null, block: options.blockStorage ?? false }
	);
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function playbackRate(page: Page) {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.playbackRate);
}

async function storedSpeed(page: Page) {
	return page.evaluate((key: string) => localStorage.getItem(key), SPEED_KEY);
}

async function selectedSpeedValue(page: Page) {
	return page.getByTestId('playback-speed').evaluate((el: HTMLSelectElement) => el.value);
}

async function selectedSpeedText(page: Page) {
	return page
		.getByTestId('playback-speed')
		.evaluate((el: HTMLSelectElement) => el.selectedOptions[0]?.textContent ?? '');
}

async function speedOptionValues(page: Page) {
	return page
		.getByTestId('playback-speed')
		.evaluate((el: HTMLSelectElement) => Array.from(el.options).map((option) => option.value));
}

async function playerStatus(page: Page) {
	return (await page.getByTestId('player-status').textContent()) ?? '';
}

async function currentTime(page: Page) {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.currentTime);
}

/** Simulate a native-control rate change on the current element. */
async function nativeRateChange(page: Page, rate: number) {
	await page
		.getByTestId('demo-audio')
		.evaluate((el: HTMLAudioElement, next: number) => {
			el.playbackRate = next;
			el.dispatchEvent(new Event('ratechange'));
		}, rate);
}

/** Counts subsequent playbackRate assignments on the current element. */
async function countRateWrites(page: Page) {
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		const holder = el as HTMLAudioElement & { __rateWrites?: number };
		holder.__rateWrites = 0;
		let backing = el.playbackRate;
		Object.defineProperty(el, 'playbackRate', {
			configurable: true,
			get: () => backing,
			set: (value: number) => {
				holder.__rateWrites = (holder.__rateWrites ?? 0) + 1;
				backing = value;
			}
		});
	});
	return () =>
		page
			.getByTestId('demo-audio')
			.evaluate((el: HTMLAudioElement & { __rateWrites?: number }) => el.__rateWrites ?? -1);
}

/** Make the current element silently ignore rate writes (rejection). */
async function rejectRateWrites(page: Page) {
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		const frozen = el.playbackRate;
		Object.defineProperty(el, 'playbackRate', {
			configurable: true,
			get: () => frozen,
			set: (_value: number) => {
				// The request is rejected: the actual rate never moves.
			}
		});
	});
}

/** Make the current element throw on every rate write. */
async function throwOnRateWrite(page: Page) {
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		const frozen = el.playbackRate;
		Object.defineProperty(el, 'playbackRate', {
			configurable: true,
			get: () => frozen,
			set: (_value: number) => {
				throw new Error('rate setter unavailable');
			}
		});
	});
}

test('[RATETRUTH-B01] restored 1.3 shows a truthful selection and applies to audio', async ({
	page
}) => {
	await openPlayer(page, { seedSpeed: '1.3' });
	const speed = page.getByTestId('playback-speed');
	await expect(speed).toBeVisible();

	// Presets are retained (control).
	const options = await speedOptionValues(page);
	for (const preset of ['0.5', '1', '1.5', '2']) expect(options).toContain(preset);

	// Truth: the effective rate is selected explicitly, never blank or snapped.
	await expect(speed).toHaveValue('1.3');
	expect(await selectedSpeedText(page)).toContain('1.3');
	expect(await selectedSpeedValue(page)).not.toBe('');
	await expect.poll(() => playbackRate(page)).toBe(1.3);
	expect(await storedSpeed(page)).toBe('1.3');

	await page.reload();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('playback-speed')).toHaveValue('1.3');
	await expect.poll(() => playbackRate(page)).toBe(1.3);
	expect(await storedSpeed(page)).toBe('1.3');
});

test('[RATETRUTH-B02] native ratechange reconciles control and storage without bouncing back', async ({
	page
}) => {
	await openPlayer(page);
	const statusBefore = await playerStatus(page);
	const timeBefore = await currentTime(page);

	// Set the native rate first so the write counter only sees handler writes.
	await page
		.getByTestId('demo-audio')
		.evaluate((el: HTMLAudioElement) => void (el.playbackRate = 1.3));
	const readWrites = await countRateWrites(page);
	await page
		.getByTestId('demo-audio')
		.evaluate((el: HTMLAudioElement) => void el.dispatchEvent(new Event('ratechange')));

	// The handler must not write back into the element (no feedback loop).
	await page.waitForTimeout(300);
	expect(await readWrites()).toBe(0);

	// Truth: state, display and persistence follow the verified native rate.
	await expect(page.getByTestId('playback-speed')).toHaveValue('1.3');
	expect(await selectedSpeedText(page)).toContain('1.3');
	expect(await storedSpeed(page)).toBe('1.3');
	await expect.poll(() => playbackRate(page)).toBe(1.3);

	// A rate observation never implies play or seek.
	expect(await playerStatus(page)).toBe(statusBefore);
	expect(await currentTime(page)).toBe(timeBefore);
	expect(await playerStatus(page)).not.toContain('playing');
});

test('[RATETRUTH-B03] detached and stale ratechange events are ignored', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('playback-speed').selectOption('1.5');
	await expect.poll(() => playbackRate(page)).toBe(1.5);
	expect(await storedSpeed(page)).toBe('1.5');

	// A detached element's event must not move state.
	await page.evaluate(() => {
		const ghost = document.createElement('audio');
		ghost.playbackRate = 1.75;
		ghost.dispatchEvent(new Event('ratechange'));
	});
	await page.waitForTimeout(200);
	expect(await selectedSpeedValue(page)).toBe('1.5');
	expect(await storedSpeed(page)).toBe('1.5');
	await expect.poll(() => playbackRate(page)).toBe(1.5);

	// A stale event from the pre-swap element must not move state either: the
	// demo audio node is keyed per source, so this handle detaches on next.
	const oldAudio = await page.getByTestId('demo-audio').elementHandle();
	if (!oldAudio) throw new Error('harness: demo-audio handle missing');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect.poll(() => playbackRate(page)).toBe(1.5);
	await oldAudio.evaluate((node) => {
		const el = node as HTMLAudioElement;
		el.playbackRate = 1.75;
		el.dispatchEvent(new Event('ratechange'));
	});
	await page.waitForTimeout(200);
	expect(await selectedSpeedValue(page)).toBe('1.5');
	expect(await storedSpeed(page)).toBe('1.5');
	expect(await storedSpeed(page)).not.toBe('1.75');
	await expect.poll(() => playbackRate(page)).toBe(1.5);
});

test('[RATETRUTH-B04] a media-rejected speed request keeps the last accepted rate everywhere', async ({
	page
}) => {
	const pageerrors: Error[] = [];
	page.on('pageerror', (error) => pageerrors.push(error));
	await openPlayer(page);
	const statusBefore = await playerStatus(page);
	expect(await selectedSpeedValue(page)).toBe('1');
	await rejectRateWrites(page);

	await page.getByTestId('playback-speed').selectOption('1.5');
	await page.waitForTimeout(200);

	// Truth: nothing accepted the request, so nothing claims it.
	await expect.poll(() => playbackRate(page)).toBe(1);
	expect(await selectedSpeedValue(page)).toBe('1');
	expect(await selectedSpeedText(page)).toContain('1x');
	expect(await storedSpeed(page)).not.toBe('1.5');
	expect(pageerrors).toHaveLength(0);

	// A failed request never implies play or seek.
	expect(await playerStatus(page)).toBe(statusBefore);
	expect(await currentTime(page)).toBe(0);
});

test('[RATETRUTH-B05] a rejected speed request announces concise failure feedback', async ({
	page
}) => {
	await openPlayer(page);
	await rejectRateWrites(page);

	await page.getByTestId('playback-speed').selectOption('1.5');
	await expect(page.getByTestId('comforts-panel')).toContainText(
		/not applied|not set|couldn|failed|rejected/i
	);
});

test('[RATETRUTH-B06] a throwing media setter is contained without losing state', async ({
	page
}) => {
	const pageerrors: Error[] = [];
	page.on('pageerror', (error) => pageerrors.push(error));
	await openPlayer(page);
	await throwOnRateWrite(page);

	await page.getByTestId('playback-speed').selectOption('1.5');
	await page.waitForTimeout(200);

	expect(pageerrors).toHaveLength(0);
	expect(await selectedSpeedValue(page)).toBe('1');
	await expect.poll(() => playbackRate(page)).toBe(1);
	expect(await storedSpeed(page)).not.toBe('1.5');
});

test('[RATETRUTH-B07] a source swap keeps the non-preset preference truthful', async ({ page }) => {
	await openPlayer(page, { seedSpeed: '1.3' });
	await expect.poll(() => playbackRate(page)).toBe(1.3);
	expect(await storedSpeed(page)).toBe('1.3');

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect.poll(() => playbackRate(page)).toBe(1.3);
	await expect(page.getByTestId('playback-speed')).toHaveValue('1.3');
	expect(await selectedSpeedText(page)).toContain('1.3');
	expect(await storedSpeed(page)).toBe('1.3');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
});

test('[RATETRUTH-B08] blocked storage still reconciles a native change in memory', async ({
	page
}) => {
	const pageerrors: Error[] = [];
	page.on('pageerror', (error) => pageerrors.push(error));
	await openPlayer(page, { blockStorage: true });

	await nativeRateChange(page, 1.3);
	await page.waitForTimeout(200);
	await expect.poll(() => playbackRate(page)).toBe(1.3);
	await expect(page.getByTestId('playback-speed')).toHaveValue('1.3');
	expect(await selectedSpeedText(page)).toContain('1.3');
	expect(pageerrors).toHaveLength(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
});

test('[RATETRUTH-B09] preset speed applies to audio and survives reload', async ({ page }) => {
	await openPlayer(page);
	const speed = page.getByTestId('playback-speed');
	await speed.selectOption('1.5');
	await expect.poll(() => playbackRate(page)).toBe(1.5);
	expect(await storedSpeed(page)).toBe('1.5');
	await expect(speed).toHaveValue('1.5');

	await page.reload();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('playback-speed')).toHaveValue('1.5');
	await expect.poll(() => playbackRate(page)).toBe(1.5);
});

test('[RATETRUTH-B10] rate changes never imply play, seek, or accounting changes', async ({
	page
}) => {
	await openPlayer(page);
	const statusBefore = await playerStatus(page);
	expect(await currentTime(page)).toBe(0);
	await expect(page.getByTestId('sleep-timer-status')).toContainText('Sleep timer off');
	await expect(page.getByTestId('repeat-status')).toContainText('Repeat: Off');

	await page.getByTestId('playback-speed').selectOption('1.5');
	await expect.poll(() => playbackRate(page)).toBe(1.5);
	expect(await playerStatus(page)).toBe(statusBefore);
	expect(await currentTime(page)).toBe(0);

	await nativeRateChange(page, 1.25);
	await page.waitForTimeout(200);
	expect(await playerStatus(page)).toBe(statusBefore);
	expect(await currentTime(page)).toBe(0);
	await expect(page.getByTestId('sleep-timer-status')).toContainText('Sleep timer off');
	await expect(page.getByTestId('repeat-status')).toContainText('Repeat: Off');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
});
