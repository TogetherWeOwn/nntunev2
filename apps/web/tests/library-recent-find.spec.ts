import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance-owned: the paired build makes this file pass without editing it.
 * Offline browser contract for recalling recent library find queries without
 * retyping, not implementation/source-shape pins. The existing
 * library-local-filter, library-find-scope, library-find-reliability,
 * library-ordering, library-favorites-local and library-subset-append banks
 * stay protected and unedited, and every behavior they pin must keep passing.
 *
 * Storage contract (favorites pattern, device-local only):
 * - `localStorage` key `nntune.library.recent-finds.v1.<auth-context>` holds a
 *   JSON array of at most 5 distinct non-empty trimmed queries,
 *   most-recent-first, where `<auth-context>` is the session
 *   `nntune.library.auth-context.v1` value. The bare base key is the legacy
 *   unscoped value, cleared (never read) at the auth boundary.
 * - Missing storage, a missing context, corrupt JSON, a non-array payload or
 *   an over-cap payload (more than 5 raw entries) all read as empty: no
 *   chips, the empty state shows, nothing throws, no page error, and unrelated
 *   keys are never touched. Arrays of 5 or fewer filter to valid distinct
 *   non-empty strings (favorites-style). Reads never fetch.
 *
 * Recording contract (applied-then-left):
 * - A query is recorded when it is applied (the find field holds a non-empty
 *   trimmed value and the list has filtered, including a zero-match filter)
 *   and then left via focus loss (blur), Clear search activation, a
 *   successful Refresh/Retry commit, or navigation away with the query
 *   applied. Typing alone never records: no chip appears until a leave event.
 * - Never empty or whitespace-only, never duplicated: re-recording moves the
 *   trimmed value to the front. Cap 5 most-recent-first; the 6th distinct
 *   query evicts the oldest. Stored values are trimmed; re-applying sets the
 *   field to the stored value verbatim.
 *
 * UI contract the build must provide, near the find field:
 * - Section `data-testid="recent-finds"` with an accessible name mentioning
 *   recent searches. When empty it shows `data-testid="recent-finds-empty"`
 *   and no chips. When non-empty it lists one `button` per query with
 *   `data-testid="recent-find-chip"`, accessible name exactly the stored
 *   query, visible text containing it, in most-recent-first DOM order.
 * - Activating a chip re-applies its query verbatim through the existing
 *   filter path (same rows and counts as typing it), moves it to the front
 *   without duplicating, and announces once. Chips never auto-apply: their
 *   presence, a reload restore or an auth purge never fills the field or
 *   filters rows by itself.
 * - An explicit `Clear recent searches` button (exact accessible name) empties
 *   history only: chips gone, empty state shows, storage reads empty,
 *   unrelated keys preserved, and the current find text, filter, queue,
 *   favorites, selection and ordering untouched. It never records.
 * - One concise (140 chars or fewer) `aria-live="polite"` announcement per
 *   real outcome (record, re-apply, clear) in
 *   `data-testid="recent-finds-announcement"`. Typing, empty blurs and reload
 *   restores stay silent. Never an assertive live region or alert for recent
 *   actions. Chips are native buttons: Tab-reachable, Enter/Space operable,
 *   visible focus, no focus dump to <body>, usable at 320px in light/dark.
 *
 * Fixture discipline: visible title/artist text is DIGIT-FREE, so bare digits
 * in <main> can only be counts — and count assertions never run beside a
 * non-empty queue (queue-position text carries digits). Titles/artists avoid
 * the words "star" and "favorite" so star lookups match only the toggle.
 * Queries are exact: 'song' hits three rows, 'harbor' hits two, 'beta' hits
 * one, 'rec-alpha' (id of a TITLED row) hits nothing, 'eclair' (ASCII) misses
 * 'Éclair', 'zzz-no-such-track' is the zero-match probe.
 *
 * Case IDs (stable): RF-00 control (passes on the baseline), RF-01 bundle and
 * empty state, RF-02 blur recording without per-keystroke/empty writes, RF-03
 * dedupe + cap 5 + oldest evicted, RF-04 chip re-applies verbatim, RF-05
 * explicit clear empties history only, RF-06 corrupt/over-cap fail-closed,
 * RF-07 auth-boundary purge, RF-08 reload restores bounded without implicit
 * execution, RF-09 no fetch and no neighbor mutation, RF-10 keyboard + polite
 * announcements, RF-11 narrow light/dark + touch, RF-12 clear/refresh/
 * navigation leave variants.
 *
 * Focused command (from apps/web, deterministic, offline):
 *   pnpm exec playwright test tests/library-recent-find.spec.ts
 */

