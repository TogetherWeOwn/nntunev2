import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance-owned; Builder must not edit this file or existing protected tests.
 * UI contract: native select [data-testid=player-repeat], accessible name Repeat,
 * values off/one/queue, labels Off/One/Queue. A live status
 * [data-testid=repeat-status] announces the selected mode without stealing focus.
 * Disabled select describes why repeat is unavailable while radio is active.
 * Missing control is a SOFT assertion: transport assertions still execute on
 * baseline Off, proving actual One/Queue failures rather than selector timeouts.
 * Only HTMLMediaElement/network I/O is faked; real host, queue, timer and resume
 * code run. No live streams. Stable IDs REP-Bxx map to the handoff report.
 */
declare global {
	interface Window {
		__repeatMedia: { calls: { src: string; time: number }[] };
	}
}
const ids = ['demo-track', 'second-signal', 'third-light'];
const arranged = ['third-light', 'demo-track', 'second-signal'];
const titles = { a: 'Demo track', b: 'Second Signal', c: 'Third Light' };

async function boot(page: Page) {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		window.__repeatMedia = { calls: [] };
		HTMLMediaElement.prototype.play = function () {
			window.__repeatMedia.calls.push({ src: this.getAttribute('src') ?? '', time: this.currentTime });
			Object.defineProperty(this, 'paused', { value: false, configurable: true });
			Object.defineProperty(this, 'ended', { value: false, configurable: true });
			return Promise.resolve();
		};
		HTMLMediaElement.prototype.pause = function () {};
		if (!sessionStorage.getItem('repeat-fixture-initialized')) {
			sessionStorage.clear();
			localStorage.clear();
			sessionStorage.setItem('repeat-fixture-initialized', '1');
		}
	});
	await page.goto('/demo/player?radio-seed=7');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}
