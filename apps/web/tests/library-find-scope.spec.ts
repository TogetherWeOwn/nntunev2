import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance-owned: the paired build makes this file pass without editing
 * it. Offline browser contract for narrowing the loaded-library find to
 * title or artist, not implementation/source-shape pins. The pure half is
 * `src/lib/library/find-scope-acceptance.test.ts`; the existing
 * library-local-filter, library-find-reliability, library-ordering,
 * library-favorites-management and library-subset-append banks stay
 * protected and unedited, and every behavior they pin must keep passing.
 *
 * UI contract the build must provide:
 * - A field-scope selector for the loaded-library find, beside/near the
 *   existing "Find in loaded tracks" field, whose options read exactly
 *   "Both title and artist", "Title only" and "Artist only" in that
 *   order, defaulting to "Both title and artist". Its own accessible name
 *   says it chooses the searched field (for example "Search in", "Find
 *   in", "Search by", "Find scope" or "Field").
 * - Shape is the build's choice among native, keyboard/touch-operable
 *   controls: a native <select> (the shipped Show/Order pattern), native
 *   radio inputs, or toggle <button>s whose active option carries
 *   `aria-pressed="true"` (inactive options `"false"`). The active scope
 *   must always be programmatically readable from the control, never
 *   color alone.
 * - No query syntax (text like "artist:x" stays a literal needle) and no
 *   change to the queue find.
 * - Changing scope re-runs the SAME displayed-items pipeline as the query,
 *   the All tracks/Favorites only view and the chosen ordering: scope,
 *   view, ordering and checkbox selections compose by intersection and
 *   none of them resets another. Scope changes keep the query text, make
 *   no request, and touch no queue, current track, media element,
 *   favorites, selection or storage.
 * - "Clear search" clears ONLY the text and retains the explicitly chosen
 *   scope. A reload and an account/session teardown return the scope to
 *   Both (visit-local state, like the query). Scope is never persisted:
 *   choosing it writes nothing to localStorage.
 * - The existing `library-find-count` element keeps showing matched/loaded
 *   numbers AND names the narrowed field while Title only or Artist only
 *   is active (for example "Showing 1 of 8 tracks (title only)"). A
 *   zero-match state under a narrowed scope says which field found
 *   nothing in the no-matches message itself and offers a clear way back
 *   to Both —
 *   a visibly labeled both-fields action, or the still-visible selector —
 *   that recovers matches WITHOUT losing the typed text.
 * - Feedback is the updated list/count (polite or plain text), never an
 *   assertive live region or alert, and operating the selector never
 *   dumps focus to <body>. At 320px in light and dark the field and
 *   selector stay visible and operable with no horizontal scrolling.
 *
 * Fixture discipline: visible title/artist text is digit-free and avoids
 * the words "star"/"favorite", so count, star and checkbox lookups can
 * only match their own controls. 'harbor' is a title word on scope-alpha
 * and an artist word on scope-beta; 'echo' is an artist on two rows and a
 * title on none; scope-untitled has neither field (id fallback);
 * scope-delta has a blank artist.
 *
 * Case IDs (stable): FS-B00 control (passes on the baseline), FS-B01
 * bundle/default/literal syntax, FS-B02 field narrowing, FS-B03 counts,
 * zero-match copy and text-preserving recovery to Both, FS-B04 clear
 * retains scope, FS-B05 Favorites-view composition, FS-B06 ordering
 * composition, FS-B07 hidden-selection composition, FS-B08 no
 * network/media/queue/favorites/storage mutation, FS-B09 keyboard and
 * visible focus, FS-B10 320px light/dark and touch, FS-B11 reload reset,
 * FS-B12 account-teardown reset.
 *
 * Focused command (repo root, deterministic, offline):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test tests/library-find-scope.spec.ts --workers=1 --retries=0
 */

const ITEMS = [
	{ id: 'scope-alpha', title: 'Harbor Song', artist: 'Echo' },
	{ id: 'scope-beta', title: 'Morning Song', artist: 'Harbor Lights' },
	{ id: 'scope-gamma', title: 'Gamma Tune', artist: 'Echo' },
	{ id: 'scope-untitled' },
	{ id: 'scope-delta', title: 'Delta Song', artist: '   ' },
	{ id: 'scope-regex', title: 'What.*Ever', artist: 'C++ Players' },
	{ id: 'scope-accent', title: 'Éclair', artist: 'écho' },
	{ id: 'scope-zulu', title: 'Zulu Last', artist: 'Zulu' }
];
const API_IDS = ITEMS.map((item) => item.id);

const NEXT_ACCOUNT = [
	{ id: 'next-scope', title: 'Next Tune', artist: 'Harbor Next' },
	{ id: 'next-other', title: 'Other Tune', artist: 'Other Band' }
];

const SCOPE_LABELS = ['Both title and artist', 'Title only', 'Artist only'] as const;
type ScopeLabel = (typeof SCOPE_LABELS)[number];

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
	await expect(field, 'library must expose the labeled find-in-loaded-tracks field').toBeVisible();
	return field;
}

