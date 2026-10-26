import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: review only selected tracks before appending them.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * Selected-only view on top of this branch and must make these tests pass
 * WITHOUT editing this file; a wrong test comes back to the acceptance lane
 * instead. The pure projection half is
 * `src/lib/library/selected-view-acceptance.test.ts`.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click/keyboard contract is pinned
 * here in the offline Playwright suite with a route-mocked `/api/library`.
 * Every test guards its first Selected-only interaction with an explicit
 * option-count assertion, so a run against a build without the option fails
 * as an assertion (missing control), never as a click timeout. Fully
 * offline and deterministic: no backend, no provider, no spend.
 *
 * Locked surface (all hooks already ship; the only new element is one
 * option in the existing control):
 * - The existing `Show` combobox gains a third option labeled exactly
 *   `Selected only`, beside the existing `All tracks` and `Favorites only`.
 *   Its value is the build's choice; these tests select by label. No second
 *   selection store, persistence key, control, or route is introduced, and
 *   there is still no Select all / Select displayed affordance: membership
 *   comes only from the existing per-row checkboxes.
 * - Selected only renders the ONE canonical projection
 *   (`selectSelectedItems`, pure half) of the loaded snapshot through the
 *   existing selected-ID set, composed with the existing find field and
 *   ordering control. Display follows loaded-list order as reshaped by the
 *   ordering step — never selection activation order. Append selected keeps
 *   its existing activation-order contract; Add displayed tracks to queue
 *   consumes exactly the displayed projection, nothing hidden.
 * - Truthful counts from the existing hooks: `library-find-count` reports
 *   displayed-of-loaded (`Showing <d> of <n> tracks`), `selection-count`
 *   reports `<s> selected`, and `selection-hidden-count` (existing testid)
 *   discloses selected rows the find query hides. An empty selection and a
 *   selection hidden by find are different states: the former says no
 *   tracks are selected (wording matching /no selected tracks|no tracks
 *   selected/i) with no hidden disclosure and offers the existing
 *   `Show all tracks` reset; the latter uses match/hidden wording with the
 *   hidden disclosure and recovers through the existing `Clear search`.
 *   Both recoveries preserve the selection and the ordering choice.
 * - Focus: deselecting a row inside Selected only moves focus to an
 *   actionable control inside the next surviving displayed row, else the
 *   previous surviving row, else the Show control once empty. Clear
 *   selection that removes the row containing focus falls back the same
 *   way (to Show, since the view is then empty). A control that remains
 *   mounted keeps its focus: activating Clear from the focused Clear
 *   selection button leaves focus there, changing Show keeps focus on
 *   Show, and typing in find never loses the caret. Feedback stays polite:
 *   no assertive live region is introduced inside <main>.
 * - View, query and selection changes are presentation-only: no fetch, no
 *   storage write, no queue/current/media/favorites change. Account
 *   teardown and snapshot pruning keep their existing behavior (auth
 *   boundary purges the selection and resets the view; a successful
 *   refresh prunes vanished IDs; a transient failure keeps everything).
 *
 * Case IDs (stable): SV-B-00 control, SV-B-01 selected-only membership and
 * view-switch purity, SV-B-02 ordering composition, SV-B-03 find
 * composition and recovery, SV-B-04 empty vs filtered-empty, SV-B-05
 * append-selected activation order, SV-B-06 add-displayed consumes the
 * projection, SV-B-07 deselect and clear focus fallbacks, SV-B-08
 * presentation-only side-effect audit, SV-B-09 refresh keep/prune,
 * SV-B-10 account teardown, SV-B-11 320px light/dark. SV-U-* live in the
 * pure sibling file.
 *
 * Out of scope (not pinned): select-all/invert/range selection, selection
 * persistence, saved lists, queue-row removal, changing append order,
 * deployed streaming. Queue/media evidence is state invariance in the
 * mocked page, not a claim of audible playback.
 *
 * Focused command (repo root, deterministic, offline):
 *   CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *     tests/library-selected-view.spec.ts --workers=1 --retries=0
 */

