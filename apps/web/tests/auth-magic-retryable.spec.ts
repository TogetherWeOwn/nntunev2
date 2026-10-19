import { expect, test, type Page } from '@playwright/test';

/**
 * Retryable-vs-spent classification pin for `/auth/magic` redeem.
 *
 * Network failures, timeouts and 5xx responses do NOT spend the emailed
 * token, so they must keep the page on the confirm state with a generic
 * "could not reach …, try again" alert (`role="alert"`), show no raw
 * upstream error text, fire no automatic retry, and let the same link
 * redeem on the next user action. Token-specific 4xx responses keep the
 * existing generic expired/used/unknown state (anti-enumeration
 * unchanged). While a redeem is in flight the confirm button is
 * disabled, re-entry is ignored (exactly one request per user action),
 * and the working state is announced politely.
 *
 * Collapsing every failure into the spent-link state fails the 503 /
 * abort / timeout cases; sending token-specific 4xx to the retryable
 * alert fails the generic-state cases; dropping the disabled state,
 * the re-entry guard or the polite announcement fails the held-request
 * case. All fixtures are synthetic; no real mail or tokens.
 */

const FINISH = '**/v1/auth/magic/finish';

function confirmButton(page: Page) {
	return page.getByRole('button', { name: /sign in with this link/i });
}

async function expectRetryableConfirmState(page: Page) {
	const alert = page.getByRole('alert').first();
	await expect(alert).toContainText(/could not reach/i);
	await expect(alert).toContainText(/try again/i);
	// The spent-link state must not appear: the token is unspent.
	await expect(page.getByText(/didn't work/i)).toHaveCount(0);
	// Still in confirm, ready for the user's next action.
	await expect(confirmButton(page)).toBeVisible();
	await expect(confirmButton(page)).toBeEnabled();
}

test('503 on redeem stays on confirm with a retryable alert, then the same link redeems', async ({
	page
}) => {
	let finishPosts = 0;
	await page.route(FINISH, async (route) => {
		finishPosts += 1;
		if (finishPosts === 1) {
			await route.fulfill({
				status: 503,
				contentType: 'application/json',
				body: JSON.stringify({ error: 'RAW_UPSTREAM_DETAIL_SENTINEL' })
			});
		} else {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ ok: true, email: 'retry@example.com' })
			});
		}
	});

	await page.goto('/auth/magic?token=fixture-retryable-503-token');
	await expect(confirmButton(page)).toBeVisible();
	await confirmButton(page).click();

	await expectRetryableConfirmState(page);
	// No raw upstream error text is leaked into the page.
	await expect(page.getByText(/RAW_UPSTREAM_DETAIL_SENTINEL/)).toHaveCount(0);
	// No automatic retry loop: the count stays at one user action.
	await page.waitForTimeout(1000);
	expect(finishPosts).toBe(1);

	await confirmButton(page).click();
	await page.waitForURL('/onboarding/byop');
	expect(finishPosts).toBe(2);
	await expect(page.getByText(/signed in as/i)).toContainText('retry@example.com');
});

test('network failure on redeem stays on confirm with a retryable alert, then the same link redeems', async ({
	page
}) => {
	let finishPosts = 0;
	await page.route(FINISH, async (route) => {
		finishPosts += 1;
		if (finishPosts === 1) {
			await route.abort('failed');
		} else {
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify({ ok: true, email: 'offline@example.com' })
			});
		}
	});

	await page.goto('/auth/magic?token=fixture-retryable-offline-token');
	await expect(confirmButton(page)).toBeVisible();
	await confirmButton(page).click();

	await expectRetryableConfirmState(page);
	await page.waitForTimeout(1000);
	expect(finishPosts).toBe(1);

	await confirmButton(page).click();
	await page.waitForURL('/onboarding/byop');
	expect(finishPosts).toBe(2);
	await expect(page.getByText(/signed in as/i)).toContainText('offline@example.com');
});

test('timed-out redeem stays on confirm with a retryable alert', async ({ page }) => {
	let finishPosts = 0;
	await page.route(FINISH, async (route) => {
		finishPosts += 1;
		await route.abort('timedout');
	});

	await page.goto('/auth/magic?token=fixture-retryable-timeout-token');
	await expect(confirmButton(page)).toBeVisible();
	await confirmButton(page).click();

	await expectRetryableConfirmState(page);
	await page.waitForTimeout(1000);
	expect(finishPosts).toBe(1);
});

for (const status of [400, 404, 410]) {
	test(`${status} on redeem shows the generic spent-link state, not the retryable alert`, async ({
		page
	}) => {
		let finishPosts = 0;
		await page.route(FINISH, async (route) => {
			finishPosts += 1;
			await route.fulfill({
				status,
				contentType: 'application/json',
				body: JSON.stringify({ error: 'check your email and try again' })
			});
		});

		await page.goto(`/auth/magic?token=fixture-spent-${status}-token`);
		await expect(confirmButton(page)).toBeVisible();
		await confirmButton(page).click();

		await expect(page.getByRole('alert').first()).toContainText(/didn't work/i);
		await expect(page.getByText(/could not reach/i)).toHaveCount(0);
		expect(finishPosts).toBe(1);
	});
}

test('held redeem disables confirm, announces working politely, and ignores a second click', async ({
	page
}) => {
	let finishPosts = 0;
	let release!: () => void;
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	await page.route(FINISH, async (route) => {
		finishPosts += 1;
		await gate;
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ ok: true, email: 'held@example.com' })
		});
	});

	await page.goto('/auth/magic?token=fixture-held-token');
	const confirm = confirmButton(page);
	await expect(confirm).toBeVisible();
	await confirm.click();

	// The request is in flight and held by the gate. The confirm button
	// stays mounted but disabled (its label may change to a working
	// label such as "Signing in…"), rather than relying on a guard or
	// unmounting.
	await expect.poll(() => finishPosts).toBe(1);
	const busyConfirm = page.getByRole('button', { name: /sign(ing)? in/i });
	await expect(busyConfirm).toBeDisabled();

	// The working state is announced politely (non-empty live region).
	await expect
		.poll(async () =>
			page.evaluate(() =>
				Array.from(document.querySelectorAll('[aria-live="polite"], [role="status"]'))
					.map((el) => (el.textContent ?? '').trim())
					.filter(Boolean)
					.join(' ')
			)
		)
		.not.toBe('');

	// A second activation attempt is ignored: still one request.
	await busyConfirm.evaluate((el) =>
		el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }))
	);
	await page.waitForTimeout(300);
	expect(finishPosts).toBe(1);

	release();
	await page.waitForURL('/onboarding/byop');
	expect(finishPosts).toBe(1);
	await expect(page.getByText(/signed in as/i)).toContainText('held@example.com');
});
