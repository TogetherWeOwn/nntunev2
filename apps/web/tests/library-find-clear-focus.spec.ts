import { expect, test, type Page } from '@playwright/test';

/**
 * Builder-owned complement to the acceptance-owned local-filter browser bank.
 * It protects the focus contract for both clear-search controls without
 * modifying that protected suite.
 */
const ITEMS = [
	{ id: 'focus-alpha', title: 'Alpha Song', artist: 'Harbor Lights' },
	{ id: 'focus-beta', title: 'Beta Song', artist: 'Harbor Lights' }
];

async function mockLibrary(page: Page): Promise<void> {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(ITEMS)
		})
	);
}

function findField(page: Page) {
	return page
		.getByRole('searchbox', { name: /find|search/i })
		.or(page.getByRole('textbox', { name: /find|search/i }));
}

function clearSearch(page: Page) {
	return page.getByRole('button', { name: /clear search/i });
}

test.beforeEach(async ({ context }) => {
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) {
			return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		}
		return route.continue();
	});
});

test('clearing a loaded-tracks find keeps focus on the find field', async ({ page }) => {
	await mockLibrary(page);
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(ITEMS.length);

	const field = findField(page);
	await expect(field).toBeVisible();

	await field.fill('alpha');
	await clearSearch(page).first().click();
	await expect(field).toHaveValue('');
	await expect(field).toBeFocused();

	await field.fill('not a loaded track');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await clearSearch(page).last().click();
	await expect(field).toHaveValue('');
	await expect(field).toBeFocused();
});
