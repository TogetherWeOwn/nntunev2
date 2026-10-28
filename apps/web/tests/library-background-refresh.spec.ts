import { expect, test, type Locator, type Page, type Route } from '@playwright/test';

/**
 * Acceptance: the library keeps its loaded list and player queue across a
 * background refresh.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * refresh-retention fixes on top of this branch and must make these tests
 * pass WITHOUT editing this file; a wrong test comes back to acceptance
 * instead. Maintenance below strengthens settle barriers and targets the
 * row action explicitly now that rows have additional controls.
 *
 * Contract under test:
 * - Initial load keeps today's behavior: a loading status until the first
 *   response lands, no list before then.
 * - Background refresh (data already loaded) retains the loaded list and
 *   queue DOM nodes for surviving entries, keeps keyboard focus where it
 *   is, and never swaps in the full-page loading state. Progress is
 *   signalled on the controls that stay mounted: the Refresh trigger is
 *   disabled while a refresh is in flight, and the library list carries
 *   `aria-busy="true"` until the refresh settles.
 * - Queue selection and Next stay usable while a refresh is pending.
 * - A fresh payload with the same ids updates queued display entries
 *   (corrected title/artist) without reordering the queue or moving the
 *   surviving current track. Absent ids are removed with the existing
 *   deterministic semantics: the first survivor becomes current, or the
 *   queue reads empty when nothing survives.
 * - A non-auth refresh failure (500 here) keeps that account's last-good
 *   view, queue order and current selection with the stale badge and a
 *   Retry trigger; a later success clears the stale state.
 * - 401/403, an unavailable 404 and auth-transition events purge the
 *   library and queue immediately, including mid-refresh; a late
 *   old-account response cannot restore purged state.
 *
 * Out of scope (not pinned): audio playback and stream URLs (the player
 * does not mount for display-only tracks), prev/seek, queue reorder, and the
 * refresher's body caps, timeout/cancellation and single-flight behavior
 * (pinned by the refresher unit suite, which stays unchanged).
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so DOM continuity is pinned here in the
 * offline Playwright suite with route-mocked `/api/library` responses that
 * tests hold and release. Node identity is observed through a JS expando
 * that cannot survive a remount. The focus test drives the Refresh trigger
 * with an in-page click because a trusted click would move focus to the
 * trigger itself and test nothing.
 *
 * Hooks: the established `library-list` / `library-item` (`data-id`),
 * `queue-item` (`data-track-id`, falling back to `data-id`),
 * `now-playing`, `player-next`, `library-stale-badge` and `audio-player`
 * hooks, plus the Refresh/Retry button names. No new hooks.
 */

// Rendered order is API order; ids are deliberately not in sorted order.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' }
];

/** Same ids as ITEMS with corrected display metadata. */
const CORRECTED = [
	{ id: 'row-zulu', title: 'Zulu Song (Remastered)', artist: 'Corrected Artist' },
	{ id: 'row-alpha', title: 'Alpha Song (Remastered)', artist: 'Corrected Artist' },
	{ id: 'row-mike', title: 'Mike Song (Remastered)', artist: 'Corrected Artist' }
];

/** Refresh payload that drops the first-selected id (row-mike). */
const WITHOUT_MIKE = [ITEMS[0], ITEMS[1]];

const NEXT_ACCOUNT = [{ id: 'row-bravo', title: 'Bravo Song', artist: 'Next Account' }];

const SNAPSHOT_KEY = 'nntune.library.snapshot.v1';
const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';

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

async function seedLibrary(page: Page, items: unknown): Promise<void> {
	await routeLibrary(page, { current: items });
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount((items as unknown[]).length);
}

interface HeldLibraryRequests {
	/** Resolves once the first held request has arrived. */
	waitForHold: () => Promise<void>;
	/** Number of `/api/library` requests observed since the hold was installed. */
	requestCount: () => number;
	/** Release the held request with a JSON payload. */
	releaseJson: (body: unknown, status?: number) => Promise<void>;
}

/**
 * Capture `/api/library` requests and hold the first one until released.
 * Any request beyond the first is failed loudly: with single-flight plus a
 * disabled trigger, a background refresh must never emit a second request.
 */
