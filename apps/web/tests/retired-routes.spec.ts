import { expect, test } from '@playwright/test';

/**
 * Retired user-provider setup: the onboarding wizard, its legacy redirect
 * and the account provider page are gone. Each path must resolve to the
 * app not-found page and never render a credential form.
 */
const RETIRED_ROUTES = ['/onboarding/byop', '/onboarding/provider', '/account/provider'] as const;

for (const href of RETIRED_ROUTES) {
	test(`${href} resolves to not-found and never a credential form`, async ({ page }) => {
		const response = await page.goto(href);
		expect(response?.status(), `${href} should 404`).toBe(404);
		expect(new URL(page.url()).pathname, `${href} should not redirect`).toBe(href);

		await expect(page.getByRole('heading', { level: 1 })).toContainText('404');
		await expect(page.locator('input[type="password"]')).toHaveCount(0);
		await expect(page.locator('input[name="host"], input[name="username"]')).toHaveCount(0);
		await expect(page.getByLabel(/hostname|server host/i)).toHaveCount(0);
		await expect(page.getByLabel(/password/i)).toHaveCount(0);
	});
}
