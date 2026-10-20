import { expect, test, type Page } from '@playwright/test';

/** Keep media loading synthetic and pending; only fixture events settle playback. */
async function openPlayer(page: Page) {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
		const control = {
			playCalls: 0,
			mode: 'pending',
			resolve: () => {},
			reject: () => {},
			handlers: new Map<string, MediaSessionActionHandler | null>()
		};
		Object.assign(window, { __lifecycle: control });
		HTMLMediaElement.prototype.play = function () {
			control.playCalls += 1;
			if (control.mode === 'reject') return Promise.reject(new Error('private decoder details'));
			if (control.mode === 'resolve') return Promise.resolve();
			return new Promise<void>((resolve, reject) => {
				control.resolve = resolve;
				control.reject = () => reject(new Error('private decoder details'));
			});
		};
		HTMLMediaElement.prototype.pause = function () {};
		if ('mediaSession' in navigator) {
			navigator.mediaSession.setActionHandler = (action, handler) => {
				control.handlers.set(action, handler);
			};
		}
	});
	await page.goto('/demo/player');
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
}

async function event(page: Page, type: string) {
	await page.getByTestId('demo-audio').evaluate((el, name) => {
		// Native play events observe paused=false; pause observes paused=true.
		if (name === 'play' || name === 'pause') {
			Object.defineProperty(el, 'paused', { value: name === 'pause', configurable: true });
		}
		el.dispatchEvent(new Event(name));
	}, type);
}

async function settle(page: Page, mode: 'resolve' | 'reject') {
	await page.evaluate((next) => {
		const control = (window as unknown as { __lifecycle: { resolve: () => void; reject: () => void } }).__lifecycle;
		control[next]();
	}, mode);
}

test('detached old-source events cannot change or advance the current track', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	const old = await page.getByTestId('demo-audio').elementHandle();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await settle(page, 'resolve');
	await event(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	for (const type of ['play', 'playing', 'pause', 'error', 'ended']) {
		await old!.evaluate((el, name) => el.dispatchEvent(new Event(name)), type);
	}
	await expect(page.getByTestId('now-playing')).toContainText('Second Signal');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
});

test('Media Session pause wins over pending success and stale playing or ended', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await page.evaluate(() => {
		const control = (window as unknown as { __lifecycle: { handlers: Map<string, () => void> } }).__lifecycle;
		control.handlers.get('pause')!();
	});
	await settle(page, 'resolve');
	// A queued play event can arrive after pause; unlike a genuine resume,
	// it observes the element still paused and must not reopen loading.
	await page.getByTestId('demo-audio').evaluate((el) => el.dispatchEvent(new Event('play')));
	await event(page, 'playing');
	await event(page, 'ended');
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await expect(page.getByTestId('now-playing')).toContainText('Demo track');
	// Native controls can resume the same source via play/playing events.
	await event(page, 'play');
	await event(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
});

test('media failure fences pending success and retry replaces the failed resource', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	const failed = await page.getByTestId('demo-audio').elementHandle();
	await event(page, 'error');
	await settle(page, 'resolve');
	await event(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: error');
	await page.getByTestId('player-retry').click();
	expect(await failed!.evaluate((el) => el.isConnected)).toBe(false);
	await settle(page, 'resolve');
	await event(page, 'playing');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
});

test('current preload and paused resource errors remain recoverable', async ({ page }) => {
	await openPlayer(page);
	await event(page, 'error');
	await expect(page.getByTestId('player-error')).toBeVisible();
	await page.getByTestId('player-retry').click();
	await settle(page, 'resolve');
	await event(page, 'pause');
	await event(page, 'error');
	await expect(page.getByTestId('player-error')).toBeVisible();
});

test('native play on a failed resource uses explicit recovery instead of hiding the error', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await event(page, 'error');
	const failed = await page.getByTestId('demo-audio').elementHandle();
	await event(page, 'play');
	expect(await failed!.evaluate((el) => el.isConnected)).toBe(false);
	await settle(page, 'resolve');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
});

async function mediaAction(page: Page, action: 'play' | 'pause') {
	await page.evaluate((name) => {
		const control = (window as unknown as { __lifecycle: { handlers: Map<string, () => void> } }).__lifecycle;
		const handler = control.handlers.get(name);
		if (!handler) throw new Error(`missing Media Session ${name} handler`);
		handler();
	}, action);
}

