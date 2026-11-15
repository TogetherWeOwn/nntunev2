import { Buffer } from 'node:buffer';
import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Locator, type Page } from '@playwright/test';
import { seekProofFailure, seekViaPlayerUi, togglePlayViaPlayerUi } from '../../../e2e/helpers/player-ui.js';
import { ICON_PATHS, type IconName } from '../src/lib/design/icons.js';
import { openQueue } from './helpers/queue-host.js';

const ITEMS = [
	{ id: 'transport-a', title: 'Transport A', artist: 'Test artist', streamUrl: '/test-audio/transport-a' },
	{ id: 'transport-b', title: 'Transport B', artist: 'Test artist', streamUrl: '/test-audio/transport-b' },
	{ id: 'transport-c', title: 'Transport C', artist: 'Test artist', streamUrl: '/test-audio/transport-c' }
];

// Generated silence, decoded by the real browser media pipeline.
function silentWav(): Buffer {
	const bytes = 60 * 8000 * 2;
	const wav = Buffer.alloc(44 + bytes);
	wav.write('RIFF', 0);
	wav.writeUInt32LE(36 + bytes, 4);
	wav.write('WAVEfmt ', 8);
	wav.writeUInt32LE(16, 16);
	wav.writeUInt16LE(1, 20);
	wav.writeUInt16LE(1, 22);
	wav.writeUInt32LE(8000, 24);
	wav.writeUInt32LE(16000, 28);
	wav.writeUInt16LE(2, 32);
	wav.writeUInt16LE(16, 34);
	wav.write('data', 36);
	wav.writeUInt32LE(bytes, 40);
	return wav;
}

const time = (audio: Locator) => audio.evaluate((el: HTMLAudioElement) => el.currentTime);

/** Select track A from the library, decode it, and leave it paused at a known position. */
async function openTransport(page: Page, { playing, failFirstLoad = false, items = ITEMS }: { playing: boolean; failFirstLoad?: boolean; items?: typeof ITEMS }) {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await page.route('**/api/library', (route) => route.fulfill({ json: items }));
	const wav = silentWav();
	let requests = 0;
	await page.route('**/test-audio/**', (route) => {
		if (++requests === 1 && failFirstLoad) return route.fulfill({ status: 503, body: '' });
		// A 200-only intercepted media response is not seekable in Chromium.
		// Honor the browser's actual Range requests, as the stream endpoint does.
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
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
	for (const item of items) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"]`)
			.getByRole('button', { name: new RegExp(`^${item.title}( — |$)`) }).click();
	}
	const audio = page.getByTestId('audio-player');
	if (failFirstLoad) await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.error?.code ?? 0)).not.toBe(0);
	else await expect(audio).toHaveJSProperty('readyState', 4);
	await expect(page.getByTestId('player-bar')).toBeVisible();
	if (!playing && !(await audio.evaluate((el: HTMLAudioElement) => el.paused))) {
		await togglePlayViaPlayerUi(page, audio);
	}
	if (playing && (await audio.evaluate((el: HTMLAudioElement) => el.paused))) {
		await page.getByTestId('player-play-pause').click();
	}
	await expect(audio).toHaveJSProperty('paused', !playing);
	return { audio, errors, requests: () => requests };
}

async function blurToBody(page: Page) {
	await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
	await expect.poll(() => page.evaluate(() => document.activeElement === document.body)).toBe(true);
}

for (const playing of [false, true]) {
	test(`native full screen and Escape preserve the actual ${playing ? 'playing' : 'paused'} decoder`, async ({ page }) => {
		const { audio, errors } = await openTransport(page, { playing });
		await seekViaPlayerUi(page, audio, 0.4);
		const original = (await audio.elementHandle())!;
		const before = await time(audio);
		await audio.evaluate((el: HTMLAudioElement) => {
			el.dataset.presentationCalls = '0';
			for (const name of ['play', 'pause', 'load'] as const) {
				const method = el[name].bind(el);
				Object.assign(el, { [name]: () => {
					el.dataset.presentationCalls = String(Number(el.dataset.presentationCalls) + 1);
					return method();
				} });
			}
		});
		const toggle = page.getByTestId('player-fullscreen');
		// Full screen is the expanded Now Playing view at every width.
		await page.getByTestId('player-now-playing-toggle').click();
		await expect(page.getByTestId('now-playing-route')).toBeVisible();
		await toggle.click();
		await expect.poll(() => page.evaluate(() => document.fullscreenElement?.getAttribute('data-testid'))).toBe('player-bar');
		await expect(toggle).toHaveAccessibleName('Exit full screen');
		await expect(page.getByTestId('player-bar').getByTestId('track-artwork')).toBeVisible();
		await page.getByRole('button', { name: 'Keyboard shortcuts', exact: true }).click();
		const help = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
		await expect(help).toBeVisible();
		expect(await help.evaluate((el) => document.fullscreenElement?.contains(el))).toBe(true);
		await page.keyboard.press('Escape');
		await expect(help).toHaveCount(0);
		await expect(toggle).toHaveAttribute('aria-pressed', 'true');
		await toggle.focus();
		await page.keyboard.press('Escape');
		await expect.poll(() => page.evaluate(() => document.fullscreenElement === null)).toBe(true);
		await expect(toggle).toHaveAccessibleName('Enter full screen');
		expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		await expect(audio).toHaveJSProperty('paused', !playing);
		await expect(audio).toHaveAttribute('data-presentation-calls', '0');
		if (playing) await expect.poll(() => time(audio)).toBeGreaterThan(before + 0.1);
		else expect(await time(audio)).toBeCloseTo(before, 2);
		expect(errors).toEqual([]);
	});
}

for (const width of [390, 1280]) {
	test(`full-screen Queue exits before opening its real ${width}px surface`, async ({ page }) => {
		await page.setViewportSize({ width, height: 844 });
		const { audio, errors } = await openTransport(page, { playing: false });
		const original = (await audio.elementHandle())!;
		await page.getByTestId('player-now-playing-toggle').click();
		await expect(page.getByTestId('now-playing-route')).toBeVisible();
		await page.getByTestId('player-fullscreen').click();
		await expect.poll(() => page.evaluate(() => !!document.fullscreenElement)).toBe(true);
		await page.getByTestId('player-queue-toggle').focus();
		await page.keyboard.press('Enter');
		await expect.poll(() => page.evaluate(() => document.fullscreenElement === null)).toBe(true);
		await expect(page.getByTestId('queue-panel')).toBeVisible();
		const host = page.getByTestId(width < 800 ? 'queue-route' : 'right-panel');
		await expect(host.getByRole('heading', { name: 'Queue', exact: true })
			.and(host.locator(width < 800 ? ':scope > h1' : ':scope > h2'))).toBeFocused();
		await page.getByRole('button', { name: 'Close queue', exact: true }).click();
		await expect(page.getByTestId('player-queue-toggle')).toBeFocused();
		expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		await expect(audio).toHaveJSProperty('paused', true);
		expect(errors).toEqual([]);
	});
}

test('full-screen controls stay visible without an idle layer', async ({ page }) => {
	await page.setViewportSize({ width: 1280, height: 900 });
	await page.emulateMedia({ reducedMotion: 'no-preference' });
	const { errors } = await openTransport(page, { playing: false });
	await page.getByTestId('player-now-playing-toggle').click();
	await expect(page.getByTestId('now-playing-route')).toBeVisible();
	await page.getByTestId('player-fullscreen').click();
	const player = page.getByTestId('player-bar');
	const controls = player.locator('.controls');
	await expect.poll(() => page.evaluate(() => !!document.fullscreenElement)).toBe(true);
	await blurToBody(page);
	// Well past the retired 3s idle timeout, controls remain fully visible.
	await page.waitForTimeout(3500);
	await expect(player).not.toHaveAttribute('data-cinema-idle', /.+/);
	await expect(controls).toHaveCSS('opacity', '1');
	await expect(page.getByTestId('player-play-pause')).toBeVisible();
	await page.evaluate(() => document.exitFullscreen());
	expect(errors).toEqual([]);
});

