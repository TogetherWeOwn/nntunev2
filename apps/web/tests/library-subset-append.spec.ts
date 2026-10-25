import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: append an explicitly selected subset of library tracks.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * subset-selection wiring on top of this branch and must make these tests
 * pass WITHOUT editing this file; a wrong test comes back to the acceptance
 * lane instead.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click-driven contract is pinned here
 * in the offline Playwright suite with a route-mocked `/api/library`. Every
 * test guards its first new-control interaction with an explicit visibility
 * assertion, so a run against a build without the controls fails as an
 * assertion (missing control), never as a click error. Fully offline and
 * deterministic: no backend, no provider, no spend.
 *
 * Hooks (new component-owned literals, not TESTIDS entries — the same rule
 * as `queue-item` / `now-playing`, which live in a component):
 * - One native `checkbox` per loaded row, scoped inside
 *   `[data-testid="library-item"][data-id="<id>"]`, with a per-track
 *   accessible name matching `/select/i` plus the visible title (for
 *   example `Select Zulu Song`). Native keyboard/touch behavior.
 * - `selection-count` (`data-testid`) showing `<n> selected` (for example
 *   `2 selected`).
 * - `Clear selection` and `Append selected` buttons (exact accessible
 *   names), the latter disabled while nothing is selected.
 * - `append-result` (`data-testid`, `role="status"`) reporting the last
 *   append as `Added <a>` plus `<s> already queued` when duplicates were
 *   skipped.
 * - `selection-hidden-count` (`data-testid`) only when a filter exists and
 *   hides selected rows.
 * There is deliberately NO `Select all` / `Select displayed` control and no
 * new route: each checkbox is an explicit user choice.
 *
 * Contract under test (paired build):
 * - Checking never queues, stars, plays, seeks, fetches, favorites-mutates,
 *   persists or crosses accounts; checkbox activation never bubbles into the
 *   row-add or star actions. Immediate row-add and star keep working.
 * - Selection is a page-local ordered set in activation order; deselect
 *   removes, reselect appends at the end. Sort changes never reorder or drop
 *   survivors; a filter discloses its hidden-selected count.
 * - Successful refresh prunes IDs absent from the snapshot; transient
 *   failure keeps every choice. Auth transitions and 401/403/404 purge the
 *   selection; tabs never share it.
 * - Append adds current loaded metadata in activation order through the
 *   existing dedupe, reports added vs already queued, preserves every queue
 *   entry/order/current ID and playback intent, stays idempotent on repeat,
 *   and keeps the selection until explicit clear/deselect or legitimate prune.
 * - An empty queue may select its first added ID without autoplaying; no
 *   audible library stream is invented (`audio-player` stays absent).
 *
 * Case IDs (stable): SUB-B-00 control, SUB-B-01 render, SUB-B-02 check
 * purity, SUB-B-03 no bubble, SUB-B-04 keyboard, SUB-B-05 append around
 * duplicates, SUB-B-06 repeat idempotent, SUB-B-07 clear pending only,
 * SUB-B-08 reselect order, SUB-B-09 sort stability, SUB-B-10 hidden count,
 * SUB-B-11 prune on removal, SUB-B-12 transient keeps, SUB-B-13 auth purge,
 * SUB-B-14 401/403/404 purge, SUB-B-15 cross-tab isolation, SUB-B-16 zero
 * network mutation, SUB-B-17 no persistence, SUB-B-18 narrow both-theme,
 * SUB-B-19 failed-refresh UX, SUB-B-20 row/star preserved, SUB-B-21 append
 * enablement, SUB-B-22 per-track names. SUB-U-* live in the sibling
 * selection-subset unit acceptance file.
 *
 * Out of scope (not pinned): Select all/displayed, Add displayed,
 * queue removal/multi-select, server playlists, saved selections,
 * cross-device state, backend search, audio transport, deployed streaming.
 */

