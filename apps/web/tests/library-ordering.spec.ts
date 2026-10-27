import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance-owned: the builder makes this file pass without editing it.
 * Offline browser contract, not implementation/source-shape pins. Existing
 * library-selectable and library-player-queue acceptance files stay protected.
 *
 * Deterministic comparison contract (no host-locale dependency): compare the
 * displayed title (existing id fallback) or trimmed artist after Unicode
 * toLowerCase(), using ascending ECMAScript string/code-unit order. Do not
 * strip accents, normalize Unicode, or rewrite displayed metadata. Equal
 * keys retain API order, with NO artist/title/id secondary key. Missing or
 * whitespace-only artists follow all named artists, retaining API order.
 * Original order always means the untouched API snapshot, never the last sort.
 *
 * Use a labeled native combobox (accessible name includes Order or Sort),
 * with Original order / Title A–Z / Artist A–Z options. No new test-id needed.
 * Unit setup is SSR/source-pin only; real interactive behavior belongs in this
 * route-mocked browser harness, alongside the existing library acceptance.
 * Breaks caught: in-place sort, locale/secondary-key drift, selection by view
 * index, queue reordering/current reset, persisted mode, sort-triggered fetch,
 * lost mode on refresh, retained-content/auth-isolation regressions.
 */

const ITEMS = [
	{ id: 'order-zulu', title: 'Zulu Song', artist: ' beta ' },
	{ id: 'order-tie-z', title: 'alpha', artist: 'Zulu' },
	{ id: 'id-fallback' },
	{ id: 'order-accent-z', title: 'Éclair', artist: 'Écho' },
	{ id: 'order-artist-z', title: 'Omega', artist: 'echo' },
	{ id: 'order-tie-a', title: 'ALPHA', artist: 'Echo' },
	{ id: 'order-blank-artist', title: 'Beta Song', artist: '   ' },
	{ id: 'order-accent-a', title: 'éclair', artist: 'écho' },
	{ id: 'order-astral', title: '🎵 Music', artist: 'Alpha' }
];

// Literal expectations: title ties oppose artist/id order; artist ties oppose
// title order. Sorting by a secondary field instead of API position must fail.
const ORIGINAL = [
	'order-zulu', 'order-tie-z', 'id-fallback', 'order-accent-z', 'order-artist-z',
	'order-tie-a', 'order-blank-artist', 'order-accent-a', 'order-astral'
];
const TITLE = [
	'order-tie-z', 'order-tie-a', 'order-blank-artist', 'id-fallback', 'order-artist-z',
	'order-zulu', 'order-accent-z', 'order-accent-a', 'order-astral'
];
const ARTIST = [
	'order-astral', 'order-zulu', 'order-artist-z', 'order-tie-a', 'order-tie-z',
	'order-accent-z', 'order-accent-a', 'id-fallback', 'order-blank-artist'
];
const REFRESHED = [
	{ id: 'fresh-zulu', title: 'Zebra', artist: 'Alpha' },
	{ id: 'fresh-bravo', title: 'Bravo', artist: 'Zulu' },
	{ id: 'fresh-alpha', title: 'Alpha', artist: 'Bravo' }
];
const NEXT_ACCOUNT = [{ id: 'other-account', title: 'Other Account Song', artist: 'Other' }];
const ORDER_LABELS = ['Original order', 'Title A–Z', 'Artist A–Z'] as const;
type OrderLabel = (typeof ORDER_LABELS)[number];

type LibraryReply = { items: unknown; status?: number };

async function mockLibrary(page: Page, reply: LibraryReply = { items: ITEMS }) {
	let requests = 0;
	await page.route('**/api/library', (route) => {
		requests += 1;
		return route.fulfill({
			status: reply.status ?? 200,
			contentType: 'application/json',
			body: JSON.stringify(reply.items)
		});
	});
	return { requests: () => requests };
}

async function openLibrary(page: Page) {
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expectView(page, ORIGINAL);
}

function ordering(page: Page): Locator {
	return page.getByRole('combobox', { name: /order|sort/i });
}

async function chooseOrder(page: Page, label: OrderLabel) {
	await expect(ordering(page), 'library must expose a labeled ordering control').toBeVisible();
	await ordering(page).selectOption({ label });
	await expectMode(page, label);
}

async function expectMode(page: Page, label: OrderLabel) {
	await expect(ordering(page).locator('option:checked')).toHaveText(label);
}