test('unsupported and refused full screen leave a usable transport and truthful recovery feedback', async ({ page }) => {
	await page.addInitScript(() => Object.defineProperty(document, 'fullscreenEnabled', { configurable: true, value: false }));
	const { audio, errors } = await openTransport(page, { playing: false });
	await page.getByTestId('player-now-playing-toggle').click();
	await expect(page.getByTestId('now-playing-route')).toBeVisible();
	await expect(page.getByTestId('player-fullscreen')).toBeDisabled();
	await expect(page.getByText('Full screen is unavailable in this browser.')).toBeVisible();
	await page.getByTestId('player-play-pause').click();
	await expect(audio).toHaveJSProperty('paused', false);
	expect(await page.evaluate(() => document.fullscreenElement)).toBeNull();
	expect(errors).toEqual([]);
});

test('a native full-screen rejection never changes playback and allows a real retry', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: false });
	await page.getByTestId('player-now-playing-toggle').click();
	await expect(page.getByTestId('now-playing-route')).toBeVisible();
	await page.getByTestId('player-bar').evaluate((el) => {
		const native = el.requestFullscreen;
		el.requestFullscreen = () => { el.requestFullscreen = native; return Promise.reject(new Error('Test refusal')); };
	});
	await page.getByTestId('player-fullscreen').click();
	await expect(page.getByText('Could not change full screen. Try again or use your browser controls.')).toBeVisible();
	expect(await page.evaluate(() => document.fullscreenElement)).toBeNull();
	await expect(audio).toHaveJSProperty('paused', true);
	await page.getByTestId('player-fullscreen').click();
	await expect.poll(() => page.evaluate(() => !!document.fullscreenElement)).toBe(true);
	await expect(page.getByText('Could not change full screen. Try again or use your browser controls.')).toHaveCount(0);
	await page.getByTestId('player-fullscreen').click();
	await expect(audio).toHaveJSProperty('paused', true);
	expect(errors).toEqual([]);
});

for (const appearance of ['light', 'dark'] as const) {
	for (const width of [320, 390, 1280]) {
		test(`full-screen presentation ${appearance} ${width}px has visible focus, reflow and reduced-motion controls`, async ({ page }) => {
			await page.setViewportSize({ width, height: 900 });
			await page.emulateMedia({ reducedMotion: 'reduce' });
			await page.addInitScript((value) => localStorage.setItem('nntune:appearance:v1', value), appearance);
			const { errors } = await openTransport(page, { playing: false });
			await page.getByTestId('player-now-playing-toggle').click();
			await expect(page.getByTestId('now-playing-route')).toBeVisible();
			await page.getByTestId('player-fullscreen').click();
			const player = page.getByTestId('player-bar');
			await expect.poll(() => page.evaluate(() => !!document.fullscreenElement)).toBe(true);
			await page.getByTestId('player-play-pause').focus();
			await page.keyboard.press('Tab');
			await expect(page.getByTestId('player-next')).toBeFocused();
			const ring = await page.getByTestId('player-next').evaluate((el) => {
				const css = getComputedStyle(el);
				return { visible: el.matches(':focus-visible'), width: parseFloat(css.outlineWidth), style: css.outlineStyle };
			});
			expect(ring.visible).toBe(true);
			expect(ring.width).toBeGreaterThanOrEqual(2);
			expect(ring.style).toBe('solid');
			await blurToBody(page);
			await expect(player).not.toHaveAttribute('data-cinema-idle', /.+/);
			await expect(player.locator('.controls')).toHaveCSS('opacity', '1');
			expect(await player.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
			expect((await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa']).analyze()).violations).toEqual([]);
			await page.screenshot({ path: `test-results/cinema-${appearance}-${width}.png` });
			await page.getByTestId('player-fullscreen').click();
			expect(errors).toEqual([]);
		});
	}
}

test('full-screen overflow-safe centering keeps long metadata and transport reachable at 320px', async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 568 });
	await page.emulateMedia({ reducedMotion: 'reduce' });
	const items = [{ ...ITEMS[0], title: 'LongTitle'.repeat(40), artist: 'LongArtist'.repeat(40) }];
	const { errors } = await openTransport(page, { playing: false, items });
	await page.getByTestId('player-now-playing-toggle').click();
	await expect(page.getByTestId('now-playing-route')).toBeVisible();
	await page.getByTestId('player-fullscreen').click();
	const player = page.getByTestId('player-bar');
	await expect.poll(() => page.evaluate(() => !!document.fullscreenElement)).toBe(true);
	await player.evaluate((el) => { el.scrollTop = 0; });
	const geometry = await player.evaluate((el) => ({
		top: el.getBoundingClientRect().top,
		controlsTop: el.querySelector('.controls')!.getBoundingClientRect().top,
		width: el.clientWidth, scrollWidth: el.scrollWidth
	}));
	expect(geometry.controlsTop).toBeGreaterThanOrEqual(geometry.top);
	expect(geometry.scrollWidth).toBeLessThanOrEqual(geometry.width + 1);
	await page.getByTestId('player-play-pause').scrollIntoViewIfNeeded();
	await expect(page.getByTestId('player-play-pause')).toBeInViewport();
	await page.getByTestId('player-fullscreen').scrollIntoViewIfNeeded();
	await page.getByTestId('player-fullscreen').click();
	expect(errors).toEqual([]);
});

for (const width of [390, 1280]) {
	test(`late full-screen exit cannot override browser Back at ${width}px`, async ({ page }) => {
		await page.setViewportSize({ width, height: 844 });
		const { audio, errors } = await openTransport(page, { playing: false });
		await page.locator('nav[aria-label="Primary"] a[href="/home"]:visible').click();
		await expect(page).toHaveURL(/\/home$/);
		await page.getByTestId('player-now-playing-toggle').click();
		await expect(page.getByTestId('now-playing-route')).toBeVisible();
		await page.getByTestId('player-fullscreen').click();
		await expect.poll(() => page.evaluate(() => !!document.fullscreenElement)).toBe(true);
		await page.evaluate(() => {
			const original = document.exitFullscreen.bind(document);
			document.exitFullscreen = () => new Promise<void>((resolve, reject) => {
				(window as unknown as { releaseCinema: () => void }).releaseCinema = () => {
					document.exitFullscreen = original;
					void original().then(resolve, reject);
				};
			});
		});
		await page.getByTestId('player-queue-toggle').click();
		await expect.poll(() => page.evaluate(() => typeof (window as unknown as { releaseCinema?: () => void }).releaseCinema)).toBe('function');
		await page.goBack();
		// Now Playing is a real route (C1), so Back lands on /home.
		await expect(page).toHaveURL(/\/home$/);
		await page.evaluate(() => (window as unknown as { releaseCinema: () => void }).releaseCinema());
		await expect.poll(() => page.evaluate(() => document.fullscreenElement === null)).toBe(true);
		await expect(page.getByTestId('player-queue-toggle')).toHaveAttribute('aria-expanded', 'false');
		await expect(page).toHaveURL(/\/home$/);
		await expect(page.getByTestId('queue-panel')).toHaveCount(0);
		await expect(audio).toHaveJSProperty('paused', true);
		expect(errors).toEqual([]);
	});
}