async function mode(page: Page, value: 'off' | 'one' | 'queue') {
	const control = page.getByTestId('player-repeat');
	await expect.soft(control, 'Repeat control is required; continue to test actual transport').toHaveCount(1, { timeout: 300 });
	if (await control.count()) await control.selectOption(value);
}
async function order(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}
async function calls(page: Page) {
	return page.evaluate(() => window.__repeatMedia.calls.length);
}
async function position(page: Page, value: number) {
	await page.getByTestId('demo-audio').evaluate((el, time) => {
		(el as HTMLMediaElement).currentTime = time;
		el.dispatchEvent(new Event('timeupdate'));
	}, value);
}
async function playing(page: Page) {
	await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('playing')));
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
}
async function start(page: Page, id = 'demo-track') {
	await page.getByTestId(`queue-select-${id}`).click();
	await playing(page);
}
async function end(page: Page) {
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: true, configurable: true });
		el.dispatchEvent(new Event('pause'));
		el.dispatchEvent(new Event('ended'));
	});
}
async function staleEnd(page: Page) {
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: false, configurable: true });
		el.dispatchEvent(new Event('ended'));
	});
}
async function paused(page: Page) {
	await page.getByTestId('demo-audio').evaluate((el) => {
		Object.defineProperty(el, 'paused', { value: true, configurable: true });
		Object.defineProperty(el, 'ended', { value: false, configurable: true });
		el.dispatchEvent(new Event('pause'));
	});
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
}
async function replayed(page: Page, title: string, count: number) {
	await expect.soft(page.getByTestId('now-playing')).toContainText(title);
	await expect.soft.poll(() => calls(page)).toBe(count + 1);
	await expect.soft.poll(() => page.getByTestId('demo-audio').evaluate((el) => (el as HTMLMediaElement).currentTime)).toBe(0);
	await expect.soft(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect.soft(page.getByTestId('player-status')).toHaveText('Status: playing');
	const last = await page.evaluate(() => window.__repeatMedia.calls.at(-1));
	expect.soft(last?.time, 'Replay starts from zero, not a stored resume offset').toBe(0);
	await expect.soft.poll(() => page.getByTestId('demo-audio').evaluate((el) => (el as HTMLMediaElement).loop)).toBe(false);
}
async function unchanged(page: Page, title: string, count: number) {
	// Two rendering frames let synchronous handlers, promises and Svelte effects
	// settle before asserting the absence of an unwanted play request.
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
	await expect(page.getByTestId('now-playing')).toContainText(title);
	expect(await calls(page)).toBe(count);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
}

test.beforeEach(async ({ page }) => boot(page));

test('REP-B01 accessible default Off, selected announcement, focus and reload reset', async ({ page }) => {
	const control = page.getByTestId('player-repeat');
	await expect(control).toHaveAccessibleName(/repeat/i);
	await expect(control).toHaveValue('off');
	for (const value of ['one', 'queue', 'off'] as const) {
		await control.focus();
		await mode(page, value);
		await expect(control).toBeFocused();
		await expect(page.getByTestId('repeat-status')).toHaveAttribute('role', 'status');
		await expect(page.getByTestId('repeat-status')).toContainText(new RegExp(value, 'i'));
	}
	await mode(page, 'queue');
	await page.reload();
	await expect(control).toHaveValue('off');
});

test('REP-B02 One replays the middle track at zero and clears its completed resume', async ({ page }) => {
	await mode(page, 'one');
	await start(page, 'second-signal');
	await page.evaluate(() => localStorage.setItem('nntune:resume:v1', JSON.stringify({ 'second-signal': 23 })));
	await position(page, 23);
	const count = await calls(page);
	await end(page);
	await replayed(page, titles.b, count);
	expect(await order(page)).toEqual(ids);
	const saved = await page.evaluate(() => JSON.parse(localStorage.getItem('nntune:resume:v1') ?? '{}')['second-signal']);
	expect(saved === undefined || saved === 0).toBe(true);
});

test('REP-B03 Queue follows the edited order then wraps without radio', async ({ page }) => {
	await page.getByTestId('queue-move-up-third-light').click();
	await page.getByTestId('queue-move-up-third-light').click();
	expect(await order(page)).toEqual(arranged);
	await mode(page, 'queue');
	await start(page, 'demo-track');
	let count = await calls(page);
	await end(page);
	await replayed(page, titles.b, count);
	count = await calls(page);
	await position(page, 17);
	await end(page);
	await replayed(page, titles.c, count);
	expect(await order(page)).toEqual(arranged);
});

for (const value of ['one', 'queue'] as const) {
	test(`REP-B04 ${value} replays a one-track queue instead of starting radio`, async ({ page }) => {
		await page.getByTestId('queue-clear').click();
		await page.getByTestId('queue-add-second-signal').click();
		await expect(page.getByTestId('queue-item')).toHaveCount(1);
		await mode(page, value);
		await start(page, 'second-signal');
		await position(page, 31);
		const count = await calls(page);
		await end(page);
		await replayed(page, titles.b, count);
		expect(await order(page)).toEqual(['second-signal']);
	});
}

test('REP-B05 Off advances, preserves existing automatic radio and disables repeat during its pass', async ({ page }) => {
	await start(page);
	let count = await calls(page);
	await end(page);
	await replayed(page, titles.b, count);
	await start(page, 'third-light');
	count = await calls(page);
	const pass = await order(page);
	await end(page);
	await expect(page.getByTestId('radio-badge')).toBeVisible();
	expect(await order(page)).toEqual(pass);
	await expect.poll(() => calls(page)).toBe(count + 1);
	const control = page.getByTestId('player-repeat');
	await expect.soft(control).toBeDisabled();
	if (await control.count()) {
		await expect(control).toHaveAccessibleDescription(/radio/i);
	}
	await page.getByTestId('radio-stop').click();
	await expect.soft(control).toBeEnabled();
	expect(await order(page)).toEqual(pass);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: ended');
	const stopped = await calls(page);
	await staleEnd(page);
	expect(await calls(page)).toBe(stopped);
	await expect(page.getByTestId('player-status')).toHaveText('Status: ended');
});

for (const value of ['one', 'queue'] as const) {
	test(`REP-B06 ${value} never traps explicit Next/Previous; Next at tail stops`, async ({ page }) => {
		await mode(page, value);
		await start(page);
		await page.getByTestId('player-next').click();
		await playing(page);
		await expect(page.getByTestId('now-playing')).toContainText(titles.b);
		await page.getByTestId('player-prev').click();
		await playing(page);
		await expect(page.getByTestId('now-playing')).toContainText(titles.a);
		await start(page, 'third-light');
		const count = await calls(page);
		await page.getByTestId('player-next').click();
		await unchanged(page, titles.c, count);
		await expect(page.getByTestId('player-status')).toHaveText('Status: ended');
		await staleEnd(page);
		await unchanged(page, titles.c, count);
		await expect(page.getByTestId('player-status')).toHaveText('Status: ended');
	});

	test(`REP-B07 ${value} ignores manual pause and stale ended from a detached resource`, async ({ page }) => {
		await mode(page, value);
		await start(page);
		await paused(page);
		let count = await calls(page);
		await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('ended')));
		await unchanged(page, titles.a, count);
		await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
		await start(page);
		const old = await page.getByTestId('demo-audio').elementHandle();
		await page.getByTestId('player-next').click();
		await playing(page);
		count = await calls(page);
		expect(await old!.evaluate((el) => el.isConnected)).toBe(false);
		await old!.evaluate((el) => {
			Object.defineProperty(el, 'ended', { value: true, configurable: true });
			el.dispatchEvent(new Event('ended'));
		});
		await unchanged(page, titles.b, count);
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	});

	for (const timer of ['end-of-track', 'stop-after'] as const) {
		test(`REP-B08 ${value}: ${timer} sleep completion wins over repeat`, async ({ page }) => {
			await mode(page, value);
			await start(page, 'third-light');
			await position(page, 19);
			if (timer === 'stop-after') await page.getByTestId('sleep-timer-tracks').fill('1');
			await page.getByTestId(`sleep-timer-set-${timer}`).click();
			const count = await calls(page);
			await end(page);
			await unchanged(page, titles.c, count);
			await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
			expect(await order(page)).toEqual(ids);
			await staleEnd(page);
			await unchanged(page, titles.c, count);
			await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
		});
	}
}

