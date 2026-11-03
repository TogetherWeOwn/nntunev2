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
		usageStatus = 200,
		plans = null as Array<Record<string, unknown>> | null
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
	// Registered last so it wins over the abort-all route above. Without
	// `plans` the hydration read aborts and the static mirror stands.
	if (plans) {
		await page.route('**/v1/plans', (route) =>
			route.fulfill({
				contentType: 'application/json',
				body: JSON.stringify({ plans })
			})
		);
	}
}

function planRows(plusHours: number): Array<Record<string, unknown>> {
	return [
		{ id: 'free', priceCents: 0, streamHoursMonth: 0, maxConcurrentStreams: 1, selectable: false },
		{ id: 'plus', priceCents: 800, streamHoursMonth: plusHours, maxConcurrentStreams: 1, selectable: true },
		{ id: 'pro', priceCents: 1500, streamHoursMonth: 300, maxConcurrentStreams: 1, selectable: true },
		{ id: 'max', priceCents: 2500, streamHoursMonth: 750, maxConcurrentStreams: 1, selectable: true }
	];
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

// Regression: the meter and the 80%/95% warnings must score against the
// cap hydrated from GET /v1/plans, not the static mirror (Plus = 100 h).
// 90 h used crosses 80% of the mirror but only 45% of a 200 h live cap,
// so deleting the hydration effect turns every assertion here red: the
// meter would read 90/100 and an 80% warning would show and persist.
test('meter and warnings score against the /v1/plans cap, not the mirror', async ({ page }) => {
	await mockBilling(page, { streamSecondsMonth: 90 * 3600, plans: planRows(200) });
	await page.goto('/account/billing');

	const meter = page.getByRole('progressbar', { name: 'Monthly streaming usage', exact: true });
	await expect(meter).toHaveAttribute('aria-valuetext', '90.0 of 200 hours used this month');
	await expect(meter).toHaveJSProperty('max', 200 * 3600);
	await expect(page.getByRole('listitem').filter({ has: meter })).toContainText(
		'Streaming: 90.0 / 200 hrs (45%)'
	);
	await expect(page.locator('main')).not.toContainText("You've crossed 80%");
	// The warning check runs only after hydration settles; the meter above
	// already proves it has, so no shown-flag may exist for this period.
	const shown = await page.evaluate(() => localStorage.getItem('nntune:usage-warning:2026-10:80'));
	expect(shown).toBeNull();
});

// Regression: a Retry reload (`invalidateAll()`) with an unchanged planId
// must refetch /v1/plans, and the warning check must stay parked until
// that refetch settles. First load fails usage (Retry shows) while the
// live cap is 200 h. The cap then changes server-side to 400 h and the
// reloaded usage is 170 h — 85% of the stale 200 h cap but only 42.5% of
// the live one. The second /v1/plans response is held: scoring the fresh
// usage against the stale hydration in that window would show an 80%
// warning and permanently mark it shown in localStorage.
test('Retry reload refetches /v1/plans and never scores warnings against the stale cap', async ({ page }) => {
	let usageOk = false;
	let plansCalls = 0;
	let releasePlans: () => void = () => {};
	const plansHeld = new Promise<void>((resolve) => { releasePlans = resolve; });
	await page.route('**/v1/**', (route) => route.abort());
	await page.route('**/v1/me/subscription', (route) =>
		route.fulfill({
			contentType: 'application/json',
			body: JSON.stringify({
				state: 'active', planId: 'plus', status: 'active',
				currentPeriodEnd: null, graceUntil: null, cancelAtPeriodEnd: false
			})
		})
	);
	await page.route('**/v1/me/usage', (route) =>
		route.fulfill({
			status: usageOk ? 200 : 503,
			contentType: 'application/json',
			body: JSON.stringify(
				usageOk
					? { period: '2026-10', streamSecondsMonth: 170 * 3600, resetsAt: '2026-11-01T00:00:00Z' }
					: { message: 'usage unavailable' }
			)
		})
	);
	await page.route('**/v1/plans', async (route) => {
		plansCalls += 1;
		if (plansCalls > 1) await plansHeld;
		await route.fulfill({
			contentType: 'application/json',
			body: JSON.stringify({ plans: planRows(plansCalls > 1 ? 400 : 200) })
		});
	});

	await page.goto('/account/billing');
	const retry = page.getByRole('button', { name: 'Retry', exact: true });
	await expect(retry).toBeVisible();
	await expect(page.getByRole('progressbar')).toHaveCount(0);
	// First hydration (200 h) must have landed before the reload starts.
	await expect.poll(() => plansCalls).toBe(1);

	usageOk = true;
	await retry.click();

	// Fresh usage renders against the stale cap while the refetch is held…
	const meter = page.getByRole('progressbar', { name: 'Monthly streaming usage', exact: true });
	await expect(meter).toHaveAttribute('aria-valuetext', '170.0 of 200 hours used this month');
	// …and the refetch must actually be in flight (a second /v1/plans call).
	await expect.poll(() => plansCalls).toBe(2);
	// No 80% warning may be shown or marked in that window.
	await page.waitForTimeout(250);
	await expect(page.locator('main')).not.toContainText("You've crossed 80%");
	expect(
		await page.evaluate(() => localStorage.getItem('nntune:usage-warning:2026-10:80'))
	).toBeNull();

	releasePlans();
	await expect(meter).toHaveAttribute('aria-valuetext', '170.0 of 400 hours used this month');
	await expect(meter).toHaveJSProperty('max', 400 * 3600);
	await expect(page.locator('main')).not.toContainText("You've crossed 80%");
	expect(
		await page.evaluate(() => localStorage.getItem('nntune:usage-warning:2026-10:80'))
	).toBeNull();
});