// Loaded (API) order is neither title order nor any activation order used
// below. Titles/artists avoid control vocabulary so name locators stay exact.
const ITEMS = [
	{ id: 'sv-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'sv-alpha', title: 'Alpha Song', artist: 'Solo Act' },
	{ id: 'sv-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'sv-bravo', title: 'Bravo Song', artist: 'Solo Act' },
	{ id: 'sv-delta', title: 'Delta Song', artist: 'Echo Band' }
];
const ALL_IDS = ITEMS.map((item) => item.id);
const NEXT_ACCOUNT = [{ id: 'sv-kilo', title: 'Kilo Song', artist: 'Next Account' }];

type Harness = {
	requests: string[];
	writes: string[];
	state: { abort: boolean };
};

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}
function checkbox(page: Page, id: string): Locator {
	return row(page, id).getByRole('checkbox');
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
function selectionCount(page: Page): Locator {
	return page.getByTestId('selection-count');
}
function appendButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Append selected', exact: true });
}
function clearButton(page: Page): Locator {
	return page.getByRole('button', { name: 'Clear selection', exact: true });
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
	await page.addInitScript(() => {
		const storageWrites: string[] = [];
		const mediaCalls = { play: 0, pause: 0 };
		Object.assign(window, { __svStorageWrites: storageWrites, __svMedia: mediaCalls });
		const originalSet = Storage.prototype.setItem;
		Storage.prototype.setItem = function (this: Storage, key: string, value: string) {
			storageWrites.push(key);
			return originalSet.call(this, key, value);
		};
		const originalRemove = Storage.prototype.removeItem;
		Storage.prototype.removeItem = function (this: Storage, key: string) {
			storageWrites.push(key);
			return originalRemove.call(this, key);
		};
		const originalPlay = HTMLMediaElement.prototype.play;
		HTMLMediaElement.prototype.play = function (this: HTMLMediaElement) {
			mediaCalls.play += 1;
			return originalPlay.call(this);
		};
		const originalPause = HTMLMediaElement.prototype.pause;
		HTMLMediaElement.prototype.pause = function (this: HTMLMediaElement) {
			mediaCalls.pause += 1;
			return originalPause.call(this);
		};
	});
	const state = { abort: false };
	await page.route('**/api/library', (route) => {
		if (state.abort) return route.abort('failed');
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(payload.current)
		});
	});
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(
		(payload.current as unknown[]).length
	);
	return { requests, writes, state };
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

async function chooseShow(page: Page, label: 'All tracks' | 'Favorites only' | 'Selected only') {
	await expect(
		showControl(page).getByRole('option', { name: label, exact: true }),
		`Show control must offer "${label}"`
	).toHaveCount(1);
	await showControl(page).selectOption({ label });
	await expect(showControl(page).locator('option:checked')).toHaveText(label);
}

async function selectTracks(page: Page, activationOrder: string[]) {
	for (const id of activationOrder) {
		const box = checkbox(page, id);
		await expect(box, `checkbox missing on row ${id}`).toBeVisible();
		await box.check();
	}
}

async function expectFindCount(page: Page, displayed: number, loaded = ITEMS.length) {
	await expect(
		page.getByTestId('library-find-count'),
		'displayed/loaded count must stay truthful in Selected only'
	).toContainText(new RegExp(`Showing ${displayed} of ${loaded} tracks?`));
}

async function focusedRow(page: Page): Promise<{ rowId: string | null; actionable: boolean }> {
	return page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		const holder = el?.closest('[data-testid="library-item"]');
		const actionable =
			el !== null &&
			(['INPUT', 'BUTTON', 'SELECT', 'A', 'TEXTAREA'].includes(el.tagName) ||
				el.getAttribute('role') === 'button' ||
				el.getAttribute('role') === 'option');
		return { rowId: holder?.getAttribute('data-id') ?? null, actionable };
	});
}

async function storageWriteCount(page: Page): Promise<number> {
	return page.evaluate(() => (Reflect.get(window, '__svStorageWrites') as string[]).length);
}

async function queueState(page: Page) {
	return page.evaluate(() => ({
		rows: [...document.querySelectorAll('[data-testid="queue-item"]')].map((node) => ({
			id: node.getAttribute('data-track-id') ?? node.getAttribute('data-id'),
			current: node.getAttribute('aria-current')
		})),
		now: document.querySelector('[data-testid="now-playing"]')?.textContent,
		media: [...document.querySelectorAll('audio, video')].map((node) => ({
			src: (node as HTMLMediaElement).currentSrc,
			paused: (node as HTMLMediaElement).paused
		})),
		calls: Reflect.get(window, '__svMedia') as { play: number; pause: number }
	}));
}

async function favoriteIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('favorite-item')
		.evaluateAll((items) => items.map((item) => item.getAttribute('data-id')));
}

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-selected-view-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
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