for (const width of [390, 1280]) {
	test(`pending navigation supersedes a late full-screen Queue exit at ${width}px`, async ({ page }) => {
		await page.setViewportSize({ width, height: 844 });
		const { audio, errors } = await openTransport(page, { playing: false });
		await page.getByTestId('player-now-playing-toggle').click();
		await expect(page.getByTestId('now-playing-route')).toBeVisible();
		await page.getByTestId('player-fullscreen').click();
		await expect.poll(() => page.evaluate(() => !!document.fullscreenElement)).toBe(true);
		await page.evaluate(() => {
			const original = document.exitFullscreen.bind(document);
			document.exitFullscreen = () => new Promise<void>((resolve, reject) => {
				(window as unknown as { releaseCinema: () => void }).releaseCinema = () => {
					document.exitFullscreen = original;
					void original().then(resolve, reject);
				};
			});
		});
		await page.getByTestId('player-queue-toggle').click();
		await expect.poll(() => page.evaluate(() => typeof (window as unknown as { releaseCinema?: () => void }).releaseCinema)).toBe('function');
		let release!: () => void;
		const gate = new Promise<void>((resolve) => { release = resolve; });
		let held = 0;
		await page.route('**/_app/immutable/**', async (route) => {
			if (route.request().resourceType() === 'script') { held += 1; await gate; }
			await route.continue();
		});
		try {
			// Start a real Kit navigation; keep its previously unvisited module pending.
			await page.locator('nav[aria-label="Primary"] a[href="/search"]:visible').evaluate((el: HTMLAnchorElement) => el.click());
			await expect.poll(() => held).toBeGreaterThan(0);
			await page.evaluate(() => (window as unknown as { releaseCinema: () => void }).releaseCinema());
			await expect.poll(() => page.evaluate(() => document.fullscreenElement === null)).toBe(true);
			await expect(page.getByTestId('player-queue-toggle')).toHaveAttribute('aria-expanded', 'false');
		} finally { release(); }
		await expect(page).toHaveURL(/\/search$/);
		await expect(page.getByTestId('queue-panel')).toHaveCount(0);
		await expect(audio).toHaveJSProperty('paused', true);
		expect(errors).toEqual([]);
	});
}

test('queue-action feedback survives navigation and an empty queue without duplicate live regions', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: false });
	const region = page.getByTestId('queue-announcement');
	await expect(region).toHaveCount(1);
	const original = (await region.elementHandle())!;
	await openQueue(page);
	await page.getByRole('button', { name: 'Remove Transport B from queue', exact: true }).click();
	await expect(region).toHaveText('Transport B removed from the queue.');
	await page.locator('nav[aria-label="Primary"] a[href="/home"]:visible').click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(region).toHaveText('Transport B removed from the queue.');
	expect(await original.evaluate((el) => el === document.querySelector('[data-testid="queue-announcement"]'))).toBe(true);
	await page.getByTestId('player-next').click();
	await expect(audio).toHaveAttribute('src', ITEMS[2].streamUrl);
	await expect(region).toHaveText('');
	await page.locator('nav[aria-label="Primary"] a[href="/library"]:visible').click();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(region).toHaveCount(1);
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await page.getByRole('button', { name: 'Yes, clear queue', exact: true }).click();
	await expect(audio).toHaveCount(0);
	await expect(region).toHaveText('Queue cleared.');
	await page.locator('nav[aria-label="Primary"] a[href="/home"]:visible').click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(region).toHaveText('Queue cleared.');
	expect(await original.evaluate((el) => el === document.querySelector('[data-testid="queue-announcement"]'))).toBe(true);
	expect(errors).toEqual([]);
});

test('a real natural end announces while Library is absent and remount never replays an obsolete automatic outcome', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: false });
	const first = (await audio.elementHandle())!;
	await first.evaluate((el: HTMLAudioElement) => {
		el.dataset.nativeEnd = '';
		el.addEventListener('ended', (event) => { el.dataset.nativeEnd = String(event.isTrusted); });
	});
	await page.locator('nav[aria-label="Primary"] a[href="/home"]:visible').click();
	await expect(page).toHaveURL(/\/home$/);
	const region = page.getByTestId('queue-announcement');
	const statusNode = (await region.elementHandle())!;
	const seek = page.getByTestId('player-seek');
	await seek.focus();
	await seek.press('End');
	for (let i = 0; i < 10; i++) await seek.press('ArrowLeft');
	await expect.poll(() => time(audio)).toBeCloseTo(59, 0);
	await page.getByTestId('player-play-pause').click();
	await expect(audio).toHaveAttribute('src', ITEMS[1].streamUrl);
	expect(await first.evaluate((el) => (el as HTMLElement).dataset.nativeEnd)).toBe('true');
	await expect(audio).toHaveJSProperty('paused', false);
	await expect.poll(() => time(audio)).toBeGreaterThan(0.1);
	await expect(region).toHaveText('Playing Transport B.');
	await page.locator('nav[aria-label="Primary"] a[href="/library"]:visible').click();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(region).toHaveCount(1);
	await openQueue(page);
	await page.getByRole('button', { name: 'Move Transport B up', exact: true }).click();
	await expect(region).toHaveText('Transport B moved to 1 of 3');
	await page.locator('nav[aria-label="Primary"] a[href="/home"]:visible').click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(region).toHaveText('Transport B moved to 1 of 3');
	await page.locator('nav[aria-label="Primary"] a[href="/library"]:visible').click();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(region).toHaveText('Transport B moved to 1 of 3');
	expect(await statusNode.evaluate((el) => el === document.querySelector('[data-testid="queue-announcement"]'))).toBe(true);
	expect(errors).toEqual([]);
});

test('custom play/pause toggles real playback and a paused range seek completes with a seeked event', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: true });
	const toggle = page.getByTestId('player-play-pause');
	await expect(toggle).toHaveAccessibleName('Pause');
	await expect.poll(() => time(audio)).toBeGreaterThan(0.2);

	await toggle.click();
	await expect(audio).toHaveJSProperty('paused', true);
	await expect(toggle).toHaveAccessibleName('Play');

	await audio.evaluate((el: HTMLAudioElement) => {
		(window as unknown as { __seeked: number }).__seeked = 0;
		el.addEventListener('seeked', () => { (window as unknown as { __seeked: number }).__seeked += 1; });
	});
	await seekViaPlayerUi(page, audio, 0.55);
	await expect.poll(() => page.evaluate(() => (window as unknown as { __seeked: number }).__seeked)).toBeGreaterThan(0);
	const state = await audio.evaluate((el: HTMLAudioElement) => ({ currentTime: el.currentTime, duration: el.duration, paused: el.paused }));
	expect(seekProofFailure(state, 0.55, true)).toBeNull();
	await expect.poll(() => page.getByTestId('player-seek').evaluate((el: HTMLInputElement) => el.valueAsNumber))
		.toBeCloseTo(state.currentTime, 0);

	await toggle.click();
	await expect(audio).toHaveJSProperty('paused', false);
	await expect.poll(() => time(audio)).toBeGreaterThan(state.currentTime + 0.2);
	expect(await time(audio)).toBeGreaterThan(state.currentTime - 3);
	expect(errors).toEqual([]);
});

