import { expect, test } from '@playwright/test';

test('signed-out billing visitor reaches the sign-in page via the Sign in link', async ({
	page
}) => {
	for (const path of ['**/v1/me/subscription', '**/v1/me/usage']) {
		await page.route(path, (route) =>
			route.fulfill({
				status: 401,
				contentType: 'application/json',
				body: JSON.stringify({ error: 'unauthorized' })
			})
		);
	}

	await page.goto('/account/billing');

	const signIn = page.getByRole('link', { name: 'Sign in' });
	await expect(signIn).toHaveAttribute('href', '/login');
	await expect(page.getByRole('button', { name: /manage billing/i })).toHaveCount(0);
	await expect(page.getByRole('button', { name: /upgrade to/i })).toHaveCount(0);

	await signIn.focus();
	await page.keyboard.press('Enter');

	await expect(page).toHaveURL(/\/login$/);
	await expect(page.getByRole('heading', { level: 1 })).toContainText('Sign in');
	await expect(page.getByRole('button', { name: /sign in with passkey/i })).toBeVisible();
	await expect(page.locator('input[type="email"]')).toBeVisible();
});
