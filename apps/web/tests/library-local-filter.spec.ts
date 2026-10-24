import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance-owned: the paired build makes this file pass without editing it.
 * Offline browser contract for finding already-loaded library tracks by title
 * or artist, not implementation/source-shape pins. The existing
 * library-ordering, library-player-queue and library-favorites-local specs stay
 * protected; the pure matching half is
 * `src/lib/library/local-filter-acceptance.test.ts`.
 *
 * Matching contract (mirrors the pure half): a trimmed literal
 * case-insensitive substring over the safe display title (title when
 * non-blank, else the stable id) and the optional display artist. Either field
 * matches (OR). Deterministic `toLowerCase()` folding, no user-generated
 * regular expression, no accent stripping. Empty query shows every
 * otherwise-visible row in canonical order with stable ids.
 *
 * Fixture discipline: visible title/artist text is DIGIT-FREE, so bare
 * word-boundary digits in <main> can only be the matching/loaded counts — and
 * count assertions never run beside a non-empty queue (queue-position text
 * carries digits). Fixture titles/artists also avoid the words "star" and
 * "favorite" so the star-toggle name filter matches only the toggle itself.
 * Queries are chosen so each expectation is exact: 'song' hits three rows,
 * 'a' hits every row, 'filter-alpha' (the id of a TITLED row) hits nothing,
 * 'eclair' (ASCII) misses 'Éclair'.
 *
 * Control vocabulary the build may choose freely: input type search or text;
 * count wording ("1 of 8", "Showing 1 of 8", ...); no-match wording ("No
 * matches", "No results", ...). Pinned instead: the field's accessible name
 * mentions find or search; clearing says what it clears ("Clear search",
 * never the queue's "Clear queue"); the no-match reset is actionable; the
 * loaded-tracks disclaimer is programmatically associated with the field
 * (label, aria-label, labelledby or describedby — a nearby order hint must not
 * satisfy it). There is deliberately NO Play All / Add displayed / batch
 * action: LF-00 and LF-01 assert its absence before and after the bundle.
 *
 * Case IDs (stable): LF-00 control, LF-01 bundle, LF-02 title, LF-03 artist +
 * id fallback, LF-04 literals + folding + reset, LF-05 presentation-only,
 * LF-06 enqueue exact id, LF-07 ordering/stars composition + reload, LF-08
 * keyboard + live-region, LF-09 narrow + themes, LF-10 stale + refresh, LF-11
 * auth transition, LF-12 loading/error/empty.
 *
 * Focused command (repo root, deterministic, offline):
 *   pnpm --dir apps/web exec playwright test tests/library-local-filter.spec.ts
 */

const ITEMS = [
	{ id: 'filter-alpha', title: 'Alpha Song', artist: 'Harbor Lights' },
	{ id: 'filter-beta', title: 'Beta Song', artist: 'harbor lights' },
	{ id: 'filter-gamma', title: 'Gamma (Live)', artist: 'Echo' },
	{ id: 'id-fallback-track' },
	{ id: 'filter-regex', title: 'What.*Ever (a|b) [x]+', artist: 'C++ Players' },
	{ id: 'filter-accent', title: 'Éclair', artist: 'écho' },
	{ id: 'filter-delta', title: 'Delta Song', artist: '   ' },
	{ id: 'filter-zulu', title: 'Zulu Last', artist: 'Zulu' }
];

const API_IDS = ITEMS.map((item) => item.id);

const REFRESHED = [
	{ id: 'fresh-keeper', title: 'Keeper Song', artist: 'Fresh Band' },
	{ id: 'fresh-other', title: 'Other Tune', artist: 'Other Band' },
	{ id: 'fresh-third', title: 'Third Wheel', artist: 'Third Band' }
];

const NEXT_ACCOUNT = [{ id: 'next-account', title: 'Next Account Tune', artist: 'Next Band' }];

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
	await expectView(page, API_IDS);
}

function ordering(page: Page): Locator {
	return page.getByRole('combobox', { name: /order|sort/i });
}

function findField(page: Page): Locator {
	// The build may use type="search" (role searchbox) or type="text" (role
	// textbox); either way the accessible name must mention find or search. A
	// "Clear search" button never matches these roles, so this stays strict.
	return page
		.getByRole('searchbox', { name: /find|search/i })
		.or(page.getByRole('textbox', { name: /find|search/i }));
}

async function requireFind(page: Page): Promise<Locator> {
	const field = findField(page);
	await expect(field, 'library must expose a labeled find-in-loaded-tracks field').toBeVisible();
	return field;
}

function clearSearch(page: Page): Locator {
	// "Clear queue" must not match: the control has to say what it clears.
	// first() because the build may render the affordance twice (toolbar plus
	// no-match state); either one must reset.
	return page.getByRole('button', { name: /clear search/i }).first();
}

function resetControl(page: Page): Locator {
	return page.getByRole('button', { name: /clear search|reset|show all/i }).first();
}

async function expectView(page: Page, ids: string[]) {
	await expect
		.poll(() =>
			page
				.getByTestId('library-item')
				.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')))
		)
		.toEqual(ids);
}

async function expectMatchCounts(page: Page, matching: number, loaded: number) {
	const region = page.locator('main');
	await expect(region, `expected a visible matching count of ${matching}`).toContainText(
		new RegExp(`\\b${matching}\\b`)
	);
	await expect(region, `expected a visible loaded count of ${loaded}`).toContainText(
		new RegExp(`\\b${loaded}\\b`)
	);
}

async function expectNoMatches(page: Page) {
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	const message = page.locator('main').getByText(/no .*match|no results|nothing found/i);
	await expect(message.first(), 'zero matches must show a distinct no-matches state').toBeVisible();
	await expect(resetControl(page), 'no-matches state must offer an actionable reset').toBeVisible();
}

async function expectCatalogDisclaimer(page: Page) {
	// Only sources programmatically tied to the field count: the order hint's
	// "Changes this list only" is a nearby span and must never satisfy this.
	const hint = await findField(page).evaluate((element) => {
		const input = element as HTMLInputElement;
		const ids =
			`${input.getAttribute('aria-describedby') ?? ''} ${input.getAttribute('aria-labelledby') ?? ''}`
				.split(/\s+/)
				.filter(Boolean);
		const referenced = ids
			.map((id) => document.getElementById(id)?.textContent ?? '')
			.join(' ');
		const own = input.getAttribute('aria-label') ?? '';
		const labels = Array.from(document.querySelectorAll('label'))
			.filter((label) => label.htmlFor === input.id || label.contains(input))
			.map((label) => label.textContent ?? '')
			.join(' ');
		return `${referenced} ${own} ${labels}`;
	});
	expect(
		hint,
		'find must say it searches loaded tracks, not the remote catalog (label, hint or describedby)'
	).toMatch(/loaded|already here|already loaded|this list|on this device|not.*catalog|offline/i);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function starButton(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /star|favorite/i });
}