// Rendered order is API order; ids are deliberately not in sorted order.
// Titles/artists avoid "star", "favorite", "select", "append" and "clear" so
// name-filtered locators below can only match their intended control.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'row-bravo', title: 'Bravo Song', artist: 'Next Account' }
];

const NEXT_ACCOUNT = [{ id: 'row-kilo', title: 'Kilo Song', artist: 'Next Account' }];

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

function appendButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Append selected', exact: true });
}

function clearButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Clear selection', exact: true });
}

function queueItem(page: Page, id: string): Locator {
	return page.locator(
		`[data-testid="queue-item"][data-track-id="${id}"], [data-testid="queue-item"][data-id="${id}"]`
	);
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

async function ariaCurrent(locator: Locator): Promise<string | null> {
	return locator.evaluate((element) => element.getAttribute('aria-current'));
}

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-subset-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
}

test('[SUB-B-00] control: the library renders fixture rows in API order', async ({
	page
}) => {
	await seedLibrary(page);

	const rendered = await page
		.getByTestId('library-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-id')));
	expect(rendered).toEqual(['row-zulu', 'row-alpha', 'row-mike', 'row-bravo']);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[SUB-B-01] each row carries a labeled checkbox with count and actions, append disabled while empty', async ({
	page
}) => {
	await seedLibrary(page);

	for (const item of ITEMS) {
		const box = checkbox(page, item.id);
		await expect(box, `checkbox missing on row ${item.id}`).toBeVisible();
		await expect(box).toHaveAttribute('type', 'checkbox');
		await expect(box).not.toBeChecked();
		await expect(box).toHaveAccessibleName(new RegExp(`select.*${item.title}`, 'i'));
	}

	await expect(selectionCount(page), 'selection count missing').toBeVisible();
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(clearButton(page), 'Clear selection missing').toBeVisible();
	await expect(appendButton(page), 'Append selected missing').toBeVisible();
	await expect(appendButton(page)).toBeDisabled();

	// No bulk-select escape hatch and no new route for this slice.
	await expect(page.getByRole('button', { name: /select all|select displayed/i })).toHaveCount(0);
	await expect(
		page.getByRole('checkbox', { name: /select all|select displayed/i })
	).toHaveCount(0);
	await expect(page.getByRole('link', { name: /select all|select displayed/i })).toHaveCount(0);
	expect(page.url()).toContain('/library');
});

test('[SUB-B-02] checking boxes queues, stars, fetches and plays nothing', async ({
	page
}) => {
	const requests: { method: string; url: string }[] = [];
	page.on('request', (request) => requests.push({ method: request.method(), url: request.url() }));
	await seedLibrary(page);

	const first = checkbox(page, 'row-mike');
	await expect(first, 'checkbox missing on row-mike').toBeVisible();
	await first.check();
	const second = checkbox(page, 'row-zulu');
	await second.check();

	await expect(selectionCount(page)).toContainText('2 selected');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	for (const item of ITEMS) {
		await expect(starButton(page, item.id)).toHaveAttribute('aria-pressed', 'false');
	}
	// No row shows the queued badge: checking is pending, not queued.
	await expect(page.getByTestId('library-queued-badge')).toHaveCount(0);

	const libraryGets = requests.filter(
		(request) => request.method === 'GET' && request.url.includes('/api/library')
	);
	expect(libraryGets, 'checking must not refetch the catalog').toHaveLength(1);
	expect(
		requests.filter((request) => request.method !== 'GET'),
		'checking must not mutate over the network'
	).toEqual([]);
});

