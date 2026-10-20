import { expect, test, type Locator, type Page, type Route } from '@playwright/test';

// Supplemental regressions; the protected acceptance handoff stays unchanged.
// Only API/credential boundaries are mocked. All credentials are synthetic.
const PASSWORD = 'fixture-password-never-announce';
const API_KEY = 'fixture-api-key-never-announce';
const LIVE = '[role="status"], [role="alert"], [aria-live="polite"], [aria-live="assertive"]';

function deferred() {
	let release!: () => void;
	const promise = new Promise<void>((resolve) => { release = resolve; });
	return { promise, release };
}

async function json(route: Route, status: number, body: unknown) {
	await route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
}

async function mockNetwork(page: Page, options: {
	expireProvider?: boolean;
	requirePasskey?: boolean;
	finishGate?: ReturnType<typeof deferred>;
	finishStatus?: number;
	saveGate?: ReturnType<typeof deferred>;
} = {}) {
	const traffic = { tests: 0, saves: 0, starts: 0, finishes: 0 };
	await page.route('**/*', async (route) => {
		const url = new URL(route.request().url());
		if (route.request().method() === 'PUT' && url.pathname === '/v1/vault') {
			traffic.saves += 1;
			if (options.requirePasskey && traffic.saves === 1) {
				return json(route, 401, { code: 'reauth_required', message: 'reauth required', retryable: false });
			}
			await options.saveGate?.promise;
			return json(route, 200, { ok: true });
		}
		if (route.request().method() === 'POST') {
			if (url.pathname === '/v1/vault/test') {
				traffic.tests += 1;
				return options.expireProvider
					? json(route, 401, { code: 'provider_auth_expired', message: 'provider credentials expired', retryable: false })
					: json(route, 200, { ok: true, latencyMs: 12 });
			}
			if (options.requirePasskey && url.pathname === '/v1/session/reauthenticate/start') {
				traffic.starts += 1;
				return json(route, 200, { challenge: 'Y2hhbGxlbmdl', rp_id: 'localhost' });
			}
			if (options.requirePasskey && url.pathname === '/v1/session/reauthenticate/finish') {
				traffic.finishes += 1;
				await options.finishGate?.promise;
				return json(route, options.finishStatus ?? 200,
					options.finishStatus === 403 ? { error: 'fixture rejected' } : { ok: true });
			}
		}
		// Never contact an unmocked API or a non-local origin.
		if (url.pathname.startsWith('/v1/') || !['localhost', '127.0.0.1'].includes(url.hostname)) {
			return route.abort('blockedbyclient');
		}
		return route.continue();
	});
	return traffic;
}

async function provider(page: Page) {
	await page.goto('/onboarding/byop');
	await page.getByTestId('byop-host').fill('news.example.test');
	await page.getByTestId('byop-user').fill('fixture-reader');
	await page.getByTestId('byop-pass').fill(PASSWORD);
}

async function testStep(page: Page) {
	await provider(page);
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByLabel(/^Indexer URL/).fill('https://index.example.test/api');
	await page.getByLabel(/^Indexer API key/).fill(API_KEY);
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await expect(page.getByTestId('byop-test-connection')).toBeVisible();
}

async function errorRelationship(page: Page, control: Locator, text: RegExp) {
	await expect(control).toBeFocused();
	await expect(control).toHaveAttribute('aria-invalid', 'true');
	const id = await control.getAttribute('id');
	expect(id).toBeTruthy();
	await expect(page.locator(`[id=${JSON.stringify(id)}]`)).toHaveCount(1);
	const descriptions = (await control.getAttribute('aria-describedby') ?? '').trim().split(/\s+/).filter(Boolean);
	expect(descriptions.length).toBeGreaterThan(0);
	for (const description of descriptions) {
		const error = page.locator(`[id=${JSON.stringify(description)}]`);
		await expect(error).toHaveCount(1);
		await expect(error).toBeVisible();
		await expect(error).toContainText(text);
	}
	await expect(control).toHaveAccessibleDescription(text);
	const link = page.locator(`a[href="#${id}"]`);
	await expect(link).toHaveCount(1);
	await link.focus();
	await page.keyboard.press('Enter');
	await expect(control).toBeFocused();
	return descriptions;
}