function clearSearch(page: Page): Locator {
	return page.getByRole('button', { name: /clear search/i }).first();
}

/* ---- scope selector: one of the three sanctioned native shapes ---- */

function scopeSelect(page: Page): Locator {
	return page.getByRole('combobox', {
		name: /search in|find in|search by|find by|match in|match against|look in|find scope|search scope|field|scope/i
	});
}

function scopeRadio(page: Page, label: ScopeLabel): Locator {
	return page.getByRole('radio', { name: label });
}

function scopeButton(page: Page, label: ScopeLabel): Locator {
	return page.getByRole('button', { name: label, exact: true });
}

type ScopeKind = 'select' | 'radio' | 'button';

async function scopeKind(page: Page): Promise<ScopeKind | null> {
	if ((await scopeSelect(page).count()) > 0) return 'select';
	if ((await scopeRadio(page, 'Title only').count()) > 0) return 'radio';
	if ((await scopeButton(page, 'Title only').count()) > 0) return 'button';
	return null;
}

async function requireScope(page: Page): Promise<ScopeKind> {
	const kind = await scopeKind(page);
	expect(
		kind,
		'library find must expose a labeled Both title and artist / Title only / Artist only ' +
			'selector (native select, radios, or aria-pressed buttons)'
	).not.toBeNull();
	return kind as ScopeKind;
}

async function expectScopeOptions(page: Page, kind: ScopeKind) {
	if (kind === 'select') {
		const options = await scopeSelect(page)
			.locator('option')
			.evaluateAll((nodes) => nodes.map((node) => (node.textContent ?? '').trim()));
		expect(options, 'scope selector options, in order').toEqual([...SCOPE_LABELS]);
		await expect(scopeSelect(page)).toBeVisible();
	} else if (kind === 'radio') {
		for (const label of SCOPE_LABELS) {
			await expect(scopeRadio(page, label), `scope radio "${label}"`).toHaveCount(1);
			await expect(scopeRadio(page, label)).toBeVisible();
		}
	} else {
		for (const label of SCOPE_LABELS) {
			await expect(scopeButton(page, label), `scope button "${label}"`).toHaveCount(1);
			await expect(scopeButton(page, label)).toBeVisible();
		}
	}
}

async function currentScope(page: Page, kind: ScopeKind): Promise<ScopeLabel | null> {
	if (kind === 'select') {
		const text = await scopeSelect(page).locator('option:checked').textContent();
		const label = (text ?? '').trim() as ScopeLabel;
		return SCOPE_LABELS.includes(label) ? label : null;
	}
	if (kind === 'radio') {
		for (const label of SCOPE_LABELS) {
			if (await scopeRadio(page, label).isChecked()) return label;
		}
		return null;
	}
	for (const label of SCOPE_LABELS) {
		const state = await scopeButton(page, label).evaluate((element) => ({
			pressed: element.getAttribute('aria-pressed'),
			checked: element.getAttribute('aria-checked')
		}));
		if (state.pressed === 'true' || state.checked === 'true') return label;
	}
	return null;
}

