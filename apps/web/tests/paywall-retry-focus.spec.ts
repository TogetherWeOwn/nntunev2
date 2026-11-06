import { expect, test, type Page } from '@playwright/test';

const SUBSCRIPTION_OK = {
	state: 'active',
	planId: 'free',
	status: 'active',
	currentPeriodEnd: null,
	cancelAtPeriodEnd: false
};
const USAGE_OK = {
	period: '2026-10',
	resetsAt: '2026-11-01T00:00:00.000Z',
	streamSecondsMonth: 0,
	concurrentNow: 0,
	libraryTracks: 0,
	ingestToday: 0
};

async function mockUsageOk(page: Page): Promise<void> {
	await page.route('**/v1/me/usage', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(USAGE_OK) })
	);
}

function deferred() {
	let resolve!: () => void;
	const promise = new Promise<void>((done) => { resolve = done; });
	return { promise, resolve };
}

test('Retry keeps meaningful focus while the repeated billing load is pending', async ({ page }) => {
	let subscriptionCalls = 0;
	let release!: () => void;
	const gate = new Promise<void>((resolve) => { release = resolve; });
	await page.route('**/v1/me/subscription', async (route) => {
		subscriptionCalls += 1;
		if (subscriptionCalls === 1) {
			await route.fulfill({
				status: 429,
				headers: { 'content-type': 'application/json', 'Retry-After': '0' },
				body: JSON.stringify({ code: 'rate_limited' })
			});
		} else {
			await gate;
			await route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify(SUBSCRIPTION_OK)
			});
		}
	});
	await mockUsageOk(page);
	try {
		await page.goto('/account/billing');
		const dialog = page.getByRole('dialog');
		await expect(dialog).toBeVisible();
		// The modal Retry carries no retry action: it only dismisses.
		await dialog.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(dialog).toBeHidden();
		expect(subscriptionCalls).toBe(1);
		// The explicit page Retry reruns the loader; the held response keeps
		// the control mounted and focused while the reattempt is pending.
		const retry = page.getByRole('button', { name: 'Retry', exact: true });
		await retry.click();
		await expect.poll(() => subscriptionCalls).toBe(2);
		await expect(retry).toBeFocused();
	} finally {
		release();
	}
	await expect(page.getByRole('button', { name: 'Retry', exact: true })).toHaveCount(0);
	await expect(page.getByRole('dialog')).toHaveCount(0);
	expect(subscriptionCalls).toBe(2);
});

test('Retry without a UI action dismisses and restores meaningful focus, without replay', async ({ page }) => {
	let requests = 0;
	await page.route('**/v1/me/subscription', (route) => {
		requests += 1;
		return route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '0' },
			body: JSON.stringify({ code: 'rate_limited' })
		});
	});
	await page.route('**/v1/me/usage', (route) => route.fulfill({
		status: 500, contentType: 'application/json', body: JSON.stringify({ code: 'internal' })
	}));
	await page.goto('/account/billing');
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await dialog.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(dialog).toBeHidden();
	await expect(page.getByRole('main')).toBeFocused();
	expect(requests).toBe(1);
});

test('Retry stays disabled throughout a known wait and sends only an explicit ready retry', async ({ page }) => {
	const start = new Date('2026-10-19T12:00:00Z');
	await page.clock.install({ time: start });
	let subscriptionCalls = 0;
	await page.route('**/v1/me/subscription', (route) => {
		subscriptionCalls += 1;
		return route.fulfill(
			subscriptionCalls === 1
				? {
					status: 429,
					headers: { 'content-type': 'application/json', 'Retry-After': '30' },
					body: JSON.stringify({ code: 'rate_limited' })
				}
				: {
					status: 200,
					contentType: 'application/json',
					body: JSON.stringify(SUBSCRIPTION_OK)
				}
		);
	});
	await mockUsageOk(page);
	// Pause before navigation: the loader fires on goto, and the deadline
	// is wall-clock from the limit response.
	await page.clock.pauseAt(new Date(start.getTime() + 300_000));
	await page.goto('/account/billing');
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	const retry = dialog.getByRole('button', { name: 'Retry', exact: true });
	await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 30 seconds.');
	await expect(retry).toBeDisabled();
	// A real pointer activation of the disabled control must neither close nor submit.
	await retry.click({ force: true });
	await expect(dialog).toBeVisible();
	expect(subscriptionCalls).toBe(1);
	await page.clock.runFor(29_000);
	await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 1 second.');
	await expect(retry).toBeDisabled();
	await page.clock.runFor(1_000);
	await expect(retry).toBeEnabled();
	expect(subscriptionCalls).toBe(1);
	// The ready modal Retry only dismisses (no retry action is attached);
	// the explicit page Retry is the single resubmission.
	await retry.click();
	await expect(dialog).toBeHidden();
	expect(subscriptionCalls).toBe(1);
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(page.getByRole('button', { name: 'Retry', exact: true })).toHaveCount(0);
	expect(subscriptionCalls).toBe(2);
});