function favoriteItem(page: Page, id: string): Locator {
	return page.locator(`[data-testid="favorite-item"][data-id="${id}"]`);
}

async function favoriteIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('favorite-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-id')));
}

async function expectQueue(page: Page, ids: string[], current: string, title: string) {
	await expect
		.poll(() =>
			page
				.getByTestId('queue-item')
				.evaluateAll((rows) =>
					rows.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
				)
		)
		.toEqual(ids);
	await expect(page.getByTestId('now-playing')).toContainText(title);
	await expect
		.poll(() =>
			page
				.getByTestId('queue-item')
				.evaluateAll((rows) =>
					rows
						.filter((item) => item.getAttribute('aria-current') === 'true')
						.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
				)
		)
		.toEqual([current]);
}

async function readLocalStorage(page: Page): Promise<[string, string][]> {
	return page.evaluate(() => Object.entries(window.localStorage).sort() as [string, string][]);
}

async function authTransition(page: Page) {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'local-filter-test');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await otherTab.close();
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

test('LF-00 control: API order and gesture-order queue work before find lands', async ({
	page
}) => {
	const api = await mockLibrary(page);
	await openLibrary(page);
	await expect(page.getByTestId('library-track-count')).toContainText('8 tracks');
	await row(page, 'filter-alpha').click();
	await row(page, 'filter-zulu').click();
	await expectQueue(page, ['filter-alpha', 'filter-zulu'], 'filter-alpha', 'Alpha Song');
	expect(api.requests()).toBe(1);
	// Finding must never grow a batch action.
	await expect(
		page.getByRole('button', { name: /play all|add all|enqueue all|add displayed|queue all/i })
	).toHaveCount(0);
});

