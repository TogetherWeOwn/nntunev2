import { expect, test } from '@playwright/test';

// Builder-owned integration coverage for bulk addition after Favorites-only
// filtering. The original bulk and favorites acceptance banks stay unchanged.
const LABEL = 'Add displayed tracks to queue';
const ITEMS = [
	{ id: 'visible-z', title: 'Zulu Song' },
	{ id: 'visible-m', title: 'Middle Song' },
	{ id: 'visible-a', title: 'Alpha Song' }
];

test.beforeEach(async ({ page, context }) => {
	await context.route('**/*', (route) => {
		const url = new URL(route.request().url());
		if (!['localhost', '127.0.0.1'].includes(url.hostname)) return route.abort('blockedbyclient');
		if (url.pathname.startsWith('/api/')) return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
		return route.continue();
	});
	await page.route('**/api/library', (route) => route.fulfill({
		contentType: 'application/json', body: JSON.stringify(ITEMS)
	}));
	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(3);
});

test('Favorites-only bulk uses visible sorted rows and retains hidden queued tracks/current', async ({ page }) => {
	for (const title of ['Zulu Song', 'Alpha Song']) {
		await page.getByRole('button', { name: `Star ${title}`, exact: true }).click();
	}
	for (const id of ['visible-m', 'visible-a']) {
		await page.locator(`[data-testid="library-item"][data-id="${id}"]`).getByRole('button').first().click();
	}
	await page.getByTestId('player-next').click();
	await page.getByRole('combobox', { name: 'Order tracks', exact: true }).selectOption('title');
	await page.getByRole('combobox', { name: 'Show', exact: true }).selectOption('favorites');
	await expect(page.getByTestId('library-item')).toHaveCount(2);
	await expect(page.getByTestId('library-item').nth(0)).toHaveAttribute('data-id', 'visible-a');
	await page.getByRole('button', { name: LABEL, exact: true }).click();
	await expect.poll(() => page.getByTestId('queue-item').evaluateAll(
		(rows) => rows.map((row) => row.getAttribute('data-track-id'))
	)).toEqual(['visible-m', 'visible-a', 'visible-z']);
	await expect(page.locator('[data-testid="queue-item"][aria-current="true"]')).toHaveAttribute('data-track-id', 'visible-a');
	await expect(page.getByTestId('queue-announcement')).toHaveText('Added 1 track to the queue.');
	await expect(page.getByRole('button', { name: LABEL, exact: true })).toBeDisabled();
	await expect(page.getByText('All displayed tracks are already in the queue.', { exact: true })).toBeVisible();
});

test('an empty Favorites-only view disables bulk instead of adding hidden library rows', async ({ page }) => {
	await page.getByRole('combobox', { name: 'Show', exact: true }).selectOption('favorites');
	await expect(page.getByTestId('library-item')).toHaveCount(0);
	await expect(page.getByRole('button', { name: LABEL, exact: true })).toBeDisabled();
	await expect(page.getByTestId('queue-item')).toHaveCount(0);
	await expect(page.getByText('All displayed tracks are already in the queue.', { exact: true })).toHaveCount(0);
	await page.getByRole('combobox', { name: 'Show', exact: true }).selectOption('all');
	await expect(page.getByRole('button', { name: LABEL, exact: true })).toBeEnabled();
});