async function expectScope(page: Page, kind: ScopeKind, label: ScopeLabel) {
	await expect
		.poll(() => currentScope(page, kind), { message: `active find scope must be "${label}"` })
		.toBe(label);
}

async function setScope(page: Page, kind: ScopeKind, label: ScopeLabel) {
	if (kind === 'select') {
		await scopeSelect(page).selectOption({ label });
	} else if (kind === 'radio') {
		await scopeRadio(page, label).check();
	} else {
		await scopeButton(page, label).click();
	}
	await expectScope(page, kind, label);
}

/** The control a keyboard user tabs onto for the CURRENT scope. */
function scopeFocusable(page: Page, kind: ScopeKind, current: ScopeLabel): Locator {
	if (kind === 'select') return scopeSelect(page);
	if (kind === 'radio') return scopeRadio(page, current);
	return scopeButton(page, current);
}

async function returnToBoth(page: Page, kind: ScopeKind) {
	// A dedicated, visibly labeled back-to-both action (also matches the
	// toggle-button shape's own Both button). The shipped "Clear search"
	// button never matches this vocabulary.
	const action = page
		.getByRole('button', { name: /both|all fields|title and artist/i })
		.first();
	if ((await action.count()) > 0 && (await action.isVisible())) {
		await action.click();
		await expectScope(page, kind, 'Both title and artist');
		return;
	}
	const link = page.getByRole('link', { name: /both|all fields|title and artist/i }).first();
	if ((await link.count()) > 0 && (await link.isVisible())) {
		await link.click();
		await expectScope(page, kind, 'Both title and artist');
		return;
	}
	// Otherwise the selector itself must still be reachable in the
	// zero-match state and switching it must be the recovery.
	await setScope(page, kind, 'Both title and artist');
}

/* ---- neighbors already shipped ---- */

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function starButton(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /star|favorite/i });
}

async function favoriteIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('favorite-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-id')));
}

function view(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Show', exact: true });
}

function ordering(page: Page): Locator {
	return page.getByRole('combobox', { name: /order|sort/i });
}

async function readLocalStorage(page: Page): Promise<[string, string][]> {
	return page.evaluate(() => Object.entries(window.localStorage).sort() as [string, string][]);
}

async function queueState(page: Page) {
	return page.evaluate(() => {
		const rows = [...document.querySelectorAll('[data-testid="queue-item"]')];
		const current = rows.find((item) => item.getAttribute('aria-current') === 'true');
		return {
			ids: rows.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id')),
			currentId: current?.getAttribute('data-track-id') ?? current?.getAttribute('data-id') ?? null,
			now: document.querySelector('[data-testid="now-playing"]')?.textContent ?? null,
			media: [...document.querySelectorAll('audio, video')].map((element) => {
				const media = element as HTMLMediaElement;
				return { src: media.currentSrc, paused: media.paused, ended: media.ended };
			}),
			calls: Reflect.get(window, '__scopeMediaCalls') ?? null
		};
	});
}

async function expectQueue(page: Page, ids: string[], current: string, title: string) {
	await expect
		.poll(async () => (await queueState(page)).ids)
		.toEqual(ids);
	await expect(page.getByTestId('now-playing')).toContainText(title);
	await expect.poll(async () => (await queueState(page)).currentId).toBe(current);
}

async function expectFindCount(
	page: Page,
	matching: number,
	loaded: number,
	scopeWord?: 'title' | 'artist'
) {
	const count = page.getByTestId('library-find-count');
	await expect(count, 'find count element is missing').toBeVisible();
	await expect(count).toContainText(new RegExp(`\\b${matching}\\s*(?:of|/)\\s*${loaded}\\b`));
	if (scopeWord) {
		await expect(
			count,
			`find count must name the active ${scopeWord}-only scope`
		).toContainText(new RegExp(scopeWord, 'i'));
	}
}

