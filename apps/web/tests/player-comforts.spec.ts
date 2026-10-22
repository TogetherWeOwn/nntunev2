import { expect, test, type Page } from '@playwright/test';

/**
 * Test-Author owned RED acceptance for the player comfort bundle (sleep
 * timer, playback speed, per-track resume, share timestamp). The Builder
 * wires `/demo/player` to satisfy this file and must not edit it.
 *
 * LOCKED PAGE CONTRACT (all hooks on `/demo/player`):
 * - `comforts-panel`: the comfort controls section.
 * - `sleep-timer-minutes`: duration input (spinbutton) in minutes.
 * - `sleep-timer-set-duration`: arms a duration timer from that input.
 * - `sleep-timer-set-end-of-track`: arms an end-of-track timer.
 * - `sleep-timer-tracks`: stop-after-N input (spinbutton) in tracks.
 * - `sleep-timer-set-stop-after`: arms a stop-after-N timer from that input.
 * - `sleep-timer-cancel`: cancels the armed timer; disabled when disarmed.
 * - `sleep-timer-status`: aria-live region announcing the timer state. The
 *   armed announcement names the mode: the minutes for duration, "end of
 *   track" for end-of-track, the count for stop-after-N.
 * - `playback-speed`: a select offering at least the values 0.5, 1, 1.5, 2.
 *   Changing it applies `audio.playbackRate` and persists localStorage
 *   `nntune:playback-speed:v1`; a reload restores both.
 * - `window.__comforts.advanceSleep(ms)`: drives the REAL armed duration
 *   timer forward by `ms` (same tick path as the live clock), so duration
 *   paths stay deterministic. Modelled on the `__seekStorm` page hook.
 * - Resume: localStorage `nntune:resume:v1` holds `{"<trackId>": seconds}`.
 *   Selecting a track seeks to its saved position; switching tracks persists
 *   the previous track's latest position; a track end clears that track.
 * - Share: `/demo/player?track=<id>&t=<seconds>` selects the track and seeks
 *   to `t` (plain seconds; clock forms per the unit contract). Unknown
 *   tracks fall back to the default queue; unparsable `t` starts at 0.
 * - Firing a timer pauses: status becomes paused, the queue keeps all three
 *   tracks, and the current track does not advance.
 */

const SPEED_KEY = 'nntune:playback-speed:v1';
const RESUME_KEY = 'nntune:resume:v1';

type ComfortsHook = { advanceSleep: (ms: number) => void };

async function advanceSleep(page: Page, ms: number) {
	await page.evaluate((delta) => {
		const hook = (window as unknown as { __comforts?: ComfortsHook }).__comforts;
		if (!hook) throw new Error('comforts hook missing');
		hook.advanceSleep(delta);
	}, ms);
}

/** Keep media loading synthetic and pending; only fixture events settle playback. */
async function openPlayer(page: Page, options: { clearComfortStorage?: boolean } = {}) {
	const { clearComfortStorage = true } = options;
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript((clear: boolean) => {
		const control = { resolve: () => {}, reject: () => {} };
		Object.assign(window, { __comfortLifecycle: control });
		HTMLMediaElement.prototype.play = function () {
			return new Promise<void>((resolve, reject) => {
				control.resolve = resolve;
				control.reject = () => reject(new Error('private decoder details'));
			});
		};
		HTMLMediaElement.prototype.pause = function () {};
		if (clear) {
			sessionStorage.removeItem('nntune:demo-queue:v1');
			localStorage.removeItem('nntune:playback-speed:v1');
			localStorage.removeItem('nntune:resume:v1');
		}
	}, clearComfortStorage);
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function settle(page: Page, mode: 'resolve' | 'reject') {
	await page.evaluate((next) => {
		const control = (
			window as unknown as { __comfortLifecycle: { resolve: () => void; reject: () => void } }
		).__comfortLifecycle;
		control[next]();
	}, mode);
}

async function startPlaying(page: Page) {
	await page.getByTestId('queue-select-demo-track').click();
	await settle(page, 'resolve');
	await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('playing')));
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
}

async function naturalEnd(page: Page) {
	await page.getByTestId('demo-audio').evaluate((el) => {
		// Browser end-of-media steps set both flags, then fire pause before ended.
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
}

async function currentTime(page: Page) {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.currentTime);
}

async function playbackRate(page: Page) {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.playbackRate);
}

async function storedValue(page: Page, key: string) {
	return page.evaluate((storageKey) => localStorage.getItem(storageKey), key);
}