test('LF-01 bundle: labeled find field, clear control, counts and loaded-tracks disclaimer', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await expect(field).toBeEditable();
	await expect(field).toHaveValue('');
	await expectMatchCounts(page, 8, 8);
	await field.fill('beta');
	await expect(clearSearch(page), 'find must bundle an explicit clear-search control').toBeVisible();
	await expectView(page, ['filter-beta']);
	await expectMatchCounts(page, 1, 8);
	await expectCatalogDisclaimer(page);
	await expect(
		page.getByRole('button', { name: /play all|add all|enqueue all|add displayed|queue all/i })
	).toHaveCount(0);
});

test('LF-02 title: mixed case, surrounding whitespace, order and clear restore all', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await field.fill('  SONG  ');
	await expectView(page, ['filter-alpha', 'filter-beta', 'filter-delta']);
	await expectMatchCounts(page, 3, 8);
	await clearSearch(page).click();
	await expect(field).toHaveValue('');
	await expectView(page, API_IDS);
	await expectMatchCounts(page, 8, 8);
	// The control is reusable, not one-shot.
	await field.fill('zulu');
	await expectView(page, ['filter-zulu']);
	await expectMatchCounts(page, 1, 8);
});

test('LF-03 artist and id fallback: harbor, untitled row, titled ids stay unsearched', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await field.fill('HARBOR');
	await expectView(page, ['filter-alpha', 'filter-beta']);
	await expectMatchCounts(page, 2, 8);
	await field.fill('id-fallback');
	await expectView(page, ['id-fallback-track']);
	await expect(row(page, 'id-fallback-track')).toContainText('id-fallback-track');
	// 'filter-alpha' is the id of a TITLED row: ids are searched only through
	// the untitled fallback, so this is a zero-match query.
	await field.fill('filter-alpha');
	await expectNoMatches(page);
});

test('LF-04 literals and folding: metacharacters, accents, zero-match reset', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await field.fill('.*');
	await expectView(page, ['filter-regex']);
	await field.fill('(a|b)');
	await expectView(page, ['filter-regex']);
	await field.fill('c++');
	await expectView(page, ['filter-regex']);
	await field.fill('éclair');
	await expectView(page, ['filter-accent']);
	// No accent stripping: the ASCII query matches nothing.
	await field.fill('eclair');
	await expectNoMatches(page);
	await field.fill('zzz-no-such-track');
	await expectNoMatches(page);
	await resetControl(page).click();
	await expect(field).toHaveValue('');
	await expectView(page, API_IDS);
	await expectMatchCounts(page, 8, 8);
});

test('LF-05 presentation-only: filter and clear change no queue, stars, storage or network', async ({
	page
}) => {
	const mutations: string[] = [];
	page.on('request', (request) => {
		if (request.method() !== 'GET') mutations.push(`${request.method()} ${request.url()}`);
	});
	const api = await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await row(page, 'filter-alpha').click();
	await row(page, 'filter-zulu').click();
	await page.getByTestId('player-next').click();
	await starButton(page, 'filter-beta').click();
	const queued = ['filter-alpha', 'filter-zulu'];
	await expectQueue(page, queued, 'filter-zulu', 'Zulu Last');
	const storageBefore = await readLocalStorage(page);
	const favoritesBefore = await favoriteIds(page);
	expect(favoritesBefore).toEqual(['filter-beta']);

	await field.fill('song');
	await expectView(page, ['filter-alpha', 'filter-beta', 'filter-delta']);
	await expectQueue(page, queued, 'filter-zulu', 'Zulu Last');
	await expect(starButton(page, 'filter-beta')).toHaveAttribute('aria-pressed', 'true');
	expect(await favoriteIds(page)).toEqual(favoritesBefore);
	expect(await readLocalStorage(page)).toEqual(storageBefore);
	expect(api.requests(), 'filtering must not fetch').toBe(1);

	await clearSearch(page).click();
	await expectView(page, API_IDS);
	await expectQueue(page, queued, 'filter-zulu', 'Zulu Last');
	expect(await favoriteIds(page)).toEqual(favoritesBefore);
	expect(await readLocalStorage(page)).toEqual(storageBefore);
	expect(api.requests(), 'clearing must not fetch').toBe(1);
	expect(mutations, 'filtering must not mutate over the network').toEqual([]);
});