async function expectView(page: Page, ids: string[]) {
	await expect.poll(() => page.getByTestId('library-item').evaluateAll(
		(rows) => rows.map((row) => row.getAttribute('data-id'))
	)).toEqual(ids);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

async function expectQueue(page: Page, ids: string[], current: string, title: string) {
	await expect.poll(() => page.getByTestId('queue-item').evaluateAll(
		(rows) => rows.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
	)).toEqual(ids);
	await expect(page.getByTestId('now-playing')).toContainText(title);
	await expect.poll(() => page.getByTestId('queue-item').evaluateAll(
		(rows) => rows.filter((item) => item.getAttribute('aria-current') === 'true')
			.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
	)).toEqual([current]);
}

async function readSnapshot(page: Page): Promise<string | null> {
	return page.evaluate(() => {
		const context = sessionStorage.getItem('nntune.library.auth-context.v1');
		return context ? sessionStorage.getItem(`nntune.library.snapshot.v1.${context}`) : null;
	});
}

test.beforeEach(async ({ context }) => {
	// No catalog, account, media or production network. The specific library
	// page.route mocks below take precedence over this context-level boundary.
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) {
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
});

async function authTransition(page: Page) {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'ordering-test');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await otherTab.close();
}

test('LORD-00 control: API order and gesture-order queue work before ordering', async ({ page }) => {
	const api = await mockLibrary(page);
	await openLibrary(page);
	await expect(row(page, 'id-fallback')).toContainText('id-fallback');
	await row(page, 'order-zulu').click();
	await row(page, 'order-astral').click();
	await expectQueue(page, ['order-zulu', 'order-astral'], 'order-zulu', 'Zulu Song');
	expect(api.requests()).toBe(1);
});

test('LORD-01 keyboard: default, both modes, Original restoration and reload reset', async ({ page }) => {
	const api = await mockLibrary(page);
	await openLibrary(page);
	await expect(ordering(page)).toBeVisible();
	await expectMode(page, 'Original order');
	for (const label of ORDER_LABELS) {
		await expect(ordering(page).getByRole('option', { name: label, exact: true })).toHaveCount(1);
	}

	// Reach the selector by Tab, not a synthetic focus-only interaction.
	for (let tabs = 0; tabs < 20 && !(await ordering(page).evaluate(
		(element) => element === document.activeElement
	)); tabs += 1) await page.keyboard.press('Tab');
	await expect(ordering(page)).toBeFocused();
	await page.keyboard.press('ArrowDown');
	await page.keyboard.press('Enter');
	await expectMode(page, 'Title A–Z');
	await expectView(page, TITLE);
	await page.keyboard.press('ArrowDown');
	await page.keyboard.press('Enter');
	await expectMode(page, 'Artist A–Z');
	await expectView(page, ARTIST);
	await chooseOrder(page, 'Original order');
	await expectView(page, ORIGINAL);
	expect(api.requests(), 'ordering must not request another snapshot').toBe(1);

	await chooseOrder(page, 'Artist A–Z');
	await page.reload();
	await expectMode(page, 'Original order');
	await expectView(page, ORIGINAL);
});

test('LORD-02 title: fallback, case ties and non-ASCII stay deterministic and intact', async ({ page }) => {
	const api = await mockLibrary(page);
	await openLibrary(page);
	await chooseOrder(page, 'Title A–Z');
	await expectView(page, TITLE);
	for (const [id, text] of [
		['order-tie-z', 'alpha'], ['order-tie-a', 'ALPHA'],
		['id-fallback', 'id-fallback'], ['order-accent-z', 'Éclair'],
		['order-accent-a', 'éclair'], ['order-astral', '🎵 Music']
	]) await expect(row(page, id)).toContainText(text);
	await chooseOrder(page, 'Artist A–Z');
	await chooseOrder(page, 'Title A–Z');
	await expectView(page, TITLE);
	await chooseOrder(page, 'Original order');
	await expectView(page, ORIGINAL);
	expect(api.requests()).toBe(1);
});

test('LORD-03 artist: named first, stable missing/blank and case/non-ASCII ties', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	await chooseOrder(page, 'Artist A–Z');
	await expectView(page, ARTIST);
	await expect(row(page, 'order-artist-z')).toContainText('echo');
	await expect(row(page, 'order-tie-a')).toContainText('Echo');
	await expect(row(page, 'order-accent-z')).toContainText('Écho');
	await expect(row(page, 'order-accent-a')).toContainText('écho');
	await chooseOrder(page, 'Title A–Z');
	await chooseOrder(page, 'Artist A–Z');
	await expectView(page, ARTIST);
});

