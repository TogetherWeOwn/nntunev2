import { expect, test as base, type Locator, type Page } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { createQueue, type QueueState, type Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/queue-orientation/mount.js';

/**
 * QUEUE-ORIENTATION — protected independent runtime acceptance.
 * Test bank ref: test/queue-orientation-acceptance; RED base
 * 9b034604bad8c489ee71effa7544132fe5017dfe (2026-10-23).
 * Observed: 10 new feature cases assertion-RED (missing summary/button);
 * QO-C00 + 10 existing library regressions pass; no harness/import errors.
 *
 * Companion: src/lib/player/playerQueue.orientation.test.ts (QO-U01..U17).
 * QO-C cases use the REAL shared component in a test-only Svelte parent:
 * replace existing state/onnext inputs on one mount, never a product stub or
 * new production export. QO-B cases use the REAL /library route with local
 * API fixtures. These suites do not exercise demo queue markup or live audio.
 *
 * Contract/hook documentation is in the unit spec's header. Keep both specs
 * and fixtures/queue-orientation/* unchanged in the Builder's combined PR;
 * route a mistaken test back to its author rather than weakening it.
 *
 * From apps/web (Node 24 / pnpm 10):
 *   pnpm install --frozen-lockfile
 *   pnpm exec playwright install chromium
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm test:e2e tests/library-queue-orientation.spec.ts --workers=1
 * Regression: same command with tests/library-player-queue.spec.ts,
 *   tests/library-player-queue-pins.spec.ts, tests/library-selection-isolation.spec.ts.
 *   pnpm check; pnpm test (the full unit suite includes intentional RED here).
 * Ensure preview port 4173 is free; never reuse another worktree's preview.
 * Standalone component Vite server uses a loopback ephemeral port and closes
 * with its Playwright worker. All APIs are fulfilled locally; foreign hosts
 * are aborted. No secrets, provider, staging or production access is needed.
 *
 * Expected pre-feature RED: summary/reveal assertions; no missing imports or
 * compile/runtime harness failures. Passing negative controls are not evidence
 * that the feature exists. Builder next action: implement shared PlayerQueue
 * only, then run this file plus protected regressions and same-head PR gates.
 */

const test = base.extend<{ componentPage: Page }, { componentOrigin: string }>({
	componentOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/queue-orientation/', import.meta.url).pathname),
			publicDir: false,
			plugins: [svelte({ configFile: false, compilerOptions: { runes: true } })],
			server: { host: '127.0.0.1', port: 0, fs: { allow: [decodeURIComponent(new URL('../', import.meta.url).pathname)] } }
		});
		try {
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('test component server has no TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally {
			await server.close();
		}
	}, { scope: 'worker' }],
	componentPage: async ({ page, componentOrigin }, use) => {
		await instrument(page);
		await localOnly(page, componentOrigin);
		await page.goto(componentOrigin);
		await expect.poll(() => page.evaluate(() => Boolean(window.queueOrientation))).toBe(true);
		await use(page);
	}
});

test.use({ viewport: { width: 1000, height: 700 } });

function track(id: string, title: string): Track {
	return { id, title, artist: 'Orientation Band', streamUrl: `/test-audio/${id}`, artworkUrl: null };
}

const MANGO = track('orientation-mango', 'Mango Season');
const APPLE = track('orientation-apple', 'Apple Cart');
const ZEBRA = track('orientation-zebra', 'Zebra Crossing');
const KIWI = track('orientation-kiwi', 'Kiwi Stand');
const LONG = Array.from({ length: 31 }, (_, index) => track(`long-${index}`, `Long Queue Song ${index}`));
const LIBRARY = [MANGO, APPLE, ZEBRA, KIWI].map(({ id, title, artist }) => ({ id, title, artist }));

function state(tracks: Track[], currentIndex: number): QueueState {
	return { ...createQueue(tracks), currentIndex };
}

function currentRow(page: Page): Locator {
	return page.locator('[data-testid="queue-item"][aria-current="true"]');
}

