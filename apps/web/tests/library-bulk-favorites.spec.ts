import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * BULKSTAR acceptance bank — protected; the paired build owns the combined
 * GREEN ready PR. Do not change this file to accommodate an implementation,
 * or open a standalone ready PR containing the deliberate pre-feature
 * failures.
 *
 * From repo root (existing package scripts and mocked-browser harness):
 *   pnpm --dir apps/web check
 *   pnpm --dir apps/web test src/routes/library/bulk-favorites.acceptance.test.ts
 *   pnpm --dir apps/web test:e2e tests/library-bulk-favorites.spec.ts --workers=1
 * Omit an extra `--` with this pnpm version: otherwise the runners ignore the
 * focus/options and run the whole suite. Linux browsers may need the host's
 * installed Chromium libraries on LD_LIBRARY_PATH; do not replace the browser.
 * Regressions: same test:e2e script with tests/library-subset-append.spec.ts,
 * tests/library-favorites-local.spec.ts and
 * tests/library-favorites-management.spec.ts.
 *
 * Real /library, real selection, real favorites store and real queue; only
 * /api/library is mocked. No backend/provider/production requests.
 *
 * Hooks (new component-owned literals, same rule as `append-result`):
 * - `Star selected` and `Unstar selected` buttons (exact accessible names) in
 *   the existing loaded-library selection area: after the selection count,
 *   above the library list. Both target the current selected loaded IDs,
 *   including selected rows hidden by the find field or the transient view.
 *   While nothing is selected both are disabled and expose an understandable
 *   reason: a `title` matching /select/i, or an `aria-describedby` target that
 *   is visible and matches /select/i. No new select-all/select-displayed
 *   control anywhere; the bulk actions must not depend on one.
 * - `bulk-favorites-result` (`data-testid`, `role="status"`, polite): exactly
 *   one after an activation, reporting changed plus already-in-state counts,
 *   for example `Starred 2 tracks (0 already starred)` and
 *   `Unstarred 2 tracks (1 already unstarred)`. Tests match the numbers, not
 *   the exact plural copy. Never per-row chatter, never an alert.
 * - The existing `selection-count` and `selection-hidden-count` hooks keep
 *   reporting the explicit selected/hidden totals through every bulk action.
 *
 * Contract under test (paired build):
 * - Activation snapshots and deduplicates the selected IDs. Starring appends
 *   previously unstarred IDs once, in deterministic selection (activation)
 *   order, after the existing favorites. Unstarring removes only the targeted
 *   IDs, preserving unrelated and temporarily absent saved IDs and their
 *   relative order. Repeat activation is idempotent. Selection, queue order,
 *   current track and playback intent are preserved.
 * - The canonical account-scoped favorites store
 *   (`nntune.library.favorites.v1.<auth-context>`, JSON string array) and its
 *   purge semantics are reused: no new storage format or key family, no new
 *   backend request, telemetry or media activation. Denied/full storage and
 *   malformed old state fail soft with no false durable-save claim. An
 *   auth/account transition clears favorites, purges selection and invalidates
 *   pending bulk feedback; IDs never cross into another account.
 * - If Favorites-only hides unstarred rows, the bulk controls stay mounted and
 *   keyboard focus never drops to the body; the resulting visible/hidden
 *   selection count stays reported. Single-row star, shelf reorder/clear and
 *   refresh contracts are unchanged.
 *
 * Pre-feature expected RED: every `Star selected` / `Unstar selected` /
 * `bulk-favorites-result` visibility assertion, never setup errors. Each test
 * guards its first new-control interaction with an explicit visibility
 * assertion so a run without the controls fails as an assertion (missing
 * control), never as a click error. BULKSTAR-00 and BULKSTAR-14 are passing
 * controls for the existing selection/star/shelf/refresh behavior the build
 * must preserve.
 *
 * Breaks caught: missing/mislabeled bulk controls; controls outside the
 * selection area or below the rows; actionable empty state; hidden selected
 * rows skipped; duplicate or reordered stars; unrelated/absent favorites
 * dropped or reordered; non-idempotent repeat; selection/queue/playback
 * disturbed; new storage format or key; false saved claim on denied storage;
 * corrupt state crash; auth leakage or stale feedback; per-row chatter or
 * alert; focus lost when rows hide; new select-all dependency; network,
 * source-list or media side effects; broken single-row star, shelf
 * reorder/clear or refresh.
 */

