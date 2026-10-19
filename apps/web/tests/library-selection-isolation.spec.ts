import { expect, test, type Page } from '@playwright/test';

/**
 * Selection isolation + row accessible names (companion to the paired
 * acceptance spec `library-selectable.spec.ts`, which pins click/keyboard
 * selection order and must stay unmodified).
 *
 * - Row buttons expose the visible title AND artist as their accessible
 *   name, so same-title tracks by different artists stay distinguishable
 *   to assistive technology (no `aria-label` override).
 * - Selections are per-account: an auth transition or a 401 clears them,
 *   so the next session's Retry never renders the previous account's
 *   queue (the same isolation invariant `library-cache.spec.ts` pins
 *   for items and snapshots).
 */

const ACCOUNT_A = [
	{ id: 'row-zulu', title: 'Zulu Song', artist: 'Select Artist' },
	{ id: 'row-alpha', title: 'Alpha Song' }
];

const ACCOUNT_B = [{ id: 'row-bravo', title: 'Bravo Song', artist: 'Next Account' }];

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

async function fireAuthTransition(page: Page): Promise<void> {
	const otherTab = await page.context().newPage();
	await otherTab.goto('/login');
	await otherTab.evaluate((key) => {
		window.localStorage.setItem(key, 'test-auth-transition');
		window.localStorage.removeItem(key);
	}, 'nntune.library.auth-transition.v1');
	await otherTab.close();
}

test('row buttons expose the visible title and artist as their accessible name', async ({
	page
}) => {
	await routeLibrary(page, { current: ACCOUNT_A });
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ACCOUNT_A.length);

	const withArtist = page.locator('[data-testid="library-item"][data-id="row-zulu"] button');
	await expect(withArtist).toHaveAccessibleName(/Zulu Song\s+—\s+Select Artist/);

	const withoutArtist = page.locator('[data-testid="library-item"][data-id="row-alpha"] button');
	await expect(withoutArtist).toHaveAccessibleName('Alpha Song');
});

test('auth transition clears selections and Retry loads the next account clean', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ACCOUNT_A };
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ACCOUNT_A.length);

	await page.locator('[data-testid="library-item"][data-id="row-zulu"]').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	await fireAuthTransition(page);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);

	payload.current = ACCOUNT_B;
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(ACCOUNT_B.length);
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	await expect(page.getByTestId('queue-list')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});

test('401 clears selections instead of carrying them into the next session', async ({
	page
}) => {
	const payload: { current: unknown } = { current: ACCOUNT_A };
	await routeLibrary(page, payload);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ACCOUNT_A.length);

	await page.locator('[data-testid="library-item"][data-id="row-alpha"]').click();
	await expect(page.getByTestId('queue-item')).toHaveCount(1);

	await page.unroute('**/api/library');
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'unauthorized' })
		})
	);
	await page.getByRole('button', { name: 'Refresh' }).click();
	await expect(page.getByRole('alert')).toContainText(/signed out|do not have access/i);

	await page.unroute('**/api/library');
	payload.current = ACCOUNT_B;
	await routeLibrary(page, payload);
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByTestId('library-list')).toContainText('Bravo Song');
	await expect(page.getByTestId('queue-list')).toHaveCount(0);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
});
