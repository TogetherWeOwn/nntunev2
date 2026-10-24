import { expect, test as base, type Locator, type Page } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { createQueue, type QueueState, type Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/queue-trim/mount.js';

/**
 * TRIM — protected independent acceptance, paired with
 * src/lib/player/queue.trim.acceptance.test.ts and fixtures/queue-trim/*.
 * Test-only branch: test/queue-trim-acceptance. Baseline: adaeac2ad6d2d1967679521618a00e3db60cf62b.
 * Keep all five test paths unchanged in the Builder's combined ready PR.
 * No standalone ready PR of this intentionally RED test bank.
 *
 * Contract: buttons "Keep current and upcoming" and "Clear upcoming" report
 * optional host intents onkeepcurrentandupcoming / onclearupcoming. A panel
 * never mutates supplied state or substitutes another management/transport
 * callback. Without a callback its action is absent or disabled. With callbacks
 * both controls are shown, disabled only for invalid selection/no eligible rows.
 * Existing queue-item/data-track-id, now-playing, queue-position and
 * queue-announcement hooks stay intact. Announcements include the removed count
 * and "removed", not "played"; the existing polite status region is reused.
 *
 * Deterministic focus default: after a keyboard-activated trim disables itself,
 * focus the surviving current row (existing tabindex=-1 focus target). Prop
 * changes, append and disabled/no-op activation never steal focus. This makes
 * the generic deterministic-focus requirement concrete without a new hook.
 *
 * Real shared component is mounted under a test-only intent-recording parent.
 * Real /library owns the actual edits. Only API/media boundaries are mocked;
 * all foreign requests and all unrecognised APIs are aborted/fulfilled locally.
 * Library has no audio element/source; media probes also catch accidental
 * play/pause/load/seek/src writes. These tests do not claim real audio evidence.
 * Breaks caught: wrong callback, inclusive/current removal, stale index,
 * duplicate no-op effects, focus loss, transport/provider calls, auth leakage.
 *
 * Reproduce from repo root (Node 24, pnpm 10; browser installed):
 *   pnpm --dir apps/web install --frozen-lockfile
 *   pnpm --dir apps/web test src/lib/player/queue.trim.acceptance.test.ts
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web test:e2e tests/library-queue-trim.spec.ts --workers=1
 *   pnpm --dir apps/web check
 *   pnpm --dir apps/web test
 * Regressions: same test:e2e command with tests/library-queue-manage.spec.ts,
 * tests/library-queue-orientation.spec.ts, tests/library-player-queue.spec.ts,
 * tests/library-shuffle-upcoming.spec.ts and tests/library-selection-isolation.spec.ts.
 * The configured harness builds this checkout and refuses preview reuse in CI.
 * If 4173 is busy, use a run-owned config extending the repository config with
 * a free port/baseURL and reuseExistingServer:false; do not stop/reuse a peer.
 * Do not add '--' before a Vitest filter: that runs all tests in this version.
 * Observed on this baseline (2026-10-24): 30 unit assertion failures + 1
 * passing real-module control; 13 browser assertion failures + 2 passing
 * component controls. All 1863 pre-existing units and 33 selected queue browser
 * regressions pass. Web check, browser strict tsc and fixture Svelte check pass.
 * Actual RED: transitions are undefined (explicit capability assertion) and
 * Keep-current-and-upcoming is not visible. No import/build/harness errors.
 * Post-trim assertions remain unverified until implementation; passing negative
 * controls are not evidence that trimming exists. Builder owns green and
 * same-head CI/review on the combined PR. No existing test or production file
 * is changed.
 */

const test = base.extend<{ componentPage: Page }, { componentOrigin: string }>({
	componentOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/queue-trim/', import.meta.url).pathname),
			publicDir: false,
			plugins: [svelte({ configFile: false, compilerOptions: { runes: true } })],
			server: { host: '127.0.0.1', port: 0, fs: { allow: [decodeURIComponent(new URL('../', import.meta.url).pathname)] } }
		});
		try {
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('trim fixture server has no TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally {
			await server.close();
		}
	}, { scope: 'worker' }],
	componentPage: async ({ page, componentOrigin }, use) => {
		await instrument(page);
		await localOnly(page, componentOrigin);
		await page.goto(componentOrigin);
		await expect.poll(() => page.evaluate(() => Boolean(window.queueTrim))).toBe(true);
		await use(page);
	}
});

