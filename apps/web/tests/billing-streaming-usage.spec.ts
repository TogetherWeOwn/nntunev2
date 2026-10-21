import { expect, test, type Page } from '@playwright/test';

const NOW = '2026-10-20T12:00:00Z';
const TRIAL_END = '2026-10-21T12:00:00Z';

test.beforeEach(async ({ page }) => {
	await page.clock.setFixedTime(new Date(NOW));
});

async function mockBilling(
	page: Page,
	{
		streamSecondsMonth = 0,
		planId = 'plus',
		status = 'active',
		trialEndsAt = status === 'trialing' ? TRIAL_END : null,
		subscriptionStatus = 200,
		usageStatus = 200
	} = {}
): Promise<void> {
	// No unmocked billing/provider requests may leave the browser.
	await page.route('**/v1/**', (route) => route.abort());
	await page.route('**/v1/me/subscription', (route) =>
		route.fulfill({
			status: subscriptionStatus,
			contentType: 'application/json',
			body: JSON.stringify({ planId, status, currentPeriodEnd: null, trialEndsAt })
		})
	);
	await page.route('**/v1/me/usage', (route) =>
		route.fulfill({
			status: usageStatus,
			contentType: 'application/json',
			body: JSON.stringify({
				period: '2026-10',
				streamSecondsMonth,
				concurrentNow: 0,
				libraryTracks: 0,
				ingestToday: 0
			})
		})
	);
}

for (const { label, usedHours, capHours, planId, status = 'active' } of [
	{ label: 'zero', usedHours: 0, capHours: 100, planId: 'plus' },
	{ label: 'partial', usedHours: 25, capHours: 100, planId: 'plus' },
	{ label: 'at cap', usedHours: 100, capHours: 100, planId: 'plus' },
	{ label: 'over cap', usedHours: 125, capHours: 100, planId: 'plus' },
	{ label: 'confirmed free trial', usedHours: 2.5, capHours: 10, planId: 'free', status: 'trialing' }
]) {
	test(`monthly streaming meter exposes its name and hours for ${label} usage`, async ({ page }) => {
		await mockBilling(page, { streamSecondsMonth: usedHours * 3600, planId, status });
		await page.goto('/account/billing');

		const meter = page.getByRole('progressbar', { name: 'Monthly streaming usage', exact: true });
		await expect(page.getByRole('progressbar')).toHaveCount(1);
		await expect(meter).toBeVisible();
		await expect(meter).toHaveAttribute(
			'aria-valuetext',
			`${usedHours.toFixed(1)} of ${capHours} hours used this month`
		);
		await expect(meter).toHaveJSProperty('tagName', 'PROGRESS');
		await expect(meter).toHaveJSProperty('max', capHours * 3600);
		await expect(meter).toHaveJSProperty('value', Math.min(usedHours, capHours) * 3600);
		await expect(meter).toHaveJSProperty('position', Math.min(usedHours / capHours, 1));
		await expect(page.getByRole('listitem').filter({ has: meter })).toContainText(
			`Streaming: ${usedHours.toFixed(1)} / ${capHours} hrs`
		);
	});
}

for (const { label, options, notice } of [
	{
		label: 'signed out',
		options: { subscriptionStatus: 401, usageStatus: 401 },
		notice: 'Sign in to see your billing.'
	},
	{
		label: 'unconfirmed subscription',
		options: { subscriptionStatus: 503 },
		notice: "We couldn't confirm your plan"
	},
	{
		label: 'unknown plan',
		options: { planId: 'unknown-plan' },
		notice: "We couldn't confirm your plan"
	},
	{
		label: 'paused subscription',
		options: { status: 'paused' },
		notice: 'Subscription is paused'
	},
	{
		label: 'active free plan without a confirmed trial',
		options: { planId: 'free' },
		notice: 'Your trial has ended or could not be confirmed'
	},
	{
		label: 'trial at its deadline',
		options: { planId: 'free', status: 'trialing', trialEndsAt: NOW },
		notice: 'Your trial has ended or could not be confirmed'
	},
	{
		label: 'trial without a deadline',
		options: { planId: 'free', status: 'trialing', trialEndsAt: null },
		notice: 'Your trial has ended or could not be confirmed'
	},
	{
		label: 'trial with an invalid deadline',
		options: { planId: 'free', status: 'trialing', trialEndsAt: 'not-a-date' },
		notice: 'Could not load your subscription'
	},
	{
		label: 'no usage record',
		options: { usageStatus: 404 },
		notice: 'Current plan: Plus'
	}
]) {
	test(`monthly streaming meter stays suppressed for ${label}`, async ({ page }) => {
		await mockBilling(page, options);
		await page.goto('/account/billing');

		await expect(page.getByRole('heading', { name: 'Billing', exact: true })).toBeVisible();
		await expect(page.locator('main')).toContainText(notice);
		await expect(page.getByRole('progressbar')).toHaveCount(0);
		await expect(page.getByRole('heading', { name: /Usage for/ })).toHaveCount(0);
	});
}