const ITEMS = [
	{ id: 'rec-alpha', title: 'Harbor Song', artist: 'Echo' },
	{ id: 'rec-beta', title: 'Beta Song', artist: 'Harbor Lights' },
	{ id: 'rec-gamma', title: 'Gamma Tune', artist: 'Echo' },
	{ id: 'rec-untitled' },
	{ id: 'rec-delta', title: 'Delta Song', artist: '   ' },
	{ id: 'rec-regex', title: 'What.*Ever', artist: 'C++ Players' },
	{ id: 'rec-accent', title: 'Éclair', artist: 'écho' },
	{ id: 'rec-zulu', title: 'Zulu Last', artist: 'Zulu' }
];

const API_IDS = ITEMS.map((item) => item.id);

const NEXT_ACCOUNT = [
	{ id: 'next-rec', title: 'Next Tune', artist: 'Harbor Next' },
	{ id: 'next-other', title: 'Other Tune', artist: 'Other Band' }
];

const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';
const AUTH_TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const RECENT_BASE = 'nntune.library.recent-finds.v1';

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
	await expectRows(page, API_IDS);
}

async function expectRows(page: Page, ids: string[]) {
	await expect
		.poll(() =>
			page
				.getByTestId('library-item')
				.evaluateAll((rows) => rows.map((row) => row.getAttribute('data-id')))
		)
		.toEqual(ids);
}

function findField(page: Page): Locator {
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
	return page.getByRole('button', { name: /clear search/i }).first();
}

function recentSection(page: Page): Locator {
	return page.getByTestId('recent-finds');
}

function recentChips(page: Page): Locator {
	return page.getByTestId('recent-find-chip');
}

function recentEmpty(page: Page): Locator {
	return page.getByTestId('recent-finds-empty');
}

function recentAnnouncement(page: Page): Locator {
	return page.getByTestId('recent-finds-announcement');
}

function clearRecents(page: Page): Locator {
	return page.getByRole('button', { name: 'Clear recent searches', exact: true });
}

function recentChip(page: Page, query: string): Locator {
	return recentSection(page).getByRole('button', { name: query, exact: true });
}

async function chipNames(page: Page): Promise<string[]> {
	return recentChips(page).evaluateAll((els) =>
		els.map((el) => (el.getAttribute('aria-label') ?? el.textContent ?? '').trim())
	);
}

async function expectChips(page: Page, expected: string[]) {
	await expect
		.poll(() => chipNames(page), {
			message: `recent chips must be [${expected.join(', ')}] most-recent-first`
		})
		.toEqual(expected);
}

async function readAuthContext(page: Page): Promise<string | null> {
	return page.evaluate((key) => window.sessionStorage.getItem(key), AUTH_CONTEXT_KEY);
}

async function readRecentRaw(page: Page, context: string | null): Promise<string | null> {
	return page.evaluate(
		({ base, ctx }) => (ctx ? window.localStorage.getItem(`${base}.${ctx}`) : null),
		{ base: RECENT_BASE, ctx: context }
	);
}

async function readBaseRaw(page: Page): Promise<string | null> {
	return page.evaluate((base) => window.localStorage.getItem(base), RECENT_BASE);
}

function storedQueries(raw: string | null): string[] {
	if (!raw) return [];
	try {
		const parsed: unknown = JSON.parse(raw);
		if (!Array.isArray(parsed)) return [];
		return parsed.filter(
			(value): value is string => typeof value === 'string' && value.length > 0
		);
	} catch {
		return [];
	}
}