test('LORD-04 queue: all modes preserve a non-first current item; selection uses gesture IDs', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	await row(page, 'order-zulu').click();
	await row(page, 'order-astral').click();
	await row(page, 'order-tie-a').click();
	await page.getByTestId('player-next').click();
	const queued = ['order-zulu', 'order-astral', 'order-tie-a'];
	await expectQueue(page, queued, 'order-astral', '🎵 Music');
	for (const mode of ['Title A–Z', 'Artist A–Z', 'Original order'] as const) {
		await chooseOrder(page, mode);
		await expectQueue(page, queued, 'order-astral', '🎵 Music');
	}
	await chooseOrder(page, 'Title A–Z');
	await row(page, 'order-accent-a').click();
	const control = row(page, 'order-blank-artist')
		.locator('button, [role="button"], [role="option"]').first();
	const keyboardRow = (await control.count()) ? control : row(page, 'order-blank-artist');
	await keyboardRow.focus();
	await expect(keyboardRow).toBeFocused();
	await page.keyboard.press('Enter');
	await expectQueue(page, [...queued, 'order-accent-a', 'order-blank-artist'],
		'order-astral', '🎵 Music');
	await row(page, 'order-tie-a').click();
	await expectQueue(page, [...queued, 'order-accent-a', 'order-blank-artist'],
		'order-astral', '🎵 Music');
});

test('LORD-05 refresh: a new snapshot keeps the selected mode without sort-triggered I/O', async ({ page }) => {
	const reply: LibraryReply = { items: ITEMS };
	const api = await mockLibrary(page, reply);
	await openLibrary(page);
	await chooseOrder(page, 'Artist A–Z');
	expect(api.requests()).toBe(1);
	reply.items = REFRESHED;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expectView(page, ['fresh-zulu', 'fresh-alpha', 'fresh-bravo']);
	await expectMode(page, 'Artist A–Z');
	expect(api.requests()).toBe(2);
	await chooseOrder(page, 'Original order');
	await expectView(page, ['fresh-zulu', 'fresh-bravo', 'fresh-alpha']);
	expect(api.requests()).toBe(2);
});

test('LORD-06 retention: failed refresh keeps the sorted snapshot and untouched cache', async ({ page }) => {
	const reply: LibraryReply = { items: ITEMS };
	const api = await mockLibrary(page, reply);
	await openLibrary(page);
	await expect.poll(() => readSnapshot(page)).not.toBeNull();
	const stored = await readSnapshot(page);
	expect(JSON.parse(stored as string).items.map((item: { id: string }) => item.id)).toEqual(ORIGINAL);
	await row(page, 'order-zulu').click();
	await chooseOrder(page, 'Artist A–Z');
	await expectView(page, ARTIST);
	expect(await readSnapshot(page), 'presentation ordering must not rewrite the cache').toBe(stored);
	reply.status = 503;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expectView(page, ARTIST);
	await expectMode(page, 'Artist A–Z');
	await expectQueue(page, ['order-zulu'], 'order-zulu', 'Zulu Song');
	expect(api.requests()).toBe(2);
	await chooseOrder(page, 'Title A–Z');
	await expectView(page, TITLE);
	await chooseOrder(page, 'Original order');
	await expectView(page, ORIGINAL);
	expect(await readSnapshot(page)).toBe(stored);
	expect(api.requests()).toBe(2);
});

test('LORD-07 states: loading/error/empty retain their semantics; retry keeps a valid mode', async ({ page }) => {
	let release!: () => void;
	const held = new Promise<void>((resolve) => { release = resolve; });
	await page.route('**/api/library', async (route) => {
		await held;
		await route.fulfill({ status: 503, contentType: 'application/json', body: '{}' });
	});
	await page.goto('/library');
	await expect(page.getByRole('status')).toContainText(/loading/i);
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	release();
	await expect(page.getByRole('alert')).toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	const reply: LibraryReply = { items: ITEMS };
	const api = await mockLibrary(page, reply);
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, ORIGINAL);
	await expectMode(page, 'Original order');
	await chooseOrder(page, 'Title A–Z');
	reply.items = [];
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByRole('status')).toContainText(/library is empty/i);
	await expect(page.getByRole('link', { name: 'Import an NZB' })).toHaveAttribute('href', '/library/import');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	reply.items = REFRESHED;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expectView(page, ['fresh-alpha', 'fresh-bravo', 'fresh-zulu']);
	await expectMode(page, 'Title A–Z');
	expect(api.requests()).toBe(3);
});

test('LORD-08 auth: sorting does not retain the previous account selection or queue', async ({ page }) => {
	const reply: LibraryReply = { items: ITEMS };
	await mockLibrary(page, reply);
	await openLibrary(page);
	await row(page, 'order-zulu').click();
	await chooseOrder(page, 'Title A–Z');
	await row(page, 'order-accent-a').click();
	await expectQueue(page, ['order-zulu', 'order-accent-a'], 'order-zulu', 'Zulu Song');
	await authTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	reply.items = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, ['other-account']);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await row(page, 'other-account').click();
	await expectQueue(page, ['other-account'], 'other-account', 'Other Account Song');
});
