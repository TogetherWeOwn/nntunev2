import { expect, test, type Page, type Route } from '@playwright/test';

test.use({ locale: 'en-US' });

const code = 'synthetic_invite_codeA';
const issued = { id: '1', code, expires_at: 1_893_456_000 };
const endpoint = '**/v1/me/invites';
const month = 30 * 86_400;
const bounds = { starts_at: 1_893_456_000, ends_at: 1_894_060_800 };
const snapshot = {
	paid_seconds: 3 * month, required_paid_seconds: 3 * month,
	entitled: true, eligible: true, per_window_limit: 1,
	open_window: { ...bounds, used: 0, remaining: 1 }, next_window: null
};
const mockInvite = (route: Route) => route.request().method() === 'GET'
	? route.fulfill({ json: snapshot })
	: route.fulfill({ status: 201, json: issued });

async function mockClipboard(page: Page, blocked = false) {
	await page.addInitScript((blocked) => {
		let value = '';
		Object.defineProperty(navigator, 'clipboard', {
			value: {
				writeText: async (text: string) => {
					if (blocked) throw new DOMException('Blocked in fixture', 'NotAllowedError');
					value = text;
				},
				readText: async () => value
			}
		});
	}, blocked);
}

test('creates one invite on demand and hands the copied link to signup', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockClipboard(page);
	const pending = Promise.withResolvers<void>();
	const refresh = Promise.withResolvers<void>();
	const methods: string[] = [];
	await page.route(endpoint, async (route) => {
		const method = route.request().method();
		methods.push(method);
		if (method === 'GET') expect(await route.request().headerValue('authorization')).toBeNull();
		if (method === 'GET' && methods.length > 1) {
			await refresh.promise;
			return route.fulfill({ json: { ...snapshot, open_window: { ...bounds, used: 1, remaining: 0 } } });
		}
		if (method === 'POST') await pending.promise;
		return mockInvite(route);
	});
	await page.route('**/v1/auth/passkey/register/start', async (route) => {
		// Only the email goes to registration start; opening a link spends nothing.
		expect(route.request().postDataJSON()).toEqual({ email: 'friend@example.test' });
		await route.fulfill({ json: { challenge: 'Zmlyc3Q=', user_id: 'dXNlcg', rp_id: 'localhost' } });
	});
	await page.goto('/account/invites');
	await expect(page.getByRole('link', { name: 'Invites' })).toHaveAttribute('aria-current', 'page');
	const create = page.getByRole('button', { name: 'Create invite' });
	await expect(create).toBeEnabled();
	await expect(page.getByText('Paid progress: 3 of 3 months.', { exact: false })).toBeVisible();
	await expect(page.getByText('You meet the membership and paid-time requirements.')).toBeVisible();
	await expect(page.getByText('Used: 0 · Remaining: 1')).toBeVisible();
	await expect(page.getByRole('region', { name: 'Invite summary' }).locator('time')).toHaveCount(2);
	expect(methods).toEqual(['GET']);
	const sent = page.waitForRequest(endpoint);
	await create.press('Enter');
	const request = await sent;
	expect(request.method()).toBe('POST');
	expect(request.postData()).toBeNull();
	expect(await request.headerValue('authorization')).toBeNull();
	await expect(page.getByRole('button', { name: 'Creating invite…' })).toBeDisabled();
	pending.resolve();

	const link = new URL(`/signup#invite=${code}`, page.url()).href;
	const field = page.getByRole('textbox', { name: 'Signup link' });
	await expect(field).toHaveValue(link);
	await expect(field).toBeFocused();
	await expect(page.getByRole('region', { name: 'Your invite link' }).locator('time'))
		.toHaveAttribute('datetime', new Date(issued.expires_at * 1000).toISOString());
	await expect(page.getByText('Loading invite summary…')).toBeVisible();
	refresh.resolve();
	await expect(page.getByText('Used: 1 · Remaining: 0')).toBeVisible();
	await expect(field).toHaveValue(link);
	await page.keyboard.press('Tab');
	await expect(page.getByRole('button', { name: 'Copy link' })).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(field).toHaveCount(0);
	await expect(page.getByText('Invite link copied.')).toBeVisible();
	await expect(create).toBeFocused();
	expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(link);
	expect(methods).toEqual(['GET', 'POST', 'GET']);

	await page.goto(link);
	await expect(page).toHaveURL(new URL('/signup', link).href);
	await expect(page.getByText('Invite link added.', { exact: false })).toBeVisible();
	expect(await page.evaluate(() => JSON.stringify(history.state))).not.toContain(code);
	await page.getByRole('textbox', { name: 'Email', exact: true }).fill('friend@example.test');
	await page.getByRole('button', { name: 'Create account with passkey' }).click();
	await expect(page.getByRole('textbox', { name: 'Invite code', exact: false })).toHaveValue(code);
	await page.reload();
	await expect(page.getByText('Invite link added.', { exact: false })).toHaveCount(0);
});