async function readLocalStorage(page: Page): Promise<[string, string][]> {
	return page.evaluate(() => Object.entries(window.localStorage).sort() as [string, string][]);
}

async function authTransition(page: Page) {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'recent-find-test');
		window.localStorage.removeItem(key);
	}, AUTH_TRANSITION_KEY);
	await otherTab.close();
}

async function recordViaBlur(page: Page, field: Locator, query: string) {
	await field.fill(query);
	await expect(field).toHaveValue(query);
	await field.blur();
}

async function expectNoAssertive(page: Page, why: string) {
	await expect(
		page.locator('[aria-live="assertive"]:not(#svelte-announcer)'),
		why
	).toHaveCount(0);
	await expect(page.getByRole('alert'), why).toHaveCount(0);
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

test('[RF-00] control: shipped find, API order and queueing work before recents land', async ({
	page
}) => {
	const api = await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await field.fill('harbor');
	await expectRows(page, ['rec-alpha', 'rec-beta']);
	await clearSearch(page).click();
	await expect(field).toHaveValue('');
	await expectRows(page, API_IDS);
	await page.locator('[data-testid="library-item"][data-id="rec-alpha"]').click();
	await page.locator('[data-testid="library-item"][data-id="rec-zulu"]').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(page.getByTestId('now-playing')).toContainText('Harbor Song');
	expect(api.requests()).toBe(1);
});

test('[RF-01] bundle: labeled recents section with an honest empty state and no premature chips', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const section = recentSection(page);
	await expect(section, 'library must expose a labeled recent-searches section').toBeVisible();
	const label = await section.evaluate((el) => {
		const labelled = el.getAttribute('aria-label') ?? '';
		const labelledby = (el.getAttribute('aria-labelledby') ?? '')
			.split(/\s+/)
			.filter(Boolean)
			.map((id) => document.getElementById(id)?.textContent ?? '')
			.join(' ');
		const heading = el.querySelector('h1, h2, h3, h4, h5, h6')?.textContent ?? '';
		return `${labelled} ${labelledby} ${heading}`;
	});
	expect(label, 'recents section must name recent searches').toMatch(/recent search/i);
	await expect(recentEmpty(page), 'empty history must show an honest empty state').toBeVisible();
	await expect(recentChips(page)).toHaveCount(0);
	// An empty history offers no explicit clear: there is nothing to clear.
	await expect(clearRecents(page)).toHaveCount(0);
	const field = await requireFind(page);
	await field.fill('beta');
	await expectRows(page, ['rec-beta']);
	// Typing alone never records: the empty state stands until a leave event.
	await expect(recentChips(page)).toHaveCount(0);
	await expect(recentEmpty(page)).toBeVisible();
	await expectNoAssertive(page, 'typing must not raise assertive regions or alerts');
});

test('[RF-02] blur records applied queries only: no per-keystroke or empty writes', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await expect(recentSection(page)).toBeVisible();

	// Per-keystroke probe: each character filters yet records nothing.
	await field.fill('');
	await field.focus();
	for (const char of ['b', 'e', 't', 'a']) {
		await page.keyboard.type(char, { delay: 20 });
		await expect(recentChips(page), `typing ${char} must not record`).toHaveCount(0);
	}
	await expectRows(page, ['rec-beta']);
	await expect(recentEmpty(page)).toBeVisible();
	await field.blur();
	await expectChips(page, ['beta']);
	expect(storedQueries(await readRecentRaw(page, await readAuthContext(page)))).toEqual(['beta']);

	// Empty and whitespace-only blurs never record and never overwrite history.
	await field.fill('');
	await field.blur();
	await field.fill('   ');
	await field.blur();
	await expectChips(page, ['beta']);
	expect(storedQueries(await readRecentRaw(page, await readAuthContext(page)))).toEqual(['beta']);

	// A zero-match query is still applied: leaving it records the probe.
	await recordViaBlur(page, field, 'zzz-no-such-track');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expectChips(page, ['zzz-no-such-track', 'beta']);
});