async function expectCleared(page: Page, control: Locator, descriptions: string[]) {
	await expect(control).not.toHaveAttribute('aria-invalid', 'true');
	const remaining = (await control.getAttribute('aria-describedby') ?? '').split(/\s+/);
	for (const id of descriptions) {
		expect(remaining).not.toContain(id);
		await expect(page.locator(`[id=${JSON.stringify(id)}]`)).toHaveCount(0);
	}
	await expect(control).toBeFocused();
}

for (const field of ['host', 'indexer URL', 'indexer key'] as const) {
	test(`${field} correction clears its error before submitting or leaving the step`, async ({ page }) => {
		const traffic = await mockNetwork(page);
		await provider(page);
		let control: Locator;
		let corrected: string;
		let message: RegExp;
		if (field === 'host') {
			control = page.getByTestId('byop-host');
			await control.fill('https://news.example.test/');
			// Keep a second error to prove correction does not clear unrelated errors.
			await page.getByTestId('byop-user').fill('   ');
			corrected = 'news.example.test';
			message = /hostname.*(?:scheme|path)/i;
		} else {
			await page.getByRole('button', { name: 'Continue', exact: true }).click();
			const url = page.getByLabel(/^Indexer URL/);
			const key = page.getByLabel(/^Indexer API key/);
			await url.fill(field === 'indexer URL' ? 'http://index.example.test/api' : 'https://index.example.test/api');
			if (field === 'indexer URL') await key.fill(API_KEY);
			control = field === 'indexer URL' ? url : key;
			corrected = field === 'indexer URL' ? 'https://index.example.test/api' : API_KEY;
			message = field === 'indexer URL' ? /https/i : /api key/i;
		}
		await page.getByRole('button', { name: 'Continue', exact: true }).click();
		const descriptions = await errorRelationship(page, control, message);
		await control.fill(corrected);
		// No Continue/Back: those handlers reset errors and can mask stale typing state.
		await expectCleared(page, control, descriptions);
		await expect(page.getByRole('heading', { level: 2 })).toHaveText(field === 'host' ? 'Provider details' : 'Indexer (optional)');
		if (field === 'host') {
			await expect(page.getByTestId('byop-user')).toHaveAttribute('aria-invalid', 'true');
			await expect(page.getByTestId('byop-user')).toHaveAccessibleDescription(/provider username/i);
		}
		expect(traffic).toEqual({ tests: 0, saves: 0, starts: 0, finishes: 0 });
	});
}

for (const field of ['username', 'password', 'TLS acknowledgement', 'connections'] as const) {
	test(`${field} custom error has first-invalid focus and an accessible description`, async ({ page }) => {
		const traffic = await mockNetwork(page);
		await provider(page);
		let control: Locator;
		let message: RegExp;
		if (field === 'username') {
			control = page.getByTestId('byop-user');
			await control.fill('   ');
			message = /enter your provider username/i;
		} else if (field === 'password') {
			control = page.getByTestId('byop-pass');
			await control.fill('é'.repeat(513));
			message = /password.*1024 UTF-8 bytes/i;
		} else if (field === 'TLS acknowledgement') {
			await page.getByTestId('byop-port').selectOption('119');
			await page.getByTestId('byop-tls').uncheck();
			control = page.getByRole('checkbox', { name: /I understand my password/i });
			message = /tick the acknowledgement/i;
		} else {
			control = page.getByLabel('Connections', { exact: true });
			await control.fill('');
			message = /connections must be between/i;
		}
		// Whitespace, UTF-8 overflow and an empty optional number bypass no native
		// constraint: the real submit must reach the custom validator, not a bubble.
		expect(await control.evaluate((element: HTMLInputElement) => element.validity.valid)).toBe(true);
		await page.getByRole('button', { name: 'Continue', exact: true }).click();
		await errorRelationship(page, control, message);
		await expect(page.getByRole('heading', { level: 2 })).toHaveText('Provider details');
		expect(traffic).toEqual({ tests: 0, saves: 0, starts: 0, finishes: 0 });
	});
}

