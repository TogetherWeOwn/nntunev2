import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Acceptance: queue management UI on /library (QUEUE-MANAGE, M2).
 *
 * OWNERSHIP: this file is acceptance-owned. The paired build lands the
 * queue-management wiring on top of this branch and must make these tests
 * pass WITHOUT editing this file; a wrong test comes back to the acceptance
 * card instead.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click-driven contract is pinned
 * here in the offline Playwright suite with a route-mocked `/api/library`.
 * Every test guards its first new-control interaction with an explicit
 * visibility assertion, so a run against a build without the controls fails
 * as an assertion (missing control), never as a click error.
 *
 * Hooks: the established queue literals `queue-item` (carrying
 * `data-track-id`, falling back to `data-id` as in the neighbouring specs),
 * `now-playing` and `player-next`, located per-row by accessible name. The
 * management contract this suite defines:
 * - Per-row buttons named `Move {title} up`, `Move {title} down` (matching
 *   the demo player's labels), `Play {title} next`, and
 *   `Remove {title} from queue`.
 * - A queue-level `Clear queue` button (matching the demo player's text),
 *   disabled when the queue is empty.
 * - A `queue-announcement` polite live region (`role="status"`) that names
 *   the moved track and its new 1-based position after a move.
 * The new hook is a component-owned literal like `queue-item`: it is not a
 * TESTIDS entry, because the route-contract unit test maps every TESTIDS
 * key to a single route source and these hooks live in a component.
 *
 * Contract under test (paired build card):
 * - Move up/down reorder the queue; the current *track* (not the slot) keeps
 *   playing and stays `aria-current` highlighted; `player-next` still
 *   advances from the preserved current.
 * - Play-next inserts the track immediately after the current track without
 *   duplicating it; on the current track it is a no-op.
 * - Remove drops the row; removing the current track advances playback to
 *   the following track, or to the new last track when the tail played.
 * - Clear empties the queue into the empty state (`now-playing` reads
 *   "Queue is empty", `player-next` and `Clear queue` disabled).
 * - Edited order and current track survive a background library refresh and
 *   the queue stays usable afterwards.
 * - Controls are labeled (located here by accessible name alone) and
 *   keyboard operable: Tab reaches them, Enter activates them.
 *
 * Out of scope (not pinned): drag-and-drop (buttons only this slice), audio
 * playback and stream URLs, and the background-refresh retention details
 * owned by the sibling retention suite (`library-background-refresh.spec.ts`):
 * DOM-node identity, focus retention, stale badge/Retry, same-id metadata
 * correction, and auth purge. The refresh test here pins only that an
 * *edited* queue keeps its order and current track across a same-id
 * refresh.
 */

// Rendered order is API order; ids are deliberately not in sorted order.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' }
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

/** A management button by its accessible name, scoped to one queue row. */
function rowButton(page: Page, trackId: string, name: string): Locator {
	return queueItem(page, trackId).getByRole('button', { name, exact: true });
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

/** Fill the queue in gesture order: mike, zulu, alpha. Current is mike. */
async function seedQueue(page: Page): Promise<void> {
	await seedLibrary(page);
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
}

test('move up reorders the queue and keeps the current track playing', async ({ page }) => {
	await seedQueue(page);

	const moveUp = rowButton(page, 'row-alpha', 'Move Alpha Song up');
	await expect(moveUp).toBeVisible();
	await moveUp.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	await expect(page.getByTestId('player-next')).toBeEnabled();
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');
	expect(await ariaCurrent(queueItem(page, 'row-alpha'))).toBeNull();

	// Next advances from the preserved current along the new order.
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
});

test('move down keeps the moved current track highlighted', async ({ page }) => {
	await seedQueue(page);

	const moveDown = rowButton(page, 'row-mike', 'Move Mike Song down');
	await expect(moveDown).toBeVisible();
	await moveDown.click();

	expect(await queueOrder(page)).toEqual(['row-zulu', 'row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBeNull();

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
});

test('edge rows disable the move that would leave the queue', async ({ page }) => {
	await seedQueue(page);

	const firstUp = rowButton(page, 'row-mike', 'Move Mike Song up');
	await expect(firstUp).toBeVisible();
	await expect(firstUp).toBeDisabled();
	await expect(rowButton(page, 'row-mike', 'Move Mike Song down')).toBeEnabled();

	await expect(rowButton(page, 'row-zulu', 'Move Zulu Song up')).toBeEnabled();
	await expect(rowButton(page, 'row-zulu', 'Move Zulu Song down')).toBeEnabled();

	await expect(rowButton(page, 'row-alpha', 'Move Alpha Song down')).toBeDisabled();
	await expect(rowButton(page, 'row-alpha', 'Move Alpha Song up')).toBeEnabled();
});

test('play next inserts the track immediately after the current track', async ({ page }) => {
	await seedQueue(page);

	const playNext = rowButton(page, 'row-alpha', 'Play Alpha Song next');
	await expect(playNext).toBeVisible();
	await playNext.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
});

test('play next on the current track leaves the queue unchanged', async ({ page }) => {
	await seedQueue(page);

	const playNext = rowButton(page, 'row-mike', 'Play Mike Song next');
	await expect(playNext).toBeVisible();
	// Enabled or disabled, activating it must not disturb the queue.
	if (await playNext.isEnabled()) await playNext.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});

test('remove drops the row and preserves the current track', async ({ page }) => {
	await seedQueue(page);

	const remove = rowButton(page, 'row-zulu', 'Remove Zulu Song from queue');
	await expect(remove).toBeVisible();
	await remove.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');

	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');
});

test('removing the current track advances playback to the following track', async ({
	page
}) => {
	await seedQueue(page);

	const remove = rowButton(page, 'row-mike', 'Remove Mike Song from queue');
	await expect(remove).toBeVisible();
	await remove.click();

	expect(await queueOrder(page)).toEqual(['row-zulu', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBe('true');
	await expect(page.getByTestId('player-next')).toBeEnabled();
});

test('removing the current tail track falls back to the new last track', async ({ page }) => {
	await seedQueue(page);

	await page.getByTestId('player-next').click();
	await page.getByTestId('player-next').click();
	await expect(page.getByTestId('now-playing')).toContainText('Alpha Song');

	const remove = rowButton(page, 'row-alpha', 'Remove Alpha Song from queue');
	await expect(remove).toBeVisible();
	await remove.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu']);
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
	expect(await ariaCurrent(queueItem(page, 'row-zulu'))).toBe('true');
});

test('clear empties the queue and disables transport', async ({ page }) => {
	await seedQueue(page);

	const clear = page.getByRole('button', { name: 'Clear queue', exact: true });
	await expect(clear).toBeVisible();
	await expect(clear).toBeEnabled();
	await clear.click();
	await page.getByRole('button', { name: 'Yes, clear queue', exact: true }).click();

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('now-playing')).toContainText('Queue is empty');
	await expect(page.getByTestId('player-next')).toBeDisabled();
	await expect(clear).toBeDisabled();
});

test('edited order and current track survive a background library refresh', async ({ page }) => {
	const payload: { current: unknown } = { current: ITEMS };
	await seedLibrary(page, payload);

	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);

	// Edit first: reorder, then remove. Current stays mike throughout.
	await expect(rowButton(page, 'row-alpha', 'Move Alpha Song up')).toBeVisible();
	await rowButton(page, 'row-alpha', 'Move Alpha Song up').click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	await rowButton(page, 'row-zulu', 'Remove Zulu Song from queue').click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');

	// Same-id refresh: the edited queue must come through untouched.
	await page.getByRole('button', { name: 'Refresh', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
	expect(await ariaCurrent(queueItem(page, 'row-mike'))).toBe('true');
	await expect(page.getByTestId('player-next')).toBeEnabled();

	// And the queue stays usable after the refresh settles.
	await rowButton(page, 'row-alpha', 'Move Alpha Song up').click();
	expect(await queueOrder(page)).toEqual(['row-alpha', 'row-mike']);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
});

test('queue controls are labeled and keyboard operable', async ({ page }) => {
	await seedQueue(page);

	// Every control is located by accessible name alone: the label is the pin.
	for (const [trackId, title] of [
		['row-mike', 'Mike Song'],
		['row-zulu', 'Zulu Song'],
		['row-alpha', 'Alpha Song']
	]) {
		await expect(rowButton(page, trackId, `Move ${title} up`)).toBeVisible();
		await expect(rowButton(page, trackId, `Move ${title} down`)).toBeVisible();
		await expect(rowButton(page, trackId, `Play ${title} next`)).toBeVisible();
		await expect(rowButton(page, trackId, `Remove ${title} from queue`)).toBeVisible();
	}
	await expect(page.getByRole('button', { name: 'Clear queue', exact: true })).toBeVisible();

	// Tab from a row control reaches another control: nothing is
	// tabindex-removed. DOM order between controls is the builder's choice,
	// so this pins reachability, not sequence.
	const start = rowButton(page, 'row-zulu', 'Move Zulu Song up');
	await start.focus();
	await expect(start).toBeFocused();
	await page.keyboard.press('Tab');
	const focused = await page.evaluate(() => {
		const el = document.activeElement as HTMLElement | null;
		return { tag: el?.tagName ?? '', name: el?.getAttribute('aria-label') ?? el?.textContent ?? '' };
	});
	expect(focused.tag).toBe('BUTTON');
	expect(focused.name.trim().length).toBeGreaterThan(0);
	expect(focused.name).not.toContain('Move Zulu Song up');

	// Enter on a focused move control performs the move.
	const keyboardMove = rowButton(page, 'row-alpha', 'Move Alpha Song up');
	await keyboardMove.focus();
	await page.keyboard.press('Enter');
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
});

test('keyboard moves keep focus on the same track for consecutive Enter presses', async ({ page }) => {
	await seedQueue(page);

	const moveAlpha = rowButton(page, 'row-alpha', 'Move Alpha Song up');
	await expect(moveAlpha).toBeVisible();
	await moveAlpha.focus();
	await expect(moveAlpha).toBeFocused();
	await page.keyboard.press('Enter');

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	// Check before Alpha reaches the top and its up control becomes disabled.
	await expect(moveAlpha).toBeFocused();
	await page.keyboard.press('Enter');
	expect(await queueOrder(page)).toEqual(['row-alpha', 'row-mike', 'row-zulu']);
});

test('moves are announced in a polite live region', async ({ page }) => {
	await seedQueue(page);

	const announcement = page.getByTestId('queue-announcement');
	await expect(announcement).toBeAttached();
	await expect(announcement).toHaveAttribute('role', 'status');

	const moveUp = rowButton(page, 'row-alpha', 'Move Alpha Song up');
	await expect(moveUp).toBeVisible();
	await moveUp.click();

	// The announcement names the moved track and its new 1-based position.
	await expect(announcement).toContainText('Alpha Song');
	await expect(announcement).toContainText('2 of 3');
});
