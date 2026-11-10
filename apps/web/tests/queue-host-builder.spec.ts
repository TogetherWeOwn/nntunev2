import { Buffer } from 'node:buffer';
import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';

const ITEMS = [
	{ id: 'host-a', title: 'Host A', artist: 'Test artist', streamUrl: '/test-audio/host-a' },
	{ id: 'host-b', title: 'Host B', artist: 'Test artist', streamUrl: '/test-audio/host-b' }
];

async function library(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: ITEMS }));
	// Generated PCM silence decoded by the browser; no playback event doubles.
	const bytes = 60 * 8000 * 2;
	const wav = Buffer.alloc(44 + bytes);
	wav.write('RIFF'); wav.writeUInt32LE(36 + bytes, 4); wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
	wav.writeUInt32LE(8000, 24); wav.writeUInt32LE(16000, 28);
	wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34);
	wav.write('data', 36); wav.writeUInt32LE(bytes, 40);
	await page.route('**/test-audio/**', (route) => route.fulfill({ contentType: 'audio/wav', body: wav }));
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	for (const item of ITEMS) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`)
			.getByRole('button', { name: new RegExp(`^${item.title} —`) }).click();
	}
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('readyState', 4);
}

async function primary(page: Page, path: string) {
	await page.locator(`nav[aria-label="Primary"] a[href="${path}"]:visible`).click();
	await expect(page).toHaveURL(new RegExp(`${path}$`));
}

test('desktop starts closed, opens without navigation, and keyboard close restores its invoker', async ({ page }) => {
	await library(page);
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
	const url = page.url();
	const toggle = page.getByTestId('player-queue-toggle');
	await toggle.focus();
	await page.keyboard.press('Enter');
	await expect(page.getByTestId('queue-panel')).toBeVisible();
	const host = page.getByTestId('right-panel');
	await expect(host.getByRole('heading', { name: 'Queue', exact: true })
		.and(host.locator(':scope > h2'))).toBeFocused();
	expect(page.url()).toBe(url);
	await page.keyboard.press('Escape');
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
	await expect(toggle).toBeFocused();
	await expect(page.getByTestId('audio-player')).toHaveCount(1);
});

test('desktop panel deep links track browser back/forward and closing removes only panel', async ({ page }) => {
	await page.goto('/home');
	await expect(page.getByTestId('player-queue-toggle')).toBeVisible();
	await page.evaluate(() => {
		const link = document.createElement('a'); link.href = '/home?panel=queue&keep=1';
		link.textContent = 'Open panel deep link'; document.body.append(link); link.click();
	});
	await expect(page.getByTestId('queue-panel')).toBeVisible();
	await page.goBack();
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
	await page.goForward();
	await expect(page.getByTestId('queue-panel')).toBeVisible();
	await page.getByRole('button', { name: 'Close queue', exact: true }).click();
	await expect(page).toHaveURL(/\/home\?keep=1$/);
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
});

for (const playing of [false, true]) {
	for (const mobile of [false, true]) {
		test(`queue presentation and navigation preserve the real ${playing ? 'playing' : 'paused'} decoder on ${mobile ? 'mobile' : 'desktop'}`, async ({ page }) => {
			if (mobile) await page.setViewportSize({ width: 390, height: 844 });
			await library(page);
			const audio = page.getByTestId('audio-player');
			const original = (await audio.elementHandle())!;
			if (playing) await page.getByTestId('player-play-pause').click();
			await expect(audio).toHaveJSProperty('paused', !playing);
			if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(0.2);
			const before = await audio.evaluate((el: HTMLAudioElement) => {
				const events: string[] = [];
				for (const name of ['play', 'pause', 'loadstart', 'emptied']) el.addEventListener(name, () => events.push(name));
				Object.assign(el, { hostEvents: events });
				return el.currentTime;
			});
			await page.getByTestId('player-queue-toggle').click();
			await expect(page.getByTestId('queue-item')).toHaveCount(2);
			if (mobile) await expect(page).toHaveURL(/\/queue$/);
			else await expect(page).toHaveURL(/\/library$/);
			await expect(page.locator('main')).toHaveCount(1);
			await page.getByRole('button', { name: 'Close queue', exact: true }).click();
			await expect(page).toHaveURL(/\/library$/);
			await primary(page, '/home');
			await page.getByTestId('player-queue-toggle').click();
			await expect(page.getByTestId('queue-item')).toHaveCount(2);
			expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
			await expect(audio).toHaveCount(1);
			await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
			await expect(audio).toHaveJSProperty('paused', !playing);
			await expect(page.getByTestId('now-playing')).toContainText('Host A');
			if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(before + 0.2);
			else expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(before);
			expect(await audio.evaluate((el) => (el as HTMLAudioElement & { hostEvents: string[] }).hostEvents)).toEqual([]);
		});
	}
}

for (const playing of [false, true]) {
	test(`queue menu survives session clock publications with a real ${playing ? 'playing' : 'paused'} decoder`, async ({ page }) => {
		await page.clock.install();
		await library(page);
		const audio = page.getByTestId('audio-player');
		if (playing) await page.getByTestId('player-play-pause').click();
		await expect(audio).toHaveJSProperty('paused', !playing);
		if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(0.2);
		const original = (await audio.elementHandle())!;
		await page.getByTestId('player-queue-toggle').click();
		await page.locator('[data-testid="queue-item"][data-track-id="host-a"]')
			.getByTestId('queue-more').click();
		const menu = page.getByTestId('track-context-menu');
		await expect(menu).toBeVisible();
		const mounted = (await menu.elementHandle())!;
		await page.keyboard.press('Home');
		const first = menu.getByRole('menuitem', { name: 'Play now', exact: true });
		await expect(first).toBeFocused();
		await page.clock.runFor(2500);
		await expect(menu).toBeVisible();
		expect(await mounted.evaluate((el) => el === document.querySelector('[data-testid="track-context-menu"]'))).toBe(true);
		await expect(first).toBeFocused();
		await expect(page.getByTestId('queue-item')).toHaveCount(2);
		expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		await expect(audio).toHaveJSProperty('paused', !playing);
		await page.keyboard.press('Escape');
		await expect(menu).toHaveCount(0);
	});
}

test('mobile queue supports back/forward and toggling closed returns to the invoking route', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await library(page);
	await page.getByTestId('player-queue-toggle').click();
	await expect(page).toHaveURL(/\/queue$/);
	await page.goBack();
	await expect(page).toHaveURL(/\/library$/);
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
	await page.goForward();
	await expect(page).toHaveURL(/\/queue$/);
	await expect(page.getByTestId('queue-panel')).toHaveCount(1);
	await page.getByTestId('player-queue-toggle').click();
	await expect(page).toHaveURL(/\/library$/);
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
	await expect(page.getByTestId('player-queue-toggle')).toBeFocused();
});

test('mobile queue with a redundant panel query never becomes its own close destination', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await page.goto('/queue?panel=queue');
	await expect(page.getByTestId('queue-route')).toBeVisible();
	await page.getByRole('button', { name: 'Close queue', exact: true }).click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
});

test('mobile direct queue entry and responsive transitions expose one actionable surface', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await page.goto('/queue');
	await expect(page.getByTestId('queue-route')).toBeVisible();
	await expect(page.getByTestId('queue-panel')).toHaveCount(1);
	await expect(page.locator('main')).toHaveCount(1);
	await page.setViewportSize({ width: 1280, height: 900 });
	await expect(page).toHaveURL(/\/home\?panel=queue$/);
	await expect(page.getByTestId('right-panel')).toBeVisible();
	await expect(page.getByTestId('queue-panel')).toHaveCount(1);
	await page.setViewportSize({ width: 390, height: 844 });
	await expect(page).toHaveURL(/\/queue$/);
	await expect(page.getByTestId('queue-panel')).toHaveCount(1);
	await expect(page.getByTestId('right-panel')).toHaveCount(0);
});

async function clientLink(page: Page, path: string) {
	await page.evaluate((path) => {
		const link = document.createElement('a'); link.href = path;
		link.textContent = 'Test route navigation'; document.body.append(link); link.click();
	}, path);
}

test('production clock interval follows the actual decoder, not the layout lifetime', async ({ page }) => {
	await page.addInitScript(() => {
		const active = new Set<number>();
		const interval = window.setInterval.bind(window);
		const clear = window.clearInterval.bind(window);
		window.setInterval = (handler, delay, ...args) => {
			const id = interval(handler, delay, ...args);
			if (delay === 1000) active.add(id);
			return id;
		};
		window.clearInterval = (id) => {
			if (id !== undefined) active.delete(id);
			clear(id);
		};
		Reflect.set(window, '__hostClockIntervals', active);
	});
	const intervals = () => page.evaluate(() => [...Reflect.get(window, '__hostClockIntervals') as Set<number>]);
	await page.goto('/home');
	await expect(page.getByTestId('player-queue-toggle')).toBeVisible();
	expect(await intervals()).toEqual([]);
	await library(page);
	await expect.poll(intervals).toHaveLength(1);
	const active = await intervals();
	const audio = (await page.getByTestId('audio-player').elementHandle())!;
	await primary(page, '/home');
	expect(await intervals()).toEqual(active);
	expect(await audio.evaluate((node) => node === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
	await clientLink(page, '/pricing');
	await expect(page).toHaveURL(/\/pricing$/);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect.poll(intervals).toEqual([]);
	await clientLink(page, '/library');
	await expect(page.getByTestId('audio-player')).toHaveJSProperty('readyState', 4);
	await expect.poll(intervals).toHaveLength(1);
});

test('mobile automatic deep-link conversion leaves an escapable Back history', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await page.goto('/home');
	await expect(page.getByTestId('player-queue-toggle')).toBeVisible();
	await clientLink(page, '/search?panel=queue');
	await expect(page).toHaveURL(/\/queue$/);
	await page.goBack();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
});

test('responsive queue close returns to its current main route rather than stale mobile history', async ({ page }) => {
	await library(page);
	await page.getByTestId('player-queue-toggle').click();
	await page.setViewportSize({ width: 390, height: 844 });
	await expect(page).toHaveURL(/\/queue$/);
	await page.getByRole('button', { name: 'Close queue', exact: true }).click();
	await expect(page).toHaveURL(/\/library$/);
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
});

test('hidden queue state never navigates a public page when the viewport narrows', async ({ page }) => {
	await library(page);
	await page.getByTestId('player-queue-toggle').click();
	await clientLink(page, '/pricing');
	await expect(page).toHaveURL(/\/pricing$/);
	await page.setViewportSize({ width: 390, height: 844 });
	await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
	await expect(page.getByTestId('shell-rail')).toHaveCount(0);
	await expect(page).toHaveURL(/\/pricing$/);
	await clientLink(page, '/pricing?panel=queue');
	await expect(page).toHaveURL(/\/pricing\?panel=queue$/);
	await expect(page.getByTestId('queue-panel')).toHaveCount(0);
});

for (const width of [320, 390, 1280]) {
	for (const appearance of ['light', 'dark']) {
		test(`queue host reflows and passes WCAG axe at ${width}px in ${appearance}`, async ({ page }) => {
			await page.setViewportSize({ width, height: 900 });
			await library(page);
			await page.getByLabel('Appearance', { exact: true }).selectOption(appearance);
			await page.getByTestId('player-queue-toggle').click();
			await expect(page.getByTestId('queue-panel')).toBeVisible();
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
			const result = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa']).analyze();
			expect(result.violations).toEqual([]);
		});
	}
}

for (const playing of [false, true]) {
	for (const colorScheme of ['light', 'dark'] as const) {
		test(`mobile metadata management leaves the mounted real ${playing ? 'playing' : 'paused'} transport unchanged in ${colorScheme}`, async ({ page }) => {
			await page.setViewportSize({ width: 390, height: 844 });
			await page.emulateMedia({ colorScheme });
			await library(page);
			const audio = page.getByTestId('audio-player');
			const original = (await audio.elementHandle())!;
			if (playing) await page.getByTestId('player-play-pause').click();
			await expect(audio).toHaveJSProperty('paused', !playing);
			if (playing) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThan(0.2);
			const clock = await audio.evaluate((el: HTMLAudioElement) => {
				const events: string[] = [];
				for (const name of ['play', 'pause', 'loadstart', 'emptied', 'seeking']) el.addEventListener(name, () => events.push(name));
				Object.assign(el, { managementEvents: events });
				return el.currentTime;
			});
			const controls = async () => {
				await expect(page.locator('[data-player-transport]')).toHaveCount(1);
				await expect(page.getByTestId('player-next')).toHaveCount(1);
				// Compact full controls are display:none per C3; the control stays
				// mounted and enabled, and the mini row remains the visible surface.
				await expect(page.getByTestId('player-next')).toBeHidden();
				await expect(page.getByTestId('player-next')).toBeEnabled();
				await expect(page.locator('button[aria-label="Previous track"]')).toBeEnabled();
				return page.locator('[data-player-transport] button:not([data-testid="player-favorite"])').evaluateAll((nodes) => nodes.map((node) => ({
					label: node.getAttribute('aria-label'), text: node.textContent,
					disabled: (node as HTMLButtonElement).disabled, pressed: node.getAttribute('aria-pressed')
				})));
			};
			const before = await controls();
			const unchanged = async (saved: boolean) => {
				// Favorite membership is deliberately shared; transport state is not.
				await expect(page.getByTestId('player-favorite')).toHaveAttribute('aria-pressed', String(saved));
				await expect(page.getByTestId('player-favorite')).toHaveAccessibleName(saved ? 'Remove from Favorites' : 'Save to Favorites');
				expect(await controls()).toEqual(before);
				expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
				await expect(audio).toHaveCount(1);
				await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
				await expect(audio).toHaveJSProperty('paused', !playing);
				expect(await audio.evaluate((el) => Reflect.get(el, 'managementEvents'))).toEqual([]);
				await expect(page.getByTestId('now-playing')).toContainText('Host A');
				expect(await audio.evaluate((el: HTMLAudioElement) => el.currentTime)).toBeGreaterThanOrEqual(clock);
			};
			await page.locator('[data-testid="library-item"][data-id="host-a"]').getByRole('button', { name: 'Star Host A', exact: true }).click();
			await unchanged(true);
			await page.getByRole('combobox', { name: 'Show', exact: true }).selectOption('favorites');
			await unchanged(true);
			const unstar = page.locator('[data-testid="favorite-item"][data-id="host-a"]').getByRole('button', { name: 'Unstar Host A', exact: true });
			await unstar.focus();
			await page.keyboard.press('Space');
			await expect(page.getByTestId('library-item')).toHaveCount(0);
			await unchanged(false);
			await page.getByRole('button', { name: 'Show all tracks', exact: true }).click();
			await expect(page.getByTestId('library-item')).toHaveCount(2);
			await unchanged(false);
		});
	}
}
