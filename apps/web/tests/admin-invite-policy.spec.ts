import { expect, test, type Page, type Route } from '@playwright/test';

const path = '/admin/invite-policy';
const api = '/v1/admin/settings';
const months = 'invite_member_min_paid_months';
const quota = 'invite_member_per_window';
const expiry = 'invite_default_expiry_days';
const settings = { [months]: 9, [quota]: 17, [expiry]: 12 };
const fields = ['Required paid months (months)', 'Per-window quota (invites)', 'Default invite expiry (days)'];
const generation = 'nntune.auth.generation.v1';
const controls = (page: Page) => page.locator('main input');

async function mock(page: Page, write?: (route: Route) => Promise<void>, read?: (route: Route) => Promise<void>) {
	const writes: unknown[] = [];
	await page.route(/\/v1\/admin\/settings(?:\/[^/]+)?$/, async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		if (request.method() === 'GET' && url.pathname === api)
			return read ? read(route) : route.fulfill({ json: { settings } });
		expect(request.method()).toBe('PUT');
		expect(await request.headerValue('authorization')).toBeNull();
		const key = url.pathname.slice(api.length + 1);
		const body = request.postDataJSON();
		writes.push({ key, body });
		return write ? write(route) : route.fulfill({ json: { key, value: body.value, previous_value: settings[key as keyof typeof settings] } });
	});
	return writes;
}

for (const [name, status, payload] of [
	['refusal', 404, settings], ['server failure', 500, settings],
	['missing value', 200, { [months]: 9, [quota]: 17 }],
	['string value', 200, { ...settings, [months]: '9' }], ['invalid expiry', 200, { ...settings, [expiry]: 0 }]
] as const) test(`shows no editor after a ${name}`, async ({ page }) => {
	const writes = await mock(page, undefined, (route) => route.fulfill({ status, json: { settings: payload } }));
	await page.goto(path);
	await expect(page.getByRole('alert')).toContainText('Invite policy is unavailable');
	await expect(controls(page)).toHaveCount(0);
	expect(writes).toEqual([]);
});

test('opens from authorized windows and saves exactly one setting by keyboard at a narrow width', async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 800 });
	await page.route('**/v1/admin/invite-windows', (route) => route.fulfill({ json: { windows: [] } }));
	const pending = Promise.withResolvers<void>();
	const writes = await mock(page, async (route) => {
		await pending.promise;
		await route.fulfill({ json: { key: months, value: 10, previous_value: 9 } });
	});
	await page.goto('/admin/invite-windows');
	await page.getByRole('link', { name: 'Invite policy', exact: true }).press('Enter');
	await expect(page).toHaveURL(path);
	await expect(page.getByRole('form')).toHaveCount(3);
	for (const [index, [value, saveLabel]] of [
		['9', 'Save required paid months'], ['17', 'Save per-window quota'], ['12', 'Save default invite expiry']
	].entries()) {
		const form = page.getByRole('form', { name: fields[index], exact: true });
		await expect(form.getByRole('textbox', { name: fields[index], exact: true })).toHaveValue(value);
		await expect(form.getByRole('button', { name: saveLabel, exact: true })).toHaveText('Save');
	}
	expect(await page.locator('main').evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
	const monthInput = page.getByRole('textbox', { name: fields[0], exact: true });
	const quotaInput = page.getByRole('textbox', { name: fields[1], exact: true });
	await quotaInput.fill('24');
	await monthInput.fill('10');
	await monthInput.press('Tab');
	const save = page.getByRole('button', { name: 'Save required paid months' });
	await expect(save).toBeFocused();
	await save.press('Enter');
	await expect(save).toBeDisabled();
	await expect(page.getByText('Saving…', { exact: true })).toBeVisible();
	await expect(page.getByText(/^Saved /)).toHaveCount(0);
	await monthInput.evaluate((el) => el.closest('form')!.requestSubmit());
	pending.resolve();
	await expect(page.getByText('Saved 10 months.', { exact: true })).toBeVisible();
	await expect(monthInput).toHaveValue('10');
	await expect(quotaInput).toHaveValue('24');
	expect(writes).toEqual([{ key: months, body: { value: 10 } }]);
});

test('ignores an unchanged submission and still allows a later edit', async ({ page }) => {
	const writes = await mock(page);
	await page.goto(path);
	const input = page.getByRole('textbox', { name: fields[0], exact: true });
	await expect(input).toHaveValue('9');
	await input.evaluate((el) => el.closest('form')!.requestSubmit());
	await expect(page.getByText(/^Saved /)).toHaveCount(0);
	await input.fill('10');
	await input.press('Enter');
	await expect(page.getByText('Saved 10 months.')).toBeVisible();
	expect(writes).toEqual([{ key: months, body: { value: 10 } }]);
});

