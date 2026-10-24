import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance-owned direction extension. Builder must not edit this file or the
 * protected ordering/selectable/queue/favorites tests. Offline mocked browser,
 * real page and queue. No implementation/source-shape pins or live providers.
 * Breaks caught: absent/inaccessible directions, reversed ties/unnamed artists,
 * locale drift, persisted order, sort-triggered I/O, cache/favorite/queue changes,
 * selection by view index, focus loss, and discarded direction on refresh.
 * Add displayed is absent at this baseline; no bulk-action feature is invented.
 */
const ITEMS = [
	{ id: 'dir-zulu', title: 'Zulu Song', artist: ' beta ' },
	{ id: 'dir-tie-z', title: 'alpha', artist: 'Zulu' },
	{ id: 'id-fallback' },
	{ id: 'dir-accent-z', title: 'Éclair', artist: 'Écho' },
	{ id: 'dir-echo', title: 'Omega', artist: 'echo' },
	{ id: 'dir-tie-a', title: 'ALPHA', artist: 'Echo' },
	{ id: 'dir-blank', title: 'Beta Song', artist: '   ' },
	{ id: 'dir-accent-a', title: 'éclair', artist: 'écho' },
	{ id: 'dir-astral', title: '🎵 Music', artist: 'Alpha' }
];
const ORIGINAL = [
	'dir-zulu', 'dir-tie-z', 'id-fallback', 'dir-accent-z', 'dir-echo',
	'dir-tie-a', 'dir-blank', 'dir-accent-a', 'dir-astral'
];
const TITLE_ASC = [
	'dir-tie-z', 'dir-tie-a', 'dir-blank', 'id-fallback', 'dir-echo',
	'dir-zulu', 'dir-accent-z', 'dir-accent-a', 'dir-astral'
];
const TITLE_DESC = [
	'dir-astral', 'dir-accent-z', 'dir-accent-a', 'dir-zulu', 'dir-echo',
	'id-fallback', 'dir-blank', 'dir-tie-z', 'dir-tie-a'
];
const ARTIST_ASC = [
	'dir-astral', 'dir-zulu', 'dir-echo', 'dir-tie-a', 'dir-tie-z',
	'dir-accent-z', 'dir-accent-a', 'id-fallback', 'dir-blank'
];
const ARTIST_DESC = [
	'dir-accent-z', 'dir-accent-a', 'dir-tie-z', 'dir-echo', 'dir-tie-a',
	'dir-zulu', 'dir-astral', 'id-fallback', 'dir-blank'
];
const MODES = [
	{ label: 'Original order', ids: ORIGINAL },
	{ label: 'Title A–Z', ids: TITLE_ASC },
	{ label: 'Title Z–A', ids: TITLE_DESC },
	{ label: 'Artist A–Z', ids: ARTIST_ASC },
	{ label: 'Artist Z–A', ids: ARTIST_DESC }
] as const;