function libraryRow(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

/** Keep real focus/scroll; suppress decoding/playback and observe its boundary. */
async function instrument(page: Page) {
	await page.addInitScript(() => {
		const probe = { media: [] as string[], storage: [] as string[], scroll: [] as string[] };
		window.orientationProbe = probe;
		for (const method of ['play', 'pause', 'load'] as const) {
			Object.defineProperty(HTMLMediaElement.prototype, method, {
				configurable: true,
				value() { probe.media.push(method); return method === 'play' ? Promise.resolve() : undefined; }
			});
		}
		for (const method of ['setItem', 'removeItem', 'clear'] as const) {
			const original = Storage.prototype[method];
			Object.defineProperty(Storage.prototype, method, {
				configurable: true,
				value(...args: unknown[]) { probe.storage.push(method); return Reflect.apply(original, this, args); }
			});
		}
		const original = Element.prototype.scrollIntoView;
		Element.prototype.scrollIntoView = function (options) {
			probe.scroll.push(typeof options === 'object' ? (options.behavior ?? 'auto') : 'auto');
			return original.call(this, options);
		};
		for (const target of [Element.prototype, window]) {
			for (const method of ['scroll', 'scrollTo'] as const) {
				const native = target[method];
				Object.defineProperty(target, method, {
					configurable: true,
					value(...args: unknown[]) {
						const options = args[0] as ScrollToOptions | undefined;
						probe.scroll.push(typeof options === 'object' ? (options?.behavior ?? 'auto') : 'auto');
						return Reflect.apply(native, this, args);
					}
				});
			}
		}
	});
}

/** No test in this file is permitted to call a live API or foreign host. */
async function localOnly(page: Page, origin: string, library?: { current: unknown }, apiCalls: string[] = []) {
	const url = new URL(origin);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(url.hostname);
	await page.route('**/*', async (route) => {
		const request = route.request();
		const target = new URL(request.url());
		if (target.origin !== url.origin) return route.abort();
		if (target.pathname.startsWith('/api/')) {
			apiCalls.push(`${request.method()} ${target.pathname}`);
			if (library && target.pathname === '/api/library' && request.method() === 'GET') {
				return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(library.current) });
			}
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		if (request.resourceType() === 'media' || target.pathname.startsWith('/test-audio/')) return route.abort();
		return route.continue();
	});
}

async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}

async function resetProbe(page: Page) {
	await page.evaluate(() => {
		const probe = window.orientationProbe;
		probe.media.length = 0;
		probe.storage.length = 0;
		probe.scroll.length = 0;
	});
}

async function replace(page: Page, value: QueueState) {
	await page.evaluate((next) => window.queueOrientation.replace(next), value);
	await settle(page);
}

async function inspect(page: Page) {
	return page.evaluate(() => window.queueOrientation.inspect());
}

async function expectNoSideEffects(page: Page) {
	const probe = await page.evaluate(() => window.orientationProbe);
	expect(probe.media).toEqual([]);
	expect(probe.storage).toEqual([]);
	return probe;
}

async function expectPosition(page: Page, position: string, upcoming: string) {
	const summary = page.getByTestId('queue-position');
	await expect(summary).toBeVisible();
	await expect(summary).toContainText(/queue selection/i);
	await expect(summary).toContainText(new RegExp(`\\b${position}\\b`));
	await expect(summary).toContainText(new RegExp(`\\b${upcoming}\\b`));
	expect(await summary.evaluate((element) => Boolean(element.closest('[data-testid="now-playing"]')))).toBe(false);
	await expect(page.getByTestId('now-playing')).not.toContainText(/upcoming|Track \d+ of|queue selection/i);
}

async function queueSnapshot(page: Page) {
	return {
		rows: await page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => ({ id: row.getAttribute('data-track-id'), current: row.getAttribute('aria-current'), text: row.textContent }))),
		announcement: await page.getByTestId('now-playing').textContent(),
		summary: await page.getByTestId('queue-position').textContent()
	};
}

