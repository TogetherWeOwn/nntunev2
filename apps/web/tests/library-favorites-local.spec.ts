import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: star favorite library tracks locally (device-local Favorites).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * favorites implementation on top of this branch and must make these tests
 * pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance lane instead of being fixed beside the implementation.
 *
 * Harness note: `/library` is client-fetched (`ssr = false`, component
 * fetches `/api/library` on mount) and the vitest setup in this package is
 * SSR-render/source-pin only — there is no DOM component-test environment.
 * The offline Playwright library specs (`library-cache.spec.ts`,
 * `library-selectable.spec.ts`) are the interactive harness for this page,
 * so the star/keyboard contract is pinned here with a route-mocked
 * `/api/library`. Fully offline and deterministic: no backend, no provider.
 *
 * Contract under test:
 * - Each library row carries a labeled star/unstar toggle: a `button` whose
 *   accessible name matches `/star|favorite/i`, with `aria-pressed`
 *   reflecting state, reachable by Tab and operable with Enter and Space.
 *   Toggling never queues, plays, or otherwise mutates the queue.
 * - A Favorites section (`data-testid="favorites-section"`) lists starred
 *   tracks as `data-testid="favorite-item"` elements with `data-id` and
 *   visible title/artist text. Clicking a favorite queues it, exactly like
 *   clicking a library row. With nothing starred the section shows an
 *   honest empty state (`data-testid="favorites-empty"`) instead of items.
 * - Favorites persist device-locally in `localStorage` under
 *   `nntune.library.favorites.v1.<auth-context>` (JSON array of track-id
 *   strings), where `<auth-context>` is the session
 *   `nntune.library.auth-context.v1` value — the same scoping discipline
 *   as the snapshot keys. Favorites survive reload, never leak across
 *   accounts (an auth transition clears them, verified by storage read),
 *   and a corrupt stored value falls back to empty without breaking the
 *   list. No starring flow issues a non-GET network request.
 *
 * Case IDs (stable): FAV-01 click toggle, FAV-02 keyboard toggle, FAV-03
 * section add/remove + empty state, FAV-04 reload persistence, FAV-05
 * auth-transition scoping, FAV-06 corrupt-storage fallback, FAV-07
 * favorites queue-add + dedupe, FAV-08 zero network mutation. The unit
 * half (`favorites-local-acceptance.test.ts`) pins the same storage keys
 * at the `invalidateLibraryCache` boundary as FAV-U01..U03.
 */

// Fixture titles/artists deliberately avoid the words "star" and
// "favorite" so the star-toggle name filter below can only match the
// toggle itself, never the row button.
const ITEMS = [
	{ id: 'fav-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'fav-alpha', title: 'Alpha Song' },
	{ id: 'fav-mike', title: 'Mike Song', artist: 'Harbor Lights' }
];

const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';
const AUTH_TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const FAVORITES_KEY_BASE = 'nntune.library.favorites.v1';

async function seedLibrary(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		})
	);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
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

/**
 * The favorite's queue affordance: a descendant button/option when the
 * build nests one, else the item itself (mirrors the row-control read in
 * the row-selection spec).
 */
async function favoriteControl(page: Page, id: string): Promise<Locator> {
	const inner = favoriteItem(page, id).locator('button, [role="button"], [role="option"]').first();
	return (await inner.count()) > 0 ? inner : favoriteItem(page, id);
}

async function queueIds(page: Page): Promise<(string | null)[]> {
	return page
		.getByTestId('queue-item')
		.evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-track-id') ?? item.getAttribute('data-id'))
		);
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

function storedIds(raw: string | null): string[] {
	if (!raw) return [];
	return (JSON.parse(raw) as unknown[]).filter(
		(value): value is string => typeof value === 'string' && value.length > 0
	);
}

