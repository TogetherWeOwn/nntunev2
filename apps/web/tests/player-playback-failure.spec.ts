import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance contract: demo playback failures are visible and recoverable.
 *
 * The element's `play()` is stubbed before page scripts run, so every
 * outcome (resolve / reject / stay pending) is controlled by the test and
 * no real media decodes. Lifecycle transitions are driven by dispatching
 * the same `playing` / `pause` / `error` events a browser would fire.
 *
 * DOM contract these tests define for the implementation:
 * - `player-status` names the truthful state (e.g. loading / playing /
 *   paused / error). Selecting a track alone never claims `playing`, and
 *   the current queue item carries no `(playing)` marker until playback
 *   actually starts.
 * - A rejected play or a media `error` for the current source shows
 *   `player-error` with `role="alert"` and generic text (never the raw
 *   error name/message), containing a `player-retry` button.
 * - Retry is explicit only: nothing re-calls `play()` until the user
 *   activates retry (or navigates). Queue navigation and clearing stay
 *   usable after a failure, and stale outcomes/events from a superseded
 *   or cleared source never overwrite the current state.
 */

const RAW_ERROR_SENTINEL = 'RAW_BOOM_SENTINEL_7f3a';

type PlayMode = 'resolve' | 'reject' | 'pending';

interface PlayControl {
	mode: PlayMode;
	calls: (string | null)[];
	pending: { resolve: () => void; reject: (reason: Error) => void }[];
}