test.describe('mounted shared component', () => {
	test('QO-C00 control: the harness mounts the real queue, updates props and observes its real Next callback', async ({ componentPage: page }) => {
		await replace(page, state([MANGO, APPLE], 0));
		await expect(page.getByTestId('now-playing')).toContainText('Mango Season');
		await expect(currentRow(page)).toHaveAttribute('data-track-id', MANGO.id);
		await page.getByTestId('player-next').click();
		expect((await inspect(page)).nextCalls).toBe(1);
		await replace(page, state([MANGO, APPLE], 1));
		await expect(page.getByTestId('now-playing')).toContainText('Apple Cart');
		await expect(currentRow(page)).toHaveAttribute('data-track-id', APPLE.id);
	});

	test('QO-C01 updates one mount through navigation/reorder/append/remove/clear without automatic focus or scroll', async ({ componentPage: page }) => {
		const sentinel = page.getByTestId('focus-sentinel');
		await sentinel.focus();
		await resetProbe(page);
		await replace(page, state([MANGO, APPLE, ZEBRA], 1));
		await expectPosition(page, 'Track 2 of 3', '1 upcoming');
		await expect(sentinel).toBeFocused();
		expect((await expectNoSideEffects(page)).scroll).toEqual([]);
		const scrollBefore = await page.evaluate(() => ({ x: window.scrollX, y: window.scrollY }));
		const snapshots = [
			{ value: state([MANGO, APPLE, ZEBRA], 2), position: 'Track 3 of 3', upcoming: '0 upcoming', title: 'Zebra Crossing' },
			{ value: state([ZEBRA, MANGO, APPLE], 0), position: 'Track 1 of 3', upcoming: '2 upcoming', title: 'Zebra Crossing' },
			{ value: state([ZEBRA, MANGO, APPLE, KIWI], 0), position: 'Track 1 of 4', upcoming: '3 upcoming', title: 'Zebra Crossing' },
			{ value: state([ZEBRA, APPLE, KIWI], 0), position: 'Track 1 of 3', upcoming: '2 upcoming', title: 'Zebra Crossing' },
			{ value: state([APPLE, KIWI], 1), position: 'Track 2 of 2', upcoming: '0 upcoming', title: 'Kiwi Stand' }
		];
		for (const snapshot of snapshots) {
			await replace(page, snapshot.value);
			await expectPosition(page, snapshot.position, snapshot.upcoming);
			await expect(currentRow(page)).toHaveCount(1);
			await expect(currentRow(page)).toContainText(snapshot.title);
			await expect(sentinel).toBeFocused();
			expect(await inspect(page)).toEqual({ state: snapshot.value, nextCalls: 0 });
			expect(await page.evaluate(() => ({ x: window.scrollX, y: window.scrollY }))).toEqual(scrollBefore);
			expect((await expectNoSideEffects(page)).scroll).toEqual([]);
		}
		await replace(page, createQueue([]));
		await expect(page.getByTestId('queue-position')).toContainText(/queue is empty/i);
		await expect(page.getByRole('button', { name: 'Show current track', exact: true })).toBeDisabled();
		await expect(sentinel).toBeFocused();
		expect(await inspect(page)).toEqual({ state: createQueue([]), nextCalls: 0 });
		expect((await expectNoSideEffects(page)).scroll).toEqual([]);
	});

	for (const example of [
		{ id: 'QO-C02', key: 'Enter', motion: 'no-preference' },
		{ id: 'QO-C03', key: 'Space', motion: 'no-preference' },
		{ id: 'QO-C04', key: 'Enter', motion: 'reduce' },
		{ id: 'QO-C05', key: 'Space', motion: 'reduce' }
	] as const) {
		test(`${example.id} ${example.key} reveals/focuses the current offscreen row (${example.motion}) without transport or state changes`, async ({ componentPage: page }) => {
			await page.emulateMedia({ reducedMotion: example.motion });
			const value = state(LONG, 15);
			await replace(page, value);
			await expectPosition(page, 'Track 16 of 31', '15 upcoming');
			const reveal = page.getByRole('button', { name: 'Show current track', exact: true });
			await expect(reveal).toHaveAttribute('type', 'button');
			await page.getByTestId('focus-sentinel').focus();
			for (let tabs = 0; tabs < 6 && !(await reveal.evaluate((element) => element === document.activeElement)); tabs += 1) {
				await page.keyboard.press('Tab');
				expect(await page.getByTestId('queue-item').evaluateAll((rows) => rows.some((row) => row === document.activeElement))).toBe(false);
			}
			await expect(reveal).toBeFocused();
			await expect(currentRow(page)).not.toBeInViewport();
			const before = await queueSnapshot(page);
			await resetProbe(page);
			await page.keyboard.press(example.key);
			await expect(currentRow(page)).toBeFocused();
			await expect(currentRow(page)).toBeInViewport({ ratio: 1 });
			const focus = await currentRow(page).evaluate((row) => {
				const style = getComputedStyle(row);
				return { visible: row.matches(':focus-visible'), ring: (style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) > 0) || style.boxShadow !== 'none', tabIndex: (row as HTMLElement).tabIndex };
			});
			expect(focus).toEqual({ visible: true, ring: true, tabIndex: -1 });
			expect(await queueSnapshot(page)).toEqual(before);
			expect(await inspect(page)).toEqual({ state: value, nextCalls: 0 });
			const probe = await expectNoSideEffects(page);
			if (example.motion === 'reduce') expect(probe.scroll).not.toContain('smooth');

			// A prior reveal must not become a sticky auto-follow preference,
			// and the next explicit reveal must use the NEW selected row.
			await page.getByTestId('focus-sentinel').focus();
			await resetProbe(page);
			const moved = state(LONG.filter((track) => track.id !== 'long-15'), 8);
			await replace(page, moved);
			await expectPosition(page, 'Track 9 of 30', '21 upcoming');
			await expect(page.getByTestId('focus-sentinel')).toBeFocused();
			expect((await expectNoSideEffects(page)).scroll).toEqual([]);
			await reveal.focus();
			await resetProbe(page);
			await page.keyboard.press(example.key);
			await expect(currentRow(page)).toHaveAttribute('data-track-id', 'long-8');
			await expect(currentRow(page)).toBeFocused();
			await expect(currentRow(page)).toBeInViewport({ ratio: 1 });
			expect(await inspect(page)).toEqual({ state: moved, nextCalls: 0 });
			const repeated = await expectNoSideEffects(page);
			if (example.motion === 'reduce') expect(repeated.scroll).not.toContain('smooth');
		});
	}

	test('QO-C06 empty and invalid selections disable reveal and cannot move focus or invoke transport', async ({ componentPage: page }) => {
		const sentinel = page.getByTestId('focus-sentinel');
		for (const value of [createQueue([]), state([MANGO, APPLE], -1), state([MANGO, APPLE], 2), state([MANGO, APPLE], 0.5), state([MANGO, APPLE], Number.NaN), state([MANGO, APPLE], Number.POSITIVE_INFINITY)]) {
			await replace(page, value);
			const reveal = page.getByRole('button', { name: 'Show current track', exact: true });
			await expect(reveal).toBeDisabled();
			await expect(currentRow(page)).toHaveCount(0);
			await sentinel.focus();
			await resetProbe(page);
			await reveal.evaluate((button) => (button as HTMLButtonElement).click());
			await settle(page);
			await expect(sentinel).toBeFocused();
			expect(await inspect(page)).toEqual({ state: value, nextCalls: 0 });
			expect((await expectNoSideEffects(page)).scroll).toEqual([]);
		}
	});
});

