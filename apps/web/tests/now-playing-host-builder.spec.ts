import { Buffer } from 'node:buffer';
import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';

const ITEM = { id: 'now-playing-a', title: 'Current track', artist: 'Test artist', streamUrl: '/test-audio/now-playing-a' };

async function library(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: [ITEM] }));
	const bytes = 60 * 8000 * 2;
	const wav = Buffer.alloc(44 + bytes);
	wav.write('RIFF'); wav.writeUInt32LE(36 + bytes, 4); wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
	wav.writeUInt32LE(8000, 24); wav.writeUInt32LE(16000, 28);
	wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34);
	wav.write('data', 36); wav.writeUInt32LE(bytes, 40);
	await page.route('**/test-audio/**', (route) => {
		const range = /^bytes=(\d+)-(\d*)$/.exec(route.request().headers()['range'] ?? '');
		const start = range ? Number(range[1]) : 0;
		const end = range?.[2] ? Math.min(Number(range[2]), wav.length - 1) : wav.length - 1;
		if (start > end) return route.fulfill({ status: 416, headers: { 'Content-Range': `bytes */${wav.length}` } });
		return route.fulfill({
			status: range ? 206 : 200,
			contentType: 'audio/wav',
			headers: { 'Accept-Ranges': 'bytes', ...(range ? { 'Content-Range': `bytes ${start}-${end}/${wav.length}` } : {}) },
			body: wav.subarray(start, end + 1)
		});
	});
	await page.goto('/library');
	await page.getByTestId('library-item').getByRole('button', { name: /^Current track —/ }).click();
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('readyState', 4);
}

async function clientLink(page: Page, path: string) {
	await page.evaluate((path) => {
		const link = document.createElement('a'); link.href = path;
		link.textContent = 'Navigate to test route'; document.body.append(link); link.click();
	}, path);
}

function view(page: Page, _mobile = false) {
	// One Now Playing route at every width; no desktop panel presentation.
	return page.getByTestId('now-playing-route');
}

for (const mobile of [false, true]) {
	for (const playing of [false, true]) {
		test(`Now playing keeps the same native ${playing ? 'playing' : 'paused'} decoder on ${mobile ? 'mobile' : 'desktop'}`, async ({ page }) => {
			await page.setViewportSize({ width: mobile ? 390 : 1280, height: 900 });
			await library(page);
			const audio = page.getByTestId('audio-player');
			const original = (await audio.elementHandle())!;
			if (playing) await page.getByTestId('player-play-pause').click();
			await expect(audio).toHaveJSProperty('paused', !playing);
			if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(0.2);
			const before = await audio.evaluate((el: HTMLAudioElement) => {
				const events: string[] = [];
				for (const name of ['play', 'pause', 'loadstart', 'emptied']) el.addEventListener(name, () => events.push(name));
				Object.assign(el, { viewEvents: events });
				return el.currentTime;
			});
			const toggle = page.getByTestId('player-now-playing-toggle');
			await toggle.focus();
			await page.keyboard.press('Enter');
			await expect(view(page, mobile)).toBeVisible();
			await expect(view(page, mobile).getByRole('heading', { name: 'Now playing', exact: true }).first()).toBeFocused();
			await expect(page).toHaveURL(/\/now-playing$/);
			await expect(page.getByTestId('right-panel')).toHaveCount(0);
			await expect(page.locator('main')).toHaveCount(1);
			await expect(page.getByTestId('player-play-pause')).toHaveCount(1);
			await expect(view(page, mobile)).toContainText(ITEM.title);
			await page.keyboard.press('Escape');
			await expect(page).toHaveURL(/\/library$/);
			await expect(toggle).toBeFocused();
			await expect(view(page, mobile)).toHaveCount(0);
			expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
			await expect(audio).toHaveCount(1);
			await expect(audio).toHaveAttribute('src', ITEM.streamUrl);
			await expect(audio).toHaveJSProperty('paused', !playing);
			if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(before + 0.2);
			else expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(before);
			expect(await audio.evaluate((el) => (el as HTMLAudioElement & { viewEvents: string[] }).viewEvents)).toEqual([]);
		});
	}
}

