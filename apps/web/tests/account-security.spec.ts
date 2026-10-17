import { expect, test } from '@playwright/test';

test.beforeEach(async ({ page }) => {
	await page.route('**/v1/vault', async (route) => {
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({})
		});
	});
});

test('account security confirms before revoking sessions and redirects after success', async ({
	page
}) => {
	let revokePosts = 0;
	await page.route('**/v1/session/revoke-all', async (route) => {
		revokePosts += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		});
	});

	await page.goto('/account/security');
	const revoke = page.getByRole('button', { name: /sign out everywhere/i });
	await expect(revoke).toBeVisible();

	// The first confirmation is declined; no session-revocation request may
	// be sent until the user explicitly accepts the destructive action.
	page.once('dialog', (dialog) => dialog.dismiss());
	await revoke.click();
	expect(revokePosts).toBe(0);

	page.once('dialog', (dialog) => dialog.accept());
	await revoke.click();
	await page.waitForURL('**/login');
	expect(revokePosts).toBe(1);
});

test('account security signs out only the current session with the existing endpoint', async ({
	page
}) => {
	let logoutPosts = 0;
	await page.route('**/v1/session/logout', async (route) => {
		logoutPosts += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true })
		});
	});

	await page.goto('/account/security');
	await page.getByRole('button', { name: /^sign out$/i }).click();

	await expect.poll(() => logoutPosts).toBe(1);
});

test('account security reports a revoke-all failure without success navigation', async ({ page }) => {
	let revokePosts = 0;
	await page.route('**/v1/session/revoke-all', async (route) => {
		revokePosts += 1;
		await route.fulfill({
			status: 500,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'temporary failure' })
		});
	});

	await page.goto('/account/security');
	page.on('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: /sign out everywhere/i }).click();

	await expect(page.getByRole('alert')).toBeVisible();
	await expect(page.getByRole('alert')).not.toContainText(/signed out|success/i);
	expect(revokePosts).toBe(1);
	expect(new URL(page.url()).pathname).toBe('/account/security');
});

test('account security keeps a revoke-all 401 on the page with an expired-session notice', async ({
	page
}) => {
	let revokePosts = 0;
	await page.route('**/v1/session/revoke-all', async (route) => {
		revokePosts += 1;
		await route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'unauthorized' })
		});
	});

	await page.goto('/account/security');
	page.on('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: /sign out everywhere/i }).click();

	// A 401 means the request was rejected before any session was revoked,
	// so the page must not look like the success path (navigation to /login).
	const alert = page.getByRole('alert');
	await expect(alert).toBeVisible();
	await expect(alert).toContainText(/session expired before other devices were signed out/i);
	await expect(alert.getByRole('link', { name: /sign in again/i })).toHaveAttribute('href', '/login');
	expect(revokePosts).toBe(1);
	expect(new URL(page.url()).pathname).toBe('/account/security');
});
