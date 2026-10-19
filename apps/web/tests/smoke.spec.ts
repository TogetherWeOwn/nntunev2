import { expect, test, type Page } from '@playwright/test';

/** Ensure passkeys read as supported so signup reaches its API branches. */
async function ensurePasskeysSupported(page: Page): Promise<void> {
	await page.addInitScript(() => {
		if (typeof window.PublicKeyCredential === 'undefined') {
			Object.defineProperty(window, 'PublicKeyCredential', { value: class {} });
		}
	});
}

test('landing explains BYOP and links to signup and pricing', async ({ page, request }) => {
	await page.goto('/');

	await expect(page.getByRole('heading', { level: 1 })).toContainText('NNTune');
	// \s+ because SSR may wrap the copy across lines.
	await expect(page.getByRole('main')).toContainText(/bring your\s+own/i);
	await expect(page.getByRole('link', { name: /get started/i })).toHaveAttribute('href', '/signup');
	await expect(page.getByRole('link', { name: /plans/i }).first()).toHaveAttribute(
		'href',
		'/pricing'
	);
	// e2e contract hook: the app-shell smoke accepts this hook
	// when present and otherwise falls back to the NNTune heading.
	await expect(page.getByTestId('app-root')).toBeVisible();

	// Every landing CTA must resolve to a real route (no 404 placeholders).
	for (const href of ['/signup', '/pricing']) {
		const response = await request.get(href);
		expect(response.status(), `${href} should not 404`).not.toBe(404);
		expect(response.ok()).toBe(true);
	}
});

test('health endpoint reports ok with the package version and nothing else', async ({
	request
}) => {
	const response = await request.get('/api/health');

	expect(response.ok()).toBe(true);
	const body = (await response.json()) as Record<string, unknown>;
	// Exactly these keys: version ships the public package version, and no
	// internal identifiers (commit SHAs, hostnames, bindings) may leak in.
	expect(Object.keys(body).sort()).toEqual(['ok', 'version']);
	expect(body.ok).toBe(true);
	expect(typeof body.version).toBe('string');
	expect((body.version as string).length).toBeGreaterThan(0);
});

test('readiness endpoint reports ok when ready', async ({ request }) => {
	const response = await request.get('/api/ready');

	expect(response.status()).toBe(200);
	await expect(response.json()).resolves.toEqual({ ok: true });
});

/**
 * E2E contract-hook coverage (TESTIDS in e2e/helpers/contract.ts, 20 hooks).
 * The static companion (apps/web/src/lib/contract.test.ts) pins that each
 * hook string appears in its route markup; these specs pin that the app
 * actually renders it, seeding the states that reveal the conditional ones.
 * All API states below are Playwright route mocks — no live provider call.
 *
 * Always-rendered (plain navigation): app-root, signup-email, signup-submit,
 * byop-host, byop-port, byop-user, byop-pass, byop-tls, usage-meter,
 * import-file, import-status.
 *
 * Conditional (seeded states): signup-error (mocked 500 register/start),
 * byop-test-connection (wizard advanced to step 3), byop-save + byop-status
 * (mocked vault/test ok, plus the failed variant for byop-status — the
 * reauth prompt shares the same hook), library-list + library-item (mocked
 * /api/library items), library-stale-badge (mocked success then a failed
 * refresh), plan-limit-notice (mocked 402 paywall — the PaywallModal
 * instance; the library inline instance is gated behind the overLimit=false
 * placeholder and cannot render yet).
 *
 * GAP — the app cannot render this hook yet, so no render pin is possible
 * without an app change (this spec is test-only, no app source touched):
 * audio-player — library/+page.svelte renders it only when currentTrack is
 * set, and that placeholder is always null on main.
 */