const TRACKS: Track[] = [
	{ id: 'trim-zulu', title: 'Zulu', artist: 'Trim Band', streamUrl: '/test-audio/zulu', artworkUrl: null },
	{ id: 'trim-apple', title: 'Apple', artist: 'Trim Band', streamUrl: '/test-audio/apple', artworkUrl: null },
	{ id: 'trim-mango', title: 'Mango', artist: 'Trim Band', streamUrl: '/test-audio/mango', artworkUrl: null },
	{ id: 'trim-kiwi', title: 'Kiwi', artist: 'Trim Band', streamUrl: '/test-audio/kiwi', artworkUrl: null }
];
const LIBRARY = TRACKS.map(({ id, title, artist }) => ({ id, title, artist }));
const KEEP = 'Keep current and upcoming';
const CLEAR = 'Clear upcoming';

function button(page: Page, name: string) { return page.getByRole('button', { name, exact: true }); }
function currentRow(page: Page) { return page.locator('[data-testid="queue-item"][aria-current="true"]'); }
function libraryRow(page: Page, id: string) { return page.locator(`[data-testid="library-item"][data-id="${id}"]`); }
async function order(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}
async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}
async function replace(page: Page, value: QueueState, callbacks = true) {
	await page.evaluate(({ value, callbacks }) => window.queueTrim.replace(value, callbacks), { value, callbacks });
	await settle(page);
}
async function requireControls(page: Page) {
	await expect(button(page, KEEP)).toBeVisible();
	await expect(button(page, CLEAR)).toBeVisible();
	await expect(button(page, KEEP)).toHaveAttribute('type', 'button');
	await expect(button(page, CLEAR)).toHaveAttribute('type', 'button');
}

async function instrument(page: Page) {
	await page.addInitScript(() => {
		const probe = { media: [] as string[], positions: [] as number[], sources: [] as string[] };
		window.trimProbe = probe;
		for (const method of ['play', 'pause', 'load'] as const) {
			Object.defineProperty(HTMLMediaElement.prototype, method, {
				configurable: true,
				value() { probe.media.push(method); return method === 'play' ? Promise.resolve() : undefined; }
			});
		}
		for (const property of ['currentTime', 'src'] as const) {
			const descriptor = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, property)!;
			Object.defineProperty(HTMLMediaElement.prototype, property, {
				...descriptor,
				set(value: number | string) {
					if (property === 'currentTime') probe.positions.push(Number(value));
					else probe.sources.push(String(value));
					descriptor.set!.call(this, value);
				}
			});
		}
	});
}

async function localOnly(page: Page, origin: string, payload?: { current: unknown }, calls: string[] = []) {
	const allowed = new URL(origin);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(allowed.hostname);
	await page.context().route('**/*', async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		if (url.origin !== allowed.origin) {
			calls.push(`foreign ${request.method()} ${url.origin}`);
			return route.abort();
		}
		if (url.pathname.startsWith('/api/')) {
			calls.push(`${request.method()} ${url.pathname}`);
			if (payload && url.pathname === '/api/library' && request.method() === 'GET') {
				return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(payload.current) });
			}
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		if (request.resourceType() === 'media' || url.pathname.startsWith('/test-audio/')) {
			calls.push(`media ${url.pathname}`);
			return route.abort();
		}
		return route.continue();
	});
}