for (const playing of [false, true]) {
	for (const gesture of ['button', 'shortcut'] as const) {
		test(`persistent ${gesture} restart preserves real ${playing ? 'playing' : 'paused'} media across client navigation`, async ({ page }) => {
			const { audio, errors } = await openTransport(page, { playing });
			const original = (await audio.elementHandle())!;
			await seekViaPlayerUi(page, audio, 0.4);
			await expect.poll(() => time(audio)).toBeGreaterThan(20);
			await page.locator('nav[aria-label="Primary"] a[href="/home"]:visible').click();
			await expect(page).toHaveURL(/\/home$/);
			await expect(page.getByTestId('library-item')).toHaveCount(0);
			await expect(page.getByRole('region', { name: 'Player', exact: true }).getByTestId('library-now-playing')).toHaveText('Transport A — Test artist');
			await expect(audio).toHaveCount(1);
			await expect(audio).toHaveJSProperty('paused', !playing);
			expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
			await original.evaluate((el: HTMLAudioElement) => {
				const native = { play: el.play, pause: el.pause, load: el.load };
				el.dataset.calls = '0';
				el.dataset.seeked = '0';
				const count = () => { el.dataset.calls = String(Number(el.dataset.calls) + 1); };
				el.play = () => { count(); return native.play.call(el); };
				el.pause = () => { count(); native.pause.call(el); };
				el.load = () => { count(); native.load.call(el); };
				el.addEventListener('seeked', () => { el.dataset.seeked = String(Number(el.dataset.seeked) + 1); });
			});
			if (gesture === 'button') await page.getByTestId('library-restart').click();
			else { await blurToBody(page); await page.keyboard.press('0'); }
			await expect.poll(() => audio.getAttribute('data-seeked')).toBe('1');
			await expect(page.getByTestId('library-restart-status')).toHaveText('Restarted Transport A.');
			expect(await time(audio)).toBeLessThan(2);
			await expect(audio).toHaveJSProperty('paused', !playing);
			await expect(audio).toHaveAttribute('data-calls', '0');
			await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
			if (playing) await expect.poll(() => time(audio)).toBeGreaterThan(0.2);
			else {
				const position = await time(audio);
				await page.waitForTimeout(300);
				expect(await time(audio)).toBe(position);
			}
			await page.locator('nav[aria-label="Primary"] a[href="/library"]:visible').click();
			await openQueue(page);
			await expect(page.getByTestId('queue-item')).toHaveCount(3);
			await expect(page.getByTestId('now-playing')).toContainText('Transport A');
			expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
			await expect(audio).toHaveJSProperty('paused', !playing);
			await expect(audio).toHaveAttribute('data-calls', '0');
			expect(errors).toEqual([]);
		});
	}
}

for (const playing of [false, true]) {
	test(`persistent Next and now-playing keep real ${playing ? 'playing' : 'paused'} intent outside Library`, async ({ page }) => {
		const { audio, errors } = await openTransport(page, { playing });
		await expect(page.getByTestId('now-playing')).toHaveCount(1);
		await expect(page.getByTestId('player-next')).toHaveCount(1);
		const announcement = page.getByTestId('now-playing');
		await expect(announcement).toHaveAttribute('role', 'status');
		await expect(announcement).toHaveText('Now playing: Transport A — Test artist');
		const statusNode = (await announcement.elementHandle())!;
		const first = (await audio.elementHandle())!;
		await page.locator('nav[aria-label="Primary"] a[href="/home"]:visible').click();
		await expect(page).toHaveURL(/\/home$/);
		await expect(page.getByTestId('library-item')).toHaveCount(0);
		await expect(announcement).toHaveText('Now playing: Transport A — Test artist');
		expect(await statusNode.evaluate((el) => el === document.querySelector('[data-testid="now-playing"]'))).toBe(true);

		await page.getByTestId('player-next').click();
		await expect(audio).toHaveAttribute('src', ITEMS[1].streamUrl);
		await expect(audio).toHaveJSProperty('readyState', 4);
		await expect(audio).toHaveJSProperty('paused', !playing);
		await expect(announcement).toHaveText('Now playing: Transport B — Test artist');
		expect(await first.evaluate((el) => el.isConnected)).toBe(false);
		if (playing) await expect.poll(() => time(audio)).toBeGreaterThan(0.2);
		else {
			const position = await time(audio);
			await page.waitForTimeout(300);
			expect(await time(audio)).toBe(position);
			expect(position).toBe(0);
		}

		await page.locator('nav[aria-label="Primary"] a[href="/library"]:visible').click();
		await openQueue(page);
		await expect(page.getByTestId('queue-item')).toHaveCount(3);
		await expect(page.getByTestId('now-playing')).toHaveCount(1);
		await expect(page.getByTestId('player-next')).toHaveCount(1);
		await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute('data-track-id', ITEMS[1].id);
		await page.getByTestId('player-next').click();
		await expect(audio).toHaveAttribute('src', ITEMS[2].streamUrl);
		await expect(audio).toHaveJSProperty('readyState', 4);
		await expect(audio).toHaveJSProperty('paused', !playing);
		const last = (await audio.elementHandle())!;
		await page.getByTestId('player-next').click();
		await expect(announcement).toHaveText('Now playing: Transport C — Test artist');
		expect(await last.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		await expect(audio).toHaveJSProperty('paused', !playing);
		expect(errors).toEqual([]);
	});
}

test('persistent now-playing wraps unbroken metadata at mobile and desktop widths', async ({ page }) => {
	const title = 'LongTitle'.repeat(40);
	const artist = 'LongArtist'.repeat(40);
	const items = ITEMS.map((item, index) => index === 0 ? { ...item, title, artist } : item);
	const { errors } = await openTransport(page, { playing: false, items });
	const announcement = page.getByTestId('now-playing');
	await expect(announcement).toHaveText(`Now playing: ${title} — ${artist}`);
	for (const width of [320, 390, 1280]) {
		await page.setViewportSize({ width, height: 900 });
		expect(await announcement.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
	}
	expect(errors).toEqual([]);
});

test('arrow keys on focused sliders stay native and are not stolen by global shortcuts', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: false });
	const seek = page.getByTestId('player-seek');
	const volume = page.getByTestId('player-volume');
	await page.evaluate(() => {
		const w = window as unknown as { __prevented: boolean[] };
		w.__prevented = [];
		document.addEventListener('keydown', (event) => {
			// Read after every window-level handler has run.
			setTimeout(() => w.__prevented.push(event.defaultPrevented), 0);
		});
	});

	await seek.focus();
	const before = await time(audio);
	await page.keyboard.press('ArrowRight');
	await expect.poll(() => time(audio)).toBeGreaterThan(before);
	// Native range step (0.1 s), never the 10 s global seek.
	expect(await time(audio)).toBeLessThan(before + 2);
	await expect(audio).toHaveJSProperty('paused', true);

	await volume.focus();
	await audio.evaluate((el: HTMLAudioElement) => { el.volume = 1; });
	await expect(volume).toHaveJSProperty('valueAsNumber', 100);
	await page.keyboard.press('ArrowLeft');
	await expect(volume).toHaveJSProperty('valueAsNumber', 95);
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.volume)).toBeCloseTo(0.95, 2);
	await expect(audio).toHaveJSProperty('paused', true);

	await expect.poll(() => page.evaluate(() => (window as unknown as { __prevented: boolean[] }).__prevented.length)).toBe(2);
	expect(await page.evaluate(() => (window as unknown as { __prevented: boolean[] }).__prevented)).toEqual([false, false]);
	expect(errors).toEqual([]);
});

