import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: select or clear the displayed track subset without losing hidden choices.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * visible-subset selection wiring on top of this branch and must make these
 * tests pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance lane instead. The pure-transform half is
 * `src/lib/library/visible-selection-acceptance.test.ts`.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click-driven contract is pinned here
 * in the offline Playwright suite with a route-mocked `/api/library`. Every
 * test guards its first new-control interaction with an explicit visibility
 * assertion, so a run against a build without the controls fails as an
 * assertion (missing control), never as a click error. Fully offline and
 * deterministic: no backend, no provider, no spend.
 *
 * Locked surface (the only new elements; every other hook already ships):
 * - Three native buttons inside the existing selection bar beside `Clear
 *   selection` / `Append selected`, with exact accessible names `Select
 *   displayed`, `Deselect displayed` and `Invert displayed selection`.
 * - Each activation resolves its targets from the current canonical displayed
 *   projection at that moment (the rendered `library-item` rows: loaded
 *   search, All/Favorites/Selected view and ordering, composing with any
 *   already-merged facet). The projection is read from the rendered rows, so
 *   these tests pin the composition black-box.
 * - `visible-selection-result` (`data-testid`, `role="status"`): one polite
 *   aggregate result after each explicit action. Its text names the displayed
 *   subset (matches /displayed/i), carries a digit, matches the action verb
 *   (select / deselect / invert) and never implies unseen remote catalog
 *   tracks were selected (no /all tracks|entire|catalog|remote|every track/).
 *   It updates in place: repeated actions never stack multiple results, and no
 *   assertive live region appears inside <main>.
 * - Empty displayed subset: all three buttons stay visible but disabled, the
 *   existing honest empty copy shows (no-match / no-favorites wording), and no
 *   result claims a selection happened.
 *
 * Contract under test (paired build):
 * - Select appends displayed IDs once in displayed order while retaining
 *   existing selected IDs and their order; deselect removes only displayed
 *   IDs; invert toggles only displayed membership, retaining hidden selected
 *   IDs and appending newly selected IDs in displayed order. Double inversion
 *   restores the exact selection and order.
 * - Operations never enqueue, star, play, clear hidden selections, fetch or
 *   write persistence. The existing stable-ID selection source of truth and
 *   limits stay: successful refresh prunes vanished IDs (visible and hidden),
 *   transient failure keeps everything, auth/account teardown purges, and
 *   global Clear selection still clears all including hidden.
 * - Visible/total/hidden counts stay consistent after every action
 *   (`library-find-count` displayed-of-loaded, `selection-count` total,
 *   `selection-hidden-count` hidden). The invoking control keeps focus;
 *   filter and favorite changes keep focus in the changed control with the
 *   selection intact. Existing Append selected consumes the resulting
 *   selection exactly once in activation order; Add displayed stays a distinct
 *   queue action that never touches the selection.
 *
 * Case IDs (stable): VISIBLESELECT-00 control, VISIBLESELECT-01 controls
 * render, VISIBLESELECT-02 select order, VISIBLESELECT-03 select idempotent,
 * VISIBLESELECT-04 deselect keeps hidden, VISIBLESELECT-05 invert order,
 * VISIBLESELECT-06 double inversion, VISIBLESELECT-07 find composition and
 * counts, VISIBLESELECT-08 favorites composition, VISIBLESELECT-09 ordering
 * composition, VISIBLESELECT-10 selected-only composition, VISIBLESELECT-11
 * empty projection disabled, VISIBLESELECT-12 side-effect audit,
 * VISIBLESELECT-13 polite aggregate result, VISIBLESELECT-14 focus,
 * VISIBLESELECT-15 append consumes once, VISIBLESELECT-16 add-displayed
 * stays distinct, VISIBLESELECT-17 clear still clears all, VISIBLESELECT-18
 * refresh prune, VISIBLESELECT-19 auth purge, VISIBLESELECT-20 duplicate IDs,
 * VISIBLESELECT-21 narrow both-theme keyboard/touch. VISIBLESELECT-30..39
 * live in the pure sibling file.
 *
 * Out of scope (not pinned): queue row multi-selection/removal, bulk
 * favorites, server-side selection, new facets/persistence, remote search,
 * playlists, backend activation, deployed streaming. Queue/media evidence is
 * state invariance in the mocked page, not a claim of audible playback.
 * Touch is pinned through the native button contract (tagName plus pointer
 * activation at 390px), the same equivalence the checkbox banks use.
 *
 * Focused command (repo root, deterministic, offline):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-visible-selection.spec.ts --workers=1 --retries=0
 */

