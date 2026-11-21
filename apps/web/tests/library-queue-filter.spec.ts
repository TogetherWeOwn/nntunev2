import { expect, test as base, type Locator, type Page } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { createQueue, type QueueState, type Track } from '../src/lib/player/queue.js';
import { choose } from './appearance.helpers.js';
import type {} from './fixtures/queue-filter/mount.js';

/**
 * QFILTER — queue-only presentation acceptance; protected test-only handoff.
 * Companion contract/ownership: src/lib/player/playerQueue.filter.acceptance.test.ts.
 * Protected: these two specs and fixtures/queue-filter/*; all other acceptance
 * suites must remain unchanged. Branch: test/queue-filter-acceptance.
 *
 * Pure input vectors C01..C08 run through the REAL mounted PlayerQueue rather
 * than inventing a not-yet-existent filter export. The frozen parent observes
 * existing callback arguments and state only; it implements no filtering or
 * queue transitions. B cases exercise the real /library and real queue store.
 * All API responses are local; no live providers, streaming or production.
 *
 * Setup: pnpm --dir apps/web install --frozen-lockfile
 *        pnpm --dir apps/web exec playwright install chromium
 * Run: CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web test:e2e \
 *        tests/library-queue-filter.spec.ts --workers=1
 * Preserve/regress: same command with tests/library-queue-manage.spec.ts,
 * tests/library-queue-orientation.spec.ts, tests/library-player-queue.spec.ts,
 * tests/library-selection-isolation.spec.ts. Unit commands in companion header.
 * Port 4173 must be free (never reuse another worktree's preview); the component
 * server has a worker-owned ephemeral loopback port and is closed in finally.
 * Expected RED before implementation: missing "Filter queue" / count assertions,
 * not timeout clicks, setup/import errors, or a dummy feature implementation.
 * Builder owns combined GREEN ready PR and all same-head gates. No RED-only PR.
 */

const test = base.extend<{ componentPage: Page }, { componentOrigin: string }>({
	componentOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/queue-filter/', import.meta.url).pathname),
			publicDir: false,
			plugins: [svelte({ configFile: false, compilerOptions: { runes: true } })],
			server: { host: '127.0.0.1', port: 0, fs: { allow: [decodeURIComponent(new URL('../', import.meta.url).pathname)] } }
		});
		try {
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('component server has no TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally { await server.close(); }
	}, { scope: 'worker' }],
	componentPage: async ({ page, componentOrigin }, use) => {
		await instrument(page);
		await localOnly(page, componentOrigin);
		await page.goto(componentOrigin);
		await expect.poll(() => page.evaluate(() => Boolean(window.queueFilter))).toBe(true);
		await use(page);
	}
});

test.use({ viewport: { width: 1000, height: 700 }, hasTouch: true, screenshot: 'only-on-failure' });
test.setTimeout(20_000);

const ITEMS = [
	{ id: 'qf-coral', title: 'Coral Night', artist: 'Mira' },
	{ id: 'qf-pine', title: 'Pine Morning', artist: 'North' },
	{ id: 'qf-ember', title: 'Ember Noon', artist: 'South' },
	{ id: 'qf-drift', title: 'Drift Evening', artist: 'Mira' },
	{ id: 'qf-reprise', title: 'Coral Reprise', artist: 'West' },
	{ id: 'unnamed-queue', title: '  ', artist: '' },
	{ id: 'qf-literal', title: '<b>Literal & Song</b>', artist: 'Symbols' }
];
const TRACKS: Track[] = ITEMS.map((track) => ({ ...track, streamUrl: '' }));
const ORDER = ['qf-coral', 'qf-pine', 'qf-ember', 'qf-drift', 'qf-reprise', 'unnamed-queue', 'qf-literal'];
const value = (tracks = TRACKS, currentIndex = 2): QueueState => ({ ...createQueue(tracks), currentIndex });
const input = (page: Page) => page.getByRole('textbox', { name: 'Filter queue', exact: true }).or(page.getByRole('searchbox', { name: 'Filter queue', exact: true }));
const clear = (page: Page) => page.getByRole('button', { name: 'Clear queue filter', exact: true });
const row = (page: Page, id: string) => page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
const current = (page: Page) => page.locator('[data-testid="queue-item"][aria-current="true"]');
const order = (page: Page) => page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));