// Titles/artists avoid "star", "favorite", "select", "append" and "clear" so
// name-filtered locators below can only match their intended control.
const ITEMS = [
	{ id: 'bulkstar-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'bulkstar-alpha', title: 'Alpha Song' },
	{ id: 'bulkstar-mike', title: 'Mike Song', artist: 'Harbor Lights' },
	{ id: 'bulkstar-bravo', title: 'Bravo Song', artist: 'Morning Band' }
];
const IDS = ['bulkstar-zulu', 'bulkstar-alpha', 'bulkstar-mike', 'bulkstar-bravo'];
const GHOST = 'bulkstar-ghost-absent-id';

const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';
const AUTH_TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const FAVORITES_KEY_BASE = 'nntune.library.favorites.v1';

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
}

function checkbox(page: Page, id: string): Locator {
	return row(page, id).getByRole('checkbox');
}

function starButton(page: Page, id: string): Locator {
	return row(page, id).getByRole('button', { name: /star|favorite/i });
}

function starSelected(page: Page): Locator {
	return page.getByRole('button', { name: 'Star selected', exact: true });
}

function unstarSelected(page: Page): Locator {
	return page.getByRole('button', { name: 'Unstar selected', exact: true });
}

function bulkResult(page: Page): Locator {
	return page.getByTestId('bulk-favorites-result');
}

function favoriteItem(page: Page, id: string): Locator {
	return page.locator(`[data-testid="favorite-item"][data-id="${id}"]`);
}

function viewSelect(page: Page): Locator {
	return page.getByRole('combobox', { name: /show|filter|view/i });
}

async function seedLibrary(page: Page, items: unknown[] = ITEMS): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(items) })
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(items.length);
}

async function renderedIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('library-item')
		.evaluateAll((rows) => rows.map((item) => item.getAttribute('data-id')));
}

async function shelfIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('favorite-item')
		.evaluateAll((rows) => rows.map((item) => item.getAttribute('data-id')));
}

async function queueIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((rows) =>
			rows.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
}

async function queueCurrentId(page: Page): Promise<string | null> {
	return page
		.getByTestId('queue-item')
		.evaluateAll(
			(rows) =>
				rows
					.filter((item) => item.getAttribute('aria-current') === 'true')
					.map(
						(item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id')
					)
		)
		.then((ids) => ids[0] ?? null);
}

async function readAuthContext(page: Page): Promise<string | null> {
	return page.evaluate((key) => window.sessionStorage.getItem(key), AUTH_CONTEXT_KEY);
}

async function readFavoritesRaw(page: Page, context: string | null): Promise<string | null> {
	return page.evaluate(
		({ base, ctx }) => (ctx ? window.localStorage.getItem(`${base}.${ctx}`) : null),
		{ base: FAVORITES_KEY_BASE, ctx: context }
	);
}

async function storedFavorites(page: Page, context: string): Promise<string[]> {
	const raw = await readFavoritesRaw(page, context);
	if (!raw) return [];
	return (JSON.parse(raw) as unknown[]).filter(
		(value): value is string => typeof value === 'string' && value.length > 0
	);
}

async function storageKeys(page: Page): Promise<string[]> {
	return page.evaluate(() => Object.keys(window.localStorage).sort());
}

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-bulkstar-auth-transition');
		window.localStorage.removeItem(key);
	}, AUTH_TRANSITION_KEY);
	await otherTab.close();
}

async function requireBulkControls(page: Page): Promise<void> {
	await expect(starSelected(page), 'Star selected missing').toBeVisible();
	await expect(unstarSelected(page), 'Unstar selected missing').toBeVisible();
}

async function expectDisabledWithReason(button: Locator): Promise<void> {
	await expect(button).toBeDisabled();
	const title = await button.getAttribute('title');
	if (title && /select/i.test(title)) return;
	const describedBy = await button.getAttribute('aria-describedby');
	expect(
		describedBy,
		'disabled bulk control must name its reason via title or aria-describedby'
	).not.toBeNull();
	const hint = button.page().locator(`#${describedBy!.split(/\s+/)[0]}`);
	await expect(hint, 'disabled reason must be visible').toBeVisible();
	await expect(hint).toContainText(/select/i);
}

