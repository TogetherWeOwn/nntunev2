import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: select all displayed library tracks in one action.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * Select all action on top of this branch and must make these tests pass
 * WITHOUT editing this file; a wrong test comes back to the acceptance
 * lane instead. The pure helper half is
 * `src/lib/library/select-all-displayed-acceptance.test.ts`.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click/keyboard contract is pinned
 * here in the offline Playwright suite with a route-mocked `/api/library`.
 * Every test guards its first Select all interaction with
 * `requireSelectAll`, whose count assertion fails as an assertion (missing
 * control) against a build without the action, never as a click timeout.
 * Fully offline and deterministic: no backend, no provider, no spend.
 *
 * Locked surface (every other hook already ships):
 * - ONE button with the exact accessible name `Select all`, inside `main`,
 *   offered while library rows are displayed. It consumes exactly the
 *   currently displayed set — the loaded snapshot as reshaped by the
 *   existing Show view, ordering control, find field and Search in scope —
 *   never the whole loaded snapshot and never anything hidden.
 * - The action is additive over the existing page-local selection (pure
 *   half: `selectAllDisplayed(selection, displayedIds)` in
 *   `src/lib/library/selection.ts`): pre-existing selections keep their
 *   activation positions, hidden selections survive, newly added IDs
 *   append in displayed order, and per-row checkboxes keep their shipped
 *   semantics (deselect-a-few after a select-all just works). Append
 *   selected therefore queues a select-all in displayed order.
 * - Truthful readouts from the existing hooks: `selection-count` reports
 *   `<s> selected`, `selection-hidden-count` discloses selected rows the
 *   current find/view hides, `library-find-count` still reports
 *   displayed-of-loaded. `Clear selection` keeps emptying everything.
 * - The control is a native button, keyboard-reachable by Tab, at least
 *   24x24 CSS px, and stays enabled while rows are displayed (activating
 *   it with everything already selected is a harmless no-op, mirroring the
 *   always-enabled Clear selection button); activation never drops
 *   keyboard focus to the body. Feedback stays polite: no assertive live
 *   region and no alert inside `main`.
 * - Pruning and purity keep their shipped behavior: a successful refresh
 *   still prunes vanished IDs and select-all over the surviving displayed
 *   set never resurrects them; selecting never queues, stars, plays,
 *   fetches or writes anything.
 *
 * This control deliberately supersedes the "no Select all affordance"
 * absence pins the earlier subset-selection and selected-only banks
 * carried for their slices; those assertions were retired in the same
 * acceptance change that added this file, and this file is now the only
 * pin on the affordance.
 *
 * Case IDs (stable): SA-B-00 control, SA-B-01 select-all then
 * deselect-a-few, SA-B-02 clear empties and select-all works again,
 * SA-B-03 find subset plus hidden preservation, SA-B-04 search-scope
 * switch follows the newly displayed set, SA-B-05 ordering switch append
 * order, SA-B-06 prune never resurrects, SA-B-07 keyboard reachability,
 * target size and polite announcements, SA-B-08 presentation-only side
 * effects, SA-B-09 Selected-only review composition. SA-U-* live in the
 * pure sibling file.
 *
 * Out of scope (not pinned): range selection, invert selection,
 * selection persistence, queue-side selection, selected export,
 * keyboard shortcuts, deployed streaming. Queue evidence is state in
 * the mocked page, not a claim of audible playback.
 *
 * Focused command (repo root, deterministic, offline):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-select-all-displayed.spec.ts --workers=1 --retries=0
 */

// Loaded (API) order is neither title order nor any activation order used
// below. "Delta Harbor Song" carries the find term in its title while the
// two Harbor Lights rows carry it in the artist, so the three search
// scopes display three different sets for one query. Titles/artists avoid
// control vocabulary so name locators stay exact.
const ITEMS = [
	{ id: 'sa-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'sa-alpha', title: 'Alpha Song', artist: 'Solo Act' },
	{ id: 'sa-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'sa-bravo', title: 'Bravo Song', artist: 'Solo Act' },
	{ id: 'sa-delta', title: 'Delta Harbor Song', artist: 'Echo Band' }
];
const ALL_IDS = ITEMS.map((item) => item.id);

type Harness = {
	requests: string[];
	writes: string[];
};

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}
function checkbox(page: Page, id: string): Locator {
	return row(page, id).getByRole('checkbox');
}
function starButton(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /star|favorite/i });
}
function selectAllButton(page: Page): Locator {
	return page.locator('main').getByRole('button', { name: 'Select all', exact: true });
}
function clearButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Clear selection', exact: true });
}
function appendButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Append selected', exact: true });
}
function selectionCount(page: Page): Locator {
	return page.getByTestId('selection-count');
}
function findField(page: Page): Locator {
	return page.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
}
function scopeControl(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Search in', exact: true });
}
function orderControl(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Order tracks', exact: true });
}
function showControl(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Show', exact: true });
}