test('[SUB-B-03] checkbox activation never bubbles into row-add or star', async ({ page }) => {
	await seedLibrary(page);

	const box = checkbox(page, 'row-alpha');
	await expect(box, 'checkbox missing on row-alpha').toBeVisible();
	// Click the checkbox input itself (not the label): the row button and the
	// star toggle must both stay untouched.
	await box.click();

	await expect(box).toBeChecked();
	await expect(selectionCount(page)).toContainText('1 selected');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(starButton(page, 'row-alpha')).toHaveAttribute('aria-pressed', 'false');
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[SUB-B-04] checkboxes are keyboard operable with stable focus', async ({ page }) => {
	await seedLibrary(page);

	const box = checkbox(page, 'row-mike');
	await expect(box, 'checkbox missing on row-mike').toBeVisible();
	await box.focus();
	await expect(box).toBeFocused();

	await page.keyboard.press('Space');
	await expect(box).toBeChecked();
	await expect(box, 'focus must stay on the toggled checkbox').toBeFocused();
	await expect(selectionCount(page)).toContainText('1 selected');

	await page.keyboard.press('Space');
	await expect(box).not.toBeChecked();
	await expect(box).toBeFocused();
	await expect(selectionCount(page)).toContainText('0 selected');

	// Tab reaches another control: nothing is tabindex-removed. Native tab
	// order interleaves checkbox, row button and star per row, so the next
	// stop after a checkbox is the row BUTTON; this pins reachability, not
	// sequence (same shape as library-queue-manage.spec.ts).
	await page.keyboard.press('Tab');
	const focused = await page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		return {
			tag: el?.tagName ?? '',
			name: el?.getAttribute('aria-label') ?? el?.textContent ?? ''
		};
	});
	expect(focused.tag).toBe('BUTTON');
	expect(focused.name.trim().length).toBeGreaterThan(0);
});

