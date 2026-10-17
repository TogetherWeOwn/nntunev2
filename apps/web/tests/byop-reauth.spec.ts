import { expect, test } from '@playwright/test';

test('stale session on provider save directs the user to sign in again', async ({ page }) => {
	await page.route('**/v1/vault/test', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, latencyMs: 12 })
		})
	);
	await page.route('**/v1/vault', (route) =>
		route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({
				code: 'reauth_required',
				message: 'reauth required',
				retryable: false
			})
		})
	);
	await page.route('**/v1/session/reauthenticate/start', (route) =>
		route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'check your email and try again' })
		})
	);

	await page.goto('/onboarding/byop');
	await page.getByLabel('Hostname').fill('news.example.test');
	await page.getByLabel('Username').fill('reader');
	await page.getByLabel('Password').fill('fixture-password');
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByTestId('byop-test-connection').click();
	await expect(page.getByTestId('byop-save')).toBeVisible();
	await page.getByTestId('byop-save').click();

	const recovery = page.getByRole('alert');
	await expect(recovery).toContainText('Your session is too old to save provider details.');
	await expect(recovery).toContainText('Reauthenticate with your passkey to continue.');
	await recovery.getByRole('button', { name: 'Reauthenticate and save' }).click();
	await expect(recovery.getByRole('link', { name: 'Sign in again' })).toHaveAttribute(
		'href',
		'/login'
	);
	await expect(page.getByText('Could not save. Your details were not stored — try again.')).toHaveCount(0);
});
