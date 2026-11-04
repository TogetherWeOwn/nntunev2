import { expect, test, type Page } from '@playwright/test';

/**
 * Protected acceptance: replacements are new displays, dismissal restores focus,
 * readiness is announced once, and Retry actually reattempts the limited action.
 * Builder changes belong in production, not in this acceptance file.
 * All network responses and credentials below are synthetic and local-only.
 */
const T0 = new Date('2026-10-19T12:00:00Z');
// Private values that travel in the mocked limit bodies below (the email
// field and the message marker); the analytics events and announcements
// must never repeat them.
const PRIVATE_MARKERS: string[] = [];

declare global {
	interface Window {
		__lifecycleShown: Array<Record<string, unknown>>;
		__lifecycleAnnouncements: string[];
	}
}

async function installClockAndTracking(page: Page): Promise<void> {
	await page.clock.install({ time: T0 });
	await page.addInitScript(() => {
		window.__lifecycleShown = [];
		window.addEventListener('usage_limit_paywall_shown', (event) => {
			window.__lifecycleShown.push((event as CustomEvent).detail);
		});
	});
}

/** Tracker body shared by both install paths below: serializable, no closures. */
function trackAnnouncements(): void {
	const w = window as unknown as { __lifecycleAnnouncements: string[] };
	w.__lifecycleAnnouncements = [];
	const selector = '[role="status"], [role="alert"], [aria-live="polite"], [aria-live="assertive"]';
	let previous = new Map<Element, string>();
	const scan = (record: boolean) => {
		const next = new Map<Element, string>();
		for (const region of document.querySelectorAll(selector)) {
			if (region.parentElement?.closest(selector)) continue;
			if (region.closest('[aria-hidden="true"], [aria-live="off"], [hidden], [inert]')) continue;
			let hidden = false;
			for (let ancestor: Element | null = region; ancestor; ancestor = ancestor.parentElement) {
				const style = getComputedStyle(ancestor);
				if (style.display === 'none' || style.visibility === 'hidden') hidden = true;
			}
			if (hidden) continue;
			const text = region.textContent?.replace(/\s+/g, ' ').trim() ?? '';
			next.set(region, text);
			if (record && text && previous.get(region) !== text) {
				w.__lifecycleAnnouncements.push(text);
			}
		}
		previous = next;
	};
	scan(false);
	// Observe the document node itself: init scripts run before
	// documentElement exists, so observing it would throw and detach.
	new MutationObserver(() => scan(true)).observe(document, {
		subtree: true,
		childList: true,
		characterData: true,
		attributes: true,
		attributeFilter: ['role', 'aria-live', 'aria-hidden', 'hidden', 'inert', 'class', 'style']
	});
}

/** Observe the accessibility delivery surface, not a private component variable.
 * Compare outermost live regions so nested nodes in one announcement count once.
 * Seed existing content: only subsequent insertions/updates are new deliveries.
 * Same-document only: a navigation replaces the observed document, so the
 * loader-triggered helper below installs the same tracker via addInitScript.
 */
async function observeAnnouncements(page: Page): Promise<void> {
	await page.evaluate(trackAnnouncements);
}

async function announcements(page: Page): Promise<string[]> {
	return page.evaluate(() => window.__lifecycleAnnouncements);
}

async function shown(page: Page): Promise<Array<Record<string, unknown>>> {
	return page.evaluate(() => window.__lifecycleShown);
}

function expectNoPrivateData(value: unknown): void {
	const serialized = JSON.stringify(value).toLowerCase();
	for (const marker of [...PRIVATE_MARKERS, 'private-body-marker', 'fixture@example.test']) {
		expect(serialized).not.toContain(marker);
	}
	for (const key of ['"email"', '"user_id"', '"userid"', '"token"', '"password"']) {
		expect(serialized).not.toContain(key);
	}
}

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

function limitBody(status: number): Record<string, unknown> {
	return {
		code: status === 429 ? 'rate_limited' : 'plan_limit_exceeded',
		message: 'private-body-marker fixture@example.test',
		email: 'fixture@example.test',
		used_bytes: 2048,
		limit_bytes: 2048,
		plan: 'free'
	};
}

function limitHeaders(retryAfter: string | null): Record<string, string> {
	return {
		'content-type': 'application/json',
		...(retryAfter === null ? {} : { 'Retry-After': retryAfter })
	};
}

/**
 * Billing-loader trigger: the first subscription read is the limit, later
 * reads succeed. The paywall opens on page load; the page-level Retry
 * re-runs the loader for the explicit reattempt.
 */
