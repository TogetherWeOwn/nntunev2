import { expect, test, type Page } from '@playwright/test';
import { openQueue } from './helpers/queue-host';

/** Ensure passkeys read as supported so signup reaches its API branches. */
async function ensurePasskeysSupported(page: Page): Promise<void> {
	await page.addInitScript(() => {
		if (typeof window.PublicKeyCredential === 'undefined') {
			Object.defineProperty(window, 'PublicKeyCredential', { value: class {} });
		}
	});
}

test('landing explains account plans and links to signup and pricing', async ({ page, request }) => {
	await page.goto('/');

	await expect(page.getByRole('heading', { level: 1 })).toContainText('NNTune');
	// \s+ because SSR may wrap the copy across lines.
	await expect(page.getByRole('main')).toContainText(/choose an\s+account plan/i);
	await expect(page.getByRole('main')).not.toContainText(/bring your\s+own|\bBYOP\b/i);
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
 * E2E contract-hook coverage (TESTIDS in e2e/helpers/contract.ts).
 * The static companion (apps/web/src/lib/contract.test.ts) pins that each
 * hook the web app still renders appears in its route markup; these specs
 * pin that the app actually renders it, seeding the states that reveal the
 * conditional ones. All API states below are Playwright route mocks.
 *
 * Always-rendered (plain navigation): app-root, signup-email, signup-submit,
 * usage-meter.
 * (User provider setup retired: no byop-* hooks. User NZB import retired in
 * TWO-1639: no import-file/import-status hooks.)
 *
 * Conditional (seeded states): signup-error (mocked 500 register/start),
 * library-list + library-item (mocked /api/library items),
 * library-stale-badge (mocked success then a failed refresh),
 * plan-limit-notice (mocked 402 paywall — the PaywallModal instance; the
 * library inline instance is gated behind the overLimit=false placeholder
 * and cannot render yet).
 *
 * Audio-player: no library item carries a stream URL yet, so selecting a
 * row must render no <audio> element at all (never one with an empty src);
 * the absent-audio and honest-copy pins are the acceptance tests at the
 * bottom of this file.
 */
test('contract routes resolve and render their e2e hooks', async ({ page, request }) => {
	for (const href of ['/signup', '/library']) {
		const response = await request.get(href);
		expect(response.status(), `${href} should not 404`).not.toBe(404);
		expect(response.ok()).toBe(true);
	}

	await page.goto('/signup');
	await expect(page.getByTestId('signup-email')).toBeVisible();
	await expect(page.getByTestId('signup-submit')).toBeVisible();

	await page.goto('/library');
	await expect(page.getByTestId('usage-meter')).toBeVisible();

	// User NZB import retired: the route is gone, so no
	// import-file/import-status hooks render anywhere.
	await expect(page.getByTestId('import-file')).toHaveCount(0);
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

/**
 * Acceptance: validated, honest library UI.
 *
 * OWNERSHIP: the tests from here to the end of this file are
 * acceptance-owned. The paired build card lands the route changes on top
 * of this branch and must make them pass WITHOUT editing them; a wrong
 * test comes back to the acceptance card instead.
 * (User NZB import retired: the import acceptance tests that
 * lived here were deleted with the route.)
 *
 * Honest-copy shape pinned here (exact wording is the builder's):
 * user-facing "not available yet" text, no internal slice/roadmap words,
 * no promise about timing, and no claim that a file was uploaded or
 * is being processed.
 */
const NOT_AVAILABLE_YET = /not (yet )?available|isn'?t available|not available yet/i;
const INTERNAL_OR_PROMISING =
	/\bslice\b|lands with|coming soon|stay tuned|will be available/i;

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
	await page.getByTestId('library-item').locator('button.nt-library-row').click();
	await openQueue(page);
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

test('retired user NZB import accepts no upload or URL anywhere', async ({ page }) => {
	// No user route accepts an NZB upload or URL. The route is
	// gone (404), the shell carries no import link, and no import hooks
	// render on any remaining route.
	const gone = await page.goto('/library/import');
	expect(gone?.status(), 'expected /library/import to be gone').toBe(404);
	await expect(page.getByTestId('import-file')).toHaveCount(0);
	await expect(page.getByTestId('import-status')).toHaveCount(0);
	await expect(
		page.getByRole('link', { name: /import an nzb/i })
	).toHaveCount(0);
	await expect(
		page.getByRole('link', { name: /select nzb locally/i })
	).toHaveCount(0);

	await page.goto('/library');
	await expect(page.getByTestId('import-file')).toHaveCount(0);
	await expect(page.getByTestId('import-status')).toHaveCount(0);
	await expect(
		page.getByRole('link', { name: /import an nzb/i })
	).toHaveCount(0);
});
