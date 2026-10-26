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

test('security.txt is served at the canonical RFC 9116 path', async ({ request }) => {
	// URL-level pin: the whole value of this endpoint is its canonical
	// location, so assert the route resolves here, not just that GET works.
	const response = await request.get('/.well-known/security.txt');

	expect(response.status()).toBe(200);
	expect(response.headers()['content-type']).toMatch(/^text\/plain(?:;|$)/);
	const body = await response.text();
	expect(body).toContain(
		'Contact: https://github.com/TogetherWeOwn/nntunev2/security/advisories/new'
	);

	const expiresLine = body.split('\n').find((line) => line.startsWith('Expires: '));
	expect(expiresLine).toBeDefined();
	const parsed = Date.parse(expiresLine!.slice('Expires: '.length));
	const DAY_MS = 24 * 60 * 60 * 1000;
	expect(parsed).toBeGreaterThan(Date.now() + 180 * DAY_MS);
	expect(parsed).toBeLessThanOrEqual(Date.now() + 366 * DAY_MS);
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
 * Audio-player: no library item carries a stream URL yet, so selecting a
 * row must render no <audio> element at all (never one with an empty src);
 * the absent-audio and honest-copy pins are the acceptance tests at the
 * bottom of this file.
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
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();
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
		await page.getByRole('button', { name: 'Continue', exact: true }).click();
		await page.getByRole('button', { name: 'Continue', exact: true }).click();
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
	await page.getByRole('button', { name: 'Continue', exact: true }).click();

	await expect(page.getByText(/indexer improves search/i)).toBeVisible();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();

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
	await page.getByRole('button', { name: 'Continue', exact: true }).click();

	await expect(page.getByText(/indexer improves search/i)).toBeVisible();
	await page.getByRole('button', { name: 'Continue', exact: true }).click();

	await expect(page.getByRole('heading', { level: 2 })).toContainText('Test the connection');
	// Save stays hidden until the live test passes, so the wizard can never
	// store credentials that were not just tested.
	await expect(page.getByRole('button', { name: /save provider/i })).toHaveCount(0);
});

/**
 * Acceptance: validated, honest library + import UI.
 *
 * OWNERSHIP: the tests from here to the end of this file are
 * acceptance-owned. The paired build card lands the route changes on top
 * of this branch and must make them pass WITHOUT editing them; a wrong
 * test comes back to the acceptance card instead. The validator half is
 * `apps/web/src/lib/library/import-validation.test.ts`.
 *
 * Honest-copy shape pinned here (exact wording is the builder's):
 * user-facing "not available yet" text, no internal slice/roadmap words,
 * no promise about timing, and no claim that a file was uploaded or
 * is being processed.
 */
const NOT_AVAILABLE_YET = /not (yet )?available|isn'?t available|not available yet/i;
const INTERNAL_OR_PROMISING =
	/\bslice\b|lands with|coming soon|stay tuned|will be available/i;
const CLAIMS_UPLOAD_OR_PROCESSING =
	/file received|has been uploaded|upload(ed|ing)? (complete|successful)|is being processed|processing your/i;

test('library renders no audio element while the selected track has no stream URL', async ({
	page
}) => {
	await page.route('**/api/library', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify([{ id: 'no-stream-1', title: 'Silent Song', artist: 'No Stream' }])
		})
	);

	await page.goto('/library');
	await expect(page.getByTestId('library-item')).toHaveCount(1);
	await expect(page.locator('audio')).toHaveCount(0);

	// Selecting the row makes it the queue's current track — the state that
	// used to reveal <audio data-testid="audio-player" src="">, which made
	// the browser request the page URL as if it were media.
	await page.getByTestId('library-item').click();
	await expect(page.getByTestId('now-playing')).toContainText('Silent Song');

	await expect(page.getByTestId('audio-player')).toHaveCount(0);
	await expect(page.locator('audio')).toHaveCount(0);
});

test('library usage copy is honest not-available-yet text', async ({ page }) => {
	await page.goto('/library');

	const meter = page.getByTestId('usage-meter');
	await expect(meter).toBeVisible();
	await expect(meter).toContainText(NOT_AVAILABLE_YET);
	await expect(meter).not.toContainText(INTERNAL_OR_PROMISING);
	await expect(page.getByRole('main')).not.toContainText(INTERNAL_OR_PROMISING);
});

test('import rejects empty, oversized and wrong-extension files with an alert and no upload request', async ({
	page
}) => {
	const cases = [
		{
			label: 'zero-byte .nzb',
			name: 'empty.nzb',
			buffer: Buffer.alloc(0),
			message: /empty|no data|0 bytes/i
		},
		{
			label: 'one byte over the 2 MiB intake cap',
			name: 'huge.nzb',
			buffer: Buffer.alloc(2 * 1024 * 1024 + 1),
			message: /too (large|big)|size|limit|exceed|maximum|2\s?(mb|mib)/i
		},
		{
			label: 'wrong extension',
			name: 'notes.txt',
			buffer: Buffer.from('not an nzb'),
			message: /nzb|extension|type/i
		}
	];

	for (const { label, name, buffer, message } of cases) {
		await page.goto('/library/import');
		await expect(page.getByTestId('import-file')).toBeAttached();
		await page.waitForLoadState('networkidle');

		// Count mutating (upload-shaped) requests made after the selection.
		const uploads: string[] = [];
		const onRequest = (request: { method(): string; url(): string }) => {
			if (['POST', 'PUT', 'PATCH'].includes(request.method())) {
				uploads.push(`${request.method()} ${request.url()}`);
			}
		};
		page.on('request', onRequest);
		try {
			await page
				.getByTestId('import-file')
				.setInputFiles({ name, mimeType: 'application/octet-stream', buffer });

			const alert = page.getByRole('alert');
			await expect(alert, `alert for ${label}`).toBeVisible();
			await expect(alert, `alert text for ${label}`).toContainText(message);
			await expect(alert).not.toContainText(INTERNAL_OR_PROMISING);
			// Let any (buggy) upload fire before counting.
			await page.waitForTimeout(250);
			expect(uploads, `no upload request for ${label}`).toEqual([]);
		} finally {
			page.off('request', onRequest);
		}
	}
});

test('import answers a valid file with an honest status, not a claim of upload or processing', async ({
	page
}) => {
	await page.goto('/library/import');
	await page.getByTestId('import-file').setInputFiles({
		name: 'album.nzb',
		mimeType: 'application/x-nzb',
		buffer: Buffer.from('<?xml version="1.0"?><nzb></nzb>')
	});

	const status = page.getByTestId('import-status');
	await expect(status).toBeVisible();
	await expect(status).toContainText(NOT_AVAILABLE_YET);
	await expect(status).not.toContainText(INTERNAL_OR_PROMISING);
	await expect(status).not.toContainText(CLAIMS_UPLOAD_OR_PROCESSING);
	// The valid path is a status, not an error.
	await expect(page.getByRole('alert')).toHaveCount(0);
	// And the whole page carries no internal slice/roadmap wording.
	await expect(page.getByRole('main')).not.toContainText(INTERNAL_OR_PROMISING);
});