// Rendered order is API order; ids are deliberately not in sorted order.
// Titles/artists avoid control vocabulary ("select", "clear", "append",
// "display", "invert", "star", "favorite", "queue", "add", "catalog",
// "remote", "entire", "every") so name-filtered locators and honest-copy
// assertions below can only match their intended control.
const ITEMS = [
	{ id: 'vs-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'vs-alpha', title: 'Alpha Song', artist: 'Solo Act' },
	{ id: 'vs-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'vs-bravo', title: 'Bravo Song', artist: 'Echo Band' },
	{ id: 'vs-delta', title: 'Delta Song', artist: 'Solo Act' }
];
const ALL_IDS = ITEMS.map((item) => item.id);

const NEXT_ACCOUNT = [{ id: 'vs-kilo', title: 'Kilo Song', artist: 'Next Account' }];

const DUPLICATE_ITEMS = [
	{ id: 'vs-dup', title: 'First Copy Song', artist: 'Dup Artist' },
	{ id: 'vs-solo', title: 'Only Song', artist: 'Dup Artist' },
	{ id: 'vs-dup', title: 'Conflicting Duplicate Title', artist: 'Other Artist' }
];

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

function selectionBar(page: Page): Locator {
	return page.locator('.nt-selection-bar');
}

function selectionCount(page: Page): Locator {
	return page.getByTestId('selection-count');
}

function hiddenSelectionCount(page: Page): Locator {
	return page.getByTestId('selection-hidden-count');
}

function findCount(page: Page): Locator {
	return page.getByTestId('library-find-count');
}

function appendButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Append selected', exact: true });
}

function clearButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Clear selection', exact: true });
}

function addDisplayedButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Add displayed tracks to queue', exact: true });
}

function selectDisplayedButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Select displayed', exact: true });
}

function deselectDisplayedButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Deselect displayed', exact: true });
}

function invertDisplayedButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Invert displayed selection', exact: true });
}

function showControl(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Show', exact: true });
}

function orderControl(page: Page): Locator {
	return page.getByRole('combobox', { name: 'Order tracks', exact: true });
}

function findField(page: Page): Locator {
	return page.getByRole('searchbox', { name: 'Find in loaded tracks', exact: true });
}

function visibleResult(page: Page): Locator {
	return page.getByTestId('visible-selection-result');
}

async function displayedIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('library-item')
		.evaluateAll((rows) => rows.map((item) => item.getAttribute('data-id')));
}

async function checkedIds(page: Page): Promise<string[]> {
	return page
		.getByTestId('library-item')
		.evaluateAll((rows) =>
			rows
				.filter(
					(item) =>
						(item.querySelector('input[type="checkbox"]') as HTMLInputElement | null)
							?.checked === true
				)
				.map((item) => item.getAttribute('data-id') ?? '')
		);
}