function ordering(page: Page): Locator {
	return page.getByRole('combobox', { name: /order|sort/i });
}
function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}
async function expectView(page: Page, ids: readonly string[]) {
	await expect.poll(() => page.getByTestId('library-item').evaluateAll(
		(rows) => rows.map((item) => item.getAttribute('data-id'))
	)).toEqual(ids);
}
async function choose(page: Page, label: string) {
	// Fail on a meaningful missing-choice assertion, not selectOption's timeout.
	await expect(ordering(page).getByRole('option', { name: label, exact: true })).toHaveCount(1);
	await ordering(page).selectOption({ label });
	await expect(ordering(page).locator('option:checked')).toHaveText(label);
}
async function expectQueue(page: Page, ids: string[], current: string, title: string) {
	await expect.poll(() => page.getByTestId('queue-item').evaluateAll(
		(rows) => rows.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
	)).toEqual(ids);
	await expect.poll(() => page.getByTestId('queue-item').evaluateAll(
		(rows) => rows.filter((item) => item.getAttribute('aria-current') === 'true')
			.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
	)).toEqual([current]);
	await expect(page.getByTestId('now-playing')).toContainText(title);
}
async function queued(page: Page, id: string) {
	return row(page, id).locator('button.nt-library-row').evaluate((element) =>
		element.getAttribute('aria-pressed') === 'true' ||
		element.getAttribute('aria-selected') === 'true' ||
		/\bqueued\b|\bin queue\b/i.test(`${element.getAttribute('aria-label') ?? ''} ${element.textContent}`)
	);
}
async function readCache(page: Page) {
	return page.evaluate(() => {
		const context = sessionStorage.getItem('nntune.library.auth-context.v1');
		return context ? sessionStorage.getItem(`nntune.library.snapshot.v1.${context}`) : null;
	});
}
async function storedFavorites(page: Page) {
	return page.evaluate(() => {
		const context = sessionStorage.getItem('nntune.library.auth-context.v1');
		return context ? localStorage.getItem(`nntune.library.favorites.v1.${context}`) : null;
	});
}

type Reply = { items: unknown; status?: number };
async function openLibrary(page: Page, reply: Reply = { items: ITEMS }) {
	const requests: string[] = [];
	const writes: string[] = [];
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (url.pathname.startsWith('/api/')) requests.push(`${request.method()} ${url.pathname}`);
		if (!['GET', 'HEAD'].includes(request.method())) writes.push(`${request.method()} ${url.pathname}`);
	});
	await page.route('**/api/library', (route) => route.fulfill({
		status: reply.status ?? 200, contentType: 'application/json', body: JSON.stringify(reply.items)
	}));
	await page.goto('/library');
	await expectView(page, ORIGINAL);
	await expect(ordering(page).locator('option:checked')).toHaveText('Original order');
	return { requests, writes };
}

test.beforeEach(async ({ context }) => {
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) {
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
});