test('LF-06 enqueue: clicking a matched row queues that exact stable id', async ({ page }) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await field.fill('gamma');
	await expectView(page, ['filter-gamma']);
	await row(page, 'filter-gamma').click();
	await expectQueue(page, ['filter-gamma'], 'filter-gamma', 'Gamma (Live)');
	// Re-selecting the same matched row is the existing dedupe no-op.
	await row(page, 'filter-gamma').click();
	await expectQueue(page, ['filter-gamma'], 'filter-gamma', 'Gamma (Live)');
	// Keyboard activation queues through the same path.
	await field.fill('zulu');
	await expectView(page, ['filter-zulu']);
	const control = row(page, 'filter-zulu')
		.locator('button, [role="button"], [role="option"]')
		.first();
	await control.focus();
	await expect(control).toBeFocused();
	await page.keyboard.press('Enter');
	await expectQueue(page, ['filter-gamma', 'filter-zulu'], 'filter-gamma', 'Gamma (Live)');
});

test('LF-07 compose: ordering and stars work on the subset; reload drops the query', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await field.fill('song');
	await expectView(page, ['filter-alpha', 'filter-beta', 'filter-delta']);
	// 'a' matches every fixture row, so ordering composes over the full set:
	// title order is deterministic code-unit order (untitled id sorts with
	// the titles, accented title last), artist order puts C++ first with
	// blank artists last.
	await field.fill('a');
	await expectView(page, API_IDS);
	await ordering(page).selectOption({ label: 'Title A–Z' });
	await expectView(page, [
		'filter-alpha',
		'filter-beta',
		'filter-delta',
		'filter-gamma',
		'id-fallback-track',
		'filter-regex',
		'filter-zulu',
		'filter-accent'
	]);
	await ordering(page).selectOption({ label: 'Artist A–Z' });
	await expectView(page, [
		'filter-regex',
		'filter-gamma',
		'filter-alpha',
		'filter-beta',
		'filter-zulu',
		'filter-accent',
		'id-fallback-track',
		'filter-delta'
	]);
	await ordering(page).selectOption({ label: 'Original order' });
	await expectView(page, API_IDS);
	// Starring while filtered lands in Favorites; clearing keeps the star.
	await field.fill('beta');
	await starButton(page, 'filter-beta').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(favoriteItem(page, 'filter-beta')).toContainText('Beta Song');
	await clearSearch(page).click();
	await expectView(page, API_IDS);
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	// Reload is transient: the query does not persist, but stars do.
	await field.fill('beta');
	await expectView(page, ['filter-beta']);
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expectView(page, API_IDS);
	const fresh = await requireFind(page);
	await expect(fresh).toHaveValue('');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
});