async function expectScopedZeroMatch(page: Page, scopeWord: 'title' | 'artist') {
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	const message = page.locator('main').getByText(/no .*match|no results|nothing found/i).first();
	await expect(message, 'zero matches must show a distinct no-matches state').toBeVisible();
	// The scope must be named in the no-matches message itself — surrounding
	// page text does not satisfy this, because the selector also reads
	// "Title only" on a 1-match state where no copy change is needed.
	const ownText = await message.evaluate((element) => element.textContent ?? '');
	expect(ownText, 'zero-match copy must identify the active scope').toMatch(
		new RegExp(scopeWord, 'i')
	);
}

async function authTransition(page: Page) {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate(() => {
		localStorage.setItem('nntune.library.auth-transition.v1', 'find-scope-test');
		localStorage.removeItem('nntune.library.auth-transition.v1');
	});
	await otherTab.close();
}

test.beforeEach(async ({ context }) => {
	// No catalog, account, media or production network. The specific library
	// page.route mocks below take precedence over this context boundary.
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) {
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
});

test('[FS-B00] control: shipped find, API order and queueing work before scope lands', async ({
	page
}) => {
	const api = await mockLibrary(page);
	await openLibrary(page);
	const field = await requireFind(page);
	await field.fill('harbor');
	// Shipped behavior: title OR artist, so both collision rows show.
	await expectRows(page, ['scope-alpha', 'scope-beta']);
	await expectFindCount(page, 2, ITEMS.length);
	await clearSearch(page).click();
	await expect(field).toHaveValue('');
	await expectRows(page, API_IDS);
	await row(page, 'scope-alpha').click();
	await row(page, 'scope-zulu').click();
	await expectQueue(page, ['scope-alpha', 'scope-zulu'], 'scope-alpha', 'Harbor Song');
	expect(api.requests()).toBe(1);
});

test('[FS-B01] bundle: labeled selector defaults to Both, shipped match preserved, syntax stays literal', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireScope(page);
	await expectScopeOptions(page, kind);
	await expectScope(page, kind, 'Both title and artist');
	const field = await requireFind(page);
	await expect(field).toHaveValue('');
	await field.fill('harbor');
	await expectRows(page, ['scope-alpha', 'scope-beta']);
	await expectFindCount(page, 2, ITEMS.length);
	// No query syntax: field prefixes are literal text no row contains.
	await field.fill('artist:echo');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await field.fill('title:harbor');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expectScope(page, kind, 'Both title and artist');
	await field.fill('harbor');
	await expectRows(page, ['scope-alpha', 'scope-beta']);
});

test('[FS-B02] narrowing: collisions, missing artists, id fallback, literals and accents split by field', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireScope(page);
	const field = await requireFind(page);

	await field.fill('harbor');
	await expectRows(page, ['scope-alpha', 'scope-beta']);
	await setScope(page, kind, 'Title only');
	await expectRows(page, ['scope-alpha']);
	await expect(field, 'scope change must not clear the query').toHaveValue('harbor');
	await setScope(page, kind, 'Artist only');
	await expectRows(page, ['scope-beta']);
	await expect(field).toHaveValue('harbor');
	await setScope(page, kind, 'Both title and artist');
	await expectRows(page, ['scope-alpha', 'scope-beta']);

	// 'echo' is an artist on two rows and a title on none.
	await field.fill('echo');
	await expectRows(page, ['scope-alpha', 'scope-gamma']);
	await setScope(page, kind, 'Title only');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await setScope(page, kind, 'Artist only');
	await expectRows(page, ['scope-alpha', 'scope-gamma']);

	// The untitled row is findable by its stable id in Title/Both only.
	await field.fill('scope-untitled');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await setScope(page, kind, 'Title only');
	await expectRows(page, ['scope-untitled']);
	await setScope(page, kind, 'Both title and artist');
	await expectRows(page, ['scope-untitled']);

	// A blank artist never matches Artist only.
	await field.fill('delta');
	await expectRows(page, ['scope-delta']);
	await setScope(page, kind, 'Artist only');
	await expect(page.getByTestId('library-item')).toHaveCount(0);

	// Metacharacters are literal per field.
	await field.fill('c++');
	await expectRows(page, ['scope-regex']);
	await setScope(page, kind, 'Title only');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await field.fill('.*');
	await expectRows(page, ['scope-regex']);
	await setScope(page, kind, 'Artist only');
	await expect(page.getByTestId('library-item')).toHaveCount(0);

	// Shipped folding, no accent stripping, per field.
	await field.fill('écho');
	await expectRows(page, ['scope-accent']);
	await setScope(page, kind, 'Title only');
	await field.fill('ÉCLAIR');
	await expectRows(page, ['scope-accent']);
	await field.fill('eclair');
	await expect(page.getByTestId('library-item')).toHaveCount(0);

	// An empty query shows every loaded row even under Artist only:
	// scope narrows matches, it is not a visibility filter.
	await setScope(page, kind, 'Artist only');
	await field.fill('');
	await expectRows(page, API_IDS);
});

