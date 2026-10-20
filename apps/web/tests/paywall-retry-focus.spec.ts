import { expect, test } from '@playwright/test';

test('Retry keeps meaningful focus while the repeated provider test is pending', async ({ page }) => {
	let requests = 0;
	let release!: () => void;
	const gate = new Promise<void>((resolve) => { release = resolve; });
	await page.route('**/v1/vault/test', async (route) => {
		requests += 1;
		if (requests > 1) await gate;
		await route.fulfill({
			status: requests === 1 ? 429 : 200,
			headers: { 'content-type': 'application/json', 'Retry-After': '0' },
			body: JSON.stringify(requests === 1 ? { code: 'rate_limited' } : { ok: true, latencyMs: 12 })
		});
	});
	try {
		await page.goto('/onboarding/provider');
		await page.locator('[name=host]').fill('news.example.test');
		await page.locator('[name=username]').fill('fixture-reader');
		await page.locator('[name=password]').fill('fixture-password');
		await page.getByRole('button', { name: 'Continue', exact: true }).click();
		await page.getByRole('button', { name: 'Continue', exact: true }).click();
		await page.getByRole('button', { name: 'Test connection', exact: true }).click();
		const dialog = page.getByRole('dialog');
		await expect(dialog).toBeVisible();
		await dialog.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(dialog).toBeHidden();
		await expect(page.getByTestId('byop-test-connection')).toBeDisabled();
		await expect(page.getByRole('main')).toBeFocused();
		expect(requests).toBe(2);
	} finally {
		release();
	}
	await expect(page.getByTestId('byop-save')).toBeVisible();
	expect(requests).toBe(2);
});

test('Retry without a UI action dismisses and restores meaningful focus, without replay', async ({ page }) => {
	let requests = 0;
	await page.route('**/v1/me/subscription', (route) => {
		requests += 1;
		return route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '0' },
			body: JSON.stringify({ code: 'rate_limited' })
		});
	});
	await page.route('**/v1/me/usage', (route) => route.fulfill({
		status: 500, contentType: 'application/json', body: JSON.stringify({ code: 'internal' })
	}));
	await page.goto('/account/billing');
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await dialog.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(dialog).toBeHidden();
	await expect(page.getByRole('main')).toBeFocused();
	expect(requests).toBe(1);
});