async function queueOrder(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

/**
 * The one polite aggregate result: visible, a single status element naming
 * the displayed subset with a count and the action verb, never implying the
 * unseen remote catalog was selected.
 */
async function expectVisibleResult(page: Page, verb: RegExp, notVerb?: RegExp): Promise<void> {
	const result = visibleResult(page);
	await expect(result, 'visible-selection result missing').toBeVisible();
	await expect(result).toHaveCount(1);
	await expect(result).toHaveAttribute('role', 'status');
	const text = (await result.textContent()) ?? '';
	expect(text).toMatch(/displayed/i);
	expect(text).toMatch(/\d/);
	expect(text).toMatch(verb);
	if (notVerb) expect(text).not.toMatch(notVerb);
	expect(text).not.toMatch(/\ball tracks\b|\bentire\b|\bcatalog\b|\bremote\b|\bevery track\b/i);
}

async function expectNoAssertiveInMain(page: Page): Promise<void> {
	await expect(page.locator('main').getByRole('alert')).toHaveCount(0);
}

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-visible-selection-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
}

test('[VISIBLESELECT-00] control: the library renders fixture rows in API order', async ({
	page
}) => {
	await seedLibrary(page);

	expect(await displayedIds(page)).toEqual(ALL_IDS);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(appendButton(page)).toBeDisabled();
});

test('[VISIBLESELECT-01] the three subset controls render beside the selection actions', async ({
	page
}) => {
	await seedLibrary(page);

	const bar = selectionBar(page);
	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await expect(deselectDisplayedButton(page), 'Deselect displayed missing').toBeVisible();
	await expect(invertDisplayedButton(page), 'Invert displayed selection missing').toBeVisible();

	// Beside the existing controls: the same selection bar hosts all five.
	for (const name of ['Select displayed', 'Deselect displayed', 'Invert displayed selection']) {
		await expect(bar.getByRole('button', { name, exact: true })).toBeVisible();
	}
	await expect(bar.getByRole('button', { name: 'Clear selection', exact: true })).toBeVisible();
	await expect(bar.getByRole('button', { name: 'Append selected', exact: true })).toBeVisible();

	// Native buttons (keyboard/touch behavior) enabled while rows display.
	for (const control of [
		selectDisplayedButton(page),
		deselectDisplayedButton(page),
		invertDisplayedButton(page)
	]) {
		expect(await control.evaluate((element) => element.tagName)).toBe('BUTTON');
		await expect(control).toBeEnabled();
	}

	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(appendButton(page)).toBeDisabled();
	await expect(addDisplayedButton(page)).toBeEnabled();
});

test('[VISIBLESELECT-02] select appends displayed IDs once in displayed order, retaining existing order', async ({
	page
}) => {
	await seedLibrary(page);

	// Activation order runs against displayed order: mike (3rd) first.
	await checkbox(page, 'vs-mike').check();
	await expect(selectionCount(page)).toContainText('1 selected');

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await select.click();

	await expect(selectionCount(page)).toContainText('5 selected');
	expect(await checkedIds(page)).toEqual(ALL_IDS);
	await expectVisibleResult(page, /select/i, /deselect/i);

	// Append proves the activation order: mike first, then the rest in the
	// displayed order they were appended in.
	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(5);
	expect(await queueOrder(page)).toEqual([
		'vs-mike',
		'vs-zulu',
		'vs-alpha',
		'vs-bravo',
		'vs-delta'
	]);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});