test('[SUB-B-05] append adds the subset around queued duplicates in activation order', async ({
	page
}) => {
	await seedLibrary(page);

	// Pre-queue one row through the immediate gesture: current is zulu.
	await row(page, 'row-zulu').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	// Activation order runs against rendered order: mike (3rd), zulu (1st,
	// already queued), alpha (2nd).
	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await checkbox(page, 'row-zulu').check();
	await checkbox(page, 'row-alpha').check();
	await expect(selectionCount(page)).toContainText('3 selected');

	const append = appendButton(page);
	await expect(append).toBeEnabled();
	await append.click();

	// Existing entry first, then the two new IDs in activation order; the
	// duplicate never appears twice and the current track never moves.
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['row-zulu', 'row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBe('true');

	const result = page.getByTestId('append-result');
	await expect(result, 'append result missing').toBeVisible();
	await expect(result).toHaveAttribute('role', 'status');
	await expect(result).toContainText('Added 2');
	await expect(result).toContainText('1 already queued');

	// Selection survives the append; nothing autoplayed.
	await expect(selectionCount(page)).toContainText('3 selected');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[SUB-B-06] appending twice is idempotent and keeps the selection', async ({ page }) => {
	await seedLibrary(page);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await checkbox(page, 'row-alpha').check();

	const append = appendButton(page);
	await append.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	await append.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	const result = page.getByTestId('append-result');
	await expect(result).toContainText('Added 0');
	await expect(result).toContainText('2 already queued');
	await expect(selectionCount(page)).toContainText('2 selected');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[SUB-B-07] clear empties the pending selection only', async ({ page }) => {
	await seedLibrary(page);

	await row(page, 'row-zulu').click();
	await starButton(page, 'row-bravo').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await checkbox(page, 'row-alpha').check();
	await expect(selectionCount(page)).toContainText('2 selected');

	const clear = clearButton(page);
	await clear.click();

	await expect(selectionCount(page)).toContainText('0 selected');
	for (const item of ITEMS) {
		await expect(checkbox(page, item.id)).not.toBeChecked();
	}
	await expect(appendButton(page)).toBeDisabled();
	// Queue and favorites are untouched by the pending-only clear.
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await queueOrder(page)).toEqual(['row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[SUB-B-08] deselect then reselect moves the ID to the end of the activation order', async ({
	page
}) => {
	await seedLibrary(page);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await checkbox(page, 'row-zulu').check();
	await checkbox(page, 'row-alpha').check();
	await checkbox(page, 'row-zulu').uncheck();
	await expect(selectionCount(page)).toContainText('2 selected');
	await checkbox(page, 'row-zulu').check();
	await expect(selectionCount(page)).toContainText('3 selected');

	await appendButton(page).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});

test('[SUB-B-09] later sort changes keep every choice in activation order', async ({ page }) => {
	await seedLibrary(page);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await checkbox(page, 'row-zulu').check();
	await expect(selectionCount(page)).toContainText('2 selected');

	const order = page.getByLabel('Order tracks');
	await expect(order, 'library order control missing').toBeVisible();
	await order.selectOption('title');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(checkbox(page, 'row-mike')).toBeChecked();
	await expect(checkbox(page, 'row-zulu')).toBeChecked();
	await expect(selectionCount(page)).toContainText('2 selected');

	await order.selectOption('artist');
	await expect(checkbox(page, 'row-mike')).toBeChecked();
	await expect(checkbox(page, 'row-zulu')).toBeChecked();

	// Append follows activation order (mike, zulu), not either sorted order.
	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
});

test('[SUB-B-10] a filter discloses its hidden-selected count; without one nothing hides', async ({
	page
}) => {
	await seedLibrary(page);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await checkbox(page, 'row-zulu').check();

	// The current page ships sort only; this branch activates if (and only
	// if) a library filter control lands independently. The queue filter
	// (#queue-filter-input, "Filter queue") is a searchbox too, so it is
	// excluded: filling it cannot hide library rows.
	const candidates = page
		.getByRole('searchbox')
		.or(page.getByLabel(/filter|search/i))
		.and(page.locator(':not(#queue-filter-input)'));
	if ((await candidates.count()) > 0) {
		const filter = candidates.first();
		await filter.fill('mike');
		await expect(row(page, 'row-mike')).toBeVisible();
		await expect(row(page, 'row-zulu')).toHaveCount(0);
		const hidden = page.getByTestId('selection-hidden-count');
		await expect(hidden, 'hidden-selected count missing').toBeVisible();
		await expect(hidden).toContainText('1');
		await expect(selectionCount(page)).toContainText('2 selected');
		await filter.fill('');
		await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
		await expect(checkbox(page, 'row-zulu')).toBeChecked();
	} else {
		// Sort-only page: every choice stays visible, so no hidden disclosure.
		await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);
		await expect(selectionCount(page)).toContainText('2 selected');
	}
});

test('[SUB-B-11] a successful refresh prunes IDs the snapshot no longer carries', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await checkbox(page, 'row-zulu').check();
	await checkbox(page, 'row-alpha').check();
	await expect(selectionCount(page)).toContainText('3 selected');

	payload.current = ITEMS.filter((item) => item.id !== 'row-zulu');
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length - 1);

	await expect(selectionCount(page)).toContainText('2 selected');
	await expect(checkbox(page, 'row-mike')).toBeChecked();
	await expect(checkbox(page, 'row-alpha')).toBeChecked();
	await expect(row(page, 'row-zulu')).toHaveCount(0);

	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
});

test('[SUB-B-12] a transient refresh failure keeps every pending choice', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await checkbox(page, 'row-zulu').check();

	await page.unroute('**/api/library');
	await page.route('**/api/library', (route) => route.abort('failed'));
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();

	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(checkbox(page, 'row-mike')).toBeChecked();
	await expect(checkbox(page, 'row-zulu')).toBeChecked();
	await expect(selectionCount(page)).toContainText('2 selected');
});

test('[SUB-B-13] an auth transition purges the pending selection', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await checkbox(page, 'row-zulu').check();
	await expect(selectionCount(page)).toContainText('2 selected');

	await fireAuthTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);

	payload.current = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(NEXT_ACCOUNT.length);
	await expect(page.getByTestId('library-list')).toContainText('Kilo Song');
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(checkbox(page, 'row-kilo')).not.toBeChecked();
	await expect(appendButton(page)).toBeDisabled();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

