import { expect, test, type Page } from '@playwright/test';

const path = '/admin/invite-windows';
const api = `**/v1${path}`;
const start = '2099-01-01T12:00';
const end = '2099-01-01T13:00';
const future = { id: 201, starts_at: Date.parse(`${start}Z`) / 1000, ends_at: Date.parse(`${end}Z`) / 1000, note: 'Synthetic window' };
const schedule = (page: Page) => page.getByRole('button', { name: 'Schedule window' });
test.use({ locale: 'en-US', timezoneId: 'America/Los_Angeles' });
async function fill(page: Page, stop = end) {
	await page.getByLabel('Start (UTC)').fill(start);
	await page.getByLabel('End (UTC)').fill(stop);
}

for (const status of [404, 500, 200]) test(`does not expose data or controls after an unusable GET (${status})`, async ({ page }) => {
	await page.route(api, (route) => route.fulfill({ status, json: { windows: status === 200 ? [future, { id: 1 }] : [future] } }));
	await page.goto(path);
	await expect(page.getByRole('alert')).toContainText('Invite windows are unavailable');
	await expect(schedule(page)).toHaveCount(0);
	await expect(page.getByRole('link', { name: 'Invite policy' })).toHaveCount(0);
	await expect(page.getByRole('link', { name: 'Invite ledger' })).toHaveCount(0);
	await expect(page.getByText(future.note)).toHaveCount(0);
});

test('schedules once after an authorized empty read and renders the server result in UTC', async ({ page }) => {
	const read = Promise.withResolvers<void>();
	const pending = Promise.withResolvers<void>();
	let posts = 0;
	await page.route(api, async (route) => {
		if (route.request().method() === 'GET') {
			await read.promise;
			return route.fulfill({ json: { windows: [] } });
		}
		posts++;
		expect(route.request().postDataJSON()).toEqual({ starts_at: future.starts_at, ends_at: future.ends_at, note: future.note });
		expect(await route.request().headerValue('authorization')).toBeNull();
		await pending.promise;
		await route.fulfill({ status: 201, json: { window: { ...future, note: 'Authoritative note' } } });
	});
	await page.goto(path);
	await expect(page.getByText('Checking access…')).toBeVisible();
	await expect(schedule(page)).toHaveCount(0);
	read.resolve();
	await expect(page.getByText('No invite windows yet.')).toBeVisible();
	await fill(page, start);
	await schedule(page).click();
	await expect(page.getByRole('alert')).toContainText('end must be after the start');
	expect(posts).toBe(0);
	await fill(page);
	await page.getByLabel('Note (optional)').fill(future.note);
	await schedule(page).click();
	await expect(schedule(page)).toBeDisabled();
	await schedule(page).press('Enter');
	pending.resolve();
	await expect(page.getByText('Window scheduled.')).toBeVisible();
	await expect(page.getByText('Authoritative note')).toBeVisible();
	await expect(page.locator('time').first()).toHaveText(/12:00:00\s+PM UTC$/);
	await expect(page.locator('time').first()).toHaveAttribute('datetime', `${start}:00.000Z`);
	expect(posts).toBe(1);
});

for (const [status, code, message] of [
	[409, 'conflict', 'overlaps another invite window'],
	[401, 'reauth_required', 'Sign out and sign in again'],
	[0, '', 'Reload and check the list before trying again']
] as const) test(`does not retry or claim success after schedule refusal ${status}`, async ({ page }) => {
	let posts = 0;
	await page.route(api, (route) => {
		if (route.request().method() === 'GET') return route.fulfill({ json: { windows: [] } });
		posts++;
		expect(route.request().postDataJSON().note).toBeNull();
		return status ? route.fulfill({ status, json: { code } }) : route.abort();
	});
	await page.goto(path);
	await fill(page);
	await schedule(page).click();
	await expect(page.getByRole('alert')).toContainText(message);
	await expect(page.getByText('Window scheduled.')).toHaveCount(0);
	expect(posts).toBe(1);
});

