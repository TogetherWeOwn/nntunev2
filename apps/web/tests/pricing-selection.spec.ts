import { expect, test, type Locator, type Page, type Route } from '@playwright/test';

/**
 * Pricing-page plan-selection serialization.
 *
 * Acceptance contract (all checkout traffic is intercepted and fulfilled
 * locally; no live checkout, portal or payment request ever leaves the
 * page):
 *
 * - At most one plan-selection request is in flight from /pricing. While
 *   one is pending, every paid-plan action button is disabled AND the
 *   click handler itself refuses re-entry, so a second mouse, keyboard
 *   or programmatic activation issues no further POST
 *   /v1/billing/checkout and does not replace the pending plan's
 *   identity ("Starting…" stays on the originally chosen plan).
 * - An ordinary (non-paywall) failure restores every choice and a later
 *   selection of a different plan can succeed.
 * - Existing behavior is preserved unchanged: a successful response
 *   navigates to the returned https checkout URL, a 401 hands off to
 *   /signup?plan=<planId>, and 402/429 show the paywall dialog instead
 *   of an inline alert. The endpoint and the { planId } payload do not
 *   change.
 * - Success keeps the selection locked until the page leaves: assigning
 *   `window.location.href` only starts the cross-origin navigation, so
 *   while a slow checkout host has not responded yet the buttons stay
 *   disabled and another activation issues no second request.
 */

const CHECKOUT_ROUTE = '**/v1/billing/checkout';
const MOCKED_CHECKOUT_URL = 'https://checkout.stripe.com/session/fixture';

interface CheckoutRequestRecord {
	method: string;
	pathname: string;
	body: unknown;
}

interface FulfillSpec {
	status: number;
	body: Record<string, unknown>;
	headers?: Record<string, string>;
}

const ORDINARY_FAILURE: FulfillSpec = {
	status: 500,
	body: { code: 'internal_error', message: 'checkout unavailable (fixture)' }
};

const SUCCESS: FulfillSpec = {
	status: 200,
	body: { url: MOCKED_CHECKOUT_URL }
};

function recordFrom(route: Route): CheckoutRequestRecord {
	const request = route.request();
	return {
		method: request.method(),
		pathname: new URL(request.url()).pathname,
		body: request.postDataJSON()
	};
}

async function fulfill(route: Route, spec: FulfillSpec): Promise<void> {
	await route.fulfill({
		status: spec.status,
		headers: { 'content-type': 'application/json', ...spec.headers },
		body: JSON.stringify(spec.body)
	});
}

/** Fulfill the (mocked) external checkout navigation target locally. */
async function mockCheckoutDestination(page: Page): Promise<void> {
	await page.route('https://checkout.stripe.com/**', (route) =>
		route.fulfill({
			status: 200,
			headers: { 'content-type': 'text/html' },
			body: '<h1>Mocked checkout</h1>'
		})
	);
}

/**
 * Intercept checkout and hold every response until released, so a test
 * can attempt a second selection while the first is still in flight.
 */
async function holdCheckout(page: Page) {
	const requests: CheckoutRequestRecord[] = [];
	let response: FulfillSpec = ORDINARY_FAILURE;
	let openGate!: () => void;
	const gate = new Promise<void>((resolve) => {
		openGate = resolve;
	});

	await page.route(CHECKOUT_ROUTE, async (route) => {
		requests.push(recordFrom(route));
		await gate;
		await fulfill(route, response);
	});

	return {
		requests,
		/** Set the response later (already-released) requests will receive. */
		setResponse(spec: FulfillSpec) {
			response = spec;
		},
		/** Release all held responses with the given fulfillment. */
		release(spec: FulfillSpec) {
			response = spec;
			openGate();
		},
		async waitForRequests(count: number) {
			await expect
				.poll(() => requests.length, { message: `expected ${count} checkout request(s)` })
				.toBe(count);
		}
	};
}

/**
 * Hold the (mocked) external checkout destination until released, so a
 * test can attempt another selection after checkout has succeeded but
 * before the cross-origin navigation commits.
 */