async function filter(page: Page, query: string) {
	await expect(input(page), 'queue-only labeled input is missing').toBeVisible({ timeout: 1500 });
	await input(page).fill(query);
}

async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}

async function instrument(page: Page) {
	await page.addInitScript(() => {
		const probe = { media: [] as string[], storage: [] as string[], scroll: 0 };
		window.queueFilterProbe = probe;
		for (const method of ['play', 'pause', 'load'] as const) {
			Object.defineProperty(HTMLMediaElement.prototype, method, { configurable: true, value() {
				probe.media.push(method);
				return method === 'play' ? Promise.resolve() : undefined;
			} });
		}
		for (const method of ['setItem', 'removeItem', 'clear'] as const) {
			const native = Storage.prototype[method];
			Object.defineProperty(Storage.prototype, method, { configurable: true, value(...args: unknown[]) {
				probe.storage.push(method);
				return Reflect.apply(native, this, args);
			} });
		}
		const native = Element.prototype.scrollIntoView;
		Element.prototype.scrollIntoView = function (options) { probe.scroll += 1; return native.call(this, options); };
	});
}

async function resetProbe(page: Page) {
	await settle(page);
	await page.evaluate(() => {
		window.queueFilterProbe.media.length = 0;
		window.queueFilterProbe.storage.length = 0;
		window.queueFilterProbe.scroll = 0;
	});
}

/** No test is allowed to reach a backend or another host, even on failure. */
async function localOnly(page: Page, origin: string, payload?: { current: unknown; status?: number }, calls: string[] = []) {
	const baseURL = new URL(origin);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(baseURL.hostname);
	await page.route('**/*', async (route) => {
		const request = route.request();
		const target = new URL(request.url());
		if (target.pathname.startsWith('/api/')) {
			calls.push(`${request.method()} ${target.pathname}`);
			if (target.origin === baseURL.origin && payload && target.pathname === '/api/library' && request.method() === 'GET') {
				return route.fulfill({ status: payload.status ?? 200, contentType: 'application/json', body: JSON.stringify(payload.current) });
			}
			return route.abort();
		}
		if (target.origin !== baseURL.origin || request.resourceType() === 'media') return route.abort();
		return route.continue();
	});
}

async function replace(page: Page, next = value()) {
	await page.evaluate((state) => window.queueFilter.replace(state), next);
}