test('[FS-B03] counts and zero-match copy name the scope; return to Both keeps the text', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireScope(page);
	const field = await requireFind(page);

	await field.fill('harbor');
	await setScope(page, kind, 'Title only');
	await expectRows(page, ['scope-alpha']);
	await expectFindCount(page, 1, ITEMS.length, 'title');
	await setScope(page, kind, 'Artist only');
	await expectRows(page, ['scope-beta']);
	await expectFindCount(page, 1, ITEMS.length, 'artist');

	await field.fill('echo');
	await expectRows(page, ['scope-alpha', 'scope-gamma']);
	await expectFindCount(page, 2, ITEMS.length, 'artist');
	await setScope(page, kind, 'Title only');
	await expectFindCount(page, 0, ITEMS.length, 'title');
	await expectScopedZeroMatch(page, 'title');

	await returnToBoth(page, kind);
	await expect(field, 'returning to Both must not lose the typed text').toHaveValue('echo');
	await expectRows(page, ['scope-alpha', 'scope-gamma']);
	await expectFindCount(page, 2, ITEMS.length);

	// And the mirror image: a title-only term under Artist only.
	await field.fill('gamma');
	await setScope(page, kind, 'Artist only');
	await expectFindCount(page, 0, ITEMS.length, 'artist');
	await expectScopedZeroMatch(page, 'artist');
	await returnToBoth(page, kind);
	await expect(field).toHaveValue('gamma');
	await expectRows(page, ['scope-gamma']);
});

test('[FS-B04] clear search clears only the text and retains the chosen scope', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireScope(page);
	const field = await requireFind(page);

	await setScope(page, kind, 'Title only');
	await field.fill('echo');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await clearSearch(page).click();
	await expect(field).toHaveValue('');
	await expectRows(page, API_IDS);
	await expectScope(page, kind, 'Title only');
	// Retention is behavioral: the same text is still scoped, untouched.
	await field.fill('echo');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await field.fill('harbor');
	await expectRows(page, ['scope-alpha']);
	await clearSearch(page).click();
	await expectRows(page, API_IDS);
	await expectScope(page, kind, 'Title only');
});

test('[FS-B05] composition: scope intersects Favorites only; view switches keep query and scope', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireScope(page);
	const field = await requireFind(page);

	await starButton(page, 'scope-alpha').click();
	await starButton(page, 'scope-beta').click();
	await expect.poll(() => favoriteIds(page)).toEqual(['scope-alpha', 'scope-beta']);

	await view(page).selectOption({ label: 'Favorites only' });
	await expectRows(page, ['scope-alpha', 'scope-beta']);
	await field.fill('harbor');
	await expectRows(page, ['scope-alpha', 'scope-beta']);
	await setScope(page, kind, 'Title only');
	await expectRows(page, ['scope-alpha']);
	await setScope(page, kind, 'Artist only');
	await expectRows(page, ['scope-beta']);
	expect(await favoriteIds(page), 'scope must not change the favorites shelf').toEqual([
		'scope-alpha',
		'scope-beta'
	]);
	await expect(starButton(page, 'scope-beta')).toHaveAttribute('aria-pressed', 'true');

	// 'echo' under Artist only matches one favorite and one non-favorite:
	// the view decides which half is visible, scope and query unchanged.
	await field.fill('echo');
	await expectRows(page, ['scope-alpha']);
	await view(page).selectOption({ label: 'All tracks' });
	await expect(field).toHaveValue('echo');
	await expectScope(page, kind, 'Artist only');
	await expectRows(page, ['scope-alpha', 'scope-gamma']);
	await view(page).selectOption({ label: 'Favorites only' });
	await expectRows(page, ['scope-alpha']);
	expect(await favoriteIds(page)).toEqual(['scope-alpha', 'scope-beta']);
});