const MISSING =
	'missing behavior: one "Select all" button for the displayed tracks in ' +
	'apps/web/src/routes/library/+page.svelte — contract header of ' +
	'apps/web/tests/library-select-all-displayed.spec.ts';

async function requireSelectAll(page: Page): Promise<Locator> {
	const button = selectAllButton(page);
	await expect(button, MISSING).toHaveCount(1);
	await expect(button, 'Select all must be visible while rows are displayed').toBeVisible();
	await expect(button, 'Select all must be enabled while rows are displayed').toBeEnabled();
	return button;
}

async function openLibrary(
	page: Page,
	payload: { current: unknown } = { current: ITEMS }
): Promise<Harness> {
	const requests: string[] = [];
	const writes: string[] = [];
	page.on('request', (request) => {
		const url = new URL(request.url());
		if (url.pathname === '/api/library') requests.push(`${request.method()} ${url.pathname}`);
		if (request.method() !== 'GET' && request.method() !== 'HEAD') {
			writes.push(`${request.method()} ${url.pathname}`);
		}
	});
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(payload.current)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(
		(payload.current as unknown[]).length
	);
	return { requests, writes };
}

async function displayedIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('library-item')
		.evaluateAll((rows) => rows.map((item) => item.getAttribute('data-id')));
}
async function expectDisplayed(page: Page, want: string[]) {
	await expect.poll(() => displayedIds(page)).toEqual(want);
}
async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((rows) =>
			rows.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}
async function expectChecked(page: Page, checked: string[], unchecked: string[]) {
	for (const id of checked) {
		await expect(checkbox(page, id), `${id} must be selected`).toBeChecked();
	}
	for (const id of unchecked) {
		await expect(checkbox(page, id), `${id} must stay unselected`).not.toBeChecked();
	}
}
async function expectFindCount(page: Page, displayed: number, loaded = ITEMS.length) {
	await expect(
		page.getByTestId('library-find-count'),
		'displayed/loaded count must stay truthful'
	).toContainText(new RegExp(`Showing ${displayed} of ${loaded} tracks?`));
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

test('[SA-B-00] control: fixtures, existing controls and individual selection already work', async ({
	page
}) => {
	const h = await openLibrary(page);
	expect(await displayedIds(page)).toEqual(ALL_IDS);
	await expect(findField(page)).toBeVisible();
	await expect(scopeControl(page)).toBeVisible();
	await expect(orderControl(page)).toBeVisible();
	await expect(showControl(page)).toBeVisible();
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(clearButton(page)).toBeVisible();
	await expect(appendButton(page)).toBeDisabled();

	const mike = checkbox(page, 'sa-mike');
	await expect(mike, 'checkbox missing on row sa-mike').toBeVisible();
	await mike.check();
	await expect(selectionCount(page)).toContainText('1 selected');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await mike.uncheck();
	await expect(selectionCount(page)).toContainText('0 selected');
	expect(h.requests).toEqual(['GET /api/library']);
	expect(h.writes).toEqual([]);
});

test('[SA-B-01] one Select all action selects every displayed row, then a few can be deselected', async ({
	page
}) => {
	await openLibrary(page);
	const button = await requireSelectAll(page);

	await button.click();
	await expect(selectionCount(page)).toContainText('5 selected');
	await expectChecked(page, ALL_IDS, []);
	await expectFindCount(page, 5);
	// Selecting is not queueing.
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(appendButton(page)).toBeEnabled();

	// Re-activating with everything selected changes nothing.
	await button.click();
	await expect(selectionCount(page)).toContainText('5 selected');
	await expectChecked(page, ALL_IDS, []);

	// Deselect-a-few keeps the rest and the truthful count.
	await checkbox(page, 'sa-alpha').uncheck();
	await checkbox(page, 'sa-delta').uncheck();
	await expect(selectionCount(page)).toContainText('3 selected');
	await expectChecked(page, ['sa-zulu', 'sa-mike', 'sa-bravo'], ['sa-alpha', 'sa-delta']);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[SA-B-02] Clear selection still empties after a select-all, and select-all works again', async ({
	page
}) => {
	await openLibrary(page);
	const button = await requireSelectAll(page);

	await button.click();
	await expect(selectionCount(page)).toContainText('5 selected');

	await clearButton(page).click();
	await expect(selectionCount(page)).toContainText('0 selected');
	await expectChecked(page, [], ALL_IDS);
	await expectDisplayed(page, ALL_IDS);
	await expect(appendButton(page)).toBeDisabled();

	await button.click();
	await expect(selectionCount(page)).toContainText('5 selected');
	await expectChecked(page, ALL_IDS, []);
});