test('duration timer pauses at the deadline without clearing the queue', async ({ page }) => {
	await openPlayer(page);
	await startPlaying(page);

	await page.getByTestId('sleep-timer-minutes').fill('15');
	await page.getByTestId('sleep-timer-set-duration').click();
	await expect(page.getByTestId('sleep-timer-status')).toContainText(/sleep/i);
	await expect(page.getByTestId('sleep-timer-status')).toContainText('15');
	await expect(page.getByTestId('sleep-timer-cancel')).toBeEnabled();

	// Before the deadline the timer is silent.
	await advanceSleep(page,14 * 60_000);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	await advanceSleep(page,61_000);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('sleep-timer-status')).toContainText(/sleep/i);
	await expect(page.getByTestId('sleep-timer-cancel')).toBeDisabled();
});

test('end-of-track timer pauses on ended without advancing', async ({ page }) => {
	await openPlayer(page);
	await startPlaying(page);

	await page.getByTestId('sleep-timer-set-end-of-track').click();
	await expect(page.getByTestId('sleep-timer-status')).toContainText(/end of (the )?track/i);

	await naturalEnd(page);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('sleep-timer-cancel')).toBeDisabled();
});

test('stop-after-2 advances once, then pauses on the second end', async ({ page }) => {
	await openPlayer(page);
	await startPlaying(page);

	await page.getByTestId('sleep-timer-tracks').fill('2');
	await page.getByTestId('sleep-timer-set-stop-after').click();
	await expect(page.getByTestId('sleep-timer-status')).toContainText('2');

	await naturalEnd(page);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await settle(page, 'resolve');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	await naturalEnd(page);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
});

test('cancel disarms the timer so deadlines and ends pass silently', async ({ page }) => {
	await openPlayer(page);
	await startPlaying(page);

	await page.getByTestId('sleep-timer-minutes').fill('15');
	await page.getByTestId('sleep-timer-set-duration').click();
	await expect(page.getByTestId('sleep-timer-cancel')).toBeEnabled();
	await page.getByTestId('sleep-timer-cancel').click();
	await expect(page.getByTestId('sleep-timer-cancel')).toBeDisabled();

	await advanceSleep(page,16 * 60_000);
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');

	// Track modes disarm too: the end advances normally after a cancel.
	await page.getByTestId('sleep-timer-set-end-of-track').click();
	await page.getByTestId('sleep-timer-cancel').click();
	await naturalEnd(page);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
});

test('speed applies to the audio element and survives reload', async ({ page }) => {
	await openPlayer(page, { clearComfortStorage: false });

	const speed = page.getByTestId('playback-speed');
	await expect(speed).toBeVisible();
	await speed.selectOption('1.5');
	await expect.poll(() => playbackRate(page)).toBe(1.5);
	expect(await storedValue(page, SPEED_KEY)).toBe('1.5');

	await page.reload();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('playback-speed')).toHaveValue('1.5');
	await expect.poll(() => playbackRate(page)).toBe(1.5);

	await page.getByTestId('playback-speed').selectOption('0.5');
	await expect.poll(() => playbackRate(page)).toBe(0.5);
	expect(await storedValue(page, SPEED_KEY)).toBe('0.5');
});

test('speed stays applied after navigating to the next track', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('playback-speed').selectOption('1.5');
	await expect.poll(() => playbackRate(page)).toBe(1.5);

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('playback-speed')).toHaveValue('1.5');
	await expect.poll(() => playbackRate(page)).toBe(1.5);
	expect(await storedValue(page, SPEED_KEY)).toBe('1.5');
});

test('resume restores on reselect and clears at track end', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-second-signal').click();
	await settle(page, 'resolve');

	// A saved position applies when the track is (re)selected.
	await page.evaluate(() =>
		localStorage.setItem('nntune:resume:v1', '{"second-signal":42.5}')
	);
	await page.getByTestId('queue-select-second-signal').click();
	await expect.poll(() => currentTime(page)).toBe(42.5);

	// Switching tracks persists the previous position; reselecting restores it.
	await page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => {
		el.currentTime = 30;
		el.dispatchEvent(new Event('timeupdate'));
	});
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	const saved = JSON.parse((await storedValue(page, RESUME_KEY)) ?? '{}') as Record<string, number>;
	expect(saved['second-signal']).toBeGreaterThanOrEqual(29);
	expect(saved['second-signal']).toBeLessThanOrEqual(31);
	await page.getByTestId('queue-select-second-signal').click();
	await expect.poll(() => currentTime(page)).toBeGreaterThanOrEqual(29);

	// A track end clears that track, so it restarts next time.
	await naturalEnd(page);
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await page.getByTestId('queue-select-second-signal').click();
	await expect.poll(() => currentTime(page)).toBe(0);
	const afterEnd = JSON.parse((await storedValue(page, RESUME_KEY)) ?? '{}') as Record<string, number>;
	expect(afterEnd['second-signal']).toBeUndefined();
});