async function holdLibraryRequests(page: Page): Promise<HeldLibraryRequests> {
	let held: Route | null = null;
	let count = 0;
	let notify!: () => void;
	const seen = new Promise<void>((resolve) => {
		notify = resolve;
	});
	await page.unroute('**/api/library');
	await page.route('**/api/library', (route) => {
		count += 1;
		if (!held) {
			held = route;
			notify();
		} else {
			void route.abort('failed');
		}
	});
	return {
		waitForHold: () => seen,
		requestCount: () => count,
		releaseJson: async (body: unknown, status = 200) => {
			await seen;
			const route = held;
			if (!route) throw new Error('no held library request to release');
			await route.fulfill({
				status,
				contentType: 'application/json',
				body: JSON.stringify(body)
			});
		}
	};
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"] .nt-library-row`);
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

/**
 * Tag every matched element with a JS expando. An expando lives on the DOM
 * node itself, so it survives in-place updates and cannot survive an
 * unmount/remount — exactly the continuity signal this spec needs.
 */
async function markNodes(locator: Locator): Promise<void> {
	await locator.evaluateAll(
		(elements, mark) => {
			for (const element of elements) {
				(element as unknown as Record<string, unknown>)[mark] = 'kept';
			}
		},
		'background-refresh-kept'
	);
}

async function keptMarks(locator: Locator): Promise<(unknown | null)[]> {
	return locator.evaluateAll(
		(elements, mark) =>
			elements.map(
				(element) => (element as unknown as Record<string, unknown>)[mark] ?? null
			),
		'background-refresh-kept'
	);
}

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
}

async function readSnapshot(page: Page): Promise<string | null> {
	return page.evaluate(
		({ key, contextKey }) => {
			const context = window.sessionStorage.getItem(contextKey);
			return context ? window.sessionStorage.getItem(`${key}.${context}`) : null;
		},
		{ key: SNAPSHOT_KEY, contextKey: AUTH_CONTEXT_KEY }
	);
}

test('initial load still shows the loading state until the first response lands', async ({
	page
}) => {
	const hold = await holdLibraryRequests(page);
	await page.goto('/library');
	await hold.waitForHold();

	await expect(page.getByText('Loading your library…')).toBeVisible();
	await expect(page.getByTestId('library-list')).toHaveCount(0);

	await hold.releaseJson(ITEMS);
	await expect(page.getByTestId('library-list')).toBeVisible();
	await expect(page.getByTestId('library-list')).toHaveAttribute('aria-busy', 'false');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
});

test('background refresh keeps the loaded list and queue mounted while the refresh is pending', async ({
	page
}) => {
	await seedLibrary(page, ITEMS);
	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	const hold = await holdLibraryRequests(page);
	await page.getByRole('button', { name: 'Refresh' }).click();
	await hold.waitForHold();

	const list = page.getByTestId('library-list');
	await expect(list, 'background refresh unmounted the loaded list').toBeVisible();
	await expect(list).toHaveAttribute('aria-busy', 'true');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('player-next')).toBeEnabled();
	await expect(page.getByText('Loading your library…')).toHaveCount(0);

	await hold.releaseJson(ITEMS);
	await expect(page.getByTestId('library-list')).toBeVisible();
	await expect(page.getByTestId('library-list')).toHaveAttribute('aria-busy', 'false');
	await expect(list).not.toHaveAttribute('aria-busy', 'true');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
});

test('background refresh preserves DOM node identity and row focus for surviving entries', async ({
	page
}) => {
	await seedLibrary(page, ITEMS);
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();

	const rows = page.getByTestId('library-item');
	const queued = page.getByTestId('queue-item');
	await markNodes(rows);
	await markNodes(queued);

	const focusTarget = page.locator('[data-testid="library-item"][data-id="row-alpha"] .nt-library-row');
	await focusTarget.focus();
	await expect(focusTarget).toBeFocused();

	const hold = await holdLibraryRequests(page);
	// In-page click: a trusted click would move focus to Refresh and test nothing.
	await page
		.getByRole('button', { name: 'Refresh' })
		.evaluate((element) => (element as HTMLElement).click());
	await hold.waitForHold();

	await expect(focusTarget, 'background refresh moved focus off the surviving row').toBeFocused();
	expect(await keptMarks(rows)).toEqual(['kept', 'kept', 'kept']);
	expect(await keptMarks(queued)).toEqual(['kept', 'kept']);

	await hold.releaseJson(ITEMS);
	await expect(page.getByTestId('library-list')).toBeVisible();
	await expect(page.getByTestId('library-list')).toHaveAttribute('aria-busy', 'false');
	await expect(focusTarget).toBeFocused();
	expect(await keptMarks(rows)).toEqual(['kept', 'kept', 'kept']);
	expect(await keptMarks(queued)).toEqual(['kept', 'kept']);
});

