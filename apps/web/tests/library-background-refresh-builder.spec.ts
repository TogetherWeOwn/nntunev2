import { expect, test, type Page } from '@playwright/test';

// Builder-owned regressions for the review's surviving key/status mutants.
// Hold only the API boundary: the real refresher, page and queue render run.
const ITEMS = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Queue Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' },
	{ id: 'row-mike', title: 'Mike Song', artist: 'Queue Artist' }
];

async function loadedLibrary(page: Page) {
	await page.route('**/api/library', (route) => route.fulfill({ json: ITEMS }));
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
}

async function heldRefresh(page: Page, payload: unknown) {
	let release!: () => void;
	const gate = new Promise<void>((resolve) => { release = resolve; });
	let seen!: () => void;
	const arrived = new Promise<void>((resolve) => { seen = resolve; });
	await page.unroute('**/api/library');
	await page.route('**/api/library', async (route) => {
		seen();
		await gate;
		await route.fulfill({ json: payload });
	});
	return { arrived, release };
}

test('refreshing status is visible only while a loaded-library refresh is pending', async ({ page }) => {
	await loadedLibrary(page);
	const status = page.getByRole('status').filter({ hasText: 'Refreshing your library…' });
	await expect(status).toHaveCount(0);
	const hold = await heldRefresh(page, ITEMS);
	try {
		await page.getByRole('button', { name: 'Refresh', exact: true }).click();
		await hold.arrived;
		await expect(status).toBeVisible();
		await expect(page.getByTestId('library-list')).toHaveAttribute('aria-busy', 'true');
		await expect(page.getByRole('button', { name: 'Refresh', exact: true })).toBeDisabled();
	} finally {
		hold.release();
	}
	await expect(page.getByTestId('library-list')).toHaveAttribute('aria-busy', 'false');
	await expect(status).toHaveCount(0);
});

test('removing an earlier queue entry preserves the surviving current track node and focus', async ({ page }) => {
	await loadedLibrary(page);
	for (const id of ['row-mike', 'row-zulu', 'row-alpha']) {
		await page.locator(`[data-testid="library-item"][data-id="${id}"] .nt-library-row`).click();
	}
	await page.getByTestId('player-next').click();
	const survivor = page.locator('[data-testid="queue-item"][data-track-id="row-zulu"]');
	await expect(survivor).toHaveAttribute('aria-current', 'true');
	// Mark only this track: marking every node identically would let an
	// index-keyed row inherit an earlier track's indistinguishable marker.
	await survivor.evaluate((element) => { (element as HTMLElement).dataset.refreshIdentity = 'zulu-only'; });
	await survivor.focus();
	const hold = await heldRefresh(page, [ITEMS[0], ITEMS[1]]);
	try {
		await page.getByRole('button', { name: 'Refresh', exact: true })
			.evaluate((element) => (element as HTMLElement).click());
		await hold.arrived;
		await expect(page.getByTestId('library-list')).toHaveAttribute('aria-busy', 'true');
		await expect(survivor).toBeFocused();
	} finally {
		hold.release();
	}
	// Positive settle barrier: a route fulfillment alone is not a render.
	await expect(page.getByTestId('library-list')).toHaveAttribute('aria-busy', 'false');
	await expect(page.getByTestId('queue-item')).toHaveCount(2);
	await expect(survivor).toHaveAttribute('data-refresh-identity', 'zulu-only');
	await expect(survivor).toBeFocused();
	await expect(survivor).toHaveAttribute('aria-current', 'true');
	await expect(page.getByTestId('now-playing')).toContainText('Zulu Song');
});