test('the compact bar is the only play and favorite surface at both widths', async ({ page }) => {
	for (const width of [390, 1280]) {
		await page.setViewportSize({ width, height: 900 });
		await library(page);
		await expect(page.getByTestId('player-play-pause')).toHaveCount(1);
		await expect(page.getByTestId('player-favorite')).toHaveCount(1);
		await expect(page.getByTestId('mini-player-play-pause')).toHaveCount(0);
		await expect(page.getByTestId('mini-player-favorite')).toHaveCount(0);
		await page.getByTestId('player-now-playing-toggle').click();
		await expect(page.getByTestId('now-playing-route')).toBeVisible();
		await expect(page.getByTestId('player-play-pause')).toHaveCount(1);
		await expect(page.getByTestId('player-favorite')).toHaveCount(1);
		await page.getByRole('button', { name: 'Close Now playing', exact: true }).click();
		await expect(page).toHaveURL(/\/library$/);
		await page.goto('/library');
	}
});

test('metadata-only selection shows plain identity with Play disabled and no audio element', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await page.route('**/api/library', (route) => route.fulfill({ json: [{ id: 'now-playing-meta', title: 'Quiet track', artist: 'No stream' }] }));
	await page.goto('/library');
	await page.getByTestId('library-item').getByRole('button', { name: /^Quiet track —/ }).click();
	const mini = page.getByTestId('mini-player');
	await expect(mini).toBeVisible();
	await expect(mini).toContainText('Quiet track');
	await expect(mini.getByRole('link')).toHaveCount(0);
	await expect(page.getByTestId('player-play-pause')).toBeDisabled();
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('mobile hint opens Now playing; browser history and Queue return preserve a usable view', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await library(page);
	await page.getByTestId('mini-player').getByRole('link').click();
	await expect(page).toHaveURL(/\/now-playing$/);
	await page.goBack();
	await expect(page).toHaveURL(/\/library$/);
	await page.goForward();
	await expect(page.getByTestId('now-playing-route')).toBeVisible();
	await page.getByTestId('player-queue-toggle').click();
	await expect(page).toHaveURL(/\/queue$/);
	await page.getByRole('button', { name: 'Close queue', exact: true }).click();
	await expect(page.getByTestId('now-playing-route')).toBeVisible();
	await page.getByRole('button', { name: 'Close Now playing', exact: true }).click();
	await expect(page).toHaveURL(/\/library$/);
});

test('Now playing direct entry and empty state have one main at every width', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await page.goto('/now-playing?panel=now-playing');
	await expect(page.getByTestId('now-playing-route')).toBeVisible();
	await expect(page.getByTestId('now-playing-route')).toContainText('Choose a playable track');
	await expect(page.getByTestId('right-panel')).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Close Now playing', exact: true })).toHaveCount(1);
	await expect(page.locator('main')).toHaveCount(1);
	// Resizing never converts the route into a panel or another route.
	await page.setViewportSize({ width: 1280, height: 900 });
	await expect(page).toHaveURL(/\/now-playing\?panel=now-playing$/);
	await expect(page.getByTestId('now-playing-route')).toBeVisible();
	await page.setViewportSize({ width: 390, height: 844 });
	await expect(page).toHaveURL(/\/now-playing\?panel=now-playing$/);
	await page.getByRole('button', { name: 'Close Now playing', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.locator('main')).toHaveCount(1);
});

test('mobile repeated Queue and Now playing switches have no cyclic close destinations', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await library(page);
	await page.getByTestId('player-queue-toggle').click();
	await expect(page).toHaveURL(/\/queue$/);
	await page.getByTestId('player-now-playing-toggle').click();
	await expect(page.getByTestId('now-playing-route')).toBeVisible();
	await page.getByTestId('player-queue-toggle').click();
	await expect(page).toHaveURL(/\/queue$/);
	await page.getByRole('button', { name: 'Close queue', exact: true }).click();
	await expect(page).not.toHaveURL(/\/queue$/);
	if (new URL(page.url()).pathname === '/now-playing') {
		await page.getByRole('button', { name: 'Close Now playing', exact: true }).click();
	}
	await expect(page).toHaveURL(/\/library$/);
});

test('an unknown panel value is ignored and keeps the current route', async ({ page }) => {
	await page.goto('/library');
	await clientLink(page, '/home?panel=now-playing&keep=1');
	await expect(page).toHaveURL(/\/home\?panel=now-playing&keep=1$/);
	await expect(page.getByTestId('now-playing-route')).toHaveCount(0);
	await expect(page.getByTestId('right-panel')).toHaveCount(0);
	await page.setViewportSize({ width: 390, height: 844 });
	await clientLink(page, '/search?panel=now-playing');
	await expect(page).toHaveURL(/\/search\?panel=now-playing$/);
	await expect(page.getByTestId('now-playing-route')).toHaveCount(0);
});

