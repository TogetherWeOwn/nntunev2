import { expect, test } from '@playwright/test';

// Supplement the acceptance suite without changing its protected assertions.
test('cache values belong to their named columns, not just their plan row', async ({ page }) => {
	await page.goto('/pricing');
	const table = page.getByRole('table');
	const headers = await table.getByRole('columnheader').allTextContents();
	const expectedRows = [
		{ name: /Free trial/, budget: '2 GiB', retention: '7 days', priority: 'low' },
		{ name: /Plus/, budget: '20 GiB', retention: '30 days', priority: 'standard' },
		{ name: /Pro 5x/, budget: '100 GiB', retention: '90 days', priority: 'high' },
		{ name: /Pro 20x/, budget: '400 GiB', retention: '90 days', priority: 'highest' }
	];

	for (const plan of expectedRows) {
		const row = table.getByRole('row', { name: plan.name });
		for (const [header, value] of [
			['Cache budget', plan.budget],
			['Cache retention', plan.retention],
			['Cache priority', plan.priority]
		]) {
			const column = headers.findIndex((text) => text.trim() === header);
			expect(column, `${header} column exists`).toBeGreaterThanOrEqual(0);
			// Include the row header so cell indexes align with the column headers.
			await expect(row.locator('th, td').nth(column), `${plan.name}: ${header}`).toHaveText(value);
		}
	}
});

for (const [claim, wording] of [
	['enforcement is not yet active', /not yet enforced/i],
	['cache is not an offline guarantee', /not an offline guarantee/i],
	['cache is not provider storage', /not (extra )?provider storage/i]
] as const) {
	test(`cache explanation independently states ${claim}`, async ({ page }) => {
		await page.goto('/pricing');
		await expect(page.getByRole('region', { name: 'About the plan cache' })).toContainText(wording);
	});
}