test('[RF-03] dedupe moves to front; the 6th distinct query evicts the oldest', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await expect(recentSection(page)).toBeVisible();

	for (const query of ['beta', 'gamma', 'delta', 'zulu', 'echo']) {
		await recordViaBlur(page, field, query);
	}
	await expectChips(page, ['echo', 'zulu', 'delta', 'gamma', 'beta']);
	expect(storedQueries(await readRecentRaw(page, await readAuthContext(page)))).toEqual([
		'echo',
		'zulu',
		'delta',
		'gamma',
		'beta'
	]);

	// Re-recording moves to the front without duplicating.
	await recordViaBlur(page, field, 'beta');
	await expectChips(page, ['beta', 'echo', 'zulu', 'delta', 'gamma']);

	// The 6th distinct query evicts the oldest ('gamma' after the move above).
	await recordViaBlur(page, field, 'harbor');
	await expectChips(page, ['harbor', 'beta', 'echo', 'zulu', 'delta']);
	const stored = storedQueries(await readRecentRaw(page, await readAuthContext(page)));
	expect(stored).toEqual(['harbor', 'beta', 'echo', 'zulu', 'delta']);
	expect(stored, 'cap 5 must hold in storage as well as in chips').toHaveLength(5);

	// Surrounding whitespace is trimmed once: no padded duplicate.
	await recordViaBlur(page, field, '  harbor  ');
	await expectChips(page, ['harbor', 'beta', 'echo', 'zulu', 'delta']);
});

test('[RF-04] chip re-applies verbatim through the filter path and moves to front', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await recordViaBlur(page, field, 'beta');
	await recordViaBlur(page, field, 'harbor');
	await expectChips(page, ['harbor', 'beta']);

	// Chips never auto-apply: clearing the field leaves every row visible.
	await clearSearch(page).click();
	await expect(field).toHaveValue('');
	await expectRows(page, API_IDS);
	await expectChips(page, ['harbor', 'beta']);

	// Re-apply the older chip verbatim: same field value, same rows, same
	// count as typing it by hand, and the chip moves to the front.
	await recentChip(page, 'beta').click();
	await expect(field).toHaveValue('beta');
	await expectRows(page, ['rec-beta']);
	await expect(page.getByTestId('library-find-count')).toContainText(/\b1\b/);
	await expectChips(page, ['beta', 'harbor']);

	// Literal metacharacters travel the same path: no regex, no accent strip.
	await recordViaBlur(page, field, 'c++');
	await clearSearch(page).click();
	await recentChip(page, 'c++').click();
	await expect(field).toHaveValue('c++');
	await expectRows(page, ['rec-regex']);
	await recordViaBlur(page, field, 'eclair');
	await clearSearch(page).click();
	await recentChip(page, 'eclair').click();
	await expect(field).toHaveValue('eclair');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
});

test('[RF-05] explicit clear empties history only and preserves the live query', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await recordViaBlur(page, field, 'beta');
	await recordViaBlur(page, field, 'gamma');
	await expectChips(page, ['gamma', 'beta']);
	await page.evaluate(() => window.localStorage.setItem('test.unrelated', 'keep'));

	const clear = clearRecents(page);
	await expect(clear, 'non-empty history must offer an explicit clear').toBeVisible();
	expect(await clear.evaluate((el) => el.tagName)).toBe('BUTTON');
	await clear.click();
	await expect(recentChips(page)).toHaveCount(0);
	await expect(recentEmpty(page)).toBeVisible();
	expect(
		storedQueries(await readRecentRaw(page, await readAuthContext(page))),
		'clear must empty stored history'
	).toEqual([]);
	expect(await page.evaluate(() => window.localStorage.getItem('test.unrelated'))).toBe('keep');

	// History clear never touches the live query: the filter still applies and
	// clearing history does not immediately re-record the visible text.
	await expect(field).toHaveValue('gamma');
	await expectRows(page, ['rec-gamma']);
	await expect(recentChips(page)).toHaveCount(0);
	await expect(clearRecents(page)).toHaveCount(0);
});