test('background refresh disables the refresh trigger and sends no redundant request', async ({
	page
}) => {
	await seedLibrary(page, ITEMS);

	const hold = await holdLibraryRequests(page);
	const refreshButton = page.getByRole('button', { name: 'Refresh' });
	await refreshButton.click();
	await hold.waitForHold();

	await expect(refreshButton).toBeDisabled();
	// Redundant activation attempts: a real click on the disabled trigger
	// plus a programmatic click through single-flight. Neither may emit a
	// request; the count is asserted after the held request settles.
	await refreshButton.click({ force: true });
	await refreshButton.evaluate((element) => (element as HTMLElement).click());

	await hold.releaseJson(ITEMS);
	await expect(page.getByTestId('library-list')).toBeVisible();
	await expect(page.getByTestId('library-list')).toHaveAttribute('aria-busy', 'false');
	await expect(refreshButton).toBeEnabled();
	expect(hold.requestCount()).toBe(1);
	await expect(page.getByTestId('library-list')).not.toHaveAttribute('aria-busy', 'true');
});

test('queue Next stays usable while a background refresh is pending', async ({ page }) => {
	await seedLibrary(page, ITEMS);
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	const hold = await holdLibraryRequests(page);
	await page.getByRole('button', { name: 'Refresh' }).click();
	await hold.waitForHold();

	await expect(page.getByTestId('player-next')).toBeEnabled();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu']);

	await hold.releaseJson(ITEMS);
	await expect(page.getByTestId('library-list')).toBeVisible();
	await expect(page.getByTestId('library-list')).toHaveAttribute('aria-busy', 'false');
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
});

test('selecting a row while a background refresh is pending queues it', async ({ page }) => {
	await seedLibrary(page, ITEMS);

	const hold = await holdLibraryRequests(page);
	await page.getByRole('button', { name: 'Refresh' }).click();
	await hold.waitForHold();

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');

	await hold.releaseJson(ITEMS);
	await expect(page.getByTestId('library-list')).toBeVisible();
	await expect(page.getByTestId('library-list')).toHaveAttribute('aria-busy', 'false');
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
});

test('same-ID title and artist corrections reach queued entries without reordering or moving current', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);

	// Gesture order runs against rendered order; Next moves current mid-queue.
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	payload.current = CORRECTED;
	await page.getByRole('button', { name: 'Refresh' }).click();

	await expect(page.getByTestId('library-list')).toContainText('Zulu Song (Remastered)');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song (Remastered)');
	await expect(queueItem(page, 'row-zulu')).toContainText('Corrected Artist');
	await expect(queueItem(page, 'row-mike')).toContainText('Mike Song (Remastered)');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBe('true');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBeNull();
	expect(await ariaCurrent(queueItem(page, 'row-alpha'))).toBeNull();
	// Main renders media only for a usable stream URL; refreshing metadata
	// must not manufacture a player or source for these display-only tracks.
	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect(page.locator('audio, video')).toHaveCount(0);
});

test('refresh to a subset drops absent ids and moves current to the first survivor', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);

	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await markNodes(page.getByTestId('library-item'));

	payload.current = WITHOUT_MIKE;
	await page.getByRole('button', { name: 'Refresh' }).click();

	await expect(page.getByTestId('library-item')).toHaveCount(2);
	expect(await queueOrder(page)).toEqual(['row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBe('true');
	expect(await ariaCurrent(queueItem(page, 'row-alpha'))).toBeNull();
	expect(await keptMarks(page.getByTestId('library-item'))).toEqual(['kept', 'kept']);
});

