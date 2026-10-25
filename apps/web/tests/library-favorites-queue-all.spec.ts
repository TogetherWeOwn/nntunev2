import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: queue all local favorites in star order with one action.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * queue-all implementation on top of this branch and must make these tests
 * pass WITHOUT editing this file; a wrong test comes back to the
 * acceptance lane instead of being fixed beside the implementation.
 *
 * Harness note: `/library` is client-fetched (`ssr = false`, component
 * fetches `/api/library` on mount) and the vitest setup in this package is
 * SSR-render/source-pin only — there is no DOM component-test environment.
 * The offline Playwright library specs (`library-favorites-local.spec.ts`,
 * `library-queue-announcement.spec.ts`) are the interactive harness for this
 * page, so the queue-all contract is pinned here with a route-mocked
 * `/api/library`. Fully offline and deterministic: no backend, no provider.
 *
 * Contract under test:
 * - The Favorites section gains one explicit bulk action whose accessible
 *   name matches `/queue.*favorites/i` ("Queue favorites"). It renders only
 *   when at least one favorite resolves against the current library; with
 *   zero resolved favorites the honest empty state shows and no such action
 *   exists. Exactly one such action exists whenever it renders.
 * - Activating it appends every resolved favorite in star order through the
 *   existing deduping add path: already-queued favorites stay in place and
 *   are never duplicated, and the current selection is never stolen.
 * - The outcome is reported once (queued N, M already queued) through the
 *   existing `queue-announcement` live region: one element, text replaced
 *   per activation, never appended. Unresolvable favorites (stored ids
 *   absent from the current library) are skipped, never invented as queue
 *   items, and never counted as queued.
 * - The action reads the same favorites source as the shelf (the shelf's
 *   resolved set is exactly what gets queued); an auth transition purges
 *   both so nothing queues across accounts; activating the action performs
 *   no new storage writes.
 * - The action is keyboard/touch operable and narrow-screen safe; it issues
 *   zero network mutations and starts zero playback (no audio element).
 *
 * Every test below asserts the action's visibility with `expect` before
 * touching it, so RED on unimplemented branches is always a failing
 * assertion — never a click timeout, compile failure, or missing import.
 * There is no acceptance-owned unit half: every new behavior is page-level
 * with no new pure-helper contract in the build, so a unit file could only
 * import a helper that does not exist yet (an import failure, not RED) or
 * prescribe the builder's internal names. The build's own units cover order,
 * dedupe, unresolvable skip, and empty at whatever helper it extracts; the
 * same cases are pinned here at the browser contract as FAVQ-02/03/05/01.
 *
 * Case IDs (stable): FAVQ-01 visibility + single action, FAVQ-02 star-order
 * queue, FAVQ-03 dedupe/in-place/current, FAVQ-04 announcement once,
 * FAVQ-05 unresolvable skip, FAVQ-06 auth-transition purge, FAVQ-07
 * keyboard, FAVQ-08 narrow-screen touch, FAVQ-09 zero side effects.
 */