test('[RF-06] corrupt, foreign and over-cap storage reads fail closed without touching neighbors', async ({
	page
}) => {
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('console', (message) => {
		if (message.type() === 'error') errors.push(message.text());
	});
	await page.addInitScript(
		({ contextKey, base }) => {
			window.sessionStorage.setItem(contextKey, 'test-corrupt-account');
			window.localStorage.setItem(`${base}.test-corrupt-account`, '{not-json');
			window.localStorage.setItem('test.unrelated', 'keep');
		},
		{ contextKey: AUTH_CONTEXT_KEY, base: RECENT_BASE }
	);
	await mockLibrary(page);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(recentSection(page)).toBeVisible();
	await expect(recentEmpty(page)).toBeVisible();
	await expect(recentChips(page)).toHaveCount(0);
	expect(await page.evaluate(() => window.localStorage.getItem('test.unrelated'))).toBe('keep');

	// The section still works after the corrupt value is ignored.
	const field = await requireFind(page);
	await recordViaBlur(page, field, 'beta');
	await expectChips(page, ['beta']);

	// A foreign (non-array) payload fails closed on the next visit.
	await page.evaluate(
		({ base }) => window.localStorage.setItem(`${base}.test-corrupt-account`, '{"q":"beta"}'),
		{ base: RECENT_BASE }
	);
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(recentEmpty(page)).toBeVisible();
	await expect(recentChips(page)).toHaveCount(0);

	// Over-cap (6 valid entries) reads as empty, never truncated to 5.
	await page.evaluate(
		({ base }) =>
			window.localStorage.setItem(
				`${base}.test-corrupt-account`,
				JSON.stringify(['one', 'two', 'three', 'four', 'five', 'six'])
			),
		{ base: RECENT_BASE }
	);
	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(recentEmpty(page)).toBeVisible();
	await expect(recentChips(page)).toHaveCount(0);
	expect(await page.evaluate(() => window.localStorage.getItem('test.unrelated'))).toBe('keep');
	expect(errors, 'corrupt or over-cap history must never throw').toEqual([]);
});

test('[RF-07] auth boundary purges recents so the next account starts empty', async ({ page }) => {
	const reply: LibraryReply = { items: ITEMS };
	await mockLibrary(page, reply);
	await openLibrary(page);
	const field = await requireFind(page);
	await recordViaBlur(page, field, 'beta');
	await recordViaBlur(page, field, 'gamma');
	await expectChips(page, ['gamma', 'beta']);
	const before = await readAuthContext(page);
	expect(before).not.toBeNull();
	expect(storedQueries(await readRecentRaw(page, before))).toEqual(['gamma', 'beta']);
	await page.evaluate(
		({ base }) => window.localStorage.setItem(base, JSON.stringify(['stale-legacy'])),
		{ base: RECENT_BASE }
	);

	await authTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	expect(
		await readRecentRaw(page, before),
		'previous-account recents must be purged at the auth boundary'
	).toBeNull();
	expect(await readBaseRaw(page), 'legacy unscoped recents must be purged too').toBeNull();
	await expect(page.getByTestId('library-item')).toHaveCount(0);

	reply.items = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(
		page.getByTestId('library-item').evaluateAll((rows) => rows.map((r) => r.getAttribute('data-id')))
	).resolves.toEqual(['next-rec', 'next-other']);
	await expect(recentEmpty(page)).toBeVisible();
	await expect(recentChips(page)).toHaveCount(0);
	const fresh = await requireFind(page);
	await expect(fresh, 'query must not cross the account boundary').toHaveValue('');

	// The next account records only its own queries; the old ones never return.
	await recordViaBlur(page, fresh, 'next');
	await expectChips(page, ['next']);
	expect(storedQueries(await readRecentRaw(page, await readAuthContext(page)))).toEqual(['next']);
	expect(await readRecentRaw(page, before)).toBeNull();
});

