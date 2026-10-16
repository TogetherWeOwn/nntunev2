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

async function assertPricingActionsResolve(page: Page): Promise<void> {
	// The pricing page must always offer a way forward (trial signup,
	// current-usage check) even when checkout itself is limit-blocked.
	for (const name of ['Start trial', 'Check your current usage']) {
		const link = page.getByRole('link', { name, exact: true });
		await expect(link, `pricing page must expose exactly one "${name}" link`).toHaveCount(1);
		await expect(link).toBeVisible();
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
	await page.route('**/v1/billing/checkout', (route) =>
		route.fulfill({
			status: 402,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ error: 'plan_limit_exceeded' })
		})
	);

	await choosePlus(page);

	await expect(page.getByRole('alert')).toBeVisible();
	await assertPricingActionsResolve(page);
});

test('mocked 429 checkout keeps pricing actions reachable', async ({ page }) => {
	await page.route('**/v1/billing/checkout', (route) =>
		route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '45' },
			body: JSON.stringify({ error: 'rate_limited' })
		})
	);

	await choosePlus(page);

	await expect(page.getByRole('alert')).toBeVisible();
	await assertPricingActionsResolve(page);
});