test('LF-08 keyboard: tab-reachable native controls, no per-keystroke assertive chatter', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	expect(await field.evaluate((element) => element.tagName)).toBe('INPUT');
	for (
		let tabs = 0;
		tabs < 40 && !(await field.evaluate((element) => element === document.activeElement));
		tabs += 1
	) {
		await page.keyboard.press('Tab');
	}
	await expect(field, 'find field must be reachable by Tab').toBeFocused();
	// SvelteKit injects a framework-owned route announcer
	// (#svelte-announcer, aria-live assertive) outside <main> on every page;
	// it is required for screen-reader route announcements and must not count
	// as build-added assertive chatter.
	await expect(page.locator('[aria-live="assertive"]:not(#svelte-announcer)')).toHaveCount(0);
	await page.keyboard.type('beta', { delay: 20 });
	await expectView(page, ['filter-beta']);
	await expect(
		page.locator('[aria-live="assertive"]:not(#svelte-announcer)'),
		'filtering must not add assertive live regions'
	).toHaveCount(0);
	await expect(page.getByRole('alert'), 'filtering must not raise alerts').toHaveCount(0);
	const clear = clearSearch(page);
	expect(await clear.evaluate((element) => element.tagName)).toBe('BUTTON');
	await clear.focus();
	await expect(clear).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(field).toHaveValue('');
	await expectView(page, API_IDS);
});

test('LF-09 responsive: narrow layout and both color schemes keep find usable', async ({
	page
}) => {
	await mockLibrary(page);
	await page.setViewportSize({ width: 360, height: 800 });
	await openLibrary(page);
	const field = await requireFind(page);
	await expect(field).toBeVisible();
	await field.fill('beta');
	await expectView(page, ['filter-beta']);
	await expect(clearSearch(page), 'clear must stay visible on narrow screens').toBeVisible();
	const overflow = await page.evaluate(
		() => document.documentElement.scrollWidth - document.documentElement.clientWidth
	);
	expect(overflow, 'narrow library must not scroll horizontally').toBeLessThanOrEqual(1);
	for (const scheme of ['dark', 'light'] as const) {
		await page.emulateMedia({ colorScheme: scheme });
		await expect(field, `find field must stay visible in ${scheme} mode`).toBeVisible();
		await field.fill('gamma');
		await expectView(page, ['filter-gamma']);
	}
});

test('LF-10 refresh: failed refresh keeps the filtered snapshot; retry applies find to the new one', async ({
	page
}) => {
	const reply: LibraryReply = { items: ITEMS };
	const api = await mockLibrary(page, reply);
	await openLibrary(page);
	const field = await requireFind(page);
	await field.fill('song');
	await expectView(page, ['filter-alpha', 'filter-beta', 'filter-delta']);
	reply.status = 503;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expect(field, 'query survives a failed refresh').toHaveValue('song');
	await expectView(page, ['filter-alpha', 'filter-beta', 'filter-delta']);
	expect(api.requests()).toBe(2);
	reply.status = 200;
	reply.items = REFRESHED;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, ['fresh-keeper']);
	await expectMatchCounts(page, 1, 3);
	await expect(field).toHaveValue('song');
	expect(api.requests()).toBe(3);
	await clearSearch(page).click();
	await expectView(page, ['fresh-keeper', 'fresh-other', 'fresh-third']);
});

test('LF-11 auth: account change clears the query with the queue and favorites', async ({
	page
}) => {
	const reply: LibraryReply = { items: ITEMS };
	await mockLibrary(page, reply);
	await openLibrary(page);
	const field = await requireFind(page);
	await field.fill('beta');
	await expectView(page, ['filter-beta']);
	await row(page, 'filter-beta').click();
	await starButton(page, 'filter-beta').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await authTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	reply.items = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, ['next-account']);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	const fresh = await requireFind(page);
	await expect(fresh, 'query must not cross the account boundary').toHaveValue('');
	await fresh.fill('next');
	await expectView(page, ['next-account']);
});

test('LF-12 states: loading, error and empty keep their semantics around find', async ({
	page
}) => {
	let release!: () => void;
	const held = new Promise<void>((resolve) => {
		release = resolve;
	});
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
	const reply: LibraryReply = { items: ITEMS };
	const api = await mockLibrary(page, reply);
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectView(page, API_IDS);
	const field = await requireFind(page);
	await expect(field).toHaveValue('');
	reply.items = [];
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByRole('status')).toContainText(/library is empty/i);
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	reply.items = REFRESHED;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expectView(page, ['fresh-keeper', 'fresh-other', 'fresh-third']);
	expect(api.requests()).toBe(3);
});