test('[FS-B06] composition: the chosen ordering survives scope changes and vice versa', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireScope(page);
	const field = await requireFind(page);

	await ordering(page).selectOption({ label: 'Title A–Z' });
	await field.fill('song');
	// Title order over the matched subset: Delta, Harbor, Morning.
	const byTitle = ['scope-delta', 'scope-alpha', 'scope-beta'];
	await expectRows(page, byTitle);
	await setScope(page, kind, 'Title only');
	await expectRows(page, byTitle);
	await setScope(page, kind, 'Artist only');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await setScope(page, kind, 'Both title and artist');
	await expectRows(page, byTitle);
	await expect(ordering(page).locator('option:checked')).toHaveText('Title A–Z');

	// Changing the ordering must not reset the scope either.
	await setScope(page, kind, 'Title only');
	await ordering(page).selectOption({ label: 'Original order' });
	await expectRows(page, ['scope-alpha', 'scope-beta', 'scope-delta']);
	await field.fill('harbor');
	await expectRows(page, ['scope-alpha']);
	await expectScope(page, kind, 'Title only');
});

test('[FS-B07] composition: scope hides but never clears checkbox selections', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireScope(page);
	const field = await requireFind(page);

	const alphaChoice = page.getByRole('checkbox', { name: 'Select Harbor Song', exact: true });
	const betaChoice = page.getByRole('checkbox', { name: 'Select Morning Song', exact: true });
	await alphaChoice.check();
	await betaChoice.check();
	await expect(page.getByTestId('selection-count')).toHaveText('2 selected');
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);

	await field.fill('harbor');
	await expectRows(page, ['scope-alpha', 'scope-beta']);
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);

	await setScope(page, kind, 'Title only');
	await expectRows(page, ['scope-alpha']);
	await expect(alphaChoice).toBeChecked();
	await expect(betaChoice).toHaveCount(0);
	await expect(page.getByTestId('selection-count')).toHaveText('2 selected');
	await expect(page.getByTestId('selection-hidden-count')).toContainText(
		/1 selected.*hidden/i
	);

	await setScope(page, kind, 'Artist only');
	await expectRows(page, ['scope-beta']);
	await expect(betaChoice).toBeChecked();
	await expect(page.getByTestId('selection-count')).toHaveText('2 selected');
	await expect(page.getByTestId('selection-hidden-count')).toContainText(
		/1 selected.*hidden/i
	);

	await setScope(page, kind, 'Both title and artist');
	await expectRows(page, ['scope-alpha', 'scope-beta']);
	await expect(alphaChoice).toBeChecked();
	await expect(betaChoice).toBeChecked();
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);
	await expect(page.getByTestId('queue-item'), 'selections never queued anything').toHaveCount(0);
});

