import { expect, test, type Page } from '@playwright/test';

// Protected acceptance spec: a dedicated `/account/provider` surface to
// manage the SAVED provider without re-entering secrets. The builder
// implements exactly these pinned page rules; the paired unit spec
// (`src/lib/api/client.saved-provider.test.ts`) pins the client methods.
//
// 1. Account navigation gains one `Provider` entry linking to
//    `/account/provider`. The page heading names the saved provider and
//    a `role="status"` region inside `<main>` announces state: loading
//    (`loading`/`checking`), configured, or not configured. Failures use
//    `role="alert"` and never look like a success state.
// 2. On load the page reads the booleans-only summary via
//    `GET /v1/vault`. It never retrieves or prefills a provider password
//    or indexer key: the page contains no `<input>` of any kind, issues
//    no `PUT` (no save/rotation call), and secret material present in a
//    hostile extra response field never appears in the DOM, web storage
//    or console output.
// 3. "Test saved provider" sends `POST /v1/vault/test` with an exactly
//    empty body (never `{}`). Result copy distinguishes three things:
//    the saved configuration exists, this reachability test passed/failed
//    for it, and provider sign-in/authentication is separate — a passed
//    reachability test is never presented as proof the provider accepted
//    the stored credentials.
// 4. "Remove saved provider" requires an explicit `window.confirm`
//    (same pattern as account security) before any `DELETE /v1/vault`:
//    dismissing the confirmation sends nothing. Only a validated
//    both-flags-false summary may show the removed/not-configured state;
//    a failed, reauth-required, rate-limited or malformed-success DELETE
//    keeps the configured state and never says "removed". Removal copy
//    never promises active/pooled connections were revoked, and says
//    existing connections may continue until they end.
// 5. Operations on this page are serialized: while a test or removal is
//    pending both action buttons are disabled, exactly one request is
//    in flight, and pending state settles (buttons re-enabled or the
//    terminal state shown) on success, failure and paywall/429 alike.
//    A response held past navigation away from the page cannot restore
//    a stale status when the page is shown again with newer state.
// 6. An expired session on load (401) ends at `/login` or an in-page
//    expired-session notice with a sign-in path — never a configured
//    claim. The existing provider-setup wizard is untouched.

const SECRET_SENTINEL = 'SAVED-PROVIDER-BROWSER-SENTINEL-9e07';

// A configured claim, never matched by "No saved provider is configured".
const CONFIGURED_TEXT = /(?<!\bno )(?<!\bnot )saved provider (is )?configured/i;

const CONFIGURED = {
	provider_configured: true,
	indexer_configured: true,
	updated_at: 1_760_000_000
};
const NOT_CONFIGURED = {
	provider_configured: false,
	indexer_configured: false,
	updated_at: 0
};

async function fulfillJson(
	route: Parameters<Parameters<Page['route']>[1]>[0],
	body: unknown,
	status = 200
): Promise<void> {
	await route.fulfill({
		status,
		contentType: 'application/json',
		body: JSON.stringify(body)
	});
}

test('account navigation links to the saved-provider page and shows the configured state', async ({
	page
}) => {
	await page.route('**/v1/vault', (route) => fulfillJson(route, CONFIGURED));

	await page.goto('/account/security');
	const navLink = page.getByRole('link', { name: /^provider$/i });
	await expect(navLink).toHaveAttribute('href', '/account/provider');
	await navLink.click();

	await page.waitForURL('**/account/provider');
	await expect(page.getByRole('heading', { name: /saved provider/i })).toBeVisible();
	await expect(page.locator('main')).toContainText(CONFIGURED_TEXT);
	await expect(page.getByRole('button', { name: /test saved provider/i })).toBeEnabled();
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toBeEnabled();
	// No secret is ever asked for again on this surface.
	await expect(page.locator('input')).toHaveCount(0);
});

test('shows the not-configured state with no test or remove actions', async ({ page }) => {
	await page.route('**/v1/vault', (route) => fulfillJson(route, NOT_CONFIGURED));

	await page.goto('/account/provider');

	await expect(page.locator('main')).toContainText(/no saved provider|not configured/i);
	await expect(page.getByRole('button', { name: /test saved provider/i })).toHaveCount(0);
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toHaveCount(0);
	await expect(page.locator('input')).toHaveCount(0);
});

