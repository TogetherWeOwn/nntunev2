import { expect, test as base, type Page } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { createQueue, type QueueState, type Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/queue-direct-selection/mount.js';

/**
 * PICK — protected test-only handoff. No live API, provider, audio or deployment.
 * Companion: src/lib/player/playerQueue.direct-selection.acceptance.test.ts.
 * Contract: optional onselect(originalQueueIndex), native "Select {title}" buttons,
 * metadata-only selection, unchanged arrangement, concise polite selection feedback.
 * The mounted real component observes callback coordinates without applying them;
 * /library tests observe the real host applying its existing validated transition.
 * Spread props let pre-feature code compile; missing controls fail assertions.
 *
 * Run from repo root (Node 24/pnpm 10; free preview port 4173):
 *   pnpm --dir apps/web install --frozen-lockfile
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web test:e2e tests/library-queue-direct-selection.spec.ts --workers=1
 * Also run web check, companion unit tests and the protected library-player-queue,
 * library-queue-manage and library-queue-orientation suites. Do not weaken those
 * files or this test bank. Builder owns implementation and the combined PR.
 *
 * Breaks caught: inert callbacks, displayed-index selection, stale removed rows,
 * host state mutation in the child, accidental Play-now/stream fetching/storage,
 * rearranging to select, incorrect Next boundary, focus theft, account leakage.
 */
const test = base.extend<{ componentPage: Page }, { componentOrigin: string }>({
	componentOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/queue-direct-selection/', import.meta.url).pathname),
			publicDir: false,
			plugins: [svelte({ configFile: false, compilerOptions: { runes: true } })],
			server: { host: '127.0.0.1', port: 0, fs: { allow: [decodeURIComponent(new URL('../', import.meta.url).pathname)] } }
		});
		try {
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('missing component TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally { await server.close(); }
	}, { scope: 'worker' }],
	componentPage: async ({ page, componentOrigin }, use) => {
		await localOnly(page, componentOrigin);
		await page.goto(componentOrigin);
		await expect.poll(() => page.evaluate(() => Boolean(window.queuePick))).toBe(true);
		await use(page);
	}
});

const TRACKS: Track[] = [
	{ id: 'pick-mike', title: 'Mike Song', artist: 'Pick Band', streamUrl: '', artworkUrl: null },
	{ id: 'pick-zulu', title: 'Zulu Song', artist: 'Pick Band', streamUrl: '', artworkUrl: null },
	{ id: 'pick-alpha', title: 'Alpha Song', artist: 'Pick Band', streamUrl: '', artworkUrl: null }
];
const IDS = ['pick-mike', 'pick-zulu', 'pick-alpha'];
/** Session key LIBQR persists the arranged queue under. */
const QUEUE_KEY = 'nntune.library.queue.v1';
const LIBRARY = TRACKS.map(({ id, title, artist }) => ({ id, title, artist }));
const item = (page: Page, id: string) => page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
const action = (page: Page, id: string, name: string) => item(page, id).getByRole('button', { name, exact: true });
const select = (page: Page, id: string, title: string) => action(page, id, `Select ${title}`);
const current = (page: Page) => page.locator('[data-testid="queue-item"][aria-current="true"]');

/** Abort every foreign host and fulfill every API locally, including unexpected ones. */
async function localOnly(page: Page, origin: string, payload = { current: LIBRARY, status: 200 }, calls: string[] = []) {
	const local = new URL(origin);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(local.hostname);
	await page.route('**/*', (route) => {
		const request = route.request();
		const url = new URL(request.url());
		if (url.origin !== local.origin || url.pathname.startsWith('/api/') || request.resourceType() === 'media') {
			calls.push(`${request.method()} ${url.origin}${url.pathname}`);
			if (url.origin === local.origin && url.pathname === '/api/library' && request.method() === 'GET') {
				return route.fulfill({ status: payload.status, contentType: 'application/json', body: JSON.stringify(payload.current) });
			}
			return route.abort();
		}
		return route.continue();
	});
}