async function expectBulkResult(
	page: Page,
	verb: 'Starred' | 'Unstarred',
	changed: number,
	already: number
): Promise<void> {
	const result = bulkResult(page);
	await expect(result, 'bulk favorites result missing').toBeVisible();
	await expect(result).toHaveAttribute('role', 'status');
	await expect(result).toContainText(`${verb} ${changed}`);
	const alreadyWord = verb === 'Starred' ? 'already starred' : 'already unstarred';
	await expect(result).toContainText(new RegExp(`${already} ${alreadyWord}`, 'i'));
	// One aggregate outcome, never per-row chatter.
	await expect(
		page.getByRole('status').filter({ hasText: /already starred|already unstarred/i }),
		'exactly one aggregate bulk outcome'
	).toHaveCount(1);
	await expect(page.getByRole('alert')).toHaveCount(0);
}

// Fail only the favorites boundary, never auth-context/snapshot storage.
async function failFavoritesStorage(page: Page): Promise<void> {
	await page.evaluate((base) => {
		const setItem = Storage.prototype.setItem;
		Storage.prototype.setItem = function (key, value) {
			if (this === window.localStorage && key.startsWith(`${base}.`)) {
				throw new DOMException('Favorites writes unavailable', 'QuotaExceededError');
			}
			setItem.call(this, key, value);
		};
		Reflect.set(window, '__restoreBulkstarStorage', () => {
			Storage.prototype.setItem = setItem;
		});
	}, FAVORITES_KEY_BASE);
	const context = await readAuthContext(page);
	expect(context, 'favorites faults must leave the session auth context available').not.toBeNull();
}

async function restoreFavoritesStorage(page: Page): Promise<void> {
	await page.evaluate(() => Reflect.get(window, '__restoreBulkstarStorage')());
}

test.beforeEach(async ({ context }) => {
	// Unmocked API calls are refused rather than touching a backend; per-page
	// library mocks take precedence (Playwright routes are LIFO).
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname))
			return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/'))
			return route.fulfill({
				status: 404,
				contentType: 'application/json',
				body: '{}'
			});
		return route.continue();
	});
});

test('BULKSTAR-00 control: selection, single-row star, append and queue already work', async ({
	page
}) => {
	await seedLibrary(page);
	expect(await renderedIds(page)).toEqual(IDS);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	await checkbox(page, 'bulkstar-mike').check();
	await checkbox(page, 'bulkstar-zulu').check();
	await expect(page.getByTestId('selection-count')).toContainText('2 selected');

	await page.getByRole('button', { name: 'Append selected', exact: true }).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueIds(page)).toEqual(['bulkstar-mike', 'bulkstar-zulu']);

	await starButton(page, 'bulkstar-alpha').click();
	await expect(starButton(page, 'bulkstar-alpha')).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(page.locator('audio')).toHaveCount(0);
});

test('BULKSTAR-01 bulk controls live in the selection area, disabled with reason while empty', async ({
	page
}) => {
	await seedLibrary(page);
	await requireBulkControls(page);

	await expect(page.getByTestId('selection-count')).toContainText('0 selected');
	await expectDisabledWithReason(starSelected(page));
	await expectDisabledWithReason(unstarSelected(page));

	// Selection area, not below the rows.
	const starBox = await starSelected(page).boundingBox();
	const listBox = await page.getByTestId('library-list').boundingBox();
	expect(starBox, 'Star selected must have a box').not.toBeNull();
	expect(listBox, 'library list must have a box').not.toBeNull();
	expect(starBox!.y + starBox!.height).toBeLessThanOrEqual(listBox!.y);

	// Enablement follows the set, and no select-all escape hatch appears.
	await checkbox(page, 'bulkstar-mike').check();
	await expect(page.getByTestId('selection-count')).toContainText('1 selected');
	await expect(starSelected(page)).toBeEnabled();
	await expect(unstarSelected(page)).toBeEnabled();
	await checkbox(page, 'bulkstar-mike').uncheck();
	await expect(page.getByTestId('selection-count')).toContainText('0 selected');
	await expectDisabledWithReason(starSelected(page));
	await expectDisabledWithReason(unstarSelected(page));

	await expect(
		page.getByRole('button', { name: /select all|select displayed/i })
	).toHaveCount(0);
	await expect(
		page.getByRole('checkbox', { name: /select all|select displayed/i })
	).toHaveCount(0);
	await expect(
		page.getByRole('link', { name: /select all|select displayed/i })
	).toHaveCount(0);
	expect(page.url()).toContain('/library');
});