async function seed(page: Page, baseURL: string, payload = { current: ITEMS as unknown, status: 200 }, calls: string[] = []) {
	await instrument(page);
	await localOnly(page, baseURL, payload, calls);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(7);
	for (const item of ITEMS) {
		await page.locator(`[data-testid="library-item"][data-id="${item.id}"] button.nt-library-row`).dispatchEvent('click');
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(7);
	await page.getByTestId('player-next').click();
	await page.getByTestId('player-next').click();
	await expect(current(page)).toHaveAttribute('data-track-id', 'qf-ember');
}

async function unchangedEffects(page: Page) {
	const probe = await page.evaluate(() => window.queueFilterProbe);
	expect(probe.media).toEqual([]);
	expect(probe.storage).toEqual([]);
}

async function count(page: Page, text: string) {
	await expect(page.getByTestId('queue-filter-count')).toHaveText(text);
}

// Hand-derived nonadjacent matching vectors, independent of any product matcher.
for (const example of [
	{ id: 'QFILTER-C01', query: '  cOrAl  ', ids: ['qf-coral', 'qf-reprise'] },
	{ id: 'QFILTER-C02', query: '  mIrA  ', ids: ['qf-coral', 'qf-drift'] },
	{ id: 'QFILTER-C03', query: '  ', ids: ORDER },
	{ id: 'QFILTER-C04', query: ' UNNAMED-QUEUE ', ids: ['unnamed-queue'] },
	{ id: 'QFILTER-C05', query: '<b>', ids: ['qf-literal'] },
	{ id: 'QFILTER-C06', query: 'not-in-this-queue', ids: [] },
	{ id: 'QFILTER-C07', query: 'undefined', ids: [] },
	{ id: 'QFILTER-C08', query: 'ember', ids: ['qf-ember'] }
]) {
	test(`${example.id} title/artist/fallback matching: ${JSON.stringify(example.query)}`, async ({ componentPage: page }) => {
		const state = value();
		await replace(page, state);
		await filter(page, example.query);
		expect(await order(page)).toEqual(example.ids);
		await count(page, `${example.ids.length} of 7 tracks`);
		await expect(page.getByTestId('queue-position')).toContainText('Track 3 of 7, 4 upcoming');
		expect(await page.evaluate(() => window.queueFilter.inspect())).toEqual({ state, calls: [] });
		if (example.ids.includes('qf-ember')) await expect(current(page)).toHaveAttribute('data-track-id', 'qf-ember');
		else await expect(current(page)).toHaveCount(0);
		if (example.ids.length === 0) await expect(page.getByTestId('queue-filter-empty')).toContainText(/no matches/i);
		if (example.id === 'QFILTER-C04') await expect(row(page, 'unnamed-queue')).toContainText('unnamed-queue');
		if (example.id === 'QFILTER-C05') await expect(row(page, 'qf-literal').locator('b')).toHaveCount(0);
	});
}

for (const example of [
	{ id: 'QFILTER-C09', label: 'Move Drift Evening up', action: 'move', index: 3, to: 2 },
	{ id: 'QFILTER-C10', label: 'Move Drift Evening down', action: 'move', index: 3, to: 4 },
	{ id: 'QFILTER-C11', label: 'Remove Drift Evening from queue', action: 'remove', index: 3 },
	{ id: 'QFILTER-C12', label: 'Play Drift Evening next', action: 'play-next', index: 3 }
]) {
	test(`${example.id} filtered action ${example.action} reports original index and identity`, async ({ componentPage: page }) => {
		const state = value();
		await replace(page, state);
		await filter(page, 'mira');
		expect(await order(page)).toEqual(['qf-coral', 'qf-drift']);
		const control = row(page, 'qf-drift').getByRole('button', { name: example.label, exact: true });
		await expect(control).toBeEnabled();
		await control.click();
		const expected = { action: example.action, index: example.index, id: 'qf-drift', ...('to' in example ? { to: example.to } : {}) };
		expect(await page.evaluate(() => window.queueFilter.inspect())).toEqual({ state, calls: [expected] });
	});
}

test('QFILTER-C13 full-queue edge and current boundaries ignore the filtered row positions', async ({ componentPage: page }) => {
	await replace(page);
	await filter(page, 'mira');
	await expect(row(page, 'qf-coral').getByRole('button', { name: 'Move Coral Night up', exact: true })).toBeDisabled();
	await expect(row(page, 'qf-drift').getByRole('button', { name: 'Move Drift Evening down', exact: true })).toBeEnabled();
	await filter(page, '<b>');
	await expect(row(page, 'qf-literal').getByRole('button', { name: 'Move <b>Literal & Song</b> down', exact: true })).toBeDisabled();
	await expect(row(page, 'qf-literal').getByRole('button', { name: 'Move <b>Literal & Song</b> up', exact: true })).toBeEnabled();
	await filter(page, 'ember');
	await expect(current(page)).toHaveAttribute('data-track-id', 'qf-ember');
	const playNext = row(page, 'qf-ember').getByRole('button', { name: 'Play Ember Noon next', exact: true });
	// Existing contract permits an enabled no-op on the current row.
	if (await playNext.isEnabled()) await playNext.click();
	const snapshot = await page.evaluate(() => window.queueFilter.inspect());
	expect(snapshot.state).toEqual(value());
	expect(snapshot.calls.every((call) => call.action === 'play-next' && call.index === 2 && call.id === 'qf-ember')).toBe(true);
});

test('QFILTER-C14 background replacement preserves query/focus, while explicit reveal clears a hiding filter', async ({ componentPage: page }) => {
	await replace(page);
	await filter(page, '  mira  ');
	await input(page).focus();
	await resetProbe(page);
	const updated = value([TRACKS[0], TRACKS[2], TRACKS[3], TRACKS[4], TRACKS[5], TRACKS[6]], 1);
	await replace(page, updated);
	await settle(page);
	await expect(input(page)).toHaveValue('  mira  ');
	await expect(input(page)).toBeFocused();
	await count(page, '2 of 6 tracks');
	await expect(current(page)).toHaveCount(0);
	await expect(page.getByTestId('queue-position')).toContainText('Track 2 of 6, 4 upcoming');
	expect((await page.evaluate(() => window.queueFilterProbe)).scroll).toBe(0);
	const reveal = page.getByRole('button', { name: 'Show current track', exact: true });
	await expect(reveal).toBeEnabled();
	await reveal.focus();
	await page.keyboard.press('Enter');
	await expect(input(page)).toHaveValue('');
	await expect(current(page)).toHaveAttribute('data-track-id', 'qf-ember');
	await expect(current(page)).toBeFocused();
	await expect(current(page)).toBeInViewport();
	expect(await page.evaluate(() => window.queueFilter.inspect())).toEqual({ state: updated, calls: [] });
	await unchangedEffects(page);
});

test('QFILTER-C15 clearing a no-match query restores all rows, never an empty-queue announcement', async ({ componentPage: page }) => {
	await replace(page);
	await filter(page, 'missing');
	await count(page, '0 of 7 tracks');
	await expect(page.getByTestId('queue-filter-empty')).toContainText(/no matches/i);
	await expect(page.getByTestId('now-playing')).toContainText('Ember Noon');
	await expect(page.getByTestId('player-next')).toBeEnabled();
	await expect(clear(page)).toBeVisible();
	await clear(page).focus();
	await page.keyboard.press('Space');
	await expect(input(page)).toHaveValue('');
	expect(await order(page)).toEqual(ORDER);
	await expect(page.getByTestId('queue-filter-empty')).toHaveCount(0);
	await replace(page, createQueue([]));
	await count(page, '0 of 0 tracks');
	await expect(page.getByTestId('now-playing')).toContainText(/queue is empty/i);
	await expect(page.getByTestId('queue-filter-empty')).toHaveCount(0);
});

test('QFILTER-C16 same-title tracks retain distinct IDs and original callback slots', async ({ componentPage: page }) => {
	const state = value([TRACKS[0], TRACKS[1], { ...TRACKS[2], title: 'Coral Night' }, TRACKS[3]], 1);
	await replace(page, state);
	await filter(page, 'coral');
	expect(await order(page)).toEqual(['qf-coral', 'qf-ember']);
	await row(page, 'qf-ember').getByRole('button', { name: 'Remove Coral Night from queue', exact: true }).click();
	expect(await page.evaluate(() => window.queueFilter.inspect())).toEqual({ state, calls: [{ action: 'remove', index: 2, id: 'qf-ember' }] });
});

test('QFILTER-C17 long-queue background append/removal/empty snapshots do not clear query or steal focus', async ({ componentPage: page }) => {
	const long = Array.from({ length: 31 }, (_, index) => ({ id: `qf-long-${index}`, title: `Long Song ${index}`, artist: index % 2 ? 'Mira' : 'Other', streamUrl: '' }));
	await replace(page, value(long, 15));
	await filter(page, '  Mira  ');
	await input(page).focus();
	await resetProbe(page);
	await replace(page, value([...long, { id: 'new-match', title: 'New Song', artist: 'Mira', streamUrl: '' }], 15));
	await count(page, '16 of 32 tracks');
	await expect(current(page)).toHaveAttribute('data-track-id', 'qf-long-15');
	await replace(page, value(long.filter((track) => track.id !== 'qf-long-15'), 15));
	await count(page, '14 of 30 tracks');
	await expect(current(page)).toHaveCount(0);
	await expect(page.getByTestId('queue-position')).toContainText('Track 16 of 30, 14 upcoming');
	await replace(page, createQueue([]));
	await count(page, '0 of 0 tracks');
	await expect(input(page)).toHaveValue('  Mira  ');
	await expect(input(page)).toBeFocused();
	await expect(page.getByTestId('queue-filter-empty')).toHaveCount(0);
	expect(await page.evaluate(() => window.queueFilter.inspect())).toEqual({ state: createQueue([]), calls: [] });
	expect((await page.evaluate(() => window.queueFilterProbe)).scroll).toBe(0);
	await unchangedEffects(page);
});

test('QFILTER-C18 named tracks match display title/artist, not hidden IDs', async ({ componentPage: page }) => {
	await replace(page);
	await filter(page, 'qf-pine');
	await count(page, '0 of 7 tracks');
	expect(await order(page)).toEqual([]);
});

test('QFILTER-C00 control: fixture mounts actual component, freezes state and observes real callback', async ({ componentPage: page }) => {
	await replace(page);
	expect(await order(page)).toEqual(ORDER);
	await expect(current(page)).toHaveAttribute('data-track-id', 'qf-ember');
	await page.getByTestId('player-next').click();
	expect(await page.evaluate(() => window.queueFilter.inspect())).toEqual({ state: value(), calls: [{ action: 'next' }] });
});

test('QFILTER-B01 typing/clearing is transient: no queue, media, persistence, network or library changes', async ({ page, baseURL }) => {
	const calls: string[] = [];
	await seed(page, baseURL!, undefined, calls);
	const announcement = await page.getByTestId('now-playing').textContent();
	const summary = await page.getByTestId('queue-position').textContent();
	const stored = await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage } }));
	await resetProbe(page);
	calls.length = 0;
	const requests: string[] = [];
	page.on('request', (request) => requests.push(`${request.method()} ${new URL(request.url()).pathname}`));
	for (const query of ['mira', 'missing', 'ember', '   ']) await filter(page, query);
	await input(page).focus();
	await page.keyboard.type('k j m r ');
	await expect(input(page)).toHaveValue('   k j m r ');
	await page.keyboard.press('ArrowLeft');
	await page.keyboard.press('ArrowRight');
	await page.keyboard.press('Enter');
	await expect(clear(page)).toBeVisible();
	await clear(page).click();
	await settle(page);
	expect(await order(page)).toEqual(ORDER);
	await expect(current(page)).toHaveAttribute('data-track-id', 'qf-ember');
	await expect(page.getByTestId('now-playing')).toHaveText(announcement!);
	await expect(page.getByTestId('queue-position')).toHaveText(summary!);
	await expect(page.getByTestId('library-item')).toHaveCount(7);
	expect(await page.evaluate(() => ({ local: { ...localStorage }, session: { ...sessionStorage } }))).toEqual(stored);
	await unchangedEffects(page);
	expect(calls).toEqual([]);
	expect(requests).toEqual([]);
	await expect(page.locator('audio')).toHaveCount(0);
});

