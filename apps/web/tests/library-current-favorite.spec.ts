import { expect, test as base, type Locator, type Page, type TestInfo } from '@playwright/test';
import { createServer } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import type { QueueState, Track } from '../src/lib/player/queue.js';
import type {} from './fixtures/library-current-favorite/mount.js';

/**
 * LIBCURRENTSTAR — favorite the current track without leaving native playback.
 *
 * Protected: this spec + tests/fixtures/library-current-favorite/*. The paired
 * build lands the current-track favorite implementation on top of this branch
 * and must make these tests pass WITHOUT editing this file or the fixture
 * dir; a wrong test comes back to the acceptance lane instead of being fixed
 * beside the implementation. Never merge this RED bank by itself: the Builder
 * owns the combined GREEN PR through review and merge.
 *
 * Locked contract under test (so the builder never guesses):
 * - The Player region (native audio[controls] + the now-playing identity
 *   readout) carries exactly one current-track toggle: a button whose
 *   accessible name matches /star/i, named `Star {display title}` when the
 *   current loaded track is unstarred and `Unstar {display title}` when it is
 *   starred, mirroring the row starLabel, with aria-pressed reflecting state.
 *   Exact placement inside the region is free; outside the region is a fail.
 * - The toggle resolves ONLY the current loaded track id and reuses the
 *   single favorites source (favorites.ts load/save with the same
 *   auth-context scoping): player, browse row, shelf entry and queue toggle
 *   agree in both directions, one scoped storage key persists the id, and no
 *   new key, account lookup or server API appears.
 * - Toggling never replaces or reloads audio (same node, src, time, paused),
 *   seeks, autoplays, reorders/reselects the queue, or changes
 *   find/subset/order/view. Missing current track and auth teardown leave no
 *   actionable stale toggle and no outgoing-account favorite state.
 * - A player or queue action keeps its own still-mounted focus, including in
 *   the Favorites-only view; Favorites-only row removal keeps the existing
 *   recovery to the Show selector. Focus never lands on an unrelated
 *   selector just because the caller shares a handler.
 * - The toggle is keyboard operable (Tab-reachable, Enter/Space, visible
 *   focus ring), announces one concise polite outcome naming the track per
 *   action (replaced, never appended; never assertive), works by touch at
 *   320px in light and dark, and stays honest when storage is denied (no
 *   durability claim for a memory-only edit).
 *
 * Harness: the REAL library page mounted through a test-only Vite server with
 * a pre-transform adding ONE exported setter calling the page's existing
 * replaceQueue (same premise as the now-playing bank: real restored-state
 * rendering, real reducers, no invented streaming or controls). Fixture
 * titles/artists deliberately avoid the words "star", "favorite" and "queue"
 * so the /star/i toggle filters can only match a star toggle itself. HTTP
 * fixtures are local; media, other API and external requests are blocked.
 *
 * Every case ends in ONE final check() emitting compact JSON
 * (case_id/expected/actual/status/error_code/next_action) to the console and
 * an attachment. RED gates are final-check mismatches read through
 * non-throwing queries — never click timeouts or import failures. A case
 * that never reaches its check emits an ERROR receipt instead, which means
 * harness/action failure, not verified feature RED. Every toggle-dependent
 * case carries a non-vacuous gate (a mid-action starred marker) so a skipped
 * click cannot fake GREEN.
 *
 * Case IDs (stable): CF01 presence + naming + pressed, CF02 star syncs
 * row/shelf/queue + single scoped persist, CF03 unstar removes the shelf
 * entry with the queue untouched, CF04 later current-track selection follows
 * state both ways, CF05 Favorites-only focus for existing callers (row
 * recovery kept, queue keeps focus), CF06 player keeps its own focus,
 * CF07 audio/transport/controls untouched + no non-GET, CF08 missing-current
 * leaves no stale toggle (guard), CF09 auth teardown hides and purges,
 * CF10 denied storage stays memory-honest, CF11 keyboard + visible focus,
 * CF12/CF13 narrow touch at 320px light/dark, CF14 single polite outcome
 * replaced per action, CF15 reload persistence through the same scoped key.
 *
 * From apps/web (Node 24 / pnpm 10):
 *   pnpm install --frozen-lockfile
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm exec playwright test \
 *     tests/library-current-favorite.spec.ts --workers=1 --retries=0
 * The bank mounts its own component server and needs no preview/baseURL.
 * Expected baseline: CF08 GREEN as the missing-current guard; every other
 * case RED at its intended assertion (CF05 RED at queue focus through
 * existing controls; CF09/CF10/CF15 RED at the star gate).
 */

