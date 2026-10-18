import { expect, test, type Page } from '@playwright/test';

/**
 * Plan-limit paywall smoke.
 *
 * Drives the pricing/upgrade CTA against mocked 402 and 429 checkout
 * responses and asserts the trial and usage links resolve to real routes
 * (no 404, no placeholder href). Runs in CI with mocked routes only —
 * no staging dependency.
 */

const PLACEHOLDER_PATTERNS = ['placeholder', 'todo', 'example.com', 'javascript:'];

async function collectPaywallEvents(page: Page): Promise<void> {
	await page.addInitScript(() => {
		const bucket: Array<Record<string, unknown>> = [];
		(window as unknown as { __paywallEvents: typeof bucket }).__paywallEvents = bucket;
		window.addEventListener('usage_limit_paywall_shown', (event) => {
			bucket.push((event as CustomEvent).detail as Record<string, unknown>);
		});
	});
}

async function assertPricingActionsResolve(page: Page): Promise<void> {
	// The pricing page must always offer a way forward (trial signup,
	// current-usage check) even when checkout itself is limit-blocked.
	for (const { name, expectedHref } of [
		{ name: 'Start trial', expectedHref: '/signup?plan=free' },
		{ name: 'Check your current usage', expectedHref: '/account/billing' }
	]) {
		const link = page.getByRole('link', { name, exact: true });
		await expect(link, `pricing page must expose exactly one "${name}" link`).toHaveCount(1);
		await expect(link).toBeVisible();
		await expect(link, `${name} link must point to ${expectedHref}`).toHaveAttribute(
			'href',
			expectedHref
		);
		const href = await link.getAttribute('href');
		expect(href, `${name} link should have an href`).toBeTruthy();
		if (!href) continue;
		for (const placeholder of PLACEHOLDER_PATTERNS) {
			expect(
				href.toLowerCase(),
				`CTA href "${href}" must not point at a placeholder`
			).not.toContain(placeholder);
		}
		const response = await page.request.get(href);
		expect(response.status(), `${href} should resolve without a client or server error`).toBeLessThan(400);
	}
}

async function choosePlus(page: Page): Promise<void> {
	await page.goto('/pricing');
	await page
		.getByRole('row', { name: /Plus/ })
		.getByRole('button', { name: /^Choose$/ })
		.click();
}

test('mocked 402 checkout keeps pricing actions reachable', async ({ page }) => {
	await collectPaywallEvents(page);
	let checkoutRequests = 0;
	await page.route('**/v1/billing/checkout', (route) => {
		checkoutRequests += 1;
		return route.fulfill({
			status: 402,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ error: 'plan_limit_exceeded' })
		});
	});

	await choosePlus(page);

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(dialog.getByRole('heading')).toHaveText("You've reached your plan limit");
	await expect(dialog.getByRole('link', { name: /upgrade/i })).toHaveAttribute('href', '/pricing');
	expect(checkoutRequests, 'checkout mock must intercept the request').toBeGreaterThan(0);
	await dialog.getByRole('button', { name: /not now/i }).click();
	await expect(dialog).toBeHidden();
	await assertPricingActionsResolve(page);

	const events = await page.evaluate(
		() => (window as unknown as { __paywallEvents: Array<Record<string, unknown>> }).__paywallEvents
	);
	expect(events).toHaveLength(1);
	expect(events[0]).toMatchObject({ kind: 'terminal', status: 402, code: 'plan_limit_exceeded' });
});

test('mocked 429 checkout keeps pricing actions reachable', async ({ page }) => {
	await collectPaywallEvents(page);
	let checkoutRequests = 0;
	await page.route('**/v1/billing/checkout', (route) => {
		checkoutRequests += 1;
		return route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '45' },
			body: JSON.stringify({ error: 'rate_limited' })
		});
	});

	await choosePlus(page);

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(page.getByRole('alert')).toHaveCount(0);
	await expect(dialog.getByRole('heading')).toHaveText('Please slow down a little');
	await expect(dialog.getByRole('link', { name: /upgrade/i })).toHaveAttribute('href', '/pricing');
	await expect(page.getByTestId('paywall-countdown')).toHaveText(
		/Try again in (?:1[5-9]|[2-3]\d|4[0-5]) seconds\./
	);
	expect(checkoutRequests, 'checkout mock must intercept the request').toBeGreaterThan(0);
	await dialog.getByRole('button', { name: /not now/i }).click();
	await expect(dialog).toBeHidden();
	await assertPricingActionsResolve(page);

	const events = await page.evaluate(
		() => (window as unknown as { __paywallEvents: Array<Record<string, unknown>> }).__paywallEvents
	);
	expect(events).toHaveLength(1);
	expect(events[0]).toMatchObject({
		kind: 'retryable',
		status: 429,
		code: 'rate_limited',
		retryAfterSeconds: 45
	});
});
