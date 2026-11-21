import { expect, test, type Page, type Route } from '@playwright/test';

const path = '/admin/invites';
const api = '/v1/admin/invites';
const issuer = 'Abcdefghijkl01234-__gA';
const generation = 'nntune.auth.generation.v1';
const row = { id: 17, issued_by: issuer, status: 'active', expires_at: 0, code: 'fixture-code', code_hash: 'fixture-hash' };
const fixture = [row, { ...row, id: 18, status: 'redeemed' }, { ...row, id: 19, status: 'revoked' }];
const results = (page: Page) => page.getByRole('list', { name: 'Invite results' });
const apply = async (page: Page, value: string) => {
	await page.getByLabel('Filter by').selectOption(value);
	await page.getByRole('button', { name: 'Apply filter' }).press('Enter');
};

async function mock(page: Page, respond?: (route: Route, pathname: string) => Promise<void>) {
	const calls: string[] = [];
	await page.route(/\/v1\/admin\/invites(?:\/[^?]*)?(?:\?.*)?$/, async (route) => {
		const request = route.request();
		const { pathname, search } = new URL(request.url());
		expect(search).toBe('');
		expect(await request.headerValue('authorization')).toBeNull();
		if (request.method() === 'POST') expect(request.postData()).toBeNull();
		calls.push(`${request.method()} ${pathname}`);
		await (respond ? respond(route, pathname) : route.fulfill({ json: { invites: fixture } }));
	});
	return calls;
}

test('shows only the returned ledger rows and applies one server-side filter at a time', async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 800 });
	const pending = Promise.withResolvers<void>();
	await page.route('**/v1/admin/invite-windows', (route) => route.fulfill({ json: { windows: [] } }));
	const calls = await mock(page, async (route, pathname) => {
		if (pathname === api) await pending.promise;
		await route.fulfill({ json: { invites: pathname === api ? fixture : pathname.endsWith('/redeemed') ? [fixture[1]] : [] } });
	});
	await page.goto('/admin/invite-windows');
	await page.getByRole('link', { name: 'Invite ledger' }).press('Enter');
	await expect(page).toHaveURL(path);
	await expect(page.getByText('Loading invites…')).toBeVisible();
	await expect(results(page)).toHaveCount(0);
	pending.resolve();
	await expect(results(page).getByRole('listitem')).toHaveCount(3);
	await expect(page.getByText(/Up to 200 matching invites.*not a complete history/)).toBeVisible();
	await expect(results(page)).toContainText('Effective expiry (UTC): Jan 1, 1970');
	await expect(results(page)).not.toContainText(/fixture-code|fixture-hash/);
	await expect(results(page).getByRole('button')).toHaveCount(1);
	expect(await page.locator('main').evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
	await apply(page, 'redeemed');
	await expect(results(page)).toContainText('Invite #18');
	await expect(results(page).getByRole('listitem')).toHaveCount(1);
	await page.getByLabel('Filter by').selectOption('issuer');
	const search = page.getByLabel('Exact issuer user ID');
	await search.fill('../status/active?foo=1');
	await search.press('Enter');
	await expect(page.getByRole('alert')).toContainText('22-character user ID');
	await search.fill(issuer);
	await search.press('Enter');
	await expect(page.getByText('No invites match this filter.')).toBeVisible();
	await expect(results(page)).toHaveCount(0);
	expect(calls).toEqual([`GET ${api}`, `GET ${api}/status/redeemed`, `GET ${api}/issuer/${issuer}`]);
});

for (const status of [404, 500, 200]) test(`does not expose ledger controls after an invalid read (${status})`, async ({ page }) => {
	const calls = await mock(page, (route) => route.fulfill({ status, json: { invites: [{ ...row, status: 'unexpected' }] } }));
	await page.goto(path);
	await expect(page.getByRole('alert')).toContainText('Reload this page to check access');
	await expect(page.getByLabel('Filter by')).toHaveCount(0);
	await expect(results(page)).toHaveCount(0);
	await expect(page.getByRole('button', { name: 'Revoke', exact: false })).toHaveCount(0);
	expect(calls).toEqual([`GET ${api}`]);
});

