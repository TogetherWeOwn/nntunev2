import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned pins around the protected `player-restart.spec.ts` suite.
 * KEYSAFETY contract: native select elements own their keys, so `0` from a
 * focused SELECT must not restart, and the player must not suppress the
 * select's native type-ahead (the 0.5x/0.75x speed options start with 0).
 * Same pending-media harness as the acceptance suite.
 */

type MediaControl = {
	playCalls: number;
	loadCalls: number;
	resolve: () => void;
	reject: () => void;
};

/** Demo player with synthetic pending media: only fixture events settle playback. */
async function openPlayer(page: Page) {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
		const control: MediaControl = {
			playCalls: 0,
			loadCalls: 0,
			resolve: () => {},
			reject: () => {}
		};
		Object.assign(window, { __restartBuilderLifecycle: control });
		HTMLMediaElement.prototype.play = function (): Promise<void> {
			control.playCalls += 1;
			return new Promise<void>((resolve, reject) => {
				control.resolve = resolve;
				control.reject = () => reject(new Error('private decoder details'));
			});
		};
		HTMLMediaElement.prototype.pause = function () {};
		const rawLoad = HTMLMediaElement.prototype.load;
		HTMLMediaElement.prototype.load = function () {
			control.loadCalls += 1;
			return rawLoad.call(this);
		};
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function mockMedia(page: Page, init: { currentTime: number; duration: number }) {
	await page.getByTestId('demo-audio').evaluate(
		(el: HTMLAudioElement, seed: { currentTime: number; duration: number }) => {
			let time = seed.currentTime;
			Object.defineProperty(el, 'duration', { value: seed.duration, configurable: true });
			Object.defineProperty(el, 'currentTime', {
				get: () => time,
				set: (next: number) => {
					time = next;
				},
				configurable: true
			});
		},
		init
	);
}

async function mediaTime(page: Page): Promise<number> {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.currentTime);
}

async function mediaRate(page: Page): Promise<number> {
	return page.getByTestId('demo-audio').evaluate((el: HTMLAudioElement) => el.playbackRate);
}

test('0 from the focused speed select stays native: no restart, type-ahead owns the key', async ({
	page
}) => {
	await openPlayer(page);
	await mockMedia(page, { currentTime: 45, duration: 120 });
	const speed = page.getByTestId('playback-speed');
	await expect(speed).toHaveValue('1');
	expect(await mediaRate(page)).toBe(1);
	await speed.focus();
	await expect(speed).toBeFocused();

	await page.keyboard.press('0');

	// The restart handler is synchronous, so a missing guard would already
	// have zeroed the clock: still 45 proves the shortcut stayed inert.
	// Native type-ahead wins instead: first `0*` option (0.5x) selects and
	// applies, proving the player suppressed nothing.
	expect(await mediaTime(page)).toBe(45);
	await expect(speed).toHaveValue('0.5');
	await expect.poll(() => mediaRate(page)).toBe(0.5);
});