test('REP-B09 clear during Queue repeat cannot resurrect playback (including detached ends)', async ({ page }) => {
	await mode(page, 'queue');
	await start(page);
	const old = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('queue-clear').click();
	const count = await calls(page);
	await old!.evaluate((el) => el.dispatchEvent(new Event('ended')));
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('player-status')).toHaveText('Status: empty');
	await expect(page.getByText('Queue is empty')).toBeVisible();
	expect(await calls(page)).toBe(count);
	await expect(page.getByTestId('radio-badge')).toHaveCount(0);
});

for (const value of ['one', 'queue'] as const) {
	test(`REP-B11 ${value}: duration sleep stops and stale ended stays inert`, async ({ page }) => {
		await mode(page, value);
		await start(page);
		await page.getByTestId('sleep-timer-minutes').fill('1');
		await page.getByTestId('sleep-timer-set-duration').click();
		const count = await calls(page);
		await page.evaluate(() => {
			const hook = (window as unknown as { __comforts: { advanceSleep: (ms: number) => void } }).__comforts;
			hook.advanceSleep(60_001);
		});
		await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
		await staleEnd(page);
		await unchanged(page, titles.a, count);
		await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
		expect(await order(page)).toEqual(ids);
	});

	test(`REP-B12 ${value}: stop-after-two counts repeated natural completions`, async ({ page }) => {
		await page.getByTestId('queue-clear').click();
		await page.getByTestId('queue-add-second-signal').click();
		await mode(page, value);
		await start(page, 'second-signal');
		await page.getByTestId('sleep-timer-tracks').fill('2');
		await page.getByTestId('sleep-timer-set-stop-after').click();
		let count = await calls(page);
		await end(page);
		await replayed(page, titles.b, count);
		count = await calls(page);
		await end(page);
		await unchanged(page, titles.b, count);
		await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	});
}

test('REP-B10 Off controls: manual Next still stops and paused ended stays inert', async ({ page }) => {
	await start(page);
	await paused(page);
	let count = await calls(page);
	await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('ended')));
	await unchanged(page, titles.a, count);
	await start(page, 'third-light');
	count = await calls(page);
	await page.getByTestId('player-next').click();
	await unchanged(page, titles.c, count);
	await expect(page.getByTestId('player-status')).toHaveText('Status: ended');
});