async function billingLimit(page: Page, retryAfter: string | null, status = 429) {
	let subscriptionCalls = 0;
	await page.route('**/v1/me/subscription', async (route) => {
		subscriptionCalls += 1;
		await route.fulfill({
			status: subscriptionCalls === 1 ? status : 200,
			headers: limitHeaders(retryAfter),
			body: JSON.stringify(subscriptionCalls === 1 ? limitBody(status) : SUBSCRIPTION_OK)
		});
	});
	await page.route('**/v1/me/usage', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(USAGE_OK) })
	);
	// Pause before navigation: the loader fires immediately on goto, and
	// the modal's deadline is wall-clock from the limit response — pausing
	// after it opened would expire the wait at once. The tracker installs
	// via init script so it observes the post-navigation document.
	await page.addInitScript(trackAnnouncements);
	await page.clock.pauseAt(new Date(T0.getTime() + 300_000));
	await page.goto('/account/billing');
	await expect(page.getByRole('dialog')).toBeVisible();
	return { requests: () => subscriptionCalls };
}

/**
 * Billing-portal trigger: the Manage billing action is the limit. The
 * opener is the clicked control, so dismissal restores focus to it.
 */
async function portalLimit(page: Page, retryAfter: string | null, status = 429) {
	let portalCalls = 0;
	await page.route('**/v1/me/subscription', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SUBSCRIPTION_OK) })
	);
	await page.route('**/v1/me/usage', (route) =>
		route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(USAGE_OK) })
	);
	await page.route('**/v1/billing/portal', async (route) => {
		portalCalls += 1;
		await route.fulfill({
			status,
			headers: limitHeaders(retryAfter),
			body: JSON.stringify(limitBody(status))
		});
	});
	// Pause before navigation so the portal response — fired by the opener
	// click below — lands at a known frozen instant.
	await page.clock.pauseAt(new Date(T0.getTime() + 300_000));
	await page.goto('/account/billing');
	await observeAnnouncements(page);
	const opener = page.getByRole('button', { name: 'Manage billing', exact: true });
	await opener.click();
	await expect(page.getByRole('dialog')).toBeVisible();
	return { requests: () => portalCalls, opener };
}

/** Two real API consumers race; their responses publish through onPaywall.
 * Never jump the clock while the held request is in flight (request deadline).
 */
async function replaceOpenPaywall(page: Page): Promise<void> {
	let subscriptionCalls = 0;
	await page.route('**/v1/me/subscription', (route) => {
		subscriptionCalls += 1;
		return route.fulfill({
			status: subscriptionCalls === 1 ? 500 : 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '45' },
			body: JSON.stringify({ code: subscriptionCalls === 1 ? 'internal' : 'rate_limited' })
		});
	});
	await page.route('**/v1/me/usage', (route) => route.fulfill({
		status: 500, contentType: 'application/json', body: JSON.stringify({ code: 'internal' })
	}));
	let release!: () => void;
	const gate = new Promise<void>((resolve) => { release = resolve; });
	let markSeen!: () => void;
	const seen = new Promise<void>((resolve) => { markSeen = resolve; });
	await page.route('**/v1/billing/portal', async (route) => {
		markSeen();
		await gate;
		await route.fulfill({
			status: 429,
			headers: { 'content-type': 'application/json', 'Retry-After': '30' },
			body: JSON.stringify({
				code: 'rate_limited', message: 'private-body-marker fixture@example.test',
				email: 'fixture@example.test'
			})
		});
	});
	await page.goto('/account/billing');
	await expect(page.getByRole('button', { name: 'Retry', exact: true })).toBeVisible();
	await page.clock.pauseAt(new Date(T0.getTime() + 300_000));
	try {
		await page.getByRole('button', { name: 'Manage billing', exact: true }).click();
		await seen;
		const response = page.waitForResponse('**/v1/me/subscription');
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		expect((await response).status()).toBe(429);
		await expect(page.getByRole('dialog')).toBeVisible();
		await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 45 seconds.');
		await expect.poll(() => shown(page)).toHaveLength(1);
		await observeAnnouncements(page);
	} finally {
		release();
	}
	await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 30 seconds.');
}

test.beforeEach(async ({ page }) => {
	await installClockAndTracking(page);
});