test('contract routes resolve and render their e2e hooks', async ({ page, request }) => {
	for (const href of ['/signup', '/onboarding/byop', '/library', '/library/import']) {
		const response = await request.get(href);
		expect(response.status(), `${href} should not 404`).not.toBe(404);
		expect(response.ok()).toBe(true);
	}

	await page.goto('/signup');
	await expect(page.getByTestId('signup-email')).toBeVisible();
	await expect(page.getByTestId('signup-submit')).toBeVisible();

	await page.goto('/onboarding/byop');
	for (const testid of [
		'byop-host',
		'byop-port',
		'byop-user',
		'byop-pass',
		'byop-tls'
	]) {
		await expect(page.getByTestId(testid)).toBeVisible();
	}
	// Conditional step-3 hook: advance the wizard (no API call yet — the
	// test button only appears on the Test step).
	await page.locator('input[name="host"]').fill('news.example.com');
	await page.locator('input[name="username"]').fill('alice');
	await page.locator('input[name="password"]').fill('s3cret');
	await page.getByRole('button', { name: 'Continue' }).click();
	await page.getByRole('button', { name: 'Continue' }).click();
	await expect(page.getByTestId('byop-test-connection')).toBeVisible();

	await page.goto('/library');
	await expect(page.getByTestId('usage-meter')).toBeVisible();

	await page.goto('/library/import');
	const fileInput = page.getByTestId('import-file');
	await expect(fileInput).toBeAttached();
	await fileInput.setInputFiles({
		name: 'smoke.nzb',
		mimeType: 'application/x-nzb',
		buffer: Buffer.from('<?xml version="1.0"?><nzb></nzb>')
	});
	await expect(page.getByTestId('import-status')).toBeVisible();
});

test('contract signup error hook renders when account start fails', async ({ page }) => {
	await ensurePasskeysSupported(page);
	await page.route('**/v1/auth/passkey/register/start', (route) =>
		route.fulfill({
			status: 500,
			contentType: 'application/json',
			body: JSON.stringify({ error: 'fixture failure' })
		})
	);

	await page.goto('/signup');
	await page.getByTestId('signup-email').fill('alice@example.com');
	await page.getByTestId('signup-submit').click();

	await expect(page.getByTestId('signup-error')).toBeVisible();
	await expect(page.getByTestId('signup-error')).toContainText('Something went wrong');
	// A 500 is not a plan limit: the inline hook owns it, not the paywall.
	await expect(page.getByRole('dialog')).toHaveCount(0);
});

test('contract byop result hooks render after a mocked connection test', async ({ page }) => {
	let result: Record<string, unknown> = { ok: true, latencyMs: 12 };
	await page.route('**/v1/vault/test', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(result)
		})
	);

	async function reachTestStep(): Promise<void> {
		await page.goto('/onboarding/byop');
		await page.getByLabel('Hostname').fill('news.example.test');
		await page.getByLabel('Username').fill('reader');
		await page.getByLabel('Password').fill('fixture-password');
		await page.getByRole('button', { name: 'Continue' }).click();
		await page.getByRole('button', { name: 'Continue' }).click();
	}

	await reachTestStep();
	await page.getByTestId('byop-test-connection').click();
	const passed = page.getByTestId('byop-status');
	await expect(passed).toBeVisible();
	await expect(passed).toHaveAttribute('role', 'status');
	await expect(page.getByTestId('byop-save')).toBeVisible();

	// Failed variant: same status hook, no save (save stays gated on a pass).
	result = { ok: false };
	await reachTestStep();
	await page.getByTestId('byop-test-connection').click();
	const failed = page.getByTestId('byop-status');
	await expect(failed).toBeVisible();
	await expect(failed).toHaveAttribute('role', 'alert');
	await expect(failed).toContainText('could not reach');
	await expect(page.getByTestId('byop-save')).toHaveCount(0);
});

test('contract library hooks render with seeded items and a stale retry', async ({ page }) => {
	const seeded = [
		{ id: 'smoke-track-1', title: 'Smoke Song One', artist: 'Smoke Artist' },
		{ id: 'smoke-track-2', title: 'Smoke Song Two' }
	];
	let failRefresh = false;
	await page.route('**/api/library', (route) => {
		if (failRefresh) return route.abort('failed');
		failRefresh = true;
		return route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(seeded)
		});
	});

	await page.goto('/library');
	await expect(page.getByTestId('library-list')).toBeVisible();
	await expect(page.getByTestId('library-list')).toContainText('Smoke Song One');
	await expect(page.getByTestId('library-item')).toHaveCount(2);

	await page.getByRole('button', { name: 'Refresh' }).click();
	const badge = page.getByTestId('library-stale-badge');
	await expect(badge).toBeVisible();
	await expect(badge).toContainText(/stale — updated /i);
	await expect(page.getByTestId('library-item')).toHaveCount(2);
});