test('[FS-B08] presentation-only: scope changes touch no network, media, queue, favorites, selection or storage', async ({
	page
}) => {
	const mutations: string[] = [];
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('request', (request) => {
		if (request.method() !== 'GET') mutations.push(`${request.method()} ${request.url()}`);
	});
	await page.addInitScript(() => {
		const calls = { play: 0, pause: 0 };
		Object.assign(window, { __scopeMediaCalls: calls });
		for (const method of ['play', 'pause'] as const) {
			const original = HTMLMediaElement.prototype[method];
			Object.defineProperty(HTMLMediaElement.prototype, method, {
				configurable: true,
				value: function (this: HTMLMediaElement) {
					calls[method] += 1;
					return original.call(this);
				}
			});
		}
	});
	const api = await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireScope(page);
	const field = await requireFind(page);

	await row(page, 'scope-alpha').click();
	await row(page, 'scope-zulu').click();
	await page.getByTestId('player-next').click();
	await starButton(page, 'scope-beta').click();
	await page.getByRole('checkbox', { name: 'Select Morning Song', exact: true }).check();
	await expectQueue(page, ['scope-alpha', 'scope-zulu'], 'scope-zulu', 'Zulu Last');

	const queueBefore = await queueState(page);
	const storageBefore = await readLocalStorage(page);
	expect(await favoriteIds(page)).toEqual(['scope-beta']);

	await field.fill('harbor');
	await setScope(page, kind, 'Title only');
	await expectRows(page, ['scope-alpha']);
	await setScope(page, kind, 'Artist only');
	await expectRows(page, ['scope-beta']);
	await setScope(page, kind, 'Both title and artist');
	await expectRows(page, ['scope-alpha', 'scope-beta']);
	await field.fill('echo');
	await setScope(page, kind, 'Title only');
	await expect(page.getByTestId('library-item')).toHaveCount(0);

	expect(await queueState(page), 'queue/current/media must be untouched by scope').toEqual(
		queueBefore
	);
	expect(await readLocalStorage(page), 'scope must write no storage').toEqual(storageBefore);
	expect(await favoriteIds(page)).toEqual(['scope-beta']);
	await expect(page.getByTestId('selection-count')).toHaveText('1 selected');
	expect(api.requests(), 'scope changes must not fetch').toBe(1);
	expect(mutations, 'scope changes must not mutate over the network').toEqual([]);
	expect(errors, 'scope changes must not raise page errors').toEqual([]);
});

test('[FS-B09] keyboard: tab-reachable native selector with visible focus and no assertive chatter', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireScope(page);
	const field = await requireFind(page);
	await field.fill('harbor');
	await expectRows(page, ['scope-alpha', 'scope-beta']);

	const target = scopeFocusable(page, kind, 'Both title and artist');
	const tag = await target.evaluate((element) => element.tagName);
	expect(tag, 'scope selector must be a native form control').toMatch(/^(SELECT|INPUT|BUTTON)$/);

	for (
		let tabs = 0;
		tabs < 80 && !(await target.evaluate((element) => element === document.activeElement));
		tabs += 1
	) {
		await page.keyboard.press('Tab');
	}
	await expect(target, 'scope selector must be reachable by Tab').toBeFocused();
	const focusRing = await target.evaluate((element) => {
		const style = getComputedStyle(element);
		return {
			outline: style.outlineStyle !== 'none' && style.outlineWidth !== '0px',
			shadow: style.boxShadow !== 'none'
		};
	});
	expect(
		focusRing.outline || focusRing.shadow,
		'keyboard focus on the scope selector must be visible'
	).toBe(true);

	if (kind === 'select' || kind === 'radio') {
		await page.keyboard.press(kind === 'select' ? 'ArrowDown' : 'ArrowRight');
		await expect
			.poll(() => currentScope(page, kind), { message: 'arrow keys must change the scope' })
			.not.toBe('Both title and artist');
		const now = await currentScope(page, kind);
		await expectRows(page, now === 'Title only' ? ['scope-alpha'] : ['scope-beta']);
	} else {
		const artistButton = scopeButton(page, 'Artist only');
		await artistButton.focus();
		await expect(artistButton).toBeFocused();
		await page.keyboard.press('Enter');
		await expectScope(page, kind, 'Artist only');
		await expectRows(page, ['scope-beta']);
	}

	const activeIsBody = await page.evaluate(() => document.activeElement === document.body);
	expect(activeIsBody, 'operating the scope selector must not dump focus to <body>').toBe(false);
	await expect(
		page.locator('[aria-live="assertive"]:not(#svelte-announcer)'),
		'scope changes must not add assertive live regions'
	).toHaveCount(0);
	await expect(page.getByRole('alert'), 'scope changes must not raise alerts').toHaveCount(0);
});

