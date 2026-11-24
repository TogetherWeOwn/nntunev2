import { expect, test, type Page, type Route } from '@playwright/test';

test.use({ locale: 'en-US' });

const code = 'synthetic_invite_codeA';
const issued = { id: '1', code, expires_at: 1_893_456_000 };
const endpoint = '**/v1/me/invites';
const historyEndpoint = (url: URL) => url.pathname === '/v1/me/invites/history';
const emptyHistory = { invites: [], next_cursor: null };

test.beforeEach(async ({ page }) => {
	await page.route(historyEndpoint, (route) => route.fulfill({ json: emptyHistory }));
});
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

const historyRow = (id: number, status: string) => ({
	id, status, created_at: bounds.starts_at + id, expires_at: bounds.starts_at + 60
});
const deleting = { code: 'account_deleting', message: 'Fixture account is being deleted', retryable: false };
const historyRegion = (page: Page) => page.getByRole('region', { name: 'Issued invites' });
const opaqueCursor = 'synthetic+cursor/&';

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
		// Registration start validates the linked invite without spending it.
		expect(route.request().postDataJSON()).toEqual({ email: 'friend@example.test', invite_code: code });
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
	await expect(page.getByRole('textbox', { name: 'Email code', exact: true })).toBeVisible();
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

for (const [status, json, detail] of [
	...readErrors, [403, {}, 'Access to the invite summary was denied.']
] as const) test(`handles a ${status} refresh after one issuance`, async ({ page }) => {
	const methods: string[] = [];
	const queries: string[] = [];
	const finishSummary = Promise.withResolvers<void>();
	await page.route(endpoint, async (route) => {
		methods.push(route.request().method());
		if (methods.length !== 3) return mockInvite(route);
		await finishSummary.promise;
		return route.fulfill({ status, json: status === 409 ? {} : json });
	});
	await page.route(historyEndpoint, (route) => {
		queries.push(new URL(route.request().url()).search);
		return route.fulfill({ json: queries.length === 1
			? { invites: [historyRow(1, 'active')], next_cursor: opaqueCursor }
			: { invites: [historyRow(2, 'redeemed')], next_cursor: null } });
	});
	await page.goto('/account/invites');
	const region = historyRegion(page);
	await expect(region.getByText('Active', { exact: true })).toBeVisible();
	const refreshedHistory = page.waitForResponse((response) => historyEndpoint(new URL(response.url())));
	await page.getByRole('button', { name: 'Create invite' }).click();
	await refreshedHistory;
	await expect(region).toHaveCount(0);
	finishSummary.resolve();
	await expect(page.getByText(detail, { exact: false })).toBeVisible();
	await expect(page.getByRole('textbox', { name: 'Signup link' })).toHaveCount(status === 500 || status === 403 ? 1 : 0);
	if (status === 500) {
		await expect(region.getByText('Redeemed', { exact: true })).toBeVisible();
		await expect(region.getByRole('listitem')).toHaveCount(1);
	} else {
		await expect(region).toHaveCount(0);
	}
	expect(queries).toEqual(['', '']);
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

test('shows server status and effective expiry, with keyboard paging and append retry at 320px', async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 720 });
	await page.clock.setFixedTime(new Date((bounds.starts_at + 55) * 1000));
	await page.route(endpoint, mockInvite);
	const invites = ['active', 'redeemed', 'revoked', 'expired'].map((status, index) => historyRow(50 - index, status));
	const hidden = { code, link: `https://example.test/signup#invite=${code}`, email: 'fixture@example.test', billing: 'fixture-billing' };
	const cursors: (string | null)[] = [];
	const failAppend = Promise.withResolvers<void>();
	await page.route(historyEndpoint, async (route) => {
		expect(route.request().method()).toBe('GET');
		expect(await route.request().headerValue('authorization')).toBeNull();
		cursors.push(new URL(route.request().url()).searchParams.get('cursor'));
		if (cursors.length === 1) return route.fulfill({ json: {
			invites: invites.map((row) => ({ ...row, ...hidden })), next_cursor: opaqueCursor
		} });
		if (cursors.length === 2) {
			await failAppend.promise;
			return route.fulfill({ status: 500, json: {} });
		}
		return route.fulfill({ json: { invites: [historyRow(1, 'active')], next_cursor: null } });
	});
	await page.goto('/account/invites');
	const region = historyRegion(page);
	const rows = region.getByRole('listitem');
	await expect(rows).toHaveCount(4);
	for (const [index, label] of ['Active', 'Redeemed', 'Revoked', 'Expired'].entries()) {
		await expect(rows.nth(index).getByText(label, { exact: true })).toBeVisible();
	}
	const times = rows.last().locator('time');
	// The early-closed row is expired on the server even though its shortened expiry is in the browser's future.
	await expect(times.first()).toHaveAttribute('datetime', new Date(invites[3]!.created_at * 1000).toISOString());
	await expect(times.last()).toHaveAttribute('datetime', new Date(invites[3]!.expires_at * 1000).toISOString());
	const markup = await region.innerHTML();
	for (const value of [...Object.values(hidden), opaqueCursor]) expect(markup).not.toContain(value);
	await expect(page).toHaveURL(/\/account\/invites$/);
	expect(await region.evaluate((section) => section.scrollWidth)).toBeLessThanOrEqual(320);

	const more = region.getByRole('button', { name: 'Load more invites' });
	await page.getByRole('button', { name: 'Create invite' }).focus();
	await page.keyboard.press('Tab');
	await expect(more).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(more).toBeDisabled();
	await more.evaluate((button: HTMLButtonElement) => { button.click(); button.click(); });
	failAppend.resolve();
	await expect(region.getByText('Couldn’t load more invites.')).toBeVisible();
	await expect(rows).toHaveCount(4);
	const retry = region.getByRole('button', { name: 'Retry loading invites' });
	await expect(retry).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(rows).toHaveCount(5);
	await expect(region.getByRole('button')).toHaveCount(0);
	await expect(region.getByRole('status')).toBeFocused();
	expect(cursors).toEqual([null, opaqueCursor, opaqueCursor]);
});

