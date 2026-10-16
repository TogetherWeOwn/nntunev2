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

/**
 * Swap-guard recovery pin, fresh tab (PR #30 review finding 1): the error
 * branch must offer sign-out even when this tab never saw the
 * sessionStorage label (magic links normally open in a new tab), and the
 * unspent link must redeem after a real sign-out.
 *
 * Deleting the error-branch sign-out button fails the sign-out visibility
 * assertion; gating it on the label fails it too (no label in this tab).
 */
test('swap-guarded link offers sign-out with no label and redeems after sign-out', async ({
	page
}) => {
	let finishPosts = 0;
	await page.route('**/v1/auth/magic/finish', async (route) => {
		finishPosts += 1;
		if (finishPosts === 1) {
			await route.fulfill({
				status: 401,
				contentType: 'application/json',
				body: JSON.stringify({ error: 'check your email and try again' })
			});
		} else {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ ok: true, email: 'bob@example.com' })
			});
		}
	});
	let logoutPosts = 0;
	await page.route('**/v1/session/logout', async (route) => {
		logoutPosts += 1;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({})
		});
	});

	// Fresh tab: stash nothing, so the personalised label is absent.
	await page.goto('/');
	await page.evaluate(() => sessionStorage.clear());
	await page.goto('/auth/magic?token=fixture-swap-token');
	const confirm = page.getByRole('button', { name: /sign in with this link/i });
	await expect(confirm).toBeVisible();
	await confirm.click();

	await expect(page.getByRole('alert').first()).toContainText(/didn't work/i);
	const signOut = page.getByRole('button', {
		name: /sign out and try this link again/i
	});
	await expect(signOut).toBeVisible();
	await signOut.click();
	expect(logoutPosts).toBe(1);

	await expect(page.getByText(/signed out/i).first()).toBeVisible();
	await page.getByRole('button', { name: /try this link again/i }).click();

	const retry = page.getByRole('button', { name: /sign in with this link/i });
	await expect(retry).toBeVisible();
	await retry.click();
	await page.waitForURL('/onboarding/provider');
	expect(finishPosts).toBe(2);
	await expect(page.getByText(/signed in as/i)).toContainText('bob@example.com');
});

/**
 * Failed-logout pin (PR #30 review finding 2): a 503 (or network/Origin
 * failure) on POST /v1/session/logout must keep the recovery button and
 * show "Could not sign out. Try again." — never the signed-out success
 * state. Clearing the label on failure fails the button-still-visible
 * assertion.
 */
test('failed logout keeps the sign-out action and reports the failure', async ({
	page
}) => {
	await page.route('**/v1/auth/magic/finish', async (route) => {
		await route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'check your email and try again' })
		});
	});
	let logoutPosts = 0;
	await page.route('**/v1/session/logout', async (route) => {
		logoutPosts += 1;
		await route.fulfill({
			status: 503,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'try again' })
		});
	});

	await page.goto('/');
	await page.evaluate(() => sessionStorage.clear());
	await page.goto('/auth/magic?token=fixture-swap-token');
	await page.getByRole('button', { name: /sign in with this link/i }).click();

	await expect(page.getByRole('alert').first()).toContainText(/didn't work/i);
	const signOut = page.getByRole('button', { name: /sign out/i });
	await expect(signOut).toBeVisible();
	await signOut.click();
	expect(logoutPosts).toBe(1);

	await expect(page.getByText(/could not sign out/i)).toBeVisible();
	// Success copy must not appear, and the button stays enabled for retry.
	await expect(page.getByText(/signed out\. if that link/i)).toHaveCount(0);
	await expect(page.getByRole('button', { name: /sign out/i })).toBeEnabled();
});

test('401 logout clears a stale account label and leaves the link retryable', async ({ page }) => {
	let finishPosts = 0;
	let logoutPosts = 0;
	await page.route('**/v1/auth/magic/finish', async (route) => {
		finishPosts += 1;
		await route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'check your email and try again' })
		});
	});
	await page.route('**/v1/session/logout', async (route) => {
		logoutPosts += 1;
		await route.fulfill({
			status: 401,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'check your email and try again' })
		});
	});

	await page.goto('/');
	await page.evaluate(() => sessionStorage.setItem('nntune:signed-in-email', 'stale@example.com'));
	await page.goto('/auth/magic?token=fixture-expired-token');
	await page.getByRole('button', { name: /sign in with this link/i }).click();
	await expect(page.getByRole('alert').first()).toContainText('stale@example.com');

	await page.getByRole('button', { name: /sign out stale@example.com/i }).click();
	expect(logoutPosts).toBe(1);
	await expect(page.getByText(/signed out\. if that link/i)).toBeVisible();
	await page.getByRole('button', { name: /try this link again/i }).click();
	await page.getByRole('button', { name: /sign in with this link/i }).click();

	await expect(page.getByRole('alert').first()).toContainText(/didn't work/i);
	await expect(page.getByRole('alert').first()).not.toContainText('stale@example.com');
	await expect(page.getByText(/could not sign out/i)).toHaveCount(0);
	expect(finishPosts).toBe(2);
});