test('BULKSTAR-02 starring appends unstarred IDs once in selection order after existing favorites', async ({
	page
}) => {
	await page.addInitScript(
		({ contextKey, base, ghost }) => {
			if (window.sessionStorage.getItem('test.bulkstar.seeded') !== 'yes') {
				window.sessionStorage.setItem(contextKey, 'bulkstar-account');
				window.localStorage.setItem(
					`${base}.bulkstar-account`,
					JSON.stringify(['bulkstar-mike', ghost])
				);
				window.sessionStorage.setItem('test.bulkstar.seeded', 'yes');
			}
		},
		{ contextKey: AUTH_CONTEXT_KEY, base: FAVORITES_KEY_BASE, ghost: GHOST }
	);
	await seedLibrary(page);
	await requireBulkControls(page);

	// Ghost stays stored but unlisted: no title to show for it.
	expect(await storedFavorites(page, 'bulkstar-account')).toEqual([
		'bulkstar-mike',
		GHOST
	]);
	expect(await shelfIds(page)).toEqual(['bulkstar-mike']);

	// Activation order is alpha then zulu, against rendered order.
	await checkbox(page, 'bulkstar-alpha').check();
	await checkbox(page, 'bulkstar-zulu').check();
	await starSelected(page).click();

	await expectBulkResult(page, 'Starred', 2, 0);
	expect(await storedFavorites(page, 'bulkstar-account')).toEqual([
		'bulkstar-mike',
		GHOST,
		'bulkstar-alpha',
		'bulkstar-zulu'
	]);
	expect(await shelfIds(page)).toEqual(['bulkstar-mike', 'bulkstar-alpha', 'bulkstar-zulu']);
	await expect(starButton(page, 'bulkstar-alpha')).toHaveAttribute('aria-pressed', 'true');
	await expect(starButton(page, 'bulkstar-zulu')).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('selection-count')).toContainText('2 selected');
	await expect(checkbox(page, 'bulkstar-alpha')).toBeChecked();
	await expect(checkbox(page, 'bulkstar-zulu')).toBeChecked();
});

test('BULKSTAR-03 unstarring removes only targeted IDs and keeps unrelated, absent and order', async ({
	page
}) => {
	await page.addInitScript(
		({ contextKey, base, ghost }) => {
			if (window.sessionStorage.getItem('test.bulkstar.seeded') !== 'yes') {
				window.sessionStorage.setItem(contextKey, 'bulkstar-account');
				window.localStorage.setItem(
					`${base}.bulkstar-account`,
					JSON.stringify(['bulkstar-alpha', ghost, 'bulkstar-mike', 'bulkstar-zulu'])
				);
				window.sessionStorage.setItem('test.bulkstar.seeded', 'yes');
			}
		},
		{ contextKey: AUTH_CONTEXT_KEY, base: FAVORITES_KEY_BASE, ghost: GHOST }
	);
	await seedLibrary(page);
	await requireBulkControls(page);
	expect(await shelfIds(page)).toEqual(['bulkstar-alpha', 'bulkstar-mike', 'bulkstar-zulu']);

	await checkbox(page, 'bulkstar-mike').check();
	await checkbox(page, 'bulkstar-zulu').check();
	await unstarSelected(page).click();

	await expectBulkResult(page, 'Unstarred', 2, 0);
	expect(await storedFavorites(page, 'bulkstar-account')).toEqual([
		'bulkstar-alpha',
		GHOST
	]);
	expect(await shelfIds(page)).toEqual(['bulkstar-alpha']);
	await expect(starButton(page, 'bulkstar-mike')).toHaveAttribute('aria-pressed', 'false');
	await expect(starButton(page, 'bulkstar-zulu')).toHaveAttribute('aria-pressed', 'false');
	await expect(page.getByTestId('selection-count')).toContainText('2 selected');
});

test('BULKSTAR-04 partial sets report changed plus already-in-state counts', async ({
	page
}) => {
	await seedLibrary(page);
	await requireBulkControls(page);

	await starButton(page, 'bulkstar-mike').click();
	await expect(starButton(page, 'bulkstar-mike')).toHaveAttribute('aria-pressed', 'true');

	await checkbox(page, 'bulkstar-mike').check();
	await checkbox(page, 'bulkstar-zulu').check();
	await starSelected(page).click();
	await expectBulkResult(page, 'Starred', 1, 1);

	const context = await readAuthContext(page);
	expect(context).not.toBeNull();
	expect(await storedFavorites(page, context!)).toEqual([
		'bulkstar-mike',
		'bulkstar-zulu'
	]);

	await checkbox(page, 'bulkstar-alpha').check();
	await unstarSelected(page).click();
	await expectBulkResult(page, 'Unstarred', 2, 1);
	expect(await storedFavorites(page, context!)).toEqual([]);
});