for (const [status, json, message] of [
	[200, emptyHistory, 'No invites issued yet.'],
	[200, { invites: [historyRow(1, 'unknown')], next_cursor: null }, 'Couldn’t load issued invites.'],
	[500, {}, 'Couldn’t load issued invites.'],
	[403, {}, 'Access to issued invites was denied.'],
	[409, deleting, 'scheduled for deletion']
] as const) test(`has no rows for ${status}: ${message}`, async ({ page }) => {
	await page.route(endpoint, mockInvite);
	await page.route(historyEndpoint, (route) => route.fulfill({ status, json }));
	await page.goto('/account/invites');
	await expect(page.getByText(message, { exact: status !== 409 })).toBeVisible();
	await expect(historyRegion(page).getByRole('listitem')).toHaveCount(0);
});

for (const [status, json, message] of [
	[403, {}, 'Access to issued invites was denied.'],
	[409, {}, 'scheduled for deletion']
] as const) test(`clears loaded history on ${status} during paging`, async ({ page }) => {
	await page.route(endpoint, mockInvite);
	await page.route(historyEndpoint, (route) => new URL(route.request().url()).search
		? route.fulfill({ status, json })
		: route.fulfill({ json: { invites: [historyRow(3, 'active')], next_cursor: opaqueCursor } }));
	await page.goto('/account/invites');
	const region = historyRegion(page);
	await expect(region.getByRole('listitem')).toHaveCount(1);
	await region.getByRole('button', { name: 'Load more invites' }).click();
	await expect(page.getByText(message, { exact: status !== 409 })).toBeVisible();
	await expect(region.getByRole('listitem')).toHaveCount(0);
	await expect(region.getByRole('button')).toHaveCount(0);
});

for (const boundary of ['session change', 'restore', 'issuance']) test(`discards a pending append on ${boundary}`, async ({ page }) => {
	const methods: string[] = [];
	const queries: string[] = [];
	await page.route(endpoint, (route) => { methods.push(route.request().method()); return mockInvite(route); });
	await page.route(historyEndpoint, (route) => {
		queries.push(new URL(route.request().url()).search);
		return route.fulfill({ json: queries.length === 1
			? { invites: [historyRow(3, 'active')], next_cursor: opaqueCursor }
			: { invites: [historyRow(4, 'redeemed')], next_cursor: null } });
	});
	await page.goto('/account/invites');
	const region = historyRegion(page);
	await expect(region.getByText('Active', { exact: true })).toBeVisible();
	await page.evaluate((stale) => {
		const fetch = window.fetch;
		const fixture = document.documentElement.dataset;
		window.fetch = (input, init) => {
			if (!String(input).includes('/v1/me/invites/history?')) return fetch(input, init);
			fixture.historyFixture = 'pending';
			init?.signal?.addEventListener('abort', () => { fixture.historyFixture = 'aborted'; }, { once: true });
			// Simulate a transport resolving after abort.
			return new Promise((resolve) => window.addEventListener('release-history', () => resolve(new Response(JSON.stringify(stale))), { once: true }));
		};
	}, { invites: [historyRow(2, 'revoked')], next_cursor: null });
	await region.getByRole('button', { name: 'Load more invites' }).click();
	await expect(page.locator('html')).toHaveAttribute('data-history-fixture', 'pending');

	if (boundary === 'issuance') {
		await page.getByRole('button', { name: 'Create invite' }).click();
		await expect(page.getByRole('textbox', { name: 'Signup link' })).toHaveValue(new RegExp(`#invite=${code}$`));
	} else if (boundary === 'restore') {
		await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true })));
		await expect(region).toHaveCount(0);
		await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
	} else {
		await page.evaluate(() => {
			const key = 'nntune.auth.generation.v1';
			localStorage.setItem(key, 'fixture-new-session');
			window.dispatchEvent(new StorageEvent('storage', { key, storageArea: localStorage }));
		});
		await expect(region).toHaveCount(0);
		await page.getByRole('button', { name: 'Refresh summary' }).click();
	}
	await expect(page.locator('html')).toHaveAttribute('data-history-fixture', 'aborted');
	await expect(region.getByText('Redeemed', { exact: true })).toBeVisible();
	await page.evaluate(() => {
		window.dispatchEvent(new Event('release-history'));
		return new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
	});
	await expect(region.getByRole('listitem')).toHaveCount(1);
	await expect(region.getByText('Redeemed', { exact: true })).toBeVisible();
	await expect(region.getByRole('textbox')).toHaveCount(0);
	expect(await region.innerHTML()).not.toContain(code);
	expect(queries).toEqual(['', '']);
	expect(methods).toEqual(boundary === 'issuance' ? ['GET', 'POST', 'GET'] : ['GET', 'GET']);
});