test('volume, mute and playback speed keep a paused track paused at the same position', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: true });
	await expect.poll(() => time(audio)).toBeGreaterThan(0.2);
	await page.getByTestId('player-play-pause').click();
	await expect(audio).toHaveJSProperty('paused', true);
	const position = await time(audio);

	const volume = page.getByTestId('player-volume');
	await volume.focus();
	await page.keyboard.press('ArrowLeft');
	await page.keyboard.press('ArrowLeft');
	await expect(volume).toHaveJSProperty('valueAsNumber', 90);
	await expect.poll(() => audio.evaluate((el: HTMLAudioElement) => el.volume)).toBeCloseTo(0.9, 2);
	await expect(audio).toHaveJSProperty('paused', true);

	await page.getByRole('button', { name: 'Mute', exact: true }).click();
	await expect(audio).toHaveJSProperty('muted', true);
	await expect(page.getByRole('button', { name: 'Unmute', exact: true })).toHaveAttribute('aria-pressed', 'true');
	await expect(audio).toHaveJSProperty('paused', true);
	await page.getByRole('button', { name: 'Unmute', exact: true }).click();
	await expect(audio).toHaveJSProperty('muted', false);
	await expect(audio).toHaveJSProperty('paused', true);

	await page.getByText('Playback', { exact: true }).click();
	await page.getByLabel('Playback speed').selectOption('1.5');
	await expect(audio).toHaveJSProperty('playbackRate', 1.5);
	await expect(audio).toHaveJSProperty('paused', true);

	await page.waitForTimeout(600);
	await expect(audio).toHaveJSProperty('paused', true);
	expect(await time(audio)).toBe(position);
	await expect(page.getByTestId('player-play-pause')).toHaveAccessibleName('Play');
	expect(errors).toEqual([]);
});

test('k, n, p, j and l drive real playback from the body and are suppressed while typing', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: false });
	await blurToBody(page);

	await page.keyboard.press('k');
	await expect(audio).toHaveJSProperty('paused', false);
	await page.keyboard.press('k');
	await expect(audio).toHaveJSProperty('paused', true);

	const start = await time(audio);
	await page.keyboard.press('l');
	await expect.poll(() => time(audio)).toBeGreaterThanOrEqual(start + 9.5);
	expect(await time(audio)).toBeLessThanOrEqual(start + 11);
	await expect(audio).toHaveJSProperty('paused', true);
	await page.keyboard.press('j');
	await expect.poll(() => time(audio)).toBeLessThan(start + 1);
	await expect(audio).toHaveJSProperty('paused', true);

	await page.keyboard.press('n');
	await expect(audio).toHaveAttribute('src', ITEMS[1].streamUrl);
	await expect(audio).toHaveJSProperty('readyState', 4);
	await expect(audio).toHaveJSProperty('paused', true);
	await page.keyboard.press('p');
	await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
	await expect(audio).toHaveJSProperty('readyState', 4);
	await expect(audio).toHaveJSProperty('paused', true);

	// Typing in the library search box must not trigger any shortcut.
	const search = page.locator('input[type="search"]').first();
	await search.click();
	await page.keyboard.type('knpjl?');
	await expect(search).toHaveValue('knpjl?');
	await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
	await expect(audio).toHaveJSProperty('paused', true);
	await expect(page.getByRole('dialog')).toHaveCount(0);
	expect(await time(audio)).toBeLessThan(start + 1);
	expect(errors).toEqual([]);
});

test('the ? help dialog is modal, suppresses shortcuts, and Escape returns focus to the opener', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: false });
	const toggle = page.getByTestId('player-play-pause');
	await toggle.focus();
	await page.keyboard.press('?');
	const dialog = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
	await expect(dialog).toBeVisible();
	await expect.poll(() => dialog.evaluate((el: HTMLDialogElement) => el.open && el.matches(':modal'))).toBe(true);

	await page.keyboard.press('k');
	await page.keyboard.press('n');
	await page.waitForTimeout(300);
	await expect(audio).toHaveJSProperty('paused', true);
	await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);

	await page.keyboard.press('Escape');
	await expect(dialog).toHaveCount(0);
	await expect(toggle).toBeFocused();

	// Same round trip through the visible button and its Close control.
	const opener = page.getByRole('button', { name: 'Keyboard shortcuts', exact: true });
	await opener.click();
	await expect(dialog).toBeVisible();
	await page.getByRole('button', { name: 'Close keyboard shortcuts' }).click();
	await expect(dialog).toHaveCount(0);
	await expect(opener).toBeFocused();
	expect(errors).toEqual([]);
});

test('Retry playback refetches a failed resource and resumes decoded media without losing speed', async ({ page }) => {
	await page.addInitScript(() => localStorage.setItem('nntune:playback-speed:v1', '1.5'));
	const { audio, errors, requests } = await openTransport(page, { playing: false, failFirstLoad: true });
	await expect(audio).toHaveJSProperty('playbackRate', 1.5);
	const original = (await audio.elementHandle())!;
	await expect(page.getByRole('alert')).toContainText('Could not play this track');
	expect(requests()).toBe(1);
	await page.getByRole('button', { name: 'Retry playback' }).click();
	await expect(audio).toHaveJSProperty('readyState', 4);
	await expect(audio).toHaveJSProperty('error', null);
	await expect(audio).toHaveJSProperty('paused', false);
	await expect.poll(() => time(audio)).toBeGreaterThan(0.2);
	expect(requests()).toBeGreaterThan(1);
	await expect(audio).toHaveJSProperty('playbackRate', 1.5);
	expect(await page.evaluate(() => localStorage.getItem('nntune:playback-speed:v1'))).toBe('1.5');
	expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
	await expect(page.getByRole('button', { name: 'Retry playback' })).toHaveCount(0);
	expect(errors).toEqual([]);
});

test('sleep controls do not start playback and end-of-track sleep fences automatic advancement', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: false });
	await page.getByText('Playback', { exact: true }).click();
	const sleep = page.getByLabel('Sleep timer', { exact: true });
	await sleep.selectOption('15');
	await expect(page.getByText('Sleep in 15 minutes', { exact: true })).toBeVisible();
	await expect(audio).toHaveJSProperty('paused', true);
	await sleep.selectOption('off');
	await expect(page.getByText('Sleep timer off', { exact: true })).toBeVisible();

	await seekViaPlayerUi(page, audio, 0.98);
	await expect.poll(() => time(audio)).toBeGreaterThan(57);
	await sleep.selectOption('end');
	await expect(page.getByText('Sleep at end of track', { exact: true })).toBeVisible();
	await page.getByTestId('player-play-pause').click();
	await expect(audio).toHaveJSProperty('ended', true);
	await expect(audio).toHaveJSProperty('paused', true);
	await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
	await expect(page.getByTestId('player-play-pause')).toHaveAccessibleName('Play');
	await expect(page.getByText('Sleep timer off', { exact: true })).toBeVisible();
	expect(errors).toEqual([]);
});