test('BULKSTAR-05 repeat activation is idempotent and preserves selection, queue and playback', async ({
	page
}) => {
	await seedLibrary(page);
	await requireBulkControls(page);

	await row(page, 'bulkstar-mike').click();
	await row(page, 'bulkstar-alpha').click();
	await row(page, 'bulkstar-zulu').click();
	await page.getByTestId('player-next').click();
	expect(await queueIds(page)).toEqual(['bulkstar-mike', 'bulkstar-alpha', 'bulkstar-zulu']);
	expect(await queueCurrentId(page)).toBe('bulkstar-alpha');
	const nowPlaying = await page.getByTestId('now-playing').textContent();

	await checkbox(page, 'bulkstar-bravo').check();
	await checkbox(page, 'bulkstar-zulu').check();
	await starSelected(page).click();
	await expectBulkResult(page, 'Starred', 2, 0);
	const context = await readAuthContext(page);
	expect(context).not.toBeNull();
	const afterFirst = await storedFavorites(page, context!);

	await starSelected(page).click();
	await expectBulkResult(page, 'Starred', 0, 2);
	expect(await storedFavorites(page, context!)).toEqual(afterFirst);
	expect(new Set(afterFirst).size, 'stored favorites must never duplicate').toBe(
		afterFirst.length
	);

	await unstarSelected(page).click();
	await expectBulkResult(page, 'Unstarred', 2, 0);
	await unstarSelected(page).click();
	await expectBulkResult(page, 'Unstarred', 0, 2);
	expect(await storedFavorites(page, context!)).toEqual([]);

	// Selection, queue arrangement, current track and playback intent survive.
	await expect(page.getByTestId('selection-count')).toContainText('2 selected');
	await expect(checkbox(page, 'bulkstar-bravo')).toBeChecked();
	await expect(checkbox(page, 'bulkstar-zulu')).toBeChecked();
	expect(await queueIds(page)).toEqual(['bulkstar-mike', 'bulkstar-alpha', 'bulkstar-zulu']);
	expect(await queueCurrentId(page)).toBe('bulkstar-alpha');
	await expect(page.getByTestId('now-playing')).toHaveText(nowPlaying ?? '');
	await expect(page.locator('audio')).toHaveCount(0);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	expect(await renderedIds(page)).toEqual(IDS);
});