test('[SV-B-00] control: fixtures, existing Show choices and individual selection already work', async ({
	page
}) => {
	const h = await openLibrary(page);
	expect(await displayedIds(page)).toEqual(ALL_IDS);
	await expect(showControl(page).getByRole('option', { name: 'All tracks', exact: true })).toHaveCount(1);
	await expect(
		showControl(page).getByRole('option', { name: 'Favorites only', exact: true })
	).toHaveCount(1);
	await expect(orderControl(page)).toBeVisible();
	await expect(findField(page)).toBeVisible();
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(appendButton(page)).toBeDisabled();

	await selectTracks(page, ['sv-mike']);
	await expect(selectionCount(page)).toContainText('1 selected');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await checkbox(page, 'sv-mike').uncheck();
	await expect(selectionCount(page)).toContainText('0 selected');
	expect(h.requests).toEqual(['GET /api/library']);
	expect(h.writes).toEqual([]);
});

test('[SV-B-01] Selected only shows a noncontiguous selection in loaded order; view switches never mutate it', async ({
	page
}) => {
	await openLibrary(page);

	// Exactly three Show choices: the new option joins, nothing else appears.
	await expect(showControl(page).locator('option')).toHaveCount(3);
	// Membership is individual-only: no bulk-select escape hatch.
	await expect(page.getByRole('button', { name: /select all|select displayed/i })).toHaveCount(0);
	await expect(page.getByRole('checkbox', { name: /select all|select displayed/i })).toHaveCount(0);

	// Activation order opposes loaded order at every step.
	await selectTracks(page, ['sv-delta', 'sv-zulu', 'sv-mike']);
	await expect(selectionCount(page)).toContainText('3 selected');

	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, ['sv-zulu', 'sv-mike', 'sv-delta']);
	await expectFindCount(page, 3);
	await expect(selectionCount(page)).toContainText('3 selected');
	// Every selected row is displayed, so nothing is disclosed as hidden and
	// unselected rows are simply absent (never queued by viewing them).
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);
	await expect(row(page, 'sv-alpha')).toHaveCount(0);
	await expect(row(page, 'sv-bravo')).toHaveCount(0);
	for (const id of ['sv-zulu', 'sv-mike', 'sv-delta']) {
		await expect(checkbox(page, id)).toBeChecked();
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// Round-trip through the other views: the selection set is untouched.
	await chooseShow(page, 'Favorites only');
	await expectDisplayed(page, []);
	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, ['sv-zulu', 'sv-mike', 'sv-delta']);
	await chooseShow(page, 'All tracks');
	await expectDisplayed(page, ALL_IDS);
	for (const id of ['sv-zulu', 'sv-mike', 'sv-delta']) {
		await expect(checkbox(page, id)).toBeChecked();
	}
	for (const id of ['sv-alpha', 'sv-bravo']) {
		await expect(checkbox(page, id)).not.toBeChecked();
	}
	await expect(selectionCount(page)).toContainText('3 selected');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[SV-B-02] ordering composes inside Selected only and Original restores loaded order', async ({
	page
}) => {
	await openLibrary(page);
	await selectTracks(page, ['sv-delta', 'sv-zulu', 'sv-mike']);
	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, ['sv-zulu', 'sv-mike', 'sv-delta']);

	await orderControl(page).selectOption('title');
	await expectDisplayed(page, ['sv-delta', 'sv-mike', 'sv-zulu']);
	await orderControl(page).selectOption('title-desc');
	await expectDisplayed(page, ['sv-zulu', 'sv-mike', 'sv-delta']);
	// Artist ties (both Harbor Lights) keep loaded order: zulu before mike.
	await orderControl(page).selectOption('artist');
	await expectDisplayed(page, ['sv-delta', 'sv-zulu', 'sv-mike']);
	await orderControl(page).selectOption('original');
	await expectDisplayed(page, ['sv-zulu', 'sv-mike', 'sv-delta']);

	await expect(selectionCount(page)).toContainText('3 selected');
	for (const id of ['sv-zulu', 'sv-mike', 'sv-delta']) {
		await expect(checkbox(page, id)).toBeChecked();
	}
});

