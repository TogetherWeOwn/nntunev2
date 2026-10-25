import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned regression pin: an explicit seek to the exact end while
 * transport is active must be made intentional before the native end events
 * arrive, rather than looking like a completed track and advancing the queue.
 */

type EndSeekProbe = {
	playCalls: number;
	pauseCalls: number;
	endEventsDispatched: boolean;
	resolve: () => void;
	reject: () => void;
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
		const probe: EndSeekProbe = {
			playCalls: 0,
			pauseCalls: 0,
			endEventsDispatched: false,
			resolve: () => {},
			reject: () => {}
		};
		window.__gotoEndSeek = probe;
		HTMLMediaElement.prototype.play = function () {
			probe.playCalls += 1;
			return new Promise<void>((resolve, reject) => {
				probe.resolve = resolve;
				probe.reject = () => reject(new DOMException('Playback was interrupted', 'AbortError'));
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

async function mockMediaThatEndsAtDuration(page: Page, queueEndEvents = true) {
	await page.getByTestId('demo-audio').evaluate((node, queueEndEvents) => {
		const audio = node as HTMLAudioElement;
		let currentTime = 30;
		Object.defineProperty(audio, 'duration', { configurable: true, value: 120 });
		Object.defineProperty(audio, 'currentTime', {
			configurable: true,
			get: () => currentTime,
			set: (next: number) => {
				currentTime = next;
				if (next !== 120 || !queueEndEvents) return;
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
	}, queueEndEvents);
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

for (const settlement of ['resolve', 'reject'] as const) {
	test(`exact end timestamp cancels pending playback before late ${settlement}`, async ({ page }) => {
		await openPlayer(page);
		// Hold native completion until after promise settlement: otherwise its
		// stop events could invalidate the play and hide a missing cancellation.
		await mockMediaThatEndsAtDuration(page, false);
		const queueBefore = await page.getByTestId('queue-item').evaluateAll((rows) =>
			rows.map((row) => row.getAttribute('data-track-id'))
		);
		const sourceBefore = await page.getByTestId('demo-audio').getAttribute('src');

		await page.keyboard.press('k');
		await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
		await expect.poll(() => page.evaluate(() => window.__gotoEndSeek.playCalls)).toBe(1);

		await page.getByTestId('player-goto-time').fill('2:00');
		await page.getByTestId('player-goto-time').press('Enter');
		await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
		await expect(page.getByTestId('demo-audio')).toHaveJSProperty('currentTime', 120);
		await expect(page.getByTestId('player-goto-status')).toContainText(/seeked to 120 seconds/i);

		await page.evaluate(async (settlement) => {
			window.__gotoEndSeek[settlement]();
			await Promise.resolve();
			await Promise.resolve();
		}, settlement);
		// Neither a stale success nor a stale AbortError may replace the pause.
		await expect(page.getByTestId('player-status')).toHaveText('Status: paused');

		await page.getByTestId('demo-audio').evaluate((node) => {
			const audio = node as HTMLAudioElement;
			Object.defineProperty(audio, 'ended', { configurable: true, value: true });
			audio.dispatchEvent(new Event('pause'));
			audio.dispatchEvent(new Event('ended'));
			audio.dispatchEvent(new Event('playing'));
		});
		await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
		await expect(page.getByTestId('now-playing')).toContainText('Demo track');
		await expect(page.getByTestId('demo-audio')).toHaveAttribute('src', sourceBefore!);
		await expect(page.getByTestId('demo-audio')).toHaveJSProperty('currentTime', 120);
		await expect.poll(() => page.getByTestId('queue-item').evaluateAll((rows) =>
			rows.map((row) => row.getAttribute('data-track-id'))
		)).toEqual(queueBefore);
		await expect.poll(() => page.evaluate(() => window.__gotoEndSeek.playCalls)).toBe(1);
		await expect.poll(() => page.evaluate(() => window.__gotoEndSeek.pauseCalls)).toBe(1);
		await expect(page.getByTestId('player-goto-status')).toContainText(/seeked to 120 seconds/i);
	});
}