for (const width of [1280, 390, 320]) {
	test(`populated transport and help have no serious accessibility violations at ${width}px`, async ({ page }) => {
		await page.setViewportSize({ width, height: 800 });
		const { errors } = await openTransport(page, { playing: false });
		// On compact widths the full controls live in the Now Playing view (C3).
		if (width < 800) {
			await page.getByTestId('player-now-playing-toggle').click();
			await expect(page.getByTestId('now-playing-route')).toBeVisible();
		}
		await page.getByTestId('player-bar').scrollIntoViewIfNeeded();
		const bar = page.getByTestId('player-bar');
		expect(await bar.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
		const blocking = async () => (await new AxeBuilder({ page }).analyze()).violations
			.filter((violation) => violation.impact === 'serious' || violation.impact === 'critical');
		expect(await blocking()).toEqual([]);
		await page.getByRole('button', { name: 'Keyboard shortcuts', exact: true }).click();
		await expect(page.getByRole('dialog', { name: 'Keyboard shortcuts' })).toBeVisible();
		expect(await blocking()).toEqual([]);
		await page.keyboard.press('Escape');
		expect(errors).toEqual([]);
	});
}

for (const theme of ['light', 'dark'] as const) {
	for (const width of [320, 390, 1280]) {
		test(`kit transport stencils preserve live state and focus in ${theme} at ${width}px`, async ({ page }) => {
			await page.setViewportSize({ width, height: 844 });
			await page.addInitScript((appearance) => localStorage.setItem('nntune:appearance:v1', appearance), theme);
			const { audio, errors } = await openTransport(page, { playing: false });
			await expect(page.locator('html')).toHaveAttribute('data-appearance', theme);
			// On compact widths the stencil controls live in the Now Playing view (C3).
			if (width < 800) {
				await page.getByTestId('player-now-playing-toggle').click();
				await expect(page.getByTestId('now-playing-route')).toBeVisible();
			}
			const bar = page.getByTestId('player-bar');
			async function stencil(button: Locator, name: IconName) {
				const svg = button.locator('svg');
				await expect(svg).toBeVisible();
				await expect(svg).toHaveAttribute('viewBox', '0 0 24 24');
				await expect(svg).toHaveAttribute('width', '24');
				await expect(svg).toHaveAttribute('height', '24');
				await expect(svg).toHaveAttribute('aria-hidden', 'true');
				await expect(svg).toHaveAttribute('stroke', 'currentColor');
				await expect(svg).toHaveAttribute('stroke-width', '2');
				await expect(svg).not.toHaveAttribute('role');
				await expect(svg).not.toHaveAttribute('aria-label');
				await expect.poll(() => svg.evaluate((node, path) => {
					const expected = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
					expected.innerHTML = path;
					return node.innerHTML.replace(/<!--[\s\S]*?-->/g, '') === expected.innerHTML;
				}, ICON_PATHS[name])).toBe(true);
			}
			await stencil(bar.getByRole('button', { name: 'Shuffle upcoming tracks', exact: true }), 'shuffle');
			await stencil(bar.getByRole('button', { name: 'Previous track', exact: true }), 'skip-back');
			await stencil(page.getByTestId('player-next'), 'skip-forward');
			const toggle = page.getByTestId('player-play-pause');
			const original = (await toggle.elementHandle())!;
			await stencil(toggle, 'play');
			await toggle.focus();
			await page.keyboard.press('Enter');
			await expect(audio).toHaveJSProperty('paused', false);
			await expect(toggle).toHaveAccessibleName('Pause');
			await stencil(toggle, 'pause');
			await expect(toggle).toBeFocused();
			await page.keyboard.press('Enter');
			await expect(audio).toHaveJSProperty('paused', true);
			await stencil(toggle, 'play');
			expect(await original.evaluate((node) => node === document.querySelector('[data-testid="player-play-pause"]'))).toBe(true);
			for (const [mode, icon] of [['Off', 'repeat'], ['Queue', 'repeat'], ['One', 'repeat-1']] as const) {
				const repeat = bar.getByRole('button', { name: `Repeat: ${mode}`, exact: true });
				await stencil(repeat, icon);
				await expect(repeat).toHaveAttribute('aria-pressed', String(mode !== 'Off'));
				await repeat.click();
			}
			await expect(bar.getByRole('button', { name: 'Repeat: Off', exact: true })).toHaveAttribute('aria-pressed', 'false');
			const mute = bar.getByRole('button', { name: 'Mute', exact: true });
			await stencil(mute, 'volume-2');
			await mute.click();
			const unmute = bar.getByRole('button', { name: 'Unmute', exact: true });
			await expect(unmute).toHaveAttribute('aria-pressed', 'true');
			await stencil(unmute, 'volume-x');
			await unmute.click();
			await stencil(mute, 'volume-2');
			await expect(audio).toHaveJSProperty('paused', true);
			expect(errors).toEqual([]);
		});
	}
}

test('controls keep driving the same element on /home and an auth transition removes player and help', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: true });
	const original = (await audio.elementHandle())!;
	await page.locator('nav[aria-label="Primary"] a[href="/home"]:visible').click();
	await expect(page).toHaveURL(/\/home$/);
	await expect(page.getByRole('heading', { name: 'Home', exact: true })).toBeVisible();
	await expect(page.getByTestId('player-bar')).toBeVisible();
	await expect(audio).toHaveJSProperty('paused', false);

	const toggle = page.getByTestId('player-play-pause');
	await toggle.click();
	await expect(audio).toHaveJSProperty('paused', true);
	await expect(toggle).toHaveAccessibleName('Play');
	await toggle.click();
	await expect(audio).toHaveJSProperty('paused', false);
	const before = await time(audio);
	await seekViaPlayerUi(page, audio, 0.5);
	await expect.poll(() => time(audio)).toBeGreaterThan(before + 10);
	expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
	await expect(page.locator('audio')).toHaveCount(1);

	await blurToBody(page);
	await page.keyboard.press('?');
	await expect(page.getByRole('dialog', { name: 'Keyboard shortcuts' })).toBeVisible();

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'transport-change');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await expect(page.getByTestId('player-bar')).toHaveCount(0);
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(audio).toHaveCount(0);
	expect(await original.evaluate((el: HTMLAudioElement) => el.paused)).toBe(true);
	await otherTab.close();
	expect(errors).toEqual([]);
});

// ---- Current-track heart (player-favorite) -------------------------------

type Watched = HTMLAudioElement & { heartLog: string[] };

const heartOf = (page: Page) => page.getByTestId('player-favorite');
const primaryNav = async (page: Page, path: 'home' | 'library') => {
	await page.locator(`nav[aria-label="Primary"] a[href="/${path}"]:visible`).click();
	await expect(page).toHaveURL(new RegExp(`/${path}$`));
};

/** Observe, never emit: record imperative transport calls and native media events on the real node. */
async function watchAudio(audio: Locator) {
	await audio.evaluate((el: HTMLAudioElement) => {
		const log: string[] = [];
		const node = el as Watched;
		node.heartLog = log;
		const native = { play: el.play, pause: el.pause, load: el.load };
		el.play = () => { log.push('call:play'); return native.play.call(el); };
		el.pause = () => { log.push('call:pause'); native.pause.call(el); };
		el.load = () => { log.push('call:load'); native.load.call(el); };
		for (const name of ['play', 'pause', 'seeking', 'seeked', 'emptied', 'loadstart']) {
			el.addEventListener(name, () => log.push(`event:${name}`));
		}
	});
}
const heartLog = (audio: Locator) => audio.evaluate((el) => (el as Watched).heartLog);

/** Compare a button's rendered glyph with the approved kit path, as the stencil test does. */
async function heartGlyph(button: Locator, name: IconName) {
	const svg = button.locator('svg');
	await expect(svg).toBeVisible();
	await expect(svg).toHaveAttribute('viewBox', '0 0 24 24');
	await expect(svg).toHaveAttribute('aria-hidden', 'true');
	await expect.poll(() => svg.evaluate((node, path) => {
		const expected = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
		expected.innerHTML = path;
		return node.innerHTML.replace(/<!--[\s\S]*?-->/g, '') === expected.innerHTML;
	}, ICON_PATHS[name])).toBe(true);
}

async function expectHeart(page: Page, saved: boolean) {
	const heart = heartOf(page);
	await expect(heart).toHaveAccessibleName(saved ? 'Remove from Favorites' : 'Save to Favorites');
	await expect(heart).toHaveAttribute('aria-pressed', String(saved));
	await heartGlyph(heart, saved ? 'heart-filled' : 'heart');
}

