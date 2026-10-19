import { expect, test, type Page } from '@playwright/test';

/**
 * Acceptance: the paywall retry countdown must count *elapsed wall-clock
 * time* from a per-model deadline, not interval callbacks.
 *
 * A suspended tab (or a throttled background timer) does not replay every
 * missed 1s callback when it wakes: the browser fires the interval roughly
 * once and `Date.now()` has already jumped. Playwright's
 * `clock.fastForward` models exactly that — "only fires due timers at most
 * once … equivalent to user closing the laptop lid for a while" — so a
 * 60s jump during a 45s wait must land on "You can try again now.", not
 * on "Try again in 44 seconds.".
 *
 * Every limit response below is a mocked 429 through the existing local
 * web harness; no real sleeps are used to measure the countdown.
 */

/** Fixed install time; the page is paused at T0 + PAUSE_AFTER before the
 * limit response, so the modal's deadline starts at a known fake instant. */
const T0 = new Date('2026-10-18T12:00:00Z');
const PAUSE_AFTER_MS = 5 * 60_000;

async function collectPaywallEvents(page: Page): Promise<void> {
	await page.addInitScript(() => {
		const bucket: Array<Record<string, unknown>> = [];
		(window as unknown as { __paywallEvents: typeof bucket }).__paywallEvents = bucket;
		window.addEventListener('usage_limit_paywall_shown', (event) => {
			bucket.push((event as CustomEvent).detail as Record<string, unknown>);
		});
	});
}

async function paywallEvents(page: Page): Promise<Array<Record<string, unknown>>> {
	return page.evaluate(
		() => (window as unknown as { __paywallEvents: Array<Record<string, unknown>> }).__paywallEvents
	);
}

async function installClock(page: Page): Promise<void> {
	await page.clock.install({ time: T0 });
}

/** Freeze fake time at a known instant (safe future relative to load). */
async function pauseClock(page: Page): Promise<void> {
	await page.clock.pauseAt(new Date(T0.getTime() + PAUSE_AFTER_MS));
}

async function toConnectionTestStep(page: Page): Promise<void> {
	await page.goto('/onboarding/provider');
	await page.locator('input[name="host"]').fill('news.example.com');
	await page.locator('input[name="username"]').fill('alice');
	await page.locator('input[name="password"]').fill('s3cret');
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByRole('button', { name: /test connection/i })).toBeVisible();
}

/**
 * Route the wizard's connection test to a mocked 429 and open the paywall
 * with the fake clock already paused, so the wait starts at a known time.
 * Returns a counter of how many times the mutation endpoint was hit.
 */
async function showWizardPaywall(
	page: Page,
	retryAfter: string | null
): Promise<{ requests: () => number }> {
	let requests = 0;
	await page.route('**/v1/vault/test', async (route) => {
		requests += 1;
		await route.fulfill({
			status: 429,
			headers: {
				'content-type': 'application/json',
				...(retryAfter === null ? {} : { 'Retry-After': retryAfter })
			},
			body: JSON.stringify({ code: 'rate_limited', message: 'limit reached' })
		});
	});
	await toConnectionTestStep(page);
	await pauseClock(page);
	await page.getByRole('button', { name: /test connection/i }).click();
	await expect(page.getByRole('dialog')).toBeVisible();
	return { requests: () => requests };
}

