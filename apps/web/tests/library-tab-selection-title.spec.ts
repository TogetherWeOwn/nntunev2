import { expect, test as base, type Page, type TestInfo } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { QueueState, Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/library-tab-title/mount.js';

/**
 * LIBTITLE protected acceptance bank (tests only; ship with the Builder's GREEN
 * implementation, not as a separate failing PR). Protected: this spec,
 * fixtures/library-tab-title/* and src/routes/library/tab-title.acceptance.test.ts.
 *
 * Canonical commands from apps/web (Node 24, cached pnpm/Chromium):
 *   pnpm check
 *   pnpm test
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm exec playwright test tests/library-tab-selection-title.spec.ts --workers=1 --retries=0
 * Preview port 4173 must be free; never reuse another worktree's preview.
 * Existing system-media.playwright.config.ts provides an isolated-port option.
 * Also preserve library-native-now-playing, library-signin-recovery,
 * library-queue-direct-selection, library-queue-manage and library-player-queue.
 *
 * Breaks caught: static/stale titles; claiming playback from metadata; dangling
 * artist separators; rendering raw metadata; removal/auth leaks; stale cleanup
 * clobbering newer navigation; hydration drift; title-induced transport/storage,
 * timer, focus, scroll or extra live-region effects.
 *
 * The isolated mount uses the REAL Library page and exports only its existing
 * replaceQueue input through a test-only Vite transform. It supplies no title
 * helper/implementation. Most cases use real library/queue controls; the seam
 * exercises rapid replacement and unmount, which public controls cannot stage.
 * All APIs are mocked, foreign requests/media blocked. No provider credentials,
 * production calls, real audio, source-text assertions or new production API.
 */
const BASELINE = 'Library — NNTune';
const CORAL_TITLE = 'Selected: Coral Night — Mira North — Library — NNTune';
const PINE_TITLE = 'Selected: Pine Morning — South Ensemble — Library — NNTune';
const TRACKS: Track[] = [
	{ id: 'title-coral', title: 'Coral Night', artist: 'Mira North', streamUrl: '' },
	{ id: 'title-pine', title: 'Pine Morning', artist: 'South Ensemble', streamUrl: '' },
	{ id: 'title-ember', title: 'Ember Noon', artist: '', streamUrl: '' }
];
const CATALOGUE: Record<string, unknown>[] = TRACKS.map(({ id, title, artist }) => ({ id, title, artist }));
const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const routePath = `${webRoot}src/routes/library/+page.svelte`;
const fixtureRoot = decodeURIComponent(new URL('./fixtures/library-tab-title/', import.meta.url).pathname);
const test = base.extend<{ mountedLibrary: Page }, { libraryOrigin: string }>({
	libraryOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: fixtureRoot,
			publicDir: false,
			resolve: { alias: {
				'#lib': `${webRoot}src/lib`,
				'$app/navigation': `${fixtureRoot}navigation.ts`
			} },
			plugins: [{
				name: 'acceptance-library-title-input', enforce: 'pre',
				transform(code, id) {
					if (id !== routePath) return;
					return code.replace('</script>', '\nexport function acceptanceReplaceQueue(next: QueueState) { replaceQueue(next); }\n</script>');
				}
			}, svelte({ configFile: false, compilerOptions: { runes: true } })],
			server: { host: '127.0.0.1', port: 0, fs: { allow: [webRoot] } }
		});
		try {
			await server.listen();
			const address = server.httpServer?.address();
			if (!address || typeof address === 'string') throw new Error('missing fixture TCP address');
			await use(`http://127.0.0.1:${address.port}`);
		} finally { await server.close(); }
	}, { scope: 'worker' }],
	mountedLibrary: async ({ page, libraryOrigin }, use) => {
		await localOnly(page, libraryOrigin);
		await page.goto(libraryOrigin);
		await expect(page.getByTestId('library-item')).toHaveCount(3);
		await expect.poll(() => page.evaluate(() => Boolean(window.libraryTabTitle))).toBe(true);
		await use(page);
	}
});

test.setTimeout(20_000);

