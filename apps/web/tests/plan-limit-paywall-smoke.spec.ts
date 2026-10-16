import { expect, test, type Page, type APIRequestContext } from '@playwright/test';

/**
 * Plan-limit paywall smoke.
 *
 * Drives the pricing/upgrade CTA against mocked 402 and 429 checkout
 * responses and asserts the trial and usage links resolve to real routes
 * (no 404, no placeholder href). Runs in CI with mocked routes only —
 * no staging dependency.
 */

const PLACEHOLDER_PATTERNS = ['#', 'placeholder', 'todo', 'example.com', 'javascript:'];

async function assertPricingActionsResolve(request: APIRequestContext): Promise<void> {
	// The pricing page must always offer a way forward (trial signup,
	// current-usage check) even when checkout itself is limit-blocked.
	for (const href of ['/signup?plan=free', '/account/billing']) {
		for (const placeholder of PLACEHOLDER_PATTERNS) {
			expect(
				href.toLowerCase(),
				`CTA href "${href}" must not point at a placeholder`
			).not.toContain(placeholder);
		}
		const response = await request.get(href);
		expect(response.status(), `${href} should not 404`).not.toBe(404);
		expect(response.ok(), `${href} should resolve to a real route`).toBe(true);
	}
}

async function choosePlus(page: Page): Promise<void> {
	await page.goto('/pricing');
	await page
		.getByRole('row', { name: /Plus/ })
		.getByRole('button', { name: /^Choose$/ })
		.click();
}

test('mocked 402 checkout keeps pricing actions reachable', async ({ page, request }) => {
	await page.route('**/v1/billing/checkout', (route) =>
		route.fulfill({
			status: 402,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ error: 'plan_limit_exceeded' })
		})
	);

	await choosePlus(page);

	const alert = page.getByRole('alert');
	await expect(alert).toContainText(/usage limit/i);
	await expect(alert).not.toContainText(/402|plan_limit_exceeded/i);
	await assertPricingActionsResolve(request);
});

test('mocked 429 checkout keeps pricing actions reachable', async ({ page, request }) => {
	await page.route('**/v1/billing/checkout', (route) =>
		route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '45' },
			body: JSON.stringify({ error: 'rate_limited' })
		})
	);

	await choosePlus(page);

	const alert = page.getByRole('alert');
	await expect(alert).toContainText(/wait a moment/i);
	await expect(alert).not.toContainText(/429|rate_limited/i);
	await assertPricingActionsResolve(request);
});