test('shows a loading state while the summary is in flight, then settles', async ({ page }) => {
	let releaseSummary: (() => void) | undefined;
	const summaryHeld = new Promise<void>((resolve) => {
		releaseSummary = resolve;
	});
	await page.route('**/v1/vault', async (route) => {
		await summaryHeld;
		await fulfillJson(route, CONFIGURED);
	});

	await page.goto('/account/provider');
	await expect(page.locator('main')).toContainText(/loading|checking/i);
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toHaveCount(0);

	releaseSummary?.();
	await expect(page.locator('main')).toContainText(CONFIGURED_TEXT);
	await expect(page.getByRole('button', { name: /test saved provider/i })).toBeEnabled();
});

test('a summary failure shows an alert and never a configured claim or actions', async ({
	page
}) => {
	await page.route('**/v1/vault', (route) =>
		fulfillJson(route, { error: 'temporary failure' }, 500)
	);

	await page.goto('/account/provider');

	await expect(page.getByRole('alert')).toBeVisible();
	await expect(page.getByRole('alert')).toContainText(/could not|failed|try again/i);
	await expect(page.locator('main')).not.toContainText(CONFIGURED_TEXT);
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toHaveCount(0);
});

test('a malformed summary body is a failure, not a configured provider', async ({ page }) => {
	await page.route('**/v1/vault', (route) =>
		fulfillJson(route, {
			provider_configured: 'true',
			indexer_configured: false,
			updated_at: 0,
			password: SECRET_SENTINEL
		})
	);

	await page.goto('/account/provider');

	await expect(page.getByRole('alert')).toBeVisible();
	await expect(page.locator('main')).not.toContainText(CONFIGURED_TEXT);
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toHaveCount(0);
	await expect(page.locator('body')).not.toContainText(SECRET_SENTINEL);
});

test('an expired session on load ends at login or an expired notice, never configured', async ({
	page
}) => {
	await page.route('**/v1/vault', (route) =>
		fulfillJson(route, { error: 'check your email and try again' }, 401)
	);

	await page.goto('/account/provider');

	await expect
		.poll(async () => {
			if (new URL(page.url()).pathname === '/login') return 'login';
			const text = (await page.locator('body').innerText().catch(() => '')) ?? '';
			return /session (has )?expired|sign in again/i.test(text) ? 'expired-notice' : 'other';
		})
		.not.toBe('other');
	if (new URL(page.url()).pathname !== '/login') {
		await expect(page.locator('main')).not.toContainText(CONFIGURED_TEXT);
		await expect(page.getByRole('link', { name: /sign in again/i })).toHaveAttribute(
			'href',
			'/login'
		);
	}
});

test('stored retest posts an exactly empty body and distinguishes reachability from provider sign-in', async ({
	page
}) => {
	await page.route('**/v1/vault', (route) => fulfillJson(route, CONFIGURED));
	const postBodies: Array<string | null> = [];
	await page.route('**/v1/vault/test', async (route) => {
		postBodies.push(route.request().postData());
		await fulfillJson(route, { ok: true, latencyMs: 12 });
	});

	await page.goto('/account/provider');
	await page.getByRole('button', { name: /test saved provider/i }).click();

	const main = page.locator('main');
	await expect(main).toContainText(/reachable|test (passed|succeeded)|connected/i);
	// Reachability of the saved config is not provider authentication.
	await expect(main).toContainText(
		/does not (check|verify|prove)|not proof|separate from/i
	);
	await expect(main).toContainText(/sign-in|sign in|authentication|credentials/i);
	await expect(main).toContainText(CONFIGURED_TEXT);
	expect(postBodies).toEqual([null]);
	await expect(page.getByRole('button', { name: /test saved provider/i })).toBeEnabled();
});

test('a failed stored retest keeps the configured state and settles pending', async ({ page }) => {
	await page.route('**/v1/vault', (route) => fulfillJson(route, CONFIGURED));
	let posts = 0;
	await page.route('**/v1/vault/test', async (route) => {
		posts += 1;
		await fulfillJson(route, { ok: false });
	});

	await page.goto('/account/provider');
	await page.getByRole('button', { name: /test saved provider/i }).click();

	await expect(page.locator('main')).toContainText(/could not|failed|not reachable/i);
	await expect(page.locator('main')).toContainText(CONFIGURED_TEXT);
	await expect(page.getByRole('button', { name: /test saved provider/i })).toBeEnabled();
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toBeEnabled();
	expect(posts).toBe(1);
});