test('[VISIBLESELECT-03] selecting when all displayed are selected changes nothing but the polite result', async ({
	page
}) => {
	await seedLibrary(page);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await select.click();
	await expect(selectionCount(page)).toContainText('5 selected');

	await select.click();

	await expect(selectionCount(page)).toContainText('5 selected');
	expect(await checkedIds(page)).toEqual(ALL_IDS);
	await expectVisibleResult(page, /select/i, /deselect/i);
	// The no-op still selects nothing new: no queue, no playback, one result.
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[VISIBLESELECT-04] deselect removes only displayed IDs and keeps hidden choices in order', async ({
	page
}) => {
	await seedLibrary(page);

	const deselect = deselectDisplayedButton(page);
	await expect(deselect, 'Deselect displayed missing').toBeVisible();

	// Nothing selected is a honest no-op: counts stay zero, queue untouched.
	await deselect.click();
	await expect(selectionCount(page)).toContainText('0 selected');
	await expectVisibleResult(page, /deselect/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	await selectDisplayedButton(page).click();
	await expect(selectionCount(page)).toContainText('5 selected');

	// Harbor narrows the displayed subset to zulu + mike; the other three
	// stay selected but hidden.
	await findField(page).fill('harbor');
	expect(await displayedIds(page)).toEqual(['vs-zulu', 'vs-mike']);
	await deselect.click();

	await expect(selectionCount(page)).toContainText('3 selected');
	await expect(hiddenSelectionCount(page)).toContainText('3 selected');
	await expectVisibleResult(page, /deselect/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	expect(await displayedIds(page)).toEqual(ALL_IDS);
	expect(await checkedIds(page)).toEqual(['vs-alpha', 'vs-bravo', 'vs-delta']);

	// Survivors append in their retained activation order.
	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['vs-alpha', 'vs-bravo', 'vs-delta']);
});

test('[VISIBLESELECT-05] invert toggles only displayed membership, appending newly selected in displayed order', async ({
	page
}) => {
	await seedLibrary(page);

	await checkbox(page, 'vs-alpha').check();
	await findField(page).fill('harbor');
	expect(await displayedIds(page)).toEqual(['vs-zulu', 'vs-mike']);

	const invert = invertDisplayedButton(page);
	await expect(invert, 'Invert displayed selection missing').toBeVisible();
	await invert.click();

	// Alpha stays selected but hidden; zulu + mike append in displayed order.
	await expect(selectionCount(page)).toContainText('3 selected');
	await expect(hiddenSelectionCount(page)).toContainText('1 selected');
	await expectVisibleResult(page, /invert/i);
	expect(await checkedIds(page)).toEqual(['vs-zulu', 'vs-mike']);

	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	expect(await checkedIds(page)).toEqual(['vs-zulu', 'vs-alpha', 'vs-mike']);

	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['vs-alpha', 'vs-zulu', 'vs-mike']);
});

test('[VISIBLESELECT-06] inverting twice restores the exact selection and order', async ({
	page
}) => {
	await seedLibrary(page);

	await checkbox(page, 'vs-bravo').check();
	await checkbox(page, 'vs-delta').check();
	await findField(page).fill('solo');
	expect(await displayedIds(page)).toEqual(['vs-alpha', 'vs-delta']);

	const invert = invertDisplayedButton(page);
	await expect(invert, 'Invert displayed selection missing').toBeVisible();
	await invert.click();

	// Delta (selected, displayed) drops out; alpha (unselected, displayed)
	// joins; bravo (selected, hidden) never moves.
	await expect(selectionCount(page)).toContainText('2 selected');
	expect(await checkedIds(page)).toEqual(['vs-alpha']);
	await expect(hiddenSelectionCount(page)).toContainText('1 selected');

	await invert.click();

	await expect(selectionCount(page)).toContainText('2 selected');
	expect(await checkedIds(page)).toEqual(['vs-delta']);
	await expectVisibleResult(page, /invert/i);

	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	expect(await checkedIds(page)).toEqual(['vs-bravo', 'vs-delta']);

	await appendButton(page).click();
	expect(await queueOrder(page)).toEqual(['vs-bravo', 'vs-delta']);
});