test('QFILTER-B02 real filtered move/play-next/remove affect the original arranged tracks only', async ({ page, baseURL }) => {
	await seed(page, baseURL!);
	await filter(page, 'coral');
	await row(page, 'qf-reprise').getByRole('button', { name: 'Move Coral Reprise up', exact: true }).click();
	await expect(input(page)).toHaveValue('coral');
	await clear(page).click();
	expect(await order(page)).toEqual(['qf-coral', 'qf-pine', 'qf-ember', 'qf-reprise', 'qf-drift', 'unnamed-queue', 'qf-literal']);
	await filter(page, 'mira');
	await row(page, 'qf-drift').getByRole('button', { name: 'Play Drift Evening next', exact: true }).click();
	await clear(page).click();
	expect(await order(page)).toEqual(['qf-coral', 'qf-pine', 'qf-ember', 'qf-drift', 'qf-reprise', 'unnamed-queue', 'qf-literal']);
	await filter(page, 'coral');
	await row(page, 'qf-reprise').getByRole('button', { name: 'Remove Coral Reprise from queue', exact: true }).click();
	await count(page, '1 of 6 tracks');
	await clear(page).click();
	expect(await order(page)).toEqual(['qf-coral', 'qf-pine', 'qf-ember', 'qf-drift', 'unnamed-queue', 'qf-literal']);
	await expect(current(page)).toHaveAttribute('data-track-id', 'qf-ember');
	await page.getByTestId('player-next').click();
	await expect(current(page)).toHaveAttribute('data-track-id', 'qf-drift');
});