test('Now playing route and Queue panel switch without a panel query for Now playing', async ({ page }) => {
	await library(page);
	await clientLink(page, '/library?panel=queue&keep=1');
	await expect(page.getByTestId('queue-panel')).toBeVisible();
	await page.getByTestId('player-now-playing-toggle').click();
	await expect(page).toHaveURL(/\/now-playing$/);
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
	await page.getByTestId('player-queue-toggle').click();
	await expect(page.getByTestId('queue-panel')).toBeVisible();
	await expect(page).toHaveURL(/\/now-playing$/);
	await page.getByRole('button', { name: 'Close queue', exact: true }).click();
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
	await expect(page).toHaveURL(/\/now-playing$/);
});

for (const width of [390, 1280]) {
	test(`Queue exits native full screen before opening at ${width}px`, async ({ page }) => {
		await page.setViewportSize({ width, height: 900 });
		await library(page);
		await page.getByTestId('player-now-playing-toggle').click();
		await expect(view(page, width < 800)).toBeVisible();
		await page.getByTestId('player-fullscreen').click();
		await expect.poll(() => page.evaluate(() => !!document.fullscreenElement)).toBe(true);
		await page.getByTestId('player-queue-toggle').focus();
		await page.keyboard.press('Enter');
		await expect.poll(() => page.evaluate(() => !!document.fullscreenElement)).toBe(false);
		await expect(page.getByTestId('queue-panel')).toBeVisible();
		await expect(page.getByRole('heading', { name: 'Queue', exact: true }).first()).toBeFocused();
	});
}

test('public routes never expose or reactivate Now playing', async ({ page }) => {
	await library(page);
	await page.getByTestId('player-now-playing-toggle').click();
	await expect(page.getByTestId('now-playing-route')).toBeVisible();
	await clientLink(page, '/pricing?panel=now-playing');
	await expect(page).toHaveURL(/\/pricing\?panel=now-playing$/);
	await page.setViewportSize({ width: 390, height: 844 });
	await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
	await expect(page.getByTestId('right-panel')).toHaveCount(0);
	await expect(page.getByTestId('now-playing-route')).toHaveCount(0);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect(page).toHaveURL(/\/pricing\?panel=now-playing$/);
});

for (const playing of [false, true]) {
	test(`expanded seek and Playback restart preserve the real ${playing ? 'playing' : 'paused'} decoder`, async ({ page }) => {
		await page.setViewportSize({ width: 390, height: 900 });
		await library(page);
		const audio = page.getByTestId('audio-player');
		const original = (await audio.elementHandle())!;
		await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.seekable.length)).toBeGreaterThan(0);
		if (playing) await page.getByTestId('player-play-pause').click();
		await expect(audio).toHaveJSProperty('paused', !playing);
		await page.getByTestId('mini-player').getByRole('link').click();
		const surface = page.getByTestId('now-playing-route');
		const seek = surface.getByTestId('player-seek');
		await expect(seek).toHaveAttribute('max', '60');
		await seek.fill('24');
		await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => Math.floor(el.currentTime))).toBeGreaterThanOrEqual(24);
		await surface.focus();
		await page.keyboard.press('0');
		await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeLessThan(3);
		await expect(surface.getByTestId('library-restart-status')).toContainText('Restarted');
		await expect(surface.getByTestId('library-restart')).toBeVisible();
		await seek.fill('24');
		await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => Math.floor(el.currentTime))).toBeGreaterThanOrEqual(24);
		await surface.getByTestId('library-restart').click();
		await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeLessThan(3);
		await expect(surface.getByTestId('library-restart-status')).toContainText('Restarted');
		await expect(audio).toHaveJSProperty('paused', !playing);
		expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		await expect(audio).toHaveCount(1);
		await expect(audio).toHaveAttribute('src', ITEM.streamUrl);
		if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(0.2);
	});
}

