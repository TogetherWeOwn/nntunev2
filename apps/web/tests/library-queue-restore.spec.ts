import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: the arranged library queue is restored across reload.
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build card lands
 * the queue persist/restore implementation on top of this branch and must
 * make these tests pass WITHOUT editing this file; a wrong test comes
 * back to the acceptance card instead.
 *
 * Harness note (same as `library-player-queue.spec.ts`): `/library` is
 * client-fetched (`ssr = false`), so the reload-driven contract is pinned
 * here in the offline Playwright suite with a route-mocked `/api/library`.
 * `sessionStorage` survives `page.reload()` (same tab), so arrange→reload
 * and seeded-storage→load cases run entirely offline with no provider,
 * network, secret, or production dependency.
 *
 * Hooks (the established literals): `library-item` (`data-id` = track id)
 * and `library-list` name the library rows; the queue hooks are
 * `queue-item` (carrying `data-track-id`, falling back to `data-id`),
 * `now-playing`, and `player-next`; the current item carries
 * `aria-current="true"`. The per-row `Move {title} up` buttons and the
 * `audio-player`/`favorite-item` hooks are owned by their suites and only
 * read here. None are TESTIDS entries: the route-contract unit test maps
 * every TESTIDS key to a single route source, and these hooks live in a
 * component or a sibling suite.
 *
 * Storage contract under test (paired build card; mirrored by the
 * `queue-restore.acceptance` unit bank, which pins the same key):
 * - The library queue persists tab-locally in `sessionStorage` under
 *   `nntune.library.queue.v1` as `{"orderIds", "currentId"}` JSON on every
 *   queue change; reload restores order + current by stable id (first item
 *   when currentId is missing).
 * - Unknown ids, duplicate ids, corrupt JSON, and an empty payload all
 *   restore an EMPTY queue — never the full catalogue, never invented
 *   tracks — and leave the page usable.
 * - Restore happens only after a successful library load: a failed load
 *   restores nothing, and Retry restores.
 * - Auth/account transitions purge the stored queue: a cross-tab clear is
 *   never resurrected, and no queue crosses accounts.
 * - Restore fetches nothing beyond the library load, renders no audio, and
 *   preserves favorites.
 *
 * RED-vs-guard split (verified on the pre-implementation base): B01-B05
 * and B10-B12 are assertion-RED (no persistence exists yet). B06-B09
 * already pass on the always-empty base queue and pin the "never invent"
 * invariants against a naive implementation (e.g. reusing the demo
 * full-catalogue fallback would fill the queue and fail them).
 */

// Rendered order is API order; ids are deliberately not in sorted order.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' }
];

const NEXT_ACCOUNT = [{ id: 'row-bravo', title: 'Bravo Song', artist: 'Next Account' }];

/** Session-storage key the build persists the library queue under. */
const QUEUE_KEY = 'nntune.library.queue.v1';

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
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
}

/** Seed the persisted queue before load; runs before page scripts on every navigation. */
async function seedQueueStorage(page: Page, value: unknown): Promise<void> {
	await page.addInitScript(
		({ key, raw }) => {
			window.sessionStorage.setItem(key, raw);
		},
		{ key: QUEUE_KEY, raw: typeof value === 'string' ? value : JSON.stringify(value) }
	);
}

async function readQueueStorage(page: Page): Promise<string | null> {
	return page.evaluate((key) => window.sessionStorage.getItem(key), QUEUE_KEY);
}

function row(page: Page, id: string): Locator {
	return page.locator(`[data-testid="library-item"][data-id="${id}"]`);
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

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
}

test('[LIBQR-B01] an arranged queue with a non-first current track survives reload', async ({
	page
}) => {
	await seedLibrary(page);

	// Gesture order runs against rendered order: mike (3rd), zulu (1st), alpha (2nd).
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	const moveUp = page.getByRole('button', { name: 'Move Alpha Song up' });
	await expect(moveUp, 'arrange control renders').toBeVisible();
	await moveUp.click();
	await page.getByRole('button', { name: 'Move Alpha Song up' }).click();
	await expect
		.poll(async () => queueOrder(page))
		.toEqual(['row-alpha', 'row-mike', 'row-zulu']);
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');

	await page.reload();

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(async () => queueOrder(page)).toEqual(['row-alpha', 'row-mike', 'row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	await expect(queueItem(page, 'row-zulu')).toHaveAttribute('aria-current', 'true');
});

test('[LIBQR-B02] an auth transition purges the persisted queue; reload stays empty', async ({
	page
}) => {
	await seedLibrary(page);
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();

	expect(JSON.parse((await readQueueStorage(page)) ?? 'null')).toEqual({
		orderIds: ['row-mike', 'row-zulu'],
		currentId: 'row-mike'
	});

	await fireAuthTransition(page);

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect
		.poll(async () => readQueueStorage(page))
		.toBeNull();

	await page.reload();

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
});

test('[LIBQR-B03] a failed load restores nothing; Retry restores the queue', async ({ page }) => {
	const api: { status: number; current: unknown } = { status: 500, current: { error: true } };
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: api.status,
			contentType: 'application/json',
			body: JSON.stringify(api.current)
		})
	);
	await seedQueueStorage(page, {
		orderIds: ['row-zulu', 'row-mike'],
		currentId: 'row-mike'
	});

	await page.goto('/library');

	await expect(page.getByRole('alert')).toBeVisible();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	api.status = 200;
	api.current = ITEMS;
	await page.getByRole('button', { name: 'Retry' }).click();

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(async () => queueOrder(page)).toEqual(['row-zulu', 'row-mike']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});

