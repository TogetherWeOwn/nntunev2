import { expect, test, type Page } from '@playwright/test';

const code = 'synthetic_invite_codeA';
const issued = { id: '1', code, expires_at: 1_893_456_000 };

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
	await mockClipboard(page);
	const pending = Promise.withResolvers<void>();
	let posts = 0;
	await page.route('**/v1/me/invites', async (route) => {
		posts++;
		await pending.promise;
		await route.fulfill({ status: 201, json: issued });
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
	expect(posts).toBe(0);
	const sent = page.waitForRequest('**/v1/me/invites');
	await create.click();
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
	await expect(page.locator('time')).toHaveAttribute('datetime', new Date(issued.expires_at * 1000).toISOString());
	await page.getByRole('button', { name: 'Copy link' }).click();
	await expect(field).toHaveCount(0);
	await expect(page.getByText('Invite link copied.')).toBeVisible();
	await expect(create).toBeFocused();
	expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(link);
	expect(posts).toBe(1);

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

test('allows manual copy when the clipboard is blocked and clears the reveal on page exit', async ({ page }) => {
	await mockClipboard(page, true);
	await page.route('**/v1/me/invites', (route) => route.fulfill({ status: 201, json: issued }));
	await page.goto('/account/invites');
	await page.getByRole('button', { name: 'Create invite' }).click();
	await page.getByRole('button', { name: 'Copy link' }).click();
	await expect(page.getByText('Couldn’t copy automatically.', { exact: false })).toBeVisible();
	const field = page.getByRole('textbox', { name: 'Signup link' });
	await expect(field).toBeFocused();
	expect(await field.evaluate((input: HTMLInputElement) => input.selectionEnd)).toBe((await field.inputValue()).length);
	await page.getByRole('button', { name: 'Hide link' }).click();
	await expect(field).toHaveCount(0);
	await page.getByRole('button', { name: 'Create invite' }).click();
	await expect(field).toBeVisible();
	// Simulate the lifecycle event that precedes a back/forward cache snapshot.
	await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true })));
	await expect(field).toHaveCount(0);
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

for (const boundary of ['library signal', 'auth fence']) test(`${boundary} clears an invite and ignores a held success`, async ({ page }) => {
	await page.route('**/v1/me/invites', (route) => route.fulfill({ status: 201, json: issued }));
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
	await create.click();
	await expect(field).toBeVisible();
	await authTransition();
	await expect(field).toHaveCount(0);

	await page.evaluate((result) => {
		const fetch = window.fetch;
		// This transport deliberately ignores abort and delivers the 201 after the transition.
		window.fetch = (input, init) => String(input).endsWith('/v1/me/invites') ? new Promise((resolve) => {
			document.documentElement.dataset.inviteFixture = 'held';
			window.addEventListener('release-invite-fixture', () => {
					resolve(new Response(JSON.stringify(result), { status: 201 }));
			}, { once: true });
		}) : fetch(input, init);
	}, issued);
	await create.click();
	await expect(page.locator('html')).toHaveAttribute('data-invite-fixture', 'held');
	await authTransition();
	await expect(create).toBeEnabled();
	await page.evaluate(() => {
		window.dispatchEvent(new Event('release-invite-fixture'));
		return new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
	});
	await expect(page).toHaveURL(/\/account\/invites$/);
	await expect(field).toHaveCount(0);
	await expect(page.getByRole('alert')).toHaveCount(0);
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
		await page.route('**/v1/me/invites', (route) => {
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