async function instrument(page: Page) {
	await page.addInitScript(() => {
		const probe = window.pickProbe = { media: [] as string[], sources: [] as string[], storage: [] as string[] };
		for (const method of ['play', 'pause', 'load'] as const) {
			Object.defineProperty(HTMLMediaElement.prototype, method, { configurable: true, value() {
				probe.media.push(method);
				return method === 'play' ? Promise.resolve() : undefined;
			} });
		}
		for (const prototype of [HTMLMediaElement.prototype, HTMLSourceElement.prototype]) {
			const original = Object.getOwnPropertyDescriptor(prototype, 'src')!;
			Object.defineProperty(prototype, 'src', { ...original, set(value: string) {
				probe.sources.push(value); original.set!.call(this, value);
			} });
		}
		const create = Document.prototype.createElement;
		Document.prototype.createElement = function (...args: Parameters<typeof create>) {
			if (/^(audio|source)$/i.test(args[0])) probe.sources.push(`create:${args[0]}`);
			return Reflect.apply(create, this, args);
		};
		const attribute = Element.prototype.setAttribute;
		Element.prototype.setAttribute = function (name, value) {
			if (name.toLowerCase() === 'src' && this.matches('audio, video, source')) probe.sources.push(value);
			return attribute.call(this, name, value);
		};
		for (const method of ['setItem', 'removeItem', 'clear'] as const) {
			const original = Storage.prototype[method];
			Object.defineProperty(Storage.prototype, method, { configurable: true, value(...args: unknown[]) {
				const area = this === window.localStorage ? 'local' : 'session';
				probe.storage.push(args.length > 0 ? `${area}:${method}:${String(args[0])}` : `${area}:${method}`);
				return Reflect.apply(original, this, args);
			} });
		}
	});
}

async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}
async function replace(page: Page, state: QueueState, available = true) {
	await page.evaluate(({ state, available }) => window.queuePick.replace(state, available), { state, available });
}
async function order(page: Page) {
	return page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')));
}
async function selected(page: Page, id: string, title: string, position: number) {
	await expect(current(page)).toHaveCount(1);
	await expect(current(page)).toHaveAttribute('data-track-id', id);
	await expect(page.getByTestId('now-playing')).toContainText(title);
	await expect(page.getByTestId('queue-position')).toContainText(`Track ${position} of 3`);
	expect(await order(page)).toEqual(IDS);
}
async function seed(page: Page, baseURL: string, payload = { current: LIBRARY, status: 200 }, calls: string[] = []) {
	await instrument(page);
	await localOnly(page, baseURL, payload, calls);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	for (const id of IDS) await page.locator(`[data-testid="library-item"][data-id="${id}"] button.nt-library-row`).click();
	await selected(page, 'pick-mike', 'Mike Song', 1);
}

// Control is green before the feature: proves mount, prop replacement and real
// existing callback operation; feature failures below are not harness failures.
test('PICK-C00 real component mount and Next callback control', async ({ componentPage: page }) => {
	await replace(page, createQueue(TRACKS));
	await expect(current(page)).toHaveAttribute('data-track-id', 'pick-mike');
	await page.getByTestId('player-next').click();
	expect((await page.evaluate(() => window.queuePick.inspect())).nextCalls).toBe(1);
});

test('PICK-C01 selection is delegated once with original coordinates, not applied inside the component', async ({ componentPage: page }) => {
	const value = createQueue(TRACKS);
	await replace(page, value);
	const target = select(page, 'pick-alpha', 'Alpha Song');
	await expect(target).toBeVisible();
	await target.click();
	expect(await page.evaluate(() => window.queuePick.inspect())).toEqual({ state: value, calls: [2], nextCalls: 0 });
	await expect(current(page)).toHaveAttribute('data-track-id', 'pick-mike');
});

test('PICK-C02 callback removal disables or removes the action on the same mount', async ({ componentPage: page }) => {
	const value = createQueue(TRACKS);
	await replace(page, value);
	await expect(select(page, 'pick-alpha', 'Alpha Song')).toBeVisible();
	await replace(page, value, false);
	const target = select(page, 'pick-alpha', 'Alpha Song');
	if (await target.count()) {
		await expect(target).toBeDisabled();
		await target.evaluate((button) => (button as HTMLButtonElement).click());
	}
	expect(await page.evaluate(() => window.queuePick.inspect())).toEqual({ state: value, calls: [], nextCalls: 0 });
});