test('[SV-B-03] find composes inside Selected only; Clear search preserves selection, view, ordering and focus', async ({
	page
}) => {
	await openLibrary(page);
	await selectTracks(page, ['sv-delta', 'sv-zulu', 'sv-mike']);
	await orderControl(page).selectOption('title');
	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, ['sv-delta', 'sv-mike', 'sv-zulu']);

	const field = findField(page);
	await expect(field).toBeVisible();
	await field.fill('harbor');
	// Typing keeps the caret in the field while rows re-render.
	await expect(field).toBeFocused();
	await expectDisplayed(page, ['sv-mike', 'sv-zulu']);
	await expectFindCount(page, 2);
	await expect(selectionCount(page)).toContainText('3 selected');
	const hidden = page.getByTestId('selection-hidden-count');
	await expect(hidden, 'find must disclose the selected row it hides').toBeVisible();
	await expect(hidden).toContainText('1');
	await expect(hidden).toContainText(/hidden/i);

	await page.getByRole('button', { name: 'Clear search', exact: true }).first().click();
	await expect(field).toHaveValue('');
	await expect(field).toBeFocused();
	await expectDisplayed(page, ['sv-delta', 'sv-mike', 'sv-zulu']);
	await expect(showControl(page).locator('option:checked')).toHaveText('Selected only');
	await expect(orderControl(page)).toHaveValue('title');
	await expect(selectionCount(page)).toContainText('3 selected');
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);
	for (const id of ['sv-delta', 'sv-mike', 'sv-zulu']) {
		await expect(checkbox(page, id)).toBeChecked();
	}
});

test('[SV-B-04] empty selection and find-filtered-empty are distinct, each with its own recovery', async ({
	page
}) => {
	await openLibrary(page);

	// (a) Nothing selected yet: not a find problem, and it must not say so.
	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, []);
	await expectFindCount(page, 0);
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);
	const emptyMessage = page.locator('main').getByText(/no selected tracks|no tracks selected/i);
	await expect(emptyMessage.first(), 'empty selection must be named as such').toBeVisible();

	const reset = page.getByRole('button', { name: 'Show all tracks', exact: true });
	await expect(reset, 'empty Selected only must offer Show all tracks').toBeVisible();
	await reset.focus();
	await page.keyboard.press('Enter');
	await expectDisplayed(page, ALL_IDS);
	await expect(showControl(page).locator('option:checked')).toHaveText('All tracks');
	await expect(showControl(page)).toBeFocused();

	// (b) Selected rows exist but find hides every one of them.
	await selectTracks(page, ['sv-zulu', 'sv-mike']);
	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, ['sv-zulu', 'sv-mike']);
	// "alpha" matches only the unselected Alpha Song.
	await findField(page).fill('alpha');
	await expectDisplayed(page, []);
	await expectFindCount(page, 0);
	await expect(selectionCount(page)).toContainText('2 selected');
	await expect(
		page.locator('main').getByText(/no .*match|hidden/i).first(),
		'filtered-empty must blame the find, not claim nothing is selected'
	).toBeVisible();
	const hidden = page.getByTestId('selection-hidden-count');
	await expect(hidden).toBeVisible();
	await expect(hidden).toContainText('2');

	await page.getByRole('button', { name: 'Clear search', exact: true }).first().click();
	await expectDisplayed(page, ['sv-zulu', 'sv-mike']);
	await expect(showControl(page).locator('option:checked')).toHaveText('Selected only');
	await expect(selectionCount(page)).toContainText('2 selected');
	await expect(checkbox(page, 'sv-zulu')).toBeChecked();
	await expect(checkbox(page, 'sv-mike')).toBeChecked();
});

test('[SV-B-05] Append selected keeps activation order, never the Selected-only display order', async ({
	page
}) => {
	await openLibrary(page);
	// Activation: delta, zulu, mike. Loaded display: zulu, mike, delta.
	// Title display: delta, mike, zulu. All three orders differ pairwise.
	await selectTracks(page, ['sv-delta', 'sv-zulu', 'sv-mike']);
	await chooseShow(page, 'Selected only');
	await orderControl(page).selectOption('title');
	await expectDisplayed(page, ['sv-delta', 'sv-mike', 'sv-zulu']);

	await expect(appendButton(page)).toBeEnabled();
	await appendButton(page).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['sv-delta', 'sv-zulu', 'sv-mike']);
	await expect(page.getByTestId('now-playing')).toContainText('Delta Song');
	await expect(selectionCount(page)).toContainText('3 selected');
	await expectDisplayed(page, ['sv-delta', 'sv-mike', 'sv-zulu']);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});

