import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Builder-owned companion to the acceptance spec
 * `library-player-queue.spec.ts` (which must stay unmodified): pins the two
 * library→queue behaviours the acceptance half leaves unexercised.
 *
 * - Refresh reconcile keeps a surviving current track current: after a
 *   refresh drops an earlier-queued id, the queue keeps gesture order and
 *   `now-playing`/`aria-current` still name the advanced-to track instead
 *   of falling back to the first kept track.
 * - The queue's current track reveals the placeholder player: `audio-player`
 *   is absent with an empty queue and attached (with an empty `src`) once
 *   a row is selected. No playback wiring — the streaming slice owns that.
 *
 * Harness note (same as the acceptance spec): `/library` is client-fetched
 * (`ssr = false`), so the click-driven contract is pinned here in the
 * offline Playwright suite with a route-mocked `/api/library`.
 */

// Rendered order is API order; ids are deliberately not in sorted order.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song' }
];

/** Refresh payload that drops the first-selected id (row-mike). */
const WITHOUT_MIKE = [ITEMS[0], ITEMS[1]];

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

test('refresh to a subset keeps the surviving current track current', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	// Queue mike, zulu, alpha in gesture order, then advance Next to alpha.
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await page.getByTestId('player-next').click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');

	// Refresh drops row-mike: the queue keeps zulu+alpha with alpha current.
	payload.current = WITHOUT_MIKE;
	await page.getByRole('button', { name: 'Refresh' }).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
	expect(await queueOrder(page)).toEqual(['row-zulu', 'row-alpha']);
	expect(await ariaCurrent(queueItem(page, 'row-alpha'))).toBe('true');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBeNull();
});

test('selecting a row reveals the audio-player placeholder with an empty src', async ({
	page
}) => {
	await seedLibrary(page);

	// No selection: the Player section stays hidden.
	await expect(page.getByTestId('audio-player')).toHaveCount(0);

	await row(page, 'row-mike').click();

	// The queue-derived current track renders the placeholder player.
	const player = page.getByTestId('audio-player');
	await expect(player).toBeAttached();
	await expect(player).toHaveAttribute('src', '');
});