test('[RF-08] reload restores at most the bounded list without executing any query', async ({
	page
}) => {
	const api = await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await recordViaBlur(page, field, 'beta');
	await recordViaBlur(page, field, 'gamma');
	await recordViaBlur(page, field, 'harbor');
	await expectChips(page, ['harbor', 'gamma', 'beta']);
	expect(api.requests()).toBe(1);

	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expectRows(page, API_IDS);
	await expectChips(page, ['harbor', 'gamma', 'beta']);
	const fresh = await requireFind(page);
	await expect(fresh, 'reload must not auto-fill a recent query').toHaveValue('');
	expect(api.requests(), 'restore must not fetch beyond the reload itself').toBe(2);

	// Restored chips re-apply on demand only.
	await recentChip(page, 'gamma').click();
	await expect(fresh).toHaveValue('gamma');
	await expectRows(page, ['rec-gamma']);
	expect(api.requests()).toBe(2);
});

test('[RF-09] recents never fetch, queue, star, select or persist beyond their own key', async ({
	page
}) => {
	const mutations: string[] = [];
	page.on('request', (request) => {
		if (request.method() !== 'GET') mutations.push(`${request.method()} ${request.url()}`);
	});
	const api = await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await expect(recentSection(page)).toBeVisible();

	// Neighbor state under observation: one queued track, one star, one choice.
	await page.locator('[data-testid="library-item"][data-id="rec-alpha"]').click();
	await page
		.locator('[data-testid="library-item"][data-id="rec-beta"]')
		.getByRole('button', { name: /star|favorite/i })
		.click();
	await page.getByRole('checkbox', { name: 'Select Beta Song', exact: true }).check();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
	const storageBefore = (await readLocalStorage(page)).filter(([key]) => !key.startsWith(RECENT_BASE));

	await recordViaBlur(page, field, 'beta');
	await expectChips(page, ['beta']);
	await clearSearch(page).click();
	await recentChip(page, 'beta').click();
	await expectRows(page, ['rec-beta']);
	await clearRecents(page).click();
	await expect(recentChips(page)).toHaveCount(0);

	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Harbor Song');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
	const storageAfter = (await readLocalStorage(page)).filter(([key]) => !key.startsWith(RECENT_BASE));
	expect(storageAfter, 'recents must not touch neighboring storage').toEqual(storageBefore);
	expect(api.requests(), 'recording, re-applying and clearing must not fetch').toBe(1);
	expect(mutations, 'recents must not mutate over the network').toEqual([]);
	await expectNoAssertive(page, 'recent actions must stay polite and alert-free');
});

test('[RF-10] keyboard reaches every chip; each real outcome announces once, politely and briefly', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await recordViaBlur(page, field, 'beta');
	await recordViaBlur(page, field, 'gamma');
	await expectChips(page, ['gamma', 'beta']);
	const announcement = recentAnnouncement(page);
	await expect(announcement, 'recents must own one polite live region').toHaveAttribute(
		'aria-live',
		'polite'
	);

	async function announcementText(): Promise<string> {
		return ((await announcement.textContent()) ?? '').trim();
	}
	async function expectConcise(outcome: string) {
		const text = await announcementText();
		expect(text, `${outcome} must announce concisely`).not.toBe('');
		expect(text.length, `${outcome} announcement must stay brief`).toBeLessThanOrEqual(140);
	}

	// Recording announced; silent typing leaves the announcement untouched.
	await expectConcise('recording');
	const afterRecord = await announcementText();
	await field.fill('zulu');
	await expectRows(page, ['rec-zulu']);
	expect(await announcementText(), 'typing must not announce').toBe(afterRecord);
	await field.fill('');
	await field.blur();
	expect(await announcementText(), 'empty blurs stay silent').toBe(afterRecord);

	// Every chip is a Tab-reachable native button with visible focus.
	await clearSearch(page).first().isHidden().catch(() => true);
	await field.fill('');
	for (const query of ['gamma', 'beta']) {
		const chip = recentChip(page, query);
		expect(await chip.evaluate((el) => el.tagName)).toBe('BUTTON');
		for (
			let tabs = 0;
			tabs < 80 && !(await chip.evaluate((el) => el === document.activeElement));
			tabs += 1
		) {
			await page.keyboard.press('Tab');
		}
		await expect(chip, `chip ${query} must be reachable by Tab`).toBeFocused();
		const ring = await chip.evaluate((el) => {
			const style = getComputedStyle(el);
			return {
				outline: style.outlineStyle !== 'none' && style.outlineWidth !== '0px',
				shadow: style.boxShadow !== 'none'
			};
		});
		expect(ring.outline || ring.shadow, `chip ${query} focus must be visible`).toBe(true);
	}

	// Keyboard activation re-applies verbatim and announces the new outcome.
	const betaChip = recentChip(page, 'beta');
	await betaChip.focus();
	await page.keyboard.press('Enter');
	await expect(field).toHaveValue('beta');
	await expectRows(page, ['rec-beta']);
	await expectChips(page, ['beta', 'gamma']);
	await expectConcise('chip re-apply');
	expect(await announcementText(), 're-apply must announce its own outcome').not.toBe(afterRecord);
	expect(
		await page.evaluate(() => document.activeElement === document.body),
		'chip activation must not dump focus to <body>'
	).toBe(false);

	await clearRecents(page).click();
	await expect(recentChips(page)).toHaveCount(0);
	await expectConcise('explicit clear');
	await expectNoAssertive(page, 'recent outcomes must never assert or alert');
});