test('[SV-B-06] Add displayed tracks consumes exactly the Selected-only projection', async ({
	page
}) => {
	await openLibrary(page);
	await selectTracks(page, ['sv-delta', 'sv-zulu', 'sv-mike']);
	await chooseShow(page, 'Selected only');
	await orderControl(page).selectOption('title-desc');
	await expectDisplayed(page, ['sv-zulu', 'sv-mike', 'sv-delta']);

	const addDisplayed = page.getByRole('button', {
		name: 'Add displayed tracks to queue',
		exact: true
	});
	await expect(addDisplayed).toBeVisible();
	await expect(addDisplayed).toBeEnabled();
	await addDisplayed.click();

	// Display order, selected rows only: unselected alpha/bravo never queue.
	await expect.poll(() => queueOrder(page)).toEqual(['sv-zulu', 'sv-mike', 'sv-delta']);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(selectionCount(page)).toContainText('3 selected');
});

test('[SV-B-07] deselect and clear land focus on the next useful control, never on the body', async ({
	page
}) => {
	await openLibrary(page);
	await selectTracks(page, ['sv-alpha', 'sv-mike', 'sv-bravo']);
	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, ['sv-alpha', 'sv-mike', 'sv-bravo']);

	// Middle row leaves: focus lands inside the NEXT surviving row.
	await checkbox(page, 'sv-mike').focus();
	await expect(checkbox(page, 'sv-mike')).toBeFocused();
	await page.keyboard.press('Space');
	await expectDisplayed(page, ['sv-alpha', 'sv-bravo']);
	await expect.poll(() => focusedRow(page)).toEqual({ rowId: 'sv-bravo', actionable: true });
	await expect(selectionCount(page)).toContainText('2 selected');

	// Last row leaves: focus falls back to the PREVIOUS surviving row.
	await checkbox(page, 'sv-bravo').focus();
	await page.keyboard.press('Space');
	await expectDisplayed(page, ['sv-alpha']);
	await expect.poll(() => focusedRow(page)).toEqual({ rowId: 'sv-alpha', actionable: true });

	// Only row leaves: focus lands on the Show control, visibly.
	await checkbox(page, 'sv-alpha').focus();
	await page.keyboard.press('Space');
	await expectDisplayed(page, []);
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(showControl(page)).toBeFocused();
	expect(await showControl(page).evaluate((node) => node.matches(':focus-visible'))).toBe(true);

	// Clear while focus sits inside a row it removes: same Show fallback.
	// (Programmatic click keeps focus in the row, like a keyboard shortcut
	// or assistive-tech activation would.)
	await chooseShow(page, 'All tracks');
	await selectTracks(page, ['sv-zulu', 'sv-delta']);
	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, ['sv-zulu', 'sv-delta']);
	await checkbox(page, 'sv-delta').focus();
	await expect(checkbox(page, 'sv-delta')).toBeFocused();
	await clearButton(page).evaluate((node: HTMLButtonElement) => node.click());
	await expectDisplayed(page, []);
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(showControl(page)).toBeFocused();

	// Clear from its own still-mounted button: focus is not stolen.
	await chooseShow(page, 'All tracks');
	await selectTracks(page, ['sv-zulu']);
	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, ['sv-zulu']);
	await clearButton(page).focus();
	await expect(clearButton(page)).toBeFocused();
	await page.keyboard.press('Enter');
	await expectDisplayed(page, []);
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(clearButton(page)).toBeFocused();
});

