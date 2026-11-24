import { expect, test, type Page, type Route } from '@playwright/test';

const issuePath = '/admin/invites/issue';
const batchPath = '/v1/admin/invites/batch';
const expires_at = 1_893_456_000;
const codes = Array.from({ length: 50 }, (_, index) => `synthetic_batch_code${String(index).padStart(2, '0')}`);
const create = (page: Page) => page.getByRole('button', { name: 'Create batch', exact: true });
const result = (page: Page) => page.getByRole('textbox', { name: 'Signup links (one per line)' });
const countField = (page: Page) => page.getByRole('spinbutton', { name: 'Number of invites' });
const expiryField = (page: Page) => page.getByRole('spinbutton', { name: 'Expiry in days (optional)' });
const ledger = (page: Page) => page.getByRole('link', { name: 'Back to invite ledger' });
const success = (route: Route, count: number) => route.fulfill({ status: 201, json: { codes: codes.slice(0, count), expires_at } });

async function mock(page: Page, write = (route: Route) => success(route, 1)) {
	const writes: unknown[] = [];
	page.on('request', (request) => {
		if (['POST', 'PUT', 'PATCH', 'DELETE'].includes(request.method())) writes.push(new URL(request.url()).pathname);
	});
	const bodies: unknown[] = [];
	await page.route(/\/v1\/admin\/invites(?:\/batch)?(?:\?.*)?$/, async (route) => {
		const request = route.request();
		if (request.method() === 'GET') return route.fulfill({ json: { invites: [] } });
		const url = new URL(request.url());
		expect([request.method(), url.pathname, url.search, url.hash]).toEqual(['POST', batchPath, '', '']);
		expect(await request.headerValue('authorization')).toBeNull();
		bodies.push(request.postDataJSON());
		await write(route);
	});
	return { writes, bodies };
}

async function openBatch(page: Page) {
	await page.goto(issuePath);
	await page.getByRole('combobox', { name: 'Issuance mode' }).selectOption({ label: 'Invite batch' });
	await expect(page.getByRole('textbox', { name: 'Email (optional)' })).toHaveCount(0);
	await expect(create(page)).toBeEnabled();
}

for (const count of [1, 50]) test(`issues exactly one batch of ${count} and copies only its fragment links`, async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 800 });
	await page.addInitScript(() => {
		let copied = '';
		Object.defineProperty(navigator, 'clipboard', { value: {
			writeText: async (value: string) => { copied = value; }, readText: async () => copied
		} });
	});
	const release = Promise.withResolvers<void>();
	const { bodies, writes } = await mock(page, async (route) => { await release.promise; await success(route, count); });
	await openBatch(page);
	await countField(page).fill(String(count));
	if (count === 50) {
		await expiryField(page).fill('30');
		await create(page).dblclick();
	} else await create(page).press('Enter');
	await expect(page.getByRole('button', { name: 'Creating batch…' })).toBeDisabled();
	await page.locator('main form').evaluate((form: HTMLFormElement) => form.requestSubmit());
	const body = count === 50 ? { count, expires_in_days: 30 } : { count };
	await expect.poll(() => bodies).toEqual([body]);
	release.resolve();
	const links = codes.slice(0, count).map((code) => new URL(`/signup#invite=${code}`, page.url()).href).join('\n');
	await expect(result(page)).toHaveValue(links);
	await expect(result(page)).toBeFocused();
	await expect(page.locator('main time')).toHaveAttribute('datetime', new Date(expires_at * 1000).toISOString());
	expect(await page.locator('main').evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
	await page.keyboard.press('Tab');
	await expect(page.getByRole('button', { name: 'Copy all links' })).toBeFocused();
	await page.keyboard.press('Enter');
	await expect(result(page)).toHaveCount(0);
	await expect(create(page)).toBeFocused();
	expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(links);
	expect(await page.evaluate(() => JSON.stringify([location.href, history.state, localStorage, sessionStorage, document.cookie]))).not.toContain(codes[0]);
	expect({ bodies, writes }).toEqual({ bodies: [body], writes: [batchPath] });
});