// Catches mount-only analytics guards, stale models, and timer-driven duplicates.
test('replacement tracks the new model exactly once without private response data', async ({ page }) => {
	await replaceOpenPaywall(page);
	await expect.poll(() => shown(page)).toHaveLength(2);
	expect(await shown(page)).toEqual([
		{ kind: 'retryable', status: 429, code: 'rate_limited', retryAfterSeconds: 45 },
		{ kind: 'retryable', status: 429, code: 'rate_limited', retryAfterSeconds: 30 }
	]);
	await page.clock.runFor(3_000);
	await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 27 seconds.');
	expect(await shown(page)).toHaveLength(2);
	expectNoPrivateData(await shown(page));
});

// Catches silent replacement and putting a ticking timer inside a live region.
test('replacement is announced once, not once per countdown tick', async ({ page }) => {
	await replaceOpenPaywall(page);
	await expect.poll(() => announcements(page)).toHaveLength(1);
	expect((await announcements(page))[0]).toMatch(/limit|slow down|retry|try again/i);
	expectNoPrivateData(await announcements(page));
	await page.clock.runFor(3_000);
	await expect(page.getByTestId('paywall-countdown')).toHaveText('Try again in 27 seconds.');
	expect(await announcements(page)).toHaveLength(1);
	await page.clock.runFor(27_000);
	await expect(page.getByTestId('paywall-countdown')).toHaveText('You can try again now.');
	await expect.poll(() => announcements(page)).toHaveLength(2);
	expect((await announcements(page))[1]).toMatch(/try again now|ready.*retry|ready.*try again|can retry/i);
	await page.clock.runFor(5_000);
	expect(await announcements(page)).toHaveLength(2);
	expectNoPrivateData(await announcements(page));
});

for (const dismissal of ['Escape', 'backdrop', 'Not now'] as const) {
	// Catches lost pre-open focus separately for every exit path.
	test(`${dismissal} dismissal restores focus to the pre-open control`, async ({ page }) => {
		const { opener } = await portalLimit(page, '30');
		const dialog = page.getByRole('dialog');
		if (dismissal === 'Escape') await page.keyboard.press('Escape');
		else if (dismissal === 'Not now') {
			await dialog.getByRole('button', { name: 'Not now', exact: true }).click();
		} else {
			const bounds = await dialog.boundingBox();
			expect(bounds).not.toBeNull();
			expect(bounds!.x > 4 || bounds!.y > 4).toBe(true);
			await page.mouse.click(4, 4);
		}
		await expect(dialog).toBeHidden();
		await expect(opener).toBeFocused();
		const delivered = await announcements(page);
		await page.clock.fastForward(60_000);
		expect(await announcements(page)).toEqual(delivered);
	});
}

test('Upgrade closes and restores focus when its pricing destination is already current', async ({ page }) => {
	await page.route('**/v1/billing/checkout', (route) => route.fulfill({
		status: 402, contentType: 'application/json', body: JSON.stringify({ code: 'plan_limit_exceeded' })
	}));
	await page.goto('/pricing');
	const opener = page.getByRole('button', { name: 'Choose Plus', exact: true });
	await opener.click();
	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	// Navigation uses animation frames; do not freeze the router's scheduler.
	await page.clock.resume();
	await dialog.getByRole('link', { name: /upgrade/i }).click();
	await expect(page).toHaveURL(/\/pricing\/?$/);
	await expect(dialog).toBeHidden();
	await expect(opener).toBeFocused();
});

async function expectMeaningfulPageFocus(page: Page): Promise<void> {
	await expect.poll(() => page.evaluate(() => {
		const active = document.activeElement;
		return Boolean(active instanceof HTMLElement && active.isConnected
			&& active !== document.body && active !== document.documentElement
			&& !active.closest('dialog, [role="dialog"]')
			&& active.matches('a[href], button, input, select, textarea, [tabindex]'));
	})).toBe(true);
}

test('a removed opener falls back to a connected meaningful page element, not body', async ({ page }) => {
	const { opener } = await portalLimit(page, '30');
	await opener.evaluate((element) => element.remove());
	await page.getByRole('dialog').getByRole('button', { name: 'Not now', exact: true }).click();
	await expect(page.getByRole('dialog')).toBeHidden();
	await expectMeaningfulPageFocus(page);
});

test('Upgrade navigation uses a meaningful destination fallback after the opener unmounts', async ({ page }) => {
	await portalLimit(page, null, 402);
	const dialog = page.getByRole('dialog');
	// Navigation uses animation frames; do not freeze the router's scheduler.
	await page.clock.resume();
	await dialog.getByRole('link', { name: /upgrade/i }).click();
	await expect(page).toHaveURL(/\/pricing\/?$/);
	await expect(dialog).toBeHidden();
	await expectMeaningfulPageFocus(page);
});

