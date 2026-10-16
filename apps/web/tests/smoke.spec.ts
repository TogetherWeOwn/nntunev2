import { expect, test } from '@playwright/test';

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
