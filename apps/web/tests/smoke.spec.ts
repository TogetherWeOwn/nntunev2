import { expect, test } from '@playwright/test';

test('landing explains BYOP and links to signup and plans', async ({ page }) => {
	await page.goto('/');

	await expect(page.getByRole('heading', { level: 1 })).toContainText('NNTune');
	// \s+ because SSR may wrap the copy across lines.
	await expect(page.getByRole('main')).toContainText(/bring your\s+own/i);
	await expect(page.getByRole('link', { name: /get started/i })).toHaveAttribute('href', '/signup');
	await expect(page.getByRole('link', { name: /plans/i }).first()).toBeVisible();
});

test('health endpoint reports ok', async ({ request }) => {
	const response = await request.get('/api/health');

	expect(response.ok()).toBe(true);
	await expect(response.json()).resolves.toEqual({ ok: true });
});