test('[SA-B-03] select-all under a find adds only the displayed rows and preserves the hidden selection', async ({
	page
}) => {
	await openLibrary(page);

	// Bravo is selected first; the find below hides it.
	await checkbox(page, 'sa-bravo').check();
	await expect(selectionCount(page)).toContainText('1 selected');

	await findField(page).fill('harbor');
	await expectDisplayed(page, ['sa-zulu', 'sa-mike', 'sa-delta']);
	await expectFindCount(page, 3);

	const button = await requireSelectAll(page);
	await button.click();
	await expect(selectionCount(page)).toContainText('4 selected');
	await expectChecked(page, ['sa-zulu', 'sa-mike', 'sa-delta'], []);
	// Bravo stays selected and is disclosed as hidden, not dropped.
	const hidden = page.getByTestId('selection-hidden-count');
	await expect(hidden, 'find must disclose the selected row it hides').toBeVisible();
	await expect(hidden).toContainText('1');
	await expect(hidden).toContainText(/hidden/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	await page.getByRole('button', { name: 'Clear search', exact: true }).first().click();
	await expectDisplayed(page, ALL_IDS);
	// Alpha was loaded all along but never displayed at select-all time.
	await expectChecked(page, ['sa-bravo', 'sa-zulu', 'sa-mike', 'sa-delta'], ['sa-alpha']);
	await expect(selectionCount(page)).toContainText('4 selected');
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);
});

test('[SA-B-04] switching the search scope then selecting-all follows the newly displayed set', async ({
	page
}) => {
	await openLibrary(page);
	await findField(page).fill('harbor');

	// Title only: only the title carrying the term is displayed.
	await scopeControl(page).selectOption({ label: 'Title only' });
	await expectDisplayed(page, ['sa-delta']);
	const button = await requireSelectAll(page);
	await button.click();
	await expect(selectionCount(page)).toContainText('1 selected');
	await expectChecked(page, ['sa-delta'], []);

	// Artist only: a different displayed set for the same query.
	await scopeControl(page).selectOption({ label: 'Artist only' });
	await expectDisplayed(page, ['sa-zulu', 'sa-mike']);
	await button.click();
	await expect(selectionCount(page)).toContainText('3 selected');
	await expectChecked(page, ['sa-zulu', 'sa-mike'], []);
	const hidden = page.getByTestId('selection-hidden-count');
	await expect(hidden, 'the title-scope selection is hidden under artist scope').toBeVisible();
	await expect(hidden).toContainText('1');

	// Both fields again: the union is displayed, all of it selected, and
	// the rows no scope ever displayed were never swept in.
	await scopeControl(page).selectOption({ label: 'Both title and artist' });
	await expectDisplayed(page, ['sa-zulu', 'sa-mike', 'sa-delta']);
	await expectChecked(page, ['sa-zulu', 'sa-mike', 'sa-delta'], []);
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);
	await page.getByRole('button', { name: 'Clear search', exact: true }).first().click();
	await expectDisplayed(page, ALL_IDS);
	await expectChecked(page, ['sa-zulu', 'sa-mike', 'sa-delta'], ['sa-alpha', 'sa-bravo']);
	await expect(selectionCount(page)).toContainText('3 selected');
});

test('[SA-B-05] after an ordering switch, append order proves new IDs were added in displayed order', async ({
	page
}) => {
	await openLibrary(page);

	// Delta is activated first, before any reorder, so it must keep the
	// head of the activation order a select-all appends behind.
	await checkbox(page, 'sa-delta').check();
	await orderControl(page).selectOption('title');
	await expectDisplayed(page, ['sa-alpha', 'sa-bravo', 'sa-delta', 'sa-mike', 'sa-zulu']);

	const button = await requireSelectAll(page);
	await button.click();
	await expect(selectionCount(page)).toContainText('5 selected');
	await expectChecked(page, ALL_IDS, []);

	await appendButton(page).click();
	await expect(page.getByTestId('append-result')).toContainText('Added 5');
	await expect
		.poll(() => queueOrder(page))
		.toEqual(['sa-delta', 'sa-alpha', 'sa-bravo', 'sa-mike', 'sa-zulu']);
	// Appending keeps the selection, per the shipped append contract.
	await expect(selectionCount(page)).toContainText('5 selected');
});