async function localOnly(page: Page, origin: string, payload = { items: CATALOGUE, status: 200 }, calls: string[] = []) {
	const local = new URL(origin);
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(local.hostname);
	await page.route('**/*', (route) => {
		const request = route.request();
		const url = new URL(request.url());
		if (url.origin !== local.origin || url.pathname.startsWith('/api/') || request.resourceType() === 'media') {
			calls.push(`${request.method()} ${url.origin}${url.pathname}`);
			if (url.origin === local.origin && url.pathname === '/api/library' && request.method() === 'GET') {
				return route.fulfill({ status: payload.status, contentType: 'application/json', body: JSON.stringify(payload.items) });
			}
			return route.abort();
		}
		return route.continue();
	});
}

async function settle(page: Page) {
	await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}
const queueRow = (page: Page, id: string) => page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`);
const add = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"] button.nt-library-row`).click();
async function seed(page: Page, origin: string, payload = { items: CATALOGUE, status: 200 }, calls: string[] = []) {
	await localOnly(page, origin, payload, calls);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(payload.items.length);
}
async function replace(page: Page, states: QueueState[]) {
	await page.evaluate((values) => window.libraryTabTitle.replace(values), states);
	await settle(page);
}
const state = (currentIndex = 0, tracks = TRACKS): QueueState => ({ tracks, currentIndex });

async function check(info: TestInfo, expected: unknown, actual: unknown) {
	let passed = false;
	try {
		expect(actual).toEqual(expected);
		passed = true;
	} finally {
		const receipt = JSON.stringify({
			case_id: info.title.split(' ')[0], expected, actual,
			status: passed ? 'GREEN' : 'RED', error_code: passed ? null : 'LIBRARY_TAB_SELECTION_TITLE',
			next_action: passed ? 'preserve protected acceptance' : 'implement selection title lifecycle; do not edit protected tests'
		});
		console.log(receipt);
		await info.attach('acceptance-case.json', { body: receipt, contentType: 'application/json' });
	}
}

test('LIBTITLE-01 client-rendered library starts at the exact baseline without hydration errors', async ({ page, baseURL }, info) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('console', (message) => { if (/hydration/i.test(message.text())) errors.push(message.text()); });
	await localOnly(page, baseURL!);
	const response = await page.goto('/library');
	const html = await response!.text();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await settle(page);
	// The shipped loader intentionally sets ssr=false. Direct SSR safety/head
	// rendering is covered by LIBTITLE-18/19, not invented for the HTTP shell.
	await check(info, { serverLeaksSelection: false, clientTitle: BASELINE, errors: [] }, {
		serverLeaksSelection: /<title[^>]*>Selected:/.test(html), clientTitle: await page.title(), errors
	});
});

test('LIBTITLE-02 metadata-only add identifies selection without claiming playback or leaking extra fields', async ({ page, baseURL }, info) => {
	const items = [{ ...CATALOGUE[0], token: 'PRIVATE_TITLE_CANARY', account: 'account-canary', url: 'https://provider.invalid/private' }];
	await seed(page, baseURL!, { items, status: 200 });
	await add(page, 'title-coral');
	await expect(queueRow(page, 'title-coral')).toHaveAttribute('aria-current', 'true');
	await settle(page);
	await check(info, { title: CORAL_TITLE, mediaSources: 0 }, {
		title: await page.title(), mediaSources: await page.locator('audio[src], video[src], source[src]').count()
	});
});

test('LIBTITLE-03 real direct selection and Next follow the current identity, not first or last row', async ({ page, baseURL }, info) => {
	await seed(page, baseURL!);
	for (const track of TRACKS) await add(page, track.id);
	const titles: string[] = [];
	await settle(page); titles.push(await page.title());
	await queueRow(page, 'title-pine').getByRole('button', { name: 'Select Pine Morning', exact: true }).click();
	await settle(page); titles.push(await page.title());
	await page.getByTestId('player-next').click();
	await settle(page); titles.push(await page.title());
	await check(info, [CORAL_TITLE, PINE_TITLE, 'Selected: Ember Noon — Library — NNTune'], titles);
});