for (const boundary of ['storage', '401'] as const) {
	test(`QFILTER-B0${boundary === 'storage' ? '3' : '4'} ${boundary} auth boundary drops previous-account query`, async ({ page, baseURL }) => {
		const payload = { current: ITEMS as unknown, status: 200 };
		await seed(page, baseURL!, payload);
		await filter(page, 'private previous account text');
		if (boundary === 'storage') {
			await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
				key: 'nntune.library.auth-transition.v1', newValue: 'queue-filter-test', storageArea: localStorage
			})));
		} else {
			payload.status = 401;
			payload.current = { error: 'unauthorized' };
			await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		}
		await expect(page.getByRole('alert')).toBeVisible();
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		payload.status = 200;
		payload.current = [{ id: 'next-account', title: 'Next Account Song', artist: 'Other' }];
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(1);
		await expect(input(page)).toBeVisible();
		await expect(input(page)).toHaveValue('');
		await count(page, '0 of 0 tracks');
		await page.locator('[data-testid="library-item"][data-id="next-account"] button.nt-library-row').click();
		await expect(row(page, 'next-account')).toBeVisible();
		await count(page, '1 of 1 tracks');
	});
}

for (const theme of ['light', 'dark'] as const) {
	test(`QFILTER-B0${theme === 'light' ? '5' : '6'} ${theme} narrow touch/keyboard filter, current-hidden reveal and readable focus`, async ({ page, baseURL }, testInfo) => {
		await page.setViewportSize({ width: 360, height: 740 });
		await seed(page, baseURL!);
		await choose(page, theme);
		await expect(input(page)).toBeVisible({ timeout: 1500 });
		await input(page).tap();
		await page.keyboard.type('mira');
		await expect(current(page)).toHaveCount(0);
		await count(page, '2 of 7 tracks');
		await input(page).focus();
		await page.keyboard.press('Tab');
		await page.keyboard.press('Shift+Tab');
		await expect(input(page)).toBeFocused();
		const focus = await input(page).evaluate((element) => {
			const css = getComputedStyle(element);
			return element.matches(':focus-visible') && ((css.outlineStyle !== 'none' && parseFloat(css.outlineWidth) > 0) || css.boxShadow !== 'none');
		});
		expect(focus, 'keyboard filter focus needs a visible ring').toBe(true);
		await assertReadable(input(page));
		for (const control of [input(page), clear(page)]) {
			const box = await control.boundingBox();
			expect(box).not.toBeNull();
			expect(box!.width).toBeGreaterThanOrEqual(24);
			expect(box!.height).toBeGreaterThanOrEqual(24);
			expect(box!.x).toBeGreaterThanOrEqual(0);
			expect(box!.x + box!.width).toBeLessThanOrEqual(361);
		}
		await clear(page).tap();
		await expect(input(page)).toHaveValue('');
		await input(page).tap();
		await page.keyboard.type('mira');
		await page.getByRole('button', { name: 'Show current track', exact: true }).tap();
		await expect(input(page)).toHaveValue('');
		await expect(current(page)).toBeFocused();
		await expect(current(page)).toBeInViewport();
		expect(await order(page)).toEqual(ORDER);
		expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
		await testInfo.attach(`queue-filter-${theme}-narrow`, { body: await page.screenshot(), contentType: 'image/png' });
	});
}