test('share link resolves the track and seeks to t', async ({ page }) => {
	await page.route('**/api/demo/audio**', () => {});
	await page.goto('/demo/player?track=second-signal&t=90');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect.poll(() => currentTime(page)).toBe(90);

	await page.goto('/demo/player?track=third-light&t=1:30');
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await expect.poll(() => currentTime(page)).toBe(90);
});

test('share link restores its track into an empty saved queue and seeks to t', async ({ page }) => {
	// Preserve session storage across document navigations in this case.
	await openPlayer(page, { clearComfortStorage: false });
	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await page.reload();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	await page.goto('/demo/player?track=second-signal&t=90');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect.poll(() => currentTime(page)).toBe(90);
});

test('share link with invalid params falls back safely', async ({ page }) => {
	await page.route('**/api/demo/audio**', () => {});
	await page.goto('/demo/player?track=no-such-track&t=90');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect.poll(() => currentTime(page)).toBe(0);

	await page.goto('/demo/player?track=third-light&t=soon');
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await expect.poll(() => currentTime(page)).toBe(0);
});

test('comfort controls are keyboard-reachable with visible focus and operable', async ({
	page
}) => {
	await openPlayer(page);

	const controls = [
		'sleep-timer-minutes',
		'sleep-timer-set-duration',
		'sleep-timer-set-end-of-track',
		'sleep-timer-tracks',
		'sleep-timer-set-stop-after',
		'sleep-timer-cancel',
		'playback-speed'
	];
	// The cancel button is disabled (unfocusable) until a timer is armed.
	await page.getByTestId('sleep-timer-minutes').fill('15');
	await page.getByTestId('sleep-timer-set-duration').click();
	await expect(page.getByTestId('sleep-timer-cancel')).toBeEnabled();

	const seen = new Set<string>();
	for (let tab = 0; tab < 60 && seen.size < controls.length; tab += 1) {
		await page.keyboard.press('Tab');
		const focused = await page.evaluate(() => {
			const el = document.activeElement as HTMLElement | null;
			if (!el) return null;
			const style = window.getComputedStyle(el);
			return {
				testid: el.getAttribute('data-testid'),
				focusVisible: el.matches(':focus-visible'),
				outlineWidth: style.outlineWidth,
				boxShadow: style.boxShadow
			};
		});
		if (focused?.testid && controls.includes(focused.testid)) {
			seen.add(focused.testid);
			expect(focused.focusVisible, `${focused.testid} must match :focus-visible`).toBe(true);
			const visible =
				focused.outlineWidth !== '0px' && focused.outlineWidth !== '0'
					? true
					: focused.boxShadow !== 'none';
			expect(visible, `${focused.testid} must show a visible focus indicator`).toBe(true);
		}
	}
	expect([...seen].sort(), 'every comfort control must be reachable by Tab').toEqual(
		[...controls].sort()
	);

	// Keyboard operation: Enter arms, arrows change speed.
	await page.getByTestId('sleep-timer-cancel').click();
	await page.getByTestId('sleep-timer-set-end-of-track').focus();
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('sleep-timer-status')).toContainText(/end of (the )?track/i);

	await page.getByTestId('playback-speed').focus();
	const before = await page.getByTestId('playback-speed').inputValue();
	await page.keyboard.press('ArrowDown');
	const afterDown = await page.getByTestId('playback-speed').inputValue();
	if (afterDown === before) await page.keyboard.press('ArrowUp');
	const after = await page.getByTestId('playback-speed').inputValue();
	expect(after, 'arrow keys must change the speed select').not.toBe(before);
	await expect.poll(() => playbackRate(page)).toBe(Number(after));
});

test('existing player hooks and queue semantics are unchanged', async ({ page }) => {
	await openPlayer(page);

	await expect(page.getByTestId('player-status')).toContainText('Status:');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(page.getByTestId('demo-audio')).toHaveAttribute('src', '/api/demo/audio');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('player-next')).toBeVisible();
	await expect(page.getByTestId('player-prev')).toBeVisible();
	await expect(page.getByTestId('queue-clear')).toBeVisible();

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await page.getByTestId('player-prev').click();
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
});
