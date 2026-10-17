import { expect, test, type Page } from '@playwright/test';

/**
 * Paywall fixtures: the provider wizard's "test connection" and pricing
 * checkout calls are routed to mocked 402/429 limit responses; the app must show a paywall
 * modal (upgrade CTA to /pricing), never a dead-end error, and fire the
 * documented `usage_limit_paywall_shown` analytics event with no PII.
 */

async function collectPaywallEvents(page: Page): Promise<void> {
	await page.addInitScript(() => {
		const bucket: Array<Record<string, unknown>> = [];
		(window as unknown as { __paywallEvents: typeof bucket }).__paywallEvents = bucket;
		window.addEventListener('usage_limit_paywall_shown', (event) => {
			bucket.push((event as CustomEvent).detail as Record<string, unknown>);
		});
	});
}

async function toConnectionTestStep(page: Page): Promise<void> {
	await page.goto('/onboarding/provider');
	await page.locator('input[name="host"]').fill('news.example.com');
	await page.locator('input[name="username"]').fill('alice');
	await page.locator('input[name="password"]').fill('s3cret');
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByRole('button', { name: /test connection/i })).toBeVisible();
}

test('pricing checkout limit shows paywall without a duplicate inline alert', async ({ page }) => {
	await collectPaywallEvents(page);
	await page.route('**/v1/billing/checkout', (route) =>
		route.fulfill({
			status: 402,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ code: 'plan_limit_exceeded' })
		})
	);

	await page.goto('/pricing');
	await page.getByRole('button', { name: 'Choose' }).first().click();

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(dialog.getByRole('link', { name: /upgrade/i })).toHaveAttribute('href', '/pricing');
	await dialog.getByRole('link', { name: /upgrade/i }).click();
	await expect(page).toHaveURL(/\/pricing\/?$/);
	await expect(dialog).toBeHidden();

	const events = await page.evaluate(
		() => (window as unknown as { __paywallEvents: Array<Record<string, unknown>> }).__paywallEvents
	);
	expect(events).toHaveLength(1);
	expect(events[0]).toMatchObject({ kind: 'terminal', status: 402, code: 'plan_limit_exceeded' });
});

test('429 limit shows paywall with Retry-After countdown and upgrade CTA', async ({ page }) => {
	await collectPaywallEvents(page);
	await page.route('**/v1/vault/test', (route) =>
		route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '45' },
			body: JSON.stringify({ code: 'rate_limited', message: 'limit reached' })
		})
	);

	await toConnectionTestStep(page);
	await page.getByRole('button', { name: /test connection/i }).click();

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(dialog.getByRole('link', { name: /upgrade/i })).toHaveAttribute('href', '/pricing');
	// Countdown ticks once per second: accept the first seconds of the 45s wait.
	await expect(page.getByTestId('paywall-countdown')).toHaveText(
		/Try again in (?:1[5-9]|[2-3]\d|4[0-5]) seconds\.|You can try again now\./
	);

	const events = await page.evaluate(
		() => (window as unknown as { __paywallEvents: Array<Record<string, unknown>> }).__paywallEvents
	);
	expect(events).toHaveLength(1);
	expect(events[0]).toMatchObject({ kind: 'retryable', status: 429, code: 'rate_limited', retryAfterSeconds: 45 });
	const serialized = JSON.stringify(events[0]).toLowerCase();
	for (const forbidden of ['email', 'userid', 'user_id', 'token', 'password', '@', 'alice', 's3cret']) {
		expect(serialized).not.toContain(forbidden);
	}
});

test('402 limit shows terminal paywall with usage and upgrade CTA, no countdown', async ({ page }) => {
	await collectPaywallEvents(page);
	await page.route('**/v1/vault/test', (route) =>
		route.fulfill({
			status: 402,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({
				code: 'plan_limit_exceeded',
				used_bytes: 2048,
				limit_bytes: 2048,
				plan: 'free'
			})
		})
	);

	await toConnectionTestStep(page);
	await page.getByRole('button', { name: /test connection/i }).click();

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(dialog).toContainText(/reached your plan limit/i);
	await expect(dialog).toContainText(/2\D*048 of 2\D*048/);
	await expect(dialog.getByRole('link', { name: /upgrade/i })).toHaveAttribute('href', '/pricing');
	await expect(page.getByTestId('paywall-countdown')).toHaveCount(0);

	// The modal dismisses back to the wizard (no dead end).
	await dialog.getByRole('button', { name: /not now/i }).click();
	await expect(dialog).toBeHidden();

	const events = await page.evaluate(
		() => (window as unknown as { __paywallEvents: Array<Record<string, unknown>> }).__paywallEvents
	);
	expect(events).toHaveLength(1);
	expect(events[0]).toMatchObject({ kind: 'terminal', status: 402, code: 'plan_limit_exceeded' });
});

test('upgrade CTA closes the paywall before navigating to pricing', async ({ page }) => {
	await page.route('**/v1/vault/test', (route) =>
		route.fulfill({
			status: 402,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ code: 'plan_limit_exceeded' })
		})
	);

	await toConnectionTestStep(page);
	await page.getByRole('button', { name: /test connection/i }).click();

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await dialog.getByRole('link', { name: /upgrade/i }).click();

	await expect(page).toHaveURL(/\/pricing\/?$/);
	await expect(dialog).toBeHidden();
});
