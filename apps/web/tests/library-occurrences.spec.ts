import { expect, test } from '@playwright/test';

const rows = [
	{ id: 'recording-a', occurrence_key: 'job-a:1', title: 'Echo One' },
	{ id: 'recording-a', occurrence_key: 'job-a:2', title: 'Echo Two' },
	{ id: 'recording-a', occurrence_key: 'job-b:1', title: 'Echo Three' }
];

for (const path of ['/library', '/home', '/search?q=Echo', '/library/liked']) {
	test(`catalogue occurrences render without duplicate keys on ${path}`, async ({ page }) => {
		const errors: Error[] = [];
		page.on('pageerror', (error) => errors.push(error));
		await page.addInitScript(() => {
			const context = 'occurrence-test-context';
			sessionStorage.setItem('nntune.library.auth-context.v1', context);
			localStorage.setItem(`nntune.library.favorites.v1.${context}`, JSON.stringify(['recording-a']));
		});
		await page.route('**/api/library', (route) => route.fulfill({ json: rows }));
		await page.goto(path);
		if (path === '/library') {
			await expect(page.getByTestId('library-item')).toHaveCount(3);
			await expect(page.getByTestId('favorite-item')).toHaveCount(1);
		} else {
			const items = page.locator('main ul').first().locator(':scope > li');
			await expect(items).toHaveCount(3);
			await expect(items).toContainText(['Echo One', 'Echo Two', 'Echo Three']);
		}
		expect(errors).toEqual([]);
	});
}
