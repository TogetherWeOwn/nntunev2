import { expect, test, type Page } from '@playwright/test';

async function reachProviderTest(page: Page): Promise<void> {
	await page.goto('/onboarding/provider');
	await page.locator('[name=host]').fill('news.example.test');
	await page.locator('[name=username]').fill('fixture-reader');
	await page.locator('[name=password]').fill('fixture-password');
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await expect(page.getByTestId('byop-test-connection')).toBeVisible();
}

function deferred() {
	let resolve!: () => void;
	const promise = new Promise<void>((done) => { resolve = done; });
	return { promise, resolve };
}

test('Retry keeps meaningful focus while the repeated provider test is pending', async ({ page }) => {
	let requests = 0;
	let release!: () => void;
	const gate = new Promise<void>((resolve) => { release = resolve; });
	await page.route('**/v1/vault/test', async (route) => {
		requests += 1;
		if (requests > 1) await gate;
		await route.fulfill({
			status: requests === 1 ? 429 : 200,
			headers: { 'content-type': 'application/json', 'Retry-After': '0' },
			body: JSON.stringify(requests === 1 ? { code: 'rate_limited' } : { ok: true, latencyMs: 12 })
		});
	});
	try {
		await page.goto('/onboarding/provider');
		await page.locator('[name=host]').fill('news.example.test');
		await page.locator('[name=username]').fill('fixture-reader');
		await page.locator('[name=password]').fill('fixture-password');
		await page.getByRole('button', { name: 'Continue', exact: true }).click();
		await page.getByRole('button', { name: 'Continue', exact: true }).click();
		await page.getByRole('button', { name: 'Test connection', exact: true }).click();
		const dialog = page.getByRole('dialog');
		await expect(dialog).toBeVisible();
		await dialog.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(dialog).toBeHidden();
		await expect(page.getByTestId('byop-test-connection')).toBeDisabled();
		await expect(page.getByRole('main')).toBeFocused();
		expect(requests).toBe(2);
	} finally {
		release();
	}
	await expect(page.getByTestId('byop-save')).toBeVisible();
	expect(requests).toBe(2);
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
	let requests = 0;
	await page.route('**/v1/vault/test', (route) => {
		requests += 1;
		return route.fulfill({
			status: requests === 1 ? 429 : 200,
			headers: { 'content-type': 'application/json', 'Retry-After': '30' },
			body: JSON.stringify(requests === 1 ? { code: 'rate_limited' } : { ok: true, latencyMs: 12 })
		});
	});
	await reachProviderTest(page);
	await page.clock.pauseAt(new Date(start.getTime() + 300_000));
	await page.getByTestId('byop-test-connection').click();
	const dialog = page.getByRole('dialog');
	const retry = dialog.getByRole('button', { name: 'Retry', exact: true });
	await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 30 seconds.');
	await expect(retry).toBeDisabled();
	// A real pointer activation of the disabled control must neither close nor submit.
	await retry.click({ force: true });
	await expect(dialog).toBeVisible();
	expect(requests).toBe(1);
	await page.clock.runFor(29_000);
	await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 1 second.');
	await expect(retry).toBeDisabled();
	await page.clock.runFor(1_000);
	await expect(retry).toBeEnabled();
	expect(requests).toBe(1);
	await retry.click();
	await expect(dialog).toBeHidden();
	await expect(page.getByTestId('byop-save')).toBeVisible();
	expect(requests).toBe(2);
});

test('Retry from a provider page that unmounted never replays its credential-bearing request', async ({ page }) => {
	await page.addInitScript(() => {
		sessionStorage.setItem('nntune:signed-in-email', 'fixture@example.test');
	});
	let requests = 0;
	await page.route('**/v1/vault/test', (route) => {
		requests += 1;
		return route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '0' },
			body: JSON.stringify({ code: 'rate_limited' })
		});
	});
	await reachProviderTest(page);
	// Build forward history through the real client-side link, not a document load.
	await page.getByRole('link', { name: 'sign in again', exact: true }).click();
	await expect(page).toHaveURL(/\/login\/?$/);
	await page.goBack();
	await expect(page).toHaveURL(/\/onboarding\/byop\/?$/);
	await page.locator('[name=host]').fill('news.example.test');
	await page.locator('[name=username]').fill('fixture-reader');
	await page.locator('[name=password]').fill('fixture-password');
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByTestId('byop-test-connection').click();
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await page.goForward();
	await expect(page).toHaveURL(/\/login\/?$/);
	await expect(page.getByTestId('byop-test-connection')).toHaveCount(0);
	// The layout-owned modal survives; the provider component and its opener do not.
	await expect(dialog).toBeVisible();
	const replay = page.waitForRequest('**/v1/vault/test', { timeout: 1_000 })
		.then(() => true, () => false);
	await dialog.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(dialog).toBeHidden();
	await expect(page.getByRole('main')).toBeFocused();
	expect(await replay).toBe(false);
	expect(requests).toBe(1);
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