test('BULKSTAR-06 hidden selected rows are targeted with an explicit hidden count', async ({
	page
}) => {
	await seedLibrary(page);
	await requireBulkControls(page);

	await checkbox(page, 'bulkstar-mike').check();
	await checkbox(page, 'bulkstar-zulu').check();
	await page.getByLabel('Find in loaded tracks').fill('zulu');
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	const hidden = page.getByTestId('selection-hidden-count');
	await expect(hidden, 'hidden selected count must stay explicit').toBeVisible();
	await expect(hidden).toContainText(/1 selected/);
	await expect(page.getByTestId('selection-count')).toContainText('2 selected');

	await starSelected(page).click();
	await expectBulkResult(page, 'Starred', 2, 0);
	const context = await readAuthContext(page);
	expect(context).not.toBeNull();
	expect(await storedFavorites(page, context!)).toEqual([
		'bulkstar-mike',
		'bulkstar-zulu'
	]);

	await page.getByRole('button', { name: 'Clear search', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(starButton(page, 'bulkstar-mike')).toHaveAttribute('aria-pressed', 'true');
	await expect(starButton(page, 'bulkstar-zulu')).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('selection-hidden-count')).toHaveCount(0);
});

test('BULKSTAR-07 Favorites-only unstar keeps controls and focus usable with visible counts', async ({
	page
}) => {
	await seedLibrary(page);
	await requireBulkControls(page);

	await checkbox(page, 'bulkstar-mike').check();
	await checkbox(page, 'bulkstar-zulu').check();
	await starSelected(page).click();
	await expectBulkResult(page, 'Starred', 2, 0);

	await viewSelect(page).selectOption({ label: 'Favorites only' });
	await expect(page.getByTestId('library-item')).toHaveCount(2);

	await unstarSelected(page).focus();
	await expect(unstarSelected(page)).toBeFocused();
	await page.keyboard.press('Enter');

	await expectBulkResult(page, 'Unstarred', 2, 0);
	await expect(
		page.getByText(/no favorites in this list/i),
		'Favorites-only must report the emptied view honestly'
	).toBeVisible();
	await expect(
		await page.evaluate(() => document.activeElement === document.body),
		'keyboard focus must never drop to the body when rows hide'
	).toBe(false);
	await expect(starSelected(page), 'bulk controls must stay mounted').toBeVisible();
	await expect(unstarSelected(page), 'bulk controls must stay mounted').toBeVisible();
	await expect(page.getByTestId('selection-count')).toContainText('2 selected');
	await expect(page.getByTestId('selection-hidden-count')).toContainText(/2 selected/);

	await viewSelect(page).selectOption({ label: 'All tracks' });
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(starButton(page, 'bulkstar-mike')).toHaveAttribute('aria-pressed', 'false');
	await expect(starButton(page, 'bulkstar-zulu')).toHaveAttribute('aria-pressed', 'false');
});

test('BULKSTAR-08 Tab reaches both bulk controls with visible focus; Enter and Space activate', async ({
	page
}) => {
	await seedLibrary(page);
	await requireBulkControls(page);

	await checkbox(page, 'bulkstar-mike').check();
	await checkbox(page, 'bulkstar-alpha').check();

	for (const [control, key] of [
		[starSelected(page), 'Enter'],
		[unstarSelected(page), 'Space']
	] as const) {
		for (
			let tabs = 0;
			tabs < 40 &&
			!(await control.evaluate((element) => element === document.activeElement));
			tabs += 1
		) {
			await page.keyboard.press('Tab');
		}
		await expect(control, 'bulk control must be Tab-reachable').toBeFocused();
		const focus = await control.evaluate((element) => {
			const style = getComputedStyle(element);
			return {
				outline: style.outlineStyle,
				width: Number.parseFloat(style.outlineWidth),
				shadow: style.boxShadow
			};
		});
		expect(
			(focus.outline !== 'none' && focus.width > 0) || focus.shadow !== 'none',
			'keyboard focus needs a visible ring'
		).toBe(true);
		await page.keyboard.press(key);
	}

	await expectBulkResult(page, 'Unstarred', 2, 0);
	const context = await readAuthContext(page);
	expect(context).not.toBeNull();
	expect(await storedFavorites(page, context!)).toEqual([]);
});

test.describe('touch and narrow viewport', () => {
	test.use({ hasTouch: true });

	test('BULKSTAR-09 320px light/dark: bulk controls stay visible and touch-operable with no overflow', async ({
		page
	}) => {
		await page.setViewportSize({ width: 320, height: 700 });
		await seedLibrary(page);
		await requireBulkControls(page);

		await checkbox(page, 'bulkstar-mike').check();
		await checkbox(page, 'bulkstar-zulu').check();

		for (const scheme of ['light', 'dark'] as const) {
			await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
			for (const control of [starSelected(page), unstarSelected(page)]) {
				await expect(control, `bulk control must stay visible in ${scheme}`).toBeVisible();
				const box = await control.boundingBox();
				expect(box, 'bulk control must have a touch target').not.toBeNull();
				expect(box!.x).toBeGreaterThanOrEqual(0);
				expect(box!.x + box!.width).toBeLessThanOrEqual(320);
				expect(box!.height, 'touch target needs a usable height').toBeGreaterThanOrEqual(24);
				expect(box!.width, 'touch target needs a usable width').toBeGreaterThanOrEqual(24);
			}
			await starSelected(page).tap();
			await expectBulkResult(page, 'Starred', 2, 0);
			await unstarSelected(page).tap();
			await expectBulkResult(page, 'Unstarred', 2, 0);
			expect(
				await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
				`no horizontal overflow at 320px in ${scheme}`
			).toBe(true);
		}
	});
});

test('BULKSTAR-10 denied storage fails soft with no false durable-save claim, then recovers', async ({
	page
}) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await seedLibrary(page);
	await requireBulkControls(page);

	const context = await readAuthContext(page);
	expect(context).not.toBeNull();
	expect(await readFavoritesRaw(page, context)).toBeNull();

	await checkbox(page, 'bulkstar-mike').check();
	await checkbox(page, 'bulkstar-zulu').check();
	await failFavoritesStorage(page);

	await starSelected(page).click();
	await expectBulkResult(page, 'Starred', 2, 0);
	await expect(starButton(page, 'bulkstar-mike')).toHaveAttribute('aria-pressed', 'true');
	await expect(starButton(page, 'bulkstar-zulu')).toHaveAttribute('aria-pressed', 'true');
	await expect(bulkResult(page)).not.toContainText(/saved|persisted/i);
	expect(
		await readFavoritesRaw(page, context),
		'denied write must not invent stored state'
	).toBeNull();
	expect(pageErrors, 'denied storage must fail soft, never throw').toEqual([]);

	await restoreFavoritesStorage(page);
	await unstarSelected(page).click();
	await expectBulkResult(page, 'Unstarred', 2, 0);
	expect(await readFavoritesRaw(page, context)).toBe('[]');
	expect(pageErrors).toEqual([]);
});

