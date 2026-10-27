import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: range-select displayed library tracks with Shift+click.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * range-selection wiring on top of this branch and must make these tests
 * pass WITHOUT editing this file; a wrong test comes back to the acceptance
 * lane instead.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click/keyboard contract is pinned
 * here in the offline Playwright suite with a route-mocked `/api/library`.
 * Every test guards its first checkbox interaction with an explicit
 * visibility assertion, so a run against a build without the controls fails
 * as an assertion (missing control), never as a click error. Fully offline
 * and deterministic: no backend, no provider, no spend.
 *
 * Hooks (new component-owned literals, following the `selection-count` /
 * `append-result` / `queue-announcement` precedent):
 * - `selection-announcement` (`data-testid`, `role="status"`): the single
 *   polite live region for selection outcomes. After every gesture that
 *   changes the selection its text is exactly the selection-count text
 *   (for example `4 selected`); gestures that change nothing leave the text
 *   untouched. Exactly one such region exists; selection gestures never
 *   introduce an assertive (`role="alert"`) region.
 *
 * Contract under test (paired build):
 * - Shift+click on a row checkbox selects the contiguous run in DISPLAYED
 *   order (post find, order and view projections) from the anchor — the most
 *   recently activated checkbox — through the clicked row. Forward and
 *   backward ranges select the same run. The primitive is purely additive:
 *   Shift+click never deselects (Shift on the anchor row itself is a no-op).
 * - Missing anchor (after Clear selection, the auth/account purge, or a
 *   successful-snapshot prune that dropped the anchor) or an anchor hidden
 *   by find/view degrades the next Shift+click to a plain toggle of the
 *   clicked row — no phantom ranges. Plain click/checkbox behavior is
 *   otherwise unchanged (toggle exactly one ID).
 * - Rows hidden by find/view are never touched by a range. Shift+Space on a
 *   focused checkbox performs the same range as Shift+click on that row.
 * - Range application never queues, stars, plays, seeks, fetches,
 *   favorites-mutates or persists anything by itself: queue entries, order
 *   and current ID, favorites, the find query, the order control and the
 *   rendered row order are unchanged beyond the selection array, and the
 *   selection-count text stays truthful through the existing element.
 *
 * Case IDs (stable): RNG-B-00 control, RNG-B-01 forward range, RNG-B-02
 * backward range, RNG-B-03 displayed-not-API order, RNG-B-04 Shift+Space
 * parity, RNG-B-05 find-hidden untouched, RNG-B-06 hidden anchor toggles,
 * RNG-B-07 view-hidden untouched, RNG-B-08 clear resets anchor, RNG-B-09
 * auth purge resets anchor, RNG-B-10 anchor prune resets, RNG-B-11 plain
 * click unchanged, RNG-B-12 Shift on anchor is a no-op, RNG-B-13 range
 * purity, RNG-B-14 narrow both-theme. RNG-U-* live in the sibling
 * range-select unit acceptance file.
 *
 * Out of scope (not pinned): all-select controls, drag selection, queue/star
 * side effects of ranges, grid-role refactor, touch-specific gestures (the
 * native checkbox carries touch), server playlists, saved selections,
 * cross-device state, backend search, audio transport, deployed streaming.
 */

// Rendered order is API order; ids are deliberately not in sorted order.
// Titles/artists avoid "star", "favorite", "select", "append" and "clear" so
// name-filtered locators below can only match their intended control.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'row-bravo', title: 'Bravo Song', artist: 'Next Account' },
	{ id: 'row-kilo', title: 'Kilo Song', artist: 'Harbor Lights' },
	{ id: 'row-delta', title: 'Delta Song' }
];

const API_ORDER = ['row-zulu', 'row-alpha', 'row-mike', 'row-bravo', 'row-kilo', 'row-delta'];
const TITLE_ORDER = ['row-alpha', 'row-bravo', 'row-delta', 'row-kilo', 'row-mike', 'row-zulu'];

/** Route `/api/library` to a mutable payload so a test can swap it mid-run. */
async function routeLibrary(page: Page, payload: { current: unknown }): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(payload.current)
		})
	);
}

async function seedLibrary(page: Page, payload: { current: unknown } = { current: ITEMS }) {
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(
		(payload.current as unknown[]).length
	);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function checkbox(page: Page, id: string): Locator {
	return row(page, id).getByRole('checkbox');
}

function starButton(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /star|favorite/i });
}

