import { expect, test, type Page } from '@playwright/test';

/**
 * Back/Forward 10 seconds on the native `/library` player. Logic lives in
 * `src/lib/player/relativeJump.ts` (unit-tested); this checks the buttons
 * drive the mounted audio element and announce politely.
 */

test.setTimeout(30_000);

const ITEMS = [{ id: 'rj-coral', title: 'Coral Night', artist: 'Mira', streamUrl: '/test-audio/rj-coral' }];

const audio = (page: Page) => page.getByTestId('audio-player');
const feedback = (page: Page) => page.getByTestId('player-seek-feedback');

async function openWithTrack(page: Page) {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(ITEMS) })
	);
	await page.route('**/test-audio/**', () => {});
	await page.addInitScript(() => {
		const calls = { play: 0, pause: 0 };
		(window as unknown as { __rjCalls: typeof calls }).__rjCalls = calls;
		HTMLMediaElement.prototype.play = function () {
			calls.play += 1;
			return new Promise<void>(() => {});
		};
		HTMLMediaElement.prototype.pause = function () {
			calls.pause += 1;
		};
	});
	await page.goto('/library');
	await page
		.locator('[data-testid="library-item"][data-id="rj-coral"]')
		.getByRole('button', { name: /^Coral Night( — |$)/ })
		.click();
	await expect(audio(page)).toHaveCount(1);
}

async function mockMedia(page: Page, time: number, duration: number, seekableEnd = duration) {
	await audio(page).evaluate(
		(el: HTMLAudioElement, s: { time: number; duration: number; seekableEnd: number }) => {
			let current = s.time;
			Object.defineProperty(el, 'duration', { value: s.duration, configurable: true });
			Object.defineProperty(el, 'seekable', {
				value: { length: 1, start: () => 0, end: () => s.seekableEnd },
				configurable: true
			});
			Object.defineProperty(el, 'currentTime', {
				get: () => current,
				set: (next: number) => {
					current = next;
				},
				configurable: true
			});
		},
		{ time, duration, seekableEnd }
	);
}

const position = (page: Page) => audio(page).evaluate((el: HTMLAudioElement) => el.currentTime);

test('RJ-01 buttons jump the native player and announce politely', async ({ page }) => {
	await openWithTrack(page);
	await expect(feedback(page)).toHaveAttribute('aria-live', 'polite');
	await mockMedia(page, 30, 120);
	const calls = await page.evaluate(() => ({ ...(window as unknown as { __rjCalls: object }).__rjCalls }));

	await page.getByRole('button', { name: 'Forward 10 seconds' }).click();
	expect(await position(page)).toBe(40);
	await expect(feedback(page)).toHaveText('Forward 10 seconds, now at 0:40.');

	await page.getByRole('button', { name: 'Back 10 seconds' }).click();
	expect(await position(page)).toBe(30);
	await expect(feedback(page)).toHaveText('Back 10 seconds, now at 0:30.');

	// The gesture never plays or pauses.
	expect(await page.evaluate(() => ({ ...(window as unknown as { __rjCalls: object }).__rjCalls }))).toEqual(calls);
});

test('RJ-02 clamps at the start and reports unseekable media without writing', async ({ page }) => {
	await openWithTrack(page);
	await mockMedia(page, 4, 120);
	await page.getByRole('button', { name: 'Back 10 seconds' }).click();
	expect(await position(page)).toBe(0);
	await expect(feedback(page)).toHaveText('Back to the start, 0:00.');
	await page.getByRole('button', { name: 'Back 10 seconds' }).click();
	await expect(feedback(page)).toHaveText('Already at the start.');

	await mockMedia(page, 5, Number.NaN);
	await page.getByRole('button', { name: 'Forward 10 seconds' }).click();
	expect(await position(page)).toBe(5);
	await expect(feedback(page)).toHaveText('Cannot jump: this track cannot seek yet.');

	// A source served without range support reports only [0, 0] as seekable.
	await mockMedia(page, 0, 120, 0);
	await page.getByRole('button', { name: 'Forward 10 seconds' }).click();
	expect(await position(page)).toBe(0);
	await expect(feedback(page)).toHaveText('Cannot jump: this track cannot seek yet.');
});