async function openPlayer(page: Page, mode: PlayMode): Promise<void> {
	await page.addInitScript(
		({ initialMode, sentinel }) => {
			const control: PlayControl = { mode: initialMode, calls: [], pending: [] };
			(window as unknown as { __playControl: PlayControl }).__playControl = control;
			HTMLMediaElement.prototype.play = function play() {
				control.calls.push(this.getAttribute('src'));
				if (control.mode === 'reject') {
					const error = new Error(sentinel);
					error.name = 'NotSupportedError';
					return Promise.reject(error);
				}
				if (control.mode === 'pending') {
					return new Promise<void>((resolve, reject) => {
						control.pending.push({ resolve, reject: (reason) => reject(reason) });
					});
				}
				return Promise.resolve();
			};
			HTMLMediaElement.prototype.pause = function pause() {};
		},
		{ initialMode: mode, sentinel: RAW_ERROR_SENTINEL }
	);
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function setPlayMode(page: Page, mode: PlayMode): Promise<void> {
	await page.evaluate((next) => {
		(window as unknown as { __playControl: PlayControl }).__playControl.mode = next;
	}, mode);
}

async function playCallCount(page: Page): Promise<number> {
	return page.evaluate(
		() => (window as unknown as { __playControl: PlayControl }).__playControl.calls.length
	);
}

async function fireAudioEvent(page: Page, type: 'playing' | 'pause' | 'error'): Promise<void> {
	await page.getByTestId('demo-audio').evaluate((el, eventType) => {
		el.dispatchEvent(new Event(eventType));
	}, type);
}

test('selecting a track does not claim playing until playback actually starts', async ({ page }) => {
	await openPlayer(page, 'pending');

	await page.getByTestId('queue-select-second-signal').click();
	await expect.poll(() => playCallCount(page)).toBe(1);

	// The play attempt is still in flight: selection is not playback.
	await expect(page.getByTestId('player-status')).not.toContainText(/playing/i);
	await expect(
		page.locator('[data-testid="queue-item"][data-track-id="second-signal"]')
	).not.toContainText('(playing)');

	// Playback starts: the promise settles and the element reports playing.
	await page.evaluate(() => {
		const control = (window as unknown as { __playControl: PlayControl }).__playControl;
		control.pending.shift()?.resolve();
	});
	await fireAudioEvent(page, 'playing');

	await expect(page.getByTestId('player-status')).toContainText(/playing/i);
	await expect(
		page.locator('[data-testid="queue-item"][data-track-id="second-signal"]')
	).toContainText('(playing)');

	// A pause event must drop the playing claim again.
	await fireAudioEvent(page, 'pause');
	await expect(page.getByTestId('player-status')).toContainText(/paused/i);
	await expect(page.getByTestId('player-status')).not.toContainText(/playing/i);
	await expect(
		page.locator('[data-testid="queue-item"][data-track-id="second-signal"]')
	).not.toContainText('(playing)');
});

test('a rejected play shows a generic accessible error with an explicit retry that can succeed', async ({
	page
}) => {
	await openPlayer(page, 'reject');

	await page.getByTestId('queue-select-demo-track').click();
	await expect.poll(() => playCallCount(page)).toBe(1);

	const error = page.getByTestId('player-error');
	await expect(error).toBeVisible();
	await expect(error).toHaveAttribute('role', 'alert');
	await expect(error).toContainText(/couldn’t play|could not play|unable to play|playback failed/i);
	// Generic copy only: the raw rejection must never reach the user.
	await expect(error).not.toContainText(RAW_ERROR_SENTINEL);
	await expect(error).not.toContainText('NotSupportedError');
	await expect(page.getByTestId('player-status')).not.toContainText(/playing/i);

	// No automatic retry loop: the call count stays put without user action.
	await page.waitForTimeout(300);
	expect(await playCallCount(page)).toBe(1);

	// Explicit retry, now succeeding.
	await setPlayMode(page, 'resolve');
	const retry = page.getByTestId('player-retry');
	await expect(retry).toBeVisible();
	await expect(retry).toHaveAccessibleName(/retry/i);
	await retry.click();
	await expect.poll(() => playCallCount(page)).toBe(2);
	await fireAudioEvent(page, 'playing');

	await expect(error).toHaveCount(0);
	await expect(page.getByTestId('player-status')).toContainText(/playing/i);
});

test('a media error for the current source shows the same recoverable error', async ({ page }) => {
	await openPlayer(page, 'resolve');

	await page.getByTestId('queue-select-demo-track').click();
	await fireAudioEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toContainText(/playing/i);

	await fireAudioEvent(page, 'error');

	const error = page.getByTestId('player-error');
	await expect(error).toBeVisible();
	await expect(error).toHaveAttribute('role', 'alert');
	await expect(page.getByTestId('player-retry')).toBeVisible();
	await expect(page.getByTestId('player-status')).not.toContainText(/playing/i);
});

test('queue navigation and clearing remain usable after a failure', async ({ page }) => {
	await openPlayer(page, 'reject');

	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-error')).toBeVisible();

	// Next/previous still navigate; the new attempt replaces the old state.
	await setPlayMode(page, 'resolve');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await fireAudioEvent(page, 'playing');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
	await expect(page.getByTestId('player-status')).toContainText(/playing/i);

	await page.getByTestId('player-prev').click();
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');

	// Clearing works and leaves no error or playing claim behind.
	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
	await expect(page.getByTestId('player-status')).not.toContainText(/playing/i);

	// Re-adding after the failure starts cleanly.
	await page.getByTestId('queue-add-third-light').click();
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
});

test('a delayed rejection from a superseded track does not flag an error on the new track', async ({
	page
}) => {
	await openPlayer(page, 'pending');

	await page.getByTestId('queue-select-demo-track').click();
	await expect.poll(() => playCallCount(page)).toBe(1);

	// Navigate before the first attempt settles; the new track plays.
	await setPlayMode(page, 'resolve');
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await fireAudioEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toContainText(/playing/i);

	// The superseded attempt rejects late: current state must not change.
	await page.evaluate((sentinel) => {
		const control = (window as unknown as { __playControl: PlayControl }).__playControl;
		const error = new Error(sentinel);
		error.name = 'AbortError';
		control.pending.shift()?.reject(error);
	}, RAW_ERROR_SENTINEL);
	await page.waitForTimeout(200);

	await expect(page.getByTestId('player-error')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('player-status')).toContainText(/playing/i);
});

test('stale media events after clearing the queue do not resurrect playback state', async ({
	page
}) => {
	await openPlayer(page, 'resolve');

	await page.getByTestId('queue-select-demo-track').click();
	await fireAudioEvent(page, 'playing');
	await expect(page.getByTestId('player-status')).toContainText(/playing/i);

	await page.getByTestId('queue-clear').click();
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');

	// Late events from the released source arrive after the clear.
	await fireAudioEvent(page, 'playing');
	await fireAudioEvent(page, 'error');
	await page.waitForTimeout(200);

	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
	await expect(page.getByTestId('player-status')).not.toContainText(/playing/i);
});