const noHorizontalOverflow = (page: Page) =>
	page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth);

for (const playing of [false, true]) {
	test(`current-track heart toggles shared Favorites on the same real ${playing ? 'playing' : 'paused'} media and keeps keyboard focus`, async ({ page }) => {
		const { audio, errors } = await openTransport(page, { playing });
		const original = (await audio.elementHandle())!;
		const heart = heartOf(page);
		const region = page.getByTestId('queue-announcement');
		await audio.evaluate((el) => {
			el.setAttribute('data-heart-setup-seeked', 'false');
			el.addEventListener('seeked', () => el.setAttribute('data-heart-setup-seeked', 'true'), { once: true });
		});
		await seekViaPlayerUi(page, audio, 0.4);
		await expect(audio).toHaveAttribute('data-heart-setup-seeked', 'true');
		await expect(audio).toHaveJSProperty('seeking', false);
		await expect.poll(() => time(audio)).toBeGreaterThan(20);
		await watchAudio(audio);
		await expectHeart(page, false);
		const before = await time(audio);

		await heart.focus();
		await page.keyboard.press('Enter');
		await expectHeart(page, true);
		await expect(heart).toBeFocused();
		await expect(region).toHaveText('Saved Transport A to Favorites.');
		await expect(audio).toHaveJSProperty('paused', !playing);
		await expect(audio).toHaveAttribute('src', ITEMS[0].streamUrl);
		if (playing) await expect.poll(() => time(audio)).toBeGreaterThan(before);
		else {
			await page.waitForTimeout(300);
			expect(await time(audio)).toBe(before);
		}
		expect(await time(audio)).toBeGreaterThan(20);
		expect(await heartLog(audio)).toEqual([]);

		await primaryNav(page, 'home');
		expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		await expectHeart(page, true);
		await expect(audio).toHaveJSProperty('paused', !playing);
		await primaryNav(page, 'library');
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await expectHeart(page, true);
		const libraryA = page.locator('[data-testid="library-item"][data-id="transport-a"]');
		await expect(libraryA.getByRole('button', { name: 'Unstar Transport A', exact: true })).toHaveAttribute('aria-pressed', 'true');
		await expect(page.getByTestId('favorite-item')).toHaveCount(1);
		await openQueue(page);
		const queuedA = page.locator('[data-testid="queue-item"][data-track-id="transport-a"]');
		await expect(queuedA.getByRole('button', { name: 'Unstar Transport A', exact: true })).toHaveAttribute('aria-pressed', 'true');

		// A queue-row star for another track never touches the current-track heart.
		await page.locator('[data-testid="queue-item"][data-track-id="transport-b"]')
			.getByRole('button', { name: 'Star Transport B', exact: true }).click();
		await expectHeart(page, true);
		// Unstarring the current track from its queue row is reflected by the heart.
		await queuedA.getByRole('button', { name: 'Unstar Transport A', exact: true }).click();
		await expectHeart(page, false);
		await expect(libraryA.getByRole('button', { name: 'Star Transport A', exact: true })).toHaveAttribute('aria-pressed', 'false');
		// ... and a library star is too.
		await libraryA.getByRole('button', { name: 'Star Transport A', exact: true }).click();
		await expectHeart(page, true);
		await expect(queuedA.getByRole('button', { name: 'Unstar Transport A', exact: true })).toHaveAttribute('aria-pressed', 'true');

		// Unstar from the heart while the Library shows Favorites only: focus stays on the heart.
		await page.locator('#library-view').selectOption('favorites');
		await expect(libraryA).toHaveCount(1);
		await heart.focus();
		await page.keyboard.press('Space');
		await expectHeart(page, false);
		await expect(region).toHaveText('Removed Transport A from Favorites.');
		await expect(heart).toBeFocused();
		await expect(libraryA).toHaveCount(0);
		await expect(heart).toBeFocused();
		await expect(audio).toHaveJSProperty('paused', !playing);
		expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		expect(await heartLog(audio)).toEqual([]);
		expect(errors).toEqual([]);
	});
}

test('Next moves heart membership to the actual selected track', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: false });
	await heartOf(page).click();
	await expectHeart(page, true);
	await page.getByTestId('player-next').click();
	await expect(audio).toHaveAttribute('src', ITEMS[1].streamUrl);
	await expectHeart(page, false);
	await heartOf(page).click();
	await expect(page.getByTestId('queue-announcement')).toHaveText('Saved Transport B to Favorites.');
	await expectHeart(page, true);
	await page.getByTestId('player-next').click();
	await expect(audio).toHaveAttribute('src', ITEMS[2].streamUrl);
	await expectHeart(page, false);
	await primaryNav(page, 'home');
	await primaryNav(page, 'library');
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await expect.poll(() => page.getByTestId('favorite-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id'))))
		.toEqual(['transport-a', 'transport-b']);
	await expectHeart(page, false);
	expect(errors).toEqual([]);
});

test('refused Favorites writes keep the heart state for the session across navigation', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: false });
	await page.evaluate(() => {
		const write = Storage.prototype.setItem;
		Storage.prototype.setItem = function (key, value) {
			if (key.startsWith('nntune.library.favorites.v1')) throw new DOMException('Refused', 'QuotaExceededError');
			return write.call(this, key, value);
		};
	});
	await heartOf(page).click();
	await expectHeart(page, true);
	await expect(page.getByTestId('queue-announcement')).toHaveText('Saved Transport A to Favorites.');
	expect(await page.evaluate(() => Object.keys(localStorage).filter((key) => key.startsWith('nntune.library.favorites.v1')))).toEqual([]);
	await primaryNav(page, 'home');
	await expectHeart(page, true);
	await primaryNav(page, 'library');
	await expectHeart(page, true);
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(page.locator('[data-testid="library-item"][data-id="transport-a"]')
		.getByRole('button', { name: 'Unstar Transport A', exact: true })).toHaveAttribute('aria-pressed', 'true');
	await expect(audio).toHaveJSProperty('paused', true);
	expect(errors).toEqual([]);
});

for (const refuseStorage of [false, true]) {
	test(`an auth boundary removes the heart, audio and ${refuseStorage ? 'memory-only' : 'persisted'} membership`, async ({ page }) => {
		const { audio, errors } = await openTransport(page, { playing: true });
		const context = await page.evaluate(() => sessionStorage.getItem('nntune.library.auth-context.v1'));
		expect(context).not.toBeNull();
		if (refuseStorage) await page.evaluate(() => {
			const write = Storage.prototype.setItem;
			Storage.prototype.setItem = function (key, value) {
				if (key.startsWith('nntune.library.favorites.v1')) throw new DOMException('Refused', 'QuotaExceededError');
				return write.call(this, key, value);
			};
		});
		await heartOf(page).click();
		await expectHeart(page, true);
		await primaryNav(page, 'home');
		const other = await page.context().newPage();
		await other.goto('/login');
		await other.evaluate(() => {
			localStorage.setItem('nntune.library.auth-transition.v1', 'heart-auth-change');
			localStorage.removeItem('nntune.library.auth-transition.v1');
		});
		await expect(heartOf(page)).toHaveCount(0);
		await expect(page.getByTestId('player-bar')).toHaveCount(0);
		await expect(audio).toHaveCount(0);
		expect(await page.evaluate((context) => localStorage.getItem(`nntune.library.favorites.v1.${context}`), context)).toBeNull();
		await other.close();
		await primaryNav(page, 'library');
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await expect(page.getByTestId('favorite-item')).toHaveCount(0);
		await expect(page.locator('[data-testid="library-item"][data-id="transport-a"]')
			.getByRole('button', { name: 'Star Transport A', exact: true })).toHaveAttribute('aria-pressed', 'false');
		await expect(heartOf(page)).toHaveCount(0);
		expect(errors).toEqual([]);
	});
}