test('rejects invalid input and accepts both inclusive endpoints for all settings', async ({ page }) => {
	const writes = await mock(page);
	await page.goto(path);
	const endpoints = [[months, 0, 600], [quota, 0, 10_000], [expiry, 1, 30]] as const;
	for (const [index, [key, min, max]] of endpoints.entries()) {
		const input = page.getByRole('textbox', { name: fields[index], exact: true });
		for (const invalid of ['', '1.5', '1e1', 'abc', String(min - 1), String(max + 1)]) {
			await input.fill(invalid);
			await input.press('Enter');
			await expect(page.getByRole('alert')).toContainText(`whole number from ${min} to ${max}`);
		}
		for (const value of [min, max]) {
			await input.fill(String(value));
			await input.press('Enter');
			await expect(input.locator('..').locator('..').getByRole('status')).toContainText(`Saved ${value} `);
		}
		expect(writes.slice(index * 2)).toEqual([min, max].map((value) => ({ key, body: { value } })));
	}
});

for (const [name, status, code, message, key = months, value = 4] of [
	['recent session required', 401, 'reauth_required', 'Sign out and sign in again'],
	['admin refusal', 404, 'not_found', 'Invite policy is unavailable'],
	['validation failure', 422, 'validation_failed', 'The server rejected this value'],
	['network failure', 0, '', 'Reload and check the current value'],
	['wrong key', 200, '', 'Reload and check the current value', quota],
	['wrong value', 200, '', 'Reload and check the current value', months, 5]
] as const) test(`does not retry or report success after ${name}`, async ({ page }) => {
	const writes = await mock(page, (route) => status === 0 ? route.abort() : route.fulfill({ status, json: { code, key, value } }));
	await page.goto(path);
	const input = page.getByRole('textbox', { name: fields[0], exact: true });
	await input.fill('4');
	await input.press('Enter');
	await expect(page.getByRole('alert')).toContainText(message);
	await expect(page.getByText(/^Saved /)).toHaveCount(0);
	if (status === 422) await expect(input).toHaveValue('4');
	else await expect(controls(page)).toHaveCount(0);
	expect(writes).toEqual([{ key: months, body: { value: 4 } }]);
});

test('does not dispatch a write using an old generation, even before the storage event', async ({ page }) => {
	const writes = await mock(page);
	await page.goto(path);
	const input = page.getByRole('textbox', { name: fields[0], exact: true });
	await input.fill('4');
	await input.evaluate((el, key) => {
		localStorage.setItem(key, 'different fixture generation');
		el.closest('form')!.requestSubmit();
	}, generation);
	await expect(page.getByRole('alert')).toContainText('Invite policy is unavailable');
	await expect(controls(page)).toHaveCount(0);
	expect(writes).toEqual([]);
});

test('removes the editor and unsaved drafts on a delivered auth transition', async ({ page }) => {
	const writes = await mock(page);
	await page.goto(path);
	await page.getByRole('textbox', { name: fields[0], exact: true }).fill('4');
	await page.evaluate((key) => window.dispatchEvent(new StorageEvent('storage', { key, storageArea: localStorage })), generation);
	await expect(controls(page)).toHaveCount(0);
	await expect(page.getByRole('heading', { name: 'Invite policy' })).toBeFocused();
	expect(writes).toEqual([]);
});

for (const method of ['GET', 'PUT']) test(`ignores late ${method} data across an auth change`, async ({ page }) => {
	await mock(page);
	await page.addInitScript(({ method, api, months, settings }) => {
		const fetchFixture = window.fetch;
		window.fetch = (input, init) => {
			if (new URL(String(input), location.href).pathname !== (method === 'GET' ? api : `${api}/${months}`) || init?.method !== method)
				return fetchFixture(input, init);
			return new Promise((resolve) => {
				document.documentElement.dataset.policyFixture = 'held';
				window.addEventListener('release-policy-fixture', () => resolve(new Response(JSON.stringify(
					method === 'GET' ? { settings } : { key: months, value: 4, previous_value: 9 }
				))), { once: true });
			});
		};
	}, { method, api, months, settings });
	await page.goto(path);
	if (method === 'PUT') {
		const input = page.getByRole('textbox', { name: fields[0], exact: true });
		await input.fill('4');
		await input.press('Enter');
	}
	await expect(page.locator('html')).toHaveAttribute('data-policy-fixture', 'held');
	await page.evaluate((key) => {
		// Same-tab mutation does not emit storage: publication itself must recheck the generation.
		localStorage.setItem(key, 'different fixture generation');
		window.dispatchEvent(new Event('release-policy-fixture'));
	}, generation);
	await expect(page.getByRole('alert')).toContainText('Invite policy is unavailable');
	await expect(controls(page)).toHaveCount(0);
	await expect(page.getByText(/^Saved /)).toHaveCount(0);
	if (method === 'PUT') await expect(page.getByRole('heading', { name: 'Invite policy' })).toBeFocused();
});