for (const width of [320, 360, 390]) {
	for (const appearance of ['light', 'dark']) {
		test(`expanded control reading and focus order at ${width}px in ${appearance}`, async ({ page }) => {
			await page.setViewportSize({ width, height: 900 });
			await library(page);
			await page.getByLabel('Appearance', { exact: true }).selectOption(appearance);
			await page.getByTestId('mini-player').getByRole('link').click();
			const surface = page.getByTestId('now-playing-route');
			await expect(surface).toBeVisible();
			await expect(surface.getByTestId('player-play-pause')).toHaveCount(1);
			await expect(surface.getByTestId('player-seek')).toHaveCount(1);
			await expect(surface.getByTestId('player-favorite')).toHaveCount(1);
			const geometry = await surface.evaluate((el) => {
				const rect = (selector: string) => el.querySelector(selector)!.getBoundingClientRect().toJSON();
				return {
					art: rect('.cinema-artwork'), seek: rect('.seek'), transport: rect('.transport-buttons'),
					play: rect('[data-testid="player-play-pause"]'), toggles: rect('.sound'),
					seekFirst: !!(el.querySelector('[data-testid="player-seek"]')!.compareDocumentPosition(el.querySelector('[data-testid="player-play-pause"]')!) & Node.DOCUMENT_POSITION_FOLLOWING)
				};
			});
			expect(geometry.art.x + geometry.art.width / 2).toBeCloseTo(width / 2, 0);
			expect(geometry.art.bottom).toBeLessThanOrEqual(geometry.seek.top);
			expect(geometry.seek.bottom).toBeLessThanOrEqual(geometry.transport.top);
			expect(geometry.transport.bottom).toBeLessThanOrEqual(geometry.toggles.top);
			expect(geometry.seekFirst).toBe(true);
			expect(geometry.play.width).toBe(56);
			expect(geometry.play.height).toBe(56);
			const seek = surface.getByTestId('player-seek');
			await seek.focus();
			await expect(seek).toBeFocused();
			for (const id of ['player-seek-back', 'player-seek-forward']) {
				await page.keyboard.press('Tab');
				await expect(surface.getByTestId(id)).toBeFocused();
			}
			await page.keyboard.press('Tab');
			await expect(surface.getByRole('button', { name: 'Previous track', exact: true })).toBeFocused();
			await page.keyboard.press('Tab');
			await expect(surface.getByTestId('library-restart')).toBeFocused();
			for (const id of ['player-play-pause', 'player-next']) {
				await page.keyboard.press('Tab');
				await expect(surface.getByTestId(id)).toBeFocused();
			}
			await page.keyboard.press('Tab');
			await expect(surface.getByRole('button', { name: 'Repeat: Off', exact: true })).toBeFocused();
			await page.keyboard.press('Tab');
			const favorite = surface.getByTestId('player-favorite');
			await expect(favorite).toBeFocused();
			expect(await favorite.evaluate((el) => getComputedStyle(el).outlineStyle)).not.toBe('none');
			expect(await favorite.evaluate((el) => {
				const box = el.getBoundingClientRect();
				const nav = document.querySelector('[data-testid="shell-tabbar"]')!.getBoundingClientRect();
				return box.top >= 0 && box.bottom + 4 <= nav.top &&
					el.contains(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2));
			})).toBe(true);
			await page.keyboard.press('Space');
			await expect(favorite).toHaveAttribute('aria-pressed', 'true');
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
			const result = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa']).analyze();
			expect(result.violations).toEqual([]);
			await page.screenshot({ path: `test-results/expanded-order-${width}-${appearance}.png`, fullPage: true });
		});
	}
}

for (const width of [320, 390, 1280]) {
	for (const appearance of ['light', 'dark']) {
		test(`Now playing keyboard focus and WCAG reflow at ${width}px in ${appearance}`, async ({ page }) => {
			await page.setViewportSize({ width, height: 900 });
			await library(page);
			await page.getByLabel('Appearance', { exact: true }).selectOption(appearance);
			await page.getByTestId('player-now-playing-toggle').focus();
			await page.keyboard.press('Enter');
			const surface = view(page, width < 800);
			await expect(surface).toBeVisible();
			await page.keyboard.press('Tab');
			await expect(surface.getByRole('button', { name: 'Close Now playing', exact: true })).toBeFocused();
			expect(await surface.getByRole('button', { name: 'Close Now playing', exact: true }).evaluate((el) => getComputedStyle(el).outlineStyle)).not.toBe('none');
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
			const result = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa']).analyze();
			expect(result.violations).toEqual([]);
			await page.screenshot({ path: `test-results/now-playing-${width}-${appearance}.png`, fullPage: true });
		});
	}
}