test('cancelling the removal confirmation sends no DELETE', async ({ page }) => {
	let deletes = 0;
	await page.route('**/v1/vault', async (route) => {
		if (route.request().method() === 'DELETE') {
			deletes += 1;
			await fulfillJson(route, NOT_CONFIGURED);
			return;
		}
		await fulfillJson(route, CONFIGURED);
	});

	await page.goto('/account/provider');
	page.once('dialog', (dialog) => dialog.dismiss());
	await page.getByRole('button', { name: /remove saved provider/i }).click();

	expect(deletes).toBe(0);
	await expect(page.locator('main')).toContainText(CONFIGURED_TEXT);
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toBeEnabled();
});

test('confirmed removal shows the removed state without promising connection revocation', async ({
	page
}) => {
	let deletes = 0;
	await page.route('**/v1/vault', async (route) => {
		if (route.request().method() === 'DELETE') {
			deletes += 1;
			await fulfillJson(route, NOT_CONFIGURED);
			return;
		}
		await fulfillJson(route, CONFIGURED);
	});

	await page.goto('/account/provider');
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: /remove saved provider/i }).click();

	const main = page.locator('main');
	await expect(main).toContainText(/removed|no saved provider|not configured/i);
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toHaveCount(0);
	await expect(main).not.toContainText(
		/(active|existing|pooled) connections? (were|have been|are now) (revoked|closed|disconnected|ended)/i
	);
	await expect(main).toContainText(
		/(existing|active) connections? may (continue|keep)|until they (end|close)|not (revoke|revoked)/i
	);
	expect(deletes).toBe(1);
});

test('a failed removal never says removed and settles pending', async ({ page }) => {
	let deletes = 0;
	await page.route('**/v1/vault', async (route) => {
		if (route.request().method() === 'DELETE') {
			deletes += 1;
			await fulfillJson(route, { error: 'vault unavailable' }, 400);
			return;
		}
		await fulfillJson(route, CONFIGURED);
	});

	await page.goto('/account/provider');
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: /remove saved provider/i }).click();

	await expect(page.getByRole('alert')).toBeVisible();
	await expect(page.locator('main')).toContainText(CONFIGURED_TEXT);
	await expect(page.locator('body')).not.toContainText(/provider (was )?removed|successfully removed/i);
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toBeEnabled();
	expect(deletes).toBe(1);
});

test('a reauth_required removal keeps the provider and names the reauthentication path once', async ({
	page
}) => {
	let deletes = 0;
	await page.route('**/v1/vault', async (route) => {
		if (route.request().method() === 'DELETE') {
			deletes += 1;
			await fulfillJson(
				route,
				{ code: 'reauth_required', message: 'reauth required', retryable: false },
				401
			);
			return;
		}
		await fulfillJson(route, CONFIGURED);
	});

	await page.goto('/account/provider');
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: /remove saved provider/i }).click();

	await expect(page.getByRole('alert')).toContainText(
		/reauth|sign in again|session is too old|session (has )?expired/i
	);
	await expect(page.locator('main')).toContainText(CONFIGURED_TEXT);
	await expect(page.locator('body')).not.toContainText(/provider (was )?removed|successfully removed/i);
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toBeEnabled();
	// No automatic repeated write or ceremony loop.
	expect(deletes).toBe(1);
});

test('a rate-limited removal settles pending without a removed claim', async ({ page }) => {
	let deletes = 0;
	await page.route('**/v1/vault', async (route) => {
		if (route.request().method() === 'DELETE') {
			deletes += 1;
			await fulfillJson(
				route,
				{ code: 'rate_limited', message: 'slow down', retryable: true },
				429
			);
			return;
		}
		await fulfillJson(route, CONFIGURED);
	});

	await page.goto('/account/provider');
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: /remove saved provider/i }).click();

	await expect(page.getByRole('button', { name: /remove saved provider/i })).toBeEnabled();
	await expect(page.locator('main')).toContainText(CONFIGURED_TEXT);
	await expect(page.locator('body')).not.toContainText(/provider (was )?removed|successfully removed/i);
	expect(deletes).toBe(1);
});

