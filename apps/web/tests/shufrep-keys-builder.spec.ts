import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned regression pins complementing the protected shufrep-keys.spec.ts.
 * Keep that acceptance bank unchanged; this supplement is safe to extend.
 *
 * Removing shuffleQueueUpcoming's radio guard must fail with an actionable
 * upcoming pair. Excluding shuffle from the native-select/IME guard must fail
 * after ONE event, before a second deterministic shuffle could undo it.
 *
 * Run: pnpm --dir apps/web exec playwright test tests/shufrep-keys-builder.spec.ts --workers=1
 * Uses real built demo code; only media I/O and randomness are doubled. This
 * is synthetic browser coverage, not evidence of physical IME or streaming.
 */

const INITIAL = ['demo-track', 'second-signal', 'third-light'];
const SWAPPED = ['demo-track', 'third-light', 'second-signal'];
const RADIO_ORDER = ['third-light', 'demo-track', 'second-signal'];
const RADIO_SWAPPED = ['third-light', 'second-signal', 'demo-track'];

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page.getByTestId('queue-item')
		.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}

async function focusBody(page: Page): Promise<void> {
	await page.evaluate(() => {
		if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
	});
	expect(await page.evaluate(() => document.activeElement === document.body)).toBe(true);
}

async function settle(page: Page): Promise<void> {
	await page.evaluate(() => new Promise<void>((resolve) =>
		requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}

async function openPlayer(page: Page, baseURL: string): Promise<void> {
	const origin = new URL(baseURL);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(origin.hostname);
	await page.route('**/*', (route) => {
		const target = new URL(route.request().url());
		if (target.origin !== origin.origin) return route.abort();
		if (target.pathname === '/api/demo/audio') return;
		if (target.pathname.startsWith('/api/')) return route.abort();
		return route.continue();
	});
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
		HTMLMediaElement.prototype.play = function () {
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
		Math.random = () => 0;
	});
	await page.goto('/demo/player?radio-seed=7');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect.poll(() => queueOrder(page)).toEqual(INITIAL);
	await focusBody(page);
	// Positive controls prove hydration and both bindings before testing no-ops.
	await page.keyboard.press('s');
	await expect.poll(() => queueOrder(page)).toEqual(SWAPPED);
	await page.keyboard.press('s');
	await expect.poll(() => queueOrder(page)).toEqual(INITIAL);
	await page.keyboard.press('r');
	await expect(page.getByTestId('player-repeat')).toHaveValue('one');
	await page.getByTestId('player-repeat').selectOption('off');
	await expect(page.getByTestId('player-repeat')).toHaveValue('off');
	await focusBody(page);
}

test('radio blocks a single shuffle even with two upcoming queue entries', async ({ page, baseURL }) => {
	await openPlayer(page, baseURL!);
	await page.getByTestId('queue-select-third-light').click();
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
	await expect(page.getByTestId('radio-badge')).toBeVisible();
	// Moving the selected row preserves radio and selection, but restores a
	// two-track suffix. The fewer-than-two guard can no longer mask radio.
	await page.getByTestId('queue-move-up-third-light').click();
	await expect.poll(() => queueOrder(page)).toEqual(['demo-track', 'third-light', 'second-signal']);
	await page.getByTestId('queue-move-up-third-light').click();
	await expect.poll(() => queueOrder(page)).toEqual(RADIO_ORDER);
	await expect(page.getByTestId('queue-select-third-light')).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('radio-badge')).toBeVisible();
	await focusBody(page);

	await page.keyboard.press('s');
	await settle(page);
	expect(await queueOrder(page)).toEqual(RADIO_ORDER);
	await expect(page.getByTestId('radio-badge')).toBeVisible();
	await expect(page.getByTestId('queue-select-third-light')).toHaveAttribute('aria-current', 'true');

	// The same suffix really is shufflable once radio releases the guard.
	await page.getByTestId('radio-stop').click();
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await focusBody(page);
	await page.keyboard.press('s');
	await expect.poll(() => queueOrder(page)).toEqual(RADIO_SWAPPED);
});

for (const key of ['s', 'S', 'r', 'R']) {
	test(`native select keeps a single ${key} without changing queue or repeat`, async ({ page, baseURL }) => {
		await openPlayer(page, baseURL!);
		const speed = page.getByTestId('playback-speed');
		const speedValue = await speed.inputValue();
		await speed.focus();
		await expect(speed).toBeFocused();

		await page.keyboard.press(key);
		await settle(page);
		expect(await queueOrder(page)).toEqual(INITIAL);
		await expect(page.getByTestId('player-repeat')).toHaveValue('off');
		await expect(page.getByTestId('repeat-status')).toHaveText('Repeat: Off');
		await expect(speed).toHaveValue(speedValue);
		await expect(speed).toBeFocused();
	});
}

for (const [flag, init] of [
	['isComposing', { isComposing: true }],
	['keyCode 229', { keyCode: 229 }]
] as const) {
	for (const key of ['s', 'S', 'r', 'R']) {
		test(`${flag} keeps a single ${key} without changing queue or repeat`, async ({ page, baseURL }) => {
			await openPlayer(page, baseURL!);
			await page.evaluate((event) => {
				window.dispatchEvent(new KeyboardEvent('keydown', { ...event, bubbles: true }));
			}, { key, ...init });
			await settle(page);
			expect(await queueOrder(page)).toEqual(INITIAL);
			await expect(page.getByTestId('player-repeat')).toHaveValue('off');
			await expect(page.getByTestId('repeat-status')).toHaveText('Repeat: Off');
		});
	}
}