test.describe('mocked library integration', () => {
	test('QO-B01 empty, append, navigate and refresh-removal counts update without announcing queue length', async ({ page, baseURL }) => {
		const payload: { current: unknown } = { current: LIBRARY };
		const apiCalls: string[] = [];
		await instrument(page);
		await localOnly(page, baseURL!, payload, apiCalls);
		await page.goto('/library');
		await expect(page.getByTestId('library-item')).toHaveCount(4);
		await expect(page.getByTestId('queue-position')).toContainText(/queue is empty/i);
		await expect(page.getByRole('button', { name: 'Show current track', exact: true })).toBeDisabled();
		await libraryRow(page, ZEBRA.id).click();
		await expectPosition(page, 'Track 1 of 1', '0 upcoming');
		const announcement = await page.getByTestId('now-playing').textContent();
		await libraryRow(page, MANGO.id).click();
		await libraryRow(page, APPLE.id).click();
		await expectPosition(page, 'Track 1 of 3', '2 upcoming');
		await expect(page.getByTestId('now-playing')).toHaveText(announcement!);
		await page.getByTestId('player-next').click();
		await expectPosition(page, 'Track 2 of 3', '1 upcoming');
		await expect(currentRow(page)).toHaveAttribute('data-track-id', MANGO.id);
		payload.current = [LIBRARY[0], LIBRARY[1], LIBRARY[3]];
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await expectPosition(page, 'Track 1 of 2', '1 upcoming');
		await expect(currentRow(page)).toHaveAttribute('data-track-id', MANGO.id);
		expect((await queueSnapshot(page)).rows.map((row) => row.id)).toEqual([MANGO.id, APPLE.id]);
		// Empty library unmounts PlayerQueue (library/+page.svelte empty
		// branch); assert the pinned page contract, not a missing hook.
		payload.current = [];
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(0);
		await expect(page.getByRole('status').filter({ hasText: /library is empty/i })).toBeVisible();
		await expect(page.getByTestId('queue-position')).toHaveCount(0);
		await expect(page.getByRole('button', { name: 'Show current track', exact: true })).toHaveCount(0);
		// The queue was dropped with the library, not just hidden: a
		// re-mounted panel shows the empty state (assertion-RED without the
		// feature, GREEN with it).
		payload.current = [LIBRARY[0]];
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(1);
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await expect(page.getByTestId('queue-position')).toContainText(/queue is empty/i);
		await expect(currentRow(page)).toHaveCount(0);
		await expect(page.getByRole('button', { name: 'Show current track', exact: true })).toBeDisabled();
		expect(apiCalls.every((call) => call === 'GET /api/library')).toBe(true);
	});

	for (const key of ['Enter', 'Space'] as const) {
		test(`QO-B0${key === 'Enter' ? '2' : '3'} ${key} reveal in a long library queue preserves selection/order and makes no playback or persistence calls`, async ({ page, baseURL }) => {
			const items = LONG.map(({ id, title, artist }) => ({ id, title, artist }));
			const apiCalls: string[] = [];
			await instrument(page);
			await localOnly(page, baseURL!, { current: items }, apiCalls);
			await page.goto('/library');
			await expect(page.getByTestId('library-item')).toHaveCount(31);
			// DOM events supply parent state without test-runner scrolling/focusing
			// every row. Reveal itself below is a genuine keyboard gesture.
			for (const item of items) await libraryRow(page, item.id).getByRole('button').dispatchEvent('click');
			await expect(page.getByTestId('queue-item')).toHaveCount(31);
			for (let index = 0; index < 15; index += 1) await page.getByTestId('player-next').dispatchEvent('click');
			await expect(currentRow(page)).toHaveAttribute('data-track-id', 'long-15');
			await expectPosition(page, 'Track 16 of 31', '15 upcoming');
			// CSP-safe offscreen determinism: pin the reveal control via CSSOM
			// property assignment (allowed under style-src 'self' +
			// style-src-attr 'unsafe-inline'), never a <style> element. No
			// row-height boost: 31 library + 31 queue rows already place the
			// current row offscreen, and artificial heights introduce
			// fractional clipping that breaks the ratio:1 assertion.
			await page.evaluate(() => {
				const reveal = document.querySelector('[data-testid="queue-reveal-current"]');
				if (reveal instanceof HTMLElement) {
					reveal.style.position = 'fixed';
					reveal.style.top = '8px';
					reveal.style.right = '8px';
				}
			});
			await page.evaluate(() => window.scrollTo(0, 0));
			const reveal = page.getByRole('button', { name: 'Show current track', exact: true });
			await reveal.focus();
			await expect(currentRow(page)).not.toBeInViewport();
			const before = await queueSnapshot(page);
			await resetProbe(page);
			apiCalls.length = 0;
			await page.keyboard.press(key);
			await expect(currentRow(page)).toBeFocused();
			await expect(currentRow(page)).toBeInViewport({ ratio: 1 });
			expect(await queueSnapshot(page)).toEqual(before);
			await expectNoSideEffects(page);
			expect(apiCalls).toEqual([]);
			await expect(page.locator('audio')).toHaveCount(0);
		});
	}

	test('QO-B04 parent navigation and append do not steal focus or automatically reveal a row', async ({ page, baseURL }) => {
		await instrument(page);
		await localOnly(page, baseURL!, { current: LIBRARY });
		await page.goto('/library');
		await expect(page.getByTestId('library-item')).toHaveCount(4);
		await libraryRow(page, ZEBRA.id).click();
		await libraryRow(page, MANGO.id).click();
		const next = page.getByTestId('player-next');
		await next.focus();
		await resetProbe(page);
		const scrollBefore = await page.evaluate(() => window.scrollY);
		await page.keyboard.press('Enter');
		await expect(currentRow(page)).toHaveAttribute('data-track-id', MANGO.id);
		await expect(next).toBeFocused();
		await libraryRow(page, APPLE.id).getByRole('button').dispatchEvent('click');
		await expect(page.getByTestId('queue-item')).toHaveCount(3);
		await expect(next).toBeFocused();
		await settle(page);
		expect(await page.evaluate(() => window.scrollY)).toBe(scrollBefore);
		expect((await expectNoSideEffects(page)).scroll).toEqual([]);
		await expectPosition(page, 'Track 2 of 3', '1 upcoming');
	});
});