async function seedLibrary(page: Page, origin: string, index: number) {
	const payload: { current: unknown } = { current: LIBRARY };
	const calls: string[] = [];
	await instrument(page);
	await localOnly(page, origin, payload, calls);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(4);
	for (const item of LIBRARY) await libraryRow(page, item.id).click();
	for (let next = 0; next < index; next += 1) await page.getByTestId('player-next').click();
	await expect(currentRow(page)).toHaveAttribute('data-track-id', TRACKS[index]!.id);
	expect(await order(page)).toEqual(['trim-zulu', 'trim-apple', 'trim-mango', 'trim-kiwi']);
	await settle(page);
	calls.length = 0;
	return { calls, payload };
}

async function noPlayback(page: Page, calls: string[]) {
	await settle(page);
	expect(calls).toEqual([]);
	expect(await page.evaluate(() => window.trimProbe)).toEqual({ media: [], positions: [], sources: [] });
	await expect(page.locator('audio')).toHaveCount(0);
}

async function tabTo(page: Page, target: Locator) {
	await button(page, 'Refresh').focus();
	for (let tabs = 0; tabs < 80 && !(await target.evaluate((element) => element === document.activeElement)); tabs += 1) {
		await page.keyboard.press('Tab');
	}
	await expect(target).toBeFocused();
	const ring = await target.evaluate((element) => {
		const style = getComputedStyle(element);
		return element.matches(':focus-visible') && ((style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) > 0) || style.boxShadow !== 'none');
	});
	expect(ring).toBe(true);
}

test('TRIM-C00 control: real mounted panel renders supplied queue and calls existing Next exactly once', async ({ componentPage: page }) => {
	const value = { ...createQueue(TRACKS), currentIndex: 1 };
	await replace(page, value);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'trim-apple');
	await page.getByTestId('player-next').click();
	expect(await page.evaluate(() => window.queueTrim.inspect())).toEqual({ state: value, calls: ['next'] });
});

for (const [id, name, call, key] of [
	['TRIM-C01', KEEP, 'keep', 'Enter'],
	['TRIM-C02', CLEAR, 'clear-upcoming', 'Space']
] as const) {
	test(`${id} panel reports only ${call} once, leaving immutable host state untouched`, async ({ componentPage: page }) => {
		const value = { ...createQueue(TRACKS), currentIndex: 1 };
		await replace(page, value);
		await requireControls(page);
		await button(page, name).focus();
		await page.keyboard.press(key);
		expect(await page.evaluate(() => window.queueTrim.inspect())).toEqual({ state: value, calls: [call] });
		expect(await page.evaluate(() => window.trimProbe)).toEqual({ media: [], positions: [], sources: [] });
	});
}

test('TRIM-C03 empty/invalid/singleton state disables both intents and repeated activation cannot call the host', async ({ componentPage: page }) => {
	for (const value of [createQueue([]), { tracks: [TRACKS[0]!], currentIndex: 0 }, ...[-1, 4, 0.5, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY].map((currentIndex) => ({ tracks: TRACKS, currentIndex }))]) {
		await replace(page, value);
		await requireControls(page);
		await page.getByTestId('trim-sentinel').focus();
		for (const name of [KEEP, CLEAR]) {
			await expect(button(page, name)).toBeDisabled();
			await button(page, name).evaluate((element) => { (element as HTMLButtonElement).click(); (element as HTMLButtonElement).click(); });
		}
		await expect(page.getByTestId('trim-sentinel')).toBeFocused();
		expect(await page.evaluate(() => window.queueTrim.inspect())).toEqual({ state: value, calls: [] });
	}
});

test('TRIM-C04 omitted host callbacks cannot expose an enabled inert trim control', async ({ componentPage: page }) => {
	const value = { ...createQueue(TRACKS), currentIndex: 1 };
	await replace(page, value, false);
	for (const name of [KEEP, CLEAR]) {
		const action = button(page, name);
		if (await action.count()) {
			await expect(action).toBeDisabled();
			await action.evaluate((element) => (element as HTMLButtonElement).click());
		}
	}
	expect(await page.evaluate(() => window.queueTrim.inspect())).toEqual({ state: value, calls: [] });
});