test.describe('touch and narrow viewport', () => {
	test.use({ hasTouch: true });

	test('[FS-B10] 320px light/dark: selector stays visible and touch-operable with no horizontal scroll', async ({
		page
	}) => {
		await mockLibrary(page);
		await page.setViewportSize({ width: 320, height: 700 });
		await openLibrary(page);
		const kind = await requireScope(page);
		const field = await requireFind(page);
		await field.fill('harbor');
		await expectRows(page, ['scope-alpha', 'scope-beta']);

		if (kind === 'button') {
			await scopeButton(page, 'Artist only').tap();
			await expectScope(page, kind, 'Artist only');
			await expectRows(page, ['scope-beta']);
		} else if (kind === 'radio') {
			await scopeRadio(page, 'Artist only').tap();
			await expectScope(page, kind, 'Artist only');
			await expectRows(page, ['scope-beta']);
		} else {
			// A native select opens the platform picker on tap, which cannot
			// be driven from the page; pin a real touch target and drive the
			// same change event the picker would fire.
			const box = await scopeSelect(page).boundingBox();
			expect(box, 'scope select must have a touch target at 320px').not.toBeNull();
			expect((box as { height: number }).height).toBeGreaterThanOrEqual(24);
			expect((box as { width: number }).width).toBeGreaterThanOrEqual(24);
			await setScope(page, kind, 'Artist only');
			await expectRows(page, ['scope-beta']);
		}

		for (const scheme of ['dark', 'light'] as const) {
			await page.emulateMedia({ colorScheme: scheme });
			await expect(field, `find field must stay visible in ${scheme} mode`).toBeVisible();
			await expect(
				scopeFocusable(page, kind, (await currentScope(page, kind)) as ScopeLabel),
				`scope selector must stay visible in ${scheme} mode`
			).toBeVisible();
			await setScope(page, kind, 'Title only');
			await expectRows(page, ['scope-alpha']);
			await setScope(page, kind, 'Both title and artist');
			await expectRows(page, ['scope-alpha', 'scope-beta']);
			const overflow = await page.evaluate(
				() => document.documentElement.scrollWidth - document.documentElement.clientWidth
			);
			expect(overflow, `no horizontal scrolling at 320px in ${scheme} mode`).toBeLessThanOrEqual(1);
		}
	});
});

test('[FS-B11] reload: scope is visit-local, resets to Both, and wrote no storage', async ({
	page
}) => {
	await mockLibrary(page);
	await openLibrary(page);
	const kind = await requireScope(page);
	const storageBefore = await readLocalStorage(page);
	const field = await requireFind(page);
	await field.fill('harbor');
	await setScope(page, kind, 'Artist only');
	await expectRows(page, ['scope-beta']);
	expect(await readLocalStorage(page), 'choosing a scope must persist nothing').toEqual(
		storageBefore
	);

	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expectRows(page, API_IDS);
	const kindAfter = await requireScope(page);
	expect(kindAfter).toBe(kind);
	await expectScope(page, kindAfter, 'Both title and artist');
	const fresh = await requireFind(page);
	await expect(fresh).toHaveValue('');
	// Behavioral reset: the artist-side collision row is findable again
	// without anyone touching the selector.
	await fresh.fill('harbor');
	await expectRows(page, ['scope-alpha', 'scope-beta']);
});

test('[FS-B12] account teardown: the next account starts at Both with an empty query', async ({
	page
}) => {
	const reply: LibraryReply = { items: ITEMS };
	await mockLibrary(page, reply);
	await openLibrary(page);
	const kind = await requireScope(page);
	const field = await requireFind(page);
	await field.fill('harbor');
	await setScope(page, kind, 'Title only');
	await expectRows(page, ['scope-alpha']);

	await authTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-item')).toHaveCount(0);

	reply.items = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expectRows(page, ['next-scope', 'next-other']);
	const kindAfter = await requireScope(page);
	expect(kindAfter).toBe(kind);
	await expectScope(page, kindAfter, 'Both title and artist');
	const fresh = await requireFind(page);
	await expect(fresh, 'query must not cross the account boundary').toHaveValue('');
	// 'harbor' only matches the next account through its ARTIST field, so
	// leaked Title-only scope would show zero rows here.
	await fresh.fill('harbor');
	await expectRows(page, ['next-scope']);
});