async function holdCheckoutDestination(page: Page) {
	let arrivals = 0;
	let openGate!: () => void;
	const gate = new Promise<void>((resolve) => {
		openGate = resolve;
	});

	await page.route('https://checkout.stripe.com/**', async (route) => {
		arrivals += 1;
		await gate;
		await route.fulfill({
			status: 200,
			headers: { 'content-type': 'text/html' },
			body: '<h1>Mocked checkout</h1>'
		});
	});

	return {
		async waitForArrival() {
			await expect
				.poll(() => arrivals, { message: 'expected checkout destination request' })
				.toBe(1);
		},
		release() {
			openGate();
		}
	};
}

/** Intercept checkout and answer every request immediately. */
async function stubCheckout(page: Page, spec: FulfillSpec) {
	const requests: CheckoutRequestRecord[] = [];
	await page.route(CHECKOUT_ROUTE, async (route) => {
		requests.push(recordFrom(route));
		await fulfill(route, spec);
	});
	return { requests };
}

function planButton(page: Page, rowName: RegExp): Locator {
	return page.getByRole('row', { name: rowName }).getByRole('button');
}

function paidPlanButtons(page: Page): Record<'plus' | 'pro5x' | 'pro20x', Locator> {
	return {
		plus: planButton(page, /Plus/),
		pro5x: planButton(page, /Pro 5x/),
		pro20x: planButton(page, /Pro 20x/)
	};
}

async function expectAllChoicesRestored(page: Page): Promise<void> {
	for (const button of Object.values(paidPlanButtons(page))) {
		await expect(button).toBeEnabled();
		await expect(button).toHaveText('Choose');
	}
	await expect(page.getByRole('button', { name: 'Starting…' })).toHaveCount(0);
}

/** Give any wrongly-issued second request a chance to arrive, then count. */
async function expectExactlyOneRequestAfterSettling(
	page: Page,
	requests: CheckoutRequestRecord[]
): Promise<void> {
	await page.waitForTimeout(300);
	expect(requests, 'exactly one checkout request may be in flight').toHaveLength(1);
}

test('held checkout: mouse activation of another plan issues no second request', async ({
	page
}) => {
	const held = await holdCheckout(page);
	const buttons = paidPlanButtons(page);

	await page.goto('/pricing');
	await buttons.plus.click();
	await held.waitForRequests(1);
	expect(held.requests[0]).toEqual({
		method: 'POST',
		pathname: '/v1/billing/checkout',
		body: { planId: 'plus' }
	});

	// Pending identity is on Plus, and every paid-plan action is disabled.
	await expect(buttons.plus).toHaveText('Starting…');
	await expect(buttons.plus).toBeDisabled();
	await expect(buttons.pro5x, 'other paid plans disable while a selection is pending').toBeDisabled();
	await expect(buttons.pro20x, 'other paid plans disable while a selection is pending').toBeDisabled();
	await expect(buttons.pro5x).toHaveText('Choose');
	await expect(buttons.pro20x).toHaveText('Choose');

	// A further mouse activation must not start another checkout.
	await buttons.pro5x.click({ force: true }).catch(() => {});
	await buttons.pro20x.click({ force: true }).catch(() => {});
	await expectExactlyOneRequestAfterSettling(page, held.requests);
	await expect(buttons.plus, 'pending identity stays on the first plan').toHaveText('Starting…');
	await expect(page.getByRole('button', { name: 'Starting…' })).toHaveCount(1);

	held.release(ORDINARY_FAILURE);
	await expect(page.getByRole('alert')).toContainText('Start checkout failed (500)');
	await expectAllChoicesRestored(page);
	expect(held.requests).toHaveLength(1);
});