test('allows manual copy and restores only a freshly read snapshot', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockClipboard(page, true);
	let restored = false;
	const methods: string[] = [];
	await page.route(endpoint, (route) => {
		methods.push(route.request().method());
		return restored && route.request().method() === 'GET'
			? route.fulfill({ json: { ...snapshot, paid_seconds: 0, entitled: false, eligible: false, open_window: null } })
			: mockInvite(route);
	});
	await page.goto('/account/invites');
	await page.getByRole('button', { name: 'Create invite' }).click();
	await page.getByRole('button', { name: 'Copy link' }).click();
	await expect(page.getByText('Couldn’t copy automatically.', { exact: false })).toBeVisible();
	const field = page.getByRole('textbox', { name: 'Signup link' });
	await expect(field).toBeFocused();
	expect(await field.evaluate((input: HTMLInputElement) => input.selectionEnd)).toBe((await field.inputValue()).length);
	await expect(page.getByText('Used: 0 · Remaining: 1')).toBeVisible();
	await page.keyboard.press('Tab');
	await page.keyboard.press('Tab');
	await expect(page.getByRole('button', { name: 'Hide link' })).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(field).toHaveCount(0);
	const create = page.getByRole('button', { name: 'Create invite' });
	await expect(create).toBeFocused();
	await create.click();
	await expect(field).toBeVisible();
	const paid = page.getByText('Paid progress: 3 of 3 months.', { exact: false });
	await expect(paid).toBeVisible();
	await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true })));
	await expect(field).toHaveCount(0);
	await expect(paid).toHaveCount(0);
	restored = true;
	await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
	await expect(page.getByText('Paid progress: 0 of 3 months.', { exact: false })).toBeVisible();
	await expect(page.getByText('Your membership is not active.')).toBeVisible();
	await expect(page.getByText('Invites are closed.')).toBeVisible();
	await expect(page.getByText('No upcoming window is scheduled.')).toBeVisible();
	await expect(field).toHaveCount(0);
	expect(methods).toEqual(['GET', 'POST', 'GET', 'POST', 'GET', 'GET']);
});

test('prefills an invite opened in the signup tab during email verification', async ({ page }) => {
	await page.route('**/v1/auth/passkey/register/start', (route) =>
		route.fulfill({ json: { challenge: 'Zmlyc3Q=', user_id: 'dXNlcg', rp_id: 'localhost' } })
	);
	await page.goto('/signup');
	await page.getByRole('textbox', { name: 'Email', exact: true }).fill('friend@example.test');
	await page.getByRole('button', { name: 'Create account with passkey' }).click();
	const emailCode = page.getByRole('textbox', { name: 'Email code', exact: true });
	await emailCode.fill('123456');
	await page.evaluate((code) => { location.hash = `invite=${code}`; }, code);
	await expect(page.getByRole('textbox', { name: 'Invite code', exact: false })).toHaveValue(code);
	await expect(emailCode).toHaveValue('123456');
	await expect(page).toHaveURL(/\/signup$/);
	expect(await page.evaluate(() => JSON.stringify(history.state))).not.toContain(code);
});