for (const vector of [
	{ id: 'LIBTITLE-04', track: { id: 'title-solo', title: 'Solo Song' }, want: 'Selected: Solo Song — Library — NNTune' },
	{ id: 'LIBTITLE-05', track: { id: 'title-blank', title: ' \t\n ', artist: ' \t ' }, want: 'Selected: title-blank — Library — NNTune' },
	{ id: 'LIBTITLE-06', track: { id: 'title-malformed', title: { token: 'PRIVATE_TITLE_CANARY' }, artist: ['account-canary'] }, want: 'Selected: title-malformed — Library — NNTune' },
	{ id: 'LIBTITLE-07', track: { id: 'title-null', title: null, artist: 42 }, want: 'Selected: title-null — Library — NNTune' },
	{ id: 'LIBTITLE-08', track: { id: 'title-literal', title: '<b>雪 & Glow</b>', artist: 'Björk — Duo' }, want: 'Selected: <b>雪 & Glow</b> — Björk — Duo — Library — NNTune' }
]) {
	test(`${vector.id} canonical display fallback and literal text for ${vector.track.id}`, async ({ page, baseURL }, info) => {
		await seed(page, baseURL!, { items: [vector.track], status: 200 });
		await add(page, vector.track.id);
		await expect(queueRow(page, vector.track.id)).toHaveAttribute('aria-current', 'true');
		await settle(page);
		await check(info, vector.want, await page.title());
	});
}

test('LIBTITLE-09 removal changes selected title and last removal/clear restore the exact baseline', async ({ page, baseURL }, info) => {
	await seed(page, baseURL!);
	await add(page, 'title-coral'); await add(page, 'title-pine');
	const titles: string[] = [];
	await queueRow(page, 'title-coral').getByRole('button', { name: 'Remove Coral Night from queue', exact: true }).click();
	await settle(page); titles.push(await page.title());
	await queueRow(page, 'title-pine').getByRole('button', { name: 'Remove Pine Morning from queue', exact: true }).click();
	await settle(page); titles.push(await page.title());
	await add(page, 'title-coral');
	await settle(page); titles.push(await page.title());
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await page.getByRole('button', { name: 'Yes, clear queue', exact: true }).click();
	await settle(page); titles.push(await page.title());
	await check(info, [PINE_TITLE, BASELINE, CORAL_TITLE, BASELINE], titles);
});

for (const boundary of ['401', '403', 'auth event'] as const) {
	test(`LIBTITLE-${boundary === '401' ? '10' : boundary === '403' ? '11' : '12'} ${boundary} removes private selected identity before next-account retry`, async ({ page, baseURL }, info) => {
		const payload = { items: CATALOGUE, status: 200 };
		await seed(page, baseURL!, payload);
		await add(page, 'title-coral');
		await settle(page);
		const before = await page.title();
		if (boundary === 'auth event') {
			await page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
				key: 'nntune.library.auth-transition.v1', newValue: 'changed', storageArea: localStorage
			})));
		} else {
			payload.status = Number(boundary);
			await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		}
		await expect(page.getByRole('alert')).toBeVisible();
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await settle(page);
		const denied = await page.title();
		payload.status = 200;
		payload.items = [{ id: 'title-new-account', title: 'Next Account Song' }];
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByTestId('library-item')).toHaveCount(1);
		await settle(page);
		const recovered = await page.title();
		await add(page, 'title-new-account'); await settle(page);
		await check(info, [CORAL_TITLE, BASELINE, BASELINE, 'Selected: Next Account Song — Library — NNTune'],
			[before, denied, recovered, await page.title()]);
	});
}

test('LIBTITLE-13 same-id metadata replacement and rapid replacements never leave a stale title', async ({ mountedLibrary: page }, info) => {
	await replace(page, [state()]);
	const before = await page.title();
	await replace(page, [state(0, [{ ...TRACKS[0]!, title: 'Corrected Coral', artist: '' }])]);
	const corrected = await page.title();
	await replace(page, [state(1), { tracks: [], currentIndex: -1 }, state(2), state(1)]);
	const final = await page.title();
	await check(info, [CORAL_TITLE, 'Selected: Corrected Coral — Library — NNTune', PINE_TITLE], [before, corrected, final]);
});

test('LIBTITLE-14 teardown restores the prior route title when the library still owns it', async ({ mountedLibrary: page }, info) => {
	await replace(page, [state()]);
	const selected = await page.title();
	await page.evaluate(() => window.libraryTabTitle.destroy()); await settle(page);
	await check(info, [CORAL_TITLE, 'Before library — fixture host'], [selected, await page.title()]);
});

test('LIBTITLE-15 teardown cannot overwrite a newer route title', async ({ mountedLibrary: page }, info) => {
	await replace(page, [state()]);
	const selected = await page.title();
	await page.evaluate(async () => {
		document.title = 'Newer route — owned elsewhere';
		await window.libraryTabTitle.destroy();
	});
	await settle(page);
	await check(info, [CORAL_TITLE, 'Newer route — owned elsewhere'], [selected, await page.title()]);
});