test('held checkout: keyboard activation of another plan issues no second request', async ({
	page
}) => {
	const held = await holdCheckout(page);
	const buttons = paidPlanButtons(page);

	await page.goto('/pricing');
	await buttons.plus.click();
	await held.waitForRequests(1);
	await expect(buttons.plus).toHaveText('Starting…');

	// Try to move focus to another plan and activate it from the keyboard.
	await buttons.pro5x.evaluate((el) => (el as HTMLElement).focus());
	await page.keyboard.press('Enter');
	await page.keyboard.press('Space');

	await expectExactlyOneRequestAfterSettling(page, held.requests);
	expect(held.requests[0]?.body).toEqual({ planId: 'plus' });
	await expect(buttons.plus, 'pending identity stays on the first plan').toHaveText('Starting…');
	await expect(buttons.pro5x).toHaveText('Choose');
	await expect(buttons.pro5x).toBeDisabled();
	await expect(buttons.pro20x).toBeDisabled();

	held.release(ORDINARY_FAILURE);
	await expect(page.getByRole('alert')).toContainText('Start checkout failed (500)');
	await expectAllChoicesRestored(page);
	expect(held.requests).toHaveLength(1);
});

test('held checkout: handler refuses re-entry even if a button is re-enabled programmatically', async ({
	page
}) => {
	const held = await holdCheckout(page);
	const buttons = paidPlanButtons(page);

	await page.goto('/pricing');
	await buttons.plus.click();
	await held.waitForRequests(1);
	await expect(buttons.plus).toHaveText('Starting…');

	// Bypass the disabled attribute the way a pre-render race or a
	// programmatic click could: the handler guard alone must serialize.
	for (const button of [buttons.pro5x, buttons.plus]) {
		await button.evaluate((el) => {
			el.removeAttribute('disabled');
			(el as HTMLButtonElement).click();
		});
	}

	await expectExactlyOneRequestAfterSettling(page, held.requests);
	expect(held.requests[0]?.body).toEqual({ planId: 'plus' });
	await expect(buttons.plus, 'pending identity stays on the first plan').toHaveText('Starting…');
	await expect(buttons.pro5x).toHaveText('Choose');

	held.release(ORDINARY_FAILURE);
	await expect(page.getByRole('alert')).toContainText('Start checkout failed (500)');
	await expectAllChoicesRestored(page);
	expect(held.requests).toHaveLength(1);
});

test('ordinary failure restores choices and a later different selection succeeds', async ({
	page
}) => {
	await mockCheckoutDestination(page);
	const held = await holdCheckout(page);
	const buttons = paidPlanButtons(page);

	await page.goto('/pricing');
	await buttons.plus.click();
	await held.waitForRequests(1);
	held.release(ORDINARY_FAILURE);

	await expect(page.getByRole('alert')).toContainText(
		'Start checkout failed (500): checkout unavailable (fixture)'
	);
	await expectAllChoicesRestored(page);

	held.setResponse(SUCCESS);
	await buttons.pro5x.click();

	await expect(page).toHaveURL(MOCKED_CHECKOUT_URL);
	await expect(page.getByRole('heading', { name: 'Mocked checkout' })).toBeVisible();
	expect(held.requests).toEqual([
		{ method: 'POST', pathname: '/v1/billing/checkout', body: { planId: 'plus' } },
		{ method: 'POST', pathname: '/v1/billing/checkout', body: { planId: 'pro-5x' } }
	]);
});

test('successful selection navigates to the returned checkout URL with one exact request', async ({
	page
}) => {
	await mockCheckoutDestination(page);
	const stub = await stubCheckout(page, SUCCESS);

	await page.goto('/pricing');
	await paidPlanButtons(page).pro20x.click();

	await expect(page).toHaveURL(MOCKED_CHECKOUT_URL);
	await expect(page.getByRole('heading', { name: 'Mocked checkout' })).toBeVisible();
	expect(stub.requests).toEqual([
		{ method: 'POST', pathname: '/v1/billing/checkout', body: { planId: 'pro-20x' } }
	]);
});

