import { expect, test as base, type Page } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { QueueState, Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/library-timeline/mount.js';

const routePath = decodeURIComponent(new URL('../src/routes/library/+page.svelte', import.meta.url).pathname);
const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const test = base.extend<{ libraryPage: Page }, { libraryOrigin: string }>({
	libraryOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/library-timeline/', import.meta.url).pathname),
			publicDir: false,
			resolve: { alias: {
				'#lib': `${webRoot}src/lib`,
				'$app/navigation': decodeURIComponent(new URL('./fixtures/library-timeline/navigation.ts', import.meta.url).pathname)
			} },
			plugins: [{
				name: 'acceptance-library-queue-input', enforce: 'pre',
				transform(code, id) {
					if (id !== routePath) return;
					if (!code.includes('</script>')) throw new Error('library script boundary missing');
					return code.replace('</script>', '\nexport function acceptanceReplaceQueue(next: QueueState) { replaceQueue(next); }\n</script>');
				}
			}, svelte({ configFile: false, compilerOptions: { runes: true } })],
			server: { host: '127.0.0.1', port: 0, fs: { allow: [webRoot] } }
		});
		try {
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('no fixture TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally { await server.close(); }
	}, { scope: 'worker' }],
	libraryPage: async ({ page, libraryOrigin }, use) => {
		await page.addInitScript(() => {
			const control = { playCalls: 0, pauseCalls: 0, loadCalls: 0 };
			Object.assign(window, { __libtimeLifecycle: control });
			HTMLMediaElement.prototype.play = function (): Promise<void> {
				control.playCalls += 1;
				return new Promise<void>(() => {});
			};
			HTMLMediaElement.prototype.pause = function () {
				control.pauseCalls += 1;
			};
			const rawLoad = HTMLMediaElement.prototype.load;
			HTMLMediaElement.prototype.load = function () {
				control.loadCalls += 1;
				return rawLoad.call(this);
			};
		});
		await localOnly(page, libraryOrigin);
		await page.goto(libraryOrigin);
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await expect.poll(() => page.evaluate(() => Boolean(window.libraryTimeline))).toBe(true);
		await use(page);
	}
});

test.setTimeout(20_000);
test.use({ viewport: { width: 1000, height: 740 } });

const TRACKS: Track[] = [
	{ id: 'lt-coral', title: 'Coral Night', artist: 'Mira North', streamUrl: '/__acceptance_audio__/coral.wav' },
	{ id: 'lt-pine', title: 'Pine Morning', artist: 'South Ensemble', streamUrl: '/__acceptance_audio__/pine.wav' },
	{ id: 'lt-ember', title: 'Ember Noon', artist: 'West Quartet', streamUrl: '/__acceptance_audio__/ember.wav' }
];
const state = (currentIndex = 0, tracks = TRACKS): QueueState => ({ tracks, currentIndex });
const player = (page: Page) => page.getByRole('region', { name: 'Player', exact: true });
const audio = (page: Page) => player(page).locator('audio');

async function localOnly(page: Page, origin: string) {
	const expectedOrigin = new URL(origin).origin;
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await page.route('**/*', (route) => {
		const request = route.request();
		const target = new URL(request.url());
		if (target.origin !== expectedOrigin || request.resourceType() === 'media') return route.abort();
		if (target.pathname === '/api/library' && request.method() === 'GET') {
			return route.fulfill({ status: 200, json: TRACKS.map(({ id, title, artist }) => ({ id, title, artist })) });
		}
		if (/^\/(?:api|v1|__acceptance_audio__)(?:\/|$)/.test(target.pathname)) return route.abort();
		return route.continue();
	});
}

async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) =>
		requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}

async function replace(page: Page, next = state()) {
	await page.evaluate((value) => window.libraryTimeline.replace(value), next);
	await settle(page);
}

/**
 * Deterministic media double on the CURRENT library audio element: finite or
 * sentinel duration, a writable currentTime, a paused flag and explicit
 * seekable ranges. Re-apply after any keyed replacement recreates the element.
 */