test('LIBTITLE-16 SPA navigation owns the destination title and a new library visit starts empty', async ({ page, baseURL }, info) => {
	await seed(page, baseURL!); await add(page, 'title-coral'); await settle(page);
	const selected = await page.title();
	await page.evaluate(() => { (window as unknown as { titleNavMarker: string }).titleNavMarker = 'same-document'; });
	const navigation = page.locator('nav:has(a[href="/library/import"])');
	await navigation.getByRole('link', { name: 'Select NZB locally', exact: true }).click();
	await expect(page).toHaveURL(/\/library\/import$/); await settle(page);
	const destination = await page.title();
	await navigation.getByRole('link', { name: 'Library', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(3);
	await expect(page.getByTestId('queue-item')).toHaveCount(0); await settle(page);
	await check(info, { titles: [CORAL_TITLE, 'NNTune — local NZB selection', BASELINE], marker: 'same-document' }, {
		titles: [selected, destination, await page.title()],
		marker: await page.evaluate(() => (window as unknown as { titleNavMarker: string }).titleNavMarker)
	});
});

test('LIBTITLE-17 title updates add no transport, storage, timers, focus, scroll or live-region topology effects', async ({ mountedLibrary: page }, info) => {
	await page.evaluate(() => {
		const probe = window.tabTitleProbe = { media: [], storage: [], timers: [], scroll: [] } as Window['tabTitleProbe'];
		for (const method of ['play', 'pause', 'load'] as const) {
			Object.defineProperty(HTMLMediaElement.prototype, method, { configurable: true, value() {
				probe.media.push(method); return method === 'play' ? Promise.resolve() : undefined;
			} });
		}
		const position = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'currentTime')!;
		Object.defineProperty(HTMLMediaElement.prototype, 'currentTime', { ...position, set(value: number) {
			probe.media.push('seek'); position.set!.call(this, value);
		} });
		for (const method of ['setItem', 'removeItem', 'clear'] as const) {
			const original = Storage.prototype[method];
			Object.defineProperty(Storage.prototype, method, { configurable: true, value(...args: unknown[]) {
				probe.storage.push(method); return Reflect.apply(original, this, args);
			} });
		}
		for (const method of ['setTimeout', 'setInterval'] as const) {
			const original = window[method];
			window[method] = ((...args: Parameters<typeof original>) => {
				probe.timers.push(method); return Reflect.apply(original, window, args);
			}) as typeof original;
		}
		Element.prototype.scrollIntoView = function () { probe.scroll.push('scrollIntoView'); };
	});
	await page.locator('#title-focus-anchor').focus();
	const live = () => page.locator('#app [aria-live], #app [role="status"], #app [role="alert"]').evaluateAll((nodes) =>
		nodes.map((node) => [node.getAttribute('data-testid'), node.getAttribute('role'), node.getAttribute('aria-live')]));
	// Mount the queue before comparison: its existing polite selection readout is
	// legitimate. Only extra effects introduced by title replacement are forbidden.
	await replace(page, [state()]);
	const topology = await live();
	const scrollPosition = await page.evaluate(() => [window.scrollX, window.scrollY]);
	const calls: string[] = [];
	page.on('request', (request) => calls.push(request.url()));
	await page.evaluate(() => { for (const entries of Object.values(window.tabTitleProbe)) entries.length = 0; });
	await replace(page, [state(1), state(2), state(1)]);
	await check(info, {
		title: PINE_TITLE, probe: { media: [], storage: [], timers: [], scroll: [] }, requests: [],
		focus: 'title-focus-anchor', topology, scrollPosition, mediaSources: 0,
		currentIds: ['title-pine'], order: ['title-coral', 'title-pine', 'title-ember']
	}, {
		title: await page.title(), probe: await page.evaluate(() => window.tabTitleProbe), requests: calls,
		focus: await page.evaluate(() => document.activeElement?.id), topology: await live(),
		scrollPosition: await page.evaluate(() => [window.scrollX, window.scrollY]),
		mediaSources: await page.locator('audio[src], video[src], source[src]').count(),
		currentIds: await page.locator('[data-testid="queue-item"][aria-current="true"]').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id'))),
		order: await page.getByTestId('queue-item').evaluateAll((rows) => rows.map((row) => row.getAttribute('data-track-id')))
	});
});