// Fixture titles/artists deliberately avoid the words "star", "favorite",
// and "queue" so the action name filter below can only match the bulk
// action itself, never a row or favorite button.
const ITEMS = [
	{ id: 'favq-zulu', title: 'Zulu Song', artist: 'Harbor Lights' },
	{ id: 'favq-alpha', title: 'Alpha Song' },
	{ id: 'favq-mike', title: 'Mike Song', artist: 'Harbor Lights' }
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

function queueAllButton(page: Page): Locator {
	return page
		.getByTestId('favorites-section')
		.getByRole('button', { name: /queue.*favorites/i });
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

test('[FAVQ-01] the queue-all action renders exactly once only when a favorite resolves', async ({
	page
}) => {
	await seedLibrary(page);

	// Zero resolved favorites: honest empty state, no bulk action.
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expect(queueAllButton(page)).toHaveCount(0);

	await starButton(page, 'favq-mike').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
	const action = queueAllButton(page);
	await expect(action, 'Queue favorites action missing once a favorite resolves').toBeVisible();
	await expect(action).toHaveCount(1);

	await starButton(page, 'favq-alpha').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);
	await expect(queueAllButton(page)).toHaveCount(1);

	// Back to zero: the action leaves with the last resolved favorite.
	await starButton(page, 'favq-mike').click();
	await starButton(page, 'favq-alpha').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expect(queueAllButton(page)).toHaveCount(0);
});

test('[FAVQ-02] queue-all appends resolved favorites in star order', async ({ page }) => {
	await seedLibrary(page);

	// Star in an order that matches neither library order nor title order.
	await starButton(page, 'favq-mike').click();
	await starButton(page, 'favq-zulu').click();
	await starButton(page, 'favq-alpha').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(3);

	const action = queueAllButton(page);
	await expect(action).toBeVisible();
	await action.click();

	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueIds(page)).toEqual(['favq-mike', 'favq-zulu', 'favq-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect(page.getByTestId('player-next')).toBeEnabled();
});

test('[FAVQ-03] queue-all dedupes in place and never steals the current track', async ({
	page
}) => {
	await seedLibrary(page);

	// The middle-starred favorite is already queued and current.
	await row(page, 'favq-zulu').click();
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	await starButton(page, 'favq-mike').click();
	await starButton(page, 'favq-zulu').click();
	await starButton(page, 'favq-alpha').click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(3);

	const action = queueAllButton(page);
	await expect(action).toBeVisible();
	await action.click();

	// Zulu stays first and current; the rest append in star order, once each.
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueIds(page)).toEqual(['favq-zulu', 'favq-mike', 'favq-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	// Re-activating is an idempotent no-op: order, count, and current hold.
	await action.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueIds(page)).toEqual(['favq-zulu', 'favq-mike', 'favq-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
});

test('[FAVQ-04] queue-all reports queued and already-queued counts once', async ({ page }) => {
	await seedLibrary(page);

	await row(page, 'favq-zulu').click();
	await starButton(page, 'favq-mike').click();
	await starButton(page, 'favq-zulu').click();
	await starButton(page, 'favq-alpha').click();

	const action = queueAllButton(page);
	await expect(action).toBeVisible();
	await action.click();

	const announcement = page.getByTestId('queue-announcement');
	await expect(announcement).toHaveCount(1);
	await expect(announcement).toContainText(/queued 2/i);
	await expect(announcement).toContainText(/1 already queued/i);

	// The second activation replaces the outcome text; it never appends a
	// second report beside the first.
	await action.click();
	await expect(page.getByTestId('queue-announcement')).toHaveCount(1);
	await expect(announcement).toContainText(/queued 0/i);
	await expect(announcement).toContainText(/3 already queued/i);
});

test('[FAVQ-05] queue-all skips stored favorites absent from the library', async ({ page }) => {
	await page.addInitScript(
		({ contextKey, scopedKey }) => {
			window.sessionStorage.setItem(contextKey, 'test-queue-all-resolve');
			window.localStorage.setItem(
				scopedKey,
				JSON.stringify(['favq-ghost', 'favq-mike', 'favq-alpha'])
			);
		},
		{
			contextKey: AUTH_CONTEXT_KEY,
			scopedKey: `${FAVORITES_KEY_BASE}.test-queue-all-resolve`
		}
	);
	await seedLibrary(page);

	// The shelf lists the resolved set; the ghost id stays stored but unlisted.
	await expect(page.getByTestId('favorite-item')).toHaveCount(2);

	const action = queueAllButton(page);
	await expect(action).toBeVisible();
	await action.click();

	// Exactly the resolved shelf set queues, in star order; nothing invented.
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueIds(page)).toEqual(['favq-mike', 'favq-alpha']);
	await expect(
		page.locator('[data-testid="queue-item"][data-track-id="favq-ghost"]')
	).toHaveCount(0);
	await expect(
		page.locator('[data-testid="queue-item"][data-id="favq-ghost"]')
	).toHaveCount(0);
	await expect(page.getByTestId('queue-announcement')).toContainText(/queued 2/i);
});

test('[FAVQ-06] an auth transition purges queued favorites before the next account', async ({
	page
}) => {
	await seedLibrary(page);

	await starButton(page, 'favq-mike').click();
	await starButton(page, 'favq-zulu').click();
	const action = queueAllButton(page);
	await expect(action).toBeVisible();
	await action.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);

	const contextBefore = await readAuthContext(page);
	expect(contextBefore).not.toBeNull();

	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-queue-all-auth-transition');
		window.localStorage.removeItem(key);
	}, AUTH_TRANSITION_KEY);

	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	expect(
		await readFavoritesRaw(page, contextBefore),
		'previous-account favorites must be cleared at the auth boundary'
	).toBeNull();
	await otherTab.close();

	// The next account refetches and starts with an empty shelf and queue.
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// Queueing in the new account carries nothing from the previous one.
	await starButton(page, 'favq-alpha').click();
	const nextAction = queueAllButton(page);
	await expect(nextAction).toBeVisible();
	await nextAction.click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await queueIds(page)).toEqual(['favq-alpha']);
});