test('BULKSTAR-11 malformed stored favorites fall back to empty without breaking bulk actions', async ({
	page
}) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (error) => pageErrors.push(error.message));
	await page.addInitScript(
		({ contextKey, base }) => {
			if (window.sessionStorage.getItem('test.bulkstar.seeded') !== 'yes') {
				window.sessionStorage.setItem(contextKey, 'bulkstar-account');
				window.localStorage.setItem(`${base}.bulkstar-account`, '{not-json');
				window.sessionStorage.setItem('test.bulkstar.seeded', 'yes');
			}
		},
		{ contextKey: AUTH_CONTEXT_KEY, base: FAVORITES_KEY_BASE }
	);
	await seedLibrary(page);
	await requireBulkControls(page);

	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);

	await checkbox(page, 'bulkstar-mike').check();
	await checkbox(page, 'bulkstar-zulu').check();
	await starSelected(page).click();
	await expectBulkResult(page, 'Starred', 2, 0);
	expect(await storedFavorites(page, 'bulkstar-account')).toEqual([
		'bulkstar-mike',
		'bulkstar-zulu'
	]);
	expect(pageErrors).toEqual([]);
});

test('BULKSTAR-12 auth transition purges favorites and feedback; IDs never cross accounts', async ({
	page
}) => {
	await page.addInitScript(
		({ contextKey, base }) => {
			if (window.sessionStorage.getItem('test.bulkstar.seeded') !== 'yes') {
				window.sessionStorage.setItem(contextKey, 'bulkstar-account');
				window.localStorage.setItem(
					`${base}.other-account`,
					JSON.stringify(['other-private-id'])
				);
				window.localStorage.setItem('test.unrelated.preference', 'unchanged');
				window.sessionStorage.setItem('test.bulkstar.seeded', 'yes');
			}
		},
		{ contextKey: AUTH_CONTEXT_KEY, base: FAVORITES_KEY_BASE }
	);
	await seedLibrary(page);
	await requireBulkControls(page);

	await checkbox(page, 'bulkstar-mike').check();
	await checkbox(page, 'bulkstar-zulu').check();
	await starSelected(page).click();
	await expectBulkResult(page, 'Starred', 2, 0);
	expect(await storedFavorites(page, 'bulkstar-account')).toEqual([
		'bulkstar-mike',
		'bulkstar-zulu'
	]);

	await fireAuthTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	expect(
		await readFavoritesRaw(page, 'bulkstar-account'),
		'previous-account favorites must be cleared at the auth boundary'
	).toBeNull();
	await expect(
		bulkResult(page),
		'pending bulk feedback must not survive the account boundary'
	).toHaveCount(0);
	await expect(page.getByTestId('selection-count')).toContainText('0 selected');

	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expectDisabledWithReason(starSelected(page));
	await expectDisabledWithReason(unstarSelected(page));

	await checkbox(page, 'bulkstar-alpha').check();
	await starSelected(page).click();
	await expectBulkResult(page, 'Starred', 1, 0);
	const nextContext = await readAuthContext(page);
	expect(nextContext).not.toBeNull();
	expect(nextContext).not.toBe('bulkstar-account');
	expect(await storedFavorites(page, nextContext!)).toEqual(['bulkstar-alpha']);
	expect(
		await readFavoritesRaw(page, 'bulkstar-account'),
		'old IDs must never reappear in another account'
	).toBeNull();
	expect(
		await page.evaluate(
			(base) => window.localStorage.getItem(`${base}.other-account`),
			FAVORITES_KEY_BASE
		),
		'other accounts stay untouched'
	).toBe('["other-private-id"]');
	expect(
		await page.evaluate(() => window.localStorage.getItem('test.unrelated.preference'))
	).toBe('unchanged');
});

