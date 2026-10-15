import { expect, test, type Page, type APIRequestContext } from '@playwright/test';

/**
 * Plan-limit paywall smoke (TOG-25596, roadmap S16).
 *
 * Drives the pricing/upgrade CTA against mocked 402 and 429 checkout
 * responses and asserts every upgrade CTA target resolves to a real route
 * (no 404, no placeholder href). Runs in CI with mocked routes only —
 * no staging dependency.
 *
 * Complements TOG-25376 (handler) and its unit pair: the handler owns the
 * paywall modal for `/v1/vault/test` limits; this spec pins the browser
 * contract that a limit failure never strands the visitor on a dead CTA.
 */

const PLACEHOLDER_PATTERNS = ['#', 'placeholder', 'todo', 'example.com', 'javascript:'];

async function upgradeCtaHrefs(page: Page): Promise<string[]> {
	const hrefs = await page.locator('main a[href]').evaluateAll((anchors) =>
		anchors.map((a) => (a as HTMLAnchorElement).getAttribute('href') ?? '')
	);
	return hrefs.filter((href) => href.startsWith('/'));
}

async function assertUpgradeCtasResolve(page: Page, request: APIRequestContext): Promise<void> {
	const hrefs = await upgradeCtaHrefs(page);
	// The pricing page must always offer a way forward (trial signup,
	// current-usage check) even when checkout itself is limit-blocked.
	expect(hrefs.length, 'pricing page should expose at least one upgrade CTA').toBeGreaterThan(0);

	for (const href of hrefs) {
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
	// Plus is the first paid tier (free renders a signup link, not a button).
	await page.getByRole('button', { name: /^choose$/ }).first().click();
}

test('mocked 402 checkout keeps upgrade CTAs on real routes', async ({ page, request }) => {
	await page.route('**/v1/billing/checkout', (route) =>
		route.fulfill({
			status: 402,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ error: 'plan_limit_exceeded' })
		})
	);

	await choosePlus(page);

	// Limit failure surfaces inline (not a silent dead end) …
	await expect(page.getByRole('alert')).toContainText(/402/);
	// … and every upgrade CTA still lands on a real route.
	await assertUpgradeCtasResolve(page, request);
});

test('mocked 429 checkout keeps upgrade CTAs on real routes', async ({ page, request }) => {
	await page.route('**/v1/billing/checkout', (route) =>
		route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '45' },
			body: JSON.stringify({ error: 'rate_limited' })
		})
	);

	await choosePlus(page);

	await expect(page.getByRole('alert')).toContainText(/429/);
	await assertUpgradeCtasResolve(page, request);
});