test('contract plan-limit notice renders through the paywall', async ({ page }) => {
	await ensurePasskeysSupported(page);
	await page.route('**/v1/auth/passkey/register/start', (route) =>
		route.fulfill({
			status: 402,
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ code: 'plan_limit_exceeded' })
		})
	);

	await page.goto('/signup');
	await page.getByTestId('signup-email').fill('alice@example.com');
	await page.getByTestId('signup-submit').click();

	// The PaywallModal instance of plan-limit-notice (the library inline
	// instance is gated behind the overLimit placeholder — see above).
	const notice = page.getByTestId('plan-limit-notice');
	await expect(notice).toBeVisible();
	await expect(notice.getByRole('link', { name: /upgrade your plan/i })).toHaveAttribute(
		'href',
		'/pricing'
	);
});

test('signup collects an email for passkey creation', async ({ page }) => {
	await page.goto('/signup');

	await expect(page.getByRole('heading', { level: 1 })).toContainText('Get started');
	await expect(page.locator('input[type="email"]')).toBeVisible();
	await expect(
		page.getByRole('button', { name: /create account with passkey/i })
	).toBeVisible();
});

test('login offers passkey and email-link sign-in', async ({ page }) => {
	await page.goto('/login');

	await expect(page.getByRole('heading', { level: 1 })).toContainText('Sign in');
	await expect(
		page.getByRole('button', { name: /sign in with passkey/i })
	).toBeVisible();
	await expect(page.locator('input[type="email"]')).toBeVisible();
});

test('magic-link landing route exists and fails generically without a token', async ({
	page,
	request
}) => {
	// The API Worker mails `{origin}/auth/magic?token=…`; the landing route
	// must exist (never a 404) and must not redeem anything tokenless.
	const response = await request.get('/auth/magic');
	expect(response.status(), '/auth/magic should not 404').not.toBe(404);
	expect(response.ok()).toBe(true);

	await page.goto('/auth/magic');
	await expect(page.getByRole('heading', { level: 1 })).toContainText('Sign in');
	await expect(page.getByRole('alert')).toContainText(/didn't work/i);
	await expect(page.getByRole('link', { name: /request a new one/i })).toHaveAttribute(
		'href',
		'/login'
	);
});

test('provider wizard walks provider, indexer and test steps', async ({ page, request }) => {
	for (const href of ['/login', '/signup', '/onboarding/provider']) {
		const response = await request.get(href);
		expect(response.status(), `${href} should not 404`).not.toBe(404);
		expect(response.ok()).toBe(true);
	}

	await page.goto('/onboarding/provider');
	await expect(page.getByRole('heading', { level: 1 })).toContainText('Connect your provider');

	await page.locator('input[name="host"]').fill('news.example.com');
	await page.locator('input[name="username"]').fill('alice');
	await page.locator('input[name="password"]').fill('s3cret');
	await page.getByRole('button', { name: 'Continue' }).click();

	await expect(page.getByText(/indexer improves search/i)).toBeVisible();
	await page.getByRole('button', { name: 'Continue' }).click();

	await expect(page.getByRole('heading', { level: 2 })).toContainText('Test the connection');
	await expect(
		page.getByRole('button', { name: /test connection/i })
	).toBeVisible();
});

test('wizard offers only the NNTP allowlist ports 119 and 563', async ({ page }) => {
	await page.goto('/onboarding/provider');

	const values = await page.locator('select[name="port"] option').evaluateAll((options) =>
		options.map((option) => option.getAttribute('value'))
	);
	expect(values).toEqual(['119', '563']);
});

test('wizard requires a plaintext acknowledgement when TLS is off', async ({ page }) => {
	await page.goto('/onboarding/provider');

	await page.locator('input[name="tls"]').uncheck();

	await expect(page.getByText(/password is sent in cleartext/i)).toBeVisible();
	await expect(page.locator('input[name="allowInsecurePlaintext"]')).toBeVisible();
});

test('wizard hides save until a connection test passes', async ({ page }) => {
	await page.goto('/onboarding/provider');

	await page.locator('input[name="host"]').fill('news.example.com');
	await page.locator('input[name="username"]').fill('alice');
	await page.locator('input[name="password"]').fill('s3cret');
	await page.getByRole('button', { name: 'Continue' }).click();

	await expect(page.getByText(/indexer improves search/i)).toBeVisible();
	await page.getByRole('button', { name: 'Continue' }).click();

	await expect(page.getByRole('heading', { level: 2 })).toContainText('Test the connection');
	// Save stays hidden until the live test passes, so the wizard can never
	// store credentials that were not just tested.
	await expect(page.getByRole('button', { name: /save provider/i })).toHaveCount(0);
});