test('[FAV-01] clicking a star toggle flips aria-pressed without touching the queue', async ({
	page
}) => {
	await seedLibrary(page);

	const star = starButton(page, 'fav-mike');
	await expect(star, 'star toggle missing on the row').toHaveAttribute('aria-pressed', 'false');

	await star.click();
	await expect(star).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');

	await star.click();
	await expect(star).toHaveAttribute('aria-pressed', 'false');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// Toggling is idempotent: a third click returns to starred, still queue-free.
	await star.click();
	await expect(star).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[FAV-02] Enter and Space toggle a focused star button', async ({ page }) => {
	await seedLibrary(page);

	const star = starButton(page, 'fav-alpha');
	await star.focus();
	await expect(star, 'star toggle must be keyboard-focusable').toBeFocused();

	await page.keyboard.press('Enter');
	await expect(star).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	await page.keyboard.press('Space');
	await expect(star).toHaveAttribute('aria-pressed', 'false');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('[FAV-03] Favorites section adds and removes starred tracks with an honest empty state', async ({
	page
}) => {
	await seedLibrary(page);

	const section = page.getByTestId('favorites-section');
	await expect(section, 'Favorites section missing').toBeVisible();
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);

	await starButton(page, 'fav-zulu').click();
	await starButton(page, 'fav-mike').click();

	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await expect(favoriteItem(page, 'fav-zulu')).toContainText('Zulu Song');
	await expect(favoriteItem(page, 'fav-zulu')).toContainText('Harbor Lights');
	await expect(favoriteItem(page, 'fav-mike')).toContainText('Mike Song');
	await expect(page.getByTestId('favorites-empty')).toHaveCount(0);

	await starButton(page, 'fav-zulu').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	await expect(favoriteItem(page, 'fav-mike')).toBeVisible();

	await starButton(page, 'fav-mike').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
});

test('[FAV-04] starred tracks survive reload via scoped local storage', async ({ page }) => {
	await seedLibrary(page);

	await starButton(page, 'fav-mike').click();
	await starButton(page, 'fav-alpha').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);

	const context = await readAuthContext(page);
	expect(context, 'library auth context must exist after a successful load').not.toBeNull();
	expect(storedIds(await readFavoritesRaw(page, context))).toEqual(
		expect.arrayContaining(['fav-mike', 'fav-alpha'])
	);

	await page.reload();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);

	await expect(starButton(page, 'fav-mike')).toHaveAttribute('aria-pressed', 'true');
	await expect(starButton(page, 'fav-alpha')).toHaveAttribute('aria-pressed', 'true');
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page)))).toEqual(
		expect.arrayContaining(['fav-mike', 'fav-alpha'])
	);
});

test('[FAV-05] an auth transition clears favorites so the next account starts empty', async ({
	page
}) => {
	await seedLibrary(page);

	await starButton(page, 'fav-zulu').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	const contextBefore = await readAuthContext(page);
	expect(contextBefore).not.toBeNull();
	expect(storedIds(await readFavoritesRaw(page, contextBefore))).toContain('fav-zulu');

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-favorites-auth-transition');
		window.localStorage.removeItem(key);
	}, AUTH_TRANSITION_KEY);

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	expect(
		await readFavoritesRaw(page, contextBefore),
		'previous-account favorites must be cleared at the auth boundary'
	).toBeNull();
	await otherTab.close();

	// The next account refetches and starts with an empty Favorites section.
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
});

test('[FAV-06] corrupt favorites storage falls back to empty without breaking the list', async ({
	page
}) => {
	await page.addInitScript(
		({ contextKey, scopedKey }) => {
			window.sessionStorage.setItem(contextKey, 'test-account-a');
			window.localStorage.setItem(scopedKey, '{not-json');
		},
		{ contextKey: AUTH_CONTEXT_KEY, scopedKey: `${FAVORITES_KEY_BASE}.test-account-a` }
	);
	await seedLibrary(page);

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);

	// The toggle still works after the corrupt value is ignored.
	await starButton(page, 'fav-mike').click();
	await expect(starButton(page, 'fav-mike')).toHaveAttribute('aria-pressed', 'true');
});