test('empty expired-provider retry returns to the mounted password error on step one', async ({ page }) => {
	const traffic = await mockNetwork(page, { expireProvider: true });
	await testStep(page);
	await page.getByTestId('byop-test-connection').click();
	const retryPassword = page.getByLabel('Password', { exact: true });
	await expect(retryPassword).toBeFocused();
	await retryPassword.fill('');
	await page.getByRole('button', { name: 'Test again', exact: true }).click();
	await expect(page.getByRole('heading', { level: 2 })).toHaveText('Provider details');
	await errorRelationship(page, page.getByTestId('byop-pass'), /enter your provider password/i);
	await expect(page.getByRole('button', { name: 'Test again', exact: true })).toHaveCount(0);
	expect(traffic).toEqual({ tests: 1, saves: 0, starts: 0, finishes: 0 });
});

async function installCredentialDouble(page: Page) {
	await page.addInitScript(() => {
		class FixturePublicKeyCredential {
			id = 'fixture-credential';
			rawId = new Uint8Array([1]).buffer;
			type = 'public-key';
			authenticatorAttachment = 'platform';
			response = {
				clientDataJSON: new Uint8Array([2]).buffer,
				authenticatorData: new Uint8Array([3]).buffer,
				signature: new Uint8Array([4]).buffer,
				userHandle: null
			};
			getClientExtensionResults() { return {}; }
		}
		Object.defineProperty(window, 'PublicKeyCredential', { value: FixturePublicKeyCredential });
		Object.defineProperty(navigator, 'credentials', {
			value: { get: async () => new FixturePublicKeyCredential() }
		});
	});
}

for (const outcome of ['success', 'failure'] as const) {
	test(`passkey progress updates the mounted status and clears on ${outcome}`, async ({ page }) => {
		const finishGate = deferred();
		const saveGate = deferred();
		await installCredentialDouble(page);
		const traffic = await mockNetwork(page, {
			requirePasskey: true, finishGate, saveGate, finishStatus: outcome === 'failure' ? 403 : 200
		});
		try {
			await testStep(page);
			await page.getByTestId('byop-test-connection').click();
			await page.getByTestId('byop-save').click();
			const reauth = page.getByRole('button', { name: 'Reauthenticate and save', exact: true });
			await expect(reauth).toBeVisible();
			const idleRegions = await page.getByRole('status').elementHandles();
			await reauth.click();
			await expect.poll(() => traffic.finishes).toBe(1);
			const progress = page.getByRole('status').filter({ hasText: 'Verifying your passkey…' });
			await expect(progress).toHaveCount(1);
			await expect(progress).toBeVisible();
			expect(await progress.evaluate((element, candidates) => candidates.includes(element), idleRegions)).toBe(true);
			await expect(progress).toHaveAttribute('aria-atomic', 'true');
			await expect(page.locator('section[aria-busy="true"]')).toHaveCount(1);
			await expect(page.getByRole('button', { name: 'Verifying…', exact: true })).toBeDisabled();
			const status = (await progress.elementHandle())!;
			const liveText = await page.locator(LIVE).allTextContents();
			expect(liveText.join('\n')).not.toContain(PASSWORD);
			expect(liveText.join('\n')).not.toContain(API_KEY);
			finishGate.release();
			if (outcome === 'failure') {
				await expect(reauth).toBeEnabled();
				await expect(page.getByText('Reauthentication failed.', { exact: false })).toBeVisible();
				expect(traffic.saves).toBe(1);
			} else {
				await expect.poll(() => traffic.saves).toBe(2);
				await expect.poll(() => status.textContent()).toContain('Saving provider details…');
				await expect(page.getByTestId('byop-save')).toBeDisabled();
			}
			expect(await status.evaluate((element) => element.isConnected)).toBe(true);
			await expect.poll(() => status.textContent()).not.toContain('Verifying your passkey');
			if (outcome === 'failure') {
				await expect.poll(() => status.textContent()).toMatch(/^\s*$/);
				await expect(page.locator('[aria-busy="true"]')).toHaveCount(0);
			} else {
				saveGate.release();
				await page.waitForURL('**/library');
			}
			expect(traffic).toEqual({ tests: 1, saves: outcome === 'failure' ? 1 : 2, starts: 1, finishes: 1 });
		} finally {
			finishGate.release();
			saveGate.release();
		}
	});
}