test('DORD-00 control: real page, queue, cache, favorites and existing ascending choices work', async ({ page }) => {
	const api = await openLibrary(page);
	await row(page, 'dir-zulu').click();
	await row(page, 'dir-astral').click();
	await page.getByTestId('player-next').click();
	await expectQueue(page, ['dir-zulu', 'dir-astral'], 'dir-astral', '🎵 Music');
	await expect.poll(() => queued(page, 'dir-zulu')).toBe(true);
	await row(page, 'dir-echo').getByRole('button', { name: /star|favorite/i }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect.poll(() => storedFavorites(page)).not.toBeNull();
	await expect.poll(() => readCache(page)).not.toBeNull();
	await choose(page, 'Title A–Z');
	await expectView(page, TITLE_ASC);
	await choose(page, 'Artist A–Z');
	await expectView(page, ARTIST_ASC);
	await choose(page, 'Original order');
	await expectView(page, ORIGINAL);
	expect(api.requests).toEqual(['GET /api/library']);
	expect(api.writes).toEqual([]);
});

test('DORD-01 narrow keyboard: all five choices, focus retention, Original and reload reset', async ({ page }) => {
	await page.setViewportSize({ width: 360, height: 800 });
	const api = await openLibrary(page);
	for (let tabs = 0; tabs < 30 && !(await ordering(page).evaluate(
		(element) => element === document.activeElement
	)); tabs += 1) await page.keyboard.press('Tab');
	await expect(ordering(page)).toBeFocused();

	for (const { label, ids } of MODES) {
		await expect(ordering(page).getByRole('option', { name: label, exact: true })).toHaveCount(1);
		// Do not prescribe option order; navigate the actual native selector.
		const labels = await ordering(page).locator('option').allTextContents();
		await page.keyboard.press('Home');
		for (let step = 0; step < labels.indexOf(label); step += 1) await page.keyboard.press('ArrowDown');
		await page.keyboard.press('Enter');
		await expect(ordering(page).locator('option:checked')).toHaveText(label);
		await expectView(page, ids);
		await expect(ordering(page)).toBeFocused();
		expect(await ordering(page).evaluate((element) => {
			const rect = element.getBoundingClientRect();
			return rect.left >= 0 && rect.right <= window.innerWidth;
		}), 'ordering control must fit the narrow viewport').toBe(true);
	}
	await choose(page, 'Original order');
	await expectView(page, ORIGINAL);
	expect(api.requests).toEqual(['GET /api/library']);
	await choose(page, 'Artist Z–A');
	await page.reload();
	await expect(ordering(page).locator('option:checked')).toHaveText('Original order');
	await expectView(page, ORIGINAL);
	expect(api.writes).toEqual([]);
});

test('DORD-02 title descending: fallback, case/non-ASCII ties and immutable cached API order', async ({ page }) => {
	const api = await openLibrary(page);
	await expect.poll(() => readCache(page)).not.toBeNull();
	const cache = await readCache(page);
	expect(JSON.parse(cache as string).items.map((item: { id: string }) => item.id)).toEqual(ORIGINAL);
	await choose(page, 'Title Z–A');
	await expectView(page, TITLE_DESC);
	for (const [id, text] of [
		['id-fallback', 'id-fallback'], ['dir-tie-z', 'alpha'], ['dir-tie-a', 'ALPHA'],
		['dir-accent-z', 'Éclair'], ['dir-accent-a', 'éclair'], ['dir-astral', '🎵 Music']
	]) await expect(row(page, id)).toContainText(text);
	await choose(page, 'Title A–Z');
	await expectView(page, TITLE_ASC);
	await choose(page, 'Title Z–A');
	await expectView(page, TITLE_DESC);
	await choose(page, 'Original order');
	await expectView(page, ORIGINAL);
	expect(await readCache(page)).toBe(cache);
	expect(api.requests).toEqual(['GET /api/library']);
	expect(api.writes).toEqual([]);
});

test('DORD-03 artist descending: stable named ties and unknown artists last in both directions', async ({ page }) => {
	const api = await openLibrary(page);
	await choose(page, 'Artist Z–A');
	await expectView(page, ARTIST_DESC);
	await expect(row(page, 'dir-echo')).toContainText('echo');
	await expect(row(page, 'dir-tie-a')).toContainText('Echo');
	await expect(row(page, 'dir-accent-z')).toContainText('Écho');
	await choose(page, 'Artist A–Z');
	await expectView(page, ARTIST_ASC);
	await choose(page, 'Artist Z–A');
	await expectView(page, ARTIST_DESC);
	expect(api.requests).toEqual(['GET /api/library']);
	expect(api.writes).toEqual([]);
});

test('DORD-04 direction preserves favorites, queue current/membership, badges and gesture-ID selection', async ({ page }) => {
	const api = await openLibrary(page);
	await row(page, 'dir-echo').getByRole('button', { name: /star|favorite/i }).click();
	await row(page, 'dir-zulu').getByRole('button', { name: /star|favorite/i }).click();
	const favorites = await storedFavorites(page);
	expect(favorites).not.toBeNull();
	const favoriteIds = await page.getByTestId('favorite-item').evaluateAll(
		(rows) => rows.map((item) => item.getAttribute('data-id'))
	);
	expect(favoriteIds).toHaveLength(2);
	await row(page, 'dir-zulu').click();
	await row(page, 'dir-astral').click();
	await page.getByTestId('player-next').click();
	for (const { label, ids } of MODES) {
		await choose(page, label);
		await expectView(page, ids);
		await expectQueue(page, ['dir-zulu', 'dir-astral'], 'dir-astral', '🎵 Music');
		for (const item of ITEMS) expect(await queued(page, item.id), item.id)
			.toBe(['dir-zulu', 'dir-astral'].includes(item.id));
		expect(await storedFavorites(page)).toBe(favorites);
		expect(await page.getByTestId('favorite-item').evaluateAll(
			(rows) => rows.map((item) => item.getAttribute('data-id'))
		)).toEqual(favoriteIds);
	}
	await choose(page, 'Artist Z–A');
	await row(page, 'dir-tie-a').click();
	await choose(page, 'Title Z–A');
	const control = row(page, 'dir-blank').locator('button.nt-library-row');
	await control.focus();
	await page.keyboard.press('Enter');
	await row(page, 'dir-tie-a').click();
	await expectQueue(page, ['dir-zulu', 'dir-astral', 'dir-tie-a', 'dir-blank'], 'dir-astral', '🎵 Music');
	await expect.poll(() => queued(page, 'dir-blank')).toBe(true);
	expect(api.requests).toEqual(['GET /api/library']);
	expect(api.writes).toEqual([]);
});

test('DORD-05 recomputation: refresh retains descending; failure keeps rows, mode and queue', async ({ page }) => {
	const reply: Reply = { items: ITEMS };
	const api = await openLibrary(page, reply);
	await row(page, 'dir-zulu').click();
	await choose(page, 'Artist Z–A');
	reply.items = [
		{ id: 'fresh-unknown', title: 'Unknown' },
		{ id: 'fresh-alpha', title: 'Alpha', artist: 'Alpha' },
		{ id: 'fresh-zulu', title: 'Zulu', artist: 'Zulu' },
		{ id: 'fresh-tie', title: 'Bravo', artist: 'zulu' }
	];
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expectView(page, ['fresh-zulu', 'fresh-tie', 'fresh-alpha', 'fresh-unknown']);
	await expect(ordering(page).locator('option:checked')).toHaveText('Artist Z–A');
	await choose(page, 'Title Z–A');
	await expectView(page, ['fresh-zulu', 'fresh-unknown', 'fresh-tie', 'fresh-alpha']);
	reply.status = 503;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expectView(page, ['fresh-zulu', 'fresh-unknown', 'fresh-tie', 'fresh-alpha']);
	await expect(ordering(page).locator('option:checked')).toHaveText('Title Z–A');
	await expectQueue(page, ['dir-zulu'], 'dir-zulu', 'Zulu Song');
	await choose(page, 'Original order');
	await expectView(page, ['fresh-unknown', 'fresh-alpha', 'fresh-zulu', 'fresh-tie']);
	expect(api.requests).toEqual(['GET /api/library', 'GET /api/library', 'GET /api/library']);
	expect(api.writes).toEqual([]);
});

test('DORD-06 empty/singleton recomputation keeps the chosen direction for the next snapshot', async ({ page }) => {
	const reply: Reply = { items: ITEMS };
	await openLibrary(page, reply);
	await choose(page, 'Title Z–A');
	reply.items = [];
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expectView(page, []);
	await expect(page.getByRole('status')).toContainText(/library is empty/i);
	await expect(page.getByRole('link', { name: 'Import an NZB' })).toHaveAttribute('href', '/library/import');
	reply.items = [{ id: 'single' }];
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expectView(page, ['single']);
	await expect(ordering(page).locator('option:checked')).toHaveText('Title Z–A');
	reply.items = ITEMS;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expectView(page, TITLE_DESC);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('DORD-07 auth transition after descending clears previous-account rows and queue', async ({ page }) => {
	const reply: Reply = { items: ITEMS };
	await openLibrary(page, reply);
	await choose(page, 'Artist Z–A');
	await row(page, 'dir-tie-a').click();
	await expectQueue(page, ['dir-tie-a'], 'dir-tie-a', 'ALPHA');
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'direction-acceptance');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await otherTab.close();
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expectView(page, []);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	reply.items = [{ id: 'other-account', title: 'Other Song', artist: 'Other' }];
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, ['other-account']);
	await row(page, 'other-account').click();
	await expectQueue(page, ['other-account'], 'other-account', 'Other Song');
});
