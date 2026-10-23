import { expect, test, type Locator, type Page } from '@playwright/test';

/**
 * Queue announcement hygiene on /library (QUEUE-MANAGE follow-up).
 *
 * The `queue-announcement` live region is host-owned page state that only a
 * move sets. Any other queue edit must clear it, otherwise the previous
 * "moved to N of M" text goes stale (positions may no longer exist) and a
 * track title can linger across queue clears.
 *
 * Harness note (same as the neighbouring library specs): `/library` is
 * client-fetched (`ssr = false`), so the click-driven contract is pinned
 * here with a route-mocked `/api/library`.
 */

const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' }
];

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

function queueItem(page: Page, id: string): Locator {
	return page.locator(
		`[data-testid="queue-item"][data-track-id="${id}"], [data-testid="queue-item"][data-id="${id}"]`
	);
}

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

/** Fill the queue in gesture order: mike, zulu, alpha. Current is mike. */
async function seedQueue(page: Page): Promise<void> {
	await seedLibrary(page);
	await row(page, 'row-mike').click();
	await row(page, 'row-zulu').click();
	await row(page, 'row-alpha').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await expect(page.getByTestId('now-playing')).toContainText('Mike Song');
}

/** Move alpha up, pinning both the reorder and the move announcement. */
async function moveAlphaUp(page: Page): Promise<Locator> {
	const moveUp = rowButton(page, 'row-alpha', 'Move Alpha Song up');
	await expect(moveUp).toBeVisible();
	await moveUp.click();
	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha', 'row-zulu']);
	const announcement = page.getByTestId('queue-announcement');
	await expect(announcement).toContainText('Alpha Song');
	await expect(announcement).toContainText('2 of 3');
	return announcement;
}

test('remove clears a stale move announcement', async ({ page }) => {
	await seedQueue(page);
	const announcement = await moveAlphaUp(page);

	const remove = rowButton(page, 'row-zulu', 'Remove Zulu Song from queue');
	await expect(remove).toBeVisible();
	await remove.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-alpha']);
	await expect(announcement).toHaveText('');
});

test('play-next clears a stale move announcement', async ({ page }) => {
	await seedQueue(page);
	const announcement = await moveAlphaUp(page);

	const playNext = rowButton(page, 'row-zulu', 'Play Zulu Song next');
	await expect(playNext).toBeVisible();
	await playNext.click();

	expect(await queueOrder(page)).toEqual(['row-mike', 'row-zulu', 'row-alpha']);
	await expect(announcement).toHaveText('');
});

test('clear empties the announcement with the queue', async ({ page }) => {
	await seedQueue(page);
	const announcement = await moveAlphaUp(page);

	const clear = page.getByRole('button', { name: 'Clear queue', exact: true });
	await expect(clear).toBeVisible();
	await clear.click();

	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(announcement).toHaveText('');
});