test('[FAVQ-07] the queue-all action is keyboard operable', async ({ page }) => {
	await seedLibrary(page);

	await starButton(page, 'favq-alpha').click();
	await starButton(page, 'favq-mike').click();

	const action = queueAllButton(page);
	await expect(action).toBeVisible();
	await action.focus();
	await expect(action, 'queue-all action must be keyboard-focusable').toBeFocused();

	await page.keyboard.press('Enter');
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueIds(page)).toEqual(['favq-alpha', 'favq-mike']);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');

	// Space re-activation stays a dedupe no-op.
	await page.keyboard.press('Space');
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueIds(page)).toEqual(['favq-alpha', 'favq-mike']);
});

test.describe('narrow-screen touch', () => {
	test.use({ viewport: { width: 360, height: 800 }, hasTouch: true });

	test('[FAVQ-08] the queue-all action is visible and tappable at 360px', async ({ page }) => {
		await seedLibrary(page);

		await starButton(page, 'favq-zulu').tap();
		await starButton(page, 'favq-mike').tap();
		await expect(page.getByTestId('favorite-item')).toHaveCount(2);

		const action = queueAllButton(page);
		await expect(action, 'queue-all action must stay visible at 360px width').toBeVisible();
		await action.tap();

		await expect(page.getByTestId('queue-item')).toHaveCount(2);
		expect(await queueIds(page)).toEqual(['favq-zulu', 'favq-mike']);
		await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	});
});

test('[FAVQ-09] queue-all issues no network mutations, storage writes, or playback', async ({
	page
}) => {
	const requests: { method: string; url: string }[] = [];
	page.on('request', (request) => requests.push({ method: request.method(), url: request.url() }));

	await seedLibrary(page);

	await starButton(page, 'favq-mike').click();
	await starButton(page, 'favq-alpha').click();
	const action = queueAllButton(page);
	await expect(action).toBeVisible();

	const context = await readAuthContext(page);
	expect(context, 'library auth context must exist after a successful load').not.toBeNull();
	const storedBefore = await readFavoritesRaw(page, context);

	await action.click();
	// Non-vacuous gate: the queue-all under observation must succeed.
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	expect(await queueIds(page)).toEqual(['favq-mike', 'favq-alpha']);

	expect(
		await readFavoritesRaw(page, context),
		'queue-all must not write favorites storage'
	).toBe(storedBefore);
	const mutations = requests.filter((request) => request.method !== 'GET');
	expect(mutations, 'queue-all must not mutate over the network').toEqual([]);
	await expect(
		page.locator('audio[data-testid="audio-player"]'),
		'queue-all must not start playback'
	).toHaveCount(0);
});