async function mockMedia(page: Page, init: {
	currentTime: number; duration: number; paused?: boolean; seekable?: Array<[number, number]>;
}) {
	await audio(page).evaluate((el: HTMLAudioElement, seed: typeof init) => {
		let time = seed.currentTime;
		const ranges = seed.seekable ?? (Number.isFinite(seed.duration) && seed.duration >= 0
			? [[0, seed.duration] as [number, number]] : []);
		Object.defineProperty(el, 'duration', { value: seed.duration, configurable: true });
		Object.defineProperty(el, 'currentTime', {
			get: () => time,
			set: (next: number) => { time = next; },
			configurable: true
		});
		Object.defineProperty(el, 'paused', { value: seed.paused ?? true, configurable: true });
		Object.defineProperty(el, 'seekable', {
			value: { length: ranges.length, start: (i: number) => ranges[i]![0], end: (i: number) => ranges[i]![1] },
			configurable: true
		});
	}, init);
}

async function fireAudio(page: Page, type: string) {
	await audio(page).evaluate((el, name) => {
		el.dispatchEvent(new Event(name));
	}, type);
}

/** Seed coordinates then deliver the metadata/time events a real load produces. */
async function loadMedia(page: Page, init: { currentTime: number; duration: number; paused?: boolean; seekable?: Array<[number, number]> }) {
	await mockMedia(page, init);
	for (const type of ['loadedmetadata', 'durationchange', 'timeupdate']) await fireAudio(page, type);
	await settle(page);
}


test('slider reflects a seek clamped to the current whole second', async ({ libraryPage: page }) => {
	await replace(page);
	await loadMedia(page, { currentTime: 30, duration: 120, seekable: [[0, 30]] });
	const slider = page.getByTestId('library-timeline');
	await expect(slider).toHaveValue('30');
	await slider.evaluate((el: HTMLInputElement) => {
		el.value = '35';
		el.dispatchEvent(new Event('input', { bubbles: true }));
	});
	await expect(slider).toHaveValue('30');
	expect(await audio(page).evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(30);
});

test('slider reflects current position if seekability disappears before input', async ({ libraryPage: page }) => {
	await replace(page);
	await loadMedia(page, { currentTime: 30, duration: 120 });
	const slider = page.getByTestId('library-timeline');
	await expect(slider).toHaveValue('30');
	await audio(page).evaluate((el: HTMLAudioElement) => {
		Object.defineProperty(el, 'seekable', { value: { length: 0 }, configurable: true });
	});
	await slider.evaluate((el: HTMLInputElement) => {
		el.value = '35';
		el.dispatchEvent(new Event('input', { bubbles: true }));
	});
	await expect(slider).toHaveValue('30');
	expect(await audio(page).evaluate((el: HTMLAudioElement) => el.currentTime)).toBe(30);
});

for (const event of ['progress', 'canplay']) {
	test(`paused timeline enables seeking when ${event} exposes ranges`, async ({ libraryPage: page }) => {
		await replace(page);
		await loadMedia(page, { currentTime: 30, duration: 120, seekable: [] });
		const slider = page.getByTestId('library-timeline');
		await expect(slider).toBeDisabled();
		await mockMedia(page, { currentTime: 30, duration: 120 });
		await fireAudio(page, event);
		await expect(slider).toBeEnabled();
		await expect(page.getByTestId('library-time-readout')).not.toContainText('Seeking unavailable');
	});
}

test('sticky player does not obscure keyboard focus in a long queue', async ({ libraryPage: page }) => {
	const tracks = Array.from({ length: 40 }, (_, index) => ({
		...TRACKS[0], id: `long-${index}`, title: `Long queue track ${index}`
	}));
	await replace(page, state(0, tracks));
	await loadMedia(page, { currentTime: 30, duration: 120 });
	// Exercise actual Tab focus, not locator.click() (which scrolls its target).
	const buttons = page.getByTestId('queue-item').locator('button:enabled');
	await buttons.first().focus();
	for (let index = 0; index < 50; index += 1) {
		await page.keyboard.press('Tab');
		await expect.poll(() => page.evaluate(() => {
			const el = document.activeElement;
			if (!(el instanceof HTMLElement) || !el.closest('[data-testid="queue-item"]')) return false;
			const box = el.getBoundingClientRect();
			const x = box.left + box.width / 2;
			const y = box.top + box.height / 2;
			const topmost = document.elementFromPoint(x, y);
			return y >= 0 && y <= innerHeight && topmost !== null && el.contains(topmost);
		})).toBe(true);
	}
});