for (const theme of ['light', 'dark'] as const) {
	for (const width of [320, 390, 1280]) {
		test(`current-track heart exposes name, state and approved glyph in ${theme} at ${width}px without overflow`, async ({ page }, info) => {
			await page.setViewportSize({ width, height: 844 });
			await page.addInitScript((appearance) => localStorage.setItem('nntune:appearance:v1', appearance), theme);
			const { audio, errors } = await openTransport(page, { playing: false });
			await expect(page.locator('html')).toHaveAttribute('data-appearance', theme);
			const heart = heartOf(page);
			await expect(heart).toBeVisible();
			await expectHeart(page, false);
			expect(await noHorizontalOverflow(page)).toBe(true);
			await heart.focus();
			await page.keyboard.press('Enter');
			await expectHeart(page, true);
			await expect(heart).toBeFocused();
			const box = (await heart.boundingBox())!;
			expect(box.x).toBeGreaterThanOrEqual(0);
			expect(box.x + box.width).toBeLessThanOrEqual(width);
			expect(box.width).toBeGreaterThanOrEqual(44);
			expect(box.height).toBeGreaterThanOrEqual(44);
			expect(await heart.evaluate((node) => node.matches(':focus-visible'))).toBe(true);
			expect(await noHorizontalOverflow(page)).toBe(true);
			await info.attach(`current-track-heart-${theme}-${width}`, { body: await page.screenshot(), contentType: 'image/png' });
			const audit = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21aa']).analyze();
			expect(audit.violations).toEqual([]);
			await page.keyboard.press('Enter');
			await expectHeart(page, false);
			await expect(heart).toBeFocused();
			await expect(audio).toHaveJSProperty('paused', true);
			expect(errors).toEqual([]);
		});
	}
}

for (const playing of [false, true]) {
	test(`f saves the actual current track across navigation without disturbing real ${playing ? 'playing' : 'paused'} media`, async ({ page }) => {
		const { audio, errors, requests } = await openTransport(page, { playing });
		const original = (await audio.elementHandle())!;
		if (playing) await expect.poll(() => time(audio)).toBeGreaterThan(0.2);
		await watchAudio(audio);
		const before = await time(audio);
		const beforeRequests = requests();
		const heart = heartOf(page);
		await heart.focus();
		await page.keyboard.press('f');
		await expectHeart(page, true);
		await expect(heart).toBeFocused();
		await expect(page.getByTestId('queue-announcement')).toHaveText('Saved Transport A to Favorites.');
		await page.keyboard.press('F');
		await expectHeart(page, false);
		await expect(heart).toBeFocused();
		await expect(page.getByTestId('queue-announcement')).toHaveText('Removed Transport A from Favorites.');
		await expect(audio).toHaveJSProperty('paused', !playing);
		if (playing) await expect.poll(() => time(audio)).toBeGreaterThan(before);
		else expect(await time(audio)).toBe(before);
		expect(await heartLog(audio)).toEqual([]);
		expect(requests()).toBe(beforeRequests);

		await primaryNav(page, 'home');
		await blurToBody(page);
		await page.keyboard.press('f');
		await expectHeart(page, true);
		expect(await original.evaluate((el) => el === document.querySelector('[data-testid="audio-player"]'))).toBe(true);
		await expect(audio).toHaveJSProperty('paused', !playing);
		expect(await heartLog(audio)).toEqual([]);
		await primaryNav(page, 'library');
		await expect(page.getByTestId('favorite-item')).toHaveCount(1);
		await expect(page.locator('[data-testid="library-item"][data-id="transport-a"]')
			.getByRole('button', { name: 'Unstar Transport A', exact: true })).toHaveAttribute('aria-pressed', 'true');
		await page.getByTestId('player-next').click();
		await expect(audio).toHaveAttribute('src', ITEMS[1].streamUrl);
		await expect(audio).toHaveJSProperty('readyState', 4);
		await expectHeart(page, false);
		await blurToBody(page);
		await page.keyboard.press('f');
		await expectHeart(page, true);
		await expect(page.getByTestId('queue-announcement')).toHaveText('Saved Transport B to Favorites.');
		await expect(page.getByTestId('favorite-item')).toHaveCount(2);
		await expect(audio).toHaveJSProperty('paused', !playing);
		expect(errors).toEqual([]);
	});
}

test('f respects native editing, sliders, modifiers, key repeat, composition, prevented events and modal help', async ({ page }) => {
	const { audio, errors } = await openTransport(page, { playing: false });
	await watchAudio(audio);
	const before = await time(audio);
	const search = page.locator('input[type="search"]').first();
	await search.focus();
	await page.keyboard.type('fF');
	await expect(search).toHaveValue('fF');
	await expectHeart(page, false);
	await search.fill('');

	for (const target of [page.getByTestId('player-seek'), page.getByTestId('player-volume')]) {
		await target.focus();
		await page.keyboard.press('f');
		await expect(target).toBeFocused();
		await expectHeart(page, false);
	}
	await page.getByText('Playback', { exact: true }).click();
	const speed = page.getByLabel('Playback speed', { exact: true });
	await speed.focus();
	await page.keyboard.press('f');
	await expect(speed).toBeFocused();
	await expectHeart(page, false);
	await blurToBody(page);
	for (const key of ['Control+f', 'Meta+f', 'Alt+f']) {
		await page.keyboard.press(key);
		await expectHeart(page, false);
	}
	for (const init of [{ repeat: true }, { isComposing: true }, { prevented: true }]) {
		await page.evaluate((init) => {
			const event = new KeyboardEvent('keydown', { key: 'f', bubbles: true, cancelable: true, ...init });
			if ('prevented' in init) event.preventDefault();
			document.body.dispatchEvent(event);
		}, init);
		await expectHeart(page, false);
	}
	// Structural typing targets use real editable nodes, not a replacement player.
	for (const kind of ['textarea', 'editable'] as const) {
		await page.evaluate((kind) => {
			const node = document.createElement(kind === 'textarea' ? 'textarea' : 'div');
			node.id = 'shortcut-editing-probe';
			if (kind === 'editable') node.contentEditable = 'true';
			document.querySelector('main')!.append(node);
			node.focus();
		}, kind);
		await page.keyboard.type('fF');
		await expectHeart(page, false);
		await page.locator('#shortcut-editing-probe').evaluate((node) => node.remove());
	}
	const opener = page.getByRole('button', { name: 'Keyboard shortcuts', exact: true });
	await opener.click();
	const dialog = page.getByRole('dialog', { name: 'Keyboard shortcuts' });
	await expect(dialog).toBeVisible();
	await expect(dialog.locator('dt').filter({ hasText: /^f$/ })).toHaveCount(1);
	await expect(dialog).toContainText('Save / remove current track from Favorites');
	await page.keyboard.press('f');
	await page.keyboard.press('F');
	await expectHeart(page, false);
	await page.keyboard.press('Escape');
	await expect(dialog).toHaveCount(0);
	await expect(opener).toBeFocused();
	await expect(audio).toHaveJSProperty('paused', true);
	expect(await time(audio)).toBe(before);
	expect(await heartLog(audio)).toEqual([]);
	await page.keyboard.press('f');
	await expectHeart(page, true);
	await expect(opener).toBeFocused();
	expect(errors).toEqual([]);
});