test('[SV-B-08] view, query and selection changes touch no queue, media, favorites, storage or network', async ({
	page
}) => {
	const h = await openLibrary(page);

	// Establish real state to protect: a two-track queue played forward,
	// one favorite, and a pending selection.
	await row(page, 'sv-zulu').click();
	await row(page, 'sv-bravo').click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Bravo Song');
	await row(page, 'sv-delta').getByRole('button', { name: 'Star Delta Song', exact: true }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await selectTracks(page, ['sv-delta', 'sv-mike']);

	const beforeQueue = await queueState(page);
	const beforeFavorites = await favoriteIds(page);
	expect(beforeFavorites).toEqual(['sv-delta']);
	const beforeRequests = [...h.requests];
	const beforeWrites = [...h.writes];
	const beforeStorageWrites = await storageWriteCount(page);

	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, ['sv-mike', 'sv-delta']);
	await findField(page).fill('delta');
	await expectDisplayed(page, ['sv-delta']);
	await page.getByRole('button', { name: 'Clear search', exact: true }).first().click();
	await orderControl(page).selectOption('title');
	await expectDisplayed(page, ['sv-delta', 'sv-mike']);
	await chooseShow(page, 'All tracks');
	await orderControl(page).selectOption('original');
	await expectDisplayed(page, ALL_IDS);

	expect(await queueState(page)).toEqual(beforeQueue);
	expect(await favoriteIds(page)).toEqual(beforeFavorites);
	await expect(row(page, 'sv-delta').getByRole('button', { name: /star|favorite/i })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	expect(h.requests, 'view changes must not refetch the library').toEqual(beforeRequests);
	expect(h.writes, 'view changes must not mutate over the network').toEqual(beforeWrites);
	expect(
		await storageWriteCount(page),
		'view changes must not write storage (no new persistence key)'
	).toBe(beforeStorageWrites);
	// Polite only: nothing assertive is announced from inside <main>.
	await expect(page.locator('main').locator('[aria-live="assertive"]')).toHaveCount(0);
	await expect(page.getByRole('alert')).toHaveCount(0);
});

test('[SV-B-09] a transient refresh keeps the Selected-only view; a successful one prunes vanished IDs', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	const h = await openLibrary(page, payload);
	// Activation order: zulu, mike, alpha. Loaded display: zulu, alpha, mike.
	await selectTracks(page, ['sv-zulu', 'sv-mike', 'sv-alpha']);
	await chooseShow(page, 'Selected only');
	await expectDisplayed(page, ['sv-zulu', 'sv-alpha', 'sv-mike']);

	h.state.abort = true;
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	await expectDisplayed(page, ['sv-zulu', 'sv-alpha', 'sv-mike']);
	await expect(showControl(page).locator('option:checked')).toHaveText('Selected only');
	await expect(selectionCount(page)).toContainText('3 selected');

	h.state.abort = false;
	payload.current = ITEMS.filter((item) => item.id !== 'sv-zulu');
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expectDisplayed(page, ['sv-alpha', 'sv-mike']);
	await expect(selectionCount(page)).toContainText('2 selected');
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);

	// Survivors keep their activation order for appending (mike before alpha).
	await appendButton(page).click();
	await expect.poll(() => queueOrder(page)).toEqual(['sv-mike', 'sv-alpha']);
});

test('[SV-B-10] account teardown purges the selection and resets the view for the next account', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await openLibrary(page, payload);
	await row(page, 'sv-zulu').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await selectTracks(page, ['sv-mike', 'sv-delta']);
	await chooseShow(page, 'Selected only');
	await findField(page).fill('mike');
	await expectDisplayed(page, ['sv-mike']);

	await fireAuthTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);

	payload.current = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry' }).click();
	await expectDisplayed(page, ['sv-kilo']);
	await expect(showControl(page).locator('option:checked')).toHaveText('All tracks');
	await expect(findField(page)).toHaveValue('');
	await expect(selectionCount(page)).toContainText('0 selected');
	await expect(checkbox(page, 'sv-kilo')).not.toBeChecked();
	await expect(appendButton(page)).toBeDisabled();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[SV-B-11] at 320px in light and dark, Selected only stays visible and operable', async ({
	page
}) => {
	await page.setViewportSize({ width: 320, height: 700 });
	await openLibrary(page);
	await selectTracks(page, ['sv-alpha', 'sv-mike']);

	for (const scheme of ['light', 'dark'] as const) {
		await page.emulateMedia({ colorScheme: scheme });
		await expect(showControl(page), `Show missing at 320px (${scheme})`).toBeVisible();
		await chooseShow(page, 'Selected only');
		await expectDisplayed(page, ['sv-alpha', 'sv-mike']);
		await expect(selectionCount(page)).toBeVisible();
		await expect(appendButton(page)).toBeVisible();
		await expect(clearButton(page)).toBeVisible();
		await expect(checkbox(page, 'sv-mike')).toBeVisible();
		// The controls this feature owns must fit the 320px viewport itself,
		// whatever unrelated shell chrome does around them.
		for (const [name, control] of [
			['Show', showControl(page)],
			['Append selected', appendButton(page)],
			['Clear selection', clearButton(page)],
			['selected row', row(page, 'sv-mike')]
		] as const) {
			const fits = await control.evaluate((element) => {
				const rect = element.getBoundingClientRect();
				return rect.left >= -1 && rect.right <= window.innerWidth + 1;
			});
			expect(fits, `${name} must fit 320px in ${scheme} mode`).toBe(true);
		}
		await chooseShow(page, 'All tracks');
		await expectDisplayed(page, ALL_IDS);
	}

	// And the narrow layout still appends the reviewed set in activation order.
	await chooseShow(page, 'Selected only');
	await appendButton(page).click();
	await expect.poll(() => queueOrder(page)).toEqual(['sv-alpha', 'sv-mike']);
});