for (const outcome of ['close', 'cancel', 'missing']) test(`confirms the right row on mobile and handles ${outcome}`, async ({ page }) => {
	await page.setViewportSize({ width: 375, height: 812 });
	const at = future.starts_at - 3600;
	await page.clock.setFixedTime(at * 1000);
	const active = { ...future, id: 200, starts_at: at - 60, ends_at: at + 600 };
	const windows = [future, active, ...Array.from({ length: 198 }, (_, i) => ({
		...active, id: 199 - i, ends_at: active.starts_at, note: '界'.repeat(170)
	}))];
	const pending = Promise.withResolvers<void>();
	const target = outcome === 'close' ? active : future;
	let posts = 0;
	let serverTime = at;
	await page.route(api, (route) => route.fulfill({ headers: { date: new Date(serverTime * 1000).toUTCString() }, json: { windows } }));
	await page.route(`${api}/*/close`, async (route) => {
		posts++;
		expect(new URL(route.request().url()).pathname).toBe(`/v1${path}/${target.id}/close`);
		expect(route.request().method()).toBe('POST');
		await pending.promise;
		if (outcome === 'missing') return route.fulfill({ status: 404, json: {} });
		const row = { ...target, ends_at: outcome === 'close' ? (serverTime = at + 20) : target.starts_at };
		windows[windows.indexOf(target)] = row;
		return route.fulfill({ json: { window: row } });
	});
	await page.goto(path);
	await expect(page.locator('main li')).toHaveCount(200);
	expect(await page.locator('main').evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
	const button = page.getByRole('button', { name: outcome === 'close' ? 'Close window #200' : 'Cancel window #201' });
	for (const accept of [false, true]) {
		page.once('dialog', async (dialog) => {
			expect(dialog.message()).toContain('Unused member invites in this window will stop working.');
			await (accept ? dialog.accept() : dialog.dismiss());
		});
		await button.press('Enter');
		if (!accept) expect(posts).toBe(0);
	}
	await expect(button).toBeDisabled();
	await button.evaluate((el: HTMLButtonElement) => { window.confirm = () => true; el.click(); });
	pending.resolve();
	await expect(button).toHaveCount(0);
	if (outcome === 'missing') {
		await expect(page.getByRole('alert')).toContainText('This window is unavailable');
		await expect(page.getByRole('heading', { name: 'Invite windows', exact: true })).toBeFocused();
	} else {
		const row = page.getByRole('listitem', { name: `Window #${target.id} · ${outcome === 'close' ? 'Ended' : 'Canceled'}` });
		await expect(row).toBeFocused();
		await page.reload();
		await expect(row).toBeVisible();
		await expect(button).toHaveCount(0);
	}
	expect(posts).toBe(1);
});

test('clears former-session data and ignores a write response even if its transport ignores abort', async ({ page }) => {
	await page.route(api, (route) => route.fulfill({ json: { windows: [future] } }));
	await page.goto(path);
	await fill(page);
	await page.evaluate(({ future, path }) => {
		const original = window.fetch;
		window.fetch = (input, init) => String(input).endsWith(`/v1${path}`) ? new Promise((resolve) => {
			document.documentElement.dataset.windowFixture = 'held';
			window.addEventListener('release-window-fixture', () => resolve(new Response(JSON.stringify({ window: future }), { status: 201 })), { once: true });
		}) : original(input, init);
	}, { future, path });
	await schedule(page).click();
	await expect(page.locator('html')).toHaveAttribute('data-window-fixture', 'held');
	await page.evaluate(() => {
		const key = 'nntune.auth.generation.v1';
		localStorage.setItem(key, 'replacement-session');
		window.dispatchEvent(new StorageEvent('storage', { key, storageArea: localStorage }));
		window.dispatchEvent(new Event('release-window-fixture'));
	});
	await expect(schedule(page)).toHaveCount(0);
	await expect(page.getByText('Window scheduled.')).toHaveCount(0);
	await expect(page.getByText(future.note)).toHaveCount(0);
});