test('[VISIBLESELECT-07] the find query composes with counts across partial and hidden sets', async ({
	page
}) => {
	await seedLibrary(page);

	await checkbox(page, 'vs-bravo').check();
	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();

	await findField(page).fill('harbor');
	expect(await displayedIds(page)).toEqual(['vs-zulu', 'vs-mike']);
	await expect(findCount(page)).toContainText('Showing 2 of 5 tracks');

	await select.click();
	await expect(selectionCount(page)).toContainText('3 selected');
	await expect(hiddenSelectionCount(page)).toContainText('1 selected');
	await expectVisibleResult(page, /select/i, /deselect/i);

	// A second query hides the whole selection; inverting there only touches
	// the newly displayed rows.
	await findField(page).fill('solo');
	expect(await displayedIds(page)).toEqual(['vs-alpha', 'vs-delta']);
	await expect(findCount(page)).toContainText('Showing 2 of 5 tracks');
	await expect(hiddenSelectionCount(page)).toContainText('3 selected');

	await invertDisplayedButton(page).click();
	await expect(selectionCount(page)).toContainText('5 selected');
	await expect(hiddenSelectionCount(page)).toContainText('3 selected');

	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	await expect(findCount(page)).toContainText('Showing 5 of 5 tracks');
	await expect(hiddenSelectionCount(page)).toHaveCount(0);
	expect(await checkedIds(page)).toEqual(ALL_IDS);
});

test('[VISIBLESELECT-08] favorites view composes: select touches only favorited rows', async ({
	page
}) => {
	await seedLibrary(page);

	await starButton(page, 'vs-zulu').click();
	await starButton(page, 'vs-bravo').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await checkbox(page, 'vs-alpha').check();

	await showControl(page).selectOption('favorites');
	expect(await displayedIds(page)).toEqual(['vs-zulu', 'vs-bravo']);
	await expect(findCount(page)).toContainText('Showing 2 of 5 tracks');

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await select.click();

	await expect(selectionCount(page)).toContainText('3 selected');
	await expect(hiddenSelectionCount(page)).toContainText('1 selected');
	await expectVisibleResult(page, /select/i, /deselect/i);
	// The favorites shelf is untouched by selecting from it.
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);

	await showControl(page).selectOption('all');
	expect(await checkedIds(page)).toEqual(['vs-zulu', 'vs-alpha', 'vs-bravo']);

	await appendButton(page).click();
	expect(await queueOrder(page)).toEqual(['vs-alpha', 'vs-zulu', 'vs-bravo']);
});

test('[VISIBLESELECT-09] ordering composes: select appends in sorted displayed order', async ({
	page
}) => {
	await seedLibrary(page);

	await checkbox(page, 'vs-mike').check();
	await orderControl(page).selectOption('title');
	expect(await displayedIds(page)).toEqual([
		'vs-alpha',
		'vs-bravo',
		'vs-delta',
		'vs-mike',
		'vs-zulu'
	]);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await select.click();

	await expect(selectionCount(page)).toContainText('5 selected');
	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(5);
	expect(await queueOrder(page)).toEqual([
		'vs-mike',
		'vs-alpha',
		'vs-bravo',
		'vs-delta',
		'vs-zulu'
	]);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});

test('[VISIBLESELECT-10] selected-only view composes: deselect clears the shown set and keeps focus', async ({
	page
}) => {
	await seedLibrary(page);

	await checkbox(page, 'vs-alpha').check();
	await checkbox(page, 'vs-mike').check();
	await showControl(page).selectOption('selected');
	expect(await displayedIds(page)).toEqual(['vs-alpha', 'vs-mike']);
	await expect(findCount(page)).toContainText('Showing 2 of 5 tracks');

	const deselect = deselectDisplayedButton(page);
	await expect(deselect, 'Deselect displayed missing').toBeVisible();
	await deselect.click();

	// Both shown rows were selected, so the set empties honestly.
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(hiddenSelectionCount(page)).toHaveCount(0);
	await expectVisibleResult(page, /deselect/i);
	await expect(page.locator('main')).toContainText(/no tracks selected/i);
	await expect(
		page.getByRole('button', { name: 'Show all tracks', exact: true })
	).toBeVisible();
	// The invoking control stays mounted and keeps focus.
	await expect(deselect).toBeFocused();

	await showControl(page).selectOption('all');
	expect(await checkedIds(page)).toEqual([]);
	await expect(appendButton(page)).toBeDisabled();
});