test('45s wait counts elapsed time: one callback after a 60s jump permits retry', async ({
	page
}) => {
	await installClock(page);
	await collectPaywallEvents(page);
	const vault = await showWizardPaywall(page, '45');

	const countdown = page.getByTestId('paywall-countdown');
	await expect(countdown).toHaveText('Try again in 45 seconds.');

	// Accessible timer semantics are preserved: role="timer", no live region
	// spamming screen readers every second.
	await expect(countdown).toHaveAttribute('role', 'timer');
	expect(await countdown.getAttribute('aria-live')).toBeNull();

	// Suspended-tab jump: 10s of wall time pass, the interval fires at most
	// once. Elapsed-time accounting shows 35s left; callback counting
	// would wrongly show 44s.
	await page.clock.fastForward(10_000);
	await expect(countdown).toHaveText('Try again in 35 seconds.');

	// Total elapsed 60s > the 45s wait, again with at most one callback:
	// the user may retry now instead of being told to keep waiting.
	await page.clock.fastForward(50_000);
	await expect(countdown).toHaveText('You can try again now.');

	// Analytics fired exactly once for this display, and reaching zero
	// never retries the mutation on its own.
	const events = await paywallEvents(page);
	expect(events).toHaveLength(1);
	expect(events[0]).toMatchObject({
		kind: 'retryable',
		status: 429,
		code: 'rate_limited',
		retryAfterSeconds: 45
	});
	expect(vault.requests()).toBe(1);
});

test('a new limit response while the modal stays open establishes a fresh deadline', async ({
	page
}) => {
	await installClock(page);
	await collectPaywallEvents(page);

	// Two user actions race on the billing page: "Manage billing" starts a
	// portal request we hold at the route, and the loader's Retry control
	// (never disabled) re-runs the loader, whose subscription fetch now
	// returns the first limit and opens the modal. Releasing the held
	// portal request then delivers a second limit to the still-open,
	// still-mounted modal — it must start a fresh 30s deadline.
	let releasePortal!: () => void;
	const portalGate = new Promise<void>((resolve) => {
		releasePortal = resolve;
	});
	let markPortalSeen!: () => void;
	const portalSeen = new Promise<void>((resolve) => {
		markPortalSeen = resolve;
	});

	let subscriptionCalls = 0;
	await page.route('**/v1/me/subscription', async (route) => {
		subscriptionCalls += 1;
		if (subscriptionCalls === 1) {
			// Initial load fails generically, so the page renders its Retry
			// control and the Manage billing button with no modal yet.
			await route.fulfill({
				status: 500,
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({ code: 'internal', message: 'unavailable' })
			});
			return;
		}
		await route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '45' },
			body: JSON.stringify({ code: 'rate_limited', message: 'limit reached' })
		});
	});
	await page.route('**/v1/me/usage', (route) =>
		route.fulfill({
			status: 500,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ code: 'internal', message: 'unavailable' })
		})
	);
	await page.route('**/v1/billing/portal', async (route) => {
		markPortalSeen();
		await portalGate;
		await route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '30' },
			body: JSON.stringify({ code: 'rate_limited', message: 'limit reached' })
		});
	});

	await page.goto('/account/billing');
	// Settle the initial load BEFORE freezing the clock. `goto` resolves at
	// the load event, which fires before SvelteKit hydration completes the
	// loader's subscription/usage fetches — and `pauseAt` jumps 5 minutes
	// forward, firing due timers. Pausing while the subscription is in flight
	// trips its 30s request deadline and aborts it, sometimes before route
	// interception, which leaves the subscriptionCalls counter off by one so
	// the retried fetch gets a 500 instead of the 429 and no modal ever
	// mounts (twice on CI; #541's waitForResponse could not catch it). The
	// Retry control renders only from loader data, which exists after both
	// fetches settled and their deadline timers cleared — so its visibility
	// proves nothing is in flight when the clock jumps.
	// Freeze after the initial load and before either billing action. The
	// portal response stays held until the modal is open; jumping the clock
	// while that request is in flight expires its deadline, and a late 429
	// must not publish a paywall. Both actions therefore run at this frozen
	// instant, inside the request deadline.
	await expect(page.getByRole('button', { name: 'Retry', exact: true })).toBeVisible();
	await pauseClock(page);
	await page.getByRole('button', { name: 'Manage billing', exact: true }).click();
	await portalSeen;

	// The Retry control re-runs the loader (subscription, then usage). Wait
	// for the loader's subscription round-trip — the response that must
	// precede the modal — instead of racing the 5s dialog assertion against
	// a cold invalidateAll round-trip on a loaded runner.
	const retriedSubscription = page.waitForResponse('**/v1/me/subscription', { timeout: 15_000 });
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	const retriedResponse = await retriedSubscription;
	// Tripwire for the root-cause invariant: the retried fetch must see the
	// 429. A 500 here means the initial load's subscription never reached
	// its route (the pauseAt-abort race the settle-wait above closes), in
	// which case no modal would ever mount and the dialog assertion below
	// would time out opaquely.
	expect(retriedResponse.status()).toBe(429);
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(page.getByTestId('paywall-countdown')).toHaveText(/Try again in \d+ seconds\./);

	// The portal request is still inside its deadline. Delivering it now
	// replaces the open modal with a fresh 30s wait, measured from this
	// response rather than the first model's deadline. The suspension jump
	// below stays after this request completes.
	releasePortal();
	const countdown = page.getByTestId('paywall-countdown');
	await expect(countdown).toHaveText('Try again in 30 seconds.');

	// One resumed callback 31s of wall time later ends the *new* wait.
	await page.clock.fastForward(31_000);
	await expect(countdown).toHaveText('You can try again now.');

	// Still the same modal mount: analytics fired once, for the first model.
	const events = await paywallEvents(page);
	expect(events).toHaveLength(1);
	expect(events[0]).toMatchObject({ kind: 'retryable', status: 429, retryAfterSeconds: 45 });
});