test('[SA-B-06] a successful refresh prunes a select-all and selecting-all again never resurrects the pruned ID', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await openLibrary(page, payload);
	const button = await requireSelectAll(page);

	await button.click();
	await expect(selectionCount(page)).toContainText('5 selected');

	payload.current = ITEMS.filter((item) => item.id !== 'sa-zulu');
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length - 1);
	await expect(selectionCount(page)).toContainText('4 selected');
	await expect(row(page, 'sa-zulu')).toHaveCount(0);

	await button.click();
	await expect(selectionCount(page)).toContainText('4 selected');
	await expectChecked(page, ['sa-alpha', 'sa-mike', 'sa-bravo', 'sa-delta'], []);
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);

	await appendButton(page).click();
	await expect
		.poll(() => queueOrder(page))
		.toEqual(['sa-alpha', 'sa-mike', 'sa-bravo', 'sa-delta']);
});

test('[SA-B-07] Select all is Tab-reachable, at least 24x24, keeps focus and announces politely', async ({
	page
}) => {
	await openLibrary(page);
	const button = await requireSelectAll(page);

	const box = await button.boundingBox();
	expect(box, 'Select all must have a real target box').not.toBeNull();
	expect((box as { width: number }).width).toBeGreaterThanOrEqual(24);
	expect((box as { height: number }).height).toBeGreaterThanOrEqual(24);

	await page.evaluate(() => {
		const active = document.activeElement as HTMLElement | null;
		active?.blur();
	});
	let reached = false;
	for (let tabs = 0; tabs < 90 && !reached; tabs += 1) {
		await page.keyboard.press('Tab');
		reached = await button.evaluate((element) => element === document.activeElement);
	}
	expect(reached, 'Select all must be reachable by Tab').toBe(true);
	await expect(button).toBeFocused();

	await page.keyboard.press('Enter');
	await expect(selectionCount(page)).toContainText('5 selected');
	await expectChecked(page, ALL_IDS, []);
	// The control stays mounted and enabled, so focus must survive.
	await expect(button).toBeFocused();

	// Polite only inside <main>: no assertive region, no alert.
	await expect(page.locator('main').locator('[aria-live="assertive"]')).toHaveCount(0);
	await expect(page.locator('main').getByRole('alert')).toHaveCount(0);
});

test('[SA-B-08] select-all and clear are presentation-only: no fetch, write, queue, star or playback', async ({
	page
}) => {
	const h = await openLibrary(page);
	const button = await requireSelectAll(page);
	const beforeRequests = [...h.requests];
	expect(beforeRequests).toEqual(['GET /api/library']);

	await button.click();
	await expect(selectionCount(page)).toContainText('5 selected');
	await clearButton(page).click();
	await expect(selectionCount(page)).toContainText('0 selected');
	await button.click();
	await expect(selectionCount(page)).toContainText('5 selected');

	expect(h.requests, 'selecting must not refetch the library').toEqual(beforeRequests);
	expect(h.writes, 'selecting must not mutate over the network').toEqual([]);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	for (const item of ITEMS) {
		await expect(starButton(page, item.id)).toHaveAttribute('aria-pressed', 'false');
	}
	await expectDisplayed(page, ALL_IDS);
});

test('[SA-B-09] a find-scoped select-all reviews exactly under Selected only, in loaded order', async ({
	page
}) => {
	await openLibrary(page);
	await findField(page).fill('harbor');
	await expectDisplayed(page, ['sa-zulu', 'sa-mike', 'sa-delta']);

	const button = await requireSelectAll(page);
	await button.click();
	await expect(selectionCount(page)).toContainText('3 selected');

	await showControl(page).selectOption({ label: 'Selected only' });
	await expectDisplayed(page, ['sa-zulu', 'sa-mike', 'sa-delta']);
	await page.getByRole('button', { name: 'Clear search', exact: true }).first().click();
	// Clearing the find inside Selected only still shows just the
	// selection the scoped select-all built — nothing else crept in.
	await expectDisplayed(page, ['sa-zulu', 'sa-mike', 'sa-delta']);
	await expectFindCount(page, 3);
	await expect(selectionCount(page)).toContainText('3 selected');
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);

	await showControl(page).selectOption({ label: 'All tracks' });
	await expectDisplayed(page, ALL_IDS);
	await expectChecked(page, ['sa-zulu', 'sa-mike', 'sa-delta'], ['sa-alpha', 'sa-bravo']);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});