test('[FAV-07] favorites queue exactly like library rows and never duplicate', async ({
	page
}) => {
	await seedLibrary(page);

	await starButton(page, 'fav-mike').click();
	await expect(favoriteItem(page, 'fav-mike')).toBeVisible();

	await row(page, 'fav-mike').click();
	await (await favoriteControl(page, 'fav-mike')).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await queueIds(page)).toEqual(['fav-mike']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	// Re-adding from either surface stays a dedupe no-op.
	await (await favoriteControl(page, 'fav-mike')).click();
	await row(page, 'fav-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await queueIds(page)).toEqual(['fav-mike']);
});

test('[FAV-08] starring, unstarring and favorites queue-add issue zero network mutations', async ({
	page
}) => {
	const requests: { method: string; url: string }[] = [];
	page.on('request', (request) => requests.push({ method: request.method(), url: request.url() }));

	await seedLibrary(page);

	const star = starButton(page, 'fav-zulu');
	await star.click();
	// Non-vacuous gate: the starring action under observation must succeed.
	await expect(star).toHaveAttribute('aria-pressed', 'true');
	await star.click();
	await expect(star).toHaveAttribute('aria-pressed', 'false');

	await starButton(page, 'fav-mike').click();
	await (await favoriteControl(page, 'fav-mike')).click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	const mutations = requests.filter((request) => request.method !== 'GET');
	expect(mutations, 'starring flows must not mutate over the network').toEqual([]);
});

// Fail only the favorites boundary, not auth-context/snapshot storage. Installing
// after the initial load also exercises a device losing storage during a visit.
async function failFavoritesStorage(page: Page, operation: 'getItem' | 'setItem'): Promise<void> {
	await page.evaluate(
		({ base, operation }) => {
			const getItem = Storage.prototype.getItem;
			const setItem = Storage.prototype.setItem;
			if (operation === 'getItem') {
				Storage.prototype.getItem = function (key) {
					if (this === window.localStorage && key.startsWith(`${base}.`)) {
						throw new DOMException('Favorites reads unavailable', 'SecurityError');
					}
					return getItem.call(this, key);
				};
			} else {
				Storage.prototype.setItem = function (key, value) {
					if (this === window.localStorage && key.startsWith(`${base}.`)) {
						throw new DOMException('Favorites writes unavailable', 'QuotaExceededError');
					}
					setItem.call(this, key, value);
				};
			}
			Reflect.set(window, '__restoreFavoritesStorage', () => {
				Storage.prototype.getItem = getItem;
				Storage.prototype.setItem = setItem;
			});
		},
		{ base: FAVORITES_KEY_BASE, operation }
	);
	const context = await readAuthContext(page);
	expect(context, 'favorites faults must leave the session auth context available').not.toBeNull();
	expect(
		await page.evaluate(() => {
			window.localStorage.setItem('test.unrelated-storage', 'available');
			return window.localStorage.getItem('test.unrelated-storage');
		})
	).toBe('available');
}

async function restoreFavoritesStorage(page: Page): Promise<void> {
	await page.evaluate(() => Reflect.get(window, '__restoreFavoritesStorage')());
}

async function expectFavoriteIds(page: Page, ids: string[]): Promise<void> {
	await expect
		.poll(() =>
			page.getByTestId('favorite-item').evaluateAll((items) =>
				items.map((item) => item.getAttribute('data-id')).sort()
			)
		)
		.toEqual([...ids].sort());
}

test.describe('favorites storage regressions', () => {
	let mutations: string[];
	let pageErrors: string[];

	test.beforeEach(async ({ page }) => {
		mutations = [];
		pageErrors = [];
		page.on('request', (request) => {
			if (request.method() !== 'GET') mutations.push(`${request.method()} ${request.url()}`);
		});
		page.on('pageerror', (error) => pageErrors.push(error.message));
		page.on('console', (message) => {
			if (message.type() === 'error') pageErrors.push(message.text());
		});
		// Unexpected API requests cannot reach a backend. seedLibrary registers
		// the only allowed response after this catch-all (Playwright is LIFO).
		await page.route('**/api/**', (route) => route.abort('blockedbyclient'));
	});

	test.afterEach(async ({ page }) => {
		// These gates still execute when a state assertion fails earlier.
		await expect.soft(page.getByTestId('queue-item')).toHaveCount(0);
		expect.soft(mutations, 'favorites must remain local even when storage fails').toEqual([]);
		expect.soft(pageErrors, 'favorites must not cause browser runtime errors').toEqual([]);
	});

	test('[FAV-R01] duplicate stored IDs render once or fall back to empty without a keyed-list error', async ({
		page
	}) => {
		await page.addInitScript(
			({ contextKey, scopedKey }) => {
				window.sessionStorage.setItem(contextKey, 'test-duplicate-account');
				window.localStorage.setItem(scopedKey, JSON.stringify(['fav-zulu', 'fav-zulu', 'fav-alpha']));
			},
			{
				contextKey: AUTH_CONTEXT_KEY,
				scopedKey: `${FAVORITES_KEY_BASE}.test-duplicate-account`
			}
		);
		await seedLibrary(page);
		await expect(page.getByTestId('favorites-section')).toBeVisible();
		const ids = await page.getByTestId('favorite-item').evaluateAll((items) =>
			items.map((item) => item.getAttribute('data-id')).sort()
		);
		// Both malformed-data fallback and stable deduplication satisfy the
		// public contract; never require an internal load/save helper signature.
		expect(ids).toEqual(ids.length === 0 ? [] : ['fav-alpha', 'fav-zulu']);
		if (ids.length === 0) await expect(page.getByTestId('favorites-empty')).toBeVisible();
		await starButton(page, 'fav-mike').click();
		await expect(starButton(page, 'fav-mike')).toHaveAttribute('aria-pressed', 'true');
		await expect(page.getByTestId('favorite-item')).toHaveCount(ids.length + 1);
	});

	for (const operation of ['getItem', 'setItem'] as const) {
		test(`[FAV-R02:${operation}] repeated star/unstar uses the visible state when favorites storage fails`, async ({
			page
		}) => {
			await seedLibrary(page);
			await failFavoritesStorage(page, operation);
			for (const pressed of [true, false, true, false]) {
				await starButton(page, 'fav-zulu').click();
				await expect(starButton(page, 'fav-zulu')).toHaveAttribute('aria-pressed', String(pressed));
				await expectFavoriteIds(page, pressed ? ['fav-zulu'] : []);
			}
		});

		test(`[FAV-R03:${operation}] toggling another track preserves pending favorites while storage fails`, async ({
			page
		}) => {
			await seedLibrary(page);
			await failFavoritesStorage(page, operation);
			await starButton(page, 'fav-zulu').click();
			await expect(starButton(page, 'fav-zulu')).toHaveAttribute('aria-pressed', 'true');
			await starButton(page, 'fav-alpha').click();
			await expectFavoriteIds(page, ['fav-zulu', 'fav-alpha']);
			await expect(starButton(page, 'fav-zulu')).toHaveAttribute('aria-pressed', 'true');
			await starButton(page, 'fav-zulu').click();
			await expectFavoriteIds(page, ['fav-alpha']);
			await expect(starButton(page, 'fav-zulu')).toHaveAttribute('aria-pressed', 'false');
			await expect(starButton(page, 'fav-alpha')).toHaveAttribute('aria-pressed', 'true');
		});

		test(`[FAV-R04:${operation}] a successful library refresh retains favorites pending during storage failure`, async ({
			page
		}) => {
			await seedLibrary(page);
			await failFavoritesStorage(page, operation);
			await starButton(page, 'fav-zulu').click();
			await expectFavoriteIds(page, ['fav-zulu']);
			await page.route('**/api/library', (route) =>
				route.fulfill({
					status: 200,
					contentType: 'application/json',
					body: JSON.stringify(ITEMS.map((item) =>
						item.id === 'fav-mike' ? { ...item, title: 'Mike Song refreshed' } : item
					))
				})
			);
			await page.getByRole('button', { name: 'Refresh' }).click();
			// Changed server text proves the response was committed, not merely
			// requested while we were still asserting the pre-refresh DOM.
			await expect(row(page, 'fav-mike')).toContainText('Mike Song refreshed');
			await expectFavoriteIds(page, ['fav-zulu']);
			await expect(starButton(page, 'fav-zulu')).toHaveAttribute('aria-pressed', 'true');
		});

		test(`[FAV-R05:${operation}] auth transition clears pending favorites before retry under continued storage failure`, async ({
			page
		}) => {
			await seedLibrary(page);
			const oldContext = await readAuthContext(page);
			await failFavoritesStorage(page, operation);
			await starButton(page, 'fav-zulu').click();
			await expectFavoriteIds(page, ['fav-zulu']);
			const otherTab = await page.context().newPage();
			await otherTab.goto('/login');
			await otherTab.evaluate((key) => {
				window.localStorage.setItem(key, 'test-pending-favorites-auth-transition');
				window.localStorage.removeItem(key);
			}, AUTH_TRANSITION_KEY);
			await expect(page.getByRole('alert')).toContainText(/session changed/i);
			await expect(page.getByTestId('favorite-item')).toHaveCount(0);
			await otherTab.close();
			await page.getByRole('button', { name: 'Retry' }).click();
			await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
			const newContext = await readAuthContext(page);
			expect(newContext).not.toBeNull();
			expect(newContext).not.toBe(oldContext);
			await expect(page.getByTestId('favorites-empty')).toBeVisible();
			await expectFavoriteIds(page, []);
			await expect(starButton(page, 'fav-zulu')).toHaveAttribute('aria-pressed', 'false');
			await starButton(page, 'fav-alpha').click();
			await expectFavoriteIds(page, ['fav-alpha']);
			await restoreFavoritesStorage(page);
			expect(await readFavoritesRaw(page, oldContext)).toBeNull();
		});
	}

	test('[FAV-R06] recovered writes persist the full pending set and survive reload', async ({ page }) => {
		await seedLibrary(page);
		await failFavoritesStorage(page, 'setItem');
		await starButton(page, 'fav-zulu').click();
		await expectFavoriteIds(page, ['fav-zulu']);
		await restoreFavoritesStorage(page);
		// The next successful write must include the unpersisted first star,
		// rather than rebuilding from the stale (empty) stored value.
		await starButton(page, 'fav-alpha').click();
		expect(storedIds(await readFavoritesRaw(page, await readAuthContext(page))).sort()).toEqual([
			'fav-alpha', 'fav-zulu'
		]);
		await expectFavoriteIds(page, ['fav-alpha', 'fav-zulu']);
		await page.reload();
		await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
		await expectFavoriteIds(page, ['fav-alpha', 'fav-zulu']);
		await expect(starButton(page, 'fav-zulu')).toHaveAttribute('aria-pressed', 'true');
		await expect(starButton(page, 'fav-alpha')).toHaveAttribute('aria-pressed', 'true');
	});
});
