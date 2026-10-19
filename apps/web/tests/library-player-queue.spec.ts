import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: selecting library rows fills the player queue (M1-i,
 * queue-order un-fixme step 3, test half).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build card lands the
 * library→queue wiring on top of this branch and must make these tests pass
 * WITHOUT editing this file; a wrong test comes back to the acceptance card
 * instead.
 *
 * Harness note (same as `library-selectable.spec.ts`): `/library` is
 * client-fetched (`ssr = false`) and the vitest setup is SSR-render/source-pin
 * only, so the click-driven contract is pinned here in the offline Playwright
 * suite with a route-mocked `/api/library`.
 *
 * Hooks (values from `e2e/helpers/contract.ts` TESTIDS and the established
 * player contract): `TESTIDS.libraryItem` ('library-item', `data-id` = track
 * id) and `TESTIDS.libraryList` ('library-list') name the library rows; the
 * queue hooks are the literals the demo player and the reusable queue panel
 * already render — `queue-item` (carrying `data-track-id`, falling back to
 * `data-id` as in the neighbouring specs), `now-playing`, and `player-next` —
 * pinned today by `player-queue.spec.ts` and the queue panel's unit contract
 * (empty queue ⇒ `now-playing` reads "Queue is empty" and `player-next` is
 * disabled; the current item carries `aria-current="true"`). They are not
 * TESTIDS entries: the route-contract unit test maps every TESTIDS key to a
 * single route source, and these hooks live in a component.
 *
 * Contract under test (paired build card):
 * - Selecting rows A, B, C in that (gesture) order fills the queue in exactly
 *   A, B, C order; `now-playing` names A and `player-next` is enabled.
 * - Re-selecting an already-queued row is a no-op (the queue store dedupes):
 *   the track appears exactly once.
 * - Activating `player-next` moves `now-playing` to B and marks B current.
 * - With an empty selection the queue reads as empty: `now-playing` shows
 *   "Queue is empty" and `player-next` is disabled.
 * - An auth transition clears the queue (same isolation as
 *   `library-selection-isolation.spec.ts`): nothing leaks into the next
 *   session's queue.
 *
 * Out of scope (not pinned): audio playback, prev/seek, queue reorder.
 */

// Rendered order is API order; ids are deliberately not in sorted order.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song' }
];

const NEXT_ACCOUNT = [{ id: 'row-bravo', title: 'Bravo Song', artist: 'Next Account' }];

/** Route `/api/library` to a mutable payload so a test can swap accounts mid-run. */
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

async function ariaCurrent(locator: Locator): Promise<string | null> {
	return locator.evaluate((element) => element.getAttribute('aria-current'));
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

test('empty selection shows an empty queue with next disabled', async ({ page }) => {
	await seedLibrary(page);

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('player-next')).toBeDisabled();
});

test('selecting rows A, B, C fills the queue in that order with A now playing', async ({
	page
}) => {
	await seedLibrary(page);

	// Gesture order runs against both rendered and sorted order:
	// A = row-mike (3rd row), B = row-zulu (1st), C = row-alpha (2nd).
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();

	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect(page.getByTestId('player-next')).toBeEnabled();

	// The current (first selected) track is marked, the rest are not.
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBeNull();
	expect(await ariaCurrent(queueItem(page, 'row-alpha'))).toBeNull();
});

test('selecting the same row twice does not duplicate it in the queue', async ({ page }) => {
	await seedLibrary(page);

	await row(page, 'row-mike').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	await row(page, 'row-mike').click();

	await expect(page.getByTestId('queue-item')).toHaveCount(1);
	expect(await queueOrder(page)).toEqual(['row-mike']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});

test('player-next advances now-playing to B and marks B current', async ({ page }) => {
	await seedLibrary(page);

	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	await page.getByTestId('player-next').click();

	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBe('true');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBeNull();
	// Advancing must not reorder the queue.
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
});

test('auth transition clears the queue so nothing leaks into the next session', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	await fireAuthTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	payload.current = NEXT_ACCOUNT;
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('player-next')).toBeDisabled();
});