for (const status of [401, 403, 404] as const) {
	test(`[SUB-B-14:${status}] a ${status} boundary purges the pending selection`, async ({
		page
	}) => {
		const payload: { current: unknown } = { current: ITEMS };
		await seedLibrary(page, payload);

		const mike = checkbox(page, 'row-mike');
		await expect(mike, 'checkbox missing on row-mike').toBeVisible();
		await mike.check();
		await checkbox(page, 'row-zulu').check();
		await expect(selectionCount(page)).toContainText('2 selected');

		await page.unroute('**/api/library');
		await page.route('**/api/library', (route) =>
			route.fulfill({
				status,
				contentType: 'application/json',
				body: JSON.stringify({ error: 'boundary' })
			})
		);
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		if (status === 404) {
			await expect(page.getByTestId('library-list')).toHaveCount(0);
		} else {
			await expect(page.getByRole('alert')).toBeVisible();
		}

		await page.unroute('**/api/library');
		payload.current = NEXT_ACCOUNT;
		await routeLibrary(page, payload);
		// The 404 landing is the neutral libraryUnavailable branch whose
		// recovery control is labeled Refresh (staleAt null); 401/403 land on
		// the error branch with Retry.
		if (status === 404) {
			await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		} else {
			await page.getByRole('button', { name: 'Retry' }).click();
		}
		await expect(page.getByTestId('library-item')).toHaveCount(NEXT_ACCOUNT.length);
		await expect(selectionCount(page)).toContainText('0 selected');
		await expect(appendButton(page)).toBeDisabled();
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
	});
}

test('[SUB-B-15] tabs never share a pending selection', async ({ page }) => {
	await seedLibrary(page);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await checkbox(page, 'row-zulu').check();
	await expect(selectionCount(page)).toContainText('2 selected');

	const otherTab = await page.context().newPage();
	await otherTab.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		})
	);
	await otherTab.goto('/library');
	await expect(otherTab.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(otherTab.getByTestId('selection-count')).toContainText('0 selected');
	for (const item of ITEMS) {
		await expect(checkbox(otherTab, item.id)).not.toBeChecked();
	}
	await otherTab.close();

	// The first tab still holds its own choices.
	await expect(selectionCount(page)).toContainText('2 selected');
});

test('[SUB-B-16] checking, clearing and appending mutate nothing over the network', async ({
	page
}) => {
	const requests: { method: string; url: string }[] = [];
	page.on('request', (request) => requests.push({ method: request.method(), url: request.url() }));
	await seedLibrary(page);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await expect(mike).toBeChecked();
	await checkbox(page, 'row-zulu').check();

	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);

	await clearButton(page).click();
	await expect(selectionCount(page)).toContainText('0 selected');

	// Non-vacuous gate: the queue proves the append under observation ran.
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu']);

	const libraryGets = requests.filter(
		(request) => request.method === 'GET' && request.url.includes('/api/library')
	);
	expect(libraryGets, 'selection flows must not refetch the catalog').toHaveLength(1);
	expect(
		requests.filter((request) => request.method !== 'GET'),
		'selection flows must not mutate over the network'
	).toEqual([]);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[SUB-B-17] the selection is page-local: reload drops it and stores nothing', async ({
	page
}) => {
	await seedLibrary(page);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await checkbox(page, 'row-zulu').check();
	await expect(selectionCount(page)).toContainText('2 selected');

	const storedKeys = await page.evaluate(() => [
		...Array.from({ length: window.sessionStorage.length }, (_, i) =>
			window.sessionStorage.key(i)
		),
		...Array.from({ length: window.localStorage.length }, (_, i) =>
			window.localStorage.key(i)
		)
	]);
	expect(
		(storedKeys.filter(Boolean) as string[]).filter((key) => key.toLowerCase().includes('select')),
		'selection must not persist under a selection key'
	).toEqual([]);

	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(selectionCount(page)).toContainText('0 selected');
	for (const item of ITEMS) {
		await expect(checkbox(page, item.id)).not.toBeChecked();
	}
	await expect(appendButton(page)).toBeDisabled();
});

