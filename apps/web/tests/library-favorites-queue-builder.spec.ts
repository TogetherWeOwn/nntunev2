import { expect, test, type Page } from '@playwright/test';

const AUTH_CONTEXT_KEY = 'nntune.library.auth-context.v1';
const AUTH_TRANSITION_KEY = 'nntune.library.auth-transition.v1';
const ITEMS = [{ id: 'builder-track', title: 'Builder Song' }];

async function seedLibrary(page: Page): Promise<void> {
	await page.route('**/api/library', (route) => route.fulfill({
		status: 200,
		contentType: 'application/json',
		body: JSON.stringify(ITEMS)
	}));
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(1);
}

test('ghost-only stored favorites show the empty shelf without a bulk queue action', async ({ page }) => {
	await page.addInitScript((contextKey) => {
		window.sessionStorage.setItem(contextKey, 'builder-ghost-only');
		window.localStorage.setItem(
			'nntune.library.favorites.v1.builder-ghost-only', JSON.stringify(['absent-track'])
		);
	}, AUTH_CONTEXT_KEY);
	await seedLibrary(page);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expect(page.getByTestId('favorite-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-section').getByRole('button', { name: 'Queue favorites' })).toHaveCount(0);
});

test('the bulk queue outcome cannot survive an account-boundary retry', async ({ page }) => {
	await seedLibrary(page);
	await page.getByTestId('library-item').getByRole('button', { name: 'Star Builder Song' }).click();
	await page.getByRole('button', { name: 'Queue favorites', exact: true }).click();
	await expect(page.getByTestId('queue-announcement')).toContainText(/queued 1.*0 already queued/i);

	// Exercise the real registered storage listener. The acceptance suite
	// separately checks delivery from a second tab and the scoped-key purge.
	await page.evaluate((key) => {
		window.dispatchEvent(new StorageEvent('storage', { key }));
	}, AUTH_TRANSITION_KEY);
	await expect(page.getByRole('alert')).toContainText(/session changed/i);
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByTestId('favorites-empty')).toBeVisible();
	await expect(page.getByTestId('queue-announcement')).toHaveText('');
});