test('held checkout destination: success keeps the lock until navigation commits', async ({
	page
}) => {
	const destination = await holdCheckoutDestination(page);
	const stub = await stubCheckout(page, SUCCESS);
	const buttons = paidPlanButtons(page);

	await page.goto('/pricing');
	// Capture the other plan's click point up front: once the redirect
	// starts, locator and evaluate calls block on the pending navigation,
	// but raw mouse input still reaches the still-interactive page —
	// exactly like a real user's click in that window.
	const pro5xBox = await buttons.pro5x.boundingBox();
	expect(pro5xBox, 'Pro 5x button is laid out before selection').not.toBeNull();
	// The destination stays held until this test releases it, and that
	// release is sequenced after this click returns. Skip only the
	// post-click navigation barrier; the pointer activation itself is
	// unchanged, so a premature lock release still fails below.
	await buttons.plus.click({ noWaitAfter: true });

	// Checkout succeeded and `window.location.href` has started the
	// cross-origin navigation, but the (held) checkout host has not
	// responded, so this page is still interactive. A further real
	// activation must not start a second checkout whose navigation
	// would replace the first.
	await destination.waitForArrival();
	await page.mouse.click(pro5xBox!.x + pro5xBox!.width / 2, pro5xBox!.y + pro5xBox!.height / 2);
	// Give any wrongly-issued second request a chance to arrive, then
	// count. (A test-process delay: the page itself is mid-navigation.)
	await new Promise((resolve) => setTimeout(resolve, 300));
	expect(stub.requests, 'exactly one checkout request may be issued').toHaveLength(1);
	expect(stub.requests[0]?.body).toEqual({ planId: 'plus' });

	destination.release();
	await expect(page).toHaveURL(MOCKED_CHECKOUT_URL);
	await expect(page.getByRole('heading', { name: 'Mocked checkout' })).toBeVisible();
	expect(stub.requests).toEqual([
		{ method: 'POST', pathname: '/v1/billing/checkout', body: { planId: 'plus' } }
	]);
});

test('401 selection hands off to signup with the chosen plan', async ({ page }) => {
	const stub = await stubCheckout(page, {
		status: 401,
		body: { code: 'unauthorized', message: 'sign in required' }
	});

	await page.goto('/pricing');
	await paidPlanButtons(page).plus.click();

	await expect(page).toHaveURL(/\/signup\?plan=plus$/);
	expect(stub.requests).toEqual([
		{ method: 'POST', pathname: '/v1/billing/checkout', body: { planId: 'plus' } }
	]);
});

test('402 selection shows the paywall, no inline alert, and restores choices', async ({ page }) => {
	const stub = await stubCheckout(page, {
		status: 402,
		body: { error: 'plan_limit_exceeded' }
	});

	await page.goto('/pricing');
	await paidPlanButtons(page).plus.click();

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(dialog.getByRole('heading')).toHaveText("You've reached your plan limit");
	await expect(page.getByRole('alert')).toHaveCount(0);
	expect(stub.requests).toEqual([
		{ method: 'POST', pathname: '/v1/billing/checkout', body: { planId: 'plus' } }
	]);

	await dialog.getByRole('button', { name: /not now/i }).click();
	await expect(dialog).toBeHidden();
	await expectAllChoicesRestored(page);
});

test('429 selection shows the paywall, no inline alert, and restores choices', async ({ page }) => {
	const stub = await stubCheckout(page, {
		status: 429,
		headers: { 'Retry-After': '45' },
		body: { error: 'rate_limited' }
	});

	await page.goto('/pricing');
	await paidPlanButtons(page).plus.click();

	const dialog = page.getByRole('dialog');
	await expect(dialog).toBeVisible();
	await expect(dialog.getByRole('heading')).toHaveText('Please slow down a little');
	await expect(page.getByRole('alert')).toHaveCount(0);
	expect(stub.requests).toEqual([
		{ method: 'POST', pathname: '/v1/billing/checkout', body: { planId: 'plus' } }
	]);

	await dialog.getByRole('button', { name: /not now/i }).click();
	await expect(dialog).toBeHidden();
	await expectAllChoicesRestored(page);
});