test('unknown wait keeps current copy, shows no countdown, and never auto-retries', async ({
	page
}) => {
	await installClock(page);
	await collectPaywallEvents(page);
	const vault = await showWizardPaywall(page, null);

	const dialog = page.getByRole('dialog');
	await expect(dialog).toContainText('Please slow down a little');
	await expect(dialog.getByRole('link', { name: /upgrade/i })).toHaveAttribute('href', '/pricing');
	await expect(page.getByTestId('paywall-countdown')).toHaveCount(0);

	// Time passing invents no countdown and retries nothing.
	await page.clock.fastForward(60_000);
	await expect(page.getByTestId('paywall-countdown')).toHaveCount(0);
	expect(vault.requests()).toBe(1);

	// Dismissal still returns to the wizard.
	await dialog.getByRole('button', { name: /not now/i }).click();
	await expect(dialog).toBeHidden();
});

test('zero wait reads try-again-now and never counts negative', async ({ page }) => {
	await installClock(page);
	await collectPaywallEvents(page);
	await showWizardPaywall(page, '0');

	const countdown = page.getByTestId('paywall-countdown');
	await expect(countdown).toHaveText('You can try again now.');

	await page.clock.fastForward(120_000);
	await expect(countdown).toHaveText('You can try again now.');
	await expect(page.getByRole('dialog')).not.toContainText(/-\d+ seconds/);
});

test('dismissed paywall stays gone: its timer is disposed with the modal', async ({ page }) => {
	const pageErrors: string[] = [];
	page.on('pageerror', (err) => pageErrors.push(String(err)));

	await installClock(page);
	await collectPaywallEvents(page);
	const vault = await showWizardPaywall(page, '45');
	await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 45 seconds.');

	await page.getByRole('dialog').getByRole('button', { name: /not now/i }).click();
	await expect(page.getByRole('dialog')).toHaveCount(0);

	// Long after the would-be deadline, a leaked timer must not resurrect
	// the modal, re-fire analytics, retry the mutation, or throw.
	await page.clock.fastForward(120_000);
	await expect(page.getByRole('dialog')).toHaveCount(0);
	await expect(page.getByTestId('paywall-countdown')).toHaveCount(0);
	expect(await paywallEvents(page)).toHaveLength(1);
	expect(vault.requests()).toBe(1);
	expect(pageErrors).toEqual([]);
});