test('[VISIBLESELECT-11] an empty displayed subset disables the controls with honest copy', async ({
	page
}) => {
	await seedLibrary(page);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	const deselect = deselectDisplayedButton(page);
	const invert = invertDisplayedButton(page);

	// A find query with no matches: disabled, honest empty copy, no result.
	await findField(page).fill('no matching title or artist');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	for (const control of [select, deselect, invert]) {
		await expect(control).toBeVisible();
		await expect(control).toBeDisabled();
	}
	await expect(page.locator('main')).toContainText(/no .*matches/i);
	await expect(visibleResult(page)).toHaveCount(0);
	await expect(selectionCount(page)).toContainText('0 selected');

	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	for (const control of [select, deselect, invert]) {
		await expect(control).toBeEnabled();
	}

	// An empty favorites view: disabled with the honest favorites copy.
	await showControl(page).selectOption('favorites');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	for (const control of [select, deselect, invert]) {
		await expect(control).toBeVisible();
		await expect(control).toBeDisabled();
	}
	await expect(page.locator('main')).toContainText(/no favorites/i);
	await expect(visibleResult(page)).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	await showControl(page).selectOption('all');
	for (const control of [select, deselect, invert]) {
		await expect(control).toBeEnabled();
	}
});

test('[VISIBLESELECT-12] subset operations queue, star, fetch, play and persist nothing', async ({
	page
}) => {
	const requests: { method: string; url: string }[] = [];
	page.on('request', (request) => requests.push({ method: request.method(), url: request.url() }));
	await seedLibrary(page);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await select.click();
	await invertDisplayedButton(page).click();
	await deselectDisplayedButton(page).click();

	// Non-vacuous gate: the three operations ran (the set round-trips to a
	// known state) and collapsed into a single polite result.
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(visibleResult(page)).toHaveCount(1);

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	for (const item of ITEMS) {
		await expect(starButton(page, item.id)).toHaveAttribute('aria-pressed', 'false');
	}
	await expect(page.getByTestId('library-queued-badge')).toHaveCount(0);

	const libraryGets = requests.filter(
		(request) => request.method === 'GET' && request.url.includes('/api/library')
	);
	expect(libraryGets, 'subset operations must not refetch the catalog').toHaveLength(1);
	expect(
		requests.filter((request) => request.method !== 'GET'),
		'subset operations must not mutate over the network'
	).toEqual([]);

	const storedKeys = await page.evaluate(() => [
		...Array.from({ length: window.sessionStorage.length }, (_, i) =>
			window.sessionStorage.key(i)
		),
		...Array.from({ length: window.localStorage.length }, (_, i) => window.localStorage.key(i))
	]);
	expect(
		(storedKeys.filter(Boolean) as string[]).filter((key) => key.toLowerCase().includes('select')),
		'subset operations must not persist under a selection key'
	).toEqual([]);
});

test('[VISIBLESELECT-13] each action reports one polite aggregate result, never per-row announcements', async ({
	page
}) => {
	await seedLibrary(page);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await select.click();
	await expectVisibleResult(page, /select/i, /deselect/i);
	await expectNoAssertiveInMain(page);

	await deselectDisplayedButton(page).click();
	await expectVisibleResult(page, /deselect/i);
	await expectNoAssertiveInMain(page);

	await checkbox(page, 'vs-alpha').check();
	await invertDisplayedButton(page).click();
	await expectVisibleResult(page, /invert/i);
	await expectNoAssertiveInMain(page);

	// Repeated actions update the single result in place; nothing stacks and
	// nothing outside the result announces rows.
	await expect(visibleResult(page)).toHaveCount(1);
	await expect(selectionCount(page)).toContainText('4 selected');
});

