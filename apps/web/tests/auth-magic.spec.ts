import { expect, test } from '@playwright/test';

/**
 * Login-CSRF gate pin (PR #30 review): the `/auth/magic` landing must never
 * redeem on load — the emailed token is spent only on an explicit confirm
 * click — and the provider wizard must name the signed-in account before
 * any provider credential is typed.
 *
 * Restoring `redeem()` on mount fails the pre-click assertion (one POST
 * already sent, confirm button gone); dropping the wizard identity label
 * fails the final assertion.
 */
test('magic-link landing waits for confirm, redeems once, then names the account', async ({
	page
}) => {
	let finishPosts = 0;
	await page.route('**/v1/auth/magic/finish', async (route) => {
		finishPosts += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, email: 'ada@example.com' })
		});
	});

	await page.goto('/auth/magic?token=fixture-magic-token');
	const confirm = page.getByRole('button', { name: /sign in with this link/i });
	await expect(confirm).toBeVisible();
	// Mount must not redeem: give any on-mount redeem a chance to fire.
	await page.waitForTimeout(500);
	expect(finishPosts).toBe(0);

	await confirm.click();
	await page.waitForURL('/onboarding/provider');
	expect(finishPosts).toBe(1);

	// The wizard names the account before any credential field.
	await expect(page.getByRole('heading', { level: 1 })).toContainText(
		'Connect your provider'
	);
	await expect(page.getByText(/signed in as/i)).toContainText('ada@example.com');
});
