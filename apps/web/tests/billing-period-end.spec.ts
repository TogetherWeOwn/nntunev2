import { expect, test } from '@playwright/test';

const PERIOD_END = '2026-11-15T12:00:00Z';

test('billing hides an earlier account statement and reloads under the replacement session', async ({ page }) => {
	let planId = 'pro';
	await page.route('**/v1/me/subscription', (route) => route.fulfill({ json: {
		state: 'active', planId, status: 'active', currentPeriodEnd: PERIOD_END,
		graceUntil: null, cancelAtPeriodEnd: false
	} }));
	await page.route('**/v1/me/usage', (route) => route.fulfill({ json: {
		period: '2026-10', streamSecondsMonth: 3600, resetsAt: '2026-11-01T00:00:00Z'
	} }));
	await page.route('**/v1/plans', (route) => route.fulfill({ json: { plans: [] } }));
	await page.goto('/account/billing');

	const earlier = page.getByRole('heading', { name: 'Current plan: Pro' });
	await expect(earlier).toBeVisible();
	planId = 'plus';
	await page.evaluate(() => {
		const key = 'nntune.auth.generation.v1';
		localStorage.setItem(key, 'replacement-session');
		window.dispatchEvent(new StorageEvent('storage', { key }));
	});
	await expect(earlier).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Manage billing', exact: true })).toHaveCount(0);
	await page.getByRole('button', { name: 'Retry' }).click();
	await expect(page.getByRole('heading', { name: 'Current plan: Plus' })).toBeVisible();
});

for (const viewport of [{ width: 1280, height: 800 }, { width: 390, height: 844 }]) {
	test.describe(`${viewport.width}px billing period-end copy`, () => {
		test.use({ viewport, locale: 'en-US', timezoneId: 'America/Los_Angeles' });

		for (const status of ['active', 'trialing', 'canceled', 'paused', 'unpaid', 'incomplete_expired']) {
			const periodEnds = [
				{
					label: 'dated',
					currentPeriodEnd: PERIOD_END,
					displayDate: '11/15/2026',
					cancelAtPeriodEnd: status === 'active'
				},
				{ label: 'null', currentPeriodEnd: null, displayDate: '', cancelAtPeriodEnd: false }
			];
			if (status === 'active') {
				periodEnds.push(
					{
						label: 'date-only',
						currentPeriodEnd: '2026-11-15',
						// Existing display treats a date-only string as UTC midnight.
						displayDate: '11/14/2026',
						cancelAtPeriodEnd: false
					},
					{
						label: 'offset',
						currentPeriodEnd: '2026-11-15T00:30:00-02:00',
						displayDate: '11/14/2026',
						cancelAtPeriodEnd: false
					}
				);
			}
			for (const { label, currentPeriodEnd, displayDate, cancelAtPeriodEnd } of periodEnds) {
				test(`${status} with ${label} period end`, async ({ page }, testInfo) => {
					const calls: Array<{ path: string; method: string }> = [];
					// Only a stored `active` folds to the server's active state;
					// every other stored status (trialing included) is lapsed.
					const confirmed = status === 'active';
					await page.route('**/v1/**', (route) => {
						const path = new URL(route.request().url()).pathname;
						calls.push({ path, method: route.request().method() });
						if (path === '/v1/me/subscription') {
							return route.fulfill({ json: {
								state: confirmed ? 'active' : 'lapsed',
								planId: 'pro', status, currentPeriodEnd,
								graceUntil: null, cancelAtPeriodEnd
							} });
						}
						if (path === '/v1/me/usage') {
							return route.fulfill({ json: {
								period: '2026-10', streamSecondsMonth: 50 * 3600,
								resetsAt: '2026-11-01T00:00:00Z'
							} });
						}
						// Valid plan rows at the mirror caps, so the page's
						// /v1/plans hydration runs without moving the meter.
						if (path === '/v1/plans') {
							return route.fulfill({ json: { plans: [
								{ id: 'free', priceCents: 0, streamHoursMonth: 0, maxConcurrentStreams: 1, selectable: false },
								{ id: 'plus', priceCents: 800, streamHoursMonth: 100, maxConcurrentStreams: 1, selectable: true },
								{ id: 'pro', priceCents: 1500, streamHoursMonth: 300, maxConcurrentStreams: 1, selectable: true },
								{ id: 'max', priceCents: 2500, streamHoursMonth: 750, maxConcurrentStreams: 1, selectable: true }
							] } });
						}
						// Keep every API request local, including an unexpected checkout.
						return route.fulfill({ status: 503, json: { message: 'Fixture portal unavailable' } });
					});

					await page.goto('/account/billing');
					const plan = page.getByRole('region', { name: 'Current plan: Pro', exact: true });
					await expect(plan).toContainText(`Status: ${status}`);
					const screenshot = testInfo.outputPath('billing-page.png');
					await page.screenshot({ path: screenshot, fullPage: true });
					await testInfo.attach('billing-page', { path: screenshot, contentType: 'image/png' });
					await expect(plan).not.toContainText(/renews/i);
					const periodEnd = plan.locator('time');
					if (currentPeriodEnd) {
						await expect(plan).toContainText(`period ends ${displayDate}`);
						await expect(periodEnd).toHaveText(displayDate);
						await expect(periodEnd).toHaveAttribute('datetime', currentPeriodEnd);
					} else {
						await expect(plan).not.toContainText('period ends');
						await expect(periodEnd).toHaveCount(0);
					}
					if (cancelAtPeriodEnd) {
						await expect(plan).toContainText('· cancels at period end');
					} else {
						await expect(plan).not.toContainText('cancels at period end');
					}
					// No trial product state exists: trial wording never renders,
					// including for a legacy stored `trialing`.
					await expect(plan).not.toContainText('trial ends');
					await expect(page.getByRole('heading', { name: 'Usage for 2026-10' })).toHaveCount(confirmed ? 1 : 0);
					await expect(page.getByRole('button', { name: 'Upgrade to max' })).toHaveCount(confirmed ? 1 : 0);
					const manage = page.getByRole('button', {
						name: confirmed ? 'Manage billing' : 'Manage billing to resolve', exact: true
					});
					await expect(manage).toBeEnabled();
					await expect(page.getByRole('button', { name: 'Retry', exact: true })).toHaveCount(0);
					// The page hydrates caps from /v1/plans after the loader's
					// two reads; wait for that third call before the exact list.
					await expect.poll(() => calls.length).toBe(3);
					expect(calls).toEqual([
						{ path: '/v1/me/subscription', method: 'GET' },
						{ path: '/v1/me/usage', method: 'GET' },
						{ path: '/v1/plans', method: 'GET' }
					]);

					await manage.click();
					await expect(page.getByRole('alert').filter({ hasText: 'Fixture portal unavailable' })).toBeVisible();
					await expect(manage).toBeEnabled();
					expect(calls).toEqual([
						{ path: '/v1/me/subscription', method: 'GET' },
						{ path: '/v1/me/usage', method: 'GET' },
						{ path: '/v1/plans', method: 'GET' },
						{ path: '/v1/billing/portal', method: 'POST' }
					]);
				});
			}
		}
	});
}
