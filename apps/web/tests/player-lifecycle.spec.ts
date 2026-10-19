import { expect, test, type Page } from '@playwright/test';

/** Keep media loading synthetic and pending; only fixture events settle playback. */
async function openPlayer(page: Page) {
	await page.route('**/api/demo/audio**', () => {});
	await page.addInitScript(() => {
		sessionStorage.removeItem('nntune:demo-queue:v1');
		const control = {
			mode: 'pending',
			resolve: () => {},
			reject: () => {},
			handlers: new Map<string, MediaSessionActionHandler | null>()
		};
		Object.assign(window, { __lifecycle: control });
		HTMLMediaElement.prototype.play = function () {
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
