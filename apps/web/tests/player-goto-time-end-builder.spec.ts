import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned regression pin: an explicit seek to the exact end while
 * transport is active must be made intentional before the native end events
 * arrive, rather than looking like a completed track and advancing the queue.
 */

type EndSeekProbe = {
	pauseCalls: number;
	endEventsDispatched: boolean;
	resolve: () => void;
};

declare global {
	interface Window {
		__gotoEndSeek: EndSeekProbe;
	}
}

async function openPlayer(page: Page) {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
		const probe: EndSeekProbe = { pauseCalls: 0, endEventsDispatched: false, resolve: () => {} };
		window.__gotoEndSeek = probe;
		HTMLMediaElement.prototype.play = function () {
			return new Promise<void>((resolve) => {
				probe.resolve = resolve;
			});
		};
		HTMLMediaElement.prototype.pause = function () {
			probe.pauseCalls += 1;
		};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('player-status')).toHaveText('Status: selected');
}

async function mockMediaThatEndsAtDuration(page: Page) {
	await page.getByTestId('demo-audio').evaluate((node) => {
		const audio = node as HTMLAudioElement;
		let currentTime = 30;
		Object.defineProperty(audio, 'duration', { configurable: true, value: 120 });
		Object.defineProperty(audio, 'currentTime', {
			configurable: true,
			get: () => currentTime,
			set: (next: number) => {
				currentTime = next;
				if (next !== 120) return;
				queueMicrotask(() => {
					Object.defineProperty(audio, 'ended', { configurable: true, value: true });
					window.__gotoEndSeek.endEventsDispatched = true;
					audio.dispatchEvent(new Event('pause'));
					audio.dispatchEvent(new Event('ended'));
				});
			}
		});
		audio.dispatchEvent(new Event('loadedmetadata'));
		audio.dispatchEvent(new Event('timeupdate'));
	});
}

async function beginPlaying(page: Page) {
	await page.keyboard.press('k');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await page.evaluate(() => window.__gotoEndSeek.resolve());
	await page.getByTestId('demo-audio').evaluate((audio) => {
		Object.defineProperty(audio, 'paused', { configurable: true, value: false });
		audio.dispatchEvent(new Event('playing'));
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
}

test('exact end timestamp pauses active transport before native completion can advance the queue', async ({ page }) => {
	await openPlayer(page);
	await mockMediaThatEndsAtDuration(page);
	await beginPlaying(page);

	const input = page.getByTestId('player-goto-time');
	await input.fill('2:00');
	await input.press('Enter');

	await expect.poll(() => page.evaluate(() => window.__gotoEndSeek.endEventsDispatched)).toBe(true);
	await expect.poll(() => page.evaluate(() => window.__gotoEndSeek.pauseCalls)).toBe(1);
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('player-goto-status')).toContainText(/seeked to 120 seconds/i);
});