test('a malformed successful removal body never shows the removed state', async ({ page }) => {
	await page.route('**/v1/vault', async (route) => {
		if (route.request().method() === 'DELETE') {
			// 2xx, but still claims a configured provider: not a removal proof.
			await fulfillJson(route, CONFIGURED);
			return;
		}
		await fulfillJson(route, CONFIGURED);
	});

	await page.goto('/account/provider');
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: /remove saved provider/i }).click();

	await expect(page.getByRole('alert')).toBeVisible();
	await expect(page.locator('main')).toContainText(CONFIGURED_TEXT);
	await expect(page.locator('body')).not.toContainText(/provider (was )?removed|successfully removed/i);
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toBeEnabled();
});

test('operations are serialized while a stored retest is pending', async ({ page }) => {
	await page.route('**/v1/vault', (route) => fulfillJson(route, CONFIGURED));
	let posts = 0;
	let releasePost: (() => void) | undefined;
	let markPostStarted: (() => void) | undefined;
	const postStarted = new Promise<void>((resolve) => {
		markPostStarted = resolve;
	});
	await page.route('**/v1/vault/test', async (route) => {
		posts += 1;
		markPostStarted?.();
		await new Promise<void>((resolve) => {
			releasePost = resolve;
		});
		await fulfillJson(route, { ok: true });
	});

	await page.goto('/account/provider');
	await page.getByRole('button', { name: /test saved provider/i }).click();
	await postStarted;

	await expect(page.getByRole('button', { name: /test saved provider/i })).toBeDisabled();
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toBeDisabled();

	releasePost?.();
	await expect(page.getByRole('button', { name: /test saved provider/i })).toBeEnabled();
	await expect(page.getByRole('button', { name: /remove saved provider/i })).toBeEnabled();
	expect(posts).toBe(1);
});

test('secret sentinels never reach the DOM, storage or console, and no save/rotation call is made', async ({
	page
}) => {
	const consoleTexts: string[] = [];
	page.on('console', (message) => consoleTexts.push(message.text()));
	page.on('pageerror', (error) => consoleTexts.push(String(error)));

	const vaultMethods: string[] = [];
	await page.route('**/v1/vault', async (route) => {
		vaultMethods.push(route.request().method());
		if (route.request().method() === 'DELETE') {
			await fulfillJson(route, { error: 'vault unavailable' }, 400);
			return;
		}
		await fulfillJson(route, {
			...CONFIGURED,
			password: SECRET_SENTINEL,
			api_key: SECRET_SENTINEL,
			username: SECRET_SENTINEL
		});
	});
	await page.route('**/v1/vault/test', (route) => fulfillJson(route, { ok: true }));

	await page.goto('/account/provider');
	await expect(page.locator('main')).toContainText(CONFIGURED_TEXT);
	await page.getByRole('button', { name: /test saved provider/i }).click();
	await expect(page.locator('main')).toContainText(/reachable|test (passed|succeeded)|connected/i);
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: /remove saved provider/i }).click();
	await expect(page.getByRole('alert')).toBeVisible();

	await expect(page.locator('body')).not.toContainText(SECRET_SENTINEL);
	const storageDump = await page.evaluate(() =>
		JSON.stringify({ local: { ...localStorage }, session: { ...sessionStorage } })
	);
	expect(storageDump).not.toContain(SECRET_SENTINEL);
	expect(consoleTexts.join('\n')).not.toContain(SECRET_SENTINEL);
	expect(vaultMethods).not.toContain('PUT');
	await expect(page.locator('input')).toHaveCount(0);
});

test('a retest response held past navigation cannot restore a stale status', async ({ page }) => {
	let configured = true;
	await page.route('**/v1/vault', (route) =>
		fulfillJson(route, configured ? CONFIGURED : NOT_CONFIGURED)
	);
	let releasePost: (() => void) | undefined;
	let markPostStarted: (() => void) | undefined;
	const postStarted = new Promise<void>((resolve) => {
		markPostStarted = resolve;
	});
	await page.route('**/v1/vault/test', async (route) => {
		markPostStarted?.();
		await new Promise<void>((resolve) => {
			releasePost = resolve;
		});
		await fulfillJson(route, { ok: true });
	});

	await page.goto('/account/provider');
	await page.getByRole('button', { name: /test saved provider/i }).click();
	await postStarted;

	// Leave the page, let the stale response land, then return to newer state.
	await page.goto('/account/security');
	releasePost?.();
	configured = false;
	await page.goto('/account/provider');

	await expect(page.locator('main')).toContainText(/no saved provider|not configured/i);
	await expect(page.locator('main')).not.toContainText(/reachable|test (passed|succeeded)/i);
	await expect(page.locator('main')).not.toContainText(CONFIGURED_TEXT);
});