async function holdResponse(page: Page, method: 'GET' | 'POST', result: unknown) {
	await page.evaluate(({ method, result }) => {
		const fetch = window.fetch;
		let held = false;
		// Deliberately ignore abort and deliver the response after invalidation.
		window.fetch = (input, init) => {
			if (held || !String(input).endsWith('/v1/me/invites') || init?.method !== method) return fetch(input, init);
			held = true;
			document.documentElement.dataset.inviteFixture = method;
			return new Promise((resolve) => window.addEventListener(`release-invite-${method}`, () => {
				resolve(new Response(JSON.stringify(result), { status: method === 'GET' ? 200 : 201 }));
			}, { once: true }));
		};
	}, { method, result });
}

async function releaseResponse(page: Page, method: 'GET' | 'POST') {
	await page.evaluate((method) => {
		window.dispatchEvent(new Event(`release-invite-${method}`));
		return new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
	}, method);
}

for (const boundary of ['library signal', 'auth fence']) test(`${boundary} clears an invite and ignores held responses`, async ({ page }) => {
	await page.route(endpoint, mockInvite);
	await page.route('**/v1/session/logout', (route) => route.fulfill({ status: 200, body: '{' }));
	await page.goto('/account/invites');
	const create = page.getByRole('button', { name: 'Create invite' });
	const field = page.getByRole('textbox', { name: 'Signup link' });
	let signOuts = 0;
	async function authTransition() {
		if (boundary === 'library signal') return page.evaluate(() => window.dispatchEvent(new StorageEvent('storage', {
			key: 'nntune.library.auth-transition.v1', newValue: 'synthetic-session-change', storageArea: localStorage
		})));
		await page.getByRole('button', { name: 'Account menu' }).click();
		await page.getByRole('menuitem', { name: 'Sign out' }).click();
		await expect(page.getByText('Could not sign out. Please try again.')).toHaveCount(++signOuts);
	}
	const paid = page.getByText('Paid progress:', { exact: false });
	await expect(paid).toBeVisible();
	await holdResponse(page, 'GET', { ...snapshot, paid_seconds: 19 * month });
	await create.click();
	await expect(field).toBeVisible();
	await expect(page.locator('html')).toHaveAttribute('data-invite-fixture', 'GET');
	await authTransition();
	await expect(field).toHaveCount(0);
	await expect(paid).toHaveCount(0);
	await page.getByRole('button', { name: 'Refresh summary' }).click();
	await expect(paid).toContainText('3 of 3 months');
	await releaseResponse(page, 'GET');
	await expect(paid).toContainText('3 of 3 months');

	await holdResponse(page, 'POST', issued);
	await create.click();
	await expect(page.locator('html')).toHaveAttribute('data-invite-fixture', 'POST');
	await authTransition();
	await releaseResponse(page, 'POST');
	await expect(page).toHaveURL(/\/account\/invites$/);
	await expect(field).toHaveCount(0);
	await expect(paid).toHaveCount(0);
	await expect(page.getByRole('alert')).toHaveCount(0);
});

