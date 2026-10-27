import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned companion to the protected queue-filter acceptance spec
 * `library-queue-filter.spec.ts` (which must stay unmodified): pins the two
 * filter guards the acceptance half leaves unexercised.
 *
 * - Explicit "Show current track" while the current row is already visible
 *   keeps the filter query and focuses the real row; only a hiding filter
 *   is cleared.
 * - A same-account refresh restores focus to the filter input only when the
 *   input held focus at refresh start; focus placed elsewhere is never
 *   stolen into the input.
 *
 * Harness note (same as the protected spec): `/library` is client-fetched,
 * so the click-driven contract is pinned here with a route-mocked
 * `/api/library`. All API responses are local; no live providers.
 *
 * Setup: pnpm --dir apps/web install --frozen-lockfile
 *        pnpm --dir apps/web exec playwright install chromium
 * Run: CI=1 PUBLIC_API_BASE_URL='' pnpm --dir apps/web exec playwright test \
 *        tests/library-queue-filter-pins.spec.ts --workers=1
 * Port 4173 must be free (never reuse another worktree's preview).
 */

const ITEMS = [
	{ id: 'pin-coral', title: 'Coral Night', artist: 'Mira' },
	{ id: 'pin-ember', title: 'Ember Noon', artist: 'South' },
	{ id: 'pin-drift', title: 'Drift Evening', artist: 'Mira' }
];

const input = (page: Page) =>
	page
		.getByRole('textbox', { name: 'Filter queue', exact: true })
		.or(page.getByRole('searchbox', { name: 'Filter queue', exact: true }));
const clearFilter = (page: Page) =>
	page.getByRole('button', { name: 'Clear queue filter', exact: true });
const current = (page: Page) =>
	page.locator('[data-testid="queue-item"][aria-current="true"]');

async function settle(page: Page) {
	await page.evaluate(
		() =>
			new Promise<void>((resolve) =>
				requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
			)
	);
}

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

/** Seed the library, queue every row in order, advance Next to Ember Noon. */
async function seed(page: Page) {
	const payload: { current: unknown } = { current: ITEMS };
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);
	for (const item of ITEMS) {
		await page
			.locator(`[data-testid="library-item"][data-id="${item.id}"] button.nt-library-row`)
			.dispatchEvent('click');
	}
	await expect(page.getByTestId('queue-item')).toHaveCount(3);
	await page.getByTestId('player-next').click();
	await expect(current(page)).toHaveAttribute('data-track-id', 'pin-ember');
}

test('reveal with a visible current track preserves the query and focuses the row', async ({
	page
}) => {
	await seed(page);
	await expect(input(page)).toBeVisible({ timeout: 1500 });
	await input(page).fill('ember');
	await expect(page.getByTestId('queue-filter-count')).toHaveText('1 of 3 tracks');
	await expect(current(page)).toHaveAttribute('data-track-id', 'pin-ember');

	await page.getByRole('button', { name: 'Show current track', exact: true }).click();

	await expect(input(page)).toHaveValue('ember');
	await expect(page.getByTestId('queue-filter-count')).toHaveText('1 of 3 tracks');
	await expect(current(page)).toBeFocused();
	await expect(current(page)).toBeInViewport();
});

test('refresh with focus outside the filter leaves the input unfocused', async ({ page }) => {
	await seed(page);
	await expect(input(page)).toBeVisible({ timeout: 1500 });
	await input(page).fill('mira');
	await expect(page.getByTestId('queue-filter-count')).toHaveText('2 of 3 tracks');
	await clearFilter(page).focus();
	await expect(clearFilter(page)).toBeFocused();

	// A DOM-dispatched refresh represents a background refresh without the
	// runner moving focus to Refresh. The real page/refresher still execute.
	const refreshed = page.waitForResponse(
		(response) => new URL(response.url()).pathname === '/api/library'
	);
	await page.getByRole('button', { name: 'Refresh', exact: true }).dispatchEvent('click');
	await refreshed;
	await settle(page);

	await expect(input(page)).toHaveValue('mira');
	await expect(input(page)).not.toBeFocused();
	await expect(page.getByTestId('queue-filter-count')).toHaveText('2 of 3 tracks');
});