test('PICK-C03 background reorder uses fresh coordinates without stealing focus; removed identity cannot select its replacement', async ({ componentPage: page }) => {
	await replace(page, createQueue(TRACKS));
	const target = select(page, 'pick-alpha', 'Alpha Song');
	await expect(target).toBeVisible();
	const oldButton = await target.elementHandle();
	await page.getByTestId('pick-sentinel').focus();
	const rearranged = { ...createQueue([TRACKS[2]!, TRACKS[0]!, TRACKS[1]!]), currentIndex: 1 };
	await replace(page, rearranged);
	await expect(page.getByTestId('pick-sentinel')).toBeFocused();
	await target.click();
	expect((await page.evaluate(() => window.queuePick.inspect())).calls).toEqual([0]);
	const removed = createQueue([TRACKS[0]!, TRACKS[1]!]);
	await replace(page, removed);
	await expect(target).toHaveCount(0);
	await oldButton!.evaluate((button) => (button as HTMLButtonElement).click());
	await settle(page);
	expect(await page.evaluate(() => window.queuePick.inspect())).toEqual({ state: removed, calls: [], nextCalls: 0 });
});

for (const example of [
	{ id: 'PICK-B01', index: 0, title: 'Mike Song', track: 'pick-mike', scheme: 'light', touch: false, key: 'Enter' },
	{ id: 'PICK-B02', index: 1, title: 'Zulu Song', track: 'pick-zulu', scheme: 'dark', touch: false, key: 'Space' },
	{ id: 'PICK-B03', index: 2, title: 'Alpha Song', track: 'pick-alpha', scheme: 'dark', touch: true, key: 'Enter' }
] as const) {
	test.describe(example.id, () => {
		test.use({ colorScheme: example.scheme, viewport: { width: 390, height: 844 }, hasTouch: example.touch });
		test('direct selection preserves arrangement, exposes honest feedback and Next follows the selection', async ({ page, baseURL }, testInfo) => {
			await seed(page, baseURL!);
			if (example.index === 0) await page.getByTestId('player-next').click();
			const target = select(page, example.track, example.title);
			await expect(target).toBeVisible();
			await expect(target).toBeEnabled();
			if (example.touch) await target.tap();
			else {
				await page.getByRole('button', { name: 'Refresh', exact: true }).focus();
				for (let tabs = 0; tabs < 70 && !(await target.evaluate((element) => element === document.activeElement)); tabs += 1) await page.keyboard.press('Tab');
				await expect(target).toBeFocused();
				await page.keyboard.press(example.key);
			}
			await selected(page, example.track, example.title, example.index + 1);
			const feedback = page.getByTestId('queue-announcement');
			await expect(feedback).toHaveAttribute('role', 'status');
			await expect(feedback).toContainText(example.title);
			await expect(feedback).toContainText(/select/i);
			await expect(feedback).not.toContainText(/play now|is playing|stream available/i);
			await page.getByRole('button', { name: 'Show current track', exact: true }).click();
			await expect(current(page)).toBeFocused();
			await expect(current(page)).toBeInViewport();
			expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
			await testInfo.attach('selection-runtime', { body: await page.screenshot(), contentType: 'image/png' });
			if (example.index === 2) {
				// Preserved boundary (QO-B04 + playerQueueContracts): Next stays
				// enabled at tail; activating it is a clamped no-op that keeps
				// selection, order and marker.
				const next = page.getByTestId('player-next');
				await expect(next).toBeEnabled();
				await next.click();
				await selected(page, example.track, example.title, example.index + 1);
			} else {
				await page.getByTestId('player-next').click();
				await selected(page, IDS[example.index + 1]!, example.index === 0 ? 'Zulu Song' : 'Alpha Song', example.index + 2);
			}
		});
	});
}

