import { expect, test, type Page, type Route } from '@playwright/test';

const path = '/admin/invites/issue';
const api = '**/v1/admin/invites*';
const code = 'synthetic_invite_codeA';
const generation = 'nntune.auth.generation.v1';
const issued = { code, invite: { id: 17, kind: 'admin', status: 'active', expires_at: 1_893_456_000 } };
const create = (page: Page) => page.getByRole('button', { name: 'Create invite', exact: true });
const link = (page: Page) => page.getByRole('textbox', { name: 'Signup link' });
const success = (route: Route) => route.fulfill({ status: 201, json: issued });

async function mock(page: Page, write = success, read = (route: Route) => route.fulfill({ json: { invites: [] } })) {
	const posts: unknown[] = [];
	await page.route(api, async (route) => {
		const request = route.request();
		expect(new URL(request.url()).search).toBe('');
		expect(await request.headerValue('authorization')).toBeNull();
		if (request.method() === 'GET') return read(route);
		expect(request.method()).toBe('POST');
		posts.push(request.postDataJSON());
		await write(route);
	});
	return posts;
}

test('issues once with server defaults and copies a fragment link that works at signup', async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 800 });
	await page.addInitScript(() => {
		let copied = '';
		Object.defineProperty(navigator, 'clipboard', { value: {
			writeText: async (value: string) => { copied = value; }, readText: async () => copied
		} });
	});
	const release = Promise.withResolvers<void>();
	const posts = await mock(page, async (route) => { await release.promise; await success(route); });
	await page.goto('/admin/invites');
	await page.getByRole('link', { name: 'Create an invite' }).press('Enter');
	await expect(create(page)).toBeEnabled();
	expect(posts).toEqual([]);
	await create(page).press('Enter');
	await expect(page.getByRole('button', { name: 'Creating invite…' })).toBeDisabled();
	await page.locator('main form').evaluate((form: HTMLFormElement) => form.requestSubmit());
	await expect.poll(() => posts).toEqual([{}]);
	release.resolve();
	const expected = new URL(`/signup#invite=${code}`, page.url());
	await expect(link(page)).toHaveValue(expected.href);
	await expect(link(page)).toBeFocused();
	await expect(page.locator('time')).toHaveAttribute('datetime', new Date(issued.invite.expires_at * 1000).toISOString());
	expect(await page.locator('main').evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
	await page.getByRole('button', { name: 'Copy link' }).press('Enter');
	await expect(link(page)).toHaveCount(0);
	await expect(page.getByText('Invite link copied.')).toBeVisible();
	await expect(create(page)).toBeFocused();
	const copied = await page.evaluate(() => navigator.clipboard.readText());
	expect(copied).toBe(expected.href);
	expect(await page.evaluate(() => JSON.stringify([location.href, history.state, localStorage, sessionStorage]))).not.toContain(code);
	expect(posts).toEqual([{}]);
	await page.goto(copied);
	await expect(page).toHaveURL(/\/signup$/);
	await expect(page.getByText('Invite link added.', { exact: false })).toBeVisible();
});

test('validates fields, sends only explicit options, and hides the one-time result', async ({ page }) => {
	await page.setViewportSize({ width: 320, height: 800 });
	await page.addInitScript(() => Object.defineProperty(navigator, 'clipboard', { value: undefined }));
	const posts = await mock(page);
	await page.goto(path);
	await expect(create(page)).toBeEnabled();
	const email = page.getByRole('textbox', { name: 'Email (optional)' });
	const expiry = page.getByRole('spinbutton', { name: 'Expiry in days (optional)' });
	await email.fill('friend@example');
	await create(page).press('Enter');
	await expect(page.getByRole('alert')).toContainText('Enter a full email address');
	await email.fill('friend@example.test');
	await expiry.fill('31');
	await create(page).press('Enter');
	await expect(expiry).toBeFocused();
	expect(posts).toEqual([]);
	await expiry.fill('30');
	await create(page).press('Enter');
	await expect(link(page)).toBeVisible();
	expect(posts).toEqual([{ email: 'friend@example.test', expires_in_days: 30 }]);
	await page.getByRole('button', { name: 'Copy link' }).press('Enter');
	await expect(page.getByRole('alert')).toContainText('Couldn’t copy automatically');
	await expect(link(page)).toBeFocused();
	await page.getByRole('button', { name: 'Hide link' }).press('Enter');
	await expect(link(page)).toHaveCount(0);
	await expect(create(page)).toBeFocused();
});

test('does not show issuance controls without an authorized read', async ({ page }) => {
	const posts = await mock(page, success, (route) => route.fulfill({ status: 404 }));
	await page.goto(path);
	await expect(page.getByRole('alert')).toContainText('check admin access');
	await expect(create(page)).toHaveCount(0);
	expect(posts).toEqual([]);
});

test('clears a revealed link on pagehide and restores access with a read only', async ({ page }) => {
	let reads = 0;
	const posts = await mock(page, success, (route) => { reads++; return route.fulfill({ json: { invites: [] } }); });
	await page.goto(path);
	await create(page).click();
	await expect(link(page)).toBeVisible();
	await page.evaluate(() => dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true })));
	await expect(link(page)).toHaveCount(0);
	await expect(create(page)).toHaveCount(0);
	await page.evaluate(() => dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
	await expect(create(page)).toBeEnabled();
	expect(reads).toBe(2);
	expect(posts).toEqual([{}]);
});

for (const phase of ['before dispatch', 'before response', 'after reveal']) test(`discards authorization changed ${phase}`, async ({ page }) => {
	const release = Promise.withResolvers<void>();
	const posts = await mock(page, async (route) => { await release.promise; await success(route); });
	await page.goto(path);
	await expect(create(page)).toBeEnabled();
	if (phase !== 'before dispatch') {
		await create(page).click();
		await expect.poll(() => posts).toEqual([{}]);
	}
	if (phase === 'after reveal') {
		release.resolve();
		await expect(link(page)).toBeVisible();
	}
	await page.evaluate(({ generation, phase }) => {
		localStorage.setItem(generation, 'fixture-new-generation');
		if (phase === 'after reveal') dispatchEvent(new StorageEvent('storage', { key: generation }));
	}, { generation, phase });
	if (phase === 'before dispatch') await create(page).click();
	release.resolve();
	await expect(page.getByRole('alert')).toContainText(phase === 'before response' ? 'An invite may have been created' : 'check admin access');
	await expect(create(page)).toHaveCount(0);
	await expect(link(page)).toHaveCount(0);
	expect(posts).toEqual(phase === 'before dispatch' ? [] : [{}]);
});

for (const failure of ['network failure', 'malformed success', 'validation rejection']) test(`reports ${failure} without retrying`, async ({ page }) => {
	const rejected = failure === 'validation rejection';
	const posts = await mock(page, (route) => failure === 'network failure' ? route.abort('failed') :
		route.fulfill({ status: rejected ? 422 : 201, json: { code } }));
	await page.goto(path);
	await create(page).click();
	await expect(page.getByRole('alert')).toContainText(rejected ? 'The server rejected the invite' : 'An invite may have been created');
	await expect(link(page)).toHaveCount(0);
	await expect(create(page)).toHaveCount(rejected ? 1 : 0);
	expect(posts).toEqual([{}]);
});