test('[SUB-B-18] narrow-screen light and dark keep every control visible and operable', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await seedLibrary(page);

	for (const scheme of ['light', 'dark'] as const) {
		await page.emulateMedia({ colorScheme: scheme });

		const mike = checkbox(page, 'row-mike');
		await expect(mike, `checkbox missing on row-mike (${scheme})`).toBeVisible();
		await expect(selectionCount(page)).toBeVisible();
		await expect(clearButton(page)).toBeVisible();
		await expect(appendButton(page)).toBeVisible();

		const overflow = await page.evaluate(
			() => document.documentElement.scrollWidth - document.documentElement.clientWidth
		);
		expect(overflow, `horizontal overflow in ${scheme} mode`).toBeLessThanOrEqual(1);
	}

	// And the narrow layout still appends in activation order.
	await checkbox(page, 'row-alpha').check();
	await checkbox(page, 'row-mike').check();
	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-alpha', 'row-mike']);
});

test('[SUB-B-19] a failed refresh keeps selection controls operable with polite feedback', async ({
	page
}) => {
	await seedLibrary(page);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();

	await page.unroute('**/api/library');
	await page.route('**/api/library', (route) => route.abort('failed'));
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();

	// Controls stay usable on the stale list; feedback never shouts.
	await checkbox(page, 'row-alpha').check();
	await expect(selectionCount(page)).toContainText('2 selected');
	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	const result = page.getByTestId('append-result');
	await expect(result).toBeVisible();
	await expect(result).toHaveAttribute('role', 'status');
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[SUB-B-20] immediate row-add and star keep working alongside selection', async ({
	page
}) => {
	await seedLibrary(page);

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();

	// Immediate queue gesture-order still works and ignores the pending set.
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-zulu', 'row-alpha']);
	await expect(selectionCount(page)).toContainText('0 selected');

	// Starring still works and never selects.
	await starButton(page, 'row-mike').click();
	await expect(starButton(page, 'row-mike')).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(checkbox(page, 'row-mike')).not.toBeChecked();
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
});

test('[SUB-B-21] append enables on first check and disables when the set empties', async ({
	page
}) => {
	await seedLibrary(page);

	const append = appendButton(page);
	await expect(append, 'Append selected missing').toBeVisible();
	await expect(append).toBeDisabled();

	const mike = checkbox(page, 'row-mike');
	await expect(mike, 'checkbox missing on row-mike').toBeVisible();
	await mike.check();
	await expect(append).toBeEnabled();

	await mike.uncheck();
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(append).toBeDisabled();
});

test('[SUB-B-22] checkbox names name their own track and match nothing else', async ({
	page
}) => {
	await seedLibrary(page);

	for (const item of ITEMS) {
		const box = checkbox(page, item.id);
		await expect(box, `checkbox missing on row ${item.id}`).toBeVisible();
		const name = await box.getAttribute('aria-label');
		expect(name, `checkbox on ${item.id} must expose its own label`).not.toBeNull();
		expect(name!).toMatch(/select/i);
		expect(name!).toContain(item.title);
		// The checkbox name is its own control: it matches exactly one
		// checkbox and never the row button or the star toggle.
		await expect(
			row(page, item.id).getByRole('checkbox', { name: name! })
		).toHaveCount(1);
		const rowButton = row(page, item.id).locator('button.nt-library-row');
		await expect(rowButton).not.toHaveAccessibleName(name!);
		await expect(starButton(page, item.id)).not.toHaveAccessibleName(name!);
	}

	// Distinct tracks stay distinguishable: no two checkboxes share a name.
	const names = await page
		.getByTestId('library-item')
		.getByRole('checkbox')
		.evaluateAll((boxes) => boxes.map((box) => box.getAttribute('aria-label')));
	expect(new Set(names).size).toBe(names.length);
});