test('PICK-B04 selection and current-row reselection have zero transport, source, provider or non-queue persistence side effects', async ({ page, baseURL }) => {
	const calls: string[] = [];
	await seed(page, baseURL!, undefined, calls);
	const target = select(page, 'pick-zulu', 'Zulu Song');
	await expect(target).toBeVisible();
	await settle(page);
	calls.length = 0;
	await page.evaluate(() => { window.pickProbe.media = []; window.pickProbe.sources = []; window.pickProbe.storage = []; });
	await target.click();
	await selected(page, 'pick-zulu', 'Zulu Song', 2);
	const feedback = await page.getByTestId('queue-announcement').textContent();
	if (await target.isEnabled()) await target.click();
	await settle(page);
	await selected(page, 'pick-zulu', 'Zulu Song', 2);
	expect(await page.getByTestId('queue-announcement').textContent()).toBe(feedback);
	// Session queue persistence (LIBQR) is the only storage effect allowed: every
	// write is the queue key in sessionStorage and it holds the unchanged
	// arrangement with the selected track current.
	const probe = await page.evaluate(() => window.pickProbe);
	expect({ media: probe.media, sources: probe.sources }).toEqual({ media: [], sources: [] });
	for (const entry of probe.storage) expect(entry).toBe(`session:setItem:${QUEUE_KEY}`);
	expect(await page.evaluate((key) => window.sessionStorage.getItem(key), QUEUE_KEY)).toBe(
		JSON.stringify({ orderIds: IDS, currentId: 'pick-zulu' })
	);
	expect(calls).toEqual([]);
	expect(await page.locator('audio[src], video[src], source[src]').count()).toBe(0);
});

test('PICK-B05 selection survives background refresh and composes with management and disabled edges', async ({ page, baseURL }) => {
	await seed(page, baseURL!);
	const target = select(page, 'pick-zulu', 'Zulu Song');
	await expect(target).toBeVisible();
	await target.click();
	const refresh = page.getByRole('button', { name: 'Refresh', exact: true });
	await refresh.click();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await selected(page, 'pick-zulu', 'Zulu Song', 2);
	await expect(refresh).toBeFocused();
	await action(page, 'pick-zulu', 'Move Zulu Song up').click();
	expect(await order(page)).toEqual(['pick-zulu', 'pick-mike', 'pick-alpha']);
	await expect(current(page)).toHaveAttribute('data-track-id', 'pick-zulu');
	await expect(action(page, 'pick-zulu', 'Move Zulu Song up')).toBeDisabled();
	await expect(action(page, 'pick-alpha', 'Move Alpha Song down')).toBeDisabled();
	await action(page, 'pick-mike', 'Remove Mike Song from queue').click();
	await page.getByTestId('player-next').click();
	await expect(current(page)).toHaveAttribute('data-track-id', 'pick-alpha');
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('player-next')).toBeDisabled();
	await expect(page.getByRole('button', { name: /^Select (Mike|Zulu|Alpha) Song$/ })).toHaveCount(0);
});

for (const boundary of ['auth event', '401'] as const) {
	test(`PICK-B0${boundary === '401' ? '7' : '6'} ${boundary} clears selected identity and stale controls before next-account Retry`, async ({ page, baseURL }) => {
		const payload = { current: LIBRARY, status: 200 };
		await seed(page, baseURL!, payload);
		const target = select(page, 'pick-alpha', 'Alpha Song');
		await expect(target).toBeVisible();
		await target.click();
		const oldButton = await target.elementHandle();
		if (boundary === '401') {
			payload.status = 401;
			await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		} else {
			await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', { key: 'nntune.library.auth-transition.v1', newValue: 'changed', storageArea: localStorage })));
		}
		await expect(page.getByRole('alert')).toContainText(/session changed|signed out|do not have access/i);
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		payload.status = 200;
		payload.current = [{ id: 'pick-bravo', title: 'Bravo Song', artist: 'Next Account' }];
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(1);
		await page.locator('[data-testid="library-item"][data-id="pick-bravo"] button.nt-library-row').click();
		await oldButton!.evaluate((button) => (button as HTMLButtonElement).click());
		await expect(current(page)).toHaveAttribute('data-track-id', 'pick-bravo');
		expect(await order(page)).toEqual(['pick-bravo']);
		await expect(page.getByRole('button', { name: 'Select Alpha Song', exact: true })).toHaveCount(0);
	});
}