test('[VISIBLESELECT-14] invoking and changed controls keep focus with the selection intact', async ({
	page
}) => {
	await seedLibrary(page);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();

	// Keyboard-activating each control leaves focus on that control.
	await select.focus();
	await page.keyboard.press('Space');
	await expect(select).toBeFocused();
	await expect(selectionCount(page)).toContainText('5 selected');

	const deselect = deselectDisplayedButton(page);
	await deselect.focus();
	await page.keyboard.press('Enter');
	await expect(deselect).toBeFocused();
	await expect(selectionCount(page)).toContainText('0 selected');

	await checkbox(page, 'vs-alpha').check();
	const invert = invertDisplayedButton(page);
	await invert.focus();
	await page.keyboard.press('Space');
	await expect(invert).toBeFocused();
	await expect(selectionCount(page)).toContainText('4 selected');

	// Narrowing the rows keeps focus in the find field; the selection survives.
	const find = findField(page);
	await find.focus();
	await find.fill('harbor');
	expect(await displayedIds(page)).toEqual(['vs-zulu', 'vs-mike']);
	await expect(find).toBeFocused();
	await expect(selectionCount(page)).toContainText('4 selected');
	await expect(hiddenSelectionCount(page)).toContainText('2 selected');

	// Starring a displayed row keeps focus on the star without touching the set.
	const star = starButton(page, 'vs-zulu');
	await star.focus();
	await page.keyboard.press('Enter');
	await expect(star).toHaveAttribute('aria-pressed', 'true');
	await expect(star).toBeFocused();
	await expect(selectionCount(page)).toContainText('4 selected');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[VISIBLESELECT-15] Append selected consumes the visible-built selection exactly once', async ({
	page
}) => {
	await seedLibrary(page);

	await checkbox(page, 'vs-delta').check();
	await findField(page).fill('solo');
	expect(await displayedIds(page)).toEqual(['vs-alpha', 'vs-delta']);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await select.click();
	await expect(selectionCount(page)).toContainText('2 selected');

	const append = appendButton(page);
	await append.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['vs-delta', 'vs-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Delta Song');

	const result = page.getByTestId('append-result');
	await expect(result, 'append result missing').toBeVisible();
	await expect(result).toContainText('Added 2');
	await expect(result).toContainText('0 already queued');

	// The selection survives the append; repeating it is idempotent.
	await expect(selectionCount(page)).toContainText('2 selected');
	await append.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['vs-delta', 'vs-alpha']);
	await expect(result).toContainText('Added 0');
	await expect(result).toContainText('2 already queued');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[VISIBLESELECT-16] Add displayed stays a distinct queue action that never touches the selection', async ({
	page
}) => {
	await seedLibrary(page);

	await checkbox(page, 'vs-alpha').check();
	await findField(page).fill('harbor');
	expect(await displayedIds(page)).toEqual(['vs-zulu', 'vs-mike']);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();

	await addDisplayedButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['vs-zulu', 'vs-mike']);
	await expect(page.getByTestId('queue-announcement')).toHaveText('Added 2 tracks to the queue.');

	// Queuing the projection selects nothing and reports no selection result.
	await expect(selectionCount(page)).toContainText('1 selected');
	await expect(visibleResult(page)).toHaveCount(0);
	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	expect(await checkedIds(page)).toEqual(['vs-alpha']);

	// And the pending selection still appends exactly once afterwards.
	await appendButton(page).click();
	expect(await queueOrder(page)).toEqual(['vs-zulu', 'vs-mike', 'vs-alpha']);
});