test('[LIBQR-B04] a seeded queue restores order with current by stable id', async ({ page }) => {
	await routeLibrary(page, { current: ITEMS });
	await seedQueueStorage(page, {
		orderIds: ['row-zulu', 'row-alpha', 'row-mike'],
		currentId: 'row-alpha'
	});

	await page.goto('/library');

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(async () => queueOrder(page)).toEqual(['row-zulu', 'row-alpha', 'row-mike']);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	await expect(queueItem(page, 'row-alpha')).toHaveAttribute('aria-current', 'true');
});

test('[LIBQR-B05] a missing currentId restores with the first item current', async ({ page }) => {
	await routeLibrary(page, { current: ITEMS });
	await seedQueueStorage(page, { orderIds: ['row-zulu', 'row-alpha'], currentId: null });

	await page.goto('/library');

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(async () => queueOrder(page)).toEqual(['row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
});

test('[LIBQR-B06] unknown ids restore an empty queue and leave the catalogue intact', async ({
	page
}) => {
	await routeLibrary(page, { current: ITEMS });
	await seedQueueStorage(page, {
		orderIds: ['row-zulu', 'row-nope'],
		currentId: 'row-zulu'
	});

	await page.goto('/library');

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('player-next')).toBeDisabled();
});

test('[LIBQR-B07] duplicate ids restore an empty queue', async ({ page }) => {
	await routeLibrary(page, { current: ITEMS });
	await seedQueueStorage(page, {
		orderIds: ['row-zulu', 'row-zulu'],
		currentId: 'row-zulu'
	});

	await page.goto('/library');

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
});

test('[LIBQR-B08] corrupt JSON restores an empty queue and leaves the page usable', async ({
	page
}) => {
	await routeLibrary(page, { current: ITEMS });
	await seedQueueStorage(page, '{not-json');

	await page.goto('/library');

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// Restore must not throw: the library stays interactive.
	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});

test('[LIBQR-B09] an explicitly empty payload restores an empty queue', async ({ page }) => {
	await routeLibrary(page, { current: ITEMS });
	await seedQueueStorage(page, { orderIds: [], currentId: null });

	await page.goto('/library');

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
});

test('[LIBQR-B10] a cross-tab clear is not resurrected by stale storage', async ({ page }) => {
	await routeLibrary(page, { current: ITEMS });
	await seedQueueStorage(page, {
		orderIds: ['row-zulu', 'row-mike'],
		currentId: 'row-mike'
	});

	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(async () => queueOrder(page)).toEqual(['row-zulu', 'row-mike']);

	await fireAuthTransition(page);

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect
		.poll(async () => readQueueStorage(page))
		.toBeNull();
});

test('[LIBQR-B11] no queue crosses accounts', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await routeLibrary(page, payload);
	await seedQueueStorage(page, {
		orderIds: ['row-zulu', 'row-mike'],
		currentId: 'row-mike'
	});

	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(async () => queueOrder(page)).toEqual(['row-zulu', 'row-mike']);

	payload.current = NEXT_ACCOUNT;
	await fireAuthTransition(page);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	// The seeded payload is re-applied on reload, but its ids belong to the
	// previous account: nothing may restore against the new catalogue.
	await page.reload();

	await expect(page.getByTestId('library-item')).toHaveCount(NEXT_ACCOUNT.length);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
});

test('[LIBQR-B12] restore fetches once, renders no audio, and preserves favorites', async ({
	page
}) => {
	let fetchCount = 0;
	await page.route('**/api/library', (route) => {
		fetchCount += 1;
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		});
	});
	await seedQueueStorage(page, {
		orderIds: ['row-zulu', 'row-mike'],
		currentId: 'row-mike'
	});

	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(async () => queueOrder(page)).toEqual(['row-zulu', 'row-mike']);
	expect(fetchCount).toBe(1);
	await expect(page.getByTestId('audio-player')).toHaveCount(0);

	const libraryList = page.getByTestId('library-list');
	await libraryList.getByRole('button', { name: 'Star Mike Song' }).click();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);

	await page.reload();

	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	await expect.poll(async () => queueOrder(page)).toEqual(['row-zulu', 'row-mike']);
	expect(fetchCount).toBe(2);
	await expect(libraryList.getByRole('button', { name: 'Unstar Mike Song' })).toBeVisible();
	await expect(page.getByTestId('favorite-item')).toHaveCount(1);
});