for (const zero of [false, true]) test(`shows closed and upcoming policy with ${zero ? 'zero' : 'unmet'} requirements`, async ({ page }) => {
	await page.route(endpoint, (route) => {
		expect(route.request().method()).toBe('GET');
		return route.fulfill({ json: {
			...snapshot, paid_seconds: zero ? 0 : 3 * month - 1, required_paid_seconds: zero ? 0 : 3 * month,
			per_window_limit: zero ? 0 : 1, eligible: zero, open_window: null, next_window: bounds
		} });
	});
	await page.goto('/account/invites');
	await expect(page.getByText(zero ? 'Paid progress: 0 of 0 months.' : 'Paid progress: 2.99 of 3 months.', { exact: false })).toBeVisible();
	await expect(page.getByText(zero ? 'You meet the membership and paid-time requirements.' : 'You haven’t reached the paid-time requirement.')).toBeVisible();
	await expect(page.getByText('Invites are closed.')).toBeVisible();
	await expect(page.getByRole('heading', { name: 'Next scheduled window' })).toBeVisible();
	await expect(page.locator('time').first()).toHaveAttribute('datetime', new Date(bounds.starts_at * 1000).toISOString());
	await expect(page.locator('time').last()).toHaveAttribute('datetime', new Date(bounds.ends_at * 1000).toISOString());
	await expect(page.getByText(`Policy: up to ${zero ? 0 : 1} per member per window.`, { exact: false })).toBeVisible();
	await expect(page.getByText('Remaining:', { exact: false })).toHaveCount(0);
});

const readErrors = [
	[401, {}, 'Sign in to see your invite summary.'],
	[409, { code: 'account_deleting', message: 'Fixture account is being deleted', retryable: false }, 'scheduled for deletion'],
	[500, {}, 'Couldn’t load your invite summary.']
] as const;

for (const [status, json, detail] of readErrors) test(`handles a ${status} refresh after one issuance`, async ({ page }) => {
	const methods: string[] = [];
	await page.route(endpoint, (route) => {
		methods.push(route.request().method());
		return methods.length === 3 ? route.fulfill({ status, json }) : mockInvite(route);
	});
	await page.goto('/account/invites');
	await page.getByRole('button', { name: 'Create invite' }).click();
	await expect(page.getByText(detail, { exact: false })).toBeVisible();
	await expect(page.getByRole('textbox', { name: 'Signup link' })).toHaveCount(status === 500 ? 1 : 0);
	expect(methods).toEqual(['GET', 'POST', 'GET']);
});

for (const [status, json, detail, label = status] of [
	...readErrors,
	[403, {}, 'Access to the invite summary was denied.'],
	[200, { ...snapshot, open_window: { ...bounds, used: 0 } }, 'Couldn’t load your invite summary.', 'missing count'],
	[200, { ...snapshot, next_window: { ...bounds, ends_at: bounds.starts_at } }, 'Couldn’t load your invite summary.', 'empty window']
] as const) test(`does not show aggregates while loading or after an unusable read (${label})`, async ({ page }) => {
	const pending = Promise.withResolvers<void>();
	await page.route(endpoint, async (route) => {
		expect(route.request().method()).toBe('GET');
		await pending.promise;
		await route.fulfill({ status, json });
	});
	await page.goto('/account/invites');
	await expect(page.getByText('Loading invite summary…')).toBeVisible();
	const paid = page.getByText('Paid progress:', { exact: false });
	await expect(paid).toHaveCount(0);
	pending.resolve();
	await expect(page.getByText(detail, { exact: false })).toBeVisible();
	await expect(paid).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Create invite' })).toHaveCount(0);
});

for (const [status, detail] of [
	[401, 'Sign in to create an invite.'],
	[403, 'You need an active membership'],
	[409, 'The invite window is closed or you’ve used all your invites'],
	[201, 'No link was received.'],
	[500, 'No link was received.']
] as const) {
	test(`shows no link for an unusable ${status} issuance response`, async ({ page }) => {
		let posts = 0;
		await page.route(endpoint, (route) => {
			if (route.request().method() === 'GET') return mockInvite(route);
			posts++;
			return route.fulfill({ status, json: { ...issued, code: 'malformed' } });
		});
		await page.goto('/account/invites');
		await page.getByRole('button', { name: 'Create invite' }).click();
		await expect(page.getByText(detail, { exact: false })).toBeVisible();
		await expect(page.getByRole('textbox', { name: 'Signup link' })).toHaveCount(0);
		if (status === 401) await expect(page.getByRole('main').getByRole('link', { name: 'Sign in' })).toHaveAttribute('href', '/login');
		expect(posts).toBe(1);
	});
}