test('[VISIBLESELECT-17] Clear selection still clears all, including hidden choices', async ({
	page
}) => {
	await seedLibrary(page);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await select.click();
	await findField(page).fill('harbor');
	await deselectDisplayedButton(page).click();
	await expect(selectionCount(page)).toContainText('3 selected');
	await expect(hiddenSelectionCount(page)).toContainText('3 selected');

	await clearButton(page).click();

	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(hiddenSelectionCount(page)).toHaveCount(0);
	await expect(appendButton(page)).toBeDisabled();
	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	expect(await checkedIds(page)).toEqual([]);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[VISIBLESELECT-18] a successful refresh prunes vanished IDs, visible and hidden', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await select.click();
	await findField(page).fill('harbor');
	expect(await displayedIds(page)).toEqual(['vs-zulu', 'vs-mike']);

	// The snapshot drops one displayed ID (zulu) and one hidden ID (alpha).
	payload.current = ITEMS.filter((item) => item.id !== 'vs-zulu' && item.id !== 'vs-alpha');
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();

	expect(await displayedIds(page)).toEqual(['vs-mike']);
	await expect(findCount(page)).toContainText('Showing 1 of 3 tracks');
	await expect(selectionCount(page)).toContainText('3 selected');
	await expect(hiddenSelectionCount(page)).toContainText('2 selected');

	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	expect(await checkedIds(page)).toEqual(['vs-mike', 'vs-bravo', 'vs-delta']);

	await appendButton(page).click();
	expect(await queueOrder(page)).toEqual(['vs-mike', 'vs-bravo', 'vs-delta']);
});

test('[VISIBLESELECT-19] an auth transition purges the visible-built selection', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await select.click();
	await findField(page).fill('harbor');
	await expect(hiddenSelectionCount(page)).toContainText('3 selected');

	await fireAuthTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);

	payload.current = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(NEXT_ACCOUNT.length);
	await expect(page.getByTestId('library-list')).toContainText('Kilo Song');
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(hiddenSelectionCount(page)).toHaveCount(0);
	await expect(checkbox(page, 'vs-kilo')).not.toBeChecked();
	await expect(appendButton(page)).toBeDisabled();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[VISIBLESELECT-20] duplicate snapshot IDs select and append once with first-occurrence metadata', async ({
	page
}) => {
	// The established duplicate discipline renders unique rows, first wins —
	// seeded directly because the shared helper asserts full payload length.
	await routeLibrary(page, { current: DUPLICATE_ITEMS });
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(page.getByTestId('library-list')).toContainText('First Copy Song');
	await expect(page.getByTestId('library-list')).not.toContainText('Conflicting Duplicate Title');

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();
	await select.click();

	await expect(selectionCount(page)).toContainText('2 selected');
	expect(await checkedIds(page)).toEqual(['vs-dup', 'vs-solo']);
	await expectVisibleResult(page, /select/i, /deselect/i);

	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['vs-dup', 'vs-solo']);
	await expect(page.getByTestId('now-playing')).toContainText('First Copy Song');
});

test('[VISIBLESELECT-21] narrow-screen light and dark keep every control visible and operable', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await seedLibrary(page);

	const select = selectDisplayedButton(page);
	await expect(select, 'Select displayed missing').toBeVisible();

	for (const scheme of ['light', 'dark'] as const) {
		await page.emulateMedia({ colorScheme: scheme });

		await expect(selectDisplayedButton(page)).toBeVisible();
		await expect(deselectDisplayedButton(page)).toBeVisible();
		await expect(invertDisplayedButton(page)).toBeVisible();
		await expect(selectionCount(page)).toBeVisible();
		await expect(clearButton(page)).toBeVisible();
		await expect(appendButton(page)).toBeVisible();

		const overflow = await page.evaluate(
			() => document.documentElement.scrollWidth - document.documentElement.clientWidth
		);
		expect(overflow, `horizontal overflow in ${scheme} mode`).toBeLessThanOrEqual(1);
	}

	// Native buttons: keyboard activation keeps focus, pointer activation
	// (the touch path) appends in displayed order.
	for (const control of [
		selectDisplayedButton(page),
		deselectDisplayedButton(page),
		invertDisplayedButton(page)
	]) {
		expect(await control.evaluate((element) => element.tagName)).toBe('BUTTON');
	}
	await select.focus();
	await page.keyboard.press('Space');
	await expect(select).toBeFocused();
	await expect(selectionCount(page)).toContainText('5 selected');

	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(5);
	expect(await queueOrder(page)).toEqual(ALL_IDS);
});