test.describe('narrow viewport with touch', () => {
	test.use({ hasTouch: true });

	test('[RF-11] 320px light/dark keeps recents visible and tappable with no sideways scroll', async ({
		page
	}) => {
		await mockLibrary(page);
		await page.setViewportSize({ width: 320, height: 700 });
		await openLibrary(page);
		const field = await requireFind(page);
		await recordViaBlur(page, field, 'beta');
		await recordViaBlur(page, field, 'harbor');
		await expectChips(page, ['harbor', 'beta']);
		await clearSearch(page).click();

		await recentChip(page, 'beta').tap();
		await expect(field).toHaveValue('beta');
		await expectRows(page, ['rec-beta']);

		for (const scheme of ['dark', 'light'] as const) {
			await page.emulateMedia({ colorScheme: scheme });
			await expect(recentSection(page), `recents must stay visible in ${scheme}`).toBeVisible();
			await expect(recentChip(page, 'beta'), `chip must stay visible in ${scheme}`).toBeVisible();
			await expect(clearRecents(page), `clear must stay visible in ${scheme}`).toBeVisible();
			await recentChip(page, 'harbor').tap();
			await expect(field).toHaveValue('harbor');
			await expectRows(page, ['rec-alpha', 'rec-beta']);
			const overflow = await page.evaluate(
				() => document.documentElement.scrollWidth - document.documentElement.clientWidth
			);
			expect(overflow, `no horizontal scrolling at 320px in ${scheme}`).toBeLessThanOrEqual(1);
		}
	});
});

test('[RF-12] leaving via clear, refresh or navigation records the applied query', async ({
	page
}) => {
	const reply: LibraryReply = { items: ITEMS };
	const api = await mockLibrary(page, reply);
	await openLibrary(page);
	const field = await requireFind(page);
	await expect(recentSection(page)).toBeVisible();

	// Clear search records the applied query, then empties only the field.
	await field.fill('beta');
	await expectRows(page, ['rec-beta']);
	await clearSearch(page).click();
	await expect(field).toHaveValue('');
	await expectRows(page, API_IDS);
	await expectChips(page, ['beta']);

	// A successful refresh commits first, then records the preserved query.
	reply.items = ITEMS.map((item) =>
		item.id === 'rec-beta' ? { ...item, title: 'Beta Song refreshed' } : item
	);
	await field.fill('gamma');
	await expectRows(page, ['rec-gamma']);
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.locator('[data-testid="library-item"][data-id="rec-beta"]')).toContainText(
		'Beta Song refreshed'
	);
	await expect(field, 'same-account refresh preserves the query').toHaveValue('gamma');
	await expectChips(page, ['gamma', 'beta']);
	expect(api.requests()).toBe(2);

	// Navigation preserves the bounded list; nothing is lost or duplicated.
	await page.goto('/login');
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expectChips(page, ['gamma', 'beta']);
	const fresh = await requireFind(page);
	await expect(fresh).toHaveValue('');
});
