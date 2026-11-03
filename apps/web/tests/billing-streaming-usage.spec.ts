import { expect, test, type Page } from '@playwright/test';

async function mockBilling(
	page: Page,
	{
		streamSecondsMonth = 0,
		planId = 'plus' as string | null,
		status = 'active' as string | null,
		state = status === 'active' ? 'active' : 'lapsed',
		currentPeriodEnd = null as string | null,
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
			body: JSON.stringify({
				state,
				planId,
				status,
				currentPeriodEnd,
				graceUntil: null,
				cancelAtPeriodEnd: false
			})
		})
	);
	await page.route('**/v1/me/usage', (route) =>
		route.fulfill({
			status: usageStatus,
			contentType: 'application/json',
			body: JSON.stringify({
				period: '2026-10',
				streamSecondsMonth,
				resetsAt: '2026-11-01T00:00:00Z'
			})
		})
	);
}

for (const { label, usedHours, capHours } of [
	{ label: 'zero', usedHours: 0, capHours: 100 },
	{ label: 'partial', usedHours: 25, capHours: 100 },
	{ label: 'at cap', usedHours: 100, capHours: 100 },
	{ label: 'over cap', usedHours: 125, capHours: 100 }
]) {
	test(`monthly streaming meter exposes its name and hours for ${label} usage`, async ({ page }) => {
		await mockBilling(page, { streamSecondsMonth: usedHours * 3600 });
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
		await expect(page.locator('main')).toContainText('Resets 1 Nov, 00:00 UTC');
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
		notice: "We couldn't confirm an active plan"
	},
	{
		label: 'unknown plan',
		options: { planId: 'unknown-plan' },
		notice: "We couldn't confirm an active plan"
	},
	{
		label: 'paused subscription',
		options: { status: 'paused' },
		notice: 'Subscription is paused'
	},
	{
		label: 'active free plan (reserved row never entitled)',
		options: { planId: 'free' },
		notice: 'Subscription is active'
	},
	{
		label: 'never subscribed',
		options: { planId: null, status: null, state: 'no_subscription' },
		notice: "You don't have a subscription yet"
	},
	{
		label: 'legacy stored trialing (folds to lapsed)',
		options: { status: 'trialing' },
		notice: 'Subscription is trialing'
	},
	{
		label: 'malformed subscription payload',
		options: { currentPeriodEnd: 'not-a-date' },
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