// Catches a silent zero transition, repeated readiness, auto-retry, and PII leaks.
test('readiness is announced exactly once at zero, with no per-second announcements', async ({ page }) => {
	const billing = await billingLimit(page, '3');
	const countdown = page.getByTestId('paywall-countdown');
	await expect(countdown).toHaveText('Try again in 3 seconds.');
	await expect(countdown).toHaveAttribute('role', 'timer');
	const initial = await announcements(page);
	await page.clock.runFor(2_000);
	await expect(countdown).toHaveText(/Try again in 1 seconds?\./);
	expect(await announcements(page)).toEqual(initial);
	await page.clock.runFor(1_000);
	await expect(countdown).toHaveText('You can try again now.');
	await expect.poll(() => announcements(page)).toHaveLength(initial.length + 1);
	const ready = (await announcements(page)).at(-1)!;
	expect(ready).toMatch(/try again now|ready.*retry|ready.*try again|can retry/i);
	expectNoPrivateData(await announcements(page));
	await page.clock.runFor(5_000);
	expect(await announcements(page)).toHaveLength(initial.length + 1);
	expect(billing.requests()).toBe(1);
	expect(await shown(page)).toHaveLength(1);
});

test('an initially zero wait announces readiness once without waiting for a timer tick', async ({ page }) => {
	const billing = await billingLimit(page, '0');
	await expect(page.getByTestId('paywall-countdown')).toHaveText('You can try again now.');
	await expect.poll(async () => (await announcements(page)).filter((text) =>
		/try again now|ready.*retry|ready.*try again|can retry/i.test(text)
	)).toHaveLength(1);
	const delivered = await announcements(page);
	await page.clock.runFor(5_000);
	expect(await announcements(page)).toEqual(delivered);
	expectNoPrivateData(delivered);
	expect(billing.requests()).toBe(1);
});

for (const retryAfter of ['3', '0', null] as const) {
	// Catches absent/cosmetic Retry, modal Retry resubmitting on its own, and
	// retrying anything other than the failed action. The modal Retry only
	// dismisses (no retry action is attached to billing limits); the
	// page-level Retry reruns the loader and reattempts exactly once.
	test(`explicit Retry reattempts the limited action once (Retry-After ${retryAfter ?? 'unknown'})`, async ({ page }) => {
		const billing = await billingLimit(page, retryAfter);
		const dialog = page.getByRole('dialog');
		if (retryAfter === '3') {
			await page.clock.runFor(3_000);
			await expect(page.getByTestId('paywall-countdown')).toHaveText('You can try again now.');
		}
		expect(billing.requests()).toBe(1); // A timer alone never resubmits.
		const retry = dialog.getByRole('button', { name: /^(?:retry|try again)(?: now)?$/i });
		await expect(retry).toBeVisible();
		await expect(retry).toBeEnabled();
		await retry.click();
		await expect(dialog).toBeHidden();
		expect(billing.requests()).toBe(1); // Modal Retry dismisses without replay.
		await page.getByRole('button', { name: 'Retry', exact: true }).click();
		await expect(page.getByRole('button', { name: 'Retry', exact: true })).toHaveCount(0);
		await expect(dialog).toHaveCount(0);
		expect(billing.requests()).toBe(2);
		await page.clock.fastForward(60_000);
		expect(billing.requests()).toBe(2);
	});
}

test('terminal limits retain upgrade/dismiss and never acquire countdown or Retry', async ({ page }) => {
	const billing = await billingLimit(page, null, 402);
	const dialog = page.getByRole('dialog');
	await expect(dialog).toContainText(/reached your plan limit/i);
	await expect(dialog).toContainText(/2\D*048 of 2\D*048/);
	await expect(dialog.getByRole('link', { name: /upgrade/i })).toHaveAttribute('href', '/pricing');
	await expect(dialog.getByRole('button', { name: 'Not now', exact: true })).toBeVisible();
	await expect(dialog.getByRole('button', { name: /^(?:retry|try again)(?: now)?$/i })).toHaveCount(0);
	await expect(page.getByTestId('paywall-countdown')).toHaveCount(0);
	const delivered = await announcements(page);
	await page.clock.fastForward(60_000);
	expect(await announcements(page)).toEqual(delivered);
	expect(billing.requests()).toBe(1);
	expectNoPrivateData(await shown(page));
});