test('TRIM-C05 background prop replacements never autofocus and refresh boundary eligibility on one mount', async ({ componentPage: page }) => {
	await page.getByTestId('trim-sentinel').focus();
	for (const [index, keepDisabled, clearDisabled] of [[0, true, false], [1, false, false], [3, false, true]] as const) {
		await replace(page, { tracks: TRACKS, currentIndex: index });
		await requireControls(page);
		await expect(button(page, KEEP)).toBeEnabled({ enabled: ! keepDisabled });
		await expect(button(page, CLEAR)).toBeEnabled({ enabled: ! clearDisabled });
		await expect(page.getByTestId('trim-sentinel')).toBeFocused();
		expect((await page.evaluate(() => window.queueTrim.inspect())).calls).toEqual([]);
	}
});

const EXAMPLES = [
	{ id: 'TRIM-B01', index: 0, name: KEEP, want: ['trim-zulu', 'trim-apple', 'trim-mango', 'trim-kiwi'], position: 'Track 1 of 4', removed: 0, key: 'Enter' },
	{ id: 'TRIM-B02', index: 2, name: KEEP, want: ['trim-mango', 'trim-kiwi'], position: 'Track 1 of 2', removed: 2, key: 'Enter' },
	{ id: 'TRIM-B03', index: 3, name: KEEP, want: ['trim-kiwi'], position: 'Track 1 of 1', removed: 3, key: 'Space' },
	{ id: 'TRIM-B04', index: 0, name: CLEAR, want: ['trim-zulu'], position: 'Track 1 of 1', removed: 3, key: 'Space' },
	{ id: 'TRIM-B05', index: 1, name: CLEAR, want: ['trim-zulu', 'trim-apple'], position: 'Track 2 of 2', removed: 2, key: 'Space' },
	{ id: 'TRIM-B06', index: 3, name: CLEAR, want: ['trim-zulu', 'trim-apple', 'trim-mango', 'trim-kiwi'], position: 'Track 4 of 4', removed: 0, key: 'Enter' }
];

for (const example of EXAMPLES) {
	test(`${example.id} ${example.name} at ${example.index}: order/current/count/focus and repeated no-op, never transport`, async ({ page, baseURL }) => {
		const { calls } = await seedLibrary(page, baseURL!, example.index);
		const nowPlaying = await page.getByTestId('now-playing').textContent();
		const currentNode = await currentRow(page).elementHandle();
		await requireControls(page);
		const action = button(page, example.name);
		const announcement = page.getByTestId('queue-announcement');
		if (example.removed) {
			await expect(action).toBeEnabled();
			await tabTo(page, action);
			await page.keyboard.press(example.key);
			await expect(currentRow(page)).toBeFocused();
			await expect(announcement).toHaveAttribute('role', 'status');
			await expect(announcement).not.toHaveAttribute('aria-live', 'assertive');
			await expect(announcement).toContainText(/removed/i);
			await expect(announcement).toContainText(new RegExp(`\\b${example.removed}\\b`));
			await expect(announcement).not.toContainText(/played/i);
			expect((await announcement.textContent())!.length).toBeLessThanOrEqual(160);
		} else {
			await expect(action).toBeDisabled();
			await button(page, 'Refresh').focus();
		}
		await expect.poll(() => order(page)).toEqual(example.want);
		await expect(currentRow(page)).toHaveAttribute('data-track-id', TRACKS[example.index]!.id);
		expect(await currentNode!.evaluate((element) => element.isConnected && element.getAttribute('aria-current') === 'true')).toBe(true);
		await expect(page.getByTestId('now-playing')).toHaveText(nowPlaying!);
		await expect(page.getByTestId('queue-position')).toContainText(example.position);
		await expect(action).toBeDisabled();
		const other = button(page, example.name === KEEP ? CLEAR : KEEP);
		await expect(other).toBeEnabled({ enabled: example.want.length !== 1 });
		const settledAnnouncement = await announcement.textContent();
		await action.evaluate((element) => { (element as HTMLButtonElement).click(); (element as HTMLButtonElement).click(); });
		await settle(page);
		expect(await order(page)).toEqual(example.want);
		await expect(announcement).toHaveText(settledAnnouncement!);
		await expect(example.removed ? currentRow(page) : button(page, 'Refresh')).toBeFocused();
		await noPlayback(page, calls);
	});
}

