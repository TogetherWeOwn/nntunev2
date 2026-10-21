import { expect, test } from '@playwright/test';

const PERIOD_END = '2026-11-15T12:00:00Z';
const TRIAL_END = '2099-11-01T12:00:00Z';

for (const viewport of [{ width: 1280, height: 800 }, { width: 390, height: 844 }]) {
	test.describe(`${viewport.width}px billing period-end copy`, () => {
		test.use({ viewport, locale: 'en-US', timezoneId: 'UTC' });

		for (const status of ['active', 'trialing', 'canceled', 'paused', 'unpaid', 'incomplete_expired']) {
			for (const currentPeriodEnd of [PERIOD_END, null]) {
				test(`${status} with ${currentPeriodEnd ? 'dated' : 'null'} period end`, async ({ page }, testInfo) => {
					const calls: Array<{ path: string; method: string }> = [];
					const confirmed = status === 'active' || status === 'trialing';
					await page.route('**/v1/**', (route) => {
						const path = new URL(route.request().url()).pathname;
						calls.push({ path, method: route.request().method() });
						if (path === '/v1/me/subscription') {
							return route.fulfill({ json: {
								planId: 'pro-5x', status, currentPeriodEnd,
								trialEndsAt: status === 'trialing' ? TRIAL_END : null
							} });
						}
						if (path === '/v1/me/usage') {
							return route.fulfill({ json: {
								period: '2026-10', streamSecondsMonth: 50 * 3600,
								concurrentNow: 0, libraryTracks: 40000, ingestToday: 0
							} });
						}
						// Keep every API request local, including an unexpected checkout.
						return route.fulfill({ status: 503, json: { message: 'Fixture portal unavailable' } });
					});

					await page.goto('/account/billing');
					const plan = page.getByRole('region', { name: 'Current plan: Pro 5x', exact: true });
					await expect(plan).toContainText(`Status: ${status}`);
					const screenshot = testInfo.outputPath('billing-page.png');
					await page.screenshot({ path: screenshot, fullPage: true });
					await testInfo.attach('billing-page', { path: screenshot, contentType: 'image/png' });
					await expect(plan).not.toContainText(/renews/i);
					if (currentPeriodEnd) {
						await expect(plan).toContainText('period ends 11/15/2026');
					} else {
						await expect(plan).not.toContainText('period ends');
					}
					if (status === 'trialing') {
						await expect(plan).toContainText('trial ends 11/1/2099');
					} else {
						await expect(plan).not.toContainText('trial ends');
					}
					await expect(page.getByRole('heading', { name: 'Usage for 2026-10' })).toHaveCount(confirmed ? 1 : 0);
					await expect(page.getByRole('button', { name: 'Upgrade to pro-20x' })).toHaveCount(confirmed ? 1 : 0);
					const manage = page.getByRole('button', {
						name: confirmed ? 'Manage billing' : 'Manage billing to resolve', exact: true
					});
					await expect(manage).toBeEnabled();
					await expect(page.getByRole('button', { name: 'Retry', exact: true })).toHaveCount(0);
					expect(calls).toEqual([
						{ path: '/v1/me/subscription', method: 'GET' },
						{ path: '/v1/me/usage', method: 'GET' }
					]);

					await manage.click();
					await expect(page.getByRole('alert').filter({ hasText: 'Fixture portal unavailable' })).toBeVisible();
					await expect(manage).toBeEnabled();
					expect(calls).toEqual([
						{ path: '/v1/me/subscription', method: 'GET' },
						{ path: '/v1/me/usage', method: 'GET' },
						{ path: '/v1/billing/portal', method: 'POST' }
					]);
				});
			}
		}
	});
}