test('Retry from a billing page that unmounted never replays its portal request', async ({ page }) => {
	let portalCalls = 0;
	await page.route('**/v1/me/subscription', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SUBSCRIPTION_OK) })
	);
	await mockUsageOk(page);
	await page.route('**/v1/billing/portal', (route) => {
		portalCalls += 1;
		return route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '0' },
			body: JSON.stringify({ code: 'rate_limited' })
		});
	});
	await page.route('**/api/library', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify([]) })
	);
	// Build history with a client-side link so the layout (and its modal)
	// stays mounted; a document load would restore a layout without one.
	await page.goto('/library');
	await page.getByRole('button', { name: 'Account menu', exact: true }).click();
	await page.getByRole('menuitem', { name: 'Account', exact: true }).click();
	await expect(page).toHaveURL(/\/account\/billing\/?$/);
	const opener = page.getByRole('button', { name: 'Manage billing', exact: true });
	await opener.click();
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	// Leave through history: the layout-owned modal survives; the billing
	// component and its opener do not.
	await page.goBack();
	await expect(page).toHaveURL(/\/library\/?$/);
	await expect(opener).toHaveCount(0);
	await expect(dialog).toBeVisible();
	const replay = page
		.waitForRequest('**/v1/billing/portal', { timeout: 1_000 })
		.then(() => true, () => false);
	await dialog.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(dialog).toBeHidden();
	await expect(page.getByRole('main')).toBeFocused();
	expect(await replay).toBe(false);
	expect(portalCalls).toBe(1);
});

test('equal-text replacements each deliver a new live announcement and shown event', async ({ page }) => {
	const start = new Date('2026-10-19T12:00:00Z');
	await page.clock.install({ time: start });
	const portal = deferred();
	const portalSeen = deferred();
	const usage = deferred();
	let subscriptions = 0;
	let usages = 0;
	await page.route('**/v1/me/subscription', (route) => {
		subscriptions += 1;
		return route.fulfill({
			status: subscriptions === 1 ? 500 : 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '45' },
			body: JSON.stringify({ code: subscriptions === 1 ? 'internal' : 'rate_limited' })
		});
	});
	await page.route('**/v1/me/usage', async (route) => {
		usages += 1;
		if (usages > 1) await usage.promise;
		await route.fulfill({
			status: usages === 1 ? 500 : 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '30' },
			body: JSON.stringify({ code: usages === 1 ? 'internal' : 'rate_limited' })
		});
	});
	await page.route('**/v1/billing/portal', async (route) => {
		portalSeen.resolve();
		await portal.promise;
		await route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '30' },
			body: JSON.stringify({ code: 'rate_limited' })
		});
	});
	try {
		await page.goto('/account/billing');
		await expect(page.getByRole('button', { name: 'Retry', exact: true })).toBeVisible();
		await page.clock.pauseAt(new Date(start.getTime() + 300_000));
		// Observe DOM delivery, not component state or screen-reader speech.
		await page.evaluate(() => {
			const evidence = { shown: [] as unknown[], announcements: [] as string[] };
			Object.assign(window, { __retryEvidence: evidence });
			window.addEventListener('usage_limit_paywall_shown', (event) => {
				evidence.shown.push((event as CustomEvent).detail);
			});
			let previous = '';
			new MutationObserver(() => {
				const text = document.querySelector('dialog [role="status"]')?.textContent?.trim() ?? '';
				if (text && text !== previous) evidence.announcements.push(text);
				previous = text;
			}).observe(document.body, { subtree: true, childList: true, characterData: true });
		});
		const evidence = () => page.evaluate(() =>
			(window as unknown as { __retryEvidence: { shown: unknown[]; announcements: string[] } }).__retryEvidence);
		await page.getByRole('button', { name: 'Manage billing', exact: true }).click();
		await portalSeen.promise;
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByRole('dialog')).toBeVisible();
		await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 45 seconds.');
		await expect.poll(async () => (await evidence()).shown).toHaveLength(1);
		usage.resolve();
		await expect.poll(async () => (await evidence()).announcements).toHaveLength(1);
		await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 30 seconds.');
		portal.resolve();
		await expect.poll(async () => (await evidence()).shown).toHaveLength(3);
		await expect.poll(async () => (await evidence()).announcements).toHaveLength(2);
		const delivered = await evidence();
		expect(delivered.announcements[0]).toMatch(/slow down.*Try again in 30 seconds\./i);
		expect(delivered.announcements[1]).toBe(delivered.announcements[0]);
		expect(delivered.shown).toEqual([45, 30, 30].map((retryAfterSeconds) => ({
			kind: 'retryable', status: 429, code: 'rate_limited', retryAfterSeconds
		})));
		await page.clock.runFor(3_000);
		await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 27 seconds.');
		expect(await evidence()).toEqual(delivered);
	} finally {
		usage.resolve();
		portal.resolve();
	}
});