test('cancel sends nothing; confirmed revoke sends one bodyless write and reloads Active', async ({ page }) => {
	const pending = Promise.withResolvers<void>();
	let revoked = false;
	let activeReads = 0;
	const calls = await mock(page, async (route, pathname) => {
		if (pathname.endsWith('/revoke')) {
			await pending.promise;
			revoked = true;
			return route.fulfill({ json: { id: row.id, status: 'revoked' } });
		}
		if (pathname.endsWith('/active')) activeReads++;
		return route.fulfill({ json: { invites: revoked ? [] : [row] } });
	});
	await page.goto(path);
	await apply(page, 'active');
	await expect(page.getByText('Results: Active')).toBeVisible();
	const revoke = page.getByRole('button', { name: 'Revoke invite #17' });
	page.once('dialog', (dialog) => dialog.dismiss());
	await revoke.press('Enter');
	expect(calls).toEqual([`GET ${api}`, `GET ${api}/status/active`]);
	page.once('dialog', async (dialog) => {
		expect(dialog.message()).toContain('Revoke invite #17?');
		await dialog.accept();
	});
	await revoke.press('Enter');
	await expect(page.getByText('Revoking invite…')).toBeVisible();
	await expect(revoke).toBeDisabled();
	await revoke.press('Enter');
	await expect(page.getByText('Invite #17 revoked.')).toHaveCount(0);
	pending.resolve();
	await expect(page.getByText('Invite #17 revoked.')).toBeVisible();
	await expect(page.getByText('No invites match this filter.')).toBeVisible();
	await expect(results(page)).toHaveCount(0);
	expect(activeReads).toBe(2);
	expect(calls).toEqual([`GET ${api}`, `GET ${api}/status/active`, `POST ${api}/17/revoke`, `GET ${api}/status/active`]);
});

for (const [name, status, json] of [
	['redemption', 422, { code: 'validation_failed' }], ['admin refusal', 404, {}],
	['reauthentication', 401, { code: 'reauth_required' }], ['wrong ID', 200, { id: 18, status: 'revoked' }],
	['wrong status', 200, { id: 17, status: 'active' }], ['network loss', 0, {}],
	['access lost on refresh', 200, { id: 17, status: 'revoked' }]
] as const) test(`requires a fresh authorized list after ${name}, without replay`, async ({ page }) => {
	let reads = 0;
	const calls = await mock(page, (route, pathname) => {
		if (pathname === api) return reads++ ? route.fulfill({ status: 404 }) : route.fulfill({ json: { invites: [row] } });
		return status === 0 ? route.abort() : route.fulfill({ status, json });
	});
	await page.goto(path);
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Revoke invite #17' }).press('Enter');
	await expect(page.getByRole('alert')).toContainText(/reload|sign in again/i);
	await expect(page.getByText('Invite #17 revoked.')).toHaveCount(0);
	await expect(results(page)).toHaveCount(0);
	expect(calls).toEqual([`GET ${api}`, `POST ${api}/17/revoke`, ...(name === 'access lost on refresh' ? [`GET ${api}`] : [])]);
});

async function holdResponse(page: Page, heldPath: string, payload: unknown) {
	await page.addInitScript(({ heldPath, payload }) => {
		const original = window.fetch;
		window.fetch = (input, init) => {
			if (new URL(String(input), location.href).pathname !== heldPath) return original(input, init);
			return new Promise((resolve) => {
				document.documentElement.dataset.inviteFixture = 'held';
				window.addEventListener('release-invite-fixture', () => resolve(Response.json(payload)), { once: true });
			});
		};
	}, { heldPath, payload });
}

for (const method of ['GET', 'POST']) test(`does not publish a late ${method} after the auth generation changes`, async ({ page }) => {
	await mock(page);
	await holdResponse(page, method === 'GET' ? api : `${api}/17/revoke`,
		method === 'GET' ? { invites: fixture } : { id: 17, status: 'revoked' });
	await page.goto(path);
	if (method === 'POST') {
		page.once('dialog', (dialog) => dialog.accept());
		await page.getByRole('button', { name: 'Revoke invite #17' }).press('Enter');
	}
	await expect(page.locator('html')).toHaveAttribute('data-invite-fixture', 'held');
	await page.evaluate((key) => {
		// No storage event reaches this tab; publication must recheck the fence.
		localStorage.setItem(key, 'other-fixture-generation');
		window.dispatchEvent(new Event('release-invite-fixture'));
	}, generation);
	await expect(page.getByRole('alert')).toContainText('Invite ledger is unavailable');
	await expect(results(page)).toHaveCount(0);
	await expect(page.getByText('Invite #17 revoked.')).toHaveCount(0);
});

test('does not publish a superseded read even when transport ignores abort', async ({ page }) => {
	await mock(page, (route, pathname) => route.fulfill({ json: { invites: pathname.endsWith('/revoked') ? [fixture[2]] : fixture } }));
	await holdResponse(page, `${api}/status/active`, { invites: [row] });
	await page.goto(path);
	await apply(page, 'active');
	await expect(page.locator('html')).toHaveAttribute('data-invite-fixture', 'held');
	await expect(results(page)).toHaveCount(0);
	await apply(page, 'revoked');
	await expect(results(page)).toContainText('Invite #19');
	await page.evaluate(async () => {
		window.dispatchEvent(new Event('release-invite-fixture'));
		await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
	});
	await expect(results(page).getByRole('listitem')).toHaveCount(1);
	await expect(results(page)).toContainText('Invite #19');
});