test('validates count and expiry and permits manual copy or hide after clipboard failure', async ({ page }) => {
	await page.addInitScript(() => Object.defineProperty(navigator, 'clipboard', { value: {
		writeText: async () => { throw new Error('Fixture clipboard unavailable'); }
	} }));
	const { bodies } = await mock(page);
	await openBatch(page);
	for (const [field, invalid, valid] of [
		[countField(page), ['', '0', '51', '1.5'], '1'], [expiryField(page), ['0', '31', '1.5'], '']
	] as const) {
		for (const value of invalid) {
			await field.fill(value);
			await create(page).press('Enter');
			await expect(field).toBeFocused();
			expect(bodies).toEqual([]);
		}
		await field.fill(valid);
	}
	await create(page).press('Enter');
	await expect(result(page)).toBeFocused();
	await page.getByRole('button', { name: 'Copy all links' }).press('Enter');
	await expect(page.getByRole('alert')).toContainText('Copy the selected links');
	await expect(result(page)).toBeFocused();
	expect(await result(page).evaluate((el: HTMLTextAreaElement) => el.value.slice(el.selectionStart, el.selectionEnd)))
		.toBe(new URL(`/signup#invite=${codes[0]}`, page.url()).href);
	await page.getByRole('button', { name: 'Hide links' }).press('Enter');
	await expect(result(page)).toHaveCount(0);
	await expect(create(page)).toBeFocused();
	expect(bodies).toEqual([{ count: 1 }]);
});

for (const [status, code] of [[401, 'unauthorized'], [404, 'not_found'], [409, 'account_deleting']] as const) {
	test(`batch denial ${status} clears authorization and reveals nothing`, async ({ page }) => {
		const { writes } = await mock(page, (route) => route.fulfill({ status, json: { code } }));
		await openBatch(page);
		await create(page).click();
		await expect(page.getByRole('alert')).toContainText('check admin access');
		await expect(create(page)).toHaveCount(0);
		await expect(result(page)).toHaveCount(0);
		expect(writes).toEqual([batchPath]);
	});
}

for (const failure of ['lost response', 'partial response', 'duplicate codes']) test(`${failure} points to the ledger without retry`, async ({ page }) => {
	const { writes } = await mock(page, (route) => {
		if (failure === 'lost response') return route.abort('failed');
		if (failure === 'partial response') return success(route, 1);
		return route.fulfill({ status: 201, json: { codes: [codes[0], codes[0]], expires_at } });
	});
	await openBatch(page);
	await countField(page).fill('2');
	await create(page).click();
	await expect(page.getByRole('alert')).toContainText('The invites may have been created');
	await expect(page.getByRole('alert')).not.toContainText(codes[0]);
	await expect(result(page)).toHaveCount(0);
	await expect(create(page)).toHaveCount(0);
	await ledger(page).click();
	await expect(page).toHaveURL(/\/admin\/invites$/);
	expect(writes).toEqual([batchPath]);
});

for (const boundary of ['auth change', 'BFCache restore']) test(`discards a delayed batch after ${boundary}`, async ({ page }) => {
	await page.addInitScript((path) => {
		const fetch = window.fetch.bind(window);
		window.fetch = (input, init) => fetch(input, String(input).endsWith(path) ? { ...init, signal: undefined } : init);
	}, batchPath);
	const release = Promise.withResolvers<void>();
	const { bodies, writes } = await mock(page, async (route) => { await release.promise; await success(route, 1); });
	await openBatch(page);
	await create(page).click();
	await expect.poll(() => bodies).toEqual([{ count: 1 }]);
	await page.evaluate((boundary) => {
		if (boundary === 'auth change') {
			const key = 'nntune.auth.generation.v1';
			localStorage.setItem(key, 'fixture-next-generation');
			dispatchEvent(new StorageEvent('storage', { key }));
		} else {
			dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true }));
			dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
		}
	}, boundary);
	await expect(page.getByRole('alert')).toContainText('The invites may have been created');
	const response = page.waitForResponse((response) => response.url().endsWith(batchPath));
	release.resolve();
	await (await response).finished();
	await expect(result(page)).toHaveCount(0);
	await expect(create(page)).toHaveCount(0);
	expect(writes).toEqual([batchPath]);
});

for (const boundary of ['back navigation', 'reload', 'BFCache restore', 'deletion transition']) {
	test(`clears a revealed batch on ${boundary}`, async ({ page }) => {
		const { writes } = await mock(page);
		await openBatch(page);
		await create(page).click();
		await expect(result(page)).toBeVisible();
		if (boundary === 'back navigation') {
			await ledger(page).click();
			await expect(page).toHaveURL(/\/admin\/invites$/);
			await page.goBack();
		} else if (boundary === 'reload') await page.reload();
		else if (boundary === 'BFCache restore') await page.evaluate(() => {
			dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true }));
			dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true }));
		});
		else await page.evaluate((key) => dispatchEvent(new StorageEvent('storage', { key })), 'nntune.library.auth-transition.v1');
		if (boundary === 'deletion transition') await expect(page.getByRole('alert')).toContainText('check admin access');
		else await expect(page.getByRole('button', { name: 'Create invite', exact: true })).toBeEnabled();
		await expect(result(page)).toHaveCount(0);
		expect(writes).toEqual([batchPath]);
	});
}