const routePath = decodeURIComponent(new URL('../src/routes/library/+page.svelte', import.meta.url).pathname);
const webRoot = decodeURIComponent(new URL('../', import.meta.url).pathname);
const test = base.extend<{ libraryPage: Page }, { libraryOrigin: string }>({
	libraryOrigin: [async ({}, use) => {
		const server = await createServer({
			configFile: false,
			root: decodeURIComponent(new URL('./fixtures/library-current-favorite/', import.meta.url).pathname),
			publicDir: false,
			resolve: { alias: {
				'#lib': `${webRoot}src/lib`,
				'$app/navigation': decodeURIComponent(new URL('./fixtures/library-current-favorite/navigation.ts', import.meta.url).pathname)
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
		await localOnly(page, libraryOrigin);
		await page.goto(libraryOrigin);
		await expect(page.getByTestId('library-item')).toHaveCount(TRACKS.length);
		await expect.poll(() => page.evaluate(() => Boolean(window.libraryCurrentFavorite))).toBe(true);
		await use(page);
	}
});

test.setTimeout(20_000);
test.use({ viewport: { width: 1000, height: 740 } });

const TRACKS: Track[] = [
	{ id: 'cf-harbor', title: 'Harbor Light', artist: 'North Ensemble', streamUrl: '/__acceptance_audio__/harbor.wav' },
	{ id: 'cf-juniper', title: 'Juniper Road', artist: 'East Duo', streamUrl: '/__acceptance_audio__/juniper.wav' },
	{ id: 'cf-copper', title: 'Copper Field', artist: 'West Trio', streamUrl: '/__acceptance_audio__/copper.wav' }
];
const ITEMS = TRACKS.map(({ id, title, artist }) => ({ id, title, artist }));
const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';
const AUTH_TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const FAVORITES_KEY_BASE = 'nntune.library.favorites.v1';

const state = (currentIndex = 0, tracks = TRACKS): QueueState => ({ tracks, currentIndex });
const player = (page: Page) => page.getByRole('region', { name: 'Player', exact: true });
const currentStar = (page: Page) => player(page).getByRole('button', { name: /star/i });
const row = (page: Page, id: string) => page.locator(`[data-testid="library-item"][data-id="${id}"]`);
const rowStar = (page: Page, id: string) => row(page, id).getByRole('button', { name: /star/i });
const queueStar = (page: Page, id: string) =>
	page.locator(`[data-testid="queue-item"][data-track-id="${id}"]`).getByRole('button', { name: /star/i });
const shelfEntry = (page: Page, id: string) =>
	page.locator(`[data-testid="favorite-item"][data-id="${id}"]`);
const shelfUnstar = (page: Page, id: string) =>
	shelfEntry(page, id).getByRole('button', { name: /star/i });

async function localOnly(page: Page, origin: string) {
	const expectedOrigin = new URL(origin).origin;
	expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(origin).hostname);
	await page.route('**/*', (route) => {
		const request = route.request();
		const target = new URL(request.url());
		if (target.origin !== expectedOrigin || request.resourceType() === 'media') return route.abort();
		if (target.pathname === '/api/library' && request.method() === 'GET') {
			return route.fulfill({ status: 200, json: ITEMS });
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
	await page.evaluate((value) => window.libraryCurrentFavorite.replace(value), next);
	await settle(page);
}

/** Non-throwing count wait for reload/transition sync; never fails the case itself. */
async function waitForCount(target: Locator, page: Page, want: number, tries = 60): Promise<void> {
	for (let i = 0; i < tries; i++) {
		if ((await target.count()) === want) return;
		await page.waitForTimeout(100);
	}
}

async function labelOf(toggle: Locator): Promise<string | null> {
	if ((await toggle.count()) !== 1) return null;
	return toggle.evaluate((element) =>
		element.getAttribute('aria-label') ?? (element.textContent ?? '').replace(/\s+/g, ' ').trim());
}

async function pressedOf(toggle: Locator): Promise<string | null> {
	if ((await toggle.count()) !== 1) return null;
	return toggle.getAttribute('aria-pressed');
}

/** Presence gate: clicks only run against a visible, enabled toggle. */
async function operable(toggle: Locator): Promise<boolean> {
	return (await toggle.count()) === 1 && (await toggle.isVisible()) && (await toggle.isEnabled());
}

async function isFocused(toggle: Locator): Promise<boolean> {
	if ((await toggle.count()) !== 1) return false;
	return toggle.evaluate((element) => document.activeElement === element);
}

async function queueIds(page: Page): Promise<(string | null)[]> {
	return page.getByTestId('queue-item').evaluateAll((items) =>
		items.map((item) => item.getAttribute('data-track-id')));
}

async function currentId(page: Page): Promise<string | null> {
	const current = page.locator('[data-testid="queue-item"][aria-current="true"]');
	if ((await current.count()) !== 1) return null;
	return current.getAttribute('data-track-id');
}

async function readAuthContext(page: Page): Promise<string | null> {
	return page.evaluate((key) => window.sessionStorage.getItem(key), AUTH_CONTEXT_KEY);
}

async function readFavoritesRaw(page: Page, context: string | null): Promise<string | null> {
	return page.evaluate(
		({ base, ctx }) => (ctx ? window.localStorage.getItem(`${base}.${ctx}`) : null),
		{ base: FAVORITES_KEY_BASE, ctx: context }
	);
}

function storedIds(raw: string | null): string[] {
	if (!raw) return [];
	return (JSON.parse(raw) as unknown[]).filter(
		(value): value is string => typeof value === 'string' && value.length > 0
	);
}

async function favoriteKeys(page: Page): Promise<string[]> {
	return page.evaluate((base) => {
		const keys: string[] = [];
		for (let i = 0; i < window.localStorage.length; i++) {
			const key = window.localStorage.key(i);
			if (key?.startsWith(base)) keys.push(key);
		}
		return keys.sort();
	}, FAVORITES_KEY_BASE);
}

async function audioState(page: Page) {
	const audio = page.locator('audio[data-testid="audio-player"]');
	if ((await audio.count()) !== 1) return null;
	return {
		src: await audio.getAttribute('src'),
		paused: await audio.evaluate((node: HTMLAudioElement) => node.paused),
		currentTime: await audio.evaluate((node: HTMLAudioElement) => node.currentTime)
	};
}

async function controlsState(page: Page) {
	const harbor = row(page, 'cf-harbor');
	return {
		find: await page.locator('#library-find').inputValue(),
		scope: await page.locator('#library-find-scope').inputValue(),
		order: await page.locator('#library-order').inputValue(),
		view: await page.locator('#library-view').inputValue(),
		visible: await page.getByTestId('library-item').count(),
		selection: await page.getByTestId('selection-count').textContent(),
		checked: (await harbor.locator('input[type="checkbox"]').count()) === 1
			&& await harbor.locator('input[type="checkbox"]').isChecked()
	};
}

async function liveTexts(page: Page) {
	const shelf = page.getByTestId('favorites-section').getByRole('status');
	const clean = (value: string | null) => (value ?? '').replace(/\s+/g, ' ').trim();
	return {
		favorites: clean(await page.getByTestId('favorites-announcement').textContent()),
		queue: clean(await page.getByTestId('queue-announcement').textContent()),
		nowPlaying: clean(await page.getByTestId('now-playing').textContent()),
		shelf: (await shelf.count()) === 1 ? clean(await shelf.textContent()) : ''
	};
}

/** Fail only the favorites write boundary, not auth-context/snapshot storage. */
async function failFavoritesStorage(page: Page): Promise<void> {
	await page.evaluate((base) => {
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (key, value) {
			if (this === window.localStorage && key.startsWith(`${base}.`)) {
				throw new DOMException('Favorites writes unavailable', 'QuotaExceededError');
			}
			setItem.call(this, key, value);
		};
	}, FAVORITES_KEY_BASE);
}

async function readableContrast(target: Locator): Promise<boolean> {
	if ((await target.count()) !== 1) return false;
	return target.evaluate((node) => {
		const rgb = (value: string) => value.match(/[\d.]+/g)?.map(Number) ?? [];
		const luminance = (color: number[]) => color.slice(0, 3).map((n) => {
			const c = n / 255; return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
		}).reduce((sum, value, i) => sum + value * [0.2126, 0.7152, 0.0722][i], 0);
		let background: number[] = [];
		for (let element: Element | null = node; element; element = element.parentElement) {
			const value = rgb(getComputedStyle(element).backgroundColor);
			if (value.length === 3 || value[3] === 1) { background = value; break; }
		}
		const foreground = rgb(getComputedStyle(node).color);
		const values = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
		return background.length >= 3 && (values[0] + 0.05) / (values[1] + 0.05) >= 4.5;
	});
}

async function check(testInfo: TestInfo, expected: unknown, actual: unknown) {
	let passed = false;
	try {
		expect(actual).toEqual(expected);
		passed = true;
	} finally {
		const receipt = {
			case_id: testInfo.title.split(' ')[0], expected, actual,
			status: passed ? 'GREEN' : 'RED',
			error_code: passed ? null : 'LIBCURRENTSTAR_CONTRACT',
			next_action: passed ? 'preserve acceptance' : 'implement current-track favorite; do not edit protected tests'
		};
		const body = JSON.stringify(receipt);
		console.log(body);
		await testInfo.attach('acceptance-case.json', { body, contentType: 'application/json' });
	}
}

test.afterEach(async ({}, testInfo) => {
	if (testInfo.attachments.some((attachment) => attachment.name === 'acceptance-case.json')) return;
	const body = JSON.stringify({
		case_id: testInfo.title.split(' ')[0], expected: 'reach behavioral assertion',
		actual: testInfo.errors.map((error) => error.message), status: 'ERROR',
		error_code: 'HARNESS_OR_ACTION_FAILURE', next_action: 'repair harness/action; not verified feature RED'
	});
	console.log(body);
	await testInfo.attach('acceptance-case.json', { body, contentType: 'application/json' });
});

test('LIBCURRENTSTAR-CF01 current-track star sits beside the native identity with a truthful name', async ({ libraryPage: page }, info) => {
	await replace(page);
	const toggle = currentStar(page);
	const gate = await operable(toggle);
	const before = { count: await toggle.count(), name: await labelOf(toggle), pressed: await pressedOf(toggle) };
	if (gate) { await toggle.click(); await settle(page); }
	const starred = { name: await labelOf(toggle), pressed: await pressedOf(toggle) };
	if (gate) { await toggle.click(); await settle(page); }
	await check(info, {
		before: { count: 1, name: 'Star Harbor Light', pressed: 'false' },
		starred: { name: 'Unstar Harbor Light', pressed: 'true' },
		after: { name: 'Star Harbor Light', pressed: 'false' },
		inPlayer: true, readout: true
	}, {
		before, starred,
		after: { name: await labelOf(toggle), pressed: await pressedOf(toggle) },
		inPlayer: (await player(page).getByRole('button', { name: /star/i }).count()) === 1,
		readout: (await player(page).getByTestId('library-now-playing').count()) === 1
	});
});

test('LIBCURRENTSTAR-CF02 starring from the player syncs row, shelf and queue through one scoped key', async ({ libraryPage: page }, info) => {
	await replace(page);
	const toggle = currentStar(page);
	if (await operable(toggle)) { await toggle.click(); await settle(page); }
	const context = await readAuthContext(page);
	await check(info, {
		player: { name: 'Unstar Harbor Light', pressed: 'true' },
		row: 'true', queue: 'true', shelf: 1, shelfName: 'Unstar Harbor Light',
		context: true, keys: context ? [`${FAVORITES_KEY_BASE}.${context}`] : [], stored: ['cf-harbor']
	}, {
		player: { name: await labelOf(toggle), pressed: await pressedOf(toggle) },
		row: await pressedOf(rowStar(page, 'cf-harbor')),
		queue: await pressedOf(queueStar(page, 'cf-harbor')),
		shelf: await shelfEntry(page, 'cf-harbor').count(),
		shelfName: await labelOf(shelfUnstar(page, 'cf-harbor')),
		context: context !== null,
		keys: await favoriteKeys(page),
		stored: storedIds(await readFavoritesRaw(page, context))
	});
});

test('LIBCURRENTSTAR-CF03 unstarring from the player removes the shelf entry with the queue untouched', async ({ libraryPage: page }, info) => {
	await replace(page);
	const harbor = currentStar(page);
	if (await operable(harbor)) { await harbor.click(); await settle(page); }
	const juniper = queueStar(page, 'cf-juniper');
	if (await operable(juniper)) { await juniper.click(); await settle(page); }
	const positionBefore = await page.getByTestId('queue-position').textContent();
	if (await operable(harbor)) { await harbor.click(); await settle(page); }
	const nowPlaying = (await page.getByTestId('now-playing').textContent()) ?? '';
	await check(info, {
		shelf: 1, juniperShelf: 1, harborShelf: 0,
		harborPlayer: { name: 'Star Harbor Light', pressed: 'false' },
		harborRow: 'false', harborQueue: 'false', juniperQueue: 'true',
		order: ['cf-harbor', 'cf-juniper', 'cf-copper'], nowPlaying: true, current: 'cf-harbor',
		position: true, stored: ['cf-juniper']
	}, {
		shelf: await page.getByTestId('favorite-item').count(),
		juniperShelf: await shelfEntry(page, 'cf-juniper').count(),
		harborShelf: await shelfEntry(page, 'cf-harbor').count(),
		harborPlayer: { name: await labelOf(harbor), pressed: await pressedOf(harbor) },
		harborRow: await pressedOf(rowStar(page, 'cf-harbor')),
		harborQueue: await pressedOf(queueStar(page, 'cf-harbor')),
		juniperQueue: await pressedOf(queueStar(page, 'cf-juniper')),
		order: await queueIds(page),
		nowPlaying: nowPlaying.includes('Harbor Light'),
		current: await currentId(page),
		position: (await page.getByTestId('queue-position').textContent()) === positionBefore,
		stored: storedIds(await readFavoritesRaw(page, await readAuthContext(page)))
	});
});

test('LIBCURRENTSTAR-CF04 later current-track selection follows favorite state in both directions', async ({ libraryPage: page }, info) => {
	await replace(page);
	const harbor = currentStar(page);
	if (await operable(harbor)) { await harbor.click(); await settle(page); }
	await page.getByTestId('player-next').click();
	await settle(page);
	const afterNext = {
		player: { name: await labelOf(currentStar(page)), pressed: await pressedOf(currentStar(page)) },
		harborRow: await pressedOf(rowStar(page, 'cf-harbor')),
		juniperRow: await pressedOf(rowStar(page, 'cf-juniper')),
		shelfHarbor: await shelfEntry(page, 'cf-harbor').count(),
		shelfJuniper: await shelfEntry(page, 'cf-juniper').count(),
		current: await currentId(page)
	};
	if (await operable(currentStar(page))) { await currentStar(page).click(); await settle(page); }
	await page.evaluate(() => window.libraryCurrentFavorite.select(0));
	await settle(page);
	const afterBack = {
		player: { name: await labelOf(currentStar(page)), pressed: await pressedOf(currentStar(page)) },
		juniperQueue: await pressedOf(queueStar(page, 'cf-juniper')),
		current: await currentId(page)
	};
	await check(info, {
		afterNext: {
			player: { name: 'Star Juniper Road', pressed: 'false' },
			harborRow: 'true', juniperRow: 'false', shelfHarbor: 1, shelfJuniper: 0, current: 'cf-juniper'
		},
		afterBack: {
			player: { name: 'Unstar Harbor Light', pressed: 'true' },
			juniperQueue: 'true', current: 'cf-harbor'
		}
	}, { afterNext, afterBack });
});

test('LIBCURRENTSTAR-CF05 favorites-only view keeps row recovery and queue focus for existing callers', async ({ libraryPage: page }, info) => {
	await replace(page);
	const view = page.locator('#library-view');
	if (await operable(rowStar(page, 'cf-harbor'))) { await rowStar(page, 'cf-harbor').click(); await settle(page); }
	await view.selectOption('favorites');
	await settle(page);
	if (await operable(rowStar(page, 'cf-harbor'))) { await rowStar(page, 'cf-harbor').click(); await settle(page); }
	const rowRecovery = await page.evaluate(() => document.activeElement?.id ?? null);
	const rowRemoved = await row(page, 'cf-harbor').count();
	await view.selectOption('all');
	await settle(page);
	if (await operable(rowStar(page, 'cf-harbor'))) { await rowStar(page, 'cf-harbor').click(); await settle(page); }
	await view.selectOption('favorites');
	await settle(page);
	const queued = queueStar(page, 'cf-harbor');
	if (await operable(queued)) { await queued.click(); await settle(page); }
	await check(info, {
		rowRecovery: 'library-view', rowRemoved: 0,
		queueFocusKept: true, viewKept: 'favorites', queuePressed: 'false'
	}, {
		rowRecovery, rowRemoved,
		queueFocusKept: await isFocused(queued),
		viewKept: await view.inputValue(),
		queuePressed: await pressedOf(queued)
	});
});

test('LIBCURRENTSTAR-CF06 player star and unstar keep focus on the still-mounted toggle', async ({ libraryPage: page }, info) => {
	await replace(page);
	const toggle = currentStar(page);
	if (await operable(toggle)) { await toggle.click(); await settle(page); }
	const keptAll = await isFocused(toggle);
	await page.locator('#library-view').selectOption('favorites');
	await settle(page);
	if (await operable(toggle)) { await toggle.click(); await settle(page); }
	const keptFavorites = await isFocused(toggle);
	const activeId = await page.evaluate(() => document.activeElement?.id ?? null);
	await check(info, {
		keptAll: true, keptFavorites: true, activeNotView: true, pressed: 'false'
	}, {
		keptAll, keptFavorites,
		activeNotView: activeId !== 'library-view',
		pressed: await pressedOf(toggle)
	});
});

test('LIBCURRENTSTAR-CF07 starring touches no audio, transport, queue or library controls', async ({ libraryPage: page }, info) => {
	const requests: { method: string; url: string }[] = [];
	page.on('request', (request) => requests.push({ method: request.method(), url: request.url() }));
	await replace(page);
	await page.locator('#library-find').fill('Harbor');
	await page.locator('#library-find-scope').selectOption('title');
	await page.locator('#library-order').selectOption('artist');
	await row(page, 'cf-harbor').locator('input[type="checkbox"]').check();
	await settle(page);
	const audio = page.locator('audio[data-testid="audio-player"]');
	const handle = await audio.elementHandle();
	if (handle) await handle.evaluate((node) => { (node as HTMLAudioElement & { __cf?: number }).__cf = 1; });
	const before = await audioState(page);
	const controlsBefore = await controlsState(page);
	const queueBefore = await queueIds(page);
	const currentBefore = await currentId(page);
	const toggle = currentStar(page);
	if (await operable(toggle)) { await toggle.click(); await settle(page); }
	const midShelf = await page.getByTestId('favorite-item').count();
	if (await operable(toggle)) { await toggle.click(); await settle(page); }
	const expando = handle ? await handle.evaluate((node) => (node as HTMLAudioElement & { __cf?: number }).__cf ?? null) : null;
	const connected = handle ? await handle.evaluate((node) => node.isConnected) : false;
	await check(info, {
		midShelf: 1, shelf: 0, audio: before, connected: true, expando: 1,
		controls: controlsBefore, queue: queueBefore, current: currentBefore, mutations: []
	}, {
		midShelf, shelf: await page.getByTestId('favorite-item').count(),
		audio: await audioState(page), connected, expando,
		controls: await controlsState(page), queue: await queueIds(page),
		current: await currentId(page),
		mutations: requests.filter((request) => request.method !== 'GET')
	});
});

test('LIBCURRENTSTAR-CF08 missing current track leaves no actionable stale toggle', async ({ libraryPage: page }, info) => {
	if (await operable(rowStar(page, 'cf-harbor'))) { await rowStar(page, 'cf-harbor').click(); await settle(page); }
	const hidden = async () => ({
		sections: await player(page).count(),
		readouts: await page.getByTestId('library-now-playing').count(),
		audio: await page.locator('audio').count(),
		star: await currentStar(page).count()
	});
	const gone = { sections: 0, readouts: 0, audio: 0, star: 0 };
	await replace(page, state(-1, []));
	const empty = await hidden();
	await replace(page, state(0, [{ ...TRACKS[0], streamUrl: '' }]));
	const blank = await hidden();
	await replace(page);
	await page.getByRole('button', { name: 'Clear queue', exact: true }).click();
	await settle(page);
	await check(info, {
		empty: gone, blank: gone, cleared: gone, rowKept: 'true'
	}, {
		empty, blank, cleared: await hidden(),
		rowKept: await pressedOf(rowStar(page, 'cf-harbor'))
	});
});

test('LIBCURRENTSTAR-CF09 auth teardown hides the current-track toggle and purges its favorite', async ({ libraryPage: page, libraryOrigin }, info) => {
	await replace(page);
	const toggle = currentStar(page);
	if (await operable(toggle)) { await toggle.click(); await settle(page); }
	const starredBefore = (await pressedOf(toggle)) === 'true';
	const contextBefore = await readAuthContext(page);
	const otherTab = await page.context().newPage();
	await otherTab.goto(`${libraryOrigin}/transition.html`);
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-current-favorite-auth-transition');
		window.localStorage.removeItem(key);
	}, AUTH_TRANSITION_KEY);
	await waitForCount(page.getByTestId('queue-item'), page, 0);
	await otherTab.close();
	await check(info, {
		starredBefore: true, player: 0, star: 0, queue: 0, outgoingKey: null
	}, {
		starredBefore,
		player: await player(page).count(),
		star: await currentStar(page).count(),
		queue: await page.getByTestId('queue-item').count(),
		outgoingKey: await readFavoritesRaw(page, contextBefore)
	});
});

test('LIBCURRENTSTAR-CF10 denied storage keeps a memory-only star without claiming a durable save', async ({ libraryPage: page }, info) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	// The harness deliberately aborts fixture media loads; those resource
	// errors are fixture noise, not product errors. Everything else counts.
	page.on('console', (message) => {
		if (message.type() === 'error' && !/Failed to load resource: net::ERR_(FAILED|ABORTED)/.test(message.text())) {
			pageErrors.push(message.text());
		}
	});
	await replace(page);
	await failFavoritesStorage(page);
	const isolated = await page.evaluate(() => {
		try {
			window.localStorage.setItem('test.unrelated-storage', 'available');
			return window.localStorage.getItem('test.unrelated-storage') === 'available';
		} catch { return false; }
	});
	const toggle = currentStar(page);
	if (await operable(toggle)) { await toggle.click(); await settle(page); }
	const ui = {
		pressed: await pressedOf(toggle),
		row: await pressedOf(rowStar(page, 'cf-harbor')),
		queue: await pressedOf(queueStar(page, 'cf-harbor')),
		shelf: await shelfEntry(page, 'cf-harbor').count()
	};
	const regions = await liveTexts(page);
	const outcome = `${regions.favorites} ${regions.queue}`.trim();
	const context = await readAuthContext(page);
	const rawBefore = await readFavoritesRaw(page, context);
	await page.reload();
	await waitForCount(page.getByTestId('library-item'), page, TRACKS.length);
	await check(info, {
		context: true, isolated: true,
		ui: { pressed: 'true', row: 'true', queue: 'true', shelf: 1 },
		honest: true, rawBefore: null, errors: [],
		afterReload: { row: 'false', shelf: 0, player: 0 }
	}, {
		context: context !== null, isolated, ui,
		honest: !/saved|persisted|stored|synced|backed up/i.test(outcome),
		rawBefore, errors: pageErrors,
		afterReload: {
			row: await pressedOf(rowStar(page, 'cf-harbor')),
			shelf: await page.getByTestId('favorite-item').count(),
			player: await player(page).count()
		}
	});
});

test('LIBCURRENTSTAR-CF11 Tab reaches the player toggle with Enter, Space and a visible ring', async ({ libraryPage: page }, info) => {
	await replace(page);
	const toggle = currentStar(page);
	let reached = false;
	if ((await toggle.count()) === 1) {
		await page.locator('audio[data-testid="audio-player"]').focus();
		for (let i = 0; i < 80 && !reached; i++) {
			reached = await isFocused(toggle);
			if (!reached) await page.keyboard.press('Tab');
		}
		reached = await isFocused(toggle);
	}
	const ring = reached ? await toggle.evaluate((element) => {
		const style = getComputedStyle(element);
		return (style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) > 0) || style.boxShadow !== 'none';
	}) : false;
	if (reached) { await page.keyboard.press('Enter'); await settle(page); }
	const afterEnter = { pressed: await pressedOf(toggle), focused: await isFocused(toggle) };
	if (reached) { await page.keyboard.press('Space'); await settle(page); }
	await check(info, {
		reached: true, ring: true,
		afterEnter: { pressed: 'true', focused: true },
		afterSpace: { pressed: 'false', focused: true }
	}, {
		reached, ring, afterEnter,
		afterSpace: { pressed: await pressedOf(toggle), focused: await isFocused(toggle) }
	});
});

test.describe('narrow touch', () => {
	test.use({ viewport: { width: 320, height: 800 }, hasTouch: true });

	for (const [index, theme] of ['light', 'dark'].entries()) {
		test(`LIBCURRENTSTAR-CF${12 + index} ${theme} 320px player toggle stays tappable, contained and readable`, async ({ libraryPage: page }, info) => {
			await page.emulateMedia({ colorScheme: theme as 'light' | 'dark' });
			await page.evaluate((value) => document.documentElement.setAttribute('data-appearance', value), theme);
			await replace(page);
			const toggle = currentStar(page);
			const present = await operable(toggle);
			if (present) { await toggle.tap(); await settle(page); }
			const tapped = await pressedOf(toggle);
			const box = present ? await toggle.boundingBox() : null;
			const regionBox = (await player(page).count()) === 1 ? await player(page).boundingBox() : null;
			if (present) { await toggle.tap(); await settle(page); }
			const within = (candidate: { x: number; width: number } | null) =>
				candidate !== null && candidate.x >= 0 && candidate.x + candidate.width <= 321;
			await check(info, {
				present: true, tapped: 'true', untapped: 'false',
				target: true, contained: true, readable: true
			}, {
				present, tapped, untapped: await pressedOf(toggle),
				target: box !== null && box.width >= 24 && box.height >= 24,
				contained: within(box) && within(regionBox),
				readable: await readableContrast(toggle)
			});
		});
	}
});

test('LIBCURRENTSTAR-CF14 each star action announces one concise polite outcome, replaced not appended', async ({ libraryPage: page }, info) => {
	await replace(page);
	const before = await liveTexts(page);
	const toggle = currentStar(page);
	if (await operable(toggle)) { await toggle.click(); await settle(page); }
	const afterStar = await liveTexts(page);
	const starPaths = (Object.keys(afterStar) as (keyof typeof afterStar)[])
		.filter((key) => afterStar[key] !== before[key]);
	const starText = starPaths.length === 1 ? afterStar[starPaths[0]] : '';
	const queued = queueStar(page, 'cf-juniper');
	if (await operable(queued)) { await queued.click(); await settle(page); }
	const afterQueue = await liveTexts(page);
	const queuePaths = (Object.keys(afterQueue) as (keyof typeof afterQueue)[])
		.filter((key) => afterQueue[key] !== afterStar[key]);
	const queueText = queuePaths.length === 1 ? afterQueue[queuePaths[0]] : '';
	const alerts = await page.locator('[role="alert"], [aria-live="assertive"]').evaluateAll((nodes) =>
		nodes.filter((node) => (node.textContent ?? '').trim() !== '').length);
	const polite = async (key: string | undefined) => {
		if (key === 'queue' || key === 'nowPlaying') return true;
		if (key === 'favorites') {
			return (await page.getByTestId('favorites-announcement').getAttribute('aria-live')) === 'polite';
		}
		return false;
	};
	await check(info, {
		star: { paths: 1, names: true, concise: true, polite: true },
		queue: { paths: 1, names: true, concise: true, replaced: true },
		alerts: 0
	}, {
		star: {
			paths: starPaths.length,
			names: starText.includes('Harbor Light'),
			concise: starText.length > 0 && starText.length <= 100,
			polite: await polite(starPaths[0])
		},
		queue: {
			paths: queuePaths.length,
			names: queueText.includes('Juniper Road'),
			concise: queueText.length > 0 && queueText.length <= 100,
			replaced: queueText.includes('Juniper Road') && !queueText.includes('Harbor Light')
		},
		alerts
	});
});

test('LIBCURRENTSTAR-CF15 a player-starred favorite persists through reload in the same scoped key', async ({ libraryPage: page }, info) => {
	await replace(page);
	const toggle = currentStar(page);
	if (await operable(toggle)) { await toggle.click(); await settle(page); }
	const context = await readAuthContext(page);
	await page.reload();
	await waitForCount(page.getByTestId('library-item'), page, TRACKS.length);
	const contextAfter = await readAuthContext(page);
	await check(info, {
		context: true,
		keys: context ? [`${FAVORITES_KEY_BASE}.${context}`] : [],
		row: 'true', shelf: 1, queue: 0, player: 0, stored: ['cf-harbor']
	}, {
		context: contextAfter !== null,
		keys: await favoriteKeys(page),
		row: await pressedOf(rowStar(page, 'cf-harbor')),
		shelf: await page.getByTestId('favorite-item').count(),
		queue: await page.getByTestId('queue-item').count(),
		player: await player(page).count(),
		stored: storedIds(await readFavoritesRaw(page, contextAfter))
	});
});