test('refresh to an empty library clears the queue and shows the empty state', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);

	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	payload.current = [];
	await page.getByRole('button', { name: 'Refresh' }).click();

	await expect(page.getByRole('status').filter({ hasText: /library is empty/i })).toContainText(/library is empty/i);
	await expect(page.getByRole('link', { name: 'Import an NZB' })).toHaveAttribute(
		'href',
		'/library/import'
	);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// The queue was cleared, not just hidden: a fresh non-empty payload
	// renders the empty-queue state rather than resurrecting the old queue.
	payload.current = ITEMS;
	await page.getByRole('button', { name: 'Refresh' }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('player-next')).toBeDisabled();
});

test('non-auth refresh failure keeps the last-good view and queue, then recovery clears the stale state', async ({
	page
}) => {
	await seedLibrary(page, ITEMS);
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	await markNodes(page.getByTestId('library-item'));
	await markNodes(page.getByTestId('queue-item'));

	const hold = await holdLibraryRequests(page);
	await page.getByRole('button', { name: 'Refresh' }).click();
	await hold.waitForHold();
	await hold.releaseJson({ error: 'boom' }, 500);

	const badge = page.getByTestId('library-stale-badge');
	await expect(badge).toBeVisible();
	await expect(badge).toContainText(/stale — updated /i);
	await expect(page.getByRole('button', { name: 'Retry' })).toBeVisible();
	await expect(page.getByTestId('library-list')).toContainText('Zulu Song');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(await keptMarks(page.getByTestId('library-item'))).toEqual(['kept', 'kept', 'kept']);
	expect(await keptMarks(page.getByTestId('queue-item'))).toEqual(['kept', 'kept']);

	await page.unroute('**/api/library');
	await routeLibrary(page, { current: ITEMS });
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(badge).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Refresh' })).toBeVisible();
	expect(await keptMarks(page.getByTestId('library-item'))).toEqual(['kept', 'kept', 'kept']);
});

for (const status of [401, 403]) {
	test(`${status} during a background refresh purges the library and queue and Retry loads the next account clean`, async ({
		page
	}) => {
		const payload: { current: unknown } = { current: ITEMS };
		await routeLibrary(page, payload);
		await page.goto('/library');
		await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
		await row(page, 'row-mike').click();
		await expect(page.getByTestId('queue-item')).toHaveCount(1);

		await page.unroute('**/api/library');
		await page.route('**/api/library', (route) =>
			route.fulfill({
				status,
				contentType: 'application/json',
				body: JSON.stringify({ error: 'denied' })
			})
		);
		await page.getByRole('button', { name: 'Refresh' }).click();

		await expect(page.getByRole('alert')).toContainText(/signed out|do not have access/i);
		await expect(page.getByTestId('library-list')).toHaveCount(0);
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await expect(page.getByTestId('now-playing')).toHaveCount(0);
		expect(await readSnapshot(page)).toBeNull();

		payload.current = NEXT_ACCOUNT;
		await page.unroute('**/api/library');
		await routeLibrary(page, payload);
		await page.getByRole('button', { name: 'Retry' }).click();
		await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
		await expect(page.getByTestId('queue-item')).toHaveCount(0);
		await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
		await expect(page.getByTestId('player-next')).toBeDisabled();
	});
}

test('404 during a background refresh purges the library and queue without exposing a snapshot', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	await page.unroute('**/api/library');
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({}) })
	);
	await page.getByRole('button', { name: 'Refresh' }).click();

	await expect(page.getByRole('status').filter({ hasText: /listing is not available yet/i })).toContainText(/listing is not available yet/i);
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toHaveCount(0);

	payload.current = NEXT_ACCOUNT;
	await page.unroute('**/api/library');
	await routeLibrary(page, payload);
	await page.getByRole('button', { name: 'Refresh' }).click();
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('auth transition during a background refresh purges immediately and a late old-account response cannot restore state', async ({
	page
}) => {
	await seedLibrary(page, ITEMS);
	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	const hold = await holdLibraryRequests(page);
	await page.getByRole('button', { name: 'Refresh' }).click();
	await hold.waitForHold();
	await fireAuthTransition(page);

	// The purge lands immediately, without waiting for the held refresh.
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// The late old-account response cannot restore state. The list stays
	// purged here, so unlike successful refreshes it cannot be a settle hook.
	await hold.releaseJson(ITEMS);
	await expect(page.getByTestId('library-list')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	expect(await readSnapshot(page)).toBeNull();

	await page.unroute('**/api/library');
	await routeLibrary(page, { current: NEXT_ACCOUNT });
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
});