function selectionCount(page: Page): Locator {
	return page.getByTestId('selection-count');
}

function clearButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Clear selection', exact: true });
}

function appendButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Append selected', exact: true });
}

/** Checked row IDs in rendered order (hidden rows are unmounted, never listed). */
async function checkedIds(page: Page): Promise<string[]> {
	return page.getByTestId('library-item').evaluateAll((items) =>
		items
			.filter((item) => {
				const box = item.querySelector('input[type="checkbox"]');
				return box instanceof HTMLInputElement && box.checked;
			})
			.map((item) => item.getAttribute('data-id') ?? '')
	);
}

async function renderedIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('library-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-id')));
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

/**
 * The single polite selection announcement carries exactly the expected
 * count text through a `role="status"` (polite) region.
 */
async function expectAnnouncement(page: Page, expected: string): Promise<void> {
	const region = page.getByTestId('selection-announcement');
	await expect(region, 'selection announcement missing').toHaveCount(1);
	await expect(region).toHaveAttribute('role', 'status');
	await expect(region).toHaveText(expected);
}

async function expectNoAssertiveFeedback(page: Page): Promise<void> {
	await expect(page.locator('main').getByRole('alert')).toHaveCount(0);
}

async function storageSnapshot(page: Page): Promise<string> {
	return page.evaluate(() => {
		const dump = (storage: Storage): [string, string | null][] => {
			const entries: [string, string | null][] = [];
			for (let i = 0; i < storage.length; i += 1) {
				const key = storage.key(i);
				if (key !== null) entries.push([key, storage.getItem(key)]);
			}
			entries.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
			return entries;
		};
		return JSON.stringify({
			session: dump(window.sessionStorage),
			local: dump(window.localStorage)
		});
	});
}

async function storageKeys(page: Page): Promise<string[]> {
	return page.evaluate(() => [
		...Array.from({ length: window.sessionStorage.length }, (_, i) =>
			window.sessionStorage.key(i)
		),
		...Array.from({ length: window.localStorage.length }, (_, i) => window.localStorage.key(i))
	].filter((key): key is string => key !== null));
}

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-range-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
}

test('[RNG-B-00] control: the library renders fixture rows in API order', async ({
	page
}) => {
	await seedLibrary(page);

	expect(await renderedIds(page)).toEqual(API_ORDER);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(selectionCount(page)).toHaveText('0 selected');
	await expect(appendButton(page)).toBeDisabled();
});