test('BULKSTAR-13 bulk star and unstar issue zero network, source, queue or media effects', async ({
	page
}) => {
	const mutations: string[] = [];
	const media: string[] = [];
	let reads = 0;
	page.on('request', (request) => {
		if (request.method() !== 'GET')
			mutations.push(`${request.method()} ${new URL(request.url()).pathname}`);
		if (request.resourceType() === 'media') media.push(request.url());
		if (new URL(request.url()).pathname === '/api/library') reads += 1;
	});
	await seedLibrary(page);
	await requireBulkControls(page);
	expect(reads).toBe(1);
	const keysBefore = await storageKeys(page);

	await row(page, 'bulkstar-mike').click();
	await row(page, 'bulkstar-alpha').click();
	await page.getByTestId('player-next').click();
	const nowPlaying = await page.getByTestId('now-playing').textContent();

	await checkbox(page, 'bulkstar-zulu').check();
	await checkbox(page, 'bulkstar-bravo').check();
	await starSelected(page).click();
	await expectBulkResult(page, 'Starred', 2, 0);
	await unstarSelected(page).click();
	await expectBulkResult(page, 'Unstarred', 2, 0);

	expect(mutations, 'bulk favorites must not mutate over the network').toEqual([]);
	expect(media, 'bulk favorites must not activate media').toEqual([]);
	expect(reads, 'bulk favorites must not refetch the library').toBe(1);
	expect(await renderedIds(page), 'loaded source order is untouched').toEqual(IDS);
	expect(await queueIds(page)).toEqual(['bulkstar-mike', 'bulkstar-alpha']);
	expect(await queueCurrentId(page)).toBe('bulkstar-alpha');
	await expect(page.getByTestId('now-playing')).toHaveText(nowPlaying ?? '');
	await expect(page.locator('audio')).toHaveCount(0);

	const keysAfter = await storageKeys(page);
	const added = keysAfter.filter((key) => !keysBefore.includes(key));
	expect(added, 'no new storage format or key family').toEqual([]);
	const context = await readAuthContext(page);
	expect(context).not.toBeNull();
	const raw = await readFavoritesRaw(page, context);
	expect(raw, 'canonical store keeps a JSON array value').not.toBeNull();
	expect(() => JSON.parse(raw!) as unknown[]).not.toThrow();
});

test('BULKSTAR-14 regression: single-row star, shelf reorder/clear and refresh are unchanged', async ({
	page
}) => {
	await seedLibrary(page);

	// Single-row star toggles and never selects.
	await starButton(page, 'bulkstar-mike').click();
	await starButton(page, 'bulkstar-alpha').click();
	await expect(starButton(page, 'bulkstar-mike')).toHaveAttribute('aria-pressed', 'true');
	expect(await shelfIds(page)).toEqual(['bulkstar-mike', 'bulkstar-alpha']);
	await expect(checkbox(page, 'bulkstar-mike')).not.toBeChecked();
	await expect(page.getByTestId('selection-count')).toContainText('0 selected');

	// Shelf reorder keeps star order and queue untouched.
	await page
		.getByRole('button', { name: 'Move favorite Alpha Song up', exact: true })
		.click();
	expect(await shelfIds(page)).toEqual(['bulkstar-alpha', 'bulkstar-mike']);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// Shelf unstar removes one row and reports politely.
	await favoriteItem(page, 'bulkstar-mike').getByRole('button', {
		name: /unstar mike song/i
	}).click();
	expect(await shelfIds(page)).toEqual(['bulkstar-alpha']);
	await expect(page.getByTestId('favorites-section').getByRole('status')).toContainText(
		/removed/i
	);

	// Clear cancellation keeps, confirmation empties through the same saver.
	await page.getByRole('button', { name: 'Clear local favorites', exact: true }).click();
	await page.getByRole('button', { name: 'Cancel', exact: true }).click();
	expect(await shelfIds(page)).toEqual(['bulkstar-alpha']);
	await page.getByRole('button', { name: 'Clear local favorites', exact: true }).click();
	await page
		.getByRole('button', { name: 'Yes, clear local favorites', exact: true })
		.click();
	await expect(page.getByTestId('favorites-empty')).toBeVisible();

	// A failed refresh keeps the loaded list and its controls operable.
	await page.unroute('**/api/library');
	await page.route('**/api/library', (route) => route.abort('failed'));
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-stale-badge')).toBeVisible();
	expect(await renderedIds(page)).toEqual(IDS);
	await checkbox(page, 'bulkstar-zulu').check();
	await expect(page.getByTestId('selection-count')).toContainText('1 selected');
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
});