test('QFILTER-B07 same-account library refresh preserves filter text and untouched full queue orientation', async ({ page, baseURL }) => {
	const payload = { current: ITEMS as unknown, status: 200 };
	await seed(page, baseURL!, payload);
	await filter(page, 'mira');
	await input(page).focus();
	const summary = await page.getByTestId('queue-position').textContent();
	const announcement = await page.getByTestId('now-playing').textContent();
	// A DOM-dispatched refresh represents a background refresh without the
	// runner moving focus to Refresh. The real page/refresher still execute.
	const refreshed = page.waitForResponse((response) => new URL(response.url()).pathname === '/api/library');
	await page.getByRole('button', { name: 'Refresh', exact: true }).dispatchEvent('click');
	await refreshed;
	await settle(page);
	await expect(input(page)).toHaveValue('mira');
	await expect(input(page)).toBeFocused();
	await count(page, '2 of 7 tracks');
	await expect(page.getByTestId('queue-position')).toHaveText(summary!);
	await expect(page.getByTestId('now-playing')).toHaveText(announcement!);
	await clear(page).click();
	expect(await order(page)).toEqual(ORDER);
});

async function assertReadable(locator: Locator) {
	const contrast = await locator.evaluate((element) => {
		const rgb = (value: string) => value.match(/[\d.]+/g)?.map(Number) ?? [];
		const luminance = (value: number[]) => value.slice(0, 3).map((channel) => {
			const c = channel / 255;
			return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
		}).reduce((sum, c, i) => sum + c * [0.2126, 0.7152, 0.0722][i], 0);
		const foreground = rgb(getComputedStyle(element).color);
		let background: number[] = [];
		for (let node: Element | null = element; node; node = node.parentElement) {
			const color = rgb(getComputedStyle(node).backgroundColor);
			if (color.length === 3 || color[3] === 1) { background = color; break; }
		}
		if (foreground.length < 3 || background.length < 3) return 0;
		const [bright, dark] = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
		return (bright + 0.05) / (dark + 0.05);
	});
	expect(contrast, 'typed text must have WCAG AA normal-text contrast').toBeGreaterThanOrEqual(4.5);
}