test('[RNG-B-01] Shift+click selects the forward run from the anchor in displayed order', async ({
	page
}) => {
	await seedLibrary(page);

	const anchor = checkbox(page, 'row-alpha');
	await expect(anchor, 'checkbox missing on row-alpha').toBeVisible();
	await anchor.click();
	await expect(selectionCount(page)).toHaveText('1 selected');

	await checkbox(page, 'row-kilo').click({ modifiers: ['Shift'] });

	expect(await checkedIds(page)).toEqual(['row-alpha', 'row-mike', 'row-bravo', 'row-kilo']);
	await expect(selectionCount(page)).toHaveText('4 selected');
	await expectAnnouncement(page, '4 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-02] Shift+click selects the same run backwards from the anchor', async ({
	page
}) => {
	await seedLibrary(page);

	const anchor = checkbox(page, 'row-kilo');
	await expect(anchor, 'checkbox missing on row-kilo').toBeVisible();
	await anchor.click();
	await expect(selectionCount(page)).toHaveText('1 selected');

	await checkbox(page, 'row-alpha').click({ modifiers: ['Shift'] });

	expect(await checkedIds(page)).toEqual(['row-alpha', 'row-mike', 'row-bravo', 'row-kilo']);
	await expect(selectionCount(page)).toHaveText('4 selected');
	await expectAnnouncement(page, '4 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-03] ranges follow displayed order, not API order, and survive reordering', async ({
	page
}) => {
	await seedLibrary(page);

	const order = page.getByLabel('Order tracks');
	await expect(order, 'library order control missing').toBeVisible();
	await order.selectOption('title');
	expect(await renderedIds(page)).toEqual(TITLE_ORDER);

	const anchor = checkbox(page, 'row-bravo');
	await expect(anchor, 'checkbox missing on row-bravo').toBeVisible();
	await anchor.click();
	await checkbox(page, 'row-mike').click({ modifiers: ['Shift'] });

	// Displayed run bravo..mike in Title A-Z order: bravo, delta, kilo, mike.
	expect(await checkedIds(page)).toEqual(['row-bravo', 'row-delta', 'row-kilo', 'row-mike']);
	await expect(selectionCount(page)).toHaveText('4 selected');
	await expectAnnouncement(page, '4 selected');

	// Reordering back keeps the same membership; only the rendered order moves.
	await order.selectOption('original');
	expect(await renderedIds(page)).toEqual(API_ORDER);
	expect(await checkedIds(page)).toEqual(['row-mike', 'row-bravo', 'row-kilo', 'row-delta']);
	await expect(selectionCount(page)).toHaveText('4 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-04] Shift+Space on a focused checkbox matches the Shift+click range', async ({
	page
}) => {
	await seedLibrary(page);

	const anchor = checkbox(page, 'row-alpha');
	await expect(anchor, 'checkbox missing on row-alpha').toBeVisible();
	await anchor.focus();
	await expect(anchor).toBeFocused();
	await page.keyboard.press('Space');
	await expect(anchor).toBeChecked();
	await expect(anchor, 'focus must stay on the toggled checkbox').toBeFocused();
	await expect(selectionCount(page)).toHaveText('1 selected');

	const focus = checkbox(page, 'row-kilo');
	await focus.focus();
	await expect(focus).toBeFocused();
	await page.keyboard.press('Shift+Space');

	expect(await checkedIds(page)).toEqual(['row-alpha', 'row-mike', 'row-bravo', 'row-kilo']);
	await expect(selectionCount(page)).toHaveText('4 selected');
	await expectAnnouncement(page, '4 selected');
	await expect(focus, 'focus must stay on the range checkbox').toBeFocused();

	// Plain Space still toggles exactly one ID after a range.
	await page.keyboard.press('Space');
	expect(await checkedIds(page)).toEqual(['row-alpha', 'row-mike', 'row-bravo']);
	await expect(selectionCount(page)).toHaveText('3 selected');
	await expectAnnouncement(page, '3 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-05] rows hidden by find are never touched by a range', async ({ page }) => {
	await seedLibrary(page);

	const anchor = checkbox(page, 'row-zulu');
	await expect(anchor, 'checkbox missing on row-zulu').toBeVisible();
	await anchor.click();

	const find = page.getByLabel('Find in loaded tracks');
	await expect(find, 'library find control missing').toBeVisible();
	await find.fill('Harbor');
	expect(await renderedIds(page)).toEqual(['row-zulu', 'row-mike', 'row-kilo']);

	await checkbox(page, 'row-kilo').click({ modifiers: ['Shift'] });

	expect(await checkedIds(page)).toEqual(['row-zulu', 'row-mike', 'row-kilo']);
	await expect(selectionCount(page)).toHaveText('3 selected');
	await expectAnnouncement(page, '3 selected');
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);

	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	expect(await renderedIds(page)).toEqual(API_ORDER);
	expect(await checkedIds(page)).toEqual(['row-zulu', 'row-mike', 'row-kilo']);
	await expect(selectionCount(page)).toHaveText('3 selected');
	await expectAnnouncement(page, '3 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-06] an anchor hidden by find degrades Shift+click to a plain toggle', async ({
	page
}) => {
	await seedLibrary(page);

	const anchor = checkbox(page, 'row-alpha');
	await expect(anchor, 'checkbox missing on row-alpha').toBeVisible();
	await anchor.click();

	const find = page.getByLabel('Find in loaded tracks');
	await find.fill('Harbor');
	expect(await renderedIds(page)).toEqual(['row-zulu', 'row-mike', 'row-kilo']);

	await checkbox(page, 'row-kilo').click({ modifiers: ['Shift'] });

	// No phantom range from the hidden anchor: only the clicked row toggles.
	expect(await checkedIds(page)).toEqual(['row-kilo']);
	await expect(selectionCount(page)).toHaveText('2 selected');
	await expectAnnouncement(page, '2 selected');
	const hidden = page.getByTestId('selection-hidden-count');
	await expect(hidden, 'hidden-selected count missing').toBeVisible();
	await expect(hidden).toContainText('1');
	await expect(hidden).toContainText('hidden by this find');

	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	expect(await checkedIds(page)).toEqual(['row-alpha', 'row-kilo']);
	await expect(selectionCount(page)).toHaveText('2 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-07] rows hidden by the view are never touched by a range', async ({ page }) => {
	await seedLibrary(page);

	const firstStar = starButton(page, 'row-alpha');
	await expect(firstStar, 'star toggle missing on row-alpha').toBeVisible();
	await firstStar.click();
	await starButton(page, 'row-bravo').click();
	await starButton(page, 'row-delta').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(3);

	const view = page.getByLabel('Show');
	await expect(view, 'library view control missing').toBeVisible();
	await view.selectOption('favorites');
	expect(await renderedIds(page)).toEqual(['row-alpha', 'row-bravo', 'row-delta']);

	const anchor = checkbox(page, 'row-alpha');
	await expect(anchor, 'checkbox missing on row-alpha').toBeVisible();
	await anchor.click();
	await checkbox(page, 'row-delta').click({ modifiers: ['Shift'] });

	expect(await checkedIds(page)).toEqual(['row-alpha', 'row-bravo', 'row-delta']);
	await expect(selectionCount(page)).toHaveText('3 selected');
	await expectAnnouncement(page, '3 selected');

	await view.selectOption('all');
	expect(await renderedIds(page)).toEqual(API_ORDER);
	expect(await checkedIds(page)).toEqual(['row-alpha', 'row-bravo', 'row-delta']);
	await expect(selectionCount(page)).toHaveText('3 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-08] Clear selection resets the anchor: the next Shift+click plain-toggles', async ({
	page
}) => {
	await seedLibrary(page);

	const anchor = checkbox(page, 'row-alpha');
	await expect(anchor, 'checkbox missing on row-alpha').toBeVisible();
	await anchor.click();
	await checkbox(page, 'row-bravo').click();
	await expect(selectionCount(page)).toHaveText('2 selected');

	await clearButton(page).click();
	await expect(selectionCount(page)).toHaveText('0 selected');
	await expect(appendButton(page)).toBeDisabled();

	await checkbox(page, 'row-kilo').click({ modifiers: ['Shift'] });

	// A surviving anchor would range bravo..kilo (2 rows); the reset toggles one.
	expect(await checkedIds(page)).toEqual(['row-kilo']);
	await expect(selectionCount(page)).toHaveText('1 selected');
	await expectAnnouncement(page, '1 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-09] an auth purge resets the anchor: the next Shift+click plain-toggles', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	const anchor = checkbox(page, 'row-alpha');
	await expect(anchor, 'checkbox missing on row-alpha').toBeVisible();
	await anchor.click();
	await checkbox(page, 'row-bravo').click();
	await expect(selectionCount(page)).toHaveText('2 selected');

	await fireAuthTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);

	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(selectionCount(page)).toHaveText('0 selected');

	await checkbox(page, 'row-kilo').click({ modifiers: ['Shift'] });

	expect(await checkedIds(page)).toEqual(['row-kilo']);
	await expect(selectionCount(page)).toHaveText('1 selected');
	await expectAnnouncement(page, '1 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-10] a prune that drops the anchor resets it: the next Shift+click plain-toggles', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	const anchor = checkbox(page, 'row-bravo');
	await expect(anchor, 'checkbox missing on row-bravo').toBeVisible();
	await anchor.click();
	await checkbox(page, 'row-mike').click();
	await expect(selectionCount(page)).toHaveText('2 selected');

	payload.current = ITEMS.filter((item) => item.id !== 'row-mike');
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length - 1);
	await expect(selectionCount(page)).toHaveText('1 selected');
	expect(await checkedIds(page)).toEqual(['row-bravo']);

	await checkbox(page, 'row-kilo').click({ modifiers: ['Shift'] });

	// The pruned anchor cannot seed a range: only the clicked row toggles.
	expect(await checkedIds(page)).toEqual(['row-bravo', 'row-kilo']);
	await expect(selectionCount(page)).toHaveText('2 selected');
	await expectAnnouncement(page, '2 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-11] plain clicks keep toggling exactly one ID with truthful feedback', async ({
	page
}) => {
	await seedLibrary(page);

	const alpha = checkbox(page, 'row-alpha');
	await expect(alpha, 'checkbox missing on row-alpha').toBeVisible();
	await alpha.click();
	expect(await checkedIds(page)).toEqual(['row-alpha']);
	await expect(selectionCount(page)).toHaveText('1 selected');
	await expectAnnouncement(page, '1 selected');

	await checkbox(page, 'row-bravo').click();
	expect(await checkedIds(page)).toEqual(['row-alpha', 'row-bravo']);
	await expect(selectionCount(page)).toHaveText('2 selected');
	await expectAnnouncement(page, '2 selected');

	await alpha.click();
	expect(await checkedIds(page)).toEqual(['row-bravo']);
	await expect(selectionCount(page)).toHaveText('1 selected');
	await expectAnnouncement(page, '1 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-12] Shift+click on the anchor row itself changes nothing', async ({
	page
}) => {
	await seedLibrary(page);

	const anchor = checkbox(page, 'row-mike');
	await expect(anchor, 'checkbox missing on row-mike').toBeVisible();
	await anchor.click();
	await expect(selectionCount(page)).toHaveText('1 selected');

	await anchor.click({ modifiers: ['Shift'] });

	// Additive ranges never toggle off: the single-row span is a no-op.
	expect(await checkedIds(page)).toEqual(['row-mike']);
	await expect(selectionCount(page)).toHaveText('1 selected');
	await expectAnnouncement(page, '1 selected');
	await expectNoAssertiveFeedback(page);
});

test('[RNG-B-13] range application queues, stars, fetches and persists nothing', async ({
	page
}) => {
	const requests: { method: string; url: string }[] = [];
	page.on('request', (request) => requests.push({ method: request.method(), url: request.url() }));
	await seedLibrary(page);

	// Pre-existing queue, favorite and ordering the range must leave alone.
	await row(page, 'row-zulu').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	const star = starButton(page, 'row-bravo');
	await expect(star, 'star toggle missing on row-bravo').toBeVisible();
	await star.click();
	await expect(star).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	const order = page.getByLabel('Order tracks');
	await order.selectOption('title');
	expect(await renderedIds(page)).toEqual(TITLE_ORDER);

	const beforeStorage = await storageSnapshot(page);
	const anchor = checkbox(page, 'row-alpha');
	await expect(anchor, 'checkbox missing on row-alpha').toBeVisible();
	await anchor.click();
	await checkbox(page, 'row-kilo').click({ modifiers: ['Shift'] });

	// Displayed run alpha..kilo in Title A-Z order: alpha, bravo, delta, kilo.
	expect(await checkedIds(page)).toEqual(['row-alpha', 'row-bravo', 'row-delta', 'row-kilo']);
	await expect(selectionCount(page)).toHaveText('4 selected');
	await expectAnnouncement(page, '4 selected');

	expect(await queueOrder(page)).toEqual(['row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	await expect(star).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(order).toHaveValue('title');
	await expect(page.getByLabel('Find in loaded tracks')).toHaveValue('');
	expect(await renderedIds(page)).toEqual(TITLE_ORDER);
	await expect(page.getByTestId('append-result')).toHaveCount(0);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expectNoAssertiveFeedback(page);

	const libraryGets = requests.filter(
		(request) => request.method === 'GET' && request.url.includes('/api/library')
	);
	expect(libraryGets, 'range selection must not refetch the catalog').toHaveLength(1);
	expect(
		requests.filter((request) => request.method !== 'GET'),
		'range selection must not mutate over the network'
	).toEqual([]);
	expect(await storageSnapshot(page), 'range selection must not persist').toBe(beforeStorage);
	expect(
		(await storageKeys(page)).filter((key) => key.toLowerCase().includes('select')),
		'selection must not persist under a selection key'
	).toEqual([]);
});

test('[RNG-B-14] narrow-screen light and dark keep range selection visible and operable', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await seedLibrary(page);

	for (const scheme of ['light', 'dark'] as const) {
		await page.emulateMedia({ colorScheme: scheme });

		const box = checkbox(page, 'row-mike');
		await expect(box, `checkbox missing on row-mike (${scheme})`).toBeVisible();
		await expect(selectionCount(page)).toBeVisible();
		await expect(clearButton(page)).toBeVisible();
		await expect(appendButton(page)).toBeVisible();

		const overflow = await page.evaluate(
			() => document.documentElement.scrollWidth - document.documentElement.clientWidth
		);
		expect(overflow, `horizontal overflow in ${scheme} mode`).toBeLessThanOrEqual(1);
	}

	// And the narrow layout still ranges in displayed order.
	const anchor = checkbox(page, 'row-mike');
	await anchor.click();
	await checkbox(page, 'row-delta').click({ modifiers: ['Shift'] });
	expect(await checkedIds(page)).toEqual(['row-mike', 'row-bravo', 'row-kilo', 'row-delta']);
	await expect(selectionCount(page)).toHaveText('4 selected');
	await expectAnnouncement(page, '4 selected');
	await expectNoAssertiveFeedback(page);
});