test('TRIM-B07 touch at narrow dark viewport trims the middle and keeps whole-clear semantics', async ({ browser, baseURL }) => {
	const context = await browser.newContext({ baseURL, hasTouch: true, viewport: { width: 375, height: 812 }, colorScheme: 'dark' });
	try {
		const page = await context.newPage();
		const { calls } = await seedLibrary(page, baseURL!, 1);
		await requireControls(page);
		for (const name of [KEEP, CLEAR]) {
			const bounds = await button(page, name).boundingBox();
			expect(bounds).not.toBeNull();
			expect(bounds!.x).toBeGreaterThanOrEqual(0);
			expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(375);
		}
		await button(page, KEEP).tap();
		await expect.poll(() => order(page)).toEqual(['trim-apple', 'trim-mango', 'trim-kiwi']);
		await button(page, CLEAR).tap();
		await expect.poll(() => order(page)).toEqual(['trim-apple']);
		await expect(currentRow(page)).toHaveAttribute('data-track-id', 'trim-apple');
		await expect(button(page, KEEP)).toBeDisabled();
		await expect(button(page, CLEAR)).toBeDisabled();
		await noPlayback(page, calls);
		await button(page, 'Clear queue').tap();
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
		await expect(button(page, 'Clear queue')).toBeDisabled();
	} finally {
		await context.close();
	}
});

test('TRIM-B08 background append/refresh never autofocus or resurrect trimmed rows', async ({ page, baseURL }) => {
	const { calls } = await seedLibrary(page, baseURL!, 2);
	await requireControls(page);
	await button(page, KEEP).click();
	await expect.poll(() => order(page)).toEqual(['trim-mango', 'trim-kiwi']);
	await button(page, 'Refresh').focus();
	await libraryRow(page, 'trim-apple').locator('button.nt-library-row').dispatchEvent('click');
	await expect.poll(() => order(page)).toEqual(['trim-mango', 'trim-kiwi', 'trim-apple']);
	await expect(button(page, 'Refresh')).toBeFocused();
	await noPlayback(page, calls);
	await button(page, 'Refresh').press('Enter');
	await expect.poll(() => calls.length).toBe(1);
	await settle(page);
	expect(calls).toEqual(['GET /api/library']);
	expect(await order(page)).toEqual(['trim-mango', 'trim-kiwi', 'trim-apple']);
	await expect(currentRow(page)).toHaveAttribute('data-track-id', 'trim-mango');
	await expect(button(page, 'Refresh')).toBeFocused();
});

test('TRIM-B09 auth purge discards the trimmed queue before another account retries', async ({ page, baseURL }) => {
	const { payload } = await seedLibrary(page, baseURL!, 1);
	await requireControls(page);
	await button(page, CLEAR).click();
	await expect.poll(() => order(page)).toEqual(['trim-zulu', 'trim-apple']);
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'trim-test-auth-transition');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await otherTab.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	payload.current = [{ id: 'trim-next-account', title: 'Next account only', artist: 'Other Band' }];
	await button(page, 'Retry').click();
	await expect(page.getByTestId('library-list')).toContainText('Next account only');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await requireControls(page);
	await expect(button(page, KEEP)).toBeDisabled();
	await expect(button(page, CLEAR)).toBeDisabled();
});