async function playCalls(page: Page) {
	return page.evaluate(() => (window as unknown as { __lifecycle: { playCalls: number } }).__lifecycle.playCalls);
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

test('natural pause-before-ended advances each track and ends the queue', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await settle(page, 'resolve');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	for (const title of ['Second Signal', 'Third Light']) {
		await naturalEnd(page);
		await expect(page.getByTestId('now-playing')).toContainText(title);
		await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
		await settle(page, 'resolve');
		await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	}
	await naturalEnd(page);
	await expect(page.getByTestId('player-status')).toHaveText('Status: ended');
	await expect(page.getByTestId('now-playing')).toContainText('Third Light');
	await expect.poll(() => playCalls(page)).toBe(3);
	await mediaAction(page, 'pause');
	await expect(page.getByTestId('player-status')).toHaveText('Status: ended');
});

test('Media Session play resumes once after pause and reloads once after failure', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await settle(page, 'resolve');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	const original = await page.getByTestId('demo-audio').elementHandle();
	// Ordinary native pause (ended=false) must still pause the transport.
	await event(page, 'pause');
	await expect(page.getByTestId('player-status')).toHaveText('Status: paused');
	await mediaAction(page, 'play');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await expect.poll(() => playCalls(page)).toBe(2);
	expect(await original!.evaluate((el) => el.isConnected)).toBe(true);
	await settle(page, 'resolve');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await event(page, 'error');
	await expect(page.getByTestId('player-status')).toHaveText('Status: error');
	await mediaAction(page, 'play');
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	await expect.poll(() => playCalls(page)).toBe(3);
	expect(await original!.evaluate((el) => el.isConnected)).toBe(false);
	await settle(page, 'resolve');
	await expect(page.getByTestId('player-status')).toHaveText('Status: playing');
	await expect(page.getByTestId('player-error')).toHaveCount(0);
});

test('Media Session pause preserves the error alert and explicit retry', async ({ page }) => {
	await openPlayer(page);
	await page.getByTestId('queue-select-demo-track').click();
	await settle(page, 'reject');
	await expect(page.getByTestId('player-status')).toHaveText('Status: error');
	await mediaAction(page, 'pause');
	await expect(page.getByTestId('player-status')).toHaveText('Status: error');
	await expect(page.getByRole('alert')).toBeVisible();
	await expect(page.getByTestId('player-retry')).toBeVisible();
	await expect.poll(() => playCalls(page)).toBe(1);
});

test('seek readiness never replaces an active transport state', async ({ page }) => {
	await openPlayer(page);
	// Keep media loads pending, but let the independent seek controller use
	// the local deterministic Range endpoint rather than the held audio route.
	await page.route('**/api/demo/audio', (route) => route.continue());
	await page.getByTestId('queue-select-second-signal').click();
	await expect(page.getByTestId('player-status')).toHaveText('Status: loading');
	for (const state of ['loading', 'playing', 'paused', 'error', 'ended', 'empty'] as const) {
		if (state === 'playing') await settle(page, 'resolve');
		if (state === 'paused') await event(page, 'pause');
		if (state === 'error') await event(page, 'error');
		if (state === 'ended') {
			await page.getByTestId('queue-select-third-light').click();
			await settle(page, 'resolve');
			await naturalEnd(page);
		}
		if (state === 'empty') await page.getByTestId('queue-clear').click();
		await expect(page.getByTestId('player-status')).toHaveText(`Status: ${state}`);
		const result = await page.evaluate(async () => {
			if (!window.__seekStorm) throw new Error('seek storm hook missing');
			return window.__seekStorm();
		});
		expect(result).toEqual({ completed: 2, aborted: 9, lastBytes: 100 });
		await expect(page.getByTestId('seek-stats')).toHaveText('completed 2, aborted 9, last 100 bytes');
		await expect(page.getByTestId('player-status')).toHaveText(`Status: ${state}`);
	}
});

test('render synthetic transport states at desktop and mobile sizes', async ({ page }, testInfo) => {
	const directory = process.env['PLAYER_RENDER_DIR'] ?? testInfo.outputPath('renders');
	for (const [name, width, height] of [['desktop', 1280, 800], ['mobile', 390, 844]] as const) {
		await page.setViewportSize({ width, height });
		await openPlayer(page);
		async function capture(state: string) {
			await page.screenshot({ path: `${directory}/${name}-${state}.png`, fullPage: true });
		}
		await capture('selected');
		await page.getByTestId('queue-select-second-signal').click();
		await capture('loading');
		await settle(page, 'resolve');
		await event(page, 'playing');
		await capture('playing');
		await event(page, 'pause');
		await capture('paused');
		await event(page, 'play');
		await event(page, 'error');
		await capture('error');
		await page.getByTestId('queue-clear').click();
		await capture('empty');
		await page.getByTestId('queue-add-third-light').click();
		await page.getByTestId('player-next').click();
		await capture('ended');
		await page.reload();
	}
});
